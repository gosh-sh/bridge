//! [`DepositSource`] — abstraction over "where do `Deposit` events come
//! from?".
//!
//! The crate ships:
//! - [`InMemoryDepositSource`] — pre-baked map keyed by `deposit_id`, used by
//!   the unit tests to drive multi-deposit scenarios in microseconds.
//! - [`EthLogSource`] — production. Polls Ethereum `eth_getLogs` for the
//!   bridge's `Deposit(depositId, sender, amount, anAccount, timestamp)` event,
//!   honouring a confirmation depth so only finalised deposits are surfaced.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy::{
    eips::BlockId,
    network::{Ethereum, Network},
    primitives::{Address, B256, U256},
    providers::Provider,
    rpc::types::{Filter, Log},
    sol_types::SolEvent,
};
use async_trait::async_trait;
use metrics::{counter, gauge};

use crate::{error::RelayerError, metrics as prom, types::DepositEvent};

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

    /// Load a deposit this relayer has already seen. Default: [`fetch`].
    ///
    /// Production uses the stored tx (receipt) or a one-block `eth_getLogs`,
    /// so a retry does not walk the shared scan cursor from the deposit's
    /// block up to the head.
    async fn fetch_known(
        &self,
        deposit_id: u64,
        tx_hash: Option<B256>,
        log_index: Option<u64>,
        block_number: Option<u64>,
    ) -> Result<Option<DepositEvent>, RelayerError> {
        let _ = (tx_hash, log_index, block_number);
        self.fetch(deposit_id).await
    }
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

/// Inclusive start block for the next `eth_getLogs` scan: the block after the
/// scan cursor, never below `from_block`. With no cursor (a fresh daemon)
/// `from_block` itself is scanned.
pub(crate) fn scan_from_block(from_block: u64, scanned_through: Option<u64>) -> u64 {
    match scanned_through {
        None => from_block,
        Some(last) => last.saturating_add(1).max(from_block),
    }
}

/// Move the scan cursor up to `through`. It never moves back: a lower value
/// only means the caller knows less than the cursor already records.
pub(crate) fn advance_scan_cursor(cursor: &Mutex<Option<u64>>, through: u64) {
    let mut guard = cursor.lock().expect("poisoned scan cursor");
    *guard = Some(guard.map_or(through, |old| old.max(through)));
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
    /// Cached `eth_chainId` from the RPC (stamped onto every [`DepositEvent`]).
    chain_id: Mutex<Option<u64>>,
    /// Scan cursor: no block up to and including this one holds a deposit the
    /// relayer still has to *discover*, so the next scan starts after it.
    /// Outstanding retries use [`DepositSource::fetch_known`], not this
    /// cursor. Moved here when the target is not made yet as of `safe_head`,
    /// when a deposit is found (its block minus one), and by the relayer
    /// after a finalize. `None` means "scan from `from_block`".
    scan_cursor: Option<Arc<Mutex<Option<u64>>>>,
    /// Cursor value when [`Self::with_scan_cursor`] ran — the persisted
    /// `scan_done_through_block` of an earlier relayer. A migrated outstanding
    /// id with no stored log starts its rescan at this plus one, because that
    /// daemon left the cursor at the stuck deposit's block minus one. The live
    /// mutex is not used: idle polls move it to `safe_head`.
    initial_scan_cursor: Option<u64>,
    /// The last deposit found, so retrying a deposit AN keeps refusing costs
    /// no `eth_getLogs`. It was already `confirmations` deep when found.
    located: Mutex<Option<DepositEvent>>,
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
            chain_id: Mutex::new(None),
            scan_cursor: None,
            initial_scan_cursor: None,
            located: Mutex::new(None),
            _network: std::marker::PhantomData,
        }
    }

    /// Attach a shared scan cursor (typically backed by
    /// [`RelayerState`](crate::state::RelayerState)).
    pub fn with_scan_cursor(mut self, cursor: Arc<Mutex<Option<u64>>>) -> Self {
        self.initial_scan_cursor = cursor.lock().ok().and_then(|g| *g);
        self.scan_cursor = Some(cursor);
        self
    }

    fn effective_scan_from(&self) -> u64 {
        let scanned_through = self
            .scan_cursor
            .as_ref()
            .and_then(|cursor| *cursor.lock().expect("poisoned scan cursor"));
        scan_from_block(self.from_block, scanned_through)
    }

    pub fn address(&self) -> Address {
        self.address
    }

    /// Resolve and cache `eth_chainId` for operator sanity checks against the
    /// proven `chainId` public input.
    async fn resolve_chain_id(&self) -> Result<u64, RelayerError> {
        if let Some(id) = *self.chain_id.lock().expect("poisoned lock") {
            return Ok(id);
        }
        let id = self
            .provider
            .get_chain_id()
            .await
            .map_err(|e| RelayerError::eth(format!("eth_chainId failed: {e}")))?;
        *self.chain_id.lock().expect("poisoned lock") = Some(id);
        Ok(id)
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

    /// `depositCounter()` as of `block`. The bridge assigns `depositId =
    /// depositCounter++`, so a deposit exists at `block` exactly when this is
    /// above its id.
    async fn deposit_counter_at(&self, block: u64) -> Result<U256, RelayerError> {
        let contract = AckiNackiBridge::new(self.address, &self.provider);
        contract
            .depositCounter()
            .block(BlockId::number(block))
            .call()
            .await
            .map_err(|e| {
                RelayerError::eth(format!("depositCounter() at block {block} failed: {e}"))
            })
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
        an_account: ev.anAccount,
        timestamp: ev.timestamp,
        tx_hash,
        log_index,
        block_number,
        block_hash: receipt.block_hash.unwrap_or_default(),
        source_contract: bridge_address,
        source_chain_id: provider
            .get_chain_id()
            .await
            .map_err(|e| RelayerError::eth(format!("get_chain_id failed: {e}")))?,
    }))
}

impl<P> EthLogSource<P, Ethereum>
where
    P: Provider<Ethereum> + Send + Sync,
{
    /// The `Deposit` event for `deposit_id` among `logs`, if one is there.
    async fn deposit_from_logs(
        &self,
        logs: Vec<Log>,
        deposit_id: u64,
    ) -> Result<Option<DepositEvent>, RelayerError> {
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
                an_account: ev.anAccount,
                timestamp: ev.timestamp,
                tx_hash,
                log_index: receipt_log_index,
                block_number: decoded.block_number.unwrap_or_default(),
                block_hash: decoded.block_hash.unwrap_or_default(),
                source_contract: self.address,
                source_chain_id: self.resolve_chain_id().await?,
            }));
        }
        Ok(None)
    }

    fn cached_located(&self, deposit_id: u64) -> Option<DepositEvent> {
        self.located
            .lock()
            .expect("poisoned lock")
            .as_ref()
            .filter(|e| e.deposit_id == deposit_id)
            .cloned()
    }

    /// Scan `eth_getLogs` for `deposit_id` starting at `scan_from`.
    ///
    /// When `move_cursor` is set, a miss (the counter says the deposit is
    /// not made yet) advances the shared cursor to `safe_head`, and a hit
    /// moves it to the deposit's block minus one so later ids in the same
    /// block stay in range. Outstanding retries with no stored log start at
    /// the snapshot cursor plus one and leave the live cursor alone.
    async fn scan_for_deposit(
        &self,
        deposit_id: u64,
        scan_from: u64,
        move_cursor: bool,
        scan_until: Option<u64>,
    ) -> Result<Option<DepositEvent>, RelayerError> {
        let head = self
            .provider
            .get_block_number()
            .await
            .map_err(|e| RelayerError::eth(format!("get_block_number failed: {e}")))?;
        let safe_head = head.saturating_sub(self.confirmations);
        gauge!(prom::ETH_SAFE_HEAD_BLOCK).set(safe_head as f64);
        if safe_head < self.from_block {
            return Ok(None);
        }
        let scan_end = scan_until.unwrap_or(safe_head).min(safe_head);
        if scan_end < self.from_block {
            return Ok(None);
        }

        let counter = self.deposit_counter_at(safe_head).await?;
        if counter <= U256::from(deposit_id) {
            if move_cursor {
                if let Some(cursor) = &self.scan_cursor {
                    advance_scan_cursor(cursor, safe_head);
                }
            }
            return Ok(None);
        }

        let topic1 = B256::from(U256::from(deposit_id).to_be_bytes::<32>());
        let scan_from = scan_from.max(self.from_block);
        if scan_from > scan_end {
            return Ok(None);
        }
        gauge!(prom::ETH_SCAN_FROM_BLOCK).set(scan_from as f64);
        let mut chunk_start = scan_from;
        while chunk_start <= scan_end {
            let chunk_end = chunk_start
                .saturating_add(GET_LOGS_CHUNK_BLOCKS - 1)
                .min(scan_end);
            let filter = Filter::new()
                .address(self.address)
                .event_signature(Deposit::SIGNATURE_HASH)
                .topic1(topic1)
                .from_block(chunk_start)
                .to_block(chunk_end);
            let logs = get_logs_with_retry(&self.provider, &filter).await?;
            counter!(prom::ETH_SCANNED_BLOCKS_TOTAL)
                .increment(chunk_end.saturating_sub(chunk_start).saturating_add(1));
            if let Some(event) = self.deposit_from_logs(logs, deposit_id).await? {
                *self.located.lock().expect("poisoned lock") = Some(event.clone());
                if move_cursor {
                    if let (Some(cursor), Some(through)) =
                        (&self.scan_cursor, event.block_number.checked_sub(1))
                    {
                        advance_scan_cursor(cursor, through);
                    }
                }
                return Ok(Some(event));
            }
            chunk_start = chunk_end.saturating_add(1);
        }

        Err(RelayerError::eth(format!(
            "depositCounter() at block {safe_head} is {counter}, so depositId {deposit_id} is at \
             or below that block, but eth_getLogs found no Deposit log for it in blocks \
             {scan_from}..={scan_end}: either the RPC returned incomplete logs, or the deposit \
             is below block {scan_from} (lower --from-block, or remove scan_done_through_block \
             from the state file)"
        )))
    }

    /// One-block `eth_getLogs` for a deposit whose block is known but whose
    /// tx is not. Does not move the shared scan cursor.
    async fn fetch_in_block(
        &self,
        deposit_id: u64,
        block: u64,
    ) -> Result<Option<DepositEvent>, RelayerError> {
        let head = self
            .provider
            .get_block_number()
            .await
            .map_err(|e| RelayerError::eth(format!("get_block_number failed: {e}")))?;
        let safe_head = head.saturating_sub(self.confirmations);
        if block > safe_head {
            return Ok(None);
        }
        let topic1 = B256::from(U256::from(deposit_id).to_be_bytes::<32>());
        let filter = Filter::new()
            .address(self.address)
            .event_signature(Deposit::SIGNATURE_HASH)
            .topic1(topic1)
            .from_block(block)
            .to_block(block);
        let logs = get_logs_with_retry(&self.provider, &filter).await?;
        counter!(prom::ETH_SCANNED_BLOCKS_TOTAL).increment(1);
        if let Some(event) = self.deposit_from_logs(logs, deposit_id).await? {
            return Ok(Some(event));
        }
        Err(RelayerError::eth(format!(
            "no Deposit log for depositId {deposit_id} in block {block}"
        )))
    }
}

#[async_trait]
impl<P> DepositSource for EthLogSource<P, Ethereum>
where
    P: Provider<Ethereum> + Send + Sync,
{
    async fn fetch(&self, deposit_id: u64) -> Result<Option<DepositEvent>, RelayerError> {
        if let Some(event) = self.cached_located(deposit_id) {
            return Ok(Some(event));
        }

        // A first look that finds the log moves the discovery cursor to the
        // deposit's block minus one (later ids can share that block).
        // Outstanding retries use [`fetch_known`] instead of this cursor.
        self.scan_for_deposit(deposit_id, self.effective_scan_from(), true, None)
            .await
    }

    async fn fetch_known(
        &self,
        deposit_id: u64,
        tx_hash: Option<B256>,
        log_index: Option<u64>,
        block_number: Option<u64>,
    ) -> Result<Option<DepositEvent>, RelayerError> {
        if let Some(event) = self.cached_located(deposit_id) {
            return Ok(Some(event));
        }
        if let (Some(tx_hash), Some(log_index)) = (tx_hash, log_index) {
            return fetch_deposit_from_receipt(
                &self.provider,
                self.address,
                tx_hash,
                log_index,
                deposit_id,
                self.confirmations,
            )
            .await;
        }
        if let Some(block) = block_number {
            return self.fetch_in_block(deposit_id, block).await;
        }
        // No stored log: a `state.json` from an earlier relayer only carried
        // the failed id's attempt count. That daemon left the cursor at the
        // deposit's block minus one, so start at snapshot+1 — not at
        // `--from-block` (a hundred thousand chunks) and not at the live
        // cursor, which an idle poll has usually moved to `safe_head`. If
        // the snapshot already sat past the deposit, fall back to
        // `--from-block ..= snapshot`.
        let snapshot = self.initial_scan_cursor;
        let scan_from = snapshot
            .map(|c| c.saturating_add(1))
            .unwrap_or(self.from_block);
        match self
            .scan_for_deposit(deposit_id, scan_from, false, None)
            .await
        {
            Ok(found) => Ok(found),
            Err(e) if snapshot.is_some() && e.to_string().contains("found no Deposit log") => {
                self.scan_for_deposit(deposit_id, self.from_block, false, snapshot)
                    .await
            },
            Err(e) => Err(e),
        }
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
            Ok(logs) => {
                counter!(prom::ETH_GET_LOGS_TOTAL, "outcome" => "ok").increment(1);
                return Ok(logs);
            },
            Err(e) if is_retryable_eth_rpc_error(&e) && attempt < GET_LOGS_MAX_ATTEMPTS => {
                counter!(prom::ETH_GET_LOGS_TOTAL, "outcome" => "retry").increment(1);
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = backoff_ms.saturating_mul(2).min(GET_LOGS_MAX_BACKOFF_MS);
            },
            Err(e) => {
                counter!(prom::ETH_GET_LOGS_TOTAL, "outcome" => "error").increment(1);
                return Err(RelayerError::eth(format!(
                    "get_logs failed after {attempt} attempt(s): {e}"
                )));
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy::{primitives::Address, providers::RootProvider};

    use super::*;
    use crate::{
        fake_rpc::{self, FakeRpc},
        types::DepositEvent,
    };

    const BRIDGE: Address = Address::repeat_byte(0x22);
    const FROM_BLOCK: u64 = 100;
    const CONFIRMATIONS: u64 = 12;

    fn log_source(rpc: &FakeRpc, cursor: &Arc<Mutex<Option<u64>>>) -> EthLogSource<RootProvider> {
        EthLogSource::new(rpc.provider(), BRIDGE, FROM_BLOCK, CONFIRMATIONS)
            .with_scan_cursor(cursor.clone())
    }

    fn dummy(deposit_id: u64) -> DepositEvent {
        DepositEvent {
            deposit_id,
            sender: Address::repeat_byte(0x11),
            amount: U256::from(deposit_id * 1000),
            an_account: B256::repeat_byte(0x33),
            timestamp: U256::from(1_700_000_000u64),
            tx_hash: B256::repeat_byte(0xaa),
            log_index: 0,
            block_number: 100 + deposit_id,
            block_hash: B256::repeat_byte(0xbb),
            source_contract: Address::repeat_byte(0x22),
            source_chain_id: 1,
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
    fn scan_from_block_includes_from_block_until_a_finalize() {
        assert_eq!(scan_from_block(100, None), 100);
        assert_eq!(scan_from_block(0, None), 0);
        // After deposit 0 in block 105: cursor = 104 → next scan starts at 105
        // (same-block sibling still visible).
        assert_eq!(scan_from_block(100, Some(104)), 105);
        assert_eq!(scan_from_block(100, Some(99)), 100);
        // Stale cursor below the deploy block must not walk genesis.
        assert_eq!(scan_from_block(100, Some(50)), 100);
    }

    #[test]
    fn scan_cursor_never_moves_back() {
        let cursor = Mutex::new(None);
        advance_scan_cursor(&cursor, 118);
        assert_eq!(*cursor.lock().unwrap(), Some(118));
        advance_scan_cursor(&cursor, 104);
        assert_eq!(*cursor.lock().unwrap(), Some(118));
        advance_scan_cursor(&cursor, 120);
        assert_eq!(*cursor.lock().unwrap(), Some(120));
    }

    #[tokio::test]
    async fn idle_poll_reads_the_counter_and_scans_no_logs() {
        let rpc = FakeRpc::new(BRIDGE, 130);
        let cursor = Arc::new(Mutex::new(None));
        let source = log_source(&rpc, &cursor);

        assert!(source.fetch(0).await.unwrap().is_none());
        assert_eq!(rpc.take_counter_blocks(), vec![118]);
        assert_eq!(rpc.take_get_logs_ranges(), vec![]);
        assert_eq!(*cursor.lock().unwrap(), Some(118));

        rpc.set_head(140);
        assert!(source.fetch(0).await.unwrap().is_none());
        assert_eq!(rpc.take_get_logs_ranges(), vec![]);
        assert_eq!(*cursor.lock().unwrap(), Some(128));
    }

    #[tokio::test]
    async fn found_deposit_stays_visible_and_so_does_its_neighbour() {
        // Two deposits confirmed in one window. Finding the first must not
        // hide the second, nor the first itself if its finalize then fails.
        let rpc = FakeRpc::new(BRIDGE, 130);
        rpc.deposit(105);
        rpc.deposit(107);
        let cursor = Arc::new(Mutex::new(None));
        let source = log_source(&rpc, &cursor);

        let first = source.fetch(0).await.unwrap().expect("deposit 0");
        assert_eq!(first.block_number, 105);
        assert_eq!(first.tx_hash, fake_rpc::tx_hash(0));
        assert_eq!(first.block_hash, fake_rpc::block_hash(105));
        assert_eq!(
            first.log_index, 1,
            "receipt-local index, not the block-global one"
        );
        assert_eq!(first.source_chain_id, 11_155_111);
        // The scan stops at the chunk that holds the deposit, and the cursor
        // keeps the deposit's block in range.
        assert_eq!(rpc.take_get_logs_ranges(), vec![(100, 109)]);
        assert_eq!(*cursor.lock().unwrap(), Some(104));

        let retry = source.fetch(0).await.unwrap().expect("deposit 0 again");
        assert_eq!(retry.block_number, 105);

        let second = source.fetch(1).await.unwrap().expect("deposit 1");
        assert_eq!(second.block_number, 107);
        assert_eq!(rpc.take_get_logs_ranges(), vec![(105, 114)]);
        assert_eq!(*cursor.lock().unwrap(), Some(106));
    }

    #[tokio::test]
    async fn retrying_a_stuck_deposit_makes_no_further_rpc_scans() {
        let rpc = FakeRpc::new(BRIDGE, 160);
        rpc.deposit(135);
        let cursor = Arc::new(Mutex::new(Some(104)));
        let source = log_source(&rpc, &cursor);

        assert_eq!(source.fetch(0).await.unwrap().unwrap().block_number, 135);
        assert_eq!(rpc.take_counter_blocks(), vec![148]);
        assert_eq!(rpc.take_get_logs_ranges(), vec![
            (105, 114),
            (115, 124),
            (125, 134),
            (135, 144)
        ]);
        assert_eq!(*cursor.lock().unwrap(), Some(134));

        rpc.set_head(400);
        for _ in 0..5 {
            assert_eq!(source.fetch(0).await.unwrap().unwrap().block_number, 135);
        }
        assert_eq!(rpc.take_counter_blocks(), Vec::<u64>::new());
        assert_eq!(rpc.take_get_logs_ranges(), vec![]);

        // A restarted daemon has only the persisted cursor: one chunk.
        let restarted = log_source(&rpc, &cursor);
        assert_eq!(restarted.fetch(0).await.unwrap().unwrap().block_number, 135);
        assert_eq!(rpc.take_get_logs_ranges(), vec![(135, 144)]);
    }

    #[tokio::test]
    async fn scan_includes_from_block_and_starts_after_the_cursor() {
        let rpc = FakeRpc::new(BRIDGE, 130);
        rpc.deposit(FROM_BLOCK);
        rpc.deposit(105);
        rpc.deposit(117);
        let cursor = Arc::new(Mutex::new(None));
        let source = log_source(&rpc, &cursor);

        assert!(source.fetch(0).await.unwrap().is_some());
        assert_eq!(rpc.take_get_logs_ranges(), vec![(100, 109)]);

        *cursor.lock().unwrap() = Some(104);
        assert_eq!(source.fetch(1).await.unwrap().unwrap().block_number, 105);
        assert_eq!(rpc.take_get_logs_ranges(), vec![(105, 114)]);

        assert_eq!(source.fetch(2).await.unwrap().unwrap().block_number, 117);
        assert_eq!(rpc.take_get_logs_ranges(), vec![(105, 114), (115, 118)]);
    }

    #[tokio::test]
    async fn deposit_above_the_confirmed_head_is_waited_for_then_found() {
        let rpc = FakeRpc::new(BRIDGE, 130);
        rpc.deposit(125);
        let cursor = Arc::new(Mutex::new(Some(104)));
        let source = log_source(&rpc, &cursor);

        assert!(source.fetch(0).await.unwrap().is_none());
        assert_eq!(rpc.take_get_logs_ranges(), vec![]);
        assert_eq!(*cursor.lock().unwrap(), Some(118));

        rpc.set_head(137);
        assert_eq!(source.fetch(0).await.unwrap().unwrap().block_number, 125);
        assert_eq!(rpc.take_counter_blocks(), vec![118, 125]);
        assert_eq!(rpc.take_get_logs_ranges(), vec![(119, 125)]);
        assert_eq!(*cursor.lock().unwrap(), Some(124));
    }

    #[tokio::test]
    async fn deposit_the_counter_confirms_but_no_log_shows_is_an_error() {
        // A cursor past the deposit (or incomplete logs from the RPC) must
        // not turn into an endless "not yet".
        let rpc = FakeRpc::new(BRIDGE, 130);
        rpc.deposit(105);
        let cursor = Arc::new(Mutex::new(Some(110)));
        let source = log_source(&rpc, &cursor);

        let err = source.fetch(0).await.unwrap_err().to_string();
        assert!(err.contains("found no Deposit log"), "{err}");
        assert!(err.contains("111..=118"), "{err}");
        assert_eq!(rpc.take_get_logs_ranges(), vec![(111, 118)]);
        assert_eq!(*cursor.lock().unwrap(), Some(110));
    }

    #[test]
    fn is_retryable_detects_429() {
        assert!(is_retryable_eth_rpc_error(
            &"HTTP error 429 with body: compute units per second"
        ));
        assert!(!is_retryable_eth_rpc_error(&"invalid params"));
    }

    #[tokio::test]
    async fn fetch_known_without_a_log_starts_at_the_snapshot_cursor() {
        // An earlier relayer found deposit 0 at block 105 and left the cursor
        // at 104. The live cursor may already sit at the safe head after an
        // idle poll; the retry must still start at 105, not at `--from-block`.
        let rpc = FakeRpc::new(BRIDGE, 130);
        rpc.deposit(105);
        let cursor = Arc::new(Mutex::new(Some(104)));
        let source = log_source(&rpc, &cursor);
        *cursor.lock().unwrap() = Some(118);

        let event = source
            .fetch_known(0, None, None, None)
            .await
            .unwrap()
            .expect("deposit 0");
        assert_eq!(event.block_number, 105);
        assert_eq!(*cursor.lock().unwrap(), Some(118));
        assert_eq!(rpc.take_get_logs_ranges(), vec![(105, 114)]);
    }

    #[tokio::test]
    async fn fetch_known_without_a_log_falls_back_when_the_snapshot_is_past_the_deposit() {
        // A cursor already past the deposit (this daemon's idle poll, or a
        // hand-edited file) still has to find it, without walking every
        // block mined since the snapshot.
        let rpc = FakeRpc::new(BRIDGE, 130);
        rpc.deposit(105);
        let cursor = Arc::new(Mutex::new(Some(110)));
        let source = log_source(&rpc, &cursor);

        let event = source
            .fetch_known(0, None, None, None)
            .await
            .unwrap()
            .expect("deposit 0");
        assert_eq!(event.block_number, 105);
        assert_eq!(*cursor.lock().unwrap(), Some(110));
        assert_eq!(rpc.take_get_logs_ranges(), vec![(111, 118), (100, 109)]);
    }
}
