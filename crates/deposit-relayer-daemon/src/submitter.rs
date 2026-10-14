//! [`AnSubmitter`] — abstraction over "how do we deliver the proof to Acki
//! Nacki and finalise the deposit?".
//!
//! The AN-side entry point is `USDCBridge.finalizeDeposit(bytes proof, bytes
//! publicInputs)` (acki-nacki branch `poseidon_dex`, deployed on shellnet
//! 2026-06-22; verified by code-hash match against
//! `contracts/0.79.3_compiled/exchange/USDCBridge.tvc`). The contract parses
//! every deposit field straight out of the `publicInputs` operand (12 × 32-byte
//! little-endian `Fr`), verifies the triple via
//! `gosh.zkhalo2VerifyWithVK(VK_BLOB, publicInputs, proof)` (opcode `0xC7
//! 0x4A`; the `VK_BLOB` is the deploy-time 12-input deposit VkBlob embedded in
//! the contract, **not** a per-call argument), reconstructs the recipient
//! account as `(anAccountHigh << 128 | anAccountLow)` from the proven inputs,
//! consumes a proof-bound deposit nullifier (deployed as a per-deposit
//! `DepositVoucher`), and credits that proven account. An EVM address is not a
//! valid AN recipient, so binding the destination account in the proof (rather
//! than trusting an off-circuit relayer hint) is what makes the credit
//! trust-minimised.
//!
//! Because the contract reads the operand directly, the relayer forwards only
//! the two raw byte blobs — there are no per-field scalar call arguments, and
//! the relayer cannot tamper with any proven field.
//!
//! Two implementations:
//!
//! - [`MockAnSubmitter`] — an in-memory mirror of the nullifier semantics, used
//!   by the unit tests to drive the relayer through many deposits without a
//!   live AN node.
//! - [`AnInterfaceSubmitter`] — wraps any [`acki_nacki_interface::IAckiNacki`]
//!   client: it ABI-encodes the `finalizeDeposit` call into an
//!   [`acki_nacki_interface::AckiNackiTransaction`] and sends it. Today only
//!   the mock `IAckiNacki` exists (the AN team will ship the live `tvm-sdk`
//!   client), so the live delivery path is exercised against the mock; the
//!   nullifier on the AN side guarantees a re-submission is rejected rather
//!   than double-crediting.

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
    types::{DepositEvent, DepositProofBundle, NUM_PUBLIC_INPUTS},
    voucher::{self, DepositIdentity},
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
    /// AN rejected the submission (proof verification failed, malformed
    /// call). The relayer logs and records an attempt.
    Rejected { reason: String },
    /// The tx was broadcast but confirmation timed out while still pending.
    /// Distinct from [`Rejected`] — the relayer retries without treating it
    /// as a verifier failure.
    Pending { reason: String },
}

#[async_trait]
pub trait AnSubmitter: Send + Sync {
    /// Cheap pre-check: has this deposit already been finalised on AN? Used
    /// to skip the expensive proof-generation step on replays. A backend that
    /// can't tell returns `Ok(false)` (the submit path is still safe — AN
    /// rejects double-finalisation).
    async fn is_finalized(&self, event: &DepositEvent) -> Result<bool, RelayerError>;

    /// Deliver the proof bundle and finalise the deposit.
    async fn submit(
        &self,
        event: &DepositEvent,
        bundle: &DepositProofBundle,
    ) -> Result<SubmitOutcome, RelayerError>;
}

// ─────────────────────────────────────────────────────────────────────
// finalizeDeposit call encoding
// ─────────────────────────────────────────────────────────────────────

/// Diagnostic (non-ABI) serialization of a proof bundle's public inputs +
/// proof.
///
/// **Not** the on-chain call ABI. The live `finalizeDeposit(bytes proof, bytes
/// publicInputs)` call is encoded by [`build_finalize_deposit_params`] + the
/// tvm_client ABI encoder. This function produces a deterministic,
/// self-describing big-endian layout used only by debug tooling and the
/// round-trip unit test (it asserts the twelve decoded scalars survive a
/// re-encode):
///
/// ```text
///   NUM_PUBLIC_INPUTS × 32-byte big-endian scalars (the proof's public inputs):
///     depositId, sender, amount, contractAddress, chainId,
///     dappIdHigh, dappIdLow, anAccountHigh, anAccountLow,
///     blockHashHigh, blockHashLow, promiseCommit
///   u32 BE proof length
///   proof bytes (raw Blake2b SHPLONK)
/// ```
pub fn encode_finalize_deposit(bundle: &DepositProofBundle) -> Vec<u8> {
    let pi = &bundle.parsed;
    let mut out = Vec::with_capacity(NUM_PUBLIC_INPUTS * 32 + 4 + bundle.proof.len());
    for scalar in [
        pi.deposit_id,
        pi.sender,
        pi.amount,
        pi.contract_address,
        pi.chain_id,
        pi.dapp_id_high,
        pi.dapp_id_low,
        pi.an_account_high,
        pi.an_account_low,
        pi.block_hash_high,
        pi.block_hash_low,
        pi.promise_commit,
    ] {
        out.extend_from_slice(&scalar.to_be_bytes::<32>());
    }
    out.extend_from_slice(&(bundle.proof.len() as u32).to_be_bytes());
    out.extend_from_slice(&bundle.proof);
    out
}

/// Header size of an [`encode_finalize_deposit`] body (before the proof bytes):
/// the public-input scalars + a u32 length.
const FINALIZE_HEADER_LEN: usize = NUM_PUBLIC_INPUTS * 32 + 4;

/// Decoded `finalizeDeposit` body: the public-input scalars and the raw proof
/// bytes. `chainId` is `scalars[4]`; dappId is `scalars[5..7]`; the AN account
/// is `scalars[7..9]`.
pub type DecodedFinalize = ([U256; NUM_PUBLIC_INPUTS], Vec<u8>);

/// Decode a body produced by [`encode_finalize_deposit`] back into the
/// public-input scalars and proof bytes. Used by round-trip tests (and any
/// future debug tooling).
pub fn decode_finalize_deposit(body: &[u8]) -> Result<DecodedFinalize, RelayerError> {
    if body.len() < FINALIZE_HEADER_LEN {
        return Err(RelayerError::other("finalizeDeposit body too short"));
    }
    let mut scalars = [U256::ZERO; NUM_PUBLIC_INPUTS];
    for (i, scalar) in scalars.iter_mut().enumerate() {
        let mut be = [0u8; 32];
        be.copy_from_slice(&body[i * 32..(i + 1) * 32]);
        *scalar = U256::from_be_bytes::<32>(be);
    }
    let mut off = NUM_PUBLIC_INPUTS * 32;
    let mut len_be = [0u8; 4];
    len_be.copy_from_slice(&body[off..off + 4]);
    off += 4;
    let proof_len = u32::from_be_bytes(len_be) as usize;
    if body.len() != off + proof_len {
        return Err(RelayerError::other(format!(
            "finalizeDeposit body length {} != header({off}) + proof({proof_len})",
            body.len()
        )));
    }
    Ok((scalars, body[off..].to_vec()))
}

// ─────────────────────────────────────────────────────────────────────
// MockAnSubmitter — in-memory nullifier mirror
// ─────────────────────────────────────────────────────────────────────

/// Verifier decision callback — returns `true` if the AN-side opcode would
/// accept the bundle. Tests inject `false` to drive the rejection path.
pub type VerifierDecision = Arc<dyn Fn(&DepositProofBundle) -> bool + Send + Sync>;

/// In-memory mirror of the AN `finalizeDeposit` nullifier semantics.
pub struct MockAnSubmitter {
    inner: Mutex<MockInner>,
    verifier: VerifierDecision,
}

struct MockInner {
    nullifiers: HashSet<u64>,
    finalized_log: Vec<u64>,
}

impl MockAnSubmitter {
    /// All bundles accepted (the common happy path).
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
        bundle: &DepositProofBundle,
    ) -> Result<SubmitOutcome, RelayerError> {
        // The proof must bind to the deposit we're finalising.
        bundle.check_binds_to(event)?;

        let mut inner = self.inner.lock().expect("poisoned lock");
        if inner.nullifiers.contains(&event.deposit_id) {
            return Ok(SubmitOutcome::AlreadyFinalized);
        }
        if !(self.verifier)(bundle) {
            return Ok(SubmitOutcome::Rejected {
                reason: "ZKHALO2VERIFYWITHVK rejected the deposit proof".to_string(),
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

/// Static configuration for the live submitter.
///
/// The deployed `finalizeDeposit(bytes proof, bytes publicInputs)` call carries
/// no token id and no caller-supplied gas (the contract is hardcoded to ECC[3]
/// USDC and pays its own gas after `tvm.accept()`), so neither is part of this
/// config.
#[derive(Clone, Debug)]
pub struct AnSubmitConfig {
    /// Relayer signer in SDK 3.0 `dapp_id::account_id` form.
    pub from: String,
    /// Bridge contract in SDK 3.0 `dapp_id::account_id` form.
    pub token_bridge: String,
    /// Seconds to wait for the finalize tx to confirm.
    pub confirm_timeout_secs: u64,
}

/// Build JSON parameters for `USDCBridge.finalizeDeposit(bytes proof, bytes
/// publicInputs)` from a proof bundle.
///
/// The deployed contract (acki-nacki `poseidon_dex`) parses every deposit field
/// out of the `publicInputs` operand itself and verifies the triple against its
/// embedded `VK_BLOB`, so the relayer forwards only the two raw byte blobs:
///
/// - `proof` — raw Blake2b-transcript SHPLONK bytes.
/// - `publicInputs` — `NUM_PUBLIC_INPUTS` × 32-byte little-endian `Fr`, exactly
///   the operand the AN-side opcode's `public_inputs_cell` carries.
///
/// Both are encoded as plain hex (no `0x`) for the tvm_client ABI JSON encoder.
/// The relayer cannot tamper with any proven field — the contract reads them
/// from the operand the opcode verifies.
pub fn build_finalize_deposit_params(bundle: &DepositProofBundle) -> serde_json::Value {
    json!({
        "proof": hex::encode(&bundle.proof),
        "publicInputs": hex::encode(&bundle.public_inputs),
    })
}

/// Live AN submitter. Encodes `finalizeDeposit` and sends it through an
/// [`IAckiNacki`] client.
///
/// With [`Self::with_voucher_check`], `is_finalized` looks for the deposit's
/// `DepositVoucher` on AN (see [`crate::voucher`]); without it, or when the
/// lookup fails, it returns `Ok(false)` and the deposit is proven and
/// submitted. That is safe: a duplicate `finalizeDeposit` still exits 0, but
/// its voucher deploy fails with exit 51, which [`settle_finalize`] reports as
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

    /// Submit an already-generated bundle directly, without binding it to a
    /// [`DepositEvent`]. The proof's public inputs are the sole authority (the
    /// AN-side opcode verifies them against the embedded VK), so this is the
    /// path used by the `finalize-one` operator command / contract self-test
    /// where the original Ethereum event is not re-fetched.
    pub async fn submit_bundle(
        &self,
        bundle: &DepositProofBundle,
    ) -> Result<SubmitOutcome, RelayerError> {
        let from = ExtendedAddress::parse(&self.config.from).map_err(RelayerError::from)?;
        let to = ExtendedAddress::parse(&self.config.token_bridge).map_err(RelayerError::from)?;
        let params = build_finalize_deposit_params(bundle);
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

// eccUSDCBridge `modifiers/errors.sol`.
const EXIT_ZERO_AMOUNT: i32 = 204;
const EXIT_INVALID_SENDER: i32 = 207;
const EXIT_HASH_MISMATCH: i32 = 219;
const EXIT_INVALID_ZKPROOF: i32 = 220;
const EXIT_UNSUPPORTED_SRC_CHAIN: i32 = 222;
const EXIT_UNKNOWN_BLOCK: i32 = 224;
const EXIT_ZERO_RECIPIENT: i32 = 230;
const EXIT_PAUSED: i32 = 231;

/// Names the `finalizeDeposit` revert and, where the cause is bridge
/// configuration rather than a bad deposit, says which call fixes it. The
/// fail-closed settings must be re-set by hand after every deploy or code
/// upgrade (they are not threaded through `onCodeUpgrade`), so without this the
/// first deposit after a redeploy reads as a broken bridge.
fn describe_exit_code(code: i32) -> Option<&'static str> {
    match code {
        EXIT_ZERO_AMOUNT => Some("ERR_ZERO_AMOUNT — the proven deposit amount is zero"),
        EXIT_INVALID_ZKPROOF => Some(
            "ERR_INVALID_ZKPROOF — proof rejected by the opcode; check the deployed VK_BLOB is \
             this prover's VkBlob",
        ),
        EXIT_UNSUPPORTED_SRC_CHAIN => Some(
            "ERR_UNSUPPORTED_SRC_CHAIN — the proven (chainId, source contract) pair is not \
             allowlisted; owner must call setTrustedL1Bridge(chainId, l1Bridge, true)",
        ),
        EXIT_UNKNOWN_BLOCK => Some(
            "ERR_UNKNOWN_BLOCK — the deposit's block is not in the canonical-chain anchor set; \
             confirm canonicality with `scripts/deposit_anchor_params.py --verify`, then the \
             anchor writer must admit it — setAcceptedBlockHash(chainId, blockHash, true) while \
             owner anchors are enabled, otherwise the light client",
        ),
        EXIT_ZERO_RECIPIENT => Some("ERR_ZERO_RECIPIENT — the proven AN recipient is zero"),
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
    let reason = match describe_exit_code(code) {
        Some(hint) => format!("{hint} (exit_code={code}): {detail}"),
        None => format!("{context} (exit_code={code}): {detail}"),
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

#[async_trait]
impl<C: IAckiNacki> AnSubmitter for AnInterfaceSubmitter<C> {
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
                    "voucher pre-check failed; proving the deposit anyway",
                );
                Ok(false)
            },
        }
    }

    async fn submit(
        &self,
        event: &DepositEvent,
        bundle: &DepositProofBundle,
    ) -> Result<SubmitOutcome, RelayerError> {
        bundle.check_binds_to(event)?;
        self.submit_bundle(bundle).await
    }
}

#[cfg(test)]
mod tests {
    use acki_nacki_interface::{ChildOutcome, ChildTransaction};
    use alloy::primitives::{Address, B256};

    use super::*;
    use crate::{prover::MockProofGenerator, types::DepositEvent};

    fn event(id: u64) -> DepositEvent {
        DepositEvent {
            deposit_id: id,
            sender: Address::repeat_byte(0x11),
            amount: U256::from(42u64),
            an_workchain: 0,
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

    fn bundle(ev: &DepositEvent) -> DepositProofBundle {
        let pi = MockProofGenerator::derive_public_inputs(ev, U256::from(0xD499u64));
        DepositProofBundle {
            vk_blob: vec![1, 2, 3].into(),
            public_inputs: pi.to_operand().into(),
            proof: vec![0xAB; 48].into(),
            parsed: pi,
        }
    }

    #[test]
    fn finalize_call_roundtrips() {
        let ev = event(7);
        let b = bundle(&ev);
        let body = encode_finalize_deposit(&b);
        let (scalars, proof) = decode_finalize_deposit(&body).unwrap();
        assert_eq!(scalars.len(), NUM_PUBLIC_INPUTS);
        assert_eq!(scalars[0], U256::from(7u64)); // depositId
        assert_eq!(scalars[2], U256::from(42u64)); // amount
        assert_eq!(scalars[4], b.parsed.chain_id); // proven chainId
                                                   // dappId is the config tag (scalars 5/6); the AN account high/low halves
                                                   // (scalars 7/8) reconstruct the proven recipient account.
        let dapp_id = (scalars[5] << 128) | scalars[6];
        assert_eq!(dapp_id, b.parsed.dapp_id());
        let reconstructed = (scalars[7] << 128) | scalars[8];
        assert_eq!(reconstructed, U256::from_be_slice(ev.an_account.as_slice()));
        assert_eq!(proof, b.proof.to_vec());
    }

    #[tokio::test]
    async fn mock_finalizes_then_replay_is_already_finalized() {
        let sub = MockAnSubmitter::accepting();
        let ev = event(1);
        let b = bundle(&ev);

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
        let b = bundle(&ev);
        match sub.submit(&ev, &b).await.unwrap() {
            SubmitOutcome::Rejected {
                reason,
            } => assert!(reason.contains("ZKHALO2VERIFYWITHVK")),
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn finalize_params_are_proof_and_public_inputs_hex() {
        let ev = event(1);
        let b = bundle(&ev);
        let params = build_finalize_deposit_params(&b);
        // Exactly the two `bytes` args the deployed contract expects.
        let obj = params.as_object().unwrap();
        assert_eq!(obj.len(), 2);

        let proof = params["proof"].as_str().unwrap();
        assert!(!proof.starts_with("0x"), "proof must be plain hex");
        assert_eq!(hex::decode(proof).unwrap(), b.proof.to_vec());

        let public_inputs = params["publicInputs"].as_str().unwrap();
        assert!(
            !public_inputs.starts_with("0x"),
            "publicInputs must be plain hex"
        );
        let pi_bytes = hex::decode(public_inputs).unwrap();
        assert_eq!(pi_bytes, b.public_inputs.to_vec());
        // The operand must be the canonical 12 × 32-byte LE layout the opcode reads.
        assert_eq!(pi_bytes.len(), NUM_PUBLIC_INPUTS * 32);
        // And it must round-trip back to the same parsed inputs.
        assert_eq!(
            crate::types::DepositPublicInputs::from_operand(&pi_bytes).unwrap(),
            b.parsed
        );
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
        sub.submit(&ev, &bundle(&ev)).await.unwrap()
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
        match sub.submit(&ev, &bundle(&ev)).await.unwrap() {
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
    fn classify_exit_220_as_rejected() {
        match classify_call_error("contract call aborted (exit_code=220)") {
            SubmitOutcome::Rejected {
                reason,
            } => {
                assert!(reason.contains("220") || reason.contains("ERR_INVALID_ZKPROOF"));
            },
            other => panic!("expected Rejected, got {other:?}"),
        }
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
