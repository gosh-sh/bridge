//! The EVM side, read-only. Signing happens in the wallet; everything
//! here only reads, so the trait has a fake and every decision above it
//! is tested on scripted chains.

use alloy::{
    consensus::Transaction as _,
    eips::{BlockId, BlockNumberOrTag, Typed2718 as _},
    providers::{DynProvider, Provider, ProviderBuilder},
    rlp::Encodable,
    rpc::types::{Filter, TransactionRequest},
    sol_types::{SolCall, SolEvent, SolInterface},
    transports::TransportError,
};
use alloy_primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;

use crate::errors::{CliError, CliResult};

alloy::sol! {
    /// The part of the EVM bridge a deposit uses.
    interface IDeposit {
        function deposit(uint256 amount, int8 anWorkchain, bytes32 anAccount) external;
        function usdc() external view returns (address);
        function paused() external view returns (bool);
        event Deposit(uint256 indexed depositId, address indexed sender, uint256 amount, int8 anWorkchain, bytes32 anAccount, uint256 timestamp);
        error BridgePaused();
    }
    /// The part of ERC-20 a deposit uses.
    interface IErc20 {
        function approve(address spender, uint256 amount) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);
        function balanceOf(address account) external view returns (uint256);
        function decimals() external view returns (uint8);
    }
}

/// The selector of `deposit(uint256,int8,bytes32)`.
pub const DEPOSIT_SELECTOR: [u8; 4] = [0xa4, 0x1d, 0x02, 0x29];
/// A deposit's calldata: the selector and three words.
pub const DEPOSIT_CALLDATA_LEN: usize = 100;
/// topic0 of the bridge's `Deposit` event.
pub const DEPOSIT_TOPIC0: B256 =
    alloy_primitives::b256!("8d5d060673b27fac84d56ee262fe8dccad60d198ae11766063f112a9be3d37ee");

/// Which block a read is made at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockTag {
    /// The head the node follows.
    Latest,
    /// The head plus the node's mempool.
    Pending,
    /// The newest block the chain has finalized.
    Finalized,
    /// A block by its number.
    Number(u64),
}

/// The fields of a block header a deposit looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// The block number.
    pub number: u64,
    /// The block hash.
    pub hash: B256,
    /// The parent block's hash.
    pub parent_hash: B256,
    /// The block timestamp, in seconds.
    pub timestamp: u64,
}

/// A log, without the block and transaction it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLite {
    /// The contract that emitted it.
    pub address: Address,
    /// Its topics, topic0 first.
    pub topics: Vec<B256>,
    /// Its non-indexed data.
    pub data: Bytes,
    /// Its index among all the logs of its block, if the node gave one.
    pub block_log_index: Option<u64>,
}

/// A mined transaction's receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptLite {
    /// The transaction hash.
    pub tx_hash: B256,
    /// The hash of the block that holds it.
    pub block_hash: B256,
    /// The number of the block that holds it.
    pub block_number: u64,
    /// The transaction succeeded.
    pub status: bool,
    /// The logs it emitted, in order.
    pub logs: Vec<LogLite>,
}

/// A transaction as the node shows it, mined or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxLite {
    /// The transaction hash.
    pub hash: B256,
    /// The sender.
    pub from: Address,
    /// The recipient; `None` for a contract creation.
    pub to: Option<Address>,
    /// The sender's nonce it uses.
    pub nonce: u64,
    /// The EIP-2718 type.
    pub tx_type: u8,
    /// The calldata.
    pub input: Bytes,
    /// The RLP length of its access list; 0 when it has none.
    pub access_list_rlp_len: usize,
    /// The block that holds it; `None` while it is pending.
    pub block_number: Option<u64>,
}

/// A bridge `Deposit` log with where it was mined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepositLogRef {
    /// The transaction that emitted it.
    pub tx_hash: B256,
    /// The block that holds it.
    pub block_number: u64,
    /// That block's hash.
    pub block_hash: B256,
    /// The log itself.
    pub log: LogLite,
}

/// EIP-1559 fee caps, in wei.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fees {
    /// `maxFeePerGas`.
    pub max_fee_per_gas: u128,
    /// `maxPriorityFeePerGas`.
    pub max_priority_fee_per_gas: u128,
}

/// Every EVM read a deposit makes. An error means "not known", never
/// "no": the caller retries or reports it, and never decides on it.
#[async_trait]
pub trait EvmRead: Send + Sync {
    /// The chain id the node serves.
    async fn chain_id(&self) -> anyhow::Result<u64>;
    /// The code at `a`; empty for an account without code.
    async fn code(&self, a: Address) -> anyhow::Result<Bytes>;
    /// `eth_call` at the latest block; returns the call's output.
    async fn call(&self, to: Address, data: Bytes) -> anyhow::Result<Bytes>;
    /// Re-executes a call at `block`; `Some(reason)` if it reverts.
    async fn revert_reason(
        &self,
        from: Address,
        to: Address,
        data: Bytes,
        block: u64,
    ) -> anyhow::Result<Option<String>>;
    /// The bridge's `paused()`. `None` when the bridge has no such getter:
    /// a deployment that predates the owner's pause, where `deposit()`
    /// cannot be paused.
    async fn bridge_paused(&self, bridge: Address) -> anyhow::Result<Option<bool>>;
    /// The header at `tag`; `None` if the node has no such block.
    async fn header(&self, tag: BlockTag) -> anyhow::Result<Option<Header>>;
    /// The header with hash `h`; `None` if the node does not know it.
    async fn header_by_hash(&self, h: B256) -> anyhow::Result<Option<Header>>;
    /// The receipt of `h`; `None` while it is not mined.
    async fn receipt(&self, h: B256) -> anyhow::Result<Option<ReceiptLite>>;
    /// The transaction `h`; `None` if the node does not show it.
    async fn transaction(&self, h: B256) -> anyhow::Result<Option<TxLite>>;
    /// The number of transactions `a` has sent, as of `tag`.
    async fn tx_count(&self, a: Address, tag: BlockTag) -> anyhow::Result<u64>;
    /// The bridge's `Deposit` logs from `sender` in the inclusive block
    /// range.
    async fn deposit_logs(
        &self,
        bridge: Address,
        sender: Address,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<DepositLogRef>>;
    /// Fetches every receipt and raw transaction of one block, the way
    /// the prover's fetcher will. Returns the transaction count.
    async fn probe_block(&self, tag: BlockTag) -> anyhow::Result<usize>;
    /// `eth_estimateGas` for a call from `from`.
    async fn estimate_gas(&self, from: Address, to: Address, data: Bytes) -> anyhow::Result<u64>;
    /// The node's EIP-1559 fee estimate.
    async fn fees(&self) -> anyhow::Result<Fees>;
}

/// The calldata of `deposit(amount, 0, account)`.
pub fn deposit_calldata(amount: u64, account: B256) -> Bytes {
    IDeposit::depositCall {
        amount: U256::from(amount),
        anWorkchain: 0,
        anAccount: account,
    }
    .abi_encode()
    .into()
}

/// The calldata of `approve(spender, amount)`.
pub fn approve_calldata(spender: Address, amount: U256) -> Bytes {
    IErc20::approveCall {
        spender,
        amount,
    }
    .abi_encode()
    .into()
}

/// A decoded bridge `Deposit` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepositEvent {
    /// The bridge's deposit counter.
    pub deposit_id: U256,
    /// Who deposited.
    pub sender: Address,
    /// The amount, in USDC units.
    pub amount: U256,
    /// The Acki Nacki workchain the deposit names.
    pub an_workchain: i8,
    /// The Acki Nacki recipient account.
    pub an_account: B256,
}

/// Decodes a bridge `Deposit` log; `None` for any other log.
pub fn parse_deposit_log(log: &LogLite) -> Option<DepositEvent> {
    if log.topics.first() != Some(&DEPOSIT_TOPIC0) {
        return None;
    }
    let ev = IDeposit::Deposit::decode_raw_log(log.topics.iter().copied(), &log.data).ok()?;
    Some(DepositEvent {
        deposit_id: ev.depositId,
        sender: ev.sender,
        amount: ev.amount,
        an_workchain: ev.anWorkchain,
        an_account: ev.anAccount,
    })
}

/// The bridge's `usdc()`: the token it takes deposits in.
pub async fn read_usdc(evm: &dyn EvmRead, bridge: Address) -> anyhow::Result<Address> {
    let out = evm
        .call(bridge, IDeposit::usdcCall {}.abi_encode().into())
        .await?;
    Ok(IDeposit::usdcCall::abi_decode_returns(&out)?)
}

/// The token's `decimals()`.
pub async fn read_decimals(evm: &dyn EvmRead, token: Address) -> anyhow::Result<u8> {
    let out = evm
        .call(token, IErc20::decimalsCall {}.abi_encode().into())
        .await?;
    Ok(IErc20::decimalsCall::abi_decode_returns(&out)?)
}

/// The token's `balanceOf(who)`.
pub async fn read_balance(evm: &dyn EvmRead, token: Address, who: Address) -> anyhow::Result<U256> {
    let out = evm
        .call(
            token,
            IErc20::balanceOfCall {
                account: who,
            }
            .abi_encode()
            .into(),
        )
        .await?;
    Ok(IErc20::balanceOfCall::abi_decode_returns(&out)?)
}

/// The token's `allowance(owner, spender)`.
pub async fn read_allowance(
    evm: &dyn EvmRead,
    token: Address,
    owner: Address,
    spender: Address,
) -> anyhow::Result<U256> {
    let out = evm
        .call(
            token,
            IErc20::allowanceCall {
                owner,
                spender,
            }
            .abi_encode()
            .into(),
        )
        .await?;
    Ok(IErc20::allowanceCall::abi_decode_returns(&out)?)
}

/// [`EvmRead`] over a JSON-RPC endpoint.
pub struct AlloyEvm {
    /// The provider every read goes through.
    p: DynProvider,
}

impl AlloyEvm {
    /// A reader for the `--rpc-url` endpoint. Nothing is sent yet.
    ///
    /// Every request is cut after
    /// [`ONE_READ`](crate::deposit::retry::ONE_READ): a node that stops
    /// answering mid-read gives an error, which the caller's retry reads
    /// again, instead of a read that never returns.
    pub fn connect(url: &str) -> CliResult<AlloyEvm> {
        let u = url.parse().map_err(|e| CliError::ArgInvalid {
            flag: "rpc-url",
            expected: format!("an http(s) URL ({e})"),
            got: crate::args::redact(url),
        })?;
        let client = alloy::transports::http::reqwest::Client::builder()
            .timeout(crate::deposit::retry::ONE_READ)
            .build()
            .map_err(|e| CliError::Preflight {
                reason: format!("cannot set up the HTTP client for the EVM RPC: {e}"),
                source: None,
            })?;
        Ok(AlloyEvm {
            p: ProviderBuilder::new().connect_reqwest(client, u).erased(),
        })
    }
}

/// The JSON-RPC block tag for `t`.
fn tag(t: BlockTag) -> BlockNumberOrTag {
    match t {
        BlockTag::Latest => BlockNumberOrTag::Latest,
        BlockTag::Pending => BlockNumberOrTag::Pending,
        BlockTag::Finalized => BlockNumberOrTag::Finalized,
        BlockTag::Number(n) => BlockNumberOrTag::Number(n),
    }
}

/// The parts of an RPC log a deposit keeps.
fn log_lite(l: &alloy::rpc::types::Log) -> LogLite {
    LogLite {
        address: l.address(),
        topics: l.topics().to_vec(),
        data: l.data().data.clone(),
        block_log_index: l.log_index,
    }
}

/// What a failed `eth_call` says about a revert. `None` when the node
/// did not say the call reverted: the transport failed, the node refused
/// the request, or it could not run the call. Otherwise the revert data
/// the node returned, empty when it returned none. A revert is what
/// alloy takes for one: an error response whose message says "revert".
fn revert_data(e: &TransportError) -> Option<Bytes> {
    let p = e.as_error_resp()?;
    p.message
        .contains("revert")
        .then(|| p.as_revert_data().unwrap_or_default())
}

/// The text for a revert: a bridge error has no text of its own, so it is
/// named; anything else is the node's message.
fn revert_text(e: &TransportError, data: &Bytes) -> String {
    match IDeposit::IDepositErrors::abi_decode(data) {
        Ok(IDeposit::IDepositErrors::BridgePaused(_)) => "BridgePaused".to_string(),
        Err(_) => e
            .as_error_resp()
            .map(|p| p.message.to_string())
            .unwrap_or_default(),
    }
}

/// A gas estimate that the node says reverts: the call would fail on chain.
/// Retrying does not help, unlike a transport error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EstimateReverted(pub String);

impl std::fmt::Display for EstimateReverted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the call would revert: {}", self.0)
    }
}

impl std::error::Error for EstimateReverted {}

/// The revert reason inside `e` when `e` says a call reverted, `None` for
/// every other failure (transport, rate limit, a node that cannot run the
/// call), which a caller may retry.
pub fn revert_of(e: &anyhow::Error) -> Option<String> {
    if let Some(r) = e.downcast_ref::<EstimateReverted>() {
        return Some(r.0.clone());
    }
    let t = e.downcast_ref::<TransportError>()?;
    revert_data(t).map(|d| revert_text(t, &d))
}

/// `eth_getLogs` ranges that public providers accept.
const LOG_CHUNK: u64 = 5_000;

#[async_trait]
impl EvmRead for AlloyEvm {
    async fn chain_id(&self) -> anyhow::Result<u64> {
        Ok(self.p.get_chain_id().await?)
    }

    async fn code(&self, a: Address) -> anyhow::Result<Bytes> {
        Ok(self.p.get_code_at(a).await?)
    }

    async fn call(&self, to: Address, data: Bytes) -> anyhow::Result<Bytes> {
        Ok(self
            .p
            .call(TransactionRequest::default().to(to).input(data.into()))
            .await?)
    }

    async fn revert_reason(
        &self,
        from: Address,
        to: Address,
        data: Bytes,
        block: u64,
    ) -> anyhow::Result<Option<String>> {
        let req = TransactionRequest::default()
            .from(from)
            .to(to)
            .input(data.into());
        match self.p.call(req).block(BlockId::number(block)).await {
            Ok(_) => Ok(None),
            Err(e) => match revert_data(&e) {
                Some(d) => Ok(Some(revert_text(&e, &d))),
                None => Err(e.into()),
            },
        }
    }

    async fn bridge_paused(&self, bridge: Address) -> anyhow::Result<Option<bool>> {
        let req = TransactionRequest::default()
            .to(bridge)
            .input(Bytes::from(IDeposit::pausedCall {}.abi_encode()).into());
        match self.p.call(req).await {
            Ok(out) => Ok(Some(IDeposit::pausedCall::abi_decode_returns(&out)?)),
            Err(e) => match revert_data(&e) {
                // No function matched the selector and there is no
                // fallback: the bridge predates `paused()`.
                Some(d) if d.is_empty() => Ok(None),
                Some(d) => Err(anyhow::anyhow!(
                    "the EVM bridge's paused() reverted with data {d}"
                )),
                None => Err(e.into()),
            },
        }
    }

    async fn header(&self, t: BlockTag) -> anyhow::Result<Option<Header>> {
        let b = self.p.get_block_by_number(tag(t)).await?;
        Ok(b.map(|b| Header {
            number: b.header.number,
            hash: b.header.hash,
            parent_hash: b.header.parent_hash,
            timestamp: b.header.timestamp,
        }))
    }

    async fn header_by_hash(&self, h: B256) -> anyhow::Result<Option<Header>> {
        let b = self.p.get_block_by_hash(h).await?;
        Ok(b.map(|b| Header {
            number: b.header.number,
            hash: b.header.hash,
            parent_hash: b.header.parent_hash,
            timestamp: b.header.timestamp,
        }))
    }

    async fn receipt(&self, h: B256) -> anyhow::Result<Option<ReceiptLite>> {
        let Some(r) = self.p.get_transaction_receipt(h).await? else {
            return Ok(None);
        };
        let (Some(block_hash), Some(block_number)) = (r.block_hash, r.block_number) else {
            return Ok(None);
        };
        Ok(Some(ReceiptLite {
            tx_hash: r.transaction_hash,
            block_hash,
            block_number,
            status: r.status(),
            logs: r.inner.logs().iter().map(log_lite).collect(),
        }))
    }

    async fn transaction(&self, h: B256) -> anyhow::Result<Option<TxLite>> {
        let Some(t) = self.p.get_transaction_by_hash(h).await? else {
            return Ok(None);
        };
        Ok(Some(TxLite {
            hash: h,
            from: t.inner.signer(),
            to: t.inner.to(),
            nonce: t.inner.nonce(),
            tx_type: t.inner.ty(),
            input: t.inner.input().clone(),
            access_list_rlp_len: t.inner.access_list().map(|al| al.length()).unwrap_or(0),
            block_number: t.block_number,
        }))
    }

    async fn tx_count(&self, a: Address, t: BlockTag) -> anyhow::Result<u64> {
        Ok(self
            .p
            .get_transaction_count(a)
            .block_id(BlockId::Number(tag(t)))
            .await?)
    }

    async fn deposit_logs(
        &self,
        bridge: Address,
        sender: Address,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<DepositLogRef>> {
        let mut out = Vec::new();
        let mut lo = from_block;
        while lo <= to_block {
            let hi = lo.saturating_add(LOG_CHUNK - 1).min(to_block);
            let f = Filter::new()
                .address(bridge)
                .event_signature(DEPOSIT_TOPIC0)
                .topic2(sender.into_word())
                .from_block(lo)
                .to_block(hi);
            for l in self.p.get_logs(&f).await? {
                // Skipping such a log would make an incomplete answer look
                // like "no deposit here".
                let (Some(tx_hash), Some(block_number), Some(block_hash)) =
                    (l.transaction_hash, l.block_number, l.block_hash)
                else {
                    anyhow::bail!(
                        "the node lists a Deposit log without its block or transaction; searching \
                         again"
                    );
                };
                out.push(DepositLogRef {
                    tx_hash,
                    block_number,
                    block_hash,
                    log: log_lite(&l),
                });
            }
            if hi == to_block {
                break;
            }
            lo = hi + 1;
        }
        Ok(out)
    }

    async fn probe_block(&self, t: BlockTag) -> anyhow::Result<usize> {
        use futures::StreamExt as _;
        let Some(b) = self.p.get_block_by_number(tag(t)).await? else {
            anyhow::bail!("the RPC has no {t:?} block");
        };
        let hashes: Vec<B256> = b.transactions.hashes().collect();
        let n = hashes.len();
        let results = futures::stream::iter(hashes)
            .map(|h| async move {
                self.p
                    .get_transaction_receipt(h)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("no receipt for {h}"))?;
                let raw: Bytes = self
                    .p
                    .raw_request("eth_getRawTransactionByHash".into(), (h,))
                    .await?;
                anyhow::ensure!(!raw.is_empty(), "no raw transaction for {h}");
                anyhow::Ok(())
            })
            .buffer_unordered(8)
            .collect::<Vec<_>>()
            .await;
        results.into_iter().collect::<anyhow::Result<Vec<()>>>()?;
        Ok(n)
    }

    async fn estimate_gas(&self, from: Address, to: Address, data: Bytes) -> anyhow::Result<u64> {
        let req = TransactionRequest::default()
            .from(from)
            .to(to)
            .input(data.into());
        match self.p.estimate_gas(req).await {
            Ok(g) => Ok(g),
            Err(e) => match revert_data(&e) {
                Some(d) => Err(EstimateReverted(revert_text(&e, &d)).into()),
                None => Err(e.into()),
            },
        }
    }

    async fn fees(&self) -> anyhow::Result<Fees> {
        let f = self.p.estimate_eip1559_fees().await?;
        Ok(Fees {
            max_fee_per_gas: f.max_fee_per_gas,
            max_priority_fee_per_gas: f.max_priority_fee_per_gas,
        })
    }
}

#[cfg(test)]
mod tests {
    use alloy::{sol_types::SolEvent, transports::mock::Asserter};
    use alloy_primitives::{address, B256, U256};

    use super::*;

    #[test]
    fn deposit_calldata_is_the_selector_and_three_words() {
        let acc = B256::repeat_byte(0xa3);
        let data = deposit_calldata(12_500_000, acc);
        assert_eq!(data.len(), DEPOSIT_CALLDATA_LEN);
        assert_eq!(&data[..4], &DEPOSIT_SELECTOR);
        assert_eq!(U256::from_be_slice(&data[4..36]), U256::from(12_500_000u64));
        assert_eq!(&data[36..68], &[0u8; 32], "anWorkchain = 0");
        assert_eq!(&data[68..100], acc.as_slice());
    }

    #[test]
    fn the_topic_is_the_bridge_event() {
        assert_eq!(DEPOSIT_TOPIC0, IDeposit::Deposit::SIGNATURE_HASH);
        assert_eq!(
            format!("{DEPOSIT_TOPIC0:x}"),
            "8d5d060673b27fac84d56ee262fe8dccad60d198ae11766063f112a9be3d37ee"
        );
    }

    #[test]
    fn a_deposit_log_parses_back() {
        let sender = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
        let log = crate::deposit::testkit::deposit_log(
            address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7"),
            U256::from(7),
            sender,
            12_500_000,
            B256::repeat_byte(0xa3),
            Some(3),
        );
        let ev = parse_deposit_log(&log).unwrap();
        assert_eq!(ev.deposit_id, U256::from(7));
        assert_eq!(ev.sender, sender);
        assert_eq!(ev.amount, U256::from(12_500_000u64));
        assert_eq!(ev.an_account, B256::repeat_byte(0xa3));
    }

    const BRIDGE: Address = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");

    /// An `AlloyEvm` whose RPC answers come from `Asserter`, in order.
    fn mocked() -> (AlloyEvm, Asserter) {
        let asserter = Asserter::new();
        let p = ProviderBuilder::new()
            .connect_mocked_client(asserter.clone())
            .erased();
        (
            AlloyEvm {
                p,
            },
            asserter,
        )
    }

    /// A JSON-RPC error response, as the node sends it.
    fn rpc_error(asserter: &Asserter, json: &str) {
        asserter.push_failure(serde_json::from_str(json).unwrap());
    }

    /// `paused()`'s return: one ABI word.
    fn word(b: bool) -> Bytes {
        Bytes::from(U256::from(u8::from(b)).to_be_bytes::<32>().to_vec())
    }

    #[test]
    fn the_pause_getter_and_error_are_the_bridge_abi() {
        assert_eq!(IDeposit::pausedCall::SELECTOR, [0x5c, 0x97, 0x5a, 0xbb]);
        assert_eq!(
            <IDeposit::BridgePaused as alloy::sol_types::SolError>::SIGNATURE,
            "BridgePaused()"
        );
    }

    #[tokio::test]
    async fn a_bridge_with_the_getter_answers_its_pause_state() {
        let (evm, rpc) = mocked();
        rpc.push_success(&word(true));
        assert_eq!(evm.bridge_paused(BRIDGE).await.unwrap(), Some(true));
        rpc.push_success(&word(false));
        assert_eq!(evm.bridge_paused(BRIDGE).await.unwrap(), Some(false));
    }

    #[tokio::test]
    async fn a_bridge_without_the_getter_reverts_empty_and_is_none() {
        let (evm, rpc) = mocked();
        // geth, anvil: code 3 and an empty `data`.
        rpc_error(
            &rpc,
            r#"{"code":3,"message":"execution reverted","data":"0x"}"#,
        );
        assert_eq!(evm.bridge_paused(BRIDGE).await.unwrap(), None);
        // Nodes that leave `data` out altogether.
        rpc_error(&rpc, r#"{"code":-32000,"message":"execution reverted"}"#);
        assert_eq!(evm.bridge_paused(BRIDGE).await.unwrap(), None);
    }

    #[tokio::test]
    async fn anything_else_about_the_pause_is_an_error_not_an_answer() {
        let (evm, rpc) = mocked();
        // A revert that carries data is not a missing getter.
        rpc_error(
            &rpc,
            r#"{"code":3,"message":"execution reverted","data":"0x7a9c9a3d"}"#,
        );
        assert!(evm.bridge_paused(BRIDGE).await.is_err());
        // A rate limit is not a revert.
        rpc_error(&rpc, r#"{"code":429,"message":"Too Many Requests"}"#);
        assert!(evm.bridge_paused(BRIDGE).await.is_err());
        // No answer at all: the transport failed.
        assert!(evm.bridge_paused(BRIDGE).await.is_err());
        // An address that runs no code returns nothing: not a bool.
        rpc.push_success(&Bytes::new());
        assert!(evm.bridge_paused(BRIDGE).await.is_err());
    }

    /// The RPC form of a bridge `Deposit` log from `sender`, mined in block
    /// 16 by transaction `0x11…`.
    fn rpc_deposit_log(sender: Address) -> alloy::rpc::types::Log {
        let l = crate::deposit::testkit::deposit_log(
            BRIDGE,
            U256::from(7),
            sender,
            12_500_000,
            B256::repeat_byte(0xa3),
            Some(3),
        );
        alloy::rpc::types::Log {
            inner: alloy_primitives::Log {
                address: l.address,
                data: alloy_primitives::LogData::new_unchecked(l.topics, l.data),
            },
            block_hash: Some(B256::repeat_byte(0x16)),
            block_number: Some(16),
            block_timestamp: None,
            transaction_hash: Some(B256::repeat_byte(0x11)),
            transaction_index: Some(0),
            log_index: Some(3),
            removed: false,
        }
    }

    #[tokio::test]
    async fn a_deposit_log_comes_back_with_where_it_was_mined() {
        let (evm, rpc) = mocked();
        let sender = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
        rpc.push_success(&vec![rpc_deposit_log(sender)]);
        let got = evm.deposit_logs(BRIDGE, sender, 10, 20).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].tx_hash, B256::repeat_byte(0x11));
        assert_eq!(got[0].block_number, 16);
        assert_eq!(got[0].block_hash, B256::repeat_byte(0x16));
        assert_eq!(got[0].log.block_log_index, Some(3));
        assert_eq!(parse_deposit_log(&got[0].log).unwrap().sender, sender);
    }

    #[tokio::test]
    async fn a_deposit_log_without_its_block_or_transaction_fails_the_search() {
        // Dropping such a log would turn "the node did not say" into "there
        // is no deposit".
        let sender = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
        let strip: [fn(&mut alloy::rpc::types::Log); 3] = [
            |l| l.transaction_hash = None,
            |l| l.block_number = None,
            |l| l.block_hash = None,
        ];
        for (i, strip) in strip.into_iter().enumerate() {
            let (evm, rpc) = mocked();
            let mut bare = rpc_deposit_log(sender);
            strip(&mut bare);
            rpc.push_success(&vec![rpc_deposit_log(sender), bare]);
            let err = evm.deposit_logs(BRIDGE, sender, 10, 20).await.unwrap_err();
            assert!(
                err.to_string().contains("without its block or transaction"),
                "case {i}: {err:#}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_node_that_never_answers_is_an_error_after_one_read() {
        use crate::deposit::retry::ONE_READ;
        // The kernel accepts the connection into the backlog and nobody ever
        // answers: without a per-request timeout the read would hang, and a
        // hung read is retried by nothing.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let evm = AlloyEvm::connect(&format!("http://{}", silent.local_addr().unwrap())).unwrap();
        let t0 = tokio::time::Instant::now();
        let got = tokio::time::timeout(ONE_READ * 2, evm.chain_id())
            .await
            .expect("the transport gives up by itself");
        let waited = t0.elapsed();
        let err = got.unwrap_err();
        assert!(
            waited >= ONE_READ && waited < ONE_READ * 2,
            "{waited:?}: {err:#}"
        );
    }

    #[tokio::test]
    async fn the_fake_bridge_runs_unpaused_unless_told() {
        use crate::deposit::testkit::FakeEvm;
        assert_eq!(
            FakeEvm::sepolia().bridge_paused(BRIDGE).await.unwrap(),
            Some(false)
        );
        assert_eq!(
            FakeEvm::default().bridge_paused(BRIDGE).await.unwrap(),
            Some(false)
        );
        let evm = FakeEvm::sepolia();
        *evm.paused.lock().unwrap() = None;
        assert_eq!(evm.bridge_paused(BRIDGE).await.unwrap(), None);
        *evm.paused.lock().unwrap() = Some(true);
        assert_eq!(evm.bridge_paused(BRIDGE).await.unwrap(), Some(true));
    }

    #[tokio::test]
    async fn a_revert_reason_is_only_what_the_node_calls_a_revert() {
        let (evm, rpc) = mocked();
        let from = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
        let data = deposit_calldata(1, B256::repeat_byte(0xa3));
        rpc.push_success(&Bytes::new());
        assert_eq!(
            evm.revert_reason(from, BRIDGE, data.clone(), 9)
                .await
                .unwrap(),
            None,
            "the call went through"
        );
        rpc_error(
            &rpc,
            r#"{"code":3,"message":"execution reverted: ERC20: insufficient allowance","data":"0x08c379a0"}"#,
        );
        assert_eq!(
            evm.revert_reason(from, BRIDGE, data.clone(), 9)
                .await
                .unwrap()
                .as_deref(),
            Some("execution reverted: ERC20: insufficient allowance")
        );
        let paused = alloy::sol_types::SolError::abi_encode(&IDeposit::BridgePaused {});
        rpc_error(
            &rpc,
            &format!(
                r#"{{"code":3,"message":"execution reverted","data":"0x{}"}}"#,
                alloy_primitives::hex::encode(paused)
            ),
        );
        assert_eq!(
            evm.revert_reason(from, BRIDGE, data.clone(), 9)
                .await
                .unwrap()
                .as_deref(),
            Some("BridgePaused")
        );
        // A node that cannot run the call has not said it reverts.
        rpc_error(&rpc, r#"{"code":-32000,"message":"missing trie node"}"#);
        assert!(evm.revert_reason(from, BRIDGE, data, 9).await.is_err());
    }

    #[tokio::test]
    async fn only_a_reverting_estimate_is_classified_as_a_revert() {
        let (evm, rpc) = mocked();
        let from = address!("b586356d52eaee055ca569ff412dfeffc5bb2307");
        let data = deposit_calldata(1, B256::repeat_byte(0xa3));
        let paused = alloy::sol_types::SolError::abi_encode(&IDeposit::BridgePaused {});
        rpc_error(
            &rpc,
            &format!(
                r#"{{"code":3,"message":"execution reverted","data":"0x{}"}}"#,
                alloy_primitives::hex::encode(paused)
            ),
        );
        let e = evm
            .estimate_gas(from, BRIDGE, data.clone())
            .await
            .unwrap_err();
        assert_eq!(revert_of(&e).as_deref(), Some("BridgePaused"));
        rpc_error(
            &rpc,
            r#"{"code":3,"message":"execution reverted: ERC20: transfer amount exceeds balance","data":"0x08c379a0"}"#,
        );
        let e = evm
            .estimate_gas(from, BRIDGE, data.clone())
            .await
            .unwrap_err();
        assert_eq!(
            revert_of(&e).as_deref(),
            Some("execution reverted: ERC20: transfer amount exceeds balance")
        );
        rpc_error(&rpc, r#"{"code":-32005,"message":"rate limit exceeded"}"#);
        let e = evm
            .estimate_gas(from, BRIDGE, data.clone())
            .await
            .unwrap_err();
        assert_eq!(revert_of(&e), None, "a rate limit is retried");
        assert_eq!(revert_of(&anyhow::anyhow!("connection reset")), None);
        rpc.push_success(&U256::from(21_000u64));
        assert_eq!(evm.estimate_gas(from, BRIDGE, data).await.unwrap(), 21_000);
    }
}
