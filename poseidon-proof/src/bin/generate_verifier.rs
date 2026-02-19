//! Binary to generate a Keccak256-based Solidity verifier from the Poseidon
//! preimage circuit.
//!
//! This generates the base verifier using snark-verifier-sdk's EvmLoader.
//! The output is a Solidity contract with Keccak256 transcript that will later
//! be modified to use Blake2b transcript (EIP-152).
//!
//! Usage:
//!   cd poseidon-proof
//!   cargo run --bin generate-verifier

use std::{fs, path::Path};

use halo2_base::{
    gates::{
        circuit::{builder::RangeCircuitBuilder, CircuitBuilderStage},
        RangeChip,
    },
    halo2_proofs::{
        halo2curves::bn256::Fr,
        plonk::{keygen_vk, Circuit, Selector},
    },
    utils::fs::gen_srs,
};
use poseidon_proof::circuit::PoseidonPreimageCircuit;
use snark_verifier_sdk::CircuitExt;

const K: u32 = 12;
const UNUSABLE_ROWS: usize = 9;

/// Wrapper around RangeCircuitBuilder that implements CircuitExt.
/// This is needed because gen_evm_verifier_shplonk requires CircuitExt.
struct PoseidonCircuitWrapper(RangeCircuitBuilder<Fr>);

impl Circuit<Fr> for PoseidonCircuitWrapper {
    type Config = <RangeCircuitBuilder<Fr> as Circuit<Fr>>::Config;
    type FloorPlanner = <RangeCircuitBuilder<Fr> as Circuit<Fr>>::FloorPlanner;
    type Params = <RangeCircuitBuilder<Fr> as Circuit<Fr>>::Params;

    fn without_witnesses(&self) -> Self {
        PoseidonCircuitWrapper(self.0.without_witnesses())
    }

    fn params(&self) -> Self::Params {
        self.0.params()
    }

    fn configure_with_params(
        meta: &mut halo2_base::halo2_proofs::plonk::ConstraintSystem<Fr>,
        params: Self::Params,
    ) -> Self::Config {
        RangeCircuitBuilder::<Fr>::configure_with_params(meta, params)
    }

    fn configure(meta: &mut halo2_base::halo2_proofs::plonk::ConstraintSystem<Fr>) -> Self::Config {
        RangeCircuitBuilder::<Fr>::configure(meta)
    }

    fn synthesize(
        &self,
        config: Self::Config,
        layouter: impl halo2_base::halo2_proofs::circuit::Layouter<Fr>,
    ) -> Result<(), halo2_base::halo2_proofs::plonk::Error> {
        self.0.synthesize(config, layouter)
    }
}

impl CircuitExt<Fr> for PoseidonCircuitWrapper {
    fn num_instance(&self) -> Vec<usize> {
        vec![1] // Single public input: Poseidon hash
    }

    fn instances(&self) -> Vec<Vec<Fr>> {
        vec![vec![Fr::zero()]] // Dummy instances for verifier generation
    }

    fn accumulator_indices() -> Option<Vec<(usize, usize)>> {
        None // No accumulator (not an aggregation circuit)
    }

    fn selectors(_config: &Self::Config) -> Vec<Selector> {
        vec![]
    }
}

fn main() {
    let lookup_bits = K as usize - 1;

    println!("=== Solidity Verifier Generator (Keccak256 base) ===");

    // --- Keygen phase (same as generate_proof.rs) ---
    println!("[1/3] Key generation...");
    let mut builder = RangeCircuitBuilder::from_stage(CircuitBuilderStage::Keygen)
        .use_k(K as usize)
        .use_instance_columns(1);
    builder.set_lookup_bits(lookup_bits);
    let range = RangeChip::new(lookup_bits, builder.lookup_manager().clone());

    let default_circuit = PoseidonPreimageCircuit::default_for_keygen(K, UNUSABLE_ROWS);
    let res = default_circuit.closure(builder.pool(0), &range);
    builder.assigned_instances[0] = res;

    let t_cells_lookup = builder
        .lookup_manager()
        .iter()
        .map(|lm| lm.total_rows())
        .sum::<usize>();
    let lookup_bits_ = if t_cells_lookup == 0 {
        None
    } else {
        Some(lookup_bits)
    };
    builder.config_params.lookup_bits = lookup_bits_;
    let config_params = builder.calculate_params(Some(UNUSABLE_ROWS));

    let params = gen_srs(K);
    let vk = keygen_vk(&params, &builder).unwrap();

    println!("  Config params: {:?}", config_params);
    println!("  k = {}", K);

    // --- Generate Solidity verifier ---
    println!("[2/3] Generating Solidity verifier...");

    let num_instance = vec![1]; // Single public input (Poseidon hash)
    let output_path = Path::new("data/Halo2Verifier_keccak_base.sol");

    fs::create_dir_all("data").unwrap();

    // We need to use gen_evm_verifier_sol_code directly since we have the VK
    // but gen_evm_verifier_shplonk needs CircuitExt type parameter
    let sol_code = snark_verifier_sdk::evm::gen_evm_verifier_sol_code::<
        PoseidonCircuitWrapper,
        snark_verifier_sdk::SHPLONK,
    >(&params, &vk, num_instance);

    fs::write(output_path, &sol_code).unwrap();

    println!("[3/3] Done!");
    println!("  Output: {}", output_path.display());
    println!("  Size: {} bytes", sol_code.len());
    println!("\nNext steps:");
    println!("  1. Review the generated Keccak256 verifier");
    println!("  2. Create Blake2bHalo2Verifier.sol by replacing keccak256 with Blake2b (EIP-152)");
}
