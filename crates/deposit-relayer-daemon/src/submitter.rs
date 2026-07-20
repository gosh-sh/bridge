//! [`AnSubmitter`] — abstraction over "how do we deliver the proof to Acki
//! Nacki and finalise the deposit?".
//!
//! The AN-side entry point is `USDCBridge.finalizeDeposit(bytes proof, bytes
//! publicInputs)` (acki-nacki branch `poseidon_dex`, deployed on shellnet
//! 2026-06-22; verified by code-hash match against
//! `contracts/0.79.3_compiled/exchange/USDCBridge.tvc`). The contract parses
//! every deposit field straight out of the `publicInputs` operand (11 × 32-byte
//! little-endian `Fr`), verifies the triple via
//! `gosh.zkhalo2VerifyWithVK(VK_BLOB, publicInputs, proof)` (opcode `0xC7
//! 0x4A`; the `VK_BLOB` is the deploy-time 11-input deposit VkBlob embedded in
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
};

use acki_nacki_interface::{ContractCallRequest, ExtendedAddress, IAckiNacki, TransactionStatus};
use alloy::primitives::U256;
use async_trait::async_trait;
use serde_json::json;

use crate::{
    error::RelayerError,
    types::{DepositEvent, DepositProofBundle, NUM_PUBLIC_INPUTS},
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
}

#[async_trait]
pub trait AnSubmitter: Send + Sync {
    /// Cheap nullifier pre-check: has this `deposit_id` already been
    /// finalised on AN? Used to skip the expensive proof-generation step on
    /// replays. A backend that can't query the nullifier returns `Ok(false)`
    /// (the submit path is still safe — AN rejects double-finalisation).
    async fn is_finalized(&self, deposit_id: u64) -> Result<bool, RelayerError>;

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
/// round-trip unit test (it asserts the eleven decoded scalars survive a
/// re-encode):
///
/// ```text
///   NUM_PUBLIC_INPUTS × 32-byte big-endian scalars (the proof's public inputs):
///     depositId, sender, amount, contractAddress,
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
/// bytes. dappId is `scalars[4..6]`; the AN account is `scalars[6..8]`.
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
    async fn is_finalized(&self, deposit_id: u64) -> Result<bool, RelayerError> {
        Ok(self
            .inner
            .lock()
            .expect("poisoned lock")
            .nullifiers
            .contains(&deposit_id))
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
/// `is_finalized` returns `Ok(false)` because the current [`IAckiNacki`]
/// surface has no contract-read primitive to query `usedDepositIds`. This is
/// safe: the AN-side nullifier rejects a duplicate `finalizeDeposit`, which
/// surfaces here as [`SubmitOutcome::Rejected`] and is handled idempotently
/// by the relayer. When the AN team ships a read API, override this to skip
/// re-proving replays.
pub struct AnInterfaceSubmitter<C: IAckiNacki> {
    client: Arc<C>,
    config: AnSubmitConfig,
}

impl<C: IAckiNacki> AnInterfaceSubmitter<C> {
    pub fn new(client: Arc<C>, config: AnSubmitConfig) -> Self {
        Self {
            client,
            config,
        }
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
            TransactionStatus::Confirmed | TransactionStatus::Included => {
                Ok(SubmitOutcome::Finalized {
                    tx_hash: Some(sent),
                })
            },
            TransactionStatus::Reverted | TransactionStatus::Failed => {
                Ok(classify_reverted(receipt.exit_code, &format!("{:?}", receipt.status)))
            },
            TransactionStatus::Pending => Ok(SubmitOutcome::Rejected {
                reason: "finalizeDeposit tx still pending after timeout".to_string(),
            }),
        }
    }
}

/// StdContractError: DepositVoucher already deployed → deposit already finalized.
const EXIT_CONSTRUCTOR_ALREADY_CALLED: i32 = 51;
/// USDCBridge `ERR_INVALID_ZKPROOF`.
const EXIT_INVALID_ZKPROOF: i32 = 220;

fn classify_reverted(exit_code: Option<i32>, detail: &str) -> SubmitOutcome {
    match exit_code {
        Some(EXIT_CONSTRUCTOR_ALREADY_CALLED) => SubmitOutcome::AlreadyFinalized,
        Some(code) => SubmitOutcome::Rejected {
            reason: format!("finalizeDeposit reverted (exit_code={code}): {detail}"),
        },
        None => SubmitOutcome::Rejected {
            reason: format!("finalizeDeposit tx status = {detail}"),
        },
    }
}

fn classify_call_error(msg: &str) -> SubmitOutcome {
    // Prefer acki_nacki_interface parser when the tvm-sdk feature is on; fall
    // back to a local regex-free scan so this module still builds without it.
    let code = parse_exit_code_loose(msg);
    match code {
        Some(EXIT_CONSTRUCTOR_ALREADY_CALLED) => SubmitOutcome::AlreadyFinalized,
        Some(EXIT_INVALID_ZKPROOF) => SubmitOutcome::Rejected {
            reason: format!("ERR_INVALID_ZKPROOF (exit_code=220): {msg}"),
        },
        Some(code) => SubmitOutcome::Rejected {
            reason: format!("finalizeDeposit failed (exit_code={code}): {msg}"),
        },
        None => SubmitOutcome::Rejected {
            reason: format!("finalizeDeposit call failed: {msg}"),
        },
    }
}

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
    async fn is_finalized(&self, _deposit_id: u64) -> Result<bool, RelayerError> {
        // No nullifier read primitive on IAckiNacki yet — see struct docs.
        Ok(false)
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
                                                   // dappId is the config tag (scalars 4/5); the AN account high/low halves
                                                   // (scalars 6/7) reconstruct the proven recipient account.
        let dapp_id = (scalars[4] << 128) | scalars[5];
        assert_eq!(dapp_id, b.parsed.dapp_id());
        let reconstructed = (scalars[6] << 128) | scalars[7];
        assert_eq!(reconstructed, U256::from_be_slice(ev.an_account.as_slice()));
        assert_eq!(proof, b.proof.to_vec());
    }

    #[tokio::test]
    async fn mock_finalizes_then_replay_is_already_finalized() {
        let sub = MockAnSubmitter::accepting();
        let ev = event(1);
        let b = bundle(&ev);

        assert!(!sub.is_finalized(1).await.unwrap());
        assert!(matches!(
            sub.submit(&ev, &b).await.unwrap(),
            SubmitOutcome::Finalized { .. }
        ));
        assert!(sub.is_finalized(1).await.unwrap());
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
        // The operand must be the canonical 11 × 32-byte LE layout the opcode reads.
        assert_eq!(pi_bytes.len(), NUM_PUBLIC_INPUTS * 32);
        // And it must round-trip back to the same parsed inputs.
        assert_eq!(
            crate::types::DepositPublicInputs::from_operand(&pi_bytes).unwrap(),
            b.parsed
        );
    }

    #[tokio::test]
    async fn interface_submitter_sends_through_mock_client() {
        use acki_nacki_interface::MockAckiNacki;

        let client = Arc::new(MockAckiNacki::new());
        let dapp = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let cfg = AnSubmitConfig {
            from: format!(
                "{dapp}::ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
            ),
            token_bridge: format!("{dapp}::{dapp}"),
            confirm_timeout_secs: 5,
        };
        let sub = AnInterfaceSubmitter::new(client, cfg);
        let ev = event(3);
        let b = bundle(&ev);
        // MockAckiNacki confirms transactions, so this should finalize.
        match sub.submit(&ev, &b).await.unwrap() {
            SubmitOutcome::Finalized {
                tx_hash,
            } => assert!(tx_hash.is_some()),
            other => panic!("expected Finalized, got {other:?}"),
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
            SubmitOutcome::Rejected { reason } => {
                assert!(reason.contains("220") || reason.contains("ERR_INVALID_ZKPROOF"));
            },
            other => panic!("expected Rejected, got {other:?}"),
        }
    }
}
