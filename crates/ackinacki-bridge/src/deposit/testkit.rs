//! Scripted chains for tests. Each fake answers from state the test sets
//! up, and a few answers can be scripted to change from one call to the
//! next (receipts, block headers), which is how reorgs are simulated.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
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

/// `eth_call` answers at one block, keyed by `(to, first 4 bytes of
/// calldata, block number)`.
pub type CallsAtBlock = HashMap<(Address, [u8; 4], u64), Bytes>;

/// An EVM node that answers from what the test put in it.
pub struct FakeEvm {
    /// What `chain_id` answers.
    pub chain_id: u64,
    /// Contract code by address; absent is an account without code.
    pub codes: Mutex<HashMap<Address, Bytes>>,
    /// `(to, first 4 bytes of calldata)` → return data, scripted per call.
    pub calls: Mutex<CallScripts>,
    /// What calls at the head (`Latest`, `Pending`) answer first, as a node
    /// behind the one that served the last receipt does; a call pinned to
    /// a block number, and a key without a script here, answer from
    /// `calls`.
    pub stale_calls: Mutex<CallScripts>,
    /// `(to, first 4 bytes of calldata, block)` → what a call pinned to that
    /// block number answers, before `calls` is asked.
    pub at_block: Mutex<CallsAtBlock>,
    /// The next N calls pinned to a block number fail, as on a node that
    /// does not have the block yet.
    pub pinned_misses: AtomicU32,
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
    /// `(address, block number)` → count at that block; a block the test did
    /// not set counts as `"pending"` does.
    pub counts_at: Mutex<HashMap<(Address, u64), u64>>,
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
    /// Contract → the reason `estimate_gas` of a call to it reverts with
    /// at the head, as on a node behind the one that served the last
    /// receipt; pinned to a block number it does not.
    pub stale_estimate_reverts: Mutex<HashMap<Address, String>>,
    /// The block of every call and estimate pinned to a block number, in
    /// order.
    pub pinned_reads: Mutex<Vec<u64>>,
    /// The next this-many `estimate_gas` calls fail with a transport error.
    pub estimate_transport_failures: AtomicU32,
    /// The next N `transaction` calls fail, as a flaky RPC does.
    pub fail_tx_reads: AtomicU32,
    /// The next N `receipt` calls fail; `u32::MAX` is an RPC that stays
    /// down.
    pub fail_receipts: AtomicU32,
    /// `Some(n)`: n more `call`s answer, then every one fails.
    pub calls_before_failing: Mutex<Option<u32>>,
    /// How long every `call` takes before it answers.
    pub call_delay: Mutex<std::time::Duration>,
    /// The next N `transaction` calls answer `None`, as a lagging backend does.
    pub miss_tx_reads: AtomicU32,
    /// `Some(n)`: after n more `transaction` calls every transaction is gone
    /// (it left the mempool, or the backend stopped showing it).
    pub vanish_tx_after: Mutex<Option<u32>>,
    /// Transaction → how many more reads show it before it is gone, as a
    /// transaction the wallet replaced.
    pub vanishing_txs: Mutex<HashMap<B256, u32>>,
    /// `header(Finalized)` never answers.
    pub hang_finalized: AtomicBool,
    /// `header_by_hash` never answers.
    pub hang_headers_by_hash: AtomicBool,
    /// `header(Finalized)` fails, as a flaky RPC does.
    pub fail_finalized: AtomicBool,
    /// `header_by_hash` fails for these hashes.
    pub failing_headers: Mutex<HashSet<B256>>,
    /// `chain_id` fails, as an RPC that is down does.
    pub fail_chain_id: AtomicBool,
}

impl Default for FakeEvm {
    /// Chain id 0, nothing on chain, and a bridge that is not paused.
    fn default() -> Self {
        FakeEvm {
            chain_id: 0,
            codes: Mutex::default(),
            calls: Mutex::default(),
            stale_calls: Mutex::default(),
            at_block: Mutex::default(),
            pinned_misses: AtomicU32::default(),
            latest: Script::default(),
            finalized: Script::default(),
            by_hash: Mutex::default(),
            receipts: Mutex::default(),
            txs: Mutex::default(),
            counts: Mutex::default(),
            counts_at: Mutex::default(),
            logs: Mutex::default(),
            probe_error: Mutex::default(),
            revert: Mutex::default(),
            paused: Mutex::new(Some(false)),
            estimate_revert: Mutex::default(),
            stale_estimate_reverts: Mutex::default(),
            pinned_reads: Mutex::default(),
            estimate_transport_failures: AtomicU32::new(0),
            fail_tx_reads: AtomicU32::default(),
            fail_receipts: AtomicU32::default(),
            calls_before_failing: Mutex::default(),
            call_delay: Mutex::default(),
            miss_tx_reads: AtomicU32::default(),
            vanish_tx_after: Mutex::default(),
            vanishing_txs: Mutex::default(),
            hang_finalized: AtomicBool::default(),
            hang_headers_by_hash: AtomicBool::default(),
            fail_finalized: AtomicBool::default(),
            failing_headers: Mutex::default(),
            fail_chain_id: AtomicBool::default(),
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
        if self.fail_chain_id.load(Ordering::SeqCst) {
            anyhow::bail!("connection refused");
        }
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

    async fn call_at(&self, to: Address, data: Bytes, at: BlockTag) -> anyhow::Result<Bytes> {
        let delay = *self.call_delay.lock().unwrap();
        tokio::time::sleep(delay).await;
        if let Some(left) = self.calls_before_failing.lock().unwrap().as_mut() {
            if *left == 0 {
                anyhow::bail!("503 Service Unavailable");
            }
            *left -= 1;
        }
        let sel: [u8; 4] = data
            .get(..4)
            .ok_or_else(|| anyhow::anyhow!("calldata without a selector"))?
            .try_into()?;
        let first = match at {
            BlockTag::Number(n) => {
                self.pinned_reads.lock().unwrap().push(n);
                if self
                    .pinned_misses
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |k| k.checked_sub(1))
                    .is_ok()
                {
                    anyhow::bail!("header not found");
                }
                self.at_block.lock().unwrap().get(&(to, sel, n)).cloned()
            },
            _ => self
                .stale_calls
                .lock()
                .unwrap()
                .get(&(to, sel))
                .and_then(|s| s.next()),
        };
        first
            .or_else(|| {
                self.calls
                    .lock()
                    .unwrap()
                    .get(&(to, sel))
                    .and_then(|s| s.next())
            })
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
        if tag == BlockTag::Finalized && self.fail_finalized.load(Ordering::SeqCst) {
            anyhow::bail!("503 Service Unavailable");
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
        if self.failing_headers.lock().unwrap().contains(&h) {
            anyhow::bail!("503 Service Unavailable");
        }
        Ok(self.by_hash.lock().unwrap().get(&h).cloned())
    }

    async fn receipt(&self, h: B256) -> anyhow::Result<Option<ReceiptLite>> {
        if self
            .fail_receipts
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            anyhow::bail!("503 Service Unavailable");
        }
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
        if let Some(left) = self.vanishing_txs.lock().unwrap().get_mut(&h) {
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
            BlockTag::Number(n) => match self.counts_at.lock().unwrap().get(&(a, n)) {
                Some(c) => return Ok(*c),
                None => "pending",
            },
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

    async fn estimate_gas_at(
        &self,
        _: Address,
        to: Address,
        _: Bytes,
        at: BlockTag,
    ) -> anyhow::Result<u64> {
        let left = self.estimate_transport_failures.load(Ordering::SeqCst);
        if left > 0 {
            self.estimate_transport_failures
                .store(left - 1, Ordering::SeqCst);
            anyhow::bail!("connection reset by peer");
        }
        if let Some(r) = self.estimate_revert.lock().unwrap().clone() {
            return Err(EstimateReverted(r).into());
        }
        match at {
            BlockTag::Number(n) => self.pinned_reads.lock().unwrap().push(n),
            _ => {
                if let Some(r) = self.stale_estimate_reverts.lock().unwrap().get(&to) {
                    return Err(EstimateReverted(r.clone()).into());
                }
            },
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

/// Signs with a key the test holds and broadcasts through the RPC. Test
/// builds only: the shipped CLI never holds an EVM key.
pub struct LocalKeyWallet {
    /// The key it signs with.
    pub signer: alloy::signers::local::PrivateKeySigner,
    /// Where it broadcasts.
    pub rpc_url: String,
    /// Sends legacy (type 0) transactions, as some wallets do.
    pub legacy: bool,
    /// Signs `approve` with this limit instead of the requested one, as a
    /// wallet does when its user lowers the spending limit.
    pub approve_cap: Option<U256>,
    /// How long one request to the node may take: a node that never answers
    /// ends the send with an error, as a wallet's own timeout would.
    pub request_timeout: std::time::Duration,
}

#[async_trait]
impl crate::deposit::wallet::Wallet for LocalKeyWallet {
    fn kind(&self) -> crate::deposit::wallet::WalletKind {
        crate::deposit::wallet::WalletKind::WalletConnect
    }

    async fn connect(
        &mut self,
        _: &dyn crate::deposit::ui::Ui,
    ) -> Result<Address, crate::deposit::wallet::WalletError> {
        Ok(self.signer.address())
    }

    async fn personal_sign(
        &mut self,
        _: Address,
        m: &str,
    ) -> Result<Bytes, crate::deposit::wallet::WalletError> {
        use alloy::signers::SignerSync as _;
        Ok(Bytes::from(
            self.signer
                .sign_message_sync(m.as_bytes())
                .unwrap()
                .as_bytes()
                .to_vec(),
        ))
    }

    async fn capabilities(&mut self, _: Address) -> Option<serde_json::Value> {
        None
    }

    async fn send_transaction(
        &mut self,
        _: &dyn crate::deposit::ui::Ui,
        tx: &crate::deposit::wallet::TxRequest,
    ) -> Result<B256, crate::deposit::wallet::WalletError> {
        use alloy::{
            network::{EthereumWallet, TransactionBuilder},
            providers::{Provider, ProviderBuilder},
            rpc::types::TransactionRequest,
        };
        let client = alloy::transports::http::reqwest::Client::builder()
            .timeout(self.request_timeout)
            .build()
            .map_err(|e| crate::deposit::wallet::WalletError::Other(e.to_string()))?;
        let p = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.signer.clone()))
            .connect_reqwest(client, self.rpc_url.parse().unwrap());
        let data = match (&tx.purpose, self.approve_cap) {
            (
                crate::deposit::wallet::TxPurpose::Approve {
                    spender, ..
                },
                Some(cap),
            ) => approve_calldata(*spender, cap),
            _ => tx.data.clone(),
        };
        let mut req = TransactionRequest::default()
            .with_from(tx.from)
            .with_to(tx.to)
            .with_input(data)
            .with_gas_limit(tx.gas);
        if self.legacy {
            req = req.with_gas_price(tx.fees.max_fee_per_gas);
        } else {
            req = req
                .with_max_fee_per_gas(tx.fees.max_fee_per_gas)
                .with_max_priority_fee_per_gas(tx.fees.max_priority_fee_per_gas);
        }
        p.send_transaction(req)
            .await
            .map(|pending| *pending.tx_hash())
            .map_err(|e| crate::deposit::wallet::WalletError::Other(e.to_string()))
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
    /// The request URI of every connection, in the order of the handshakes.
    uris: std::sync::Arc<Mutex<Vec<String>>>,
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
    /// Publications with these tags are answered with an error and dropped.
    refused_tags: HashSet<u64>,
    /// How the next `irn_subscribe` calls are answered, one each; an empty
    /// queue answers as a relay does.
    subscribe_answers: VecDeque<SubscribeAnswer>,
}

/// How [`MockRelay`] answers one `irn_subscribe` it was told to treat
/// differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscribeAnswer {
    /// With an error; nothing is subscribed.
    Refuse,
    /// Not at all, and nothing is subscribed, as when the request is lost.
    Drop,
    /// Subscribes and hands over the topic's backlog first, the answer
    /// after it.
    AfterBacklog,
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
        let uris = std::sync::Arc::new(Mutex::new(Vec::new()));
        let (st, k, sl, n, u) = (
            state.clone(),
            kill.clone(),
            stall.clone(),
            accepted.clone(),
            uris.clone(),
        );
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                n.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(MockRelay::serve(
                    tcp,
                    st.clone(),
                    k.subscribe(),
                    sl.subscribe(),
                    u.clone(),
                ));
            }
        });
        MockRelay {
            addr,
            state,
            kill,
            stall,
            accepted,
            uris,
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

    /// The request URI (path and query) of every connection so far,
    /// reconnects included, in the order of their handshakes.
    pub fn request_uris(&self) -> Vec<String> {
        self.uris.lock().unwrap().clone()
    }

    /// Answers the next `irn_subscribe` calls as `answers` says, one each.
    pub fn answer_next_subscribes(&self, answers: impl IntoIterator<Item = SubscribeAnswer>) {
        self.state
            .lock()
            .unwrap()
            .subscribe_answers
            .extend(answers);
    }

    /// From now on, answers every publication tagged `tag` with an error
    /// and drops it.
    pub fn refuse_publishes_tagged(&self, tag: u32) {
        self.state
            .lock()
            .unwrap()
            .refused_tags
            .insert(u64::from(tag));
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
        uris: std::sync::Arc<Mutex<Vec<String>>>,
    ) {
        use futures::{SinkExt as _, StreamExt as _};
        use tokio_tungstenite::tungstenite::{
            handshake::server::{Request, Response},
            Message,
        };

        #[expect(
            clippy::result_large_err,
            reason = "the handshake callback's signature is tungstenite's"
        )]
        let note_uri = move |req: &Request, resp: Response| {
            uris.lock().unwrap().push(req.uri().to_string());
            Ok(resp)
        };
        let Ok(ws) = tokio_tungstenite::accept_hdr_async(tcp, note_uri).await else {
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
                            let answer = s.subscribe_answers.pop_front();
                            if answer == Some(SubscribeAnswer::Refuse) {
                                let no = serde_json::json!({"id": v["id"], "jsonrpc": "2.0",
                                    "error": {"code": -32000, "message": "subscribe refused"}});
                                MockRelay::push(&out_tx, no);
                                continue;
                            }
                            if answer == Some(SubscribeAnswer::Drop) {
                                continue;
                            }
                            s.subs.entry(topic.clone()).or_default().push(out_tx.clone());
                            let ok = serde_json::json!({"id": v["id"], "jsonrpc": "2.0", "result": "sub"});
                            if answer.is_none() {
                                MockRelay::push(&out_tx, ok.clone());
                            }
                            for (msg, tag) in s.backlog.get(&topic).cloned().unwrap_or_default() {
                                MockRelay::push(&out_tx, MockRelay::delivery(1, &topic, &msg, tag));
                            }
                            if answer == Some(SubscribeAnswer::AfterBacklog) {
                                MockRelay::push(&out_tx, ok);
                            }
                        }
                        Some("irn_publish") => {
                            let msg = v["params"]["message"].as_str().unwrap().to_string();
                            let tag = v["params"]["tag"].as_u64().unwrap();
                            if s.refused_tags.contains(&tag) {
                                let no = serde_json::json!({"id": v["id"], "jsonrpc": "2.0",
                                    "error": {"code": -32000, "message": "publish refused"}});
                                MockRelay::push(&out_tx, no);
                                continue;
                            }
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
    ///
    /// A copy whose publish fails is published again, as a wallet does: the
    /// relay client fails every call in flight when it replaces its socket.
    /// Under paused time that happens without any fault. Every time the
    /// runtime waits, for a relay's answer too, the clock jumps to the next
    /// timer, so the peer's read deadline can pass before the acknowledgment
    /// of its own publish is read.
    pub async fn send(
        r: &crate::deposit::wc::relay::Relay,
        topic: &str,
        sym: &[u8; 32],
        v: serde_json::Value,
        tag: u32,
    ) {
        use crate::deposit::wc::crypto::{random_bytes, seal_type0};

        /// Publishes of one copy before the peer gives up.
        const TRIES: u32 = 10;
        for _ in 0..2 {
            let sealed = seal_type0(sym, random_bytes(), v.to_string().as_bytes());
            let mut tries = 1;
            while let Err(e) = r.publish(topic, &sealed, 300, tag).await {
                assert!(tries < TRIES, "the wallet peer could not publish: {e:#}");
                tries += 1;
                // Lets the relay client notice a broken socket first.
                tokio::task::yield_now().await;
            }
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
    /// message)`. Each lands in front of its account's list, as the newest.
    pub on_send_ext_out: Mutex<Vec<([u8; 32], MsgView)>>,
    /// Accounts as they are once a send was made: `(account, state)`.
    pub on_send_accounts: Mutex<Vec<([u8; 32], AccountInfo)>>,
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
    /// Getters, by name, whose every call fails.
    pub failing_getters: Mutex<HashSet<String>>,
    /// Blocks whose `isAcceptedBlockHash` read fails.
    pub failing_accepted: Mutex<HashSet<B256>>,
    /// Accounts whose external-message lists fail to read.
    pub failing_ext: Mutex<HashSet<[u8; 32]>>,
    /// Every `message` call fails.
    pub fail_message_reads: AtomicBool,
    /// Accounts whose every read fails.
    pub failing_accounts: Mutex<HashSet<[u8; 32]>>,
    /// Account id → how many more reads find no account before the one in
    /// `accounts` shows.
    pub accounts_hidden_for: Mutex<HashMap<[u8; 32], u32>>,
    /// External-message and transaction lists are served this many items
    /// a page, the cursor being where the next page starts; 0 serves each
    /// list as one page.
    pub page_size: AtomicUsize,
    /// How many list pages were served, both kinds of list together.
    pub pages_read: AtomicU32,
}

impl FakeAn {
    /// Scripts the answers of getter `f` on account `id`.
    pub fn getter(&self, id: [u8; 32], f: &str, seq: Vec<serde_json::Value>) {
        self.getters
            .lock()
            .unwrap()
            .insert((id, f.into()), Script::new(seq));
    }

    /// The page of `all` that starts at `cursor`, `page_size` items long.
    fn page<T: Clone>(&self, all: Vec<T>, cursor: Option<String>) -> Page<T> {
        self.pages_read.fetch_add(1, Ordering::SeqCst);
        let size = self.page_size.load(Ordering::SeqCst);
        let start = cursor
            .map_or(0, |c| c.parse().expect("a cursor this fake handed out"))
            .min(all.len());
        let end = if size == 0 {
            all.len()
        } else {
            (start + size).min(all.len())
        };
        Page {
            cursor: (end < all.len()).then(|| end.to_string()),
            items: all[start..end].to_vec(),
        }
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
        if self.failing_accounts.lock().unwrap().contains(&id) {
            anyhow::bail!("503 Service Unavailable");
        }
        if let Some(n @ 1..) = self.accounts_hidden_for.lock().unwrap().get_mut(&id) {
            *n -= 1;
            return Ok(None);
        }
        Ok(self.accounts.lock().unwrap().get(&id).cloned())
    }

    async fn run_getter(
        &self,
        id: [u8; 32],
        _: &str,
        f: &str,
        input: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        if self.fail_getters.load(Ordering::SeqCst)
            || self.failing_getters.lock().unwrap().contains(f)
        {
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
            if self.failing_accepted.lock().unwrap().contains(&h) {
                anyhow::bail!("503 Service Unavailable");
            }
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
        before: Option<String>,
    ) -> anyhow::Result<Page<MsgView>> {
        if self.fail_ext_messages.load(Ordering::SeqCst)
            || self.failing_ext.lock().unwrap().contains(&id)
        {
            anyhow::bail!("GraphQL timeout");
        }
        let m = match dir {
            ExtDir::In => &self.ext_in,
            ExtDir::Out => &self.ext_out,
        };
        let all = m.lock().unwrap().get(&id).cloned().unwrap_or_default();
        Ok(self.page(all, before))
    }

    async fn transactions(
        &self,
        id: [u8; 32],
        after: Option<String>,
    ) -> anyhow::Result<Page<TxListItem>> {
        if self.fail_transactions.load(Ordering::SeqCst) {
            anyhow::bail!("GraphQL timeout");
        }
        let all = self
            .txs
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .unwrap_or_default();
        Ok(self.page(all, after))
    }

    async fn message(&self, h: &str) -> anyhow::Result<Option<MsgView>> {
        if self.fail_message_reads.load(Ordering::SeqCst) {
            anyhow::bail!("GraphQL timeout");
        }
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
            self.ext_out
                .lock()
                .unwrap()
                .entry(acc)
                .or_default()
                .insert(0, m);
        }
        for (acc, a) in self.on_send_accounts.lock().unwrap().drain(..) {
            self.accounts.lock().unwrap().insert(acc, a);
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

/// The temporary directory of a [`fake_prover_dir`], deleted when this is
/// dropped, and the hold on [`crate::test_forks::spawning`] that its test
/// keeps for as long as it can start the fake tools.
pub struct FakeProverHome {
    /// The directory.
    _dir: tempfile::TempDir,
    /// Taken before the tools were written.
    _spawning: crate::test_forks::Hold,
}

/// A prover directory that passes `check_prover_dir` and whose
/// `export_blake2b_proof` copies `expected_pi.bin` (written by the test,
/// the `proof_00` fixture's public inputs until then) into place. `extra`
/// runs inside `export_blake2b_proof` before it writes. Each run of it
/// appends a line to `data/runs.log`. The fetcher exits 7 without
/// `ETH_RPC_URL` in its environment. Both tools answer `--help` with exit
/// 0 before anything else, as the preflight's probe expects.
///
/// Whoever has one may start its tools, so it holds
/// [`crate::test_forks::spawning`] until it is dropped: its test runs
/// while no other test starts a subprocess or takes a released lock again.
pub fn fake_prover_dir(extra: &str) -> (FakeProverHome, crate::deposit::prover_files::ProverDir) {
    use std::os::unix::fs::PermissionsExt;
    let spawning = crate::test_forks::spawning();
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("configs")).unwrap();
    std::fs::create_dir_all(d.path().join("data")).unwrap();
    std::fs::write(
        d.path().join(crate::deposit::prover_files::CIRCUIT_PARAMS),
        include_str!("../../../../deposit-prover/configs/circuit_params.json"),
    )
    .unwrap();
    let mut srs = vec![0u8; 4096];
    let mut tail = [0u8; 128];
    tail[..6].copy_from_slice(&crate::deposit::prover_files::HERMEZ_SG2_HEAD);
    tail[122..].copy_from_slice(&crate::deposit::prover_files::HERMEZ_SG2_TAIL);
    srs.extend_from_slice(&tail);
    std::fs::write(d.path().join(crate::deposit::prover_files::SRS_FILE), srs).unwrap();
    std::fs::write(
        d.path().join("expected_pi.bin"),
        crate::deposit::pi::tests_support::fixture_pi(),
    )
    .unwrap();
    // `--help` answers at once, as a clap tool does, and is no run.
    let help = "[ \"$1\" = --help ] && { echo usage; exit 0; }";
    let fetch = format!(
        "#!/bin/sh\n{help}\nwhile [ $# -gt 0 ]; do case $1 in --output) out=$2; shift;; esac; \
         shift; done\n[ -n \"$ETH_RPC_URL\" ] || exit 7\necho '{{}}' > \"$out\"\n"
    );
    let prove = format!(
        "#!/bin/sh\n{help}\nwhile [ $# -gt 0 ]; do case $1 in --proof-out) p=$2; shift;; \
         --pubin-out) i=$2; shift;; esac; shift; done\n{extra}\nprintf proof > \"$p\"\ncp \
         expected_pi.bin \"$i\"\necho run >> data/runs.log\n"
    );
    for (name, body) in [
        (crate::deposit::prover_files::FETCH_BIN, fetch),
        (crate::deposit::prover_files::PROVE_BIN, prove),
    ] {
        let p = d.path().join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let dir = crate::deposit::prover_files::ProverDir {
        root: d.path().to_path_buf(),
    };
    let home = FakeProverHome {
        _dir: d,
        _spawning: spawning,
    };
    (home, dir)
}

/// Makes the prover of `dir` write both its files, as the exporter does
/// before it checks the proof itself, and then exit 1 as when that check
/// fails. Each run still appends a line to `data/runs.log`.
pub fn prover_fails_after_writing(dir: &crate::deposit::prover_files::ProverDir) {
    let body = "#!/bin/sh\n[ \"$1\" = --help ] && { echo usage; exit 0; }\nwhile [ $# -gt 0 ]; do \
                case $1 in --proof-out) p=$2; shift;; --pubin-out) i=$2; shift;; esac; shift; \
                done\nprintf proof > \"$p\"\ncp expected_pi.bin \"$i\"\necho run >> \
                data/runs.log\necho 'the proof does not verify' >&2\nexit 1\n";
    std::fs::write(dir.bin(crate::deposit::prover_files::PROVE_BIN), body).unwrap();
}

// ---- the world of the end-to-end driver tests ----

/// The EVM bridge of [`World`].
pub const W_BRIDGE: Address =
    alloy_primitives::address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");
/// The token the bridge of [`World`] takes.
pub const W_USDC: Address = alloy_primitives::address!("1c7d4b196cb0c7b01d743fbc6116a902379c7238");
/// The Acki Nacki bridge account of [`World`].
pub const W_BRIDGE_ACC: [u8; 32] = [0x1a; 32];
/// The light client the Acki Nacki bridge of [`World`] names.
pub const W_LC: [u8; 32] = [0x20; 32];
/// The recipient account of [`World`].
pub const W_ACC: [u8; 32] = [0xa3; 32];
/// The deposit of [`World`], in micro-USDC.
pub const W_AMOUNT: u64 = 12_500_000;
/// When the owner's anchors were switched off in [`World`].
const W_T_FLIP: u64 = 1_786_000_000;

/// A deposit transaction [`World`] put on its chain.
#[derive(Clone)]
pub struct Mined {
    /// The transaction.
    pub tx: B256,
    /// The block that holds it.
    pub block: Header,
    /// The bridge's deposit id.
    pub deposit_id: U256,
    /// The sender.
    pub from: Address,
}

/// One set of scripted chains, prover directory, state and work
/// directories and wallet for the end-to-end tests of the driver. Its
/// deposit uses the `W_*` values. The fake prover answers with the public
/// inputs `World` writes for every deposit it mines (`expected_pi.bin`),
/// so the check of the prover's inputs runs for real.
pub struct World {
    /// The EVM node.
    pub evm: std::sync::Arc<FakeEvm>,
    /// The Acki Nacki node.
    pub an: std::sync::Arc<FakeAn>,
    /// The prover directory and what `check_prover_dir` made of it.
    pub prover: (FakeProverHome, crate::deposit::prover_files::ProverDir),
    /// `--state-dir`.
    pub state: tempfile::TempDir,
    /// `--work-dir`.
    pub work: tempfile::TempDir,
    /// The wallet.
    pub wallet: FakeWallet,
    /// The deposit mined last.
    pub mined: Option<Mined>,
    /// The deposit id [`World::mined_deposit`] gives next, less one.
    next_id: u64,
}

/// A block hash whose two byte orders differ: bytes `tag, tag+1, …`.
fn seq_hash(tag: u8) -> B256 {
    let mut b = [0u8; 32];
    for (i, x) in b.iter_mut().enumerate() {
        *x = tag.wrapping_add(i as u8);
    }
    B256::from(b)
}

/// Block `number` of [`World`]'s chain, timed like [`header`]'s, but with
/// a [`seq_hash`] hash: it reads differently in the light client's key
/// form and with its halves swapped, so a test that passes the hash in the
/// wrong byte order finds no anchor and no matching public input.
fn world_header(number: u64, tag: u8) -> Header {
    let h = Header {
        number,
        hash: seq_hash(tag),
        parent_hash: seq_hash(tag.wrapping_sub(1)),
        timestamp: header(number, tag).timestamp,
    };
    assert_ne!(
        B256::from(crate::deposit::identity::pi_form(h.hash.0)),
        h.hash,
        "a block whose two forms differ"
    );
    assert_ne!(
        h.hash[..16],
        h.hash[16..],
        "a block whose halves differ, so swapping them changes it"
    );
    h
}

/// A uint as the SDK renders it in a decoded body: a decimal string.
fn uint_json(v: impl std::fmt::Display) -> serde_json::Value {
    serde_json::json!(v.to_string())
}

impl World {
    /// Everything a deposit needs, healthy: the EVM bridge and its token,
    /// a funded sender with nothing approved, and an Acki Nacki bridge
    /// version 1.6.0, not paused, trusting the EVM bridge, anchored by its
    /// owner, deploying the voucher this build knows. The light client's
    /// head is no block on the chain.
    pub fn healthy() -> World {
        use alloy::sol_types::{SolCall, SolValue};

        use crate::deposit::{
            an::AccStatus,
            evm::{IDeposit, IErc20},
        };
        let evm = FakeEvm::sepolia();
        evm.codes
            .lock()
            .unwrap()
            .insert(W_BRIDGE, Bytes::from(vec![0x60, 0x80]));
        evm.set_call(
            W_BRIDGE,
            IDeposit::usdcCall::SELECTOR,
            W_USDC.abi_encode().into(),
        );
        evm.set_call(
            W_USDC,
            IErc20::decimalsCall::SELECTOR,
            U256::from(6u8).abi_encode().into(),
        );
        evm.set_call(
            W_USDC,
            IErc20::balanceOfCall::SELECTOR,
            U256::from(1_000_000_000_000u64).abi_encode().into(),
        );
        // Nothing approved yet; the approve step's re-read sees the amount.
        evm.script_call(W_USDC, IErc20::allowanceCall::SELECTOR, vec![
            U256::ZERO.abi_encode().into(),
            U256::from(W_AMOUNT).abi_encode().into(),
        ]);
        evm.latest.set([header(1000, 0xf0)]);
        evm.finalized.set([Some(header(990, 0xf1))]);
        let an = FakeAn::default();
        let active = |dapp: [u8; 32]| AccountInfo {
            status: AccStatus::Active,
            dapp_id: Some(dapp),
            ecc3: 0,
        };
        an.accounts
            .lock()
            .unwrap()
            .insert(W_BRIDGE_ACC, active([0; 32]));
        an.accounts.lock().unwrap().insert(W_ACC, active([0; 32]));
        an.getter(W_BRIDGE_ACC, "getVersion", vec![
            serde_json::json!({ "value0": "1.6.0", "value1": "eccUSDCBridge" }),
        ]);
        an.getter(W_BRIDGE_ACC, "isPaused", vec![
            serde_json::json!({ "value0": false }),
        ]);
        an.getter(W_BRIDGE_ACC, "isTrustedL1Bridge", vec![
            serde_json::json!({ "value0": true }),
        ]);
        an.getter(W_BRIDGE_ACC, "getAnchorConfig", vec![
            serde_json::json!({ "lightClient": format!("0:{}", hex::encode(W_LC)), "ownerAnchorsEnabled": true }),
        ]);
        an.getter(W_BRIDGE_ACC, "getDepositVoucherCodeHash", vec![
            serde_json::json!({ "value0": format!("0x{}", hex::encode(crate::deposit::identity::EXPECTED_VOUCHER_CODE_HASH)) }),
        ]);
        an.getter(W_LC, "getConfig", vec![
            serde_json::json!({ "l1ChainId": "11155111" }),
        ]);
        an.getter(W_LC, "getHead", vec![
            serde_json::json!({ "executionBlockHash": format!("{:#x}", B256::repeat_byte(0xee)) }),
        ]);
        World {
            evm: std::sync::Arc::new(evm),
            an: std::sync::Arc::new(an),
            prover: fake_prover_dir(""),
            state: tempfile::tempdir().unwrap(),
            work: tempfile::tempdir().unwrap(),
            wallet: FakeWallet::eoa(),
            mined: None,
            next_id: 5,
        }
    }

    /// `--to`: the recipient in dapp zero.
    pub fn target() -> crate::deposit::args::AnTarget {
        crate::deposit::args::AnTarget {
            dapp_id: [0; 32],
            account_id: W_ACC,
        }
    }

    /// A validated command line for this world in `mode`.
    pub fn params(
        &self,
        mode: crate::deposit::args::RunMode,
    ) -> crate::deposit::args::DepositParams {
        use std::time::Duration;
        crate::deposit::args::DepositParams {
            mode,
            globals: Default::default(),
            network: Some(crate::deposit::args::Network::Sepolia),
            amount: Some(crate::args::UsdcAmount(u128::from(W_AMOUNT))),
            to: Some(Self::target()),
            rpc_url: Some("http://rpc.invalid/key".into()),
            bridge: Some(W_BRIDGE),
            gql_endpoint: Some("http://gql.invalid/graphql".into()),
            usdc_bridge_account: Some(W_BRIDGE_ACC),
            prover_dir: Some(self.prover.1.root.clone()),
            confirmations: 12,
            state_dir: self.state.path().to_path_buf(),
            work_dir: Some(self.work.path().to_path_buf()),
            prover_timeout: Duration::from_secs(10),
            anchor_timeout: Some(Duration::from_secs(3600)),
            relayer_grace: Duration::from_secs(1),
            recovery_window: Duration::from_secs(60),
            credit_timeout: Duration::from_secs(300),
            pair_timeout: Duration::from_secs(60),
            qr_mode: crate::deposit::args::QrMode::Walletconnect,
            qr_out: None,
            uri_only: true,
            qr_invert: false,
            wc_project_id: Some("test".into()),
            wc_relay_url: "ws://relay.invalid".into(),
            from_address: None,
        }
    }

    /// [`World::params`] for a deposit one micro-USDC larger.
    pub fn params_with_other_amount(
        &self,
        mode: crate::deposit::args::RunMode,
    ) -> crate::deposit::args::DepositParams {
        let mut p = self.params(mode);
        p.amount = Some(crate::args::UsdcAmount(u128::from(W_AMOUNT + 1)));
        p
    }

    /// The driver's dependencies on this world's chains, with a recording
    /// UI that answers yes, one-second polls and bridge 1.6.0 as the
    /// minimum.
    pub fn deps(&self) -> crate::deposit::preflight::Deps {
        use std::time::Duration;
        crate::deposit::preflight::Deps {
            evm: self.evm.clone(),
            an: self.an.clone(),
            ui: std::sync::Arc::new(crate::deposit::ui::RecordingUi::new(true)),
            polls: crate::deposit::preflight::Polls {
                evm: Duration::from_secs(1),
                anchor: Duration::from_secs(1),
                credit: Duration::from_secs(1),
                wallet: Duration::from_secs(1),
            },
            min_bridge: Some(crate::deposit::an_preflight::BridgeVersion(1, 6, 0)),
            current_op: Default::default(),
        }
    }

    /// Pauses or unpauses the Acki Nacki bridge.
    pub fn paused(&self, yes: bool) {
        self.an.getter(W_BRIDGE_ACC, "isPaused", vec![
            serde_json::json!({ "value0": yes }),
        ]);
    }

    /// Switches the owner's anchors on or off; the light client stays.
    pub fn owner_anchors(&self, enabled: bool) {
        self.an.getter(W_BRIDGE_ACC, "getAnchorConfig", vec![
            serde_json::json!({ "lightClient": format!("0:{}", hex::encode(W_LC)), "ownerAnchorsEnabled": enabled }),
        ]);
    }

    /// Head H, checkpoint C (one epoch behind) and C's parent P, all after
    /// the switch-off, all fresh, all on the EVM chain. Their hashes read
    /// differently in the two byte orders.
    fn light_client_blocks(&self) -> (Header, Header, Header) {
        let fin_ts = self
            .evm
            .finalized
            .next()
            .flatten()
            .map(|h| h.timestamp)
            .unwrap_or(W_T_FLIP + 10_000);
        let h = Header {
            number: 980,
            hash: seq_hash(0xe1),
            parent_hash: seq_hash(0xe0),
            timestamp: fin_ts - 100,
        };
        let c_ts = h.timestamp - 384;
        let p = Header {
            number: 947,
            hash: seq_hash(0xc0),
            parent_hash: seq_hash(0xbf),
            timestamp: c_ts - 12,
        };
        let c = Header {
            number: 948,
            hash: seq_hash(0xc1),
            parent_hash: p.hash,
            timestamp: c_ts,
        };
        for b in [&h, &c, &p] {
            self.evm.by_hash.lock().unwrap().insert(b.hash, b.clone());
        }
        (h, c, p)
    }

    /// A light client that can anchor a deposit alone: its head is
    /// accepted by the bridge in Ethereum byte order, the owner switched
    /// anchors off, and a fresh ancestry after that reached the bridge.
    pub fn light_client_ready(&self) {
        let (h, c, p) = self.light_client_blocks();
        self.an.getter(W_LC, "getHead", vec![
            serde_json::json!({ "executionBlockHash": format!("{:#x}", h.hash) }),
        ]);
        self.an.accepted.lock().unwrap().insert(h.hash, 0);
        self.an.accepted.lock().unwrap().insert(p.hash, 0);
        let mut flip = msg(
            "flip",
            "ExtIn",
            "",
            &format!("0:{}", hex::encode(W_BRIDGE_ACC)),
            Some("B-disable"),
        );
        self.an
            .ext_in
            .lock()
            .unwrap()
            .entry(W_BRIDGE_ACC)
            .or_default()
            .push(flip.clone());
        flip.dst_tx = Some(crate::deposit::an::TxRef {
            hash: "flip-tx".into(),
            aborted: false,
            account: String::new(),
            now: W_T_FLIP,
        });
        self.an.messages.lock().unwrap().insert("flip".into(), flip);
        self.an.bodies.lock().unwrap().insert(
            "B-disable".into(),
            ("disableOwnerAnchors".into(), serde_json::json!({})),
        );
        let mut anc = msg(
            "anc",
            "ExtOut",
            &format!("0:{}", hex::encode(W_LC)),
            crate::deposit::lc_readiness::ANCESTRY_ACCEPTED_DST,
            Some("B-anc"),
        );
        anc.created_at = h.timestamp;
        self.an
            .ext_out
            .lock()
            .unwrap()
            .entry(W_LC)
            .or_default()
            .push(anc);
        self.an.bodies.lock().unwrap().insert(
            "B-anc".into(),
            (
                "AncestryAccepted".into(),
                serde_json::json!({ "checkpointHash": format!("{:#x}", c.hash), "hashesAdded": "31" }),
            ),
        );
    }

    /// The state of 2026-09-28: the head is found only in the reversed-halves
    /// form, and the bridge does not accept its real hash.
    pub fn light_client_head_in_pi_form(&self) {
        let (h, _, _) = self.light_client_blocks();
        let pi = B256::from(crate::deposit::identity::pi_form(h.hash.0));
        assert_ne!(pi, h.hash, "a head whose two forms differ");
        self.an.getter(W_LC, "getHead", vec![
            serde_json::json!({ "executionBlockHash": format!("{pi:#x}") }),
        ]);
    }

    /// A mined `approve`; its hash.
    pub fn approve_hash(&self) -> B256 {
        let h = B256::repeat_byte(0xaa);
        self.evm.script_receipt(h, vec![Some(deposit_receipt(
            h,
            &world_header(899, 0x89),
            true,
            vec![],
        ))]);
        h
    }

    /// Makes the fake prover answer with the public inputs of `m`.
    fn write_expected_pi(&self, m: &Mined) {
        let e = crate::deposit::pi::ExpectedInputs {
            deposit_id: m.deposit_id,
            sender: m.from,
            amount: W_AMOUNT,
            contract: W_BRIDGE,
            chain_id: 11_155_111,
            dapp_id: [0; 32],
            account_id: W_ACC,
            block_hash: m.block.hash,
        };
        let bytes = crate::deposit::pi::DepositPublicInputs::from_expected(&e).encode();
        std::fs::write(self.prover.1.root.join("expected_pi.bin"), bytes).unwrap();
    }

    /// Puts a deposit transaction from the wallet's account at `nonce` in
    /// `block`, with its receipt, its log (when it succeeded) and the
    /// sender's nonce counts past it.
    fn mine(
        &mut self,
        nonce: u64,
        tx_type: u8,
        status: bool,
        block: Header,
        deposit_id: U256,
        amount: u64,
    ) -> B256 {
        let from = self.wallet.account;
        let tx = B256::from(U256::from(0xdead_0000u64 + nonce));
        let calldata = crate::deposit::evm::deposit_calldata(amount, B256::from(W_ACC));
        let dlog = deposit_log(
            W_BRIDGE,
            deposit_id,
            from,
            amount,
            B256::from(W_ACC),
            Some(41),
        );
        let logs = if status {
            vec![transfer_log(W_USDC), dlog.clone()]
        } else {
            vec![]
        };
        self.evm
            .txs
            .lock()
            .unwrap()
            .insert(tx, deposit_tx(tx, from, W_BRIDGE, nonce, tx_type, calldata));
        self.evm
            .script_receipt(tx, vec![Some(deposit_receipt(tx, &block, status, logs))]);
        self.evm
            .by_hash
            .lock()
            .unwrap()
            .insert(block.hash, block.clone());
        for (tag, n) in [
            ("pending", nonce),
            ("latest", nonce + 1),
            ("finalized", nonce + 1),
        ] {
            self.evm.counts.lock().unwrap().insert((from, tag), n);
        }
        if status {
            self.evm.logs.lock().unwrap().push(DepositLogRef {
                tx_hash: tx,
                block_number: block.number,
                block_hash: block.hash,
                log: dlog,
            });
        }
        let m = Mined {
            tx,
            block,
            deposit_id,
            from,
        };
        self.write_expected_pi(&m);
        self.mined = Some(m);
        tx
    }

    /// A successful deposit of `amount` at `nonce` with the next deposit
    /// id, in block `number` (block hash tag `tag`).
    pub fn mined_deposit_in_block(
        &mut self,
        nonce: u64,
        amount: u64,
        number: u64,
        tag: u8,
    ) -> B256 {
        self.next_id += 1;
        let id = U256::from(self.next_id);
        self.mine(nonce, 2, true, world_header(number, tag), id, amount)
    }

    /// A successful deposit at `nonce` with the next deposit id.
    pub fn mined_deposit(&mut self, nonce: u64) -> B256 {
        self.next_id += 1;
        let id = U256::from(self.next_id);
        self.mine(nonce, 2, true, world_header(900, 0x90), id, W_AMOUNT)
    }

    /// A deposit in the slot with another amount, as a wallet that changed
    /// it would send.
    pub fn mined_deposit_of_amount(&mut self, nonce: u64, amount: u64) -> B256 {
        self.mine(
            nonce,
            2,
            true,
            world_header(900, 0x90),
            U256::from(6),
            amount,
        )
    }

    /// A successful deposit at `nonce` of transaction type `tx_type`.
    pub fn mined_deposit_of_type(&mut self, nonce: u64, tx_type: u8) -> B256 {
        self.mine(
            nonce,
            tx_type,
            true,
            world_header(900, 0x90),
            U256::from(6),
            W_AMOUNT,
        )
    }

    /// A deposit at `nonce` that reverted.
    pub fn reverted_deposit_finalized(&mut self, nonce: u64) -> B256 {
        self.mine(
            nonce,
            2,
            false,
            world_header(900, 0x90),
            U256::ZERO,
            W_AMOUNT,
        )
    }

    /// The mined deposit's block turns accepted after `polls` reads.
    pub fn anchor_after(&self, polls: usize) {
        let m = self.mined.as_ref().expect("mine a deposit first");
        self.an.accepted.lock().unwrap().insert(m.block.hash, polls);
    }

    /// After two readings in its first block, the transaction is found in
    /// block 901 with depositId 8; only that block ever gets an anchor.
    pub fn reorg_after_confirmation(&mut self, tx: B256, polls: usize) {
        let old = self.mined.clone().expect("mine a deposit first");
        let new_block = world_header(901, 0x91);
        let from = self.wallet.account;
        let log = deposit_log(
            W_BRIDGE,
            U256::from(8),
            from,
            W_AMOUNT,
            B256::from(W_ACC),
            Some(3),
        );
        let r = |b: &Header, logs| Some(deposit_receipt(tx, b, true, logs));
        let old_logs = || {
            vec![
                transfer_log(W_USDC),
                deposit_log(
                    W_BRIDGE,
                    old.deposit_id,
                    from,
                    W_AMOUNT,
                    B256::from(W_ACC),
                    Some(41),
                ),
            ]
        };
        self.evm.script_receipt(tx, vec![
            r(&old.block, old_logs()),
            r(&old.block, old_logs()),
            r(&new_block, vec![transfer_log(W_USDC), log]),
        ]);
        self.evm
            .by_hash
            .lock()
            .unwrap()
            .insert(new_block.hash, new_block.clone());
        let m = Mined {
            tx,
            block: new_block,
            deposit_id: U256::from(8),
            from,
        };
        self.write_expected_pi(&m);
        self.an.accepted.lock().unwrap().insert(m.block.hash, polls);
        self.mined = Some(m);
    }

    /// The Acki Nacki side of a successful finalize for the mined deposit:
    /// the send is executed, and the voucher → confirmDeposit → transfer →
    /// delivery chain and the DepositFinalized event are all visible.
    pub fn credit_chain_for_mined_deposit(&self) {
        use crate::deposit::{
            an::{AccStatus, TxRef},
            identity::{offline_context, voucher_account_id, DepositIdentity},
        };
        let m = self.mined.as_ref().expect("mine a deposit first");
        let id = DepositIdentity {
            deposit_id: m.deposit_id,
            contract: W_BRIDGE,
            chain_id: 11_155_111,
        };
        let voucher = voucher_account_id(&offline_context(), &id).unwrap();
        let fields = serde_json::json!({
            "depositId": uint_json(m.deposit_id), "contractAddr": uint_json(U256::from_be_slice(W_BRIDGE.as_slice())),
            "dappId": "0", "chainId": "11155111", "amount": uint_json(W_AMOUNT), "anAccount": format!("0x{}", hex::encode(W_ACC)),
        });
        self.an
            .sends
            .lock()
            .unwrap()
            .push_back(FinalizeSend::Executed {
                tx_id: "fin".into(),
                aborted: false,
                exit_code: Some(0),
            });
        self.an
            .txs
            .lock()
            .unwrap()
            .insert(voucher, vec![TxListItem {
                hash: "deploy".into(),
                now: 2,
                orig_status: AccStatus::NonExist,
                end_status: AccStatus::Active,
                aborted: false,
                out_msgs: vec!["confirm".into()],
            }]);
        let bridge = format!("0:{}", hex::encode(W_BRIDGE_ACC));
        let mut confirm = msg(
            "confirm",
            "Internal",
            &format!("0:{}", hex::encode(voucher)),
            &bridge,
            Some("B-confirm"),
        );
        confirm.dst_tx = Some(TxRef {
            hash: "btx".into(),
            aborted: false,
            account: bridge.clone(),
            now: 3,
        });
        self.an
            .messages
            .lock()
            .unwrap()
            .insert("confirm".into(), confirm);
        self.an.bodies.lock().unwrap().insert(
            "B-confirm".into(),
            ("confirmDeposit".into(), fields.clone()),
        );
        let mut transfer = msg(
            "xfer",
            "Internal",
            &bridge,
            &format!("0:{}", hex::encode(W_ACC)),
            None,
        );
        transfer.ecc = vec![(3, u128::from(W_AMOUNT))];
        transfer.bounce = Some(false);
        let mut event = msg(
            "ev",
            "ExtOut",
            &bridge,
            crate::deposit::credit::DEPOSIT_FINALIZED_DST,
            Some("B-event"),
        );
        event.created_at = u64::MAX / 2;
        self.an
            .bodies
            .lock()
            .unwrap()
            .insert("B-event".into(), ("DepositFinalized".into(), fields));
        self.an
            .transactions
            .lock()
            .unwrap()
            .insert("btx".into(), TxView {
                hash: "btx".into(),
                aborted: false,
                exit_code: Some(0),
                account: bridge.clone(),
                now: 3,
                out: vec![transfer.clone(), event.clone()],
                ecc_delta: vec![],
            });
        let mut delivered = transfer;
        delivered.dst_tx = Some(TxRef {
            hash: "rtx".into(),
            aborted: false,
            account: String::new(),
            now: 4,
        });
        self.an
            .messages
            .lock()
            .unwrap()
            .insert("xfer".into(), delivered);
        self.an
            .transactions
            .lock()
            .unwrap()
            .insert("rtx".into(), TxView {
                hash: "rtx".into(),
                aborted: false,
                exit_code: Some(0),
                account: String::new(),
                now: 4,
                out: vec![],
                ecc_delta: vec![(3, i128::from(W_AMOUNT))],
            });
        event.src_tx = Some("btx".into());
        self.an
            .messages
            .lock()
            .unwrap()
            .insert("ev".into(), event.clone());
        self.an
            .ext_out
            .lock()
            .unwrap()
            .entry(W_BRIDGE_ACC)
            .or_default()
            .push(event);
    }

    /// A record of this world's deposit at `stage`, requested from `from`
    /// at `nonce_before`.
    fn record(
        &self,
        stage: crate::deposit::store::OpStage,
        from: Address,
        nonce_before: u64,
    ) -> crate::deposit::store::OpRecord {
        use crate::deposit::store::*;
        let mut r = OpRecord::new(
            Store::new_op_id(),
            OpParams {
                chain_id: 11_155_111,
                bridge: W_BRIDGE,
                to: Self::target().extended(),
                amount_units: W_AMOUNT,
                an_bridge: hex::encode(W_BRIDGE_ACC),
                an_network: "http://gql.invalid:80".into(),
            },
            hex::encode(crate::deposit::identity::EXPECTED_VOUCHER_CODE_HASH),
        );
        r.from = Some(from);
        r.stage = stage;
        r.an_bridge_dapp = Some(hex::encode([0u8; 32]));
        r.request = Some(RequestInfo {
            nonce_before,
            from_block: 800,
            calldata: crate::deposit::evm::deposit_calldata(W_AMOUNT, B256::from(W_ACC)),
            wallet_hash: None,
        });
        r
    }

    /// Writes `r` to the state directory; its operation id.
    fn save(&self, r: &mut crate::deposit::store::OpRecord) -> String {
        crate::deposit::store::Store::open(self.state.path())
            .unwrap()
            .write(r)
            .unwrap();
        r.op_id.clone()
    }

    /// An operation killed after its `Requested` write, before the wallet
    /// answered.
    pub fn left_requested_operation_from(&self, from: Address) -> String {
        let mut r = self.record(crate::deposit::store::OpStage::Requested, from, 7);
        self.save(&mut r)
    }

    /// Killed after the wallet answered with `h` and before its nonce was read.
    pub fn left_requested_operation_with_wallet_hash(&self, from: Address, h: B256) -> String {
        let mut r = self.record(crate::deposit::store::OpStage::Requested, from, 7);
        if let Some(q) = r.request.as_mut() {
            q.wallet_hash = Some(h);
        }
        self.save(&mut r)
    }

    /// An operation killed in `Signed` at `nonce`.
    pub fn left_signed_operation_from(&self, from: Address, nonce: u64) -> String {
        let mut r = self.record(crate::deposit::store::OpStage::Signed, from, nonce);
        r.tx = Some(crate::deposit::store::TxClaim {
            tx_hash: B256::repeat_byte(0x55),
            tx_nonce: nonce,
        });
        self.save(&mut r)
    }

    /// The deposit of operation `_op`, mined at `nonce`.
    pub fn mined_deposit_for_operation(&mut self, _op: &str, nonce: u64) -> B256 {
        self.mined_deposit(nonce)
    }

    /// A: requested at nonce n and abandoned; B: requested at nonce n and
    /// died before its hash; T: B's transaction, mined at nonce n.
    pub fn two_operations_one_slot(&mut self, n: u64) -> (String, String, B256) {
        let from = self.wallet.account;
        let mut a = self.record(crate::deposit::store::OpStage::Abandoned, from, n);
        a.abandoned_ever = true;
        let a = self.save(&mut a);
        let mut b = self.record(crate::deposit::store::OpStage::Requested, from, n);
        let b = self.save(&mut b);
        let t = self.mined_deposit(n);
        (a, b, t)
    }

    /// What an operation records about the mined deposit `m`.
    fn deposit_info(&self, m: &Mined) -> crate::deposit::store::DepositInfo {
        use crate::deposit::identity::{offline_context, voucher_account_id, DepositIdentity};
        let id = DepositIdentity {
            deposit_id: m.deposit_id,
            contract: W_BRIDGE,
            chain_id: 11_155_111,
        };
        let voucher = voucher_account_id(&offline_context(), &id).unwrap();
        crate::deposit::store::DepositInfo {
            deposit_id: m.deposit_id,
            block_number: m.block.number,
            block_hash: m.block.hash,
            block_log_index: 41,
            receipt_log_index: 1,
            access_list_rlp_len: 1,
            voucher_account: hex::encode(voucher),
        }
    }

    /// A finished operation, credited through its voucher.
    pub fn credited_operation(&mut self) -> String {
        self.mined_deposit(7);
        let m = self.mined.clone().unwrap();
        let mut r = self.record(crate::deposit::store::OpStage::Credited, m.from, 7);
        r.tx = Some(crate::deposit::store::TxClaim {
            tx_hash: m.tx,
            tx_nonce: 7,
        });
        r.deposit = Some(self.deposit_info(&m));
        r.credit = Some(crate::deposit::store::CreditInfo {
            confirm_tx: "btx".into(),
            delivery_tx: "rtx".into(),
            via_events: false,
        });
        r.anchor_writer = Some("owner".into());
        self.save(&mut r)
    }

    /// Finalize sent earlier, no proof left on disk.
    pub fn finalizing_operation(&mut self) -> String {
        self.mined_deposit(7);
        let m = self.mined.clone().unwrap();
        let mut r = self.record(crate::deposit::store::OpStage::Finalizing, m.from, 7);
        r.tx = Some(crate::deposit::store::TxClaim {
            tx_hash: m.tx,
            tx_nonce: 7,
        });
        let info = self.deposit_info(&m);
        r.finalize = Some(crate::deposit::store::FinalizeInfo {
            voucher_code_hash: r.voucher_code_hash.clone(),
            voucher_account: info.voucher_account.clone(),
            sends: 1,
        });
        r.deposit = Some(info);
        r.anchor_writer = Some("owner".into());
        self.save(&mut r)
    }

    /// A mined deposit whose operation stopped at `stage` (`Confirmed` or
    /// `Anchored`): confirmed on chain, its work directory recorded and
    /// empty.
    fn stopped_operation(&mut self, stage: crate::deposit::store::OpStage) -> String {
        self.mined_deposit(7);
        let m = self.mined.clone().unwrap();
        let mut r = self.record(stage, m.from, 7);
        r.tx = Some(crate::deposit::store::TxClaim {
            tx_hash: m.tx,
            tx_nonce: 7,
        });
        r.deposit = Some(self.deposit_info(&m));
        r.work_dir = Some(self.work.path().join(&r.op_id));
        self.save(&mut r)
    }

    /// Stopped in the anchor wait, the deposit confirmed on chain.
    pub fn confirmed_operation(&mut self) -> String {
        self.stopped_operation(crate::deposit::store::OpStage::Confirmed)
    }

    /// Stopped after the anchor, before a proof was written.
    pub fn anchored_operation(&mut self) -> String {
        self.stopped_operation(crate::deposit::store::OpStage::Anchored)
    }

    /// Proved, with the proof and its public inputs in the work directory.
    pub fn proved_operation(&mut self) -> String {
        self.mined_deposit(7);
        let m = self.mined.clone().unwrap();
        let mut r = self.record(crate::deposit::store::OpStage::Proved, m.from, 7);
        r.tx = Some(crate::deposit::store::TxClaim {
            tx_hash: m.tx,
            tx_nonce: 7,
        });
        r.deposit = Some(self.deposit_info(&m));
        let work = self.work.path().join(&r.op_id);
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("proof.bin"), b"proof").unwrap();
        std::fs::copy(
            self.prover.1.root.join("expected_pi.bin"),
            work.join("public_inputs.bin"),
        )
        .unwrap();
        r.work_dir = Some(work);
        self.save(&mut r)
    }

    /// The bridge now deploys vouchers from other code.
    pub fn voucher_code_moved(&self) {
        self.an
            .getter(W_BRIDGE_ACC, "getDepositVoucherCodeHash", vec![
                serde_json::json!({ "value0": format!("0x{}", "11".repeat(32)) }),
            ]);
    }

    /// Accepted, withdrawn, accepted again, while the bridge is first paused.
    pub fn anchor_flapping_while_paused(&self) {
        let m = self.mined.as_ref().expect("mine a deposit first");
        self.an
            .accepted_seq
            .lock()
            .unwrap()
            .insert(m.block.hash, Script::new([true, false, true]));
        // preflight reads it once; the wait reads it only while anchored.
        self.an.getter(W_BRIDGE_ACC, "isPaused", vec![
            serde_json::json!({ "value0": false }),
            serde_json::json!({ "value0": true }),
            serde_json::json!({ "value0": false }),
        ]);
    }

    /// The mined deposit's voucher is deployed.
    pub fn voucher_deployed(&self) {
        use crate::deposit::{
            an::AccStatus,
            identity::{offline_context, voucher_account_id, DepositIdentity},
        };
        let m = self.mined.as_ref().expect("mine a deposit first");
        let id = DepositIdentity {
            deposit_id: m.deposit_id,
            contract: W_BRIDGE,
            chain_id: 11_155_111,
        };
        let voucher = voucher_account_id(&offline_context(), &id).unwrap();
        self.an
            .accounts
            .lock()
            .unwrap()
            .insert(voucher, AccountInfo {
                status: AccStatus::Active,
                dapp_id: Some([0; 32]),
                ecc3: 0,
            });
    }

    /// The recipient holds `before` micro-USDC until the first send; by the
    /// time anything reads it after the send, it has spent everything, the
    /// credit included.
    pub fn recipient_spends_everything_after_the_send(&self, before: u128) {
        let holding = |ecc3| AccountInfo {
            status: crate::deposit::an::AccStatus::Active,
            dapp_id: Some([0; 32]),
            ecc3,
        };
        self.an
            .accounts
            .lock()
            .unwrap()
            .insert(W_ACC, holding(before));
        self.an
            .on_send_accounts
            .lock()
            .unwrap()
            .push((W_ACC, holding(0)));
    }

    /// The first `finalizeDeposit` aborts with 224 (the anchor went away).
    pub fn first_finalize_hits_224(&self) {
        let mut s = self.an.sends.lock().unwrap();
        s.push_front(FinalizeSend::Executed {
            tx_id: "f224".into(),
            aborted: true,
            exit_code: Some(224),
        });
    }

    /// How many times the fake prover ran.
    pub fn prover_runs(&self) -> usize {
        std::fs::read_to_string(self.prover.1.root.join("data/runs.log"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
}

/// `ackinacki-bridge deposit <argv>` as clap parses it, with nothing taken
/// from the environment: a setting clap would read from an environment
/// variable is unset unless `argv` gives its flag, whatever the shell
/// running the tests exports.
pub fn deposit_args(argv: &[&str]) -> crate::deposit::args::DepositArgs {
    use clap::Parser as _;
    let cli = crate::args::Cli::try_parse_from(["ackinacki-bridge", "deposit"].iter().chain(argv))
        .unwrap();
    let crate::args::Command::Deposit(mut a) = cli.cmd else {
        panic!("not a deposit command line")
    };
    let given = |flag: &str| argv.contains(&flag);
    if !given("--rpc-url") {
        a.rpc_url = None;
    }
    if !given("--bridge-address") {
        a.bridge_address = None;
    }
    if !given("--gql-endpoint") {
        a.gql_endpoint = None;
    }
    if !given("--usdc-bridge-account") {
        a.usdc_bridge_account = None;
    }
    if !given("--deposit-prover-dir") {
        a.deposit_prover_dir = None;
    }
    if !given("--state-dir") {
        a.state_dir = None;
    }
    if !given("--work-dir") {
        a.work_dir = None;
    }
    if !given("--wc-project-id") {
        a.wc_project_id = None;
    }
    if !given("--confirmations") {
        a.confirmations = 12;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deposit::identity::pi_form;

    /// `h` with its two 16-byte halves swapped.
    fn halves_swapped(h: B256) -> B256 {
        let mut b = [0u8; 32];
        b[..16].copy_from_slice(&h[16..]);
        b[16..].copy_from_slice(&h[..16]);
        B256::from(b)
    }

    /// What the world recorded for its deposit block, everywhere a
    /// deposit's block hash is read: the chain, the log, the anchor and
    /// the prover's public inputs.
    fn assert_one_hash_everywhere(w: &World) {
        let m = w.mined.as_ref().unwrap();
        let h = m.block.hash;
        assert_ne!(B256::from(pi_form(h.0)), h, "key form");
        assert_ne!(halves_swapped(h), h, "halves swapped");
        assert_eq!(w.evm.by_hash.lock().unwrap().get(&h), Some(&m.block));
        assert!(w.an.accepted.lock().unwrap().contains_key(&h));
        let pi = std::fs::read(w.prover.1.root.join("expected_pi.bin")).unwrap();
        let pi = crate::deposit::pi::DepositPublicInputs::decode(&pi).unwrap();
        assert_eq!(pi.block_hash(), h);
    }

    #[test]
    fn the_worlds_deposit_blocks_read_differently_in_every_byte_order() {
        let mut w = World::healthy();
        let tx = w.mined_deposit(7);
        w.anchor_after(0);
        assert_one_hash_everywhere(&w);
        assert_eq!(
            w.evm.logs.lock().unwrap().last().unwrap().block_hash,
            w.mined.as_ref().unwrap().block.hash
        );
        w.reorg_after_confirmation(tx, 0);
        assert_one_hash_everywhere(&w);
    }

    /// A node that takes the connection and never answers: the wallet
    /// gives up within its request timeout instead of waiting forever, and
    /// the run treats the send as one whose outcome is unknown.
    #[tokio::test]
    async fn a_local_key_wallet_gives_up_on_a_node_that_never_answers() {
        use crate::deposit::wallet::{TxPurpose, TxRequest, Wallet as _, WalletError};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Accepts every connection and holds it open without a byte back.
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let from = signer.address();
        let mut w = LocalKeyWallet {
            signer,
            rpc_url: format!("http://{addr}"),
            legacy: false,
            approve_cap: None,
            request_timeout: std::time::Duration::from_millis(300),
        };
        let tx = TxRequest {
            from,
            to: Address::repeat_byte(0x22),
            data: Bytes::new(),
            gas: 50_000,
            fees: crate::deposit::evm::Fees {
                max_fee_per_gas: 2_000_000_000,
                max_priority_fee_per_gas: 1_000_000_000,
            },
            purpose: TxPurpose::Approve {
                token: Address::repeat_byte(0x22),
                spender: Address::repeat_byte(0x33),
                amount: U256::from(1u64),
            },
        };
        let ui = crate::deposit::ui::RecordingUi::new(true);
        let got = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            w.send_transaction(&ui, &tx),
        )
        .await
        .expect("the wallet must give up on a silent node, not wait forever");
        assert!(matches!(got, Err(WalletError::Other(_))), "{got:?}");
    }
}
