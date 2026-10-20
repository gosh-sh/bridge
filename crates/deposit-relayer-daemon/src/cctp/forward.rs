//! Watch `CctpEndpoint.HubDeposit` and submit the Circle attestation on the
//! hub.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use alloy::{
    eips::BlockId,
    network::{EthereumWallet, ReceiptResponse},
    primitives::{keccak256, Address, Bytes, B256},
    providers::{Provider, ProviderBuilder},
    rpc::types::{Filter, TransactionRequest},
    signers::local::PrivateKeySigner,
    sol,
    sol_types::{SolCall, SolEvent},
    transports::{RpcError, TransportErrorKind},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use super::iris::{expected_domain, normalize_tx_hash, AttestedMessage, IrisClass, IrisClient};
use crate::{
    error::RelayerError, is_supported_deposit_chain,
    supported_chains::supported_deposit_chain_name, StateLock,
};

sol! {
    #[sol(rpc)]
    #[allow(missing_docs)]
    contract CctpHubReceiver {
        event HubDeposit(
            address indexed token,
            address indexed depositor,
            uint32 destinationDomain,
            bytes32 anAccount,
            uint256 amount,
            uint256 maxFee
        );

        function receiveBurn(bytes message, bytes attestation) external returns (bytes32 key, uint256 minted);
        function forward(bytes32 key) external;
        function pending(bytes32 key) external view returns (uint256 amount, bytes32 anAccount, address depositor, int8 anWorkchain);
        function forwarders(address account) external view returns (bool);
        function messageTransmitter() external view returns (address);
    }

    #[sol(rpc)]
    #[allow(missing_docs)]
    contract MessageTransmitterV2 {
        function usedNonces(bytes32 nonce) external view returns (uint256);
    }
}

/// Give up on a burn after this many failed submissions. Iris `Pending` does
/// not count: Circle not having signed yet is not a failure.
pub const MAX_FORWARD_FAILURES: u32 = 8;

const LOG_CHUNK_BLOCKS: u64 = 10;
/// Deposit circuit limits (`deposit-prover/src/circuit_v2.rs`:
/// `PRODUCTION_MAX_LOG_NUM`, and EIP-2930 / EIP-1559 enclosing transactions).
const MAX_PROVABLE_LOGS: usize = 20;
const PROVABLE_TX_TYPES: [u8; 2] = [1, 2];
/// Iris lookups per tick. A backlog waits for the next poll instead of
/// bursting the public API.
const MAX_LOOKUPS_PER_TICK: usize = 20;

/// One-shot: one source burn transaction.
pub struct CctpForwardConfig {
    pub iris_url: String,
    pub source_domain: u32,
    pub tx_hash: String,
    pub hub_rpc_url: String,
    pub receiver: Address,
    pub private_key: Option<String>,
    pub dry_run: bool,
    pub wait_secs: u64,
    pub poll_secs: u64,
}

/// Long-running: every `HubDeposit` on `endpoint` since `from_block`.
pub struct CctpDaemonConfig {
    pub source_rpc_url: String,
    pub source_domain: u32,
    pub endpoint: Address,
    pub from_block: u64,
    pub confirmations: u64,
    pub iris_url: String,
    pub hub_rpc_url: String,
    pub receiver: Address,
    pub private_key: String,
    pub poll_secs: u64,
    pub state_path: PathBuf,
    pub force_state: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ForwardState {
    pub schema: u32,
    pub source_chain_id: u64,
    pub source_domain: u32,
    pub endpoint: String,
    pub hub_chain_id: u64,
    pub receiver: String,
    pub last_scanned_block: Option<u64>,
    #[serde(default)]
    pub pending: Vec<PendingBurn>,
    #[serde(default)]
    pub submitted: Vec<String>,
    #[serde(default)]
    pub gave_up: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PendingBurn {
    pub tx_hash: String,
    pub failures: u32,
}

impl ForwardState {
    pub fn fresh(
        source_chain_id: u64,
        source_domain: u32,
        endpoint: Address,
        hub_chain_id: u64,
        receiver: Address,
    ) -> Self {
        Self {
            schema: 1,
            source_chain_id,
            source_domain,
            endpoint: endpoint.to_string(),
            hub_chain_id,
            receiver: receiver.to_string(),
            last_scanned_block: None,
            pending: Vec::new(),
            submitted: Vec::new(),
            gave_up: Vec::new(),
        }
    }

    pub fn matches(&self, other: &Self) -> bool {
        self.source_chain_id == other.source_chain_id
            && self.source_domain == other.source_domain
            && addr_eq(&self.endpoint, &other.endpoint)
            && self.hub_chain_id == other.hub_chain_id
            && addr_eq(&self.receiver, &other.receiver)
    }

    /// First block to ask `eth_getLogs` for. A cursor already past
    /// `from_block` is not rewound; lowering `--from-block` does not rescan.
    pub fn next_scan_block(&self, from_block: u64) -> u64 {
        match self.last_scanned_block {
            Some(last) => from_block.max(last.saturating_add(1)),
            None => from_block,
        }
    }

    pub fn ingest(&mut self, tx_hashes: &[String]) {
        for tx in tx_hashes {
            if self.submitted.iter().any(|s| s == tx) || self.gave_up.iter().any(|s| s == tx) {
                continue;
            }
            if self.pending.iter().any(|p| &p.tx_hash == tx) {
                continue;
            }
            self.pending.push(PendingBurn {
                tx_hash: tx.clone(),
                failures: 0,
            });
        }
    }

    pub fn mark_submitted(&mut self, tx_hash: &str) {
        self.pending.retain(|p| p.tx_hash != tx_hash);
        if !self.submitted.iter().any(|s| s == tx_hash) {
            self.submitted.push(tx_hash.to_string());
        }
    }

    pub fn give_up(&mut self, tx_hash: &str) {
        self.pending.retain(|p| p.tx_hash != tx_hash);
        if !self.gave_up.iter().any(|s| s == tx_hash) {
            self.gave_up.push(tx_hash.to_string());
            error!(
                %tx_hash,
                "giving up on CCTP burn after repeated failures; funds stay parked or unminted. \
                 Retry with cctp-forward --tx-hash; gave_up is never retried by the daemon"
            );
        }
    }

    pub fn record_failure(&mut self, tx_hash: &str) {
        let failures = {
            let Some(entry) = self.pending.iter_mut().find(|p| p.tx_hash == tx_hash) else {
                return;
            };
            entry.failures = entry.failures.saturating_add(1);
            entry.failures
        };
        if failures >= MAX_FORWARD_FAILURES {
            self.give_up(tx_hash);
        }
    }

    pub fn load(path: &Path) -> Result<Option<Self>, RelayerError> {
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(path)?;
        let state = serde_json::from_slice(&bytes)?;
        Ok(Some(state))
    }

    pub fn save(&self, path: &Path) -> Result<(), RelayerError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = {
            let mut tmp = path.to_path_buf();
            let mut name = path
                .file_name()
                .map(|s| s.to_owned())
                .unwrap_or_else(|| std::ffi::OsString::from("cctp-forward-state.json"));
            name.push(".tmp");
            tmp.set_file_name(name);
            tmp
        };
        let bytes = serde_json::to_vec_pretty(self)?;
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

pub async fn run_cctp_forward(cfg: CctpForwardConfig) -> Result<(), RelayerError> {
    if !cfg.dry_run && cfg.private_key.is_none() {
        return Err(RelayerError::other(
            "cctp-forward needs --private-key (or CCTP_HUB_PRIVATE_KEY), or --dry-run",
        ));
    }
    let iris = IrisClient::new(&cfg.iris_url)?;
    let tx_hash = normalize_tx_hash(&cfg.tx_hash)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(cfg.wait_secs);
    let attested = loop {
        match iris
            .lookup(cfg.source_domain, &tx_hash, cfg.receiver)
            .await?
        {
            IrisClass::Ready {
                messages, ..
            } => break messages,
            IrisClass::Unrelated => {
                return Err(RelayerError::other(format!(
                    "Iris has messages for {tx_hash} but none name receiver {}",
                    cfg.receiver
                )));
            },
            IrisClass::Pending => {
                if cfg.wait_secs == 0 || std::time::Instant::now() >= deadline {
                    return Err(RelayerError::other(format!(
                        "Circle has not attested {tx_hash} yet (source domain {})",
                        cfg.source_domain
                    )));
                }
                info!(%tx_hash, "attestation not ready, waiting");
                tokio::time::sleep(Duration::from_secs(cfg.poll_secs.max(1))).await;
            },
        }
    };
    if cfg.dry_run {
        for (i, message) in attested.iter().enumerate() {
            info!(
                index = i,
                message_bytes = message.message.len(),
                attestation_bytes = message.attestation.len(),
                "dry-run: attestation ready, not submitting"
            );
        }
        return Ok(());
    }
    let Some(key) = cfg.private_key.as_deref() else {
        return Err(RelayerError::other(
            "cctp-forward needs --private-key (or CCTP_HUB_PRIVATE_KEY), or --dry-run",
        ));
    };
    let provider = hub_provider(&cfg.hub_rpc_url, key).await?;
    let hub = ChainHub::connect(provider, cfg.receiver).await?;
    for message in &attested {
        submit_one(&hub, message)
            .await
            .map_err(|e| RelayerError::eth(e.to_string()))?;
    }
    Ok(())
}

pub async fn run_cctp_daemon(cfg: CctpDaemonConfig) -> Result<(), RelayerError> {
    let _lock = StateLock::acquire(&cfg.state_path)?;
    let source = ProviderBuilder::new().connect_http(parse_url(&cfg.source_rpc_url)?);
    let source_chain_id = source
        .get_chain_id()
        .await
        .map_err(|e| RelayerError::eth(format!("source eth_chainId: {e}")))?;
    warn_domain(source_chain_id, cfg.source_domain, "source");

    let hub = hub_provider(&cfg.hub_rpc_url, &cfg.private_key).await?;
    let hub_chain_id = hub
        .get_chain_id()
        .await
        .map_err(|e| RelayerError::eth(format!("hub eth_chainId: {e}")))?;
    if !is_supported_deposit_chain(hub_chain_id) {
        warn!(
            hub_chain_id,
            "hub chain is not in the deposit-prover allowlist; a forwarded Deposit cannot be \
             proved until it is"
        );
    }

    let fresh = ForwardState::fresh(
        source_chain_id,
        cfg.source_domain,
        cfg.endpoint,
        hub_chain_id,
        cfg.receiver,
    );
    let mut state = match ForwardState::load(&cfg.state_path)? {
        Some(existing) if existing.matches(&fresh) => existing,
        Some(existing) if cfg.force_state => {
            warn!(
                path = %cfg.state_path.display(),
                "CCTP state identity differs; --force-state starts a new cursor"
            );
            let _ = existing;
            fresh
        },
        Some(_) => {
            return Err(RelayerError::other(format!(
                "CCTP state {} belongs to a different source, domain, endpoint, hub or receiver. \
                 Pass --force-state to discard it.",
                cfg.state_path.display()
            )));
        },
        None => fresh,
    };

    let signer: PrivateKeySigner = cfg
        .private_key
        .parse()
        .map_err(|e| RelayerError::other(format!("CCTP hub private key: {e}")))?;
    let signer_addr = signer.address();
    let allowed = forwarder_allowed(&hub, cfg.receiver, signer_addr).await?;
    if !allowed {
        return Err(RelayerError::other(format!(
            "hub signer {signer_addr} is not a forwarder on {}; owner must setForwarder",
            cfg.receiver
        )));
    }
    let balance = hub
        .get_balance(signer_addr)
        .await
        .map_err(|e| RelayerError::eth(format!("signer balance: {e}")))?;
    if balance.is_zero() {
        return Err(RelayerError::other(format!(
            "hub signer {signer_addr} has no ETH for gas"
        )));
    }

    let hub = ChainHub::connect(hub, cfg.receiver).await?;
    let iris = IrisClient::new(&cfg.iris_url)?;
    info!(
        source_chain_id,
        source_domain = cfg.source_domain,
        endpoint = %cfg.endpoint,
        hub_chain_id,
        receiver = %cfg.receiver,
        pending = state.pending.len(),
        "cctp-daemon started"
    );

    loop {
        if let Err(error) = tick(&source, &hub, &iris, &cfg, &mut state).await {
            warn!(%error, "cctp tick failed; retrying next poll");
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("cctp-daemon stopping");
                state.save(&cfg.state_path)?;
                return Ok(());
            }
            _ = tokio::time::sleep(Duration::from_secs(cfg.poll_secs.max(1))) => {}
        }
    }
}

async fn tick<S, H>(
    source: &S,
    hub: &H,
    iris: &IrisClient,
    cfg: &CctpDaemonConfig,
    state: &mut ForwardState,
) -> Result<(), RelayerError>
where
    S: Provider,
    H: HubOps,
{
    let head = source
        .get_block_number()
        .await
        .map_err(|e| RelayerError::eth(format!("source block number: {e}")))?;
    let safe = head.saturating_sub(cfg.confirmations);
    let from = state.next_scan_block(cfg.from_block);
    if from <= safe {
        let txs = hub_deposit_txs(source, cfg.endpoint, from, safe).await?;
        state.ingest(&txs);
        state.last_scanned_block = Some(safe);
    }

    let mut pending: Vec<(String, u32)> = state
        .pending
        .iter()
        .map(|p| (p.tx_hash.clone(), p.failures))
        .collect();
    pending.sort_by_key(|(_, failures)| *failures);
    let pending: Vec<String> = pending
        .into_iter()
        .take(MAX_LOOKUPS_PER_TICK)
        .map(|(tx, _)| tx)
        .collect();
    for tx_hash in pending {
        match iris.lookup(cfg.source_domain, &tx_hash, cfg.receiver).await {
            Ok(IrisClass::Pending) => {},
            Ok(IrisClass::Unrelated) => {
                warn!(%tx_hash, "Iris messages do not name this receiver; giving up");
                state.give_up(&tx_hash);
            },
            Ok(IrisClass::Ready {
                messages,
                complete,
            }) => {
                let mut failed = false;
                for message in &messages {
                    match submit_one(hub, message).await {
                        Ok(()) => {},
                        Err(error) => {
                            warn!(%tx_hash, %error, "hub submission failed");
                            if !matches!(error, HubError::Transient(_)) {
                                state.record_failure(&tx_hash);
                            }
                            failed = true;
                            break;
                        },
                    }
                }
                if !failed && complete {
                    state.mark_submitted(&tx_hash);
                    info!(%tx_hash, "forwarded to hub");
                } else if !failed {
                    info!(
                        %tx_hash,
                        submitted = messages.len(),
                        "some burns in this transaction are still pending Circle attestation"
                    );
                }
            },
            Err(error) => {
                warn!(%tx_hash, %error, "Iris lookup failed");
            },
        }
    }
    state.save(&cfg.state_path)?;
    Ok(())
}

async fn hub_deposit_txs<P: Provider>(
    provider: &P,
    endpoint: Address,
    from: u64,
    to: u64,
) -> Result<Vec<String>, RelayerError> {
    let mut out = Vec::new();
    let mut cursor = from;
    while cursor <= to {
        let end = cursor.saturating_add(LOG_CHUNK_BLOCKS - 1).min(to);
        let filter = Filter::new()
            .address(endpoint)
            .event_signature(CctpHubReceiver::HubDeposit::SIGNATURE_HASH)
            .from_block(cursor)
            .to_block(end);
        let logs = provider
            .get_logs(&filter)
            .await
            .map_err(|e| RelayerError::eth(format!("HubDeposit logs {cursor}..={end}: {e}")))?;
        for log in logs {
            if let Some(hash) = log.transaction_hash {
                let text = normalize_tx_hash(&hash.to_string())?;
                if !out.contains(&text) {
                    out.push(text);
                }
            }
        }
        cursor = end.saturating_add(1);
    }
    Ok(out)
}

async fn hub_provider(hub_rpc_url: &str, private_key: &str) -> Result<impl Provider, RelayerError> {
    let signer: PrivateKeySigner = private_key
        .parse()
        .map_err(|e| RelayerError::other(format!("CCTP hub private key: {e}")))?;
    info!(signer = %signer.address(), "hub submitter");
    let wallet = EthereumWallet::from(signer);
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(parse_url(hub_rpc_url)?);
    Ok(provider)
}

/// A failed hub step. `Transient` does not count towards
/// [`MAX_FORWARD_FAILURES`]: the RPC was unreachable or rate-limited, or a
/// receipt did not arrive in time — the transaction may still land, and the
/// next tick reads the outcome from chain state.
#[derive(Debug, thiserror::Error)]
enum HubError {
    #[error("{0}")]
    Transient(String),
    #[error("{0}")]
    Failed(String),
}

impl From<RelayerError> for HubError {
    fn from(e: RelayerError) -> Self {
        Self::Failed(e.to_string())
    }
}

fn rpc_error(what: &str, e: RpcError<TransportErrorKind>) -> HubError {
    let transient = match &e {
        RpcError::Transport(_) => true,
        RpcError::ErrorResp(payload) => matches!(payload.code, 429 | -32005),
        _ => false,
    };
    let text = format!("{what}: {e}");
    if transient {
        HubError::Transient(text)
    } else {
        HubError::Failed(text)
    }
}

/// One burn on the hub, both values read at the same block.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BurnState {
    /// `MessageTransmitterV2.usedNonces(nonce) != 0`.
    nonce_used: bool,
    /// `CctpHubReceiver.pending(key).amount != 0`.
    parked: bool,
}

#[async_trait]
trait HubOps: Send + Sync {
    async fn burn_state(&self, nonce: B256, key: B256) -> Result<BurnState, HubError>;
    async fn receive_burn(&self, message: &AttestedMessage) -> Result<(), HubError>;
    async fn forward(&self, key: B256) -> Result<(), HubError>;
}

/// Settle one burn from the hub's state, never from revert text. The message's
/// `destinationCaller` is the receiver, so only its `receiveBurn` can use the
/// nonce, and `forward` is the only way `pending(key)` goes back to zero:
///
/// - nonce unused: `receiveBurn`, then `forward`;
/// - parked: `forward`;
/// - nonce used, nothing parked: already forwarded, done.
///
/// After a failed step the state is read again, so a burn another forwarder
/// parked or forwarded in the meantime is not counted as a failure.
async fn submit_one(hub: &impl HubOps, message: &AttestedMessage) -> Result<(), HubError> {
    let nonce = message_nonce(&message.message)?;
    let key = message_key(&message.message)?;
    let mut state = hub.burn_state(nonce, key).await?;
    if !state.nonce_used {
        match hub.receive_burn(message).await {
            Ok(()) => {
                state = BurnState {
                    nonce_used: true,
                    parked: true,
                }
            },
            Err(error) => {
                state = hub.burn_state(nonce, key).await?;
                if !state.nonce_used {
                    return Err(error);
                }
                info!(%key, %error, "receiveBurn failed but another caller received the burn");
            },
        }
    }
    if !state.parked {
        info!(%key, "burn already forwarded");
        return Ok(());
    }
    match hub.forward(key).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let after = hub.burn_state(nonce, key).await?;
            if after.nonce_used && !after.parked {
                info!(%key, %error, "forward failed but the burn is already forwarded");
                return Ok(());
            }
            Err(error)
        },
    }
}

/// [`HubOps`] on a live hub chain.
struct ChainHub<P> {
    provider: P,
    receiver: Address,
    transmitter: Address,
    /// Highest block of a receipt this process has seen. State is never read
    /// below it, so a load-balanced RPC lagging behind our own transaction
    /// cannot report a burn as not received yet.
    seen_block: AtomicU64,
}

impl<P: Provider> ChainHub<P> {
    async fn connect(provider: P, receiver: Address) -> Result<Self, RelayerError> {
        let transmitter = CctpHubReceiver::new(receiver, &provider)
            .messageTransmitter()
            .call()
            .await
            .map_err(|e| RelayerError::eth(format!("receiver messageTransmitter(): {e}")))?;
        Ok(Self {
            provider,
            receiver,
            transmitter,
            seen_block: AtomicU64::new(0),
        })
    }

    fn saw_receipt(&self, receipt: &impl ReceiptResponse) {
        if let Some(block) = receipt.block_number() {
            self.seen_block.fetch_max(block, Ordering::Relaxed);
        }
    }

    async fn wait_receipt(
        &self,
        pending: alloy::providers::PendingTransactionBuilder<alloy::network::Ethereum>,
        what: &str,
    ) -> Result<alloy::rpc::types::TransactionReceipt, HubError> {
        match tokio::time::timeout(RECEIPT_WAIT, pending.get_receipt()).await {
            Ok(Ok(receipt)) => {
                self.saw_receipt(&receipt);
                Ok(receipt)
            },
            Ok(Err(e)) => Err(HubError::Transient(format!("{what} receipt: {e}"))),
            Err(_) => Err(HubError::Transient(format!(
                "{what} receipt timed out after {}s",
                RECEIPT_WAIT.as_secs()
            ))),
        }
    }

    async fn call_at<C: SolCall>(
        &self,
        to: Address,
        call: C,
        block: u64,
        what: &str,
    ) -> Result<C::Return, HubError> {
        let tx = TransactionRequest::default()
            .to(to)
            .input(call.abi_encode().into());
        let out = self
            .provider
            .call(tx)
            .block(BlockId::number(block))
            .await
            .map_err(|e| rpc_error(what, e))?;
        C::abi_decode_returns(&out).map_err(|e| HubError::Failed(format!("decode {what}: {e}")))
    }
}

#[async_trait]
impl<P: Provider> HubOps for ChainHub<P> {
    async fn burn_state(&self, nonce: B256, key: B256) -> Result<BurnState, HubError> {
        let head = self
            .provider
            .get_block_number()
            .await
            .map_err(|e| rpc_error("hub block number", e))?;
        let seen = self.seen_block.load(Ordering::Relaxed);
        if head < seen {
            return Err(HubError::Transient(format!(
                "hub RPC is at block {head}, behind our last receipt at {seen}"
            )));
        }
        let used = self
            .call_at(
                self.transmitter,
                MessageTransmitterV2::usedNoncesCall {
                    nonce,
                },
                head,
                "usedNonces",
            )
            .await?;
        let parked = self
            .call_at(
                self.receiver,
                CctpHubReceiver::pendingCall {
                    key,
                },
                head,
                "pending",
            )
            .await?;
        Ok(BurnState {
            nonce_used: !used.is_zero(),
            parked: !parked.amount.is_zero(),
        })
    }

    async fn receive_burn(&self, message: &AttestedMessage) -> Result<(), HubError> {
        let call = CctpHubReceiver::receiveBurnCall {
            message: Bytes::copy_from_slice(&message.message),
            attestation: Bytes::copy_from_slice(&message.attestation),
        };
        let tx = TransactionRequest::default()
            .to(self.receiver)
            .input(call.abi_encode().into());
        let pending = self
            .provider
            .send_transaction(tx)
            .await
            .map_err(|e| expired_hint(rpc_error("send receiveBurn", e)))?;
        let receipt = self.wait_receipt(pending, "receiveBurn").await?;
        if !receipt.status() {
            return Err(HubError::Failed(format!(
                "receiveBurn reverted (tx {})",
                receipt.transaction_hash
            )));
        }
        info!(tx = %receipt.transaction_hash, "receiveBurn mined");
        Ok(())
    }

    /// The deposit circuit proves this transaction's `Deposit`: type 1 or 2,
    /// at most [`MAX_PROVABLE_LOGS`] logs in the receipt. Sent as EIP-1559
    /// with no access list.
    async fn forward(&self, key: B256) -> Result<(), HubError> {
        let fees = self
            .provider
            .estimate_eip1559_fees()
            .await
            .map_err(|e| rpc_error("EIP-1559 fee estimate", e))?;
        let tx = TransactionRequest::default()
            .to(self.receiver)
            .input(
                CctpHubReceiver::forwardCall {
                    key,
                }
                .abi_encode()
                .into(),
            )
            .max_fee_per_gas(fees.max_fee_per_gas)
            .max_priority_fee_per_gas(fees.max_priority_fee_per_gas);
        let pending = self
            .provider
            .send_transaction(tx)
            .await
            .map_err(|e| rpc_error("send forward", e))?;
        let receipt = self.wait_receipt(pending, "forward").await?;
        if !receipt.status() {
            return Err(HubError::Failed(format!(
                "forward reverted (tx {}); is this key a forwarder on the receiver?",
                receipt.transaction_hash
            )));
        }
        let tx_type = u8::from(receipt.transaction_type());
        let logs = receipt.logs().len();
        if !PROVABLE_TX_TYPES.contains(&tx_type) || logs > MAX_PROVABLE_LOGS {
            error!(
                tx = %receipt.transaction_hash,
                tx_type,
                logs,
                "hub Deposit is not provable: the deposit circuit needs a type 1 or 2 transaction \
                 with at most 20 logs"
            );
        } else {
            info!(tx = %receipt.transaction_hash, %key, "forward mined; hub Deposit emitted");
        }
        Ok(())
    }
}

/// Circle refuses a message whose attestation outlived its expiration block;
/// only a fresh signature from Iris lets it through.
fn expired_hint(error: HubError) -> HubError {
    match error {
        HubError::Failed(text) if text.to_ascii_lowercase().contains("expired") => {
            HubError::Failed(format!(
                "{text} — the attestation has expired; ask Iris to re-sign it with POST \
                 /v2/reattest/<nonce>, then retry"
            ))
        },
        other => other,
    }
}

const MESSAGE_NONCE: std::ops::Range<usize> = 12..44;

fn message_nonce(message: &[u8]) -> Result<B256, RelayerError> {
    message
        .get(MESSAGE_NONCE)
        .map(B256::from_slice)
        .ok_or_else(|| {
            RelayerError::other(format!(
                "CCTP message is {} bytes, too short for a nonce",
                message.len()
            ))
        })
}

/// `CctpHubReceiver.messageKey(sourceDomain, nonce)` from the raw message:
/// source domain at byte 4, nonce at byte 12.
pub fn message_key(message: &[u8]) -> Result<B256, RelayerError> {
    let nonce = message_nonce(message)?;
    let mut encoded = [0u8; 64];
    encoded[28..32].copy_from_slice(&message[4..8]);
    encoded[32..64].copy_from_slice(nonce.as_slice());
    Ok(keccak256(encoded))
}

const RECEIPT_WAIT: Duration = Duration::from_secs(120);

async fn forwarder_allowed<P: Provider>(
    provider: &P,
    receiver: Address,
    signer: Address,
) -> Result<bool, RelayerError> {
    let tx = TransactionRequest::default().to(receiver).input(
        CctpHubReceiver::forwardersCall {
            account: signer,
        }
        .abi_encode()
        .into(),
    );
    let out = provider
        .call(tx)
        .await
        .map_err(|e| RelayerError::eth(format!("forwarders({signer}): {e}")))?;
    CctpHubReceiver::forwardersCall::abi_decode_returns(&out)
        .map_err(|e| RelayerError::eth(format!("decode forwarders({signer}): {e}")))
}

fn warn_domain(chain_id: u64, domain: u32, which: &str) {
    if let Some(expected) = expected_domain(chain_id) {
        if expected != domain {
            let name = supported_deposit_chain_name(chain_id).unwrap_or("unknown");
            warn!(
                chain_id,
                chain = name,
                domain,
                expected,
                "{which} --source-domain does not match the CCTP domain this chain id usually uses"
            );
        }
    }
}

fn parse_url(raw: &str) -> Result<reqwest::Url, RelayerError> {
    raw.parse()
        .map_err(|e| RelayerError::other(format!("RPC URL {raw}: {e}")))
}

fn addr_eq(left: &str, right: &str) -> bool {
    match (left.parse::<Address>(), right.parse::<Address>()) {
        (Ok(a), Ok(b)) => a == b,
        _ => left.eq_ignore_ascii_case(right),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ForwardState {
        ForwardState::fresh(
            42_161,
            3,
            Address::from([0x11; 20]),
            11_155_111,
            Address::from([0xaa; 20]),
        )
    }

    #[test]
    fn ingest_skips_submitted_and_duplicates() {
        let mut state = state();
        let tx = "0x".to_string() + &"ab".repeat(32);
        state.ingest(std::slice::from_ref(&tx));
        state.ingest(std::slice::from_ref(&tx));
        assert_eq!(state.pending.len(), 1);
        state.mark_submitted(&tx);
        state.ingest(std::slice::from_ref(&tx));
        assert!(state.pending.is_empty());
        assert_eq!(state.submitted, vec![tx]);
    }

    #[test]
    fn failures_give_up_at_the_cap() {
        let mut state = state();
        let tx = "0x".to_string() + &"cd".repeat(32);
        state.ingest(std::slice::from_ref(&tx));
        for _ in 0..MAX_FORWARD_FAILURES {
            state.record_failure(&tx);
        }
        assert!(state.pending.is_empty());
        assert_eq!(state.gave_up, vec![tx.clone()]);
        state.ingest(&[tx]);
        assert!(state.pending.is_empty());
    }

    #[test]
    fn scan_cursor_does_not_rewind() {
        let mut state = state();
        assert_eq!(state.next_scan_block(100), 100);
        state.last_scanned_block = Some(150);
        assert_eq!(state.next_scan_block(100), 151);
        assert_eq!(state.next_scan_block(400), 400);
    }

    #[test]
    fn message_key_matches_abi_encode_of_domain_and_nonce() {
        use alloy::sol_types::SolValue;
        let nonce = B256::from([0x5a; 32]);
        let mut message = vec![0u8; 400];
        message[4..8].copy_from_slice(&3u32.to_be_bytes());
        message[12..44].copy_from_slice(nonce.as_slice());
        let expected = keccak256((3u32, nonce).abi_encode());
        assert_eq!(message_key(&message).unwrap(), expected);
        assert!(message_key(&message[..43]).is_err());
    }

    /// A hub whose state moves the way `CctpHubReceiver` does: `receiveBurn`
    /// uses the nonce and parks, `forward` unparks. `other_*` simulate another
    /// forwarder winning the race just before our transaction.
    #[derive(Default)]
    struct FakeHub {
        state: std::sync::Mutex<BurnState>,
        other_receives_first: bool,
        other_forwards_first: bool,
        forward_fails: std::sync::Mutex<u32>,
        calls: std::sync::Mutex<Vec<&'static str>>,
    }

    impl FakeHub {
        fn at(state: BurnState) -> Self {
            Self {
                state: std::sync::Mutex::new(state),
                ..Self::default()
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl HubOps for FakeHub {
        async fn burn_state(&self, _nonce: B256, _key: B256) -> Result<BurnState, HubError> {
            Ok(*self.state.lock().unwrap())
        }

        async fn receive_burn(&self, _message: &AttestedMessage) -> Result<(), HubError> {
            self.calls.lock().unwrap().push("receiveBurn");
            let mut state = self.state.lock().unwrap();
            if self.other_receives_first {
                *state = BurnState {
                    nonce_used: true,
                    parked: true,
                };
            }
            if state.parked {
                return Err(HubError::Failed("AlreadyPending".into()));
            }
            *state = BurnState {
                nonce_used: true,
                parked: true,
            };
            Ok(())
        }

        async fn forward(&self, _key: B256) -> Result<(), HubError> {
            self.calls.lock().unwrap().push("forward");
            let mut fails = self.forward_fails.lock().unwrap();
            if *fails > 0 {
                *fails -= 1;
                return Err(HubError::Failed("forward reverted".into()));
            }
            let mut state = self.state.lock().unwrap();
            if self.other_forwards_first {
                state.parked = false;
            }
            if !state.parked {
                return Err(HubError::Failed("NotPending".into()));
            }
            state.parked = false;
            Ok(())
        }
    }

    fn message() -> AttestedMessage {
        AttestedMessage {
            message: vec![0u8; 400],
            attestation: vec![0u8; 65],
        }
    }

    const UNUSED: BurnState = BurnState {
        nonce_used: false,
        parked: false,
    };
    const PARKED: BurnState = BurnState {
        nonce_used: true,
        parked: true,
    };
    const FORWARDED: BurnState = BurnState {
        nonce_used: true,
        parked: false,
    };

    #[tokio::test]
    async fn fresh_burn_is_received_then_forwarded() {
        let hub = FakeHub::at(UNUSED);
        submit_one(&hub, &message()).await.unwrap();
        assert_eq!(hub.calls(), ["receiveBurn", "forward"]);
        assert_eq!(*hub.state.lock().unwrap(), FORWARDED);
    }

    #[tokio::test]
    async fn burn_parked_by_someone_else_is_only_forwarded() {
        let hub = FakeHub::at(PARKED);
        submit_one(&hub, &message()).await.unwrap();
        assert_eq!(hub.calls(), ["forward"]);
        assert_eq!(*hub.state.lock().unwrap(), FORWARDED);
    }

    #[tokio::test]
    async fn burn_parked_during_our_receive_is_forwarded() {
        let hub = FakeHub {
            other_receives_first: true,
            ..FakeHub::at(UNUSED)
        };
        submit_one(&hub, &message()).await.unwrap();
        assert_eq!(hub.calls(), ["receiveBurn", "forward"]);
        assert_eq!(*hub.state.lock().unwrap(), FORWARDED);
    }

    #[tokio::test]
    async fn already_forwarded_burn_sends_nothing() {
        let hub = FakeHub::at(FORWARDED);
        submit_one(&hub, &message()).await.unwrap();
        assert!(hub.calls().is_empty());
    }

    #[tokio::test]
    async fn burn_forwarded_during_our_forward_is_success() {
        let hub = FakeHub {
            other_forwards_first: true,
            ..FakeHub::at(PARKED)
        };
        submit_one(&hub, &message()).await.unwrap();
        assert_eq!(hub.calls(), ["forward"]);
    }

    #[tokio::test]
    async fn failed_forward_is_retried_with_forward_alone() {
        let hub = FakeHub::at(UNUSED);
        *hub.forward_fails.lock().unwrap() = 1;
        let error = submit_one(&hub, &message()).await.unwrap_err();
        assert!(matches!(error, HubError::Failed(_)));
        assert_eq!(*hub.state.lock().unwrap(), PARKED);

        submit_one(&hub, &message()).await.unwrap();
        assert_eq!(hub.calls(), ["receiveBurn", "forward", "forward"]);
        assert_eq!(*hub.state.lock().unwrap(), FORWARDED);
    }

    #[test]
    fn expired_attestation_gets_a_reattest_hint() {
        let hinted = expired_hint(HubError::Failed(
            "execution reverted: Message expired".into(),
        ));
        assert!(hinted.to_string().contains("/v2/reattest/"));
        let other = expired_hint(HubError::Failed("execution reverted: NotForwarder".into()));
        assert!(!other.to_string().contains("reattest"));
    }

    #[test]
    fn state_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cctp-forward-state.json");
        let mut state = state();
        state.ingest(&["0x".to_string() + &"11".repeat(32)]);
        state.save(&path).unwrap();
        let loaded = ForwardState::load(&path).unwrap().unwrap();
        assert!(loaded.matches(&state));
        assert_eq!(loaded.pending.len(), 1);
    }
}
