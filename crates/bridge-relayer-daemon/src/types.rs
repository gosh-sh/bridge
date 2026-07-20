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
            },
            bridge_prover_lib::live_driver::BundleFinalizationType::Fallback => {
                FinalizationType::Fallback
            },
        }
    }
}

impl From<&bridge_prover_lib::live_driver::BundleProofArtifacts> for AnBlockData {
    fn from(b: &bridge_prover_lib::live_driver::BundleProofArtifacts) -> Self {
        // `*_be` artefact fields are misnamed: they are `Fr::to_repr()` bytes
        // (little-endian), matching `fr_hex_to_u256` / on-chain genesis encoding.
        // `U256::from_be_bytes` here previously flipped every Fr PI and made
        // `daemon-live` fail the prev_max / bk_set anchor checks against Sepolia.
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
            block_id: U256::from_le_bytes(b.block_id_be),
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
        // `applyBkSetUpdate` verifies the SHA-256 Merkle open against the raw
        // 32-byte block hash (`block_id_hash_be`), not the Fr-reduced
        // attestation `block_id_be`. Commitments are `Fr::to_repr()` (LE).
        BkSetUpdateData {
            fin_type: u.fin_type.into(),
            block_id: U256::from_be_bytes(u.block_id_hash_be),
            block_seq_no: u.block_seq_no,
            old_commitment_l2: U256::from_le_bytes(u.old_bk_set_commitment_be),
            new_commitment_l3: U256::from_le_bytes(u.new_bk_set_commitment_be),
            sibling_h0: u.merkle_sibling_h0_be,
            sibling_h23: u.merkle_sibling_h23_be,
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
    pub sibling_h0: [u8; 32],
    pub sibling_h23: [u8; 32],
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
