//! Plain-data types shared between the relayer loop, [`crate::source`] and
//! [`crate::bridge`].
//!
//! These mirror the calldata layout of `AckiNackiBridge.verifyBlock`
//! exactly (see `contracts/ethereum/src/AckiNackiBridge.sol`), with
//! Solidity's `enum FinalizationType` represented as a Rust enum and
//! Solidity's `uint256[10] layerHashes` as a fixed-size `[U256; 10]`.

use alloy::primitives::{Bytes, U256};
use serde::{Deserialize, Serialize};

/// Maximum number of layer hashes per AN block (mirrors
/// `AckiNackiBridge.MAX_LAYER_HASHES`).
pub const MAX_LAYER_HASHES: usize = 10;

/// BN254 scalar field order — the modulus every circuit public input lives in
/// (mirrors `AckiNackiBridge.BN254_R`).
pub const BN254_FR_MODULUS: U256 = U256::from_limbs([
    0x43e1f593f0000001,
    0x2833e84879b97091,
    0xb85045b68181585d,
    0x30644e72e131a029,
]);

/// Map a raw 32-byte big-endian chain hash to the field element a circuit can
/// actually commit to.
///
/// The on-chain adapters (`PrimaryAggregatorVerifier` and friends) compare each
/// argument byte-for-byte against an instance read out of the proof, and those
/// instances are canonical `Fr` — so a raw hash `>= r` matches nothing and the
/// call fails before the pairing runs. Only `r / 2^256 = 18.9%` of hashes are
/// canonical as-is, so this is the common case, not the corner case. The
/// contract applies the same reduction to the SHA-256 root it folds in
/// `applyBkSetUpdate`, which keeps one meaning of `blockId` on both paths.
pub fn block_id_to_field(block_id_be: [u8; 32]) -> U256 {
    U256::from_be_bytes(block_id_be) % BN254_FR_MODULUS
}

/// Whether the AN block was finalised on the Primary or Fallback path.
///
/// Mirrors `AckiNackiBridge.FinalizationType`. The numeric encoding
/// (`Primary = 0`, `Fallback = 1`) is required to match the Solidity ABI
/// when the value is converted to `u8` during contract calls.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalizationType {
    /// Primary (Circuit 1A) — common-case ≥2/3 BK signatures.
    Primary,
    /// Fallback (Circuit 1B) — secondary path when the primary round
    /// fails to reach quorum within the deadline.
    Fallback,
}

impl FinalizationType {
    /// Numeric tag matching the Solidity enum encoding.
    pub fn tag(self) -> u8 {
        match self {
            FinalizationType::Primary => 0,
            FinalizationType::Fallback => 1,
        }
    }
}

impl From<bridge_prover_lib::live_driver::BundleFinalizationType> for FinalizationType {
    fn from(v: bridge_prover_lib::live_driver::BundleFinalizationType) -> Self {
        match v {
            bridge_prover_lib::live_driver::BundleFinalizationType::Primary => {
                FinalizationType::Primary
            }
            bridge_prover_lib::live_driver::BundleFinalizationType::Fallback => {
                FinalizationType::Fallback
            }
        }
    }
}

impl From<&bridge_prover_lib::live_driver::BundleProofArtifacts> for AnBlockData {
    fn from(b: &bridge_prover_lib::live_driver::BundleProofArtifacts) -> Self {
        // Schema v6: `block_id_be` is the raw 32-byte BE chain hash, reduced
        // into `Fr` here. The SHPLONK verifier does *not* get a chance to
        // auto-reduce it: the adapter compares the argument against the proof's
        // instance byte-for-byte first and returns false on any mismatch, so a
        // raw hash `>= r` is rejected before the pairing. The remaining `*_be`
        // fields
        // (`bk_set_commitment_be`, `layer_hashes_be[i]`,
        // `prev_max_level_layer_hash_be`) are still `Fr::to_repr()` LE
        // bytes — those wire fields have not yet been unified with the raw
        // hash convention (tracked as an open item alongside the schema v6
        // block_id fix).
        let mut layer_hashes = [U256::ZERO; MAX_LAYER_HASHES];
        for (i, h) in b
            .layer_hashes_be
            .iter()
            .enumerate()
            .take(b.num_layers as usize)
        {
            layer_hashes[i] = U256::from_le_bytes(*h);
        }
        AnBlockData {
            fin_type: b.fin_type.into(),
            block_id: block_id_to_field(b.block_id_be),
            bk_set_commitment: U256::from_le_bytes(b.bk_set_commitment_be),
            block_seq_no: b.block_seq_no,
            num_layers: b.num_layers,
            layer_hashes,
            prev_max_level_layer_hash: U256::from_le_bytes(b.prev_max_level_layer_hash_be),
            attestation_proof: Bytes::from(b.attestation_proof.clone()),
            layer_hashes_proof: Bytes::from(b.layer_hashes_proof.clone()),
        }
    }
}

impl From<&bridge_prover_lib::live_driver::BkUpdateProofArtifacts> for BkSetUpdateData {
    fn from(u: &bridge_prover_lib::live_driver::BkUpdateProofArtifacts) -> Self {
        // Schema v7: single `block_id_be` = raw 32-byte BE chain hash, reduced
        // into `Fr` for the same reason as the block path above — inside
        // `applyBkSetUpdate` this value is handed to the attestation adapter
        // *and* compared against the folded SHA-256 root, and the contract
        // reduces that root before comparing so both consumers agree.
        // Commitments remain
        // `Fr::to_repr()` LE bytes (open cleanup item), and go on-chain as the
        // numeric field element — the same convention as
        // `storedBkSetCommitment` and the attestation verifier's public
        // input. The contract re-derives the LE repr the block-id tree hashes
        // (`AckiNackiBridge._frToLeBytes`), so no byte-flip belongs here. The
        // three open siblings walk the depth-4 authentication path of L2/L3 in
        // the 16-leaf block-id tree: `h01` (depth 3), `h4_7` (depth 2), and
        // `h8_15` (depth 1).
        BkSetUpdateData {
            fin_type: u.fin_type.into(),
            block_id: block_id_to_field(u.block_id_be),
            block_seq_no: u.block_seq_no,
            old_commitment_l2: U256::from_le_bytes(u.old_bk_set_commitment_be),
            new_commitment_l3: U256::from_le_bytes(u.new_bk_set_commitment_be),
            sibling_h01: u.merkle_sibling_h01_be,
            sibling_h4_7: u.merkle_sibling_h4_7_be,
            sibling_h8_15: u.merkle_sibling_h8_15_be,
            attestation_proof: Bytes::from(u.attestation_proof.clone()),
        }
    }
}

/// All data the relayer needs to submit one block to
/// `AckiNackiBridge.verifyBlock`.
///
/// Both proofs are opaque byte strings passed to `verifyBlock`:
/// - Circuit 1A / 1B / 2: R15 SHPLONK aggregator calldata (`instances ‖
///   proof`). Circuit 1B is keygen'd at K=21 so its aggregated Yul fits
///   EIP-170.
///
/// The remaining fields are the cross-circuit-bound public inputs the contract
/// checks against its stored anchors.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnBlockData {
    pub fin_type: FinalizationType,
    /// Bound across both proofs: 32-byte AN block identifier.
    pub block_id: U256,
    /// Bound across both proofs: Poseidon commitment to the active BK set.
    pub bk_set_commitment: U256,
    pub block_seq_no: u64,
    /// Number of active layers for this block (1..=`MAX_LAYER_HASHES`).
    pub num_layers: u8,
    /// Per-layer Poseidon roots; index ≥ `num_layers` must be 0.
    pub layer_hashes: [U256; MAX_LAYER_HASHES],
    /// Anchors `prev_max_level_layer_hash` from the previously verified
    /// block (must equal the contract's `storedPrevMaxLevelLayerHash`).
    pub prev_max_level_layer_hash: U256,
    /// Attestation proof bytes (Circuit 1A or 1B SHPLONK calldata).
    pub attestation_proof: Bytes,
    /// Layer-hashes proof bytes (Circuit 2 SHPLONK calldata).
    pub layer_hashes_proof: Bytes,
}

/// Data for `AckiNackiBridge.applyBkSetUpdate` (BK-set rotation without Circuit
/// 3 ZK).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BkSetUpdateData {
    pub fin_type: FinalizationType,
    pub block_id: U256,
    pub block_seq_no: u64,
    pub old_commitment_l2: U256,
    pub new_commitment_l3: U256,
    /// Depth-3 sibling: hashes with `SHA(L2‖L3)` to form `h0_3`.
    pub sibling_h01: [u8; 32],
    /// Depth-2 sibling: hashes with `h0_3` to form `h0_7`.
    pub sibling_h4_7: [u8; 32],
    /// Depth-1 sibling: hashes with `h0_7` to form the root/`block_id`.
    pub sibling_h8_15: [u8; 32],
    pub attestation_proof: Bytes,
}

impl AnBlockData {
    /// Validates the structural invariants the contract enforces (cheap
    /// checks; runs before we pay gas).
    ///
    /// - `num_layers` is in `1..=MAX_LAYER_HASHES`.
    /// - Slots `[num_layers, MAX_LAYER_HASHES)` are zero.
    pub fn validate_shape(&self) -> Result<(), ShapeError> {
        if self.num_layers == 0 || self.num_layers as usize > MAX_LAYER_HASHES {
            return Err(ShapeError::NumLayers(self.num_layers));
        }
        for i in self.num_layers as usize..MAX_LAYER_HASHES {
            if !self.layer_hashes[i].is_zero() {
                return Err(ShapeError::TailNonZero(i));
            }
        }
        Ok(())
    }

    /// Returns the layer-hash that becomes the next block's anchor.
    /// Mirrors the contract's update rule
    /// `storedPrevMaxLevelLayerHash = layerHashes[numLayers - 1]`.
    pub fn next_anchor(&self) -> U256 {
        self.layer_hashes[(self.num_layers - 1) as usize]
    }
}

/// Cheap structural error reported by [`AnBlockData::validate_shape`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ShapeError {
    #[error("numLayers={0} out of range 1..=10")]
    NumLayers(u8),
    #[error("layerHashes[{0}] must be zero (tail past numLayers)")]
    TailNonZero(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modulus_constant_matches_bn254_r() {
        assert_eq!(
            BN254_FR_MODULUS,
            U256::from_str_radix(
                "21888242871839275222246405745257275088548364400416034343698204186575808495617",
                10
            )
            .unwrap()
        );
    }

    #[test]
    fn block_id_above_the_field_order_is_reduced() {
        // Real fold output from the 16-leaf block-id tree; like ~81% of
        // SHA-256 roots it does not fit in `Fr`, and sending it unreduced is
        // what makes the adapter reject the attestation before the pairing.
        let raw = [0xffu8; 32];
        let reduced = block_id_to_field(raw);

        assert!(U256::from_be_bytes(raw) >= BN254_FR_MODULUS);
        assert!(reduced < BN254_FR_MODULUS);
        assert_eq!(reduced, U256::from_be_bytes(raw) % BN254_FR_MODULUS);
    }

    #[test]
    fn canonical_block_id_passes_through_unchanged() {
        let mut raw = [0u8; 32];
        raw[0] = 0x01;
        assert_eq!(block_id_to_field(raw), U256::from_be_bytes(raw));
    }
}
