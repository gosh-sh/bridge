//! Scripted chains for tests. Each fake answers from state the test sets
//! up, and a few answers can be scripted to change from one call to the
//! next (receipts, block headers), which is how reorgs are simulated.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Mutex,
    },
};

use alloy_primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;

use crate::deposit::{
    an::{AccountInfo, AnRead, AnSend, ExtDir, MsgView, Page, TxListItem, TxView},
    evm::*,
    refusals::FinalizeSend,
};

/// A queue that repeats its last element once drained.
pub struct Script<T: Clone>(Mutex<VecDeque<T>>);

impl<T: Clone> Default for Script<T> {
    fn default() -> Self {
        Script(Mutex::new(VecDeque::new()))
    }
}

impl<T: Clone> Script<T> {
    /// A script that answers `items` in order, then the last one forever.
    pub fn new(items: impl IntoIterator<Item = T>) -> Self {
        Script(Mutex::new(items.into_iter().collect()))
    }

    /// The next answer; `None` only for an empty script.
    pub fn next(&self) -> Option<T> {
        let mut q = self.0.lock().unwrap();
        if q.len() > 1 {
            q.pop_front()
        } else {
            q.front().cloned()
        }
    }

    /// Replaces what is left of the script.
    pub fn set(&self, items: impl IntoIterator<Item = T>) {
        *self.0.lock().unwrap() = items.into_iter().collect();
    }
}

/// Scripted `eth_call` answers, keyed by `(to, first 4 bytes of calldata)`.
pub type CallScripts = HashMap<(Address, [u8; 4]), Script<Bytes>>;

/// An EVM node that answers from what the test put in it.
pub struct FakeEvm {
    /// What `chain_id` answers.
    pub chain_id: u64,
    /// Contract code by address; absent is an account without code.
    pub codes: Mutex<HashMap<Address, Bytes>>,
    /// `(to, first 4 bytes of calldata)` → return data, scripted per call.
    pub calls: Mutex<CallScripts>,
    /// `header(Latest)` and `header(Pending)`, scripted per call.
    pub latest: Script<Header>,
    /// `header(Finalized)`, scripted per call.
    pub finalized: Script<Option<Header>>,
    /// Headers `header_by_hash` and `header(Number)` know.
    pub by_hash: Mutex<HashMap<B256, Header>>,
    /// Receipts by transaction hash, scripted per call.
    pub receipts: Mutex<HashMap<B256, Script<Option<ReceiptLite>>>>,
    /// Transactions by hash.
    pub txs: Mutex<HashMap<B256, TxLite>>,
    /// `(address, tag)` → count; `Finalized` absent means the RPC does not know
    /// the tag.
    pub counts: Mutex<HashMap<(Address, &'static str), u64>>,
    /// Every `Deposit` log on the chain.
    pub logs: Mutex<Vec<DepositLogRef>>,
    /// `probe_block` fails with this text.
    pub probe_error: Mutex<Option<String>>,
    /// What `revert_reason` answers.
    pub revert: Mutex<Option<String>>,
    /// What `bridge_paused` answers; `None` is a bridge without `paused()`.
    pub paused: Mutex<Option<bool>>,
    /// `estimate_gas` reverts with this reason when set.
    pub estimate_revert: Mutex<Option<String>>,
    /// The next this-many `estimate_gas` calls fail with a transport error.
    pub estimate_transport_failures: AtomicU32,
    /// The next N `transaction` calls fail, as a flaky RPC does.
    pub fail_tx_reads: AtomicU32,
    /// The next N `transaction` calls answer `None`, as a lagging backend does.
    pub miss_tx_reads: AtomicU32,
    /// `Some(n)`: after n more `transaction` calls every transaction is gone
    /// (it left the mempool, or the backend stopped showing it).
    pub vanish_tx_after: Mutex<Option<u32>>,
    /// `header(Finalized)` never answers.
    pub hang_finalized: AtomicBool,
    /// `header_by_hash` never answers.
    pub hang_headers_by_hash: AtomicBool,
}

impl Default for FakeEvm {
    /// Chain id 0, nothing on chain, and a bridge that is not paused.
    fn default() -> Self {
        FakeEvm {
            chain_id: 0,
            codes: Mutex::default(),
            calls: Mutex::default(),
            latest: Script::default(),
            finalized: Script::default(),
            by_hash: Mutex::default(),
            receipts: Mutex::default(),
            txs: Mutex::default(),
            counts: Mutex::default(),
            logs: Mutex::default(),
            probe_error: Mutex::default(),
            revert: Mutex::default(),
            paused: Mutex::new(Some(false)),
            estimate_revert: Mutex::default(),
            estimate_transport_failures: AtomicU32::new(0),
            fail_tx_reads: AtomicU32::default(),
            miss_tx_reads: AtomicU32::default(),
            vanish_tx_after: Mutex::default(),
            hang_finalized: AtomicBool::default(),
            hang_headers_by_hash: AtomicBool::default(),
        }
    }
}

impl FakeEvm {
    /// An empty Sepolia.
    pub fn sepolia() -> Self {
        FakeEvm {
            chain_id: 11_155_111,
            ..Default::default()
        }
    }

    /// Every call of `selector` on `to` returns `ret`.
    pub fn set_call(&self, to: Address, selector: [u8; 4], ret: Bytes) {
        self.calls
            .lock()
            .unwrap()
            .insert((to, selector), Script::new([ret]));
    }

    /// Calls of `selector` on `to` return `seq` in order, then its last
    /// element.
    pub fn script_call(&self, to: Address, selector: [u8; 4], seq: Vec<Bytes>) {
        self.calls
            .lock()
            .unwrap()
            .insert((to, selector), Script::new(seq));
    }

    /// Reads of the receipt of `h` return `seq` in order, then its last
    /// element.
    pub fn script_receipt(&self, h: B256, seq: Vec<Option<ReceiptLite>>) {
        self.receipts.lock().unwrap().insert(h, Script::new(seq));
    }
}

#[async_trait]
impl EvmRead for FakeEvm {
    async fn chain_id(&self) -> anyhow::Result<u64> {
        Ok(self.chain_id)
    }

    async fn code(&self, a: Address) -> anyhow::Result<Bytes> {
        Ok(self
            .codes
            .lock()
            .unwrap()
            .get(&a)
            .cloned()
            .unwrap_or_default())
    }

    async fn call(&self, to: Address, data: Bytes) -> anyhow::Result<Bytes> {
        let sel: [u8; 4] = data
            .get(..4)
            .ok_or_else(|| anyhow::anyhow!("calldata without a selector"))?
            .try_into()?;
        self.calls
            .lock()
            .unwrap()
            .get(&(to, sel))
            .and_then(|s| s.next())
            .ok_or_else(|| anyhow::anyhow!("execution reverted"))
    }

    async fn revert_reason(
        &self,
        _: Address,
        _: Address,
        _: Bytes,
        _: u64,
    ) -> anyhow::Result<Option<String>> {
        Ok(self.revert.lock().unwrap().clone())
    }

    async fn bridge_paused(&self, _: Address) -> anyhow::Result<Option<bool>> {
        Ok(*self.paused.lock().unwrap())
    }

    async fn header(&self, tag: BlockTag) -> anyhow::Result<Option<Header>> {
        if tag == BlockTag::Finalized && self.hang_finalized.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(match tag {
            BlockTag::Latest | BlockTag::Pending => self.latest.next(),
            BlockTag::Finalized => self.finalized.next().flatten(),
            BlockTag::Number(n) => self
                .by_hash
                .lock()
                .unwrap()
                .values()
                .find(|h| h.number == n)
                .cloned(),
        })
    }

    async fn header_by_hash(&self, h: B256) -> anyhow::Result<Option<Header>> {
        if self.hang_headers_by_hash.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(self.by_hash.lock().unwrap().get(&h).cloned())
    }

    async fn receipt(&self, h: B256) -> anyhow::Result<Option<ReceiptLite>> {
        Ok(self
            .receipts
            .lock()
            .unwrap()
            .get(&h)
            .and_then(|s| s.next())
            .flatten())
    }

    async fn transaction(&self, h: B256) -> anyhow::Result<Option<TxLite>> {
        if self
            .fail_tx_reads
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            anyhow::bail!("503 Service Unavailable");
        }
        if self
            .miss_tx_reads
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Ok(None);
        }
        if let Some(left) = self.vanish_tx_after.lock().unwrap().as_mut() {
            if *left == 0 {
                return Ok(None);
            }
            *left -= 1;
        }
        Ok(self.txs.lock().unwrap().get(&h).cloned())
    }

    async fn tx_count(&self, a: Address, tag: BlockTag) -> anyhow::Result<u64> {
        let key = match tag {
            BlockTag::Latest => "latest",
            BlockTag::Pending => "pending",
            BlockTag::Finalized => "finalized",
            BlockTag::Number(_) => "latest",
        };
        self.counts
            .lock()
            .unwrap()
            .get(&(a, key))
            .copied()
            .ok_or_else(|| anyhow::anyhow!("unknown tag {key}"))
    }

    async fn deposit_logs(
        &self,
        bridge: Address,
        sender: Address,
        from: u64,
        to: u64,
    ) -> anyhow::Result<Vec<DepositLogRef>> {
        Ok(self
            .logs
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.log.address == bridge && l.log.topics.get(2) == Some(&sender.into_word()))
            .filter(|l| (from..=to).contains(&l.block_number))
            .cloned()
            .collect())
    }

    async fn probe_block(&self, _: BlockTag) -> anyhow::Result<usize> {
        match self.probe_error.lock().unwrap().clone() {
            Some(e) => anyhow::bail!(e),
            None => Ok(3),
        }
    }

    async fn estimate_gas(&self, _: Address, _: Address, _: Bytes) -> anyhow::Result<u64> {
        let left = self.estimate_transport_failures.load(Ordering::SeqCst);
        if left > 0 {
            self.estimate_transport_failures
                .store(left - 1, Ordering::SeqCst);
            anyhow::bail!("connection reset by peer");
        }
        if let Some(r) = self.estimate_revert.lock().unwrap().clone() {
            return Err(EstimateReverted(r).into());
        }
        Ok(80_000)
    }

    async fn fees(&self) -> anyhow::Result<Fees> {
        Ok(Fees {
            max_fee_per_gas: 2_000_000_000,
            max_priority_fee_per_gas: 1_000_000_000,
        })
    }
}

/// Block `number` whose hash is `tag` repeated and whose parent's hash is
/// `tag - 1` repeated.
pub fn header(number: u64, tag: u8) -> Header {
    Header {
        number,
        hash: B256::repeat_byte(tag),
        parent_hash: B256::repeat_byte(tag.wrapping_sub(1)),
        timestamp: 1_786_600_000 + number * 12,
    }
}

/// The bridge's `Deposit` log for these values.
pub fn deposit_log(
    bridge: Address,
    id: U256,
    sender: Address,
    amount: u64,
    account: B256,
    block_log_index: Option<u64>,
) -> LogLite {
    use alloy::sol_types::SolEvent;
    let ev = IDeposit::Deposit {
        depositId: id,
        sender,
        amount: U256::from(amount),
        anWorkchain: 0,
        anAccount: account,
        timestamp: U256::from(1_786_610_040u64),
    };
    let data = ev.encode_log_data();
    LogLite {
        address: bridge,
        topics: data.topics().to_vec(),
        data: data.data.clone(),
        block_log_index,
    }
}

/// A log of `token` that is not a bridge `Deposit`.
pub fn transfer_log(token: Address) -> LogLite {
    LogLite {
        address: token,
        topics: vec![B256::repeat_byte(0xdd)],
        data: Bytes::from(vec![0u8; 32]),
        block_log_index: Some(0),
    }
}

/// The receipt of `tx` mined in `block`.
pub fn deposit_receipt(tx: B256, block: &Header, status: bool, logs: Vec<LogLite>) -> ReceiptLite {
    ReceiptLite {
        tx_hash: tx,
        block_hash: block.hash,
        block_number: block.number,
        status,
        logs,
    }
}

/// A pending transaction from `from` to `bridge`.
pub fn deposit_tx(
    hash: B256,
    from: Address,
    bridge: Address,
    nonce: u64,
    tx_type: u8,
    input: Bytes,
) -> TxLite {
    TxLite {
        hash,
        from,
        to: Some(bridge),
        nonce,
        tx_type,
        input,
        access_list_rlp_len: 1,
        block_number: None,
    }
}

/// A wallet that answers from what the test put in it.
pub struct FakeWallet {
    /// The account `connect` reports.
    pub account: Address,
    /// Signs `personal_sign` requests.
    pub signer: Option<alloy::signers::local::PrivateKeySigner>,
    /// Returned by `personal_sign` instead of a signature.
    pub sign_override: Option<Bytes>,
    /// Answers to `send_transaction`, in order; `Timeout` once drained.
    pub send_results: VecDeque<Result<B256, crate::deposit::wallet::WalletError>>,
    /// Every request `send_transaction` received.
    pub sent: Vec<crate::deposit::wallet::TxRequest>,
    /// Runs at the start of every `send_transaction`.
    pub before_send: Option<Box<dyn Fn() + Send + Sync>>,
    /// What `capabilities` answers.
    pub caps: Option<serde_json::Value>,
}

impl FakeWallet {
    /// An EOA whose key the wallet holds.
    pub fn eoa() -> Self {
        let signer = alloy::signers::local::PrivateKeySigner::random();
        FakeWallet {
            account: signer.address(),
            signer: Some(signer),
            sign_override: None,
            send_results: Default::default(),
            sent: vec![],
            before_send: None,
            caps: None,
        }
    }
}

#[async_trait]
impl crate::deposit::wallet::Wallet for FakeWallet {
    fn kind(&self) -> crate::deposit::wallet::WalletKind {
        crate::deposit::wallet::WalletKind::WalletConnect
    }

    async fn connect(
        &mut self,
        _: &dyn crate::deposit::ui::Ui,
    ) -> Result<Address, crate::deposit::wallet::WalletError> {
        Ok(self.account)
    }

    async fn personal_sign(
        &mut self,
        _: Address,
        message: &str,
    ) -> Result<Bytes, crate::deposit::wallet::WalletError> {
        use alloy::signers::SignerSync as _;
        if let Some(b) = &self.sign_override {
            return Ok(b.clone());
        }
        let s = self.signer.as_ref().expect("a signer or an override");
        Ok(Bytes::from(
            s.sign_message_sync(message.as_bytes())
                .unwrap()
                .as_bytes()
                .to_vec(),
        ))
    }

    async fn capabilities(&mut self, _: Address) -> Option<serde_json::Value> {
        self.caps.clone()
    }

    async fn send_transaction(
        &mut self,
        _: &dyn crate::deposit::ui::Ui,
        tx: &crate::deposit::wallet::TxRequest,
    ) -> Result<B256, crate::deposit::wallet::WalletError> {
        if let Some(h) = &self.before_send {
            h();
        }
        self.sent.push(tx.clone());
        self.send_results
            .pop_front()
            .unwrap_or(Err(crate::deposit::wallet::WalletError::Timeout))
    }

    async fn close(&mut self) {}
}

/// A WalletConnect relay on loopback: `irn_subscribe`, `irn_publish` and
/// delivery to every subscriber of the topic — the publisher included, as a
/// real relay may do. It keeps every message and replays a topic's backlog
/// to each new subscription, standing in for the relay's TTL storage.
pub struct MockRelay {
    /// Where it listens.
    addr: std::net::SocketAddr,
    /// Subscriptions and backlog, shared by all connections.
    state: std::sync::Arc<Mutex<MockRelayState>>,
    /// Tells every open connection to hang up.
    kill: tokio::sync::broadcast::Sender<()>,
    /// Tells every open connection to go silent without closing.
    stall: tokio::sync::broadcast::Sender<()>,
    /// Connections accepted so far.
    accepted: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// What [`MockRelay`] knows across connections.
#[derive(Default)]
struct MockRelayState {
    /// Live subscriptions: topic → the outgoing queue of each subscribed
    /// connection.
    subs: HashMap<String, Vec<tokio::sync::mpsc::UnboundedSender<String>>>,
    /// Every message published so far, per topic, with its tag.
    backlog: HashMap<String, Vec<(String, u64)>>,
    /// Every publication so far, in order: topic, tag and TTL.
    published: Vec<(String, u64, u64)>,
}

impl MockRelay {
    /// Starts the relay on a free loopback port.
    pub async fn start() -> MockRelay {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = std::sync::Arc::new(Mutex::new(MockRelayState::default()));
        let (kill, _) = tokio::sync::broadcast::channel(4);
        let (stall, _) = tokio::sync::broadcast::channel(4);
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (st, k, sl, n) = (state.clone(), kill.clone(), stall.clone(), accepted.clone());
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                n.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(MockRelay::serve(
                    tcp,
                    st.clone(),
                    k.subscribe(),
                    sl.subscribe(),
                ));
            }
        });
        MockRelay {
            addr,
            state,
            kill,
            stall,
            accepted,
        }
    }

    /// The `ws://` URL to connect to.
    pub fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    /// Connections accepted so far, reconnects included.
    pub fn connections(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    /// Every publication so far, in order: topic, tag and TTL in seconds.
    pub fn published(&self) -> Vec<(String, u64, u64)> {
        self.state.lock().unwrap().published.clone()
    }

    /// Makes every open connection half-open: from now on it reads and
    /// writes nothing — not even a pong — and it does not close. The
    /// relay still counts its subscriptions, so what is published to them
    /// meanwhile is only in the backlog.
    pub fn stall_all(&self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "with no connection open there is nothing to stall"
        )]
        let _ = self.stall.send(());
    }

    /// Cuts every open connection, stalled ones included. Subscriptions die
    /// with their connections; the backlog stays.
    pub fn drop_all(&self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "with no connection open there is nothing to cut"
        )]
        let _ = self.kill.send(());
        self.state.lock().unwrap().subs.clear();
    }

    /// One client connection, until the client leaves or `killed` fires.
    /// After `stalled` fires it only waits for `killed`.
    async fn serve(
        tcp: tokio::net::TcpStream,
        state: std::sync::Arc<Mutex<MockRelayState>>,
        mut killed: tokio::sync::broadcast::Receiver<()>,
        mut stalled: tokio::sync::broadcast::Receiver<()>,
    ) {
        use futures::{SinkExt as _, StreamExt as _};
        use tokio_tungstenite::tungstenite::Message;

        let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else {
            return;
        };
        let (mut sink, mut stream) = ws.split();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        loop {
            tokio::select! {
                _ = killed.recv() => return,
                _ = stalled.recv() => {
                    // The socket stays open and untouched until `drop_all`.
                    #[expect(
                        clippy::let_underscore_must_use,
                        reason = "a closed kill channel ends the stall just the same"
                    )]
                    let _ = killed.recv().await;
                    return;
                }
                Some(o) = out_rx.recv() => {
                    if sink.send(Message::Text(o.into())).await.is_err() {
                        return;
                    }
                }
                m = stream.next() => {
                    let t = match m {
                        Some(Ok(Message::Text(t))) => t,
                        // Pings are answered by tungstenite on the next read.
                        Some(Ok(_)) => continue,
                        _ => return,
                    };
                    let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                    let topic = v["params"]["topic"].as_str().unwrap_or_default().to_string();
                    let mut s = state.lock().unwrap();
                    match v["method"].as_str() {
                        Some("irn_subscribe") => {
                            s.subs.entry(topic.clone()).or_default().push(out_tx.clone());
                            let ok = serde_json::json!({"id": v["id"], "jsonrpc": "2.0", "result": "sub"});
                            MockRelay::push(&out_tx, ok);
                            for (msg, tag) in s.backlog.get(&topic).cloned().unwrap_or_default() {
                                MockRelay::push(&out_tx, MockRelay::delivery(1, &topic, &msg, tag));
                            }
                        }
                        Some("irn_publish") => {
                            let msg = v["params"]["message"].as_str().unwrap().to_string();
                            let tag = v["params"]["tag"].as_u64().unwrap();
                            s.backlog.entry(topic.clone()).or_default().push((msg.clone(), tag));
                            let ttl = v["params"]["ttl"].as_u64().unwrap();
                            s.published.push((topic.clone(), tag, ttl));
                            for sub in s.subs.get(&topic).cloned().unwrap_or_default() {
                                MockRelay::push(&sub, MockRelay::delivery(2, &topic, &msg, tag));
                            }
                            let ok = serde_json::json!({"id": v["id"], "jsonrpc": "2.0", "result": true});
                            MockRelay::push(&out_tx, ok);
                        }
                        _ => {} // acks of `irn_subscription`
                    }
                }
            }
        }
    }

    /// Queues `frame` on a connection's outgoing queue.
    fn push(to: &tokio::sync::mpsc::UnboundedSender<String>, frame: serde_json::Value) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "a connection that has closed takes no more frames"
        )]
        let _ = to.send(frame.to_string());
    }

    /// An `irn_subscription` push carrying one published message.
    fn delivery(id: u64, topic: &str, message: &str, tag: u64) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "jsonrpc": "2.0",
            "method": "irn_subscription",
            "params": {
                "id": "sub",
                "data": {"topic": topic, "message": message, "tag": tag, "publishedAt": 0}
            }
        })
    }
}

/// When a [`MockWalletPeer`] tells the session about the chain it just
/// added.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AddUpdate {
    /// `wc_sessionUpdate` first, then the answer.
    Before,
    /// The answer first, then `wc_sessionUpdate`.
    After,
    /// Answers and never updates the session.
    Never,
}

/// How a [`MockWalletPeer`] answers.
pub struct PeerBehaviour {
    /// The CAIP-10 accounts it shares, e.g. `eip155:11155111:0x…`.
    pub accounts: Vec<String>,
    /// Signs `personal_sign`.
    pub signer: alloy::signers::local::PrivateKeySigner,
    /// What `eth_sendTransaction` answers: a hash, or an error code.
    pub send_result: Result<B256, i64>,
    /// Settles with the signer on chain 1 only; `wallet_switchEthereumChain`
    /// then answers 4902, and `wallet_addEthereumChain` updates the session
    /// to `accounts` before it answers.
    pub missing_chain_then_add: bool,
    /// When the update that follows `wallet_addEthereumChain` is sent.
    pub add_update: AddUpdate,
}

/// The wallet side of WalletConnect, on the same primitives as the dApp
/// side. Like a wallet whose relay client retried its publish, it sends
/// everything twice, each copy sealed with its own nonce; like a wallet, it
/// answers each request id once.
pub struct MockWalletPeer;

impl MockWalletPeer {
    /// How long a peer waits for the next message before it leaves.
    const IDLE: std::time::Duration = std::time::Duration::from_secs(600);

    /// Scans `uri` on the relay at `url`, approves the session and answers
    /// requests until nothing arrives for [`Self::IDLE`].
    pub fn spawn(
        url: String,
        uri: crate::deposit::wc::session::PairingUri,
        b: PeerBehaviour,
    ) -> tokio::task::JoinHandle<()> {
        use alloy::signers::SignerSync as _;
        use serde_json::json;

        use crate::deposit::wc::{relay::Relay, session::*};

        tokio::spawn(async move {
            let mut r = Relay::connect(url).await.unwrap();
            let mut accounts = if b.missing_chain_then_add {
                vec![format!("eip155:1:{:#x}", b.signer.address())]
            } else {
                b.accounts.clone()
            };
            let (topic, sym) = Self::pair(&mut r, &uri, &accounts).await;
            let mut answered = std::collections::HashSet::new();
            while let Some((v, _)) = Self::next(&mut r, &topic, &sym, Self::IDLE).await {
                if v["method"] != "wc_sessionRequest" || !answered.insert(v["id"].as_u64().unwrap())
                {
                    continue;
                }
                let req = &v["params"]["request"];
                let ok = |result: serde_json::Value| json!({"id": v["id"], "jsonrpc": "2.0", "result": result});
                let err = |code: i64, message: &str| json!({"id": v["id"], "jsonrpc": "2.0", "error": {"code": code, "message": message}});
                let reply = match req["method"].as_str().unwrap() {
                    "personal_sign" => {
                        let msg = hex::decode(
                            req["params"][0].as_str().unwrap().trim_start_matches("0x"),
                        )
                        .unwrap();
                        let sig = b.signer.sign_message_sync(&msg).unwrap();
                        ok(json!(format!("0x{}", hex::encode(sig.as_bytes()))))
                    },
                    "eth_sendTransaction" => match &b.send_result {
                        Ok(h) => ok(json!(format!("{h:#x}"))),
                        Err(code) => err(*code, "User rejected"),
                    },
                    "eth_accounts" => {
                        let addresses: Vec<&str> = accounts
                            .iter()
                            .filter_map(|a| a.rsplit_once(':').map(|(_, x)| x))
                            .collect();
                        ok(json!(addresses))
                    },
                    "wallet_switchEthereumChain" => err(4902, "Unrecognized chain"),
                    "wallet_addEthereumChain" => {
                        accounts = b.accounts.clone();
                        let update = json!({"id": 98, "jsonrpc": "2.0", "method": "wc_sessionUpdate",
                            "params": {"namespaces": {"eip155": {"accounts": accounts, "methods": METHODS, "events": EVENTS}}}});
                        match b.add_update {
                            AddUpdate::Before => {
                                Self::send(&r, &topic, &sym, update, TAG_UPDATE).await;
                            },
                            AddUpdate::After => {
                                Self::send(
                                    &r,
                                    &topic,
                                    &sym,
                                    ok(serde_json::Value::Null),
                                    TAG_REQUEST_RESP,
                                )
                                .await;
                                Self::send(&r, &topic, &sym, update, TAG_UPDATE).await;
                            },
                            AddUpdate::Never => {},
                        }
                        ok(serde_json::Value::Null)
                    },
                    _ => err(-32601, "unsupported"),
                };
                Self::send(&r, &topic, &sym, reply, TAG_REQUEST_RESP).await;
            }
        })
    }

    /// The wallet's half of pairing: reads the proposal on `uri`'s topic,
    /// settles a session that shares `accounts`, then answers the proposal
    /// — in that order, as some wallets do, so the dApp picks the settlement
    /// up from what the relay kept. Returns the session topic and its key.
    pub async fn pair(
        r: &mut crate::deposit::wc::relay::Relay,
        uri: &crate::deposit::wc::session::PairingUri,
        accounts: &[String],
    ) -> (String, [u8; 32]) {
        use serde_json::json;

        use crate::deposit::wc::{crypto::*, session::*};

        r.subscribe(&uri.topic).await.unwrap();
        let propose = loop {
            let (v, _) = Self::next(r, &uri.topic, &uri.sym_key, Self::IDLE)
                .await
                .expect("a session proposal");
            if v["method"] == "wc_sessionPropose" {
                break v;
            }
        };
        let dapp: [u8; 32] =
            hex::decode(propose["params"]["proposer"]["publicKey"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
        let me = KeyPair::generate();
        let sym = derive_sym_key(&me.secret, &dapp);
        let topic = topic_of(&sym);
        r.subscribe(&topic).await.unwrap();
        let week = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 7 * 86_400;
        let settle = json!({"id": 99, "jsonrpc": "2.0", "method": "wc_sessionSettle",
            "params": {"relay": {"protocol": "irn"},
                       "namespaces": {"eip155": {"accounts": accounts, "methods": METHODS, "events": EVENTS}},
                       "controller": {"publicKey": hex::encode(me.public)}, "expiry": week}});
        Self::send(r, &topic, &sym, settle, TAG_SETTLE).await;
        let answer = json!({"id": propose["id"], "jsonrpc": "2.0",
            "result": {"relay": {"protocol": "irn"}, "responderPublicKey": hex::encode(me.public)}});
        Self::send(r, &uri.topic, &uri.sym_key, answer, TAG_PROPOSE_RESP).await;
        (topic, sym)
    }

    /// Publishes `v` on `topic` under `sym` twice, each copy sealed with its
    /// own nonce, the way a relay client that retried would.
    pub async fn send(
        r: &crate::deposit::wc::relay::Relay,
        topic: &str,
        sym: &[u8; 32],
        v: serde_json::Value,
        tag: u32,
    ) {
        use crate::deposit::wc::crypto::{random_bytes, seal_type0};

        for _ in 0..2 {
            let sealed = seal_type0(sym, random_bytes(), v.to_string().as_bytes());
            r.publish(topic, &sealed, 300, tag).await.unwrap();
        }
    }

    /// The next message on `topic` that opens under `sym` as JSON, with its
    /// tag; `None` once `idle` passes without one.
    pub async fn next(
        r: &mut crate::deposit::wc::relay::Relay,
        topic: &str,
        sym: &[u8; 32],
        idle: std::time::Duration,
    ) -> Option<(serde_json::Value, u32)> {
        use crate::deposit::wc::crypto::{open, parse};

        loop {
            let m = r.recv(idle).await?;
            if m.topic != topic {
                continue;
            }
            let Some(plain) = parse(&m.message).ok().and_then(|e| open(sym, &e).ok()) else {
                continue;
            };
            if let Ok(v) = serde_json::from_slice(&plain) {
                return Some((v, m.tag));
            }
        }
    }
}

/// Scripted getter answers, keyed by `(account, function)`.
pub type GetterScripts = HashMap<([u8; 32], String), Script<serde_json::Value>>;

/// An Acki Nacki node that answers from what the test put in it.
#[derive(Default)]
pub struct FakeAn {
    /// Accounts by id; absent is an account the node does not have.
    pub accounts: Mutex<HashMap<[u8; 32], AccountInfo>>,
    /// `(account, function)` → output, scripted per call.
    pub getters: Mutex<GetterScripts>,
    /// Inbound external messages by account, newest first.
    pub ext_in: Mutex<HashMap<[u8; 32], Vec<MsgView>>>,
    /// Outbound external messages (events) by account, newest first.
    pub ext_out: Mutex<HashMap<[u8; 32], Vec<MsgView>>>,
    /// Transactions by account, oldest first.
    pub txs: Mutex<HashMap<[u8; 32], Vec<TxListItem>>>,
    /// Messages by hash.
    pub messages: Mutex<HashMap<String, MsgView>>,
    /// Transactions by hash.
    pub transactions: Mutex<HashMap<String, TxView>>,
    /// Pre-decoded bodies: body string → (name, value).
    pub bodies: Mutex<HashMap<String, (String, serde_json::Value)>>,
    /// Answers to `send_finalize`, in order; `Unknown` once drained.
    pub sends: Mutex<VecDeque<FinalizeSend>>,
    /// How many times `send_finalize` was called.
    pub sent: Mutex<u32>,
    /// How long each send takes before it answers.
    pub send_delay: Mutex<std::time::Duration>,
    /// External messages that appear once a send was made: `(account,
    /// message)`.
    pub on_send_ext_out: Mutex<Vec<([u8; 32], MsgView)>>,
    /// Getter name → how many more calls answer before every call hangs.
    pub hang_getter_after: Mutex<HashMap<String, u32>>,
    /// `isAcceptedBlockHash` by block: how many calls answer `false`
    /// before the block turns accepted (0 = accepted from the start).
    pub accepted: Mutex<HashMap<B256, usize>>,
    /// `isAcceptedBlockHash` calls so far, by block.
    accepted_calls: Mutex<HashMap<B256, usize>>,
    /// Takes precedence over `accepted`: answers in order, the last one
    /// repeating.
    pub accepted_seq: Mutex<HashMap<B256, Script<bool>>>,
    /// Make a whole family of reads fail, as a broken endpoint would.
    pub fail_getters: AtomicBool,
    /// Every `transactions` call fails.
    pub fail_transactions: AtomicBool,
    /// Every `ext_messages` call fails.
    pub fail_ext_messages: AtomicBool,
}

impl FakeAn {
    /// Scripts the answers of getter `f` on account `id`.
    pub fn getter(&self, id: [u8; 32], f: &str, seq: Vec<serde_json::Value>) {
        self.getters
            .lock()
            .unwrap()
            .insert((id, f.into()), Script::new(seq));
    }
}

#[async_trait]
impl AnRead for FakeAn {
    async fn rest_probe(&self, id: [u8; 32]) -> anyhow::Result<()> {
        self.accounts
            .lock()
            .unwrap()
            .get(&id)
            .map(|_| ())
            .ok_or_else(|| anyhow::anyhow!("404"))
    }

    async fn account(&self, id: [u8; 32]) -> anyhow::Result<Option<AccountInfo>> {
        Ok(self.accounts.lock().unwrap().get(&id).cloned())
    }

    async fn run_getter(
        &self,
        id: [u8; 32],
        _: &str,
        f: &str,
        input: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        if self.fail_getters.load(Ordering::SeqCst) {
            anyhow::bail!("503 Service Unavailable");
        }
        let hang = match self.hang_getter_after.lock().unwrap().get_mut(f) {
            Some(0) => true,
            Some(n) => {
                *n -= 1;
                false
            },
            None => false,
        };
        if hang {
            std::future::pending::<()>().await;
        }
        if f == "isAcceptedBlockHash" {
            let h: B256 = input["blockHash"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default();
            if let Some(yes) = self
                .accepted_seq
                .lock()
                .unwrap()
                .get(&h)
                .and_then(|s| s.next())
            {
                return Ok(serde_json::json!({ "value0": yes }));
            }
            let mut calls = self.accepted_calls.lock().unwrap();
            let n = calls.entry(h).or_insert(0);
            *n += 1;
            let yes = self
                .accepted
                .lock()
                .unwrap()
                .get(&h)
                .is_some_and(|k| *n > *k);
            return Ok(serde_json::json!({ "value0": yes }));
        }
        self.getters
            .lock()
            .unwrap()
            .get(&(id, f.to_string()))
            .and_then(|s| s.next())
            .ok_or_else(|| anyhow::anyhow!("no getter {f}"))
    }

    async fn ext_messages(
        &self,
        id: [u8; 32],
        dir: ExtDir,
        _: Option<String>,
    ) -> anyhow::Result<Page<MsgView>> {
        if self.fail_ext_messages.load(Ordering::SeqCst) {
            anyhow::bail!("GraphQL timeout");
        }
        let m = match dir {
            ExtDir::In => &self.ext_in,
            ExtDir::Out => &self.ext_out,
        };
        Ok(Page {
            items: m.lock().unwrap().get(&id).cloned().unwrap_or_default(),
            cursor: None,
        })
    }

    async fn transactions(
        &self,
        id: [u8; 32],
        _: Option<String>,
    ) -> anyhow::Result<Page<TxListItem>> {
        if self.fail_transactions.load(Ordering::SeqCst) {
            anyhow::bail!("GraphQL timeout");
        }
        Ok(Page {
            items: self
                .txs
                .lock()
                .unwrap()
                .get(&id)
                .cloned()
                .unwrap_or_default(),
            cursor: None,
        })
    }

    async fn message(&self, h: &str) -> anyhow::Result<Option<MsgView>> {
        Ok(self.messages.lock().unwrap().get(h).cloned())
    }

    async fn transaction(&self, h: &str) -> anyhow::Result<Option<TxView>> {
        Ok(self.transactions.lock().unwrap().get(h).cloned())
    }

    fn decode(&self, _: &str, body: &str, _: bool) -> Option<(String, serde_json::Value)> {
        self.bodies.lock().unwrap().get(body).cloned()
    }
}

#[async_trait]
impl AnSend for FakeAn {
    async fn send_finalize(&self, _: [u8; 32], _: [u8; 32], _: &[u8], _: &[u8]) -> FinalizeSend {
        *self.sent.lock().unwrap() += 1;
        for (acc, m) in self.on_send_ext_out.lock().unwrap().drain(..) {
            self.ext_out.lock().unwrap().entry(acc).or_default().push(m);
        }
        let delay = *self.send_delay.lock().unwrap();
        tokio::time::sleep(delay).await;
        self.sends
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(FinalizeSend::Unknown {
                message: "no script".into(),
            })
    }
}

/// A message with no currencies and no resolved transactions.
pub fn msg(hash: &str, kind: &str, src: &str, dst: &str, body: Option<&str>) -> MsgView {
    MsgView {
        hash: hash.into(),
        msg_type: kind.into(),
        src: src.into(),
        dst: dst.into(),
        body: body.map(String::from),
        ecc: vec![],
        bounce: None,
        created_at: 0,
        dst_tx: None,
        src_tx: None,
    }
}
