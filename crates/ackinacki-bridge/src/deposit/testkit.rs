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

use crate::deposit::evm::*;

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
