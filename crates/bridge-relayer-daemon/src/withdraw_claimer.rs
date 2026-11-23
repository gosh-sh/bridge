//! The withdrawal claimer `relayer daemon-withdraw-live` runs: every burn on
//! the Acki Nacki bridge that names this EVM bridge is paid out on the EVM
//! chain, with the daemon's own key paying the gas. A process of its own,
//! apart from `daemon-live`, so that neither waits for the other's proofs or
//! stops with it.
//!
//! Once a minute ([`POLL_INTERVAL`]) the claimer reads the bridge account's
//! ExtOut messages after the last one it saw, keeps every
//! `WithdrawalInitiated` among them in [`CLAIMS_FILE`], and takes each
//! pending one as far as it can, in the order the burns happened. The steps
//! are the ones `ackinacki-bridge withdraw` takes after its burn:
//!
//! 1. the event has to name this chain and this bridge (`dstChainId`,
//!    `dstBridge`); another one is left alone;
//! 2. the bridge has to have verified the bundle that covers the burn's block
//!    ([`covering_bundle_seq_no`]); until then the burn waits;
//! 3. the witness is enriched against the bridge's own windows, read from the
//!    contract ([`BridgeState::from_contract`]), and the nullifier it proves is
//!    looked up: a burn somebody paid already (the CLI, another relayer) is not
//!    proven again;
//! 4. Circuit 4 and its aggregation, one burn at a time;
//! 5. a dry run, then `withdrawByProof` from the daemon's account.
//!
//! A step that fails on an RPC or a GraphQL error is tried again on the next
//! round. A burn that fails otherwise (a proof the bridge rejects, a witness
//! that cannot be built, a prover that dies) is tried again later and later
//! ([`retry_delay`]: one minute, doubling, at most [`MAX_RETRY_DELAY`]), with
//! the reason in the file. A burn the bridge can no longer pay is parked
//! ([`ClaimState::Parked`]) and keeps its event BOC.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use alloy::{
    network::Network,
    primitives::{Address, U256},
    providers::Provider,
};
use anyhow::{bail, Context, Result};
use bridge_event_witness::AnchorLayerMode;
use bridge_gql_fetcher::gql_client::GqlClient;
use bridge_prover_lib::bridge_state::BridgeState;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::{
    bridge::{
        classify_withdraw_revert, decode_withdraw_revert, DryRunOutcome, EthBridgeClient,
        WithdrawRevertKind, WithdrawSubmitOutcome, HISTORY_PROOF_WINDOW,
    },
    withdraw_e2e::{
        driver::{enrich_captured_with, prove_enriched},
        CapturedEvent, WithdrawE2EConfig,
    },
    withdrawal::PartnerWithdrawalProof,
};

/// How often the claimer looks for new burns and moves the pending ones on.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// The claimer's record, in its state directory.
pub const CLAIMS_FILE: &str = "withdraw-claims.json";

/// The `dst` of a `WithdrawalInitiated` ExtOut: `makeAddrExtern(618)`.
pub const WITHDRAWAL_EVENT_DST: &str =
    ":000000000000000000000000000000000000000000000000000000000000026a";

/// The longest a failing burn waits for its next try.
pub const MAX_RETRY_DELAY: Duration = Duration::from_secs(6 * 3600);

/// How long a burn waits after its `failures`-th failure in a row: one
/// minute, doubling, at most [`MAX_RETRY_DELAY`].
pub fn retry_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    POLL_INTERVAL
        .saturating_mul(1u32 << doublings)
        .min(MAX_RETRY_DELAY)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// ExtOut messages read per GraphQL page, and pages read per round at most.
const PAGE: u32 = 50;
const PAGES_PER_ROUND: usize = 20;

/// How long `aggregate-proof` may take, as `ackinacki-bridge withdraw`'s
/// `--prover-timeout-s` default.
const PROVER_TIMEOUT: Duration = Duration::from_secs(1800);

/// The covering bundle is the strictly-next multiple of `stride`: the root
/// at a boundary belongs to the batch before it, so a burn exactly on one
/// waits for the next. `ackinacki-bridge withdraw` waits for the same one.
pub fn covering_bundle_seq_no(burn_seq_no: u64, stride: u64) -> u64 {
    debug_assert!(stride > 0);
    if burn_seq_no == 0 {
        return 0;
    }
    (burn_seq_no / stride) * stride + stride
}

/// Where a burn stands. Every state but `Pending` is final.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ClaimState {
    /// Waiting for coverage, or for the next round after a failure.
    Pending,
    /// The event names another chain or another EVM bridge.
    NotOurs { reason: String },
    /// This relayer's `withdrawByProof` paid it.
    Paid { tx_hash: String },
    /// Its nullifier was spent before this relayer's send landed: somebody
    /// else paid it, or an earlier send of this relayer whose receipt it
    /// did not see.
    AlreadyPaid,
    Parked { reason: String },
}

/// One `WithdrawalInitiated` the claimer saw.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// The ExtOut message id.
    pub message_id: String,
    /// The block's `hash` (what `src_transaction.block_id` carries).
    pub block_hash: String,
    pub created_at: Option<u64>,
    /// The event BOC while the burn is pending; dropped once it is final.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_seq_no: Option<u64>,
    /// What the event asks for, once decoded: base units, the token, the
    /// recipient (0x-hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    #[serde(flatten)]
    pub state: ClaimState,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub anchor_auto: bool,
    /// Failures in a row that were not the network's ([`retry_delay`]).
    #[serde(default)]
    pub failures: u32,
    /// Not tried again before this Unix time, after a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// [`CLAIMS_FILE`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClaimsFile {
    /// The GraphQL cursor of the last ExtOut message read.
    pub cursor: Option<String>,
    /// Every withdrawal seen, in the order they were emitted.
    pub withdrawals: Vec<Claim>,
}

impl ClaimsFile {
    /// The file under `state_dir`, or an empty one when there is none yet.
    pub fn load(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join(CLAIMS_FILE);
        let file: Self = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(file) => file,
                // Moved aside, not fatal: the claimer reads the bridge's
                // events from the start again, and every burn paid already
                // is recognised by its nullifier before anything is proven.
                Err(e) => {
                    let aside = state_dir.join(format!(
                        "{CLAIMS_FILE}.corrupt-{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0)
                    ));
                    std::fs::rename(&path, &aside)
                        .with_context(|| format!("move the unreadable {} aside", path.display()))?;
                    warn!(
                        error = %e,
                        moved_to = %aside.display(),
                        "withdraw claimer: the record does not parse; starting from the first \
                         event again",
                    );
                    Self::default()
                },
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        Ok(file)
    }

    /// Written to a temporary file and renamed over the old one.
    pub fn save(&self, state_dir: &Path) -> Result<()> {
        let path = state_dir.join(CLAIMS_FILE);
        let tmp = state_dir.join(format!("{CLAIMS_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))
    }

    /// Adds every `WithdrawalInitiated` in `page` not seen yet and moves the
    /// cursor past the page. Returns the message ids it added.
    pub fn take_page(
        &mut self,
        page: Vec<(String, bridge_gql_fetcher::gql_client::BridgeExtOutMessage)>,
    ) -> Vec<String> {
        let mut added = Vec::new();
        for (cursor, msg) in page {
            self.cursor = Some(cursor);
            if !msg.dst.eq_ignore_ascii_case(WITHDRAWAL_EVENT_DST) {
                continue;
            }
            if self.withdrawals.iter().any(|c| c.message_id == msg.id) {
                continue;
            }
            added.push(msg.id.clone());
            self.withdrawals.push(Claim {
                message_id: msg.id,
                block_hash: msg.block_id.unwrap_or_default(),
                created_at: msg.created_at,
                boc: Some(msg.boc),
                block_seq_no: None,
                amount: None,
                token_id: None,
                recipient: None,
                state: ClaimState::Pending,
                anchor_auto: false,
                failures: 0,
                retry_at: None,
                last_error: None,
            });
        }
        added
    }
}

/// What the claimer needs besides the bridge client.
#[derive(Clone, Debug)]
pub struct ClaimerConfig {
    /// The Acki Nacki bridge account, `dapp_id` and `account_id` in hex.
    pub an_bridge_dapp_id_hex: String,
    pub an_bridge_account_id_hex: String,
    /// The first GraphQL endpoint; the prover's helpers take one.
    pub gql_endpoint: String,
    /// The EVM chain the bridge is on, from the RPC.
    pub chain_id: u64,
    /// The bridge's anchoring: its `bundleStride` and the level it means.
    pub anchor_level: u8,
    pub stride: u64,
    /// Where [`CLAIMS_FILE`] lives.
    pub state_dir: PathBuf,
    /// Scratch for the witness and the intermediate proof files, one
    /// subdirectory per burn.
    pub work_dir: PathBuf,
    pub params_dir: PathBuf,
    pub verifiers_dir: PathBuf,
    pub aggregate_proof_bin: PathBuf,
    pub pk_cache_dir: PathBuf,
}

/// Why a step stopped: try again next round, or later ([`retry_delay`]).
enum StepError {
    /// The network: an RPC or GraphQL call failed.
    Retry(anyhow::Error),
    /// Anything else: the next try waits [`retry_delay`].
    Refused(String),
    AnchorGone(String),
}

impl From<anyhow::Error> for StepError {
    fn from(e: anyhow::Error) -> Self {
        Self::Retry(e)
    }
}

/// What one pending burn came to this round.
enum Outcome {
    /// Not covered yet, or nothing new.
    Wait,
    Final(ClaimState),
}

/// The claimer: [`Self::run`] until the process stops.
pub struct WithdrawClaimer<P, N>
where
    P: Provider<N> + Clone,
    N: Network,
{
    cfg: ClaimerConfig,
    gql: GqlClient,
    bridge: Arc<EthBridgeClient<P, N>>,
    file: ClaimsFile,
    /// Proofs built but not yet paid, so that a send that failed is not
    /// proven again.
    proofs: HashMap<String, PartnerWithdrawalProof>,
    /// The bridge's windows, read once a round at most: the contract mirror
    /// the witnesses of the round are enriched against.
    round_state: Option<BridgeState>,
}

impl<P, N> WithdrawClaimer<P, N>
where
    P: Provider<N> + Clone + Send + Sync + 'static,
    N: Network,
{
    pub fn new(
        cfg: ClaimerConfig,
        gql: GqlClient,
        bridge: Arc<EthBridgeClient<P, N>>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&cfg.state_dir)
            .with_context(|| format!("state directory {}", cfg.state_dir.display()))?;
        let file = ClaimsFile::load(&cfg.state_dir)?;
        let pending = file
            .withdrawals
            .iter()
            .filter(|c| c.state == ClaimState::Pending)
            .count();
        info!(
            bridge = %bridge.address(),
            chain_id = cfg.chain_id,
            an_bridge = %format!("{}::{}", cfg.an_bridge_dapp_id_hex, cfg.an_bridge_account_id_hex),
            seen = file.withdrawals.len(),
            pending,
            file = %cfg.state_dir.join(CLAIMS_FILE).display(),
            poll_interval_s = POLL_INTERVAL.as_secs(),
            "withdraw claimer starting",
        );
        Ok(Self {
            cfg,
            gql,
            bridge,
            file,
            proofs: HashMap::new(),
            round_state: None,
        })
    }

    /// A round every [`POLL_INTERVAL`], forever. A round that fails is
    /// logged and the next one starts on time.
    pub async fn run(mut self) {
        loop {
            if let Err(e) = self.round().await {
                warn!(error = %format!("{e:#}"), "withdraw claimer: the round failed");
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Reads the new burns, then moves each pending one on.
    pub async fn round(&mut self) -> Result<()> {
        self.round_state = None;
        // The burns found before still move on when GraphQL cannot list new
        // ones this round.
        if let Err(e) = self.discover().await {
            warn!(error = %format!("{e:#}"), "withdraw claimer: could not read new withdrawals");
        }
        for i in 0..self.file.withdrawals.len() {
            let claim = &self.file.withdrawals[i];
            if claim.state != ClaimState::Pending || claim.retry_at.is_some_and(|t| t > unix_now())
            {
                continue;
            }
            let before = self.file.withdrawals[i].clone();
            let mut claim = before.clone();
            let id = claim.message_id.clone();
            match self.advance(&mut claim).await {
                Ok(Outcome::Wait) => {},
                Ok(Outcome::Final(state)) => {
                    if let ClaimState::Parked {
                        reason,
                    } = &state
                    {
                        error!(
                            message_id = %id,
                            %reason,
                            amount = claim.amount.as_deref().unwrap_or("?"),
                            recipient = claim.recipient.as_deref().unwrap_or("?"),
                            "withdraw claimer: parked; not tried again until its state is set \
                             back to \"pending\" in {} and the claimer restarted",
                            self.cfg.state_dir.join(CLAIMS_FILE).display(),
                        );
                    } else {
                        claim.boc = None;
                    }
                    claim.state = state;
                    claim.failures = 0;
                    claim.retry_at = None;
                    claim.last_error = None;
                    self.proofs.remove(&id);
                    let _ = std::fs::remove_dir_all(self.work_dir_for(&id));
                },
                Err(StepError::Retry(e)) => {
                    let text = format!("{e:#}");
                    warn!(message_id = %id, error = %text, "withdraw claimer: will try again next round");
                    claim.last_error = Some(text);
                },
                Err(StepError::Refused(reason)) => self.back_off(&mut claim, reason),
                Err(StepError::AnchorGone(reason)) => {
                    claim.anchor_auto = true;
                    self.back_off(&mut claim, reason);
                },
            }
            if claim != before {
                self.file.withdrawals[i] = claim;
                self.file.save(&self.cfg.state_dir)?;
            }
        }
        Ok(())
    }

    fn back_off(&mut self, claim: &mut Claim, reason: String) {
        claim.failures += 1;
        let wait = retry_delay(claim.failures);
        claim.retry_at = Some(unix_now() + wait.as_secs());
        self.proofs.remove(&claim.message_id);
        let _ = std::fs::remove_dir_all(self.work_dir_for(&claim.message_id));
        warn!(
            message_id = %claim.message_id,
            failures = claim.failures,
            retry_in_s = wait.as_secs(),
            anchor_auto = claim.anchor_auto,
            %reason,
            "withdraw claimer: failed; will try again",
        );
        claim.last_error = Some(reason);
    }

    /// Every ExtOut after the cursor, page by page.
    async fn discover(&mut self) -> Result<()> {
        for _ in 0..PAGES_PER_ROUND {
            let page = self
                .gql
                .query_bridge_extouts_page(
                    &self.cfg.an_bridge_account_id_hex,
                    &self.cfg.an_bridge_dapp_id_hex,
                    PAGE,
                    self.file.cursor.as_deref(),
                )
                .await
                .context("read the bridge's ExtOut messages")?;
            let full = page.len() as u32 >= PAGE;
            if page.is_empty() {
                return Ok(());
            }
            let added = self.file.take_page(page);
            self.file.save(&self.cfg.state_dir)?;
            for message_id in added {
                info!("withdraw claimer: found withdrawal {message_id}");
            }
            if !full {
                return Ok(());
            }
        }
        Ok(())
    }

    fn work_dir_for(&self, message_id: &str) -> PathBuf {
        let short: String = message_id.chars().take(16).collect();
        self.cfg.work_dir.join("withdrawals").join(short)
    }

    /// One burn, as far as it goes this round.
    async fn advance(&mut self, claim: &mut Claim) -> Result<Outcome, StepError> {
        if claim.block_hash.is_empty() {
            // GraphQL listed the message before its block: read it again.
            let msg = self
                .gql
                .query_bridge_extout_by_id(&claim.message_id)
                .await
                .context("the event's block")?;
            match msg.and_then(|m| m.block_id) {
                Some(block) => claim.block_hash = block,
                None => {
                    return Err(StepError::Retry(anyhow::anyhow!(
                        "GraphQL gives the event no block yet"
                    )))
                },
            }
        }
        let Some(boc) = claim.boc.clone() else {
            return Err(StepError::Refused(
                "no event BOC kept for a pending withdrawal".into(),
            ));
        };

        // 1. Ours?
        let event = bridge_event_witness::decode_event_boc_base64(&boc)
            .map_err(|e| StepError::Refused(format!("the event does not decode: {e:#}")))?;
        claim.amount = u128::from_str_radix(&event.amount_hex, 16)
            .ok()
            .map(|a| a.to_string());
        claim.token_id = Some(event.token_id);
        claim.recipient = Some(format!("0x{}", event.recipient_hex));
        let chain = hex::decode(&event.dst_chain_id_hex)
            .ok()
            .filter(|b| b.len() == 32)
            .map(|b| U256::from_be_slice(&b));
        if chain != Some(U256::from(self.cfg.chain_id)) {
            return Ok(Outcome::Final(ClaimState::NotOurs {
                reason: format!(
                    "dstChainId 0x{}, this bridge is on {}",
                    event.dst_chain_id_hex, self.cfg.chain_id
                ),
            }));
        }
        let dst_bridge = hex::decode(&event.dst_bridge_hex)
            .ok()
            .filter(|b| b.len() == 20)
            .map(|b| Address::from_slice(&b));
        if dst_bridge != Some(self.bridge.address()) {
            return Ok(Outcome::Final(ClaimState::NotOurs {
                reason: format!(
                    "dstBridge 0x{}, this bridge is {}",
                    event.dst_bridge_hex,
                    self.bridge.address()
                ),
            }));
        }

        // 2. Covered?
        let block = self
            .gql
            .query_block_by_hash(&claim.block_hash)
            .await
            .with_context(|| format!("block {}", claim.block_hash))?;
        claim.block_seq_no = Some(block.seq_no);
        let target = covering_bundle_seq_no(block.seq_no, self.cfg.stride);
        let last_seen = self
            .bridge
            .stored_last_seen_block_seq_no()
            .await
            .context("storedLastSeenBlockSeqNo")?;
        if last_seen < target {
            info!(
                message_id = %claim.message_id,
                burn_seq_no = block.seq_no,
                covering_bundle = target,
                bridge_last_seen = last_seen,
                "withdraw claimer: waiting for the bundle that covers the burn",
            );
            return Ok(Outcome::Wait);
        }

        let proof = match self.proofs.get(&claim.message_id) {
            Some(p) => p.clone(),
            None => {
                // 3. The witness, against the bridge's own windows.
                let state = match self.round_state.as_ref() {
                    Some(state) if state.stored_last_seen_block_seq_no >= target => state.clone(),
                    _ => {
                        let cfs = self
                            .bridge
                            .read_full_state()
                            .await
                            .context("read_full_state")?;
                        let state = BridgeState::from_contract(
                            cfs,
                            HISTORY_PROOF_WINDOW,
                            self.cfg.anchor_level,
                        )
                        .map_err(|e| {
                            StepError::Refused(format!("BridgeState::from_contract: {e:#}"))
                        })?;
                        self.round_state = Some(state.clone());
                        state
                    },
                };
                if state.stored_last_seen_block_seq_no < target {
                    return Ok(Outcome::Wait);
                }
                let account_dapp_id_hex = self
                    .gql
                    .query_account_dapp_id(
                        &self.cfg.an_bridge_account_id_hex,
                        &self.cfg.an_bridge_dapp_id_hex,
                    )
                    .await
                    .context("the bridge account's dapp_id")?
                    .unwrap_or_else(|| "0".repeat(64));
                let captured = CapturedEvent {
                    event_boc_b64: boc,
                    block_id_hex: block.block_id.clone(),
                    block_hash: block.hash.clone(),
                    block_seq_no: block.seq_no,
                    block_height: block.height,
                    envelope_hash_hex: block.envelope_hash.clone(),
                    account_dapp_id_hex,
                    account_id_hex: self.cfg.an_bridge_account_id_hex.clone(),
                    message_id: claim.message_id.clone(),
                    key_block: block.key_block,
                };
                let e2e_cfg = self.e2e_config(&claim.message_id, block.seq_no, claim.anchor_auto);
                let enriched =
                    match enrich_captured_with(&self.gql, &e2e_cfg, &state, &captured).await {
                        Ok(enriched) => enriched,
                        Err(e) => return witness_failure(&format!("{e:#}"), block.seq_no, &state),
                    };

                // Paid already?
                let nullifier = nullifier_of(&enriched.witness)
                    .map_err(|e| StepError::Refused(format!("{e:#}")))?;
                if self
                    .bridge
                    .is_nullifier_used(nullifier)
                    .await
                    .context("isNullifierUsed")?
                {
                    info!(message_id = %claim.message_id, %nullifier, "withdraw claimer: paid already");
                    return Ok(Outcome::Final(ClaimState::AlreadyPaid));
                }

                // 4. Prove, one proof at a time in this process.
                info!(
                    message_id = %claim.message_id,
                    burn_seq_no = block.seq_no,
                    amount = claim.amount.as_deref().unwrap_or("?"),
                    recipient = claim.recipient.as_deref().unwrap_or("?"),
                    "withdraw claimer: proving the withdrawal",
                );
                let summary = prove_enriched(&e2e_cfg, captured, enriched)
                    .await
                    .map_err(|e| StepError::Refused(format!("the proof: {e:#}")))?;
                info!(message_id = %claim.message_id, "withdraw claimer: proof built");
                self.proofs
                    .insert(claim.message_id.clone(), summary.proof.clone());
                summary.proof
            },
        };

        // 5. Dry run, then send.
        let proof_bytes = proof
            .proof_bytes()
            .map_err(|e| StepError::Refused(format!("proof bytes: {e}")))?;
        let pub_inputs = proof
            .public_inputs()
            .map_err(|e| StepError::Refused(format!("public inputs: {e}")))?;
        if !pub_inputs.names_bridge(self.bridge.address()) {
            return Err(StepError::Refused(format!(
                "the proof names bridge {:#042x}, not {}",
                pub_inputs.dst_bridge,
                self.bridge.address()
            )));
        }
        match self
            .bridge
            .dry_run_withdraw(&proof_bytes, &pub_inputs)
            .await
            .context("dry run")?
        {
            DryRunOutcome::WouldSucceed => {},
            DryRunOutcome::WouldRevert {
                reason,
            } => {
                return self
                    .after_revert(pub_inputs.nullifier, refusal(&reason))
                    .await
            },
        }
        match self
            .bridge
            .submit_withdraw(&proof_bytes, &pub_inputs)
            .await
            .context("withdrawByProof")?
        {
            WithdrawSubmitOutcome::Paid {
                tx_hash,
            } => {
                let tx_hash = format!("{tx_hash:#x}");
                info!(
                    message_id = %claim.message_id,
                    nullifier = %pub_inputs.nullifier,
                    "withdraw claimer: withdrawByProof paid out tx={tx_hash} amount={} \
                     recipient={}",
                    claim.amount.as_deref().unwrap_or("?"),
                    claim.recipient.as_deref().unwrap_or("?"),
                );
                Ok(Outcome::Final(ClaimState::Paid {
                    tx_hash,
                }))
            },
            WithdrawSubmitOutcome::Reverted {
                reason,
                permanent: true,
            } => {
                self.after_revert(pub_inputs.nullifier, refusal(&reason))
                    .await
            },
            // Mined and reverted after a dry run that passed: gas was spent,
            // so the next send waits longer each time, with a new proof.
            WithdrawSubmitOutcome::Reverted {
                reason,
                permanent: false,
            } if reason.contains("reverted on chain") => {
                self.after_revert(pub_inputs.nullifier, Err(StepError::Refused(reason)))
                    .await
            },
            WithdrawSubmitOutcome::Reverted {
                reason,
                permanent: false,
            } => {
                self.after_revert(pub_inputs.nullifier, refusal(&reason))
                    .await
            },
        }
    }

    /// What a revert of the dry run or the send means, asked of the bridge
    /// once more: a spent nullifier is a payout, whatever the revert said.
    /// Another submitter (`ackinacki-bridge withdraw`, a second relayer)
    /// proves the same burn at the same time; when its transaction lands in
    /// the same block as ours, our receipt carries no reason, and a check
    /// the bridge makes before the nullifier's can revert first.
    async fn after_revert(
        &self,
        nullifier: U256,
        outcome: Result<Outcome, StepError>,
    ) -> Result<Outcome, StepError> {
        if matches!(outcome, Ok(Outcome::Final(ClaimState::AlreadyPaid))) {
            return outcome;
        }
        match self.bridge.is_nullifier_used(nullifier).await {
            Ok(true) => {
                info!(%nullifier, "withdraw claimer: paid already, by another submitter");
                Ok(Outcome::Final(ClaimState::AlreadyPaid))
            },
            Ok(false) => outcome,
            Err(e) => {
                warn!(error = %e, "withdraw claimer: isNullifierUsed after a revert failed");
                outcome
            },
        }
    }

    fn e2e_config(
        &self,
        message_id: &str,
        burn_seq_no: u64,
        anchor_auto: bool,
    ) -> WithdrawE2EConfig {
        let work_dir = self.work_dir_for(message_id);
        WithdrawE2EConfig {
            gql_endpoint: self.cfg.gql_endpoint.clone(),
            prover_state_path: PathBuf::new(),
            window_size: 0,
            bridge_account_id_hex: self.cfg.an_bridge_account_id_hex.clone(),
            bridge_dapp_id_hex: self.cfg.an_bridge_dapp_id_hex.clone(),
            event_dst_filter: WITHDRAWAL_EVENT_DST.to_string(),
            event_wait: Duration::ZERO,
            event_poll_interval: Duration::ZERO,
            anchor_mode: anchor_mode(anchor_auto, self.cfg.anchor_level),
            i_know_the_wait: true,
            snark_dir: work_dir.join("shplonk-snark"),
            work_dir,
            aggregate_proof_bin: self.cfg.aggregate_proof_bin.clone(),
            verifiers_dir: self.cfg.verifiers_dir.clone(),
            params_dir: self.cfg.params_dir.clone(),
            pk_cache_dir: Some(self.cfg.pk_cache_dir.clone()),
            prover_out_dir: None,
            prover_timeout: PROVER_TIMEOUT,
            slot_report: None,
            prover_seq_no: u32::try_from(burn_seq_no).unwrap_or(u32::MAX),
            replay_latest: true,
        }
    }
}

/// A revert of the dry run or the send. A spent nullifier ends the claim. A
/// revert intrinsic to the proof ([`classify_withdraw_revert`]), and an anchor
/// the bridge no longer holds, drop the proof and back off. A
/// state of the bridge that passes (paused, the treasury short while AAVE
/// pays back), and anything that does not decode, is tried again next round
/// with the same proof.
fn refusal(reason: &str) -> Result<Outcome, StepError> {
    match decode_withdraw_revert(reason) {
        Some(r) if r.name == "NullifierAlreadyUsed" => Ok(Outcome::Final(ClaimState::AlreadyPaid)),
        Some(r) if r.name == "NacklMinterGone" => Ok(Outcome::Final(ClaimState::Parked {
            reason: format!(
                "withdrawByProof reverts {}: this bridge handed the NACKL minter over and can no \
                 longer mint this payout; settle it outside the claimer",
                r.rendered
            ),
        })),
        Some(r) if r.name == "WithdrawalWindowFull" => Err(StepError::Retry(anyhow::anyhow!(
            "withdrawByProof reverts {}: the EVM bridge's rolling 24-hour withdrawal limit has no \
             room for this payout yet; the nullifier is unspent and the claim is retried later",
            r.rendered
        ))),
        Some(r) if r.name == "UnknownAnchor" => Err(StepError::AnchorGone(format!(
            "withdrawByProof reverts {}: the anchor has left the bridge's window; the next proof \
             picks the layer itself",
            r.rendered
        ))),
        Some(r) if classify_withdraw_revert(reason) == WithdrawRevertKind::Permanent => Err(
            StepError::Refused(format!("withdrawByProof reverts {}", r.rendered)),
        ),
        Some(r) => Err(StepError::Retry(anyhow::anyhow!(
            "withdrawByProof reverts {} for now",
            r.rendered
        ))),
        None => Err(StepError::Retry(anyhow::anyhow!(
            "withdrawByProof: {reason}"
        ))),
    }
}

fn anchor_mode(anchor_auto: bool, anchor_level: u8) -> AnchorLayerMode {
    if anchor_auto {
        AnchorLayerMode::Auto
    } else {
        AnchorLayerMode::Explicit(anchor_level)
    }
}

fn witness_failure(
    text: &str,
    burn_seq_no: u64,
    state: &BridgeState,
) -> Result<Outcome, StepError> {
    if text.contains("Event is older than") {
        let top = (state.num_active_layers() as u8).max(2);
        let pending_boundary = (2..=top).find_map(|n| {
            let boundary = *bridge_prover_lib::real_chain_builder::l_n_anchor_boundaries(
                burn_seq_no,
                bridge_event_witness::HISTORY_WINDOW_SIZE,
                n,
            )
            .last()?;
            (boundary > state.stored_last_seen_block_seq_no).then_some((n, boundary))
        });
        return match pending_boundary {
            Some((n, boundary)) => Err(StepError::Refused(format!(
                "the witness: no layer covers the burn yet; the L{n} boundary {boundary} is not \
                 verified (bridge at {}): {text}",
                state.stored_last_seen_block_seq_no
            ))),
            None => Ok(Outcome::Final(ClaimState::Parked {
                reason: format!(
                    "no active layer of the bridge covers the burn any more; the bridge cannot \
                     pay it until it verifies a higher layer's boundary for it, then set the \
                     claim back to pending: {text}"
                ),
            })),
        };
    }
    if text.contains("rolled out of the L") {
        return Err(StepError::AnchorGone(format!("the witness: {text}")));
    }
    Err(StepError::Refused(format!("the witness: {text}")))
}

/// The nullifier the proof of `witness` will carry, computed natively from
/// the witness (public input [8]).
pub fn nullifier_of(witness: &bridge_event_witness::schema::PrivateWitness) -> Result<U256> {
    use halo2_base::utils::ScalarField;
    let inputs = bridge_event_prover_lib::prover::build_proof_inputs(
        witness,
        bridge_event_prover_lib::prover::default_event_circuit_params(),
    )
    .context("the proof's public inputs")?;
    let Some(nullifier) = inputs.public_instances.get(8) else {
        bail!(
            "the witness gives {} public inputs, not 12",
            inputs.public_instances.len()
        );
    };
    let le = nullifier.to_bytes_le();
    Ok(U256::from_le_slice(&le))
}

#[cfg(test)]
mod tests {
    use bridge_gql_fetcher::gql_client::BridgeExtOutMessage;

    use super::*;

    fn msg(id: &str, dst: &str, block: Option<&str>) -> BridgeExtOutMessage {
        BridgeExtOutMessage {
            id: id.into(),
            boc: format!("boc-{id}"),
            dst: dst.into(),
            created_at: Some(1),
            src_dapp_id: None,
            block_id: block.map(str::to_string),
        }
    }

    #[test]
    fn a_burn_on_a_boundary_waits_for_the_next_bundle() {
        assert_eq!(covering_bundle_seq_no(0, 16384), 0);
        assert_eq!(covering_bundle_seq_no(1, 16384), 16384);
        assert_eq!(covering_bundle_seq_no(16383, 16384), 16384);
        assert_eq!(covering_bundle_seq_no(16384, 16384), 32768);
    }

    #[test]
    fn a_page_keeps_only_new_withdrawals_and_moves_the_cursor() {
        let mut file = ClaimsFile::default();
        let added = file.take_page(vec![
            ("c1".into(), msg("a", WITHDRAWAL_EVENT_DST, Some("h1"))),
            (
                "c2".into(),
                msg(
                    "b",
                    ":0000000000000000000000000000000000000000000000000000000000000268",
                    Some("h2"),
                ),
            ),
            (
                "c3".into(),
                msg("c", &WITHDRAWAL_EVENT_DST.to_uppercase(), Some("h3")),
            ),
        ]);
        assert_eq!(added, ["a", "c"]);
        assert_eq!(file.cursor.as_deref(), Some("c3"));
        assert_eq!(
            file.withdrawals
                .iter()
                .map(|c| c.message_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "c"]
        );
        // The same page again adds nothing.
        let again = file.take_page(vec![(
            "c3".into(),
            msg("c", WITHDRAWAL_EVENT_DST, Some("h3")),
        )]);
        assert!(again.is_empty());
        assert_eq!(file.withdrawals.len(), 2);
    }

    #[test]
    fn an_event_listed_without_its_block_waits_for_it() {
        let mut file = ClaimsFile::default();
        file.take_page(vec![("c1".into(), msg("a", WITHDRAWAL_EVENT_DST, None))]);
        assert_eq!(file.withdrawals[0].state, ClaimState::Pending);
        assert!(file.withdrawals[0].block_hash.is_empty());
    }

    #[test]
    fn an_unreadable_record_is_moved_aside_and_the_claimer_starts_over() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CLAIMS_FILE), b"{not json").unwrap();
        let file = ClaimsFile::load(dir.path()).unwrap();
        assert!(file.cursor.is_none() && file.withdrawals.is_empty());
        assert!(!dir.path().join(CLAIMS_FILE).exists());
        let aside = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("withdraw-claims.json.corrupt-")
            });
        assert!(aside, "the unreadable record is kept beside");
    }

    #[test]
    fn the_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = ClaimsFile::default();
        file.take_page(vec![
            ("c1".into(), msg("a", WITHDRAWAL_EVENT_DST, Some("h1"))),
            ("c2".into(), msg("b", WITHDRAWAL_EVENT_DST, Some("h2"))),
        ]);
        file.withdrawals[0].state = ClaimState::Paid {
            tx_hash: "0x01".into(),
        };
        file.withdrawals[1].failures = 2;
        file.withdrawals[1].retry_at = Some(7);
        file.save(dir.path()).unwrap();
        let back = ClaimsFile::load(dir.path()).unwrap();
        assert_eq!(back.cursor, file.cursor);
        assert_eq!(back.withdrawals, file.withdrawals);
    }

    #[test]
    fn a_failing_burn_waits_longer_each_time_up_to_six_hours() {
        assert_eq!(retry_delay(1), Duration::from_secs(60));
        assert_eq!(retry_delay(2), Duration::from_secs(120));
        assert_eq!(retry_delay(5), Duration::from_secs(960));
        assert_eq!(retry_delay(9), Duration::from_secs(15360));
        assert_eq!(retry_delay(10), MAX_RETRY_DELAY);
        assert_eq!(retry_delay(u32::MAX), MAX_RETRY_DELAY);
    }

    #[test]
    fn a_missing_file_is_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = ClaimsFile::load(dir.path()).unwrap();
        assert!(file.cursor.is_none() && file.withdrawals.is_empty());
    }

    #[test]
    fn a_spent_nullifier_ends_the_claim_and_other_reverts_count() {
        let selector = |sig: &str| hex::encode(&alloy::primitives::keccak256(sig.as_bytes())[..4]);
        let spent = format!(
            "execution reverted: 0x{}{:064x}",
            selector("NullifierAlreadyUsed(uint256)"),
            7
        );
        assert!(matches!(
            refusal(&spent),
            Ok(Outcome::Final(ClaimState::AlreadyPaid))
        ));
        let rejected = format!(
            "execution reverted: 0x{}",
            selector("WithdrawalProofRejected()")
        );
        assert!(matches!(refusal(&rejected), Err(StepError::Refused(_))));
        let evicted = format!(
            "execution reverted: 0x{}{:064x}",
            selector("UnknownAnchor(uint256)"),
            5
        );
        assert!(matches!(refusal(&evicted), Err(StepError::AnchorGone(_))));
        let paused = format!("execution reverted: 0x{}", selector("BridgePaused()"));
        assert!(matches!(refusal(&paused), Err(StepError::Retry(_))));
        assert!(matches!(
            refusal("connection reset"),
            Err(StepError::Retry(_))
        ));
    }

    #[test]
    fn a_minter_handed_over_parks_the_claim_and_a_full_window_waits() {
        let selector = |sig: &str| hex::encode(&alloy::primitives::keccak256(sig.as_bytes())[..4]);
        let gone = format!("execution reverted: 0x{}", selector("NacklMinterGone()"));
        assert!(matches!(
            refusal(&gone),
            Ok(Outcome::Final(ClaimState::Parked { .. }))
        ));
        let full = format!(
            "execution reverted: 0x{}{:064x}{:064x}{:064x}{:064x}",
            selector("WithdrawalWindowFull(uint32,uint256,uint256,uint256)"),
            1,
            900,
            200,
            1000
        );
        assert!(matches!(refusal(&full), Err(StepError::Retry(_))));
        let not_minter = format!(
            "execution reverted: 0x{}{:064x}",
            selector("NacklNotMinter(address)"),
            0x11
        );
        assert!(matches!(refusal(&not_minter), Err(StepError::Retry(_))));
    }

    #[test]
    fn a_retry_after_the_anchor_left_lets_the_proof_pick_the_layer() {
        assert!(matches!(
            anchor_mode(false, 2),
            AnchorLayerMode::Explicit(2)
        ));
        assert!(matches!(anchor_mode(true, 2), AnchorLayerMode::Auto));
    }

    fn state_at(last_seen: u64, active_layers: u8) -> BridgeState {
        let mut state = BridgeState::new(HISTORY_PROOF_WINDOW);
        state.stored_last_seen_block_seq_no = last_seen;
        for layer in 1..=active_layers {
            state.append_layer(layer, [layer; 32], last_seen);
        }
        state
    }

    #[test]
    fn a_witness_past_every_active_layer_parks_the_claim() {
        let rolled = "anchor key block height 9 not found in L1 window [] — block has rolled out \
                      of the L1 rolling window. Re-run with `--anchor-layer auto`";
        assert!(matches!(
            witness_failure(rolled, 100, &state_at(5_000_000, 2)),
            Err(StepError::AnchorGone(_))
        ));
        let older = "auto: L1 (K=1024, height=9) and all higher active layers rolled out of their \
                     rolling windows. Event is older than verifier coverage.";
        assert!(matches!(
            witness_failure(older, 100, &state_at(5_000_000, 2)),
            Ok(Outcome::Final(ClaimState::Parked { .. }))
        ));
        assert!(matches!(
            witness_failure(older, 100, &state_at(10_000, 2)),
            Err(StepError::Refused(_))
        ));
        assert!(matches!(
            witness_failure(older, 100, &state_at(1_000_000, 3)),
            Err(StepError::Refused(_))
        ));
        assert!(matches!(
            witness_failure("GraphQL timed out", 100, &state_at(5_000_000, 2)),
            Err(StepError::Refused(_))
        ));
    }

    #[test]
    fn a_parked_claim_round_trips_and_older_files_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = ClaimsFile::default();
        file.take_page(vec![
            ("c1".into(), msg("a", WITHDRAWAL_EVENT_DST, Some("h1"))),
            ("c2".into(), msg("b", WITHDRAWAL_EVENT_DST, Some("h2"))),
        ]);
        file.withdrawals[0].state = ClaimState::Parked {
            reason: "NacklMinterGone()".into(),
        };
        file.withdrawals[1].anchor_auto = true;
        file.save(dir.path()).unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(CLAIMS_FILE)).unwrap()).unwrap();
        assert_eq!(raw["withdrawals"][0]["state"], "parked");
        assert_eq!(raw["withdrawals"][0]["reason"], "NacklMinterGone()");
        assert_eq!(raw["withdrawals"][0]["boc"], "boc-a");
        assert!(raw["withdrawals"][0].get("anchor_auto").is_none());
        assert_eq!(raw["withdrawals"][1]["anchor_auto"], true);
        assert_eq!(
            ClaimsFile::load(dir.path()).unwrap().withdrawals,
            file.withdrawals
        );

        let old = r#"{"cursor":"c1","withdrawals":[{"message_id":"a","block_hash":"h1",
            "created_at":1,"boc":"boc-a","state":"pending","failures":3},
            {"message_id":"b","block_hash":"h2","boc":"boc-b","state":"pending",
            "reason":"NacklMinterGone()"}]}"#;
        std::fs::write(dir.path().join(CLAIMS_FILE), old).unwrap();
        let back = ClaimsFile::load(dir.path()).unwrap();
        assert_eq!(back.withdrawals[0].state, ClaimState::Pending);
        assert!(!back.withdrawals[0].anchor_auto);
        assert_eq!(back.withdrawals[0].failures, 3);
        assert_eq!(back.withdrawals[1].state, ClaimState::Pending);
        assert_eq!(back.withdrawals[1].boc.as_deref(), Some("boc-b"));
    }
}
