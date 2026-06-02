//! Plain-data types shared between the relayer loop, [`crate::source`],
//! [`crate::prover`] and [`crate::submitter`].
//!
//! The EVM→AN deposit flow has three artefacts that move through the
//! pipeline:
//!
//! 1. A [`DepositEvent`] — the parsed on-chain `Deposit(depositId, sender,
//!    amount, anWorkchain, anAccount, timestamp)` log plus the
//!    receipt-locating metadata the prover needs (`tx_hash`, `log_index`,
//!    `block_number`). `anAccount` is the Acki Nacki destination account (an
//!    EVM address cannot be an AN recipient).
//! 2. A [`DepositProofBundle`] — the three operands the AN-side
//!    `ZKHALO2VERIFYWITHVK` opcode consumes (`vk_blob`, `public_inputs`,
//!    `proof`), produced by [`crate::prover::ProofGenerator`].
//! 3. The [`DepositPublicInputs`] — the eleven field elements the proof commits
//!    to (including the Acki Nacki destination account and the config-supplied
//!    `dappId` tag), decoded from the `public_inputs` operand so the submitter
//!    can build the `finalizeDeposit(...)` call arguments.

use alloy::primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};

use crate::error::RelayerError;

/// Number of public inputs the deposit circuit commits to. Matches
/// `deposit-prover`'s `num_instance() == vec![11]`:
/// `[depositId, sender, amount, contractAddress, dappIdHigh, dappIdLow,
/// anAccountHigh, anAccountLow, blockHashHigh, blockHashLow, promiseCommit]`.
///
/// `anAccount{High,Low}` bind the Acki Nacki destination account into the proof
/// (an EVM address is not a valid AN recipient). `dappId{High,Low}` (the UInt256
/// AN dApp identifier, replaced `anWorkchain` on 2026-06-02) is a config-supplied
/// tag — it is not bound to event data in-circuit; `TokenBridge.finalizeDeposit`
/// checks it against its configured dappId.
pub const NUM_PUBLIC_INPUTS: usize = 11;

/// Each public input is a 32-byte little-endian `Fr` (`Fr::to_repr()`).
pub const PUBLIC_INPUT_BYTES: usize = NUM_PUBLIC_INPUTS * 32;

/// A parsed `Deposit` event from `AckiNackiBridge.sol`, augmented with the
/// receipt-locating metadata the prover needs.
///
/// The `deposit_id` is the bridge's monotonic `depositCounter` value; the
/// relayer processes deposits in increasing `deposit_id` order, treating it
/// as the loop cursor (the on-chain `uint256 depositId` topic is a small
/// counter that always fits in a `u64`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DepositEvent {
    /// Monotonic deposit counter (event topic `depositId`). Loop cursor.
    pub deposit_id: u64,
    /// Depositor address (event topic `sender`).
    pub sender: Address,
    /// Deposited amount (event data word 0).
    pub amount: U256,
    /// Acki Nacki destination workchain id (TVM `int8`, event data word 1).
    /// Still emitted by the event; no longer bound in-circuit (the dappId tag
    /// took its public-input slot on 2026-06-02). Kept for logging / recipient
    /// reconstruction on the AN side.
    pub an_workchain: i8,
    /// Acki Nacki destination account (256-bit TVM address, event data word 2).
    ///
    /// The EVM `sender` is not a valid AN recipient (different address system),
    /// so the destination is chosen by the depositor and carried here. The
    /// deposit circuit binds it as the `anAccountHigh`/`anAccountLow` public
    /// inputs (2026-06-02), so the AN side credits a proven account.
    pub an_account: B256,
    /// Block timestamp recorded in the event (event data word 3).
    pub timestamp: U256,
    /// Hash of the transaction that emitted the event. Drives the prover's
    /// receipt + MPT witness fetch.
    pub tx_hash: B256,
    /// Index of the `Deposit` log within its transaction receipt.
    pub log_index: u64,
    /// Block number the event was mined in.
    pub block_number: u64,
    /// Block hash the event was mined in (informational / for logging).
    pub block_hash: B256,
    /// The bridge contract that emitted the event (the proof's
    /// `contractAddress` public input binds to this).
    pub source_contract: Address,
}

/// The eleven public inputs the deposit proof commits to, decoded from the
/// `public_inputs` opcode operand. Each is a full `U256` (the field element
/// re-interpreted as an integer); the submitter forwards these as the
/// `finalizeDeposit(...)` scalar arguments.
///
/// `dapp_id_high` / `dapp_id_low` are the high/low 16-byte halves of the
/// 256-bit AN dApp identifier (config-supplied tag); `an_account_high` /
/// `an_account_low` are the high/low halves of the 256-bit AN account, matching
/// the circuit's split. The AN side reconstructs each as `(high << 128) | low`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DepositPublicInputs {
    pub deposit_id: U256,
    pub sender: U256,
    pub amount: U256,
    pub contract_address: U256,
    pub dapp_id_high: U256,
    pub dapp_id_low: U256,
    pub an_account_high: U256,
    pub an_account_low: U256,
    pub block_hash_high: U256,
    pub block_hash_low: U256,
    pub promise_commit: U256,
}

impl DepositPublicInputs {
    /// Decode the inputs from the raw `public_inputs` operand: a
    /// concatenation of `NUM_PUBLIC_INPUTS` 32-byte **little-endian** field
    /// elements (the exact layout `deposit-prover`'s `export_blake2b_proof`
    /// writes, and the layout the opcode's `public_inputs_cell` carries).
    pub fn from_operand(bytes: &[u8]) -> Result<Self, RelayerError> {
        if bytes.len() != PUBLIC_INPUT_BYTES {
            return Err(RelayerError::ProofGeneration(format!(
                "public_inputs operand is {} bytes, expected {}",
                bytes.len(),
                PUBLIC_INPUT_BYTES
            )));
        }
        let field = |i: usize| -> U256 {
            let mut le = [0u8; 32];
            le.copy_from_slice(&bytes[i * 32..(i + 1) * 32]);
            U256::from_le_bytes(le)
        };
        Ok(Self {
            deposit_id: field(0),
            sender: field(1),
            amount: field(2),
            contract_address: field(3),
            dapp_id_high: field(4),
            dapp_id_low: field(5),
            an_account_high: field(6),
            an_account_low: field(7),
            block_hash_high: field(8),
            block_hash_low: field(9),
            promise_commit: field(10),
        })
    }

    /// Re-encode to the raw little-endian operand layout. Inverse of
    /// [`DepositPublicInputs::from_operand`]; used by the mock prover and
    /// round-trip tests.
    pub fn to_operand(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(PUBLIC_INPUT_BYTES);
        for fr in [
            self.deposit_id,
            self.sender,
            self.amount,
            self.contract_address,
            self.dapp_id_high,
            self.dapp_id_low,
            self.an_account_high,
            self.an_account_low,
            self.block_hash_high,
            self.block_hash_low,
            self.promise_commit,
        ] {
            out.extend_from_slice(&fr.to_le_bytes::<32>());
        }
        out
    }

    /// Reconstruct the full 256-bit Acki Nacki account from its high/low halves.
    pub fn an_account(&self) -> U256 {
        (self.an_account_high << 128) | self.an_account_low
    }

    /// Reconstruct the full 256-bit Acki Nacki dApp identifier from its halves.
    pub fn dapp_id(&self) -> U256 {
        (self.dapp_id_high << 128) | self.dapp_id_low
    }
}

/// The three operands the AN-side `ZKHALO2VERIFYWITHVK` opcode consumes,
/// plus the decoded public inputs for building the `finalizeDeposit` call.
///
/// Stack ABI (per `docs/zkhalo2verifywithvk_reference.md`):
///
/// ```text
/// bottom: vk_cell            ← `vk_blob`        (VkBlob v2 RLC for deposit)
/// middle: public_inputs_cell ← `public_inputs`  (11 × 32-byte LE Fr, no header)
/// top:    proof_cell         ← `proof`          (raw Blake2b SHPLONK bytes)
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DepositProofBundle {
    /// `VkBlob` (v2 RLC shape for the deposit circuit). Stable for a given
    /// deployment; cheap to cache across deposits.
    pub vk_blob: Bytes,
    /// `NUM_PUBLIC_INPUTS` × 32-byte little-endian field elements.
    pub public_inputs: Bytes,
    /// Raw Blake2b-transcript SHPLONK proof bytes.
    pub proof: Bytes,
    /// Decoded public inputs (parsed from `public_inputs`).
    pub parsed: DepositPublicInputs,
}

impl DepositProofBundle {
    /// Assemble a bundle from the three raw operands, decoding the public
    /// inputs and validating their length.
    pub fn from_operands(
        vk_blob: impl Into<Bytes>,
        public_inputs: impl Into<Bytes>,
        proof: impl Into<Bytes>,
    ) -> Result<Self, RelayerError> {
        let vk_blob = vk_blob.into();
        let public_inputs = public_inputs.into();
        let proof = proof.into();
        let parsed = DepositPublicInputs::from_operand(&public_inputs)?;
        Ok(Self {
            vk_blob,
            public_inputs,
            proof,
            parsed,
        })
    }

    /// Sanity-check that the proof's `depositId` public input matches the
    /// deposit we think we're finalizing. A mismatch means the prover was
    /// fed the wrong witness — a terminal bug, not a retryable condition.
    pub fn check_binds_to(&self, event: &DepositEvent) -> Result<(), RelayerError> {
        let expected = U256::from(event.deposit_id);
        if self.parsed.deposit_id != expected {
            return Err(RelayerError::ProofGeneration(format!(
                "proof depositId {:#x} != event depositId {}",
                self.parsed.deposit_id, event.deposit_id
            )));
        }
        let expected_sender = U256::from_be_bytes::<32>({
            let mut buf = [0u8; 32];
            buf[12..].copy_from_slice(event.sender.as_slice());
            buf
        });
        if self.parsed.sender != expected_sender {
            return Err(RelayerError::ProofGeneration(format!(
                "proof sender {:#x} != event sender {}",
                self.parsed.sender, event.sender
            )));
        }
        // The AN destination account is bound in-circuit; it must match the
        // event we read from chain. The 16-byte account halves are below the
        // field modulus so they're exact. dappId is a config tag (not in the
        // event), so it is not checked against the event here — the AN-side
        // TokenBridge verifies it against its configured dappId.
        let exp_acc_hi = U256::from_be_slice(&event.an_account.as_slice()[0..16]);
        let exp_acc_lo = U256::from_be_slice(&event.an_account.as_slice()[16..32]);
        if self.parsed.an_account_high != exp_acc_hi || self.parsed.an_account_low != exp_acc_lo {
            return Err(RelayerError::ProofGeneration(format!(
                "proof anAccount {:#x} != event anAccount {:#x}",
                self.parsed.an_account(),
                U256::from_be_slice(event.an_account.as_slice())
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_inputs_roundtrip() {
        let pi = DepositPublicInputs {
            deposit_id: U256::from(7u64),
            sender: U256::from(0x1234u64),
            amount: U256::from(1_000_000u64),
            contract_address: U256::from(0xabcdu64),
            dapp_id_high: U256::from(0xaaaa_bbbbu64),
            dapp_id_low: U256::from(0xcccc_ddddu64),
            an_account_high: U256::from(0x1111_2222u64),
            an_account_low: U256::from(0x3333_4444u64),
            block_hash_high: U256::from(0xdead_beefu64),
            block_hash_low: U256::from(0xcafe_babeu64),
            promise_commit: U256::from(0x99u64),
        };
        let operand = pi.to_operand();
        assert_eq!(operand.len(), PUBLIC_INPUT_BYTES);
        let back = DepositPublicInputs::from_operand(&operand).unwrap();
        assert_eq!(pi, back);
        // High/low halves reconstruct the full account and dappId.
        assert_eq!(
            pi.an_account(),
            (U256::from(0x1111_2222u64) << 128) | U256::from(0x3333_4444u64)
        );
        assert_eq!(
            pi.dapp_id(),
            (U256::from(0xaaaa_bbbbu64) << 128) | U256::from(0xcccc_ddddu64)
        );
    }

    #[test]
    fn from_operand_rejects_wrong_length() {
        let err = DepositPublicInputs::from_operand(&[0u8; 100]).unwrap_err();
        assert!(matches!(err, RelayerError::ProofGeneration(_)));
    }

    #[test]
    fn bundle_binding_check() {
        let pi = DepositPublicInputs {
            deposit_id: U256::from(3u64),
            sender: U256::from_be_bytes::<32>({
                let mut b = [0u8; 32];
                b[12..].copy_from_slice(Address::repeat_byte(0x11).as_slice());
                b
            }),
            amount: U256::from(5u64),
            contract_address: U256::ZERO,
            dapp_id_high: U256::from_be_slice(&[0x77u8; 16]),
            dapp_id_low: U256::from_be_slice(&[0x88u8; 16]),
            an_account_high: U256::from_be_slice(&[0x55u8; 16]),
            an_account_low: U256::from_be_slice(&[0x55u8; 16]),
            block_hash_high: U256::ZERO,
            block_hash_low: U256::ZERO,
            promise_commit: U256::ZERO,
        };
        let bundle = DepositProofBundle {
            vk_blob: Bytes::from(vec![1u8; 4]),
            public_inputs: Bytes::from(pi.to_operand()),
            proof: Bytes::from(vec![2u8; 8]),
            parsed: pi,
        };
        let event = DepositEvent {
            deposit_id: 3,
            sender: Address::repeat_byte(0x11),
            amount: U256::from(5u64),
            an_workchain: 0,
            an_account: B256::repeat_byte(0x55),
            timestamp: U256::ZERO,
            tx_hash: B256::ZERO,
            log_index: 0,
            block_number: 1,
            block_hash: B256::ZERO,
            source_contract: Address::ZERO,
        };
        bundle.check_binds_to(&event).unwrap();

        let mut wrong = event.clone();
        wrong.deposit_id = 4;
        assert!(bundle.check_binds_to(&wrong).is_err());

        // A proof bound to a different AN account must be rejected.
        let mut wrong_acct = event.clone();
        wrong_acct.an_account = B256::repeat_byte(0x66);
        assert!(bundle.check_binds_to(&wrong_acct).is_err());
    }
}
