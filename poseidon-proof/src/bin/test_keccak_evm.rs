//! Test that the Keccak verifier works with a Keccak proof using revm.
//! This confirms the base verifier is correct before testing Blake2b.
//!
//! Usage:
//!   cd poseidon-proof
//!   cargo run --bin test-keccak-evm

use halo2_base::{
    gates::{
        circuit::{builder::RangeCircuitBuilder, CircuitBuilderStage},
        RangeChip,
    },
    halo2_proofs::{
        halo2curves::bn256::Fr,
        plonk::{keygen_pk, keygen_vk, Circuit, Selector},
    },
    utils::fs::gen_srs,
};
use poseidon_proof::circuit::PoseidonPreimageCircuit;
use snark_verifier_sdk::{
    evm::{evm_verify, gen_evm_proof_shplonk, gen_evm_verifier_shplonk},
    CircuitExt,
};

const K: u32 = 12;
const UNUSABLE_ROWS: usize = 9;

/// Wrapper around RangeCircuitBuilder that implements CircuitExt.
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
        vec![1]
    }

    fn instances(&self) -> Vec<Vec<Fr>> {
        vec![vec![Fr::zero()]]
    }

    fn accumulator_indices() -> Option<Vec<(usize, usize)>> {
        None
    }

    fn selectors(_config: &Self::Config) -> Vec<Selector> {
        vec![]
    }
}

fn main() {
    let lookup_bits = K as usize - 1;

    println!("=== Keccak EVM Verifier Test ===");

    // --- Keygen phase ---
    println!("[1/4] Key generation...");
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
    let pk = keygen_pk(&params, vk.clone(), &builder).unwrap();
    let break_points = builder.break_points();
    drop(builder);

    // --- Generate Keccak EVM verifier bytecode ---
    println!("[2/4] Generating Keccak EVM verifier bytecode...");
    let deployment_code =
        gen_evm_verifier_shplonk::<PoseidonCircuitWrapper>(&params, pk.get_vk(), vec![1], None);
    println!("  Deployment code size: {} bytes", deployment_code.len());

    // --- Generate Keccak proof ---
    println!("[3/4] Generating Keccak proof...");
    let secret = Fr::from(42u64);
    let hash = poseidon_proof::poseidon::poseidon_hash(&[secret]);
    let pub_inputs: Vec<Fr> = vec![hash];

    let mut prover_builder =
        RangeCircuitBuilder::prover(config_params, break_points).use_instance_columns(1);
    let range2 = RangeChip::new(lookup_bits, prover_builder.lookup_manager().clone());
    let circuit = PoseidonPreimageCircuit::new(K, UNUSABLE_ROWS, secret);
    let instances = circuit.closure(prover_builder.pool(0), &range2);
    prover_builder.assigned_instances[0] = instances;

    let proof = gen_evm_proof_shplonk(&params, &pk, PoseidonCircuitWrapper(prover_builder), vec![
        pub_inputs.clone(),
    ]);
    println!("  Keccak proof size: {} bytes", proof.len());

    // Save Keccak calldata for comparison
    let calldata = snark_verifier_sdk::evm::encode_calldata(&[pub_inputs.clone()], &proof);
    std::fs::write("data/keccak_evm_calldata.bin", &calldata).unwrap();
    std::fs::write("data/keccak_evm_calldata.hex", hex::encode(&calldata)).unwrap();
    println!("  Keccak calldata size: {} bytes", calldata.len());
    println!("  Saved to data/keccak_evm_calldata.bin");

    // Save Keccak verifier bytecode for Foundry testing
    std::fs::write("data/keccak_verifier_deployment.bin", &deployment_code).unwrap();
    println!("  Saved Keccak verifier deployment code to data/keccak_verifier_deployment.bin");

    // --- Verify with revm ---
    println!("[4/4] Verifying with revm...");
    evm_verify(deployment_code, vec![pub_inputs], proof);
    println!("  ✓ Keccak proof verified successfully!");
}
