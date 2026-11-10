//! [`AnSubmitter`] — abstraction over "how do we deliver the deposit to Acki
//! Nacki and finalise it?".
//!
//! The AN-side entry point is `eccBridge.finalizeDeposit(uint256 chainId,
//! bytes header, uint16 txIndex, bytes[] receiptProof, uint16 logIndex)`. The
//! bridge requires `keccak256(header)` to be an accepted block of `chainId`
//! (exit 224 otherwise, before it accepts the message), proves the receipt of
//! transaction `txIndex` against the header's `receiptsRoot`, reads log
//! `logIndex` of that receipt as the bridge's `Deposit` event, and deploys a
//! per-deposit `DepositVoucher`, whose `confirmDeposit` call back into the
//! bridge mints. Every deposit field comes from the proven receipt; the
//! relayer cannot change any of them.
//!
//! Two implementations:
//!
//! - [`MockAnSubmitter`] — an in-memory mirror of the voucher semantics, used
//!   by the unit tests to drive the relayer through many deposits without a
//!   live AN node.
//! - [`AnInterfaceSubmitter`] — wraps any [`acki_nacki_interface::IAckiNacki`]
//!   client: it ABI-encodes the `finalizeDeposit` call into an
//!   [`acki_nacki_interface::AckiNackiTransaction`] and sends it. The live
//!   client is [`acki_nacki_interface::TvmAckiNacki`]. A second
//!   `finalizeDeposit` for the same deposit fails in the voucher constructor
//!   (exit 51), so re-submission is rejected rather than double-crediting.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use acki_nacki_interface::{
    types::TxHash, AckiNackiError, ContractCallRequest, ExtendedAddress, IAckiNacki,
    TransactionStatus,
};
use alloy::primitives::U256;
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::OnceCell;
use tracing::{debug, warn};
use tvm_client::{abi::Abi, ClientContext};

use crate::{
    error::RelayerError,
    types::DepositEvent,
    voucher::{self, DepositIdentity},
    witness::{check_gas_credit, check_witness_matches, DepositWitness},
};

/// Outcome of an [`AnSubmitter::submit`] call.
#[derive(Clone, Debug)]
pub enum SubmitOutcome {
    /// `finalizeDeposit` succeeded; the deposit's nullifier is now set.
    Finalized {
        /// `Some` for the live client (the AN tx hash), `None` for the mock.
        tx_hash: Option<[u8; 32]>,
    },
    /// The `depositId` nullifier was already consumed (someone else
    /// finalised it, or we retried after a successful submit we didn't
    /// observe). Treated as success for progress purposes.
    AlreadyFinalized,
    /// AN rejected the submission (receipt proof refused, malformed call).
    /// The relayer logs and records an attempt.
    Rejected { reason: String },
    /// The tx was broadcast but confirmation timed out while still pending.
    /// Distinct from [`Rejected`] — the relayer retries without counting it
    /// as a failed attempt.
    Pending { reason: String },
}

#[async_trait]
pub trait AnSubmitter: Send + Sync {
    /// Read the AN bridge's canonical-chain gate before building the receipt
    /// proof. Errors must propagate: an unreadable gate is not permission to
    /// submit.
    async fn is_block_accepted(&self, event: &DepositEvent) -> Result<bool, RelayerError>;

    /// Cheap pre-check: has this deposit already been finalised on AN? Used
    /// to skip building and sending on replays. A backend that
    /// can't tell returns `Ok(false)` (the submit path is still safe — AN
    /// rejects double-finalisation).
    async fn is_finalized(&self, event: &DepositEvent) -> Result<bool, RelayerError>;

    /// Send `finalizeDeposit` with `witness` and finalise the deposit.
    async fn submit(
        &self,
        event: &DepositEvent,
        witness: &DepositWitness,
    ) -> Result<SubmitOutcome, RelayerError>;
}

// ─────────────────────────────────────────────────────────────────────
// MockAnSubmitter — in-memory nullifier mirror
// ─────────────────────────────────────────────────────────────────────

/// Verifier decision callback — returns `true` if the AN bridge would accept
/// the receipt proof. Tests inject `false` to drive the rejection path.
pub type VerifierDecision = Arc<dyn Fn(&DepositWitness) -> bool + Send + Sync>;

/// In-memory mirror of the AN `finalizeDeposit` voucher semantics.
pub struct MockAnSubmitter {
    inner: Mutex<MockInner>,
    verifier: VerifierDecision,
}

struct MockInner {
    nullifiers: HashSet<u64>,
    finalized_log: Vec<u64>,
}

impl MockAnSubmitter {
    /// Every receipt proof accepted (the common happy path).
    pub fn accepting() -> Self {
        Self::with_verifier(Arc::new(|_| true))
    }

    pub fn with_verifier(verifier: VerifierDecision) -> Self {
        Self {
            inner: Mutex::new(MockInner {
                nullifiers: HashSet::new(),
                finalized_log: Vec::new(),
            }),
            verifier,
        }
    }

    /// Pre-seed a finalised deposit (simulate another relayer having already
    /// processed it).
    pub fn seed_finalized(&self, deposit_id: u64) {
        let mut inner = self.inner.lock().expect("poisoned lock");
        inner.nullifiers.insert(deposit_id);
    }

    pub fn finalized_count(&self) -> usize {
        self.inner
            .lock()
            .expect("poisoned lock")
            .finalized_log
            .len()
    }

    pub fn finalized_log(&self) -> Vec<u64> {
        self.inner
            .lock()
            .expect("poisoned lock")
            .finalized_log
            .clone()
    }
}

#[async_trait]
impl AnSubmitter for MockAnSubmitter {
    async fn is_block_accepted(&self, _: &DepositEvent) -> Result<bool, RelayerError> {
        Ok(true)
    }

    async fn is_finalized(&self, event: &DepositEvent) -> Result<bool, RelayerError> {
        Ok(self
            .inner
            .lock()
            .expect("poisoned lock")
            .nullifiers
            .contains(&event.deposit_id))
    }

    async fn submit(
        &self,
        event: &DepositEvent,
        witness: &DepositWitness,
    ) -> Result<SubmitOutcome, RelayerError> {
        check_witness_matches(event, witness)?;
        check_gas_credit(witness)?;

        let mut inner = self.inner.lock().expect("poisoned lock");
        if inner.nullifiers.contains(&event.deposit_id) {
            return Ok(SubmitOutcome::AlreadyFinalized);
        }
        if !(self.verifier)(witness) {
            return Ok(SubmitOutcome::Rejected {
                reason: "the mock bridge refused the receipt proof".to_string(),
            });
        }
        inner.nullifiers.insert(event.deposit_id);
        inner.finalized_log.push(event.deposit_id);
        Ok(SubmitOutcome::Finalized {
            tx_hash: None,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────
// AnInterfaceSubmitter — live delivery over IAckiNacki
// ─────────────────────────────────────────────────────────────────────

/// Static configuration for the live submitter. `finalizeDeposit` carries
/// no caller-supplied gas: the bridge pays its own after `tvm.accept()`.
#[derive(Clone, Debug)]
pub struct AnSubmitConfig {
    /// Relayer signer in SDK 3.0 `dapp_id::account_id` form.
    pub from: String,
    /// Bridge contract in SDK 3.0 `dapp_id::account_id` form.
    pub token_bridge: String,
    /// Seconds to wait for the finalize tx to confirm.
    pub confirm_timeout_secs: u64,
}

/// Live AN submitter. Encodes `finalizeDeposit` and sends it through an
/// [`IAckiNacki`] client.
///
/// With [`Self::with_voucher_check`], `is_finalized` looks for the deposit's
/// `DepositVoucher` on AN (see [`crate::voucher`]); without it, or when the
/// lookup fails, it returns `Ok(false)` and the deposit is submitted. That is
/// safe: a duplicate `finalizeDeposit` still exits 0, but its voucher deploy
/// fails with exit 51, which [`settle_finalize`] reports as
/// [`SubmitOutcome::AlreadyFinalized`].
pub struct AnInterfaceSubmitter<C: IAckiNacki> {
    client: Arc<C>,
    config: AnSubmitConfig,
    voucher_check: Option<VoucherCheck>,
}

/// What the voucher pre-check needs beyond the client.
struct VoucherCheck {
    ctx: Arc<ClientContext>,
    bridge_abi: Abi,
    /// `_depositVoucherCode`, read from the bridge once per process.
    code: OnceCell<String>,
}

impl<C: IAckiNacki> AnInterfaceSubmitter<C> {
    pub fn new(client: Arc<C>, config: AnSubmitConfig) -> Self {
        Self {
            client,
            config,
            voucher_check: None,
        }
    }

    /// Answer `is_finalized` by looking for the deposit's `DepositVoucher`.
    /// `bridge_abi` is the deployed bridge's ABI; its storage fields are what
    /// locates `_depositVoucherCode`.
    pub fn with_voucher_check(mut self, bridge_abi: Abi) -> Result<Self, RelayerError> {
        self.voucher_check = Some(VoucherCheck {
            ctx: voucher::offline_context()?,
            bridge_abi,
            code: OnceCell::new(),
        });
        Ok(self)
    }

    #[cfg(test)]
    fn with_voucher_code(mut self, code: &str) -> Self {
        self.voucher_check = Some(VoucherCheck {
            ctx: voucher::offline_context().unwrap(),
            bridge_abi: Abi::Json("{}".to_string()),
            code: OnceCell::new_with(Some(code.to_string())),
        });
        self
    }

    async fn voucher_code(&self, check: &VoucherCheck) -> Result<String, RelayerError> {
        check
            .code
            .get_or_try_init(|| async {
                let bridge = ExtendedAddress::parse(&self.config.token_bridge)
                    .map_err(RelayerError::from)?;
                let data = self
                    .client
                    .get_account_info(&bridge)
                    .await
                    .map_err(RelayerError::from)?
                    .and_then(|info| info.data)
                    .ok_or_else(|| RelayerError::other("bridge account has no data"))?;
                voucher::voucher_code_from_bridge_data(&check.ctx, &check.bridge_abi, &data)
            })
            .await
            .cloned()
    }

    async fn voucher_active(
        &self,
        check: &VoucherCheck,
        event: &DepositEvent,
    ) -> Result<bool, RelayerError> {
        let code = self.voucher_code(check).await?;
        let id = DepositIdentity {
            deposit_id: U256::from(event.deposit_id),
            contract_addr: U256::from_be_slice(event.source_contract.as_slice()),
            chain_id: U256::from(event.source_chain_id),
        };
        let account = voucher::voucher_account_id(&check.ctx, &code, &id)?;
        // A voucher is the root of its own dapp.
        let address =
            ExtendedAddress::parse(&format!("{account}::{account}")).map_err(RelayerError::from)?;
        let info = self
            .client
            .get_account_info(&address)
            .await
            .map_err(RelayerError::from)?;
        debug!(deposit_id = event.deposit_id, voucher = %account, ?info, "voucher pre-check");
        Ok(info.is_some_and(|i| i.active))
    }

    /// Submit a witness directly, without a [`DepositEvent`] to compare it
    /// with: the bridge reads every deposit field from the proven receipt, so
    /// this is the path of the `finalize-one` operator command, where the
    /// Ethereum event is not fetched again. A witness the bridge could not
    /// check within its gas credit is refused before anything is sent.
    pub async fn submit_witness(
        &self,
        witness: &DepositWitness,
    ) -> Result<SubmitOutcome, RelayerError> {
        check_gas_credit(witness)?;
        let from = ExtendedAddress::parse(&self.config.from).map_err(RelayerError::from)?;
        let to = ExtendedAddress::parse(&self.config.token_bridge).map_err(RelayerError::from)?;
        let params = witness.finalize_params();
        let call = ContractCallRequest {
            from,
            to,
            function: "finalizeDeposit".to_string(),
            params,
        };

        let sent = match self.client.call_contract(call).await {
            Ok(hash) => hash,
            Err(e) => return Ok(classify_call_error(&e.to_string())),
        };
        let receipt = self
            .client
            .wait_for_confirmation(&sent, self.config.confirm_timeout_secs)
            .await?;

        match receipt.status {
            TransactionStatus::Confirmed | TransactionStatus::Included => self.settle(&sent).await,
            TransactionStatus::Reverted | TransactionStatus::Failed => Ok(classify_reverted(
                receipt.exit_code,
                &format!("{:?}", receipt.status),
            )),
            TransactionStatus::Pending => Ok(SubmitOutcome::Pending {
                reason: "finalizeDeposit tx still pending after timeout".to_string(),
            }),
        }
    }

    /// Waits up to `confirm_timeout_secs` for [`settle_finalize`] to reach a
    /// verdict.
    async fn settle(&self, root: &TxHash) -> Result<SubmitOutcome, RelayerError> {
        let deadline =
            Instant::now() + Duration::from_secs(self.config.confirm_timeout_secs.max(1));
        let mut last_error = None;
        loop {
            match settle_finalize(self.client.as_ref(), root).await {
                Ok(Some(outcome)) => return Ok(outcome),
                Ok(None) => {},
                Err(AckiNackiError::NotImplemented) => {
                    warn!(
                        tx = %hex::encode(root),
                        "AN client cannot read child transactions; finalizeDeposit counted as \
                         finalized without checking the voucher"
                    );
                    return Ok(SubmitOutcome::Finalized {
                        tx_hash: Some(*root),
                    });
                },
                Err(e) => last_error = Some(e.to_string()),
            }
            if Instant::now() >= deadline {
                let why = last_error.map(|e| format!(": {e}")).unwrap_or_default();
                return Ok(SubmitOutcome::Pending {
                    reason: format!(
                        "finalizeDeposit {} exited 0 but its voucher deploy and confirmDeposit \
                         are not both visible yet{why}",
                        hex::encode(root)
                    ),
                });
            }
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }
}

/// TVM: the constructor of an already deployed contract was called again — a
/// `DepositVoucher` for this deposit exists, so it was finalized before.
const EXIT_CONSTRUCTOR_ALREADY_CALLED: i32 = 51;
/// TVM: cell underflow. From a voucher constructor, the signature of a bridge
/// whose `_depositVoucherCode` predates the voucher's constructor ABI.
const EXIT_CELL_UNDERFLOW: i32 = 9;

// eccBridge `modifiers/errors.sol`.
const EXIT_ZERO_AMOUNT: i32 = 204;
const EXIT_INVALID_SENDER: i32 = 207;
const EXIT_OVERFLOW: i32 = 214;
const EXIT_HASH_MISMATCH: i32 = 219;
const EXIT_UNSUPPORTED_SRC_CHAIN: i32 = 222;
const EXIT_UNKNOWN_BLOCK: i32 = 224;
const EXIT_ZERO_RECIPIENT: i32 = 230;
const EXIT_PAUSED: i32 = 231;
const EXIT_TRANSFER_TOO_LARGE: i32 = 234;
const EXIT_TVL_CAP: i32 = 235;
const EXIT_TOKEN_UNREGISTERED: i32 = 241;
const EXIT_TOKEN_DEPOSITS_DISABLED: i32 = 243;
const EXIT_NOT_DEPOSIT_LOG: i32 = 244;

// eccBridge `EthTrie.sol`: the receipt proof.
const EXIT_TRIE_SHORT: i32 = 300;
const EXIT_TRIE_RLP: i32 = 301;
const EXIT_TRIE_NODE_HASH: i32 = 302;
const EXIT_TRIE_PATH: i32 = 303;
const EXIT_TRIE_INLINE: i32 = 304;
const EXIT_TRIE_ABSENT: i32 = 305;
const EXIT_NO_LOG: i32 = 307;
const EXIT_RECEIPT_STATUS: i32 = 308;

/// What 300–305 share: the proof does not lead from the header's
/// `receiptsRoot` to the receipt.
const RECEIPT_PROOF_HINT: &str = "the receipt proof does not match the header's receiptsRoot; the \
                                  relayer builds a fresh one on the next attempt. If it keeps \
                                  failing, the source RPC serves receipts that are not the \
                                  block's: point --rpc-url at another node";

/// Names the `finalizeDeposit` revert and, where the cause is bridge
/// configuration rather than a bad deposit, says which call fixes it. The
/// fail-closed settings must be re-set by hand after every deploy or code
/// upgrade (they are not threaded through `onCodeUpgrade`), so without this the
/// first deposit after a redeploy reads as a broken bridge.
fn describe_exit_code(code: i32) -> Option<&'static str> {
    match code {
        EXIT_ZERO_AMOUNT => Some("ERR_ZERO_AMOUNT — the proven deposit amount is zero"),
        EXIT_OVERFLOW => Some("ERR_OVERFLOW — the proven deposit amount does not fit a uint64"),
        EXIT_NOT_DEPOSIT_LOG => Some(
            "ERR_NOT_DEPOSIT_LOG — the log at logIndex of the proven receipt is not a Deposit \
             event in the shape the bridge reads (three topics, four data words); the EVM bridge \
             emitting it is not the one this AN bridge was built for",
        ),
        EXIT_TRIE_SHORT => Some("EthTrie ERR_SHORT — a receipt proof item ends early"),
        EXIT_TRIE_RLP => Some("EthTrie ERR_RLP — a receipt proof item is malformed"),
        EXIT_TRIE_NODE_HASH => {
            Some("EthTrie ERR_NODE_HASH — a receipt proof node does not hash to its parent")
        },
        EXIT_TRIE_PATH => Some("EthTrie ERR_PATH — the receipt proof does not follow txIndex"),
        EXIT_TRIE_INLINE => Some(
            "EthTrie ERR_INLINE — the receipt trie of this block has a node under 32 bytes, which \
             the bridge cannot walk; the deposit needs the EVM bridge operator",
        ),
        EXIT_TRIE_ABSENT => Some("EthTrie ERR_ABSENT — no receipt at txIndex in this block"),
        EXIT_NO_LOG => Some(
            "EthTrie ERR_NO_LOG — the proven receipt has no log at logIndex; logIndex counts \
             within the receipt, not the block",
        ),
        EXIT_RECEIPT_STATUS => Some(
            "EthTrie ERR_RECEIPT_STATUS — the deposit transaction failed on the source chain, so \
             there is no deposit to finalize",
        ),
        EXIT_UNSUPPORTED_SRC_CHAIN => Some(
            "ERR_UNSUPPORTED_SRC_CHAIN — the proven (chainId, source contract) pair is not \
             allowlisted; owner must call setTrustedL1Bridge(chainId, l1Bridge, true)",
        ),
        EXIT_UNKNOWN_BLOCK => Some(
            "ERR_UNKNOWN_BLOCK — the deposit's block is not in the canonical-chain anchor set; \
             confirm canonicality with `scripts/deposit_anchor_params.py --verify`, then the \
             chain's anchor writer must admit it — the owner's setAcceptedBlockHash(chainId, \
             blockHash, true) while owner anchors are enabled for that chain, the light client \
             for Ethereum, the Arbitrum block verifier (`eth-lc-relayer arbitrum-anchors`) for \
             Arbitrum",
        ),
        EXIT_ZERO_RECIPIENT => Some("ERR_ZERO_RECIPIENT — the proven AN recipient is zero"),
        EXIT_TRANSFER_TOO_LARGE => {
            Some("ERR_TRANSFER_TOO_LARGE — the proven deposit exceeds the per-transfer ceiling")
        },
        _ => None,
    }
}

/// Explains a failed `DepositVoucher` constructor. It runs in its own
/// transaction after `finalizeDeposit` has already exited 0, so nothing was
/// minted and nothing upstream reports it.
fn describe_voucher_exit(code: Option<i32>) -> &'static str {
    match code {
        Some(EXIT_CELL_UNDERFLOW) => {
            "cell underflow in the voucher constructor — the bridge's _depositVoucherCode does not \
             match the DepositVoucher constructor it calls; compare getDepositVoucherCodeHash() \
             with the compiled DepositVoucher and redeploy the pair"
        },
        Some(EXIT_INVALID_SENDER) => {
            "ERR_INVALID_SENDER — the voucher's USDC_BRIDGE_ADDRESS is not this bridge"
        },
        Some(EXIT_HASH_MISMATCH) => {
            "ERR_HASH_MISMATCH — the voucher's deposit hash disagrees with the bridge's"
        },
        _ => "DepositVoucher deploy failed",
    }
}

fn rejected(code: i32, context: &str, detail: &str) -> SubmitOutcome {
    let reason = match (describe_exit_code(code), code) {
        (
            Some(hint),
            EXIT_TRIE_SHORT | EXIT_TRIE_RLP | EXIT_TRIE_NODE_HASH | EXIT_TRIE_PATH
            | EXIT_TRIE_ABSENT,
        ) => format!("{hint}; {RECEIPT_PROOF_HINT} (exit_code={code}): {detail}"),
        (Some(hint), _) => format!("{hint} (exit_code={code}): {detail}"),
        (None, _) => format!("{context} (exit_code={code}): {detail}"),
    };
    SubmitOutcome::Rejected {
        reason,
    }
}

fn classify_exit(code: i32, context: &str, detail: &str) -> SubmitOutcome {
    match code {
        EXIT_CONSTRUCTOR_ALREADY_CALLED => SubmitOutcome::AlreadyFinalized,
        // A paused lane rejects every deposit; counting that against the
        // deposit would park healthy deposits for the length of the pause.
        EXIT_PAUSED => SubmitOutcome::Pending {
            reason: format!(
                "ERR_PAUSED — the bridge lane is paused (setPaused); retrying until it resumes \
                 (exit_code={code}): {detail}"
            ),
        },
        EXIT_TVL_CAP => SubmitOutcome::Pending {
            reason: format!(
                "ERR_TVL_CAP — source-chain deposit capacity is full; waiting for withdrawals \
                 (exit_code={code}): {detail}"
            ),
        },
        EXIT_TOKEN_UNREGISTERED => SubmitOutcome::Pending {
            reason: format!(
                "ERR_TOKEN_UNREGISTERED — this token is not registered for the source chain; \
                 retrying until the owner calls setBridgeToken (exit_code={code}): {detail}"
            ),
        },
        EXIT_TOKEN_DEPOSITS_DISABLED => SubmitOutcome::Pending {
            reason: format!(
                "ERR_TOKEN_DEPOSITS_DISABLED — deposits of this token are closed on AN; retrying \
                 until setBridgeToken reopens them (exit_code={code}): {detail}"
            ),
        },
        400 | 402 => SubmitOutcome::Pending {
            reason: format!(
                "external message already seen or expired; observe the voucher before retrying \
                 (exit_code={code}): {detail}"
            ),
        },
        401 => rejected(
            code,
            "external message expiry exceeds the five-minute window",
            detail,
        ),
        code => rejected(code, context, detail),
    }
}

fn classify_reverted(exit_code: Option<i32>, detail: &str) -> SubmitOutcome {
    match exit_code {
        Some(code) => classify_exit(code, "finalizeDeposit reverted", detail),
        None => SubmitOutcome::Rejected {
            reason: format!("finalizeDeposit tx status = {detail}"),
        },
    }
}

fn classify_call_error(msg: &str) -> SubmitOutcome {
    // Prefer acki_nacki_interface parser when the tvm-sdk feature is on; fall
    // back to a local regex-free scan so this module still builds without it.
    match parse_exit_code_loose(msg) {
        Some(code) => classify_exit(code, "finalizeDeposit failed", msg),
        None => SubmitOutcome::Rejected {
            reason: format!("finalizeDeposit call failed: {msg}"),
        },
    }
}

/// Verdict on a `finalizeDeposit` that exited 0, from the two transactions
/// it sets off: the `DepositVoucher` deploy, then the voucher's
/// `confirmDeposit` call back into the bridge, which is where the mint
/// happens. `None` while either is not yet visible.
async fn settle_finalize<C: IAckiNacki>(
    client: &C,
    root: &TxHash,
) -> Result<Option<SubmitOutcome>, AckiNackiError> {
    let children = client.get_internal_children(root).await?;
    let Some(voucher) = children.first() else {
        // Empty is also "transaction not indexed yet": GraphQL returns
        // `transaction: null` and `parse_internal_children` yields `[]`.
        // Rejecting here parked successful deposits after a laggy index.
        return Ok(None);
    };
    let Some(deploy) = &voucher.outcome else {
        return Ok(None);
    };
    if deploy.exit_code == Some(EXIT_CONSTRUCTOR_ALREADY_CALLED) {
        return Ok(Some(SubmitOutcome::AlreadyFinalized));
    }
    if !deploy.is_success() {
        return Ok(Some(SubmitOutcome::Rejected {
            reason: format!(
                "{} (voucher {}, exit_code={:?}, aborted={}); nothing was minted",
                describe_voucher_exit(deploy.exit_code),
                voucher.dst,
                deploy.exit_code,
                deploy.aborted,
            ),
        }));
    }

    let confirms = client.get_internal_children(&deploy.tx_hash).await?;
    let Some(confirm) = confirms.first() else {
        return Ok(None);
    };
    match &confirm.outcome {
        None => Ok(None),
        Some(c) if c.is_success() => Ok(Some(SubmitOutcome::Finalized {
            tx_hash: Some(*root),
        })),
        Some(c) => Ok(Some(SubmitOutcome::Rejected {
            reason: format!(
                "confirmDeposit failed (exit_code={:?}, aborted={}) after voucher {} was \
                 deployed; nothing was minted and the voucher now makes every resubmission report \
                 already-finalized, so this deposit needs the bridge owner",
                c.exit_code, c.aborted, voucher.dst,
            ),
        })),
    }
}

/// Interval between looks at the voucher and `confirmDeposit` transactions.
const SETTLE_POLL: Duration = Duration::from_secs(2);

fn parse_exit_code_loose(msg: &str) -> Option<i32> {
    if let Some(c) = acki_nacki_interface::parse_exit_code_from_message(msg) {
        return Some(c);
    }
    for marker in ["exit_code=Some(", "exit_code=", "local_exit_code="] {
        if let Some(rest) = msg.split(marker).nth(1) {
            let digits: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            if let Ok(code) = digits.parse::<i32>() {
                return Some(code);
            }
        }
    }
    None
}

fn accepted_block_hash_params(event: &DepositEvent) -> serde_json::Value {
    json!({
        "chainId": event.source_chain_id.to_string(),
        "blockHash": format!("{:#x}", event.block_hash),
    })
}

#[async_trait]
impl<C: IAckiNacki> AnSubmitter for AnInterfaceSubmitter<C> {
    async fn is_block_accepted(&self, event: &DepositEvent) -> Result<bool, RelayerError> {
        let output = self
            .client
            .run_getter(
                &self.config.token_bridge,
                "isAcceptedBlockHash",
                accepted_block_hash_params(event),
            )
            .await?;
        output
            .get("value0")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| RelayerError::other("isAcceptedBlockHash: expected a boolean value0"))
    }

    async fn is_finalized(&self, event: &DepositEvent) -> Result<bool, RelayerError> {
        let Some(check) = &self.voucher_check else {
            return Ok(false);
        };
        match self.voucher_active(check, event).await {
            Ok(active) => Ok(active),
            Err(e) => {
                warn!(
                    deposit_id = event.deposit_id,
                    error = %e,
                    "voucher pre-check failed; submitting the deposit anyway",
                );
                Ok(false)
            },
        }
    }

    async fn submit(
        &self,
        event: &DepositEvent,
        witness: &DepositWitness,
    ) -> Result<SubmitOutcome, RelayerError> {
        check_witness_matches(event, witness)?;
        self.submit_witness(witness).await
    }
}

#[cfg(test)]
mod tests {
    use acki_nacki_interface::{ChildOutcome, ChildTransaction};
    use alloy::primitives::{Address, B256};

    use super::*;
    use crate::{types::DepositEvent, witness::MockWitnessBuilder};

    #[test]
    fn anchor_getter_params_use_decimal_chain_and_ethereum_order_hex_hash() {
        let mut deposit = event(0);
        deposit.source_chain_id = 11_155_111;
        deposit.block_hash = "0x000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
            .parse()
            .unwrap();
        assert_eq!(
            accepted_block_hash_params(&deposit),
            json!({
                "chainId": "11155111",
                "blockHash": "0x000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            })
        );
    }

    #[tokio::test]
    async fn anchor_getter_requires_a_boolean_and_propagates_read_errors() {
        use acki_nacki_interface::MockAckiNacki;
        let client = Arc::new(MockAckiNacki::new());
        let sub = AnInterfaceSubmitter::new(client.clone(), AnSubmitConfig {
            from: "unused".into(),
            token_bridge: "unused".into(),
            confirm_timeout_secs: 1,
        });
        assert!(sub.is_block_accepted(&event(0)).await.is_err());
        for value in [false, true] {
            client.set_getter_response(json!({"value0": value}));
            assert_eq!(sub.is_block_accepted(&event(0)).await.unwrap(), value);
        }
        for output in [json!({}), json!({"value0": "false"}), json!({"value0": 1})] {
            client.set_getter_response(output);
            assert!(sub.is_block_accepted(&event(0)).await.is_err());
        }
        assert_eq!(client.transaction_count(), 0);
    }

    fn event(id: u64) -> DepositEvent {
        DepositEvent {
            deposit_id: id,
            token: Address::repeat_byte(0x7c),
            sender: Address::repeat_byte(0x11),
            amount: U256::from(42u64),
            an_account: B256::repeat_byte(0x77),
            timestamp: U256::ZERO,
            tx_hash: B256::repeat_byte(0xaa),
            log_index: 0,
            block_number: 1,
            block_hash: B256::repeat_byte(0xcd),
            source_contract: Address::repeat_byte(0x22),
            source_chain_id: 1,
        }
    }

    fn witness(ev: &DepositEvent) -> DepositWitness {
        MockWitnessBuilder::new().witness_for(ev)
    }

    #[tokio::test]
    async fn mock_finalizes_then_replay_is_already_finalized() {
        let sub = MockAnSubmitter::accepting();
        let ev = event(1);
        let b = witness(&ev);

        assert!(!sub.is_finalized(&ev).await.unwrap());
        assert!(matches!(
            sub.submit(&ev, &b).await.unwrap(),
            SubmitOutcome::Finalized { .. }
        ));
        assert!(sub.is_finalized(&ev).await.unwrap());
        assert!(matches!(
            sub.submit(&ev, &b).await.unwrap(),
            SubmitOutcome::AlreadyFinalized
        ));
        assert_eq!(sub.finalized_count(), 1);
    }

    #[tokio::test]
    async fn mock_rejects_when_verifier_says_no() {
        let sub = MockAnSubmitter::with_verifier(Arc::new(|_| false));
        let ev = event(2);
        let b = witness(&ev);
        match sub.submit(&ev, &b).await.unwrap() {
            SubmitOutcome::Rejected {
                reason,
            } => assert!(reason.contains("refused the receipt proof")),
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_oversized_witness_is_refused_before_anything_is_sent() {
        use acki_nacki_interface::MockAckiNacki;

        let client = Arc::new(MockAckiNacki::new());
        let sub = AnInterfaceSubmitter::new(client.clone(), AnSubmitConfig {
            from: "unused".into(),
            token_bridge: "unused".into(),
            confirm_timeout_secs: 1,
        });
        let ev = event(4);
        let big = MockWitnessBuilder::with_header_len(48 * 1024).witness_for(&ev);
        assert!(matches!(
            sub.submit(&ev, &big).await,
            Err(RelayerError::NeedsWallet(_))
        ));
        let too_large = MockWitnessBuilder::with_header_len(70_000).witness_for(&ev);
        assert!(matches!(
            sub.submit(&ev, &too_large).await,
            Err(RelayerError::Unprovable(_))
        ));
        assert_eq!(client.transaction_count(), 0);
    }

    const VOUCHER_TX: TxHash = [0x11; 32];
    const CONFIRM_TX: TxHash = [0x22; 32];

    fn child(dst: &str, outcome: Option<(TxHash, i32)>) -> ChildTransaction {
        ChildTransaction {
            message_id: format!("msg-to-{dst}"),
            dst: dst.to_string(),
            outcome: outcome.map(|(tx_hash, code)| ChildOutcome {
                tx_hash,
                exit_code: Some(code),
                aborted: code != 0,
            }),
        }
    }

    /// `finalizeDeposit` confirmed on a mock chain whose voucher deploy ends
    /// with `voucher` and, when that succeeds, whose `confirmDeposit` ends
    /// with `confirm` (`None` = not executed yet).
    async fn submit_with_chain(voucher: Option<i32>, confirm: Option<i32>) -> SubmitOutcome {
        use acki_nacki_interface::MockAckiNacki;

        let client = Arc::new(MockAckiNacki::new());
        client.set_children(MockAckiNacki::CALL_TX_HASH, vec![child(
            "0:voucher",
            voucher.map(|c| (VOUCHER_TX, c)),
        )]);
        client.set_children(VOUCHER_TX, vec![child(
            "0:bridge",
            confirm.map(|c| (CONFIRM_TX, c)),
        )]);
        let dapp = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let cfg = AnSubmitConfig {
            from: format!(
                "{dapp}::ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
            ),
            token_bridge: format!("{dapp}::{dapp}"),
            confirm_timeout_secs: 1,
        };
        let sub = AnInterfaceSubmitter::new(client, cfg);
        let ev = event(3);
        sub.submit(&ev, &witness(&ev)).await.unwrap()
    }

    #[tokio::test]
    async fn finalized_only_once_confirm_deposit_succeeds() {
        match submit_with_chain(Some(0), Some(0)).await {
            SubmitOutcome::Finalized {
                tx_hash,
            } => assert!(tx_hash.is_some()),
            other => panic!("expected Finalized, got {other:?}"),
        }
    }

    /// A duplicate `finalizeDeposit` exits 0 itself; only the voucher deploy
    /// it sends fails, with 51.
    #[tokio::test]
    async fn duplicate_is_read_from_the_voucher_deploy() {
        assert!(matches!(
            submit_with_chain(Some(51), None).await,
            SubmitOutcome::AlreadyFinalized
        ));
    }

    /// The 2026-07-02 outage: the bridge exited 0, the voucher constructor hit
    /// a cell underflow and nothing was minted.
    #[tokio::test]
    async fn failed_voucher_deploy_is_rejected_not_finalized() {
        match submit_with_chain(Some(9), None).await {
            SubmitOutcome::Rejected {
                reason,
            } => {
                assert!(reason.contains("_depositVoucherCode"), "{reason}");
                assert!(reason.contains("nothing was minted"), "{reason}");
            },
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn failed_confirm_deposit_is_rejected_not_finalized() {
        match submit_with_chain(Some(0), Some(214)).await {
            SubmitOutcome::Rejected {
                reason,
            } => assert!(reason.contains("confirmDeposit failed"), "{reason}"),
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unexecuted_children_leave_the_deposit_pending() {
        for (voucher, confirm) in [(None, None), (Some(0), None)] {
            match submit_with_chain(voucher, confirm).await {
                SubmitOutcome::Pending {
                    reason,
                } => assert!(reason.contains("not both visible"), "{reason}"),
                other => panic!("expected Pending for {voucher:?}/{confirm:?}, got {other:?}"),
            }
        }
    }

    /// GraphQL `transaction: null` parses as no children. That is lag, not
    /// a missing voucher — keep polling until the confirm timeout.
    #[tokio::test]
    async fn unindexed_root_leaves_the_deposit_pending() {
        use acki_nacki_interface::MockAckiNacki;

        let client = Arc::new(MockAckiNacki::new());
        client.set_children(MockAckiNacki::CALL_TX_HASH, vec![]);
        let dapp = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let cfg = AnSubmitConfig {
            from: format!(
                "{dapp}::ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
            ),
            token_bridge: format!("{dapp}::{dapp}"),
            confirm_timeout_secs: 1,
        };
        let sub = AnInterfaceSubmitter::new(client, cfg);
        let ev = event(3);
        match sub.submit(&ev, &witness(&ev)).await.unwrap() {
            SubmitOutcome::Pending {
                reason,
            } => {
                assert!(reason.contains("not both visible"), "{reason}");
            },
            other => panic!("expected Pending, got {other:?}"),
        }
    }

    #[test]
    fn paused_bridge_is_pending_not_rejected() {
        for outcome in [
            classify_call_error("contract call aborted (exit_code=231)"),
            classify_reverted(Some(231), "Reverted"),
        ] {
            match outcome {
                SubmitOutcome::Pending {
                    reason,
                } => assert!(reason.contains("ERR_PAUSED"), "{reason}"),
                other => panic!("expected Pending, got {other:?}"),
            }
        }
    }

    #[test]
    fn classify_exit_51_as_already_finalized() {
        assert!(matches!(
            classify_call_error("contract call aborted (exit_code=Some(51))"),
            SubmitOutcome::AlreadyFinalized
        ));
        assert!(matches!(
            classify_reverted(Some(51), "Reverted"),
            SubmitOutcome::AlreadyFinalized
        ));
    }

    #[test]
    fn receipt_proof_reverts_are_rejected_with_a_hint() {
        for (code, name) in [
            (244, "ERR_NOT_DEPOSIT_LOG"),
            (300, "ERR_SHORT"),
            (301, "ERR_RLP"),
            (302, "ERR_NODE_HASH"),
            (303, "ERR_PATH"),
            (304, "ERR_INLINE"),
            (305, "ERR_ABSENT"),
            (307, "ERR_NO_LOG"),
            (308, "ERR_RECEIPT_STATUS"),
        ] {
            for outcome in [
                classify_call_error(&format!("contract call aborted (exit_code={code})")),
                classify_reverted(Some(code), "Reverted"),
            ] {
                match outcome {
                    SubmitOutcome::Rejected {
                        reason,
                    } => {
                        assert!(reason.contains(name), "{code}: {reason}");
                        assert!(reason.contains(&format!("exit_code={code}")), "{reason}");
                        let rebuilt = reason.contains("builds a fresh one");
                        assert_eq!(
                            rebuilt,
                            (300..=305).contains(&code) && code != 304,
                            "{reason}"
                        );
                    },
                    other => panic!("expected Rejected for {code}, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn capacity_and_message_retries_do_not_park_a_deposit() {
        for code in [235, 241, 243, 400, 402] {
            for result in [
                classify_reverted(Some(code), "Reverted"),
                classify_call_error(&format!("exit_code={code}")),
            ] {
                assert!(
                    matches!(result, SubmitOutcome::Pending { .. }),
                    "{code}: {result:?}"
                );
            }
        }
        assert!(matches!(
            classify_reverted(Some(234), "Reverted"),
            SubmitOutcome::Rejected { .. }
        ));
    }

    /// The fail-closed config reverts must name the call that fixes them — an
    /// operator seeing a bare `exit_code=224` after a redeploy has no way to
    /// tell an unconfigured bridge from a rejected deposit.
    #[test]
    fn config_reverts_name_the_setter_that_fixes_them() {
        for (code, err_name, setter) in [
            (222, "ERR_UNSUPPORTED_SRC_CHAIN", "setTrustedL1Bridge"),
            (224, "ERR_UNKNOWN_BLOCK", "setAcceptedBlockHash"),
        ] {
            for outcome in [
                classify_call_error(&format!("contract call aborted (exit_code={code})")),
                classify_reverted(Some(code), "Reverted"),
            ] {
                match outcome {
                    SubmitOutcome::Rejected {
                        reason,
                    } => {
                        assert!(reason.contains(err_name), "{code}: {reason}");
                        assert!(reason.contains(setter), "{code}: {reason}");
                        assert!(reason.contains(&code.to_string()), "{code}: {reason}");
                    },
                    other => panic!("expected Rejected for {code}, got {other:?}"),
                }
            }
        }
    }

    /// The deposit behind the shellnet voucher in `voucher::tests`.
    fn shellnet_deposit() -> DepositEvent {
        DepositEvent {
            deposit_id: 1,
            token: Address::repeat_byte(0x7c),
            source_contract: "0x32b9e87acaa1ad7d61a81f93dd9d525f64ff4f38"
                .parse()
                .unwrap(),
            source_chain_id: 11_155_111,
            ..event(1)
        }
    }

    fn voucher_submitter(
        client: Arc<acki_nacki_interface::MockAckiNacki>,
        with_code: bool,
    ) -> AnInterfaceSubmitter<acki_nacki_interface::MockAckiNacki> {
        let dapp = "1a".repeat(32);
        let sub = AnInterfaceSubmitter::new(client, AnSubmitConfig {
            from: format!("{dapp}::{}", "ff".repeat(32)),
            token_bridge: format!("{dapp}::{dapp}"),
            confirm_timeout_secs: 1,
        });
        if with_code {
            sub.with_voucher_code(include_str!("../tests/fixtures/deposit_voucher_code.b64").trim())
        } else {
            sub.with_voucher_check(Abi::Json("{}".to_string())).unwrap()
        }
    }

    #[tokio::test]
    async fn voucher_precheck_reads_the_voucher_account() {
        use acki_nacki_interface::{AccountInfo, MockAckiNacki};

        let voucher = "ffd5c4e7770b73e67ad8a44c828c0d2ee2e642cbbff53096ffd0c68ed0f9a80e";
        let ev = shellnet_deposit();
        let client = Arc::new(MockAckiNacki::new());
        let sub = voucher_submitter(client.clone(), true);
        assert!(!sub.is_finalized(&ev).await.unwrap(), "no voucher yet");

        let info = |active| AccountInfo {
            active,
            code_hash: None,
            data: None,
        };
        client.set_account(voucher, info(false));
        assert!(
            !sub.is_finalized(&ev).await.unwrap(),
            "a failed deploy leaves no active voucher"
        );
        client.set_account(voucher, info(true));
        assert!(sub.is_finalized(&ev).await.unwrap());
        let other = DepositEvent {
            source_chain_id: 1,
            ..shellnet_deposit()
        };
        assert!(
            !sub.is_finalized(&other).await.unwrap(),
            "same depositId on another chain is a different deposit"
        );
    }

    /// A pre-check that cannot run must not block the deposit.
    #[tokio::test]
    async fn voucher_precheck_failure_falls_back_to_proving() {
        let client = Arc::new(acki_nacki_interface::MockAckiNacki::new());
        let sub = voucher_submitter(client, false);
        assert!(!sub.is_finalized(&shellnet_deposit()).await.unwrap());
    }
}
