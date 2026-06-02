//! [`AnSubmitter`] — abstraction over "how do we deliver the proof to Acki
//! Nacki and finalise the deposit?".
//!
//! The AN-side entry point is `TokenBridge.finalizeDeposit(depositId, sender,
//! amount, contractAddress, anWorkchain, anAccountHigh, anAccountLow,
//! blockHashHigh, blockHashLow, promiseCommit, proofCell)` (partner branch
//! `poseidon_dex_with_verify`). The contract builds the `public_inputs` cell
//! from the ten scalar arguments — which now *include* the Acki Nacki
//! destination (`anWorkchain`, `anAccountHigh`, `anAccountLow`) bound in the
//! proof — calls `ZKHALO2VERIFYWITHVK(vkBlob, publicInputsCell, proofCell)`
//! (opcode `0xC7 0x4A`), and on success reconstructs the recipient as
//! `anWorkchain:(anAccountHigh << 128 | anAccountLow)`, consumes the
//! `usedDepositIds[depositId]` nullifier, and credits that proven account. An
//! EVM address is not a valid AN recipient, so binding the destination in the
//! proof (rather than trusting an off-circuit relayer hint) is what makes the
//! credit trust-minimised.
//!
//! Two implementations:
//!
//! - [`MockAnSubmitter`] — an in-memory mirror of the nullifier semantics,
//!   used by the unit tests to drive the relayer through many deposits
//!   without a live AN node.
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

use acki_nacki_interface::{AckiNackiTransaction, IAckiNacki, TransactionStatus};
use alloy::primitives::U256;
use async_trait::async_trait;

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

/// Interim wire encoding of a `finalizeDeposit` call body.
///
/// **Not** the final TVM message ABI — the AN team's `tvm-sdk` client will
/// own the canonical encoder. Until then this produces a deterministic,
/// self-describing byte layout so the live-delivery plumbing
/// ([`AnInterfaceSubmitter`]) is fully wired and round-trip-testable:
///
/// ```text
///   NUM_PUBLIC_INPUTS × 32-byte big-endian scalars (the proof's public inputs):
///     depositId, sender, amount, contractAddress,
///     anWorkchain, anAccountHigh, anAccountLow,
///     blockHashHigh, blockHashLow, promiseCommit
///   u32 BE proof length
///   proof bytes (raw Blake2b SHPLONK)
/// ```
///
/// The AN destination (`anWorkchain`, `anAccountHigh`, `anAccountLow`) is now
/// part of the proof's public inputs — the deposit circuit binds it (see
/// `deposit-prover/src/circuit_v2.rs`), so there is no separate out-of-circuit
/// destination side-channel: the AN side reconstructs the recipient from these
/// proven scalars. The `vk_blob` is deploy-time configuration on the AN
/// contract and is therefore **not** part of the per-call body.
pub fn encode_finalize_deposit(bundle: &DepositProofBundle) -> Vec<u8> {
    let pi = &bundle.parsed;
    let mut out = Vec::with_capacity(NUM_PUBLIC_INPUTS * 32 + 4 + bundle.proof.len());
    for scalar in [
        pi.deposit_id,
        pi.sender,
        pi.amount,
        pi.contract_address,
        pi.an_workchain,
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
/// bytes. The AN destination is reconstructed from `scalars[4..7]`.
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
        Ok(SubmitOutcome::Finalized { tx_hash: None })
    }
}

// ─────────────────────────────────────────────────────────────────────
// AnInterfaceSubmitter — live delivery over IAckiNacki
// ─────────────────────────────────────────────────────────────────────

/// Static configuration for the live submitter.
#[derive(Clone, Debug)]
pub struct AnSubmitConfig {
    /// Relayer's AN account address (the `from` of the finalize tx).
    pub from: String,
    /// The AN `TokenBridge` contract address (the `to`).
    pub token_bridge: String,
    /// Gas limit for the `finalizeDeposit` call.
    pub gas_limit: u64,
    /// Seconds to wait for the finalize tx to confirm.
    pub confirm_timeout_secs: u64,
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
    nonce: Mutex<u64>,
}

impl<C: IAckiNacki> AnInterfaceSubmitter<C> {
    pub fn new(client: Arc<C>, config: AnSubmitConfig, start_nonce: u64) -> Self {
        Self {
            client,
            config,
            nonce: Mutex::new(start_nonce),
        }
    }

    fn next_nonce(&self) -> u64 {
        let mut n = self.nonce.lock().expect("poisoned lock");
        let cur = *n;
        *n = n.saturating_add(1);
        cur
    }
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

        let data = encode_finalize_deposit(bundle);
        // Derive a deterministic local tx id from the deposit id; the live
        // client overwrites this with the real hash on send.
        let mut tx_hash = [0u8; 32];
        tx_hash[24..].copy_from_slice(&event.deposit_id.to_be_bytes());

        let tx = AckiNackiTransaction::new(
            tx_hash,
            self.config.from.clone(),
            self.config.token_bridge.clone(),
            data,
            self.config.gas_limit,
            self.next_nonce(),
        );

        let sent = self.client.send_transaction(tx).await?;
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
                Ok(SubmitOutcome::Rejected {
                    reason: format!("finalizeDeposit tx status = {:?}", receipt.status),
                })
            },
            TransactionStatus::Pending => Ok(SubmitOutcome::Rejected {
                reason: "finalizeDeposit tx still pending after timeout".to_string(),
            }),
        }
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
        let pi = MockProofGenerator::derive_public_inputs(ev);
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
                                                   // AN destination is now bound in the public inputs: workchain (scalar 4)
                                                   // and the account high/low halves (scalars 5/6) reconstruct the account.
        assert_eq!(scalars[4], U256::from(ev.an_workchain.max(0) as u64));
        let reconstructed = (scalars[5] << 128) | scalars[6];
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
            SubmitOutcome::Rejected { reason } => assert!(reason.contains("ZKHALO2VERIFYWITHVK")),
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn interface_submitter_sends_through_mock_client() {
        use acki_nacki_interface::MockAckiNacki;

        let client = Arc::new(MockAckiNacki::new());
        let cfg = AnSubmitConfig {
            from: "0:relayer".to_string(),
            token_bridge: "0:tokenbridge".to_string(),
            gas_limit: 1_000_000,
            confirm_timeout_secs: 5,
        };
        let sub = AnInterfaceSubmitter::new(client, cfg, 0);
        let ev = event(3);
        let b = bundle(&ev);
        // MockAckiNacki confirms transactions, so this should finalize.
        match sub.submit(&ev, &b).await.unwrap() {
            SubmitOutcome::Finalized { tx_hash } => assert!(tx_hash.is_some()),
            other => panic!("expected Finalized, got {other:?}"),
        }
    }
}
