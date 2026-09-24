pub mod circuit;

use halo2_base::{
    gates::{
        circuit::{builder::BaseCircuitBuilder, BaseCircuitParams, BaseConfig},
        GateInstructions, RangeChip, RangeInstructions,
    },
    halo2_proofs::{
        halo2curves::bn256::Fr,
        halo2curves::group::ff::{Field, PrimeField},
        plonk::{Circuit, ConstraintSystem},
    },
    AssignedValue, QuantumCell,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

// Poseidon parameters and layer-hashes sizes are sourced from bridge_poseidon.
pub use bridge_poseidon::{
    LAYER_PREIMAGE_SIZE, MAX_LAYERS, POSEIDON_RATE, POSEIDON_R_F, POSEIDON_R_P, POSEIDON_T,
};

/// Number of SHA-256 Merkle siblings the circuit walks up the block-id tree
/// to open L0. The block-id tree is the canonical 16-leaf (depth-4) SHA-256
/// Merkle tree from the `poseidon_profile_new` branch of `acki-nacki`, so
/// opening L0 requires four opaque 32-byte siblings:
///
///   `sibling[0] = L1`                        (level 0: pair with L0)
///   `sibling[1] = sha_pair(L2, L3)`          (level 1)
///   `sibling[2] = subtree(L4..L7)`           (level 2)
///   `sibling[3] = subtree(L8..L15)`          (level 3)
///
/// Circuit 2 treats each sibling as an opaque 32-byte witness — the
/// semantics of L1..L15 are not verified here (event binding via L8 is a
/// Circuit 4 concern; L9..L15 are protocol-fixed zero padding).
pub const NUM_MERKLE_SIBLINGS: usize = 4;

// ---------------------------------------------------------------------------
// Circuit parameters
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct LayerHashesCircuitParams {
    pub k: usize,
    pub num_unusable_rows: usize,
    pub base_circuit_params: BaseCircuitParams,
}

// ---------------------------------------------------------------------------
// Circuit config
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct LayerHashesConfig {
    pub(crate) base_config: BaseConfig<Fr>,
}

impl LayerHashesConfig {
    pub fn configure_with_params(
        meta: &mut ConstraintSystem<Fr>,
        params: BaseCircuitParams,
    ) -> Self {
        let base_config =
            <BaseCircuitBuilder<Fr> as Circuit<Fr>>::configure_with_params(meta, params);
        LayerHashesConfig { base_config }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Decompose an Fr value into 32 LE bytes in-circuit, with range checks and
/// inner_product reconstruction constraint.
pub fn decompose_fr_to_bytes(
    ctx: &mut halo2_base::Context<Fr>,
    range: &RangeChip<Fr>,
    fr_val: AssignedValue<Fr>,
) -> Vec<AssignedValue<Fr>> {
    let native_bytes: [u8; 32] = fr_val.value().to_repr();
    let byte_cells: Vec<AssignedValue<Fr>> = native_bytes
        .iter()
        .map(|&b| ctx.load_witness(Fr::from(b as u64)))
        .collect();
    for &b in &byte_cells {
        range.range_check(ctx, b, 8);
    }
    let gate = range.gate();
    let reconstructed = gate.inner_product(
        ctx,
        byte_cells.iter().map(|&b| QuantumCell::Existing(b)),
        (0..32).map(|i| QuantumCell::Constant(Fr::from(256u64).pow([i as u64]))),
    );
    ctx.constrain_equal(&reconstructed, &fr_val);
    byte_cells
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

pub mod test_helpers {
    use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

    pub const K: u32 = 17;
    pub const NUM_UNUSABLE_ROWS: usize = 109;
    pub const LOOKUP_BITS: usize = 16;

    /// Convert up to 32 LE bytes to Fr using inner_product with powers of 256 (native).
    ///
    /// This matches the in-circuit `gate.inner_product(bytes, [256^0, 256^1, ...])`.
    pub fn bytes_le_to_fr(bytes: &[u8]) -> Fr {
        assert!(bytes.len() <= 32);
        let mut result = Fr::zero();
        let mut power = Fr::one();
        let base = Fr::from(256u64);
        for &byte in bytes {
            result += Fr::from(byte as u64) * power;
            power *= base;
        }
        result
    }
}
