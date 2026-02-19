//! Simple Poseidon preimage circuit.
//!
//! Proves knowledge of a secret `preimage` such that `Poseidon(preimage) =
//! hash`.
//! - Private witness: `preimage` (a single field element)
//! - Public input: `hash` (the Poseidon hash of the preimage)

use halo2_base::{
    gates::{
        circuit::builder::RangeCircuitBuilder, flex_gate::threads::SinglePhaseCoreManager,
        RangeChip, RangeInstructions,
    },
    halo2_proofs::halo2curves::bn256::Fr,
    poseidon::hasher::{spec::OptimizedPoseidonSpec, PoseidonHasher},
    AssignedValue,
};

use crate::poseidon::*;

/// A simple circuit that proves knowledge of a Poseidon preimage.
///
/// Given a secret `preimage`, the circuit constrains that `Poseidon(preimage) =
/// hash`, where `hash` is the single public input.
pub struct PoseidonPreimageCircuit {
    pub k: u32,
    pub unusable_rows: usize,
    pub lookup_bits: usize,
    /// The secret preimage (private witness)
    pub preimage: Fr,
}

impl PoseidonPreimageCircuit {
    /// Create a new circuit with default (zero) values for key generation.
    pub fn default_for_keygen(k: u32, unusable_rows: usize) -> Self {
        Self {
            k,
            unusable_rows,
            lookup_bits: k as usize - 1,
            preimage: Fr::zero(),
        }
    }

    /// Create a new circuit with a specific preimage.
    pub fn new(k: u32, unusable_rows: usize, preimage: Fr) -> Self {
        Self {
            k,
            unusable_rows,
            lookup_bits: k as usize - 1,
            preimage,
        }
    }

    /// Compute the expected public inputs (the hash).
    pub fn public_inputs(&self) -> Vec<Vec<Fr>> {
        let hash = poseidon_hash(&[self.preimage]);
        vec![vec![hash]]
    }

    /// The circuit closure: assigns witnesses and constrains Poseidon(preimage)
    /// = hash. Returns the public instance values (the hash).
    pub fn closure(
        &self,
        core: &mut SinglePhaseCoreManager<Fr>,
        range: &RangeChip<Fr>,
    ) -> Vec<AssignedValue<Fr>> {
        let ctx = core.main();

        // Assign the secret preimage as a witness
        let preimage_cell = ctx.assign_witnesses([self.preimage])[0];

        // Load the length (1 element)
        let len = ctx.load_witness(Fr::from(1u64));

        // Initialize Poseidon hasher with the same spec as gosh_dark_dex_halo2_circuit
        let spec = OptimizedPoseidonSpec::<Fr, T, RATE>::new::<R_F, R_P, 0>();
        let mut hasher = PoseidonHasher::<Fr, T, RATE>::new(spec);
        hasher.initialize_consts(ctx, range.gate());

        // Compute hash inside the circuit
        let hash_result = hasher.hash_var_len_array(ctx, range, &[preimage_cell], len);

        // The hash is the public output
        vec![hash_result]
    }

    /// Create a MockProver-compatible builder for testing.
    pub fn create_mock(&self) -> RangeCircuitBuilder<Fr> {
        let mut builder = RangeCircuitBuilder::default()
            .use_k(self.k as usize)
            .use_instance_columns(1);
        builder.set_lookup_bits(self.lookup_bits);
        let range = RangeChip::new(self.lookup_bits, builder.lookup_manager().clone());
        let instances = self.closure(builder.pool(0), &range);
        builder.assigned_instances[0] = instances;
        builder
    }
}
