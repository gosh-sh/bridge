//! [`DepositSource`] — abstraction over "where do `Deposit` events come
//! from?".
//!
//! The crate ships:
//! - [`InMemoryDepositSource`] — pre-baked map keyed by `deposit_id`, used by
//!   the unit tests to drive multi-deposit scenarios in microseconds.
//! - [`EthLogSource`] — production. Polls Ethereum `eth_getLogs` for the
//!   bridge's `Deposit(depositId, sender, amount, anWorkchain, anAccount,
//!   timestamp)` event, honouring a confirmation depth so only finalised
//!   deposits are surfaced.

use std::{collections::BTreeMap, sync::{Arc, Mutex}, time::Duration};

use alloy::{
    network::{Ethereum, Network},
    primitives::{Address, B256, U256},
    providers::Provider,
    rpc::types::{Filter, Log},
    sol_types::SolEvent,
};
use async_trait::async_trait;

use crate::{error::RelayerError, types::DepositEvent};

/// Asynchronous source of `Deposit` events.
///
/// `fetch(deposit_id)` returns:
/// - `Ok(Some(event))` — the deposit is visible and sufficiently confirmed;
/// - `Ok(None)` — not available yet (not emitted, or not buried under enough
///   confirmations); the relayer waits;
/// - `Err(RelayerError)` — terminal error inside the source.
#[async_trait]
pub trait DepositSource: Send + Sync {
    async fn fetch(&self, deposit_id: u64) -> Result<Option<DepositEvent>, RelayerError>;
}

// ─────────────────────────────────────────────────────────────────────
// In-memory source for unit tests
// ─────────────────────────────────────────────────────────────────────

/// Pre-baked map keyed by `deposit_id`. Mutable through an interior `Mutex`
/// so tests can stage / mutate deposits at runtime.
pub struct InMemoryDepositSource {
    deposits: Mutex<BTreeMap<u64, DepositEvent>>,
}

impl InMemoryDepositSource {
    pub fn new() -> Self {
        Self {
            deposits: Mutex::new(BTreeMap::new()),
        }
    }

    /// Stage a deposit. Overwrites any prior entry for the same id.
    pub fn insert(&self, event: DepositEvent) {
        self.deposits
            .lock()
            .expect("poisoned lock")
            .insert(event.deposit_id, event);
    }

    pub fn len(&self) -> usize {
        self.deposits.lock().expect("poisoned lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for InMemoryDepositSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DepositSource for InMemoryDepositSource {
    async fn fetch(&self, deposit_id: u64) -> Result<Option<DepositEvent>, RelayerError> {
        Ok(self
            .deposits
            .lock()
            .expect("poisoned lock")
            .get(&deposit_id)
            .cloned())
    }
}

// ─────────────────────────────────────────────────────────────────────
// EthLogSource — production log poller over alloy
// ─────────────────────────────────────────────────────────────────────

mod sol_bindings {
    use alloy::sol;
    sol! {
        #[sol(rpc)]
        #[allow(missing_docs)]
        contract AckiNackiBridge {
            event Deposit(
                uint256 indexed depositId,
                address indexed sender,
                uint256 amount,
                int8 anWorkchain,
                bytes32 anAccount,
                uint256 timestamp
            );

            function depositCounter() external view returns (uint256);
        }
    }
}

pub use sol_bindings::AckiNackiBridge;
// Re-export at module scope so the `sol!` event type is reachable as
// `Deposit` for callers that want the signature hash.
use sol_bindings::AckiNackiBridge::Deposit;

/// Maximum `eth_getLogs` block span per request. Alchemy's free tier caps this
/// at 10 blocks; chunking keeps wide scans working on any RPC.
const GET_LOGS_CHUNK_BLOCKS: u64 = 10;

/// Retry `eth_getLogs` on transient RPC rate limits (HTTP 429 / CU/sec caps).
const GET_LOGS_MAX_ATTEMPTS: u32 = 5;
const GET_LOGS_INITIAL_BACKOFF_MS: u64 = 500;
const GET_LOGS_MAX_BACKOFF_MS: u64 = 8_000;

/// Environment variable read by [`resolve_from_block`] when `--from-block` is
/// left at zero.
pub const BRIDGE_DEPLOY_BLOCK_ENV: &str = "BRIDGE_DEPLOY_BLOCK";

/// Lower bound for log scans: explicit CLI `--from-block` wins; otherwise
/// `BRIDGE_DEPLOY_BLOCK`. Returns `0` when neither is set (scans from genesis —
/// avoid on mainnet; operators should set the deploy block).
pub fn resolve_from_block(cli_from_block: u64) -> u64 {
    if cli_from_block != 0 {
        return cli_from_block;
    }
    std::env::var(BRIDGE_DEPLOY_BLOCK_ENV)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Map a block-global `logIndex` (from `eth_getLogs`) to the receipt-local
/// position `deposit-prover` indexes by.
pub fn receipt_log_index_from_block_log(
    logs: &[Log],
    block_log_index: u64,
) -> Result<u64, RelayerError> {
    logs.iter()
        .position(|l| l.log_index == Some(block_log_index))
        .map(|i| i as u64)
        .ok_or_else(|| {
            RelayerError::eth(format!(
                "deposit log index {block_log_index} not found in receipt ({} logs)",
                logs.len()
            ))
        })
}

/// Whether an Ethereum RPC error is likely transient (429 / throughput).
pub fn is_retryable_eth_rpc_error(err: &impl std::fmt::Display) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("429")
        || msg.contains("rate limit")
        || msg.contains("compute units per second")
        || msg.contains("too many requests")
}

/// Production deposit source — scans `eth_getLogs` for the bridge's
/// `Deposit` event.
///
/// Generic over any alloy [`Provider`]; a plain HTTP provider is enough
/// (read-only). Only deposits at least `confirmations` blocks behind the
/// chain head are surfaced, so a reorg can't make the relayer prove a
/// deposit that later disappears.
pub struct EthLogSource<P: Provider<N>, N: Network = alloy::network::Ethereum> {
    provider: P,
    address: Address,
    /// Lower bound for the log scan (typically the bridge's deploy block).
    from_block: u64,
    /// Confirmation depth: `safe_head = head - confirmations`.
    confirmations: u64,
    /// Highest `safe_head` scanned on the previous fetch. When set, the next
    /// scan starts at `scanned_through + 1` instead of re-walking from
    /// `from_block`.
    scan_cursor: Option<Arc<Mutex<u64>>>,
    _network: std::marker::PhantomData<N>,
}

impl<P, N> EthLogSource<P, N>
where
    P: Provider<N>,
    N: Network,
{
    pub fn new(provider: P, address: Address, from_block: u64, confirmations: u64) -> Self {
        Self {
            provider,
            address,
            from_block,
            confirmations,
            scan_cursor: None,
            _network: std::marker::PhantomData,
        }
    }

    /// Attach a shared scan cursor (typically backed by `RelayerState`).
    pub fn with_scan_cursor(mut self, cursor: Arc<Mutex<u64>>) -> Self {
        self.scan_cursor = Some(cursor);
        self
    }

    fn effective_scan_from(&self) -> u64 {
        match &self.scan_cursor {
            Some(cursor) => {
                let last = *cursor.lock().expect("poisoned scan cursor");
                if last >= self.from_block {
                    last.saturating_add(1)
                } else {
                    self.from_block
                }
            },
            None => self.from_block,
        }
    }

    pub fn address(&self) -> Address {
        self.address
    }

    /// Read the bridge's `depositCounter()` — the number of deposits made so
    /// far. Useful for an operator-facing "how far behind am I?" view.
    pub async fn deposit_counter(&self) -> Result<U256, RelayerError> {
        let contract = AckiNackiBridge::new(self.address, &self.provider);
        contract
            .depositCounter()
            .call()
            .await
            .map_err(|e| RelayerError::eth(format!("depositCounter() failed: {e}")))
    }
}

/// Locate a deposit via `eth_getTransactionReceipt` only — no `eth_getLogs`.
///
/// `log_index` is the **receipt-local** index (the position inside
/// `receipt.logs[]`), matching what `deposit-prover`'s `fetch_deposit_data`
/// expects. This avoids wide log scans that free-tier RPCs (e.g. Alchemy)
/// rate-limit.
pub async fn fetch_deposit_from_receipt<P>(
    provider: &P,
    bridge_address: Address,
    tx_hash: B256,
    log_index: u64,
    expected_deposit_id: u64,
    confirmations: u64,
) -> Result<Option<DepositEvent>, RelayerError>
where
    P: Provider<Ethereum> + Send + Sync,
{
    let receipt = provider
        .get_transaction_receipt(tx_hash)
        .await
        .map_err(|e| RelayerError::eth(format!("get_transaction_receipt failed: {e}")))?
        .ok_or_else(|| RelayerError::eth("deposit tx receipt not found"))?;

    let block_number = receipt
        .block_number
        .ok_or_else(|| RelayerError::eth("deposit tx receipt missing block_number"))?;

    let head = provider
        .get_block_number()
        .await
        .map_err(|e| RelayerError::eth(format!("get_block_number failed: {e}")))?;
    let safe_head = head.saturating_sub(confirmations);
    if block_number > safe_head {
        return Ok(None);
    }

    let log = receipt
        .inner
        .logs()
        .get(log_index as usize)
        .ok_or_else(|| {
            RelayerError::eth(format!(
                "receipt log index {log_index} out of range ({} logs in tx {tx_hash:#x})",
                receipt.inner.logs().len()
            ))
        })?;

    if log.address() != bridge_address {
        return Err(RelayerError::eth(format!(
            "log at index {log_index} emitted by {} != bridge {}",
            log.address(),
            bridge_address
        )));
    }

    let decoded = log
        .log_decode::<Deposit>()
        .map_err(|e| RelayerError::eth(format!("log at index {log_index} is not Deposit: {e}")))?;
    let ev = &decoded.inner.data;
    let id_u64: u64 = ev
        .depositId
        .try_into()
        .map_err(|_| RelayerError::eth("depositId does not fit in u64"))?;
    if id_u64 != expected_deposit_id {
        return Err(RelayerError::eth(format!(
            "receipt depositId {id_u64} != expected {expected_deposit_id}"
        )));
    }

    Ok(Some(DepositEvent {
        deposit_id: id_u64,
        sender: ev.sender,
        amount: ev.amount,
        an_workchain: ev.anWorkchain,
        an_account: ev.anAccount,
        timestamp: ev.timestamp,
        tx_hash,
        log_index,
        block_number,
        block_hash: receipt.block_hash.unwrap_or_default(),
        source_contract: bridge_address,
    }))
}

#[async_trait]
impl<P> DepositSource for EthLogSource<P, Ethereum>
where
    P: Provider<Ethereum> + Send + Sync,
{
    async fn fetch(&self, deposit_id: u64) -> Result<Option<DepositEvent>, RelayerError> {
        let head = self
            .provider
            .get_block_number()
            .await
            .map_err(|e| RelayerError::eth(format!("get_block_number failed: {e}")))?;
        let safe_head = head.saturating_sub(self.confirmations);
        if safe_head < self.from_block {
            // Nothing finalised in our window yet.
            return Ok(None);
        }

        // `depositId` is the first indexed topic; filter on it directly so
        // the node only returns the single matching log. Scan in small chunks
        // so free-tier RPCs (Alchemy: 10-block cap) don't reject wide ranges.
        let topic1 = B256::from(U256::from(deposit_id).to_be_bytes::<32>());
        let mut logs = Vec::new();
        let mut chunk_start = self.effective_scan_from();
        while chunk_start <= safe_head {
            let chunk_end = chunk_start
                .saturating_add(GET_LOGS_CHUNK_BLOCKS - 1)
                .min(safe_head);
            let filter = Filter::new()
                .address(self.address)
                .event_signature(Deposit::SIGNATURE_HASH)
                .topic1(topic1)
                .from_block(chunk_start)
                .to_block(chunk_end);
            let chunk = get_logs_with_retry(&self.provider, &filter).await?;
            logs.extend(chunk);
            chunk_start = chunk_end.saturating_add(1);
        }

        if let Some(cursor) = &self.scan_cursor {
            *cursor.lock().expect("poisoned scan cursor") = safe_head;
        }

        for log in logs {
            let decoded = match log.log_decode::<Deposit>() {
                Ok(d) => d,
                Err(_) => continue,
            };
            let ev = &decoded.inner.data;
            let id_u64: u64 = match ev.depositId.try_into() {
                Ok(v) => v,
                Err(_) => continue, // depositId outside u64 — not our counter
            };
            if id_u64 != deposit_id {
                continue;
            }
            let tx_hash = decoded.transaction_hash.unwrap_or_default();
            // `deposit-prover` indexes logs by position inside the tx receipt,
            // not the block-global `logIndex` that `eth_getLogs` returns.
            let receipt = self
                .provider
                .get_transaction_receipt(tx_hash)
                .await
                .map_err(|e| RelayerError::eth(format!("get_transaction_receipt failed: {e}")))?
                .ok_or_else(|| RelayerError::eth("deposit tx receipt not found"))?;
            let block_log_index = decoded.log_index.unwrap_or_default();
            let receipt_log_index =
                receipt_log_index_from_block_log(receipt.inner.logs(), block_log_index)?;
            return Ok(Some(DepositEvent {
                deposit_id: id_u64,
                sender: ev.sender,
                amount: ev.amount,
                an_workchain: ev.anWorkchain,
                an_account: ev.anAccount,
                timestamp: ev.timestamp,
                tx_hash,
                log_index: receipt_log_index,
                block_number: decoded.block_number.unwrap_or_default(),
                block_hash: decoded.block_hash.unwrap_or_default(),
                source_contract: self.address,
            }));
        }
        Ok(None)
    }
}

async fn get_logs_with_retry<P>(provider: &P, filter: &Filter) -> Result<Vec<Log>, RelayerError>
where
    P: Provider<Ethereum> + Send + Sync,
{
    let mut attempt = 0u32;
    let mut backoff_ms = GET_LOGS_INITIAL_BACKOFF_MS;
    loop {
        attempt += 1;
        match provider.get_logs(filter).await {
            Ok(logs) => return Ok(logs),
            Err(e) if is_retryable_eth_rpc_error(&e) && attempt < GET_LOGS_MAX_ATTEMPTS => {
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = backoff_ms.saturating_mul(2).min(GET_LOGS_MAX_BACKOFF_MS);
            },
            Err(e) => {
                return Err(RelayerError::eth(format!(
                    "get_logs failed after {attempt} attempt(s): {e}"
                )));
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::Address;

    use super::*;
    use crate::types::DepositEvent;

    fn dummy(deposit_id: u64) -> DepositEvent {
        DepositEvent {
            deposit_id,
            sender: Address::repeat_byte(0x11),
            amount: U256::from(deposit_id * 1000),
            an_workchain: 0,
            an_account: B256::repeat_byte(0x33),
            timestamp: U256::from(1_700_000_000u64),
            tx_hash: B256::repeat_byte(0xaa),
            log_index: 0,
            block_number: 100 + deposit_id,
            block_hash: B256::repeat_byte(0xbb),
            source_contract: Address::repeat_byte(0x22),
        }
    }

    #[tokio::test]
    async fn in_memory_source_returns_none_when_missing() {
        let src = InMemoryDepositSource::new();
        assert!(src.fetch(0).await.unwrap().is_none());
        src.insert(dummy(1));
        assert!(src.fetch(0).await.unwrap().is_none());
        assert_eq!(src.fetch(1).await.unwrap().unwrap().deposit_id, 1);
    }

    #[test]
    fn resolve_from_block_prefers_cli() {
        assert_eq!(resolve_from_block(42), 42);
    }

    #[test]
    fn is_retryable_detects_429() {
        assert!(is_retryable_eth_rpc_error(
            &"HTTP error 429 with body: compute units per second"
        ));
        assert!(!is_retryable_eth_rpc_error(&"invalid params"));
    }
}
