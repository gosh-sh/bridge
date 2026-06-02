//! Aggregation Circuit for Deposit Proofs
//!
//! This module implements proof aggregation to reduce the verifier size.
//! The aggregation circuit verifies a deposit proof and generates a smaller
//! verifier contract that fits under the 24KB Ethereum contract size limit.
//!
//! ## Architecture
//!
//! 1. **Deposit Circuit** (degree 15-18): Generates the main proof
//! 2. **Aggregation Circuit** (degree 20-22): Verifies the deposit proof
//! 3. **Aggregation Verifier** (~10-15KB): Small verifier that fits on-chain
//!
//! ## Usage
//!
//! ```ignore
//! // 1. Generate deposit proof (as usual)
//! let deposit_proof = generate_proof(input, &config)?;
//!
//! // 2. Aggregate the proof
//! let aggregated_proof = aggregate_proof(deposit_proof, &agg_config)?;
//!
//! // 3. Generate aggregation verifier (small, fits under 24KB)
//! generate_aggregation_verifier(&agg_config)?;
//! ```

use std::{fs, path::Path};

use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{halo2curves::bn256::G1Affine, plonk::ProvingKey},
};
use snark_verifier_sdk::{
    evm::gen_evm_verifier_shplonk,
    gen_pk,
    halo2::{
        aggregation::{AggregationCircuit, AggregationConfigParams, VerifierUniversality},
        gen_snark_shplonk,
    },
    Snark, SHPLONK,
};

use crate::{prover::load_kzg_params_from_trusted_setup, types::DepositProofOutput};

/// Configuration for the aggregation circuit
#[derive(Debug, Clone)]
pub struct AggregationConfig {
    /// Circuit degree for aggregation (typically 20-22)
    /// Larger than the deposit circuit to accommodate verification logic
    pub degree: u32,

    /// Lookup bits (default: 19)
    pub lookup_bits: usize,
}

impl Default for AggregationConfig {
    fn default() -> Self {
        Self {
            degree: 18,      // 2^18 = ~256K rows (use same as deposit circuit for now)
            lookup_bits: 17, // Standard lookup bits (k-1)
        }
    }
}

/// Aggregate a deposit proof into a smaller proof
///
/// This function takes a deposit proof and creates an aggregation proof
/// that verifies the deposit proof. The aggregation verifier will be
/// much smaller (~10-15KB) than the deposit verifier (~30KB).
///
/// # Arguments
///
/// * `deposit_proof` - The deposit proof to aggregate
/// * `config` - Aggregation circuit configuration
///
/// # Returns
///
/// Aggregated proof output
pub fn aggregate_proof(
    deposit_proof: &DepositProofOutput,
    config: &AggregationConfig,
) -> Result<DepositProofOutput, String> {
    let k = config.degree;

    println!("=== Aggregating Deposit Proof ===");
    println!("Aggregation circuit degree: {}", k);
    println!();

    // 1. Load KZG parameters for aggregation circuit
    println!("Loading KZG parameters for aggregation...");
    let params = load_kzg_params_from_trusted_setup(k)?;

    // 2. Deserialize the deposit proof SNARK
    println!("Deserializing deposit proof...");
    let deposit_snark: Snark = bincode::deserialize(&deposit_proof.proof)
        .map_err(|e| format!("Failed to deserialize deposit proof: {}", e))?;

    println!("Deposit proof instances: {:?}", deposit_snark.instances);
    println!();

    // 3. Create aggregation circuit
    println!("Creating aggregation circuit...");
    let agg_config_params = AggregationConfigParams {
        degree: k,
        lookup_bits: config.lookup_bits,
        ..Default::default()
    };

    let mut agg_circuit = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Keygen,
        agg_config_params,
        &params,
        vec![deposit_snark.clone()], // Aggregate single deposit proof
        VerifierUniversality::Full,
    );

    // Calculate circuit parameters
    println!("Calculating circuit parameters...");
    let agg_config = agg_circuit.calculate_params(Some(10));

    // Get break points AFTER calculate_params
    let break_points = agg_circuit.break_points();

    // 4. Generate proving key for aggregation circuit
    println!("Generating proving key for aggregation circuit...");
    let pk_path = format!("data/aggregation_k{}.pk", k);
    let pk = if Path::new(&pk_path).exists() {
        println!("Loading existing proving key from {}...", pk_path);
        let mut pk_file = fs::File::open(&pk_path)
            .map_err(|e| format!("Failed to open proving key file: {}", e))?;
        ProvingKey::<G1Affine>::read::<_, AggregationCircuit>(
            &mut pk_file,
            halo2_base::halo2_proofs::SerdeFormat::RawBytes,
            agg_config.clone(),
        )
        .map_err(|e| format!("Failed to read proving key: {}", e))?
    } else {
        println!("Generating new proving key (this may take a few minutes)...");
        let pk = gen_pk(&params, &agg_circuit, Some(Path::new(&pk_path)));
        println!("Proving key saved to {}", pk_path);
        pk
    };

    // 5. Create aggregation circuit in Prover mode
    println!("Creating aggregation circuit in Prover mode...");
    let agg_circuit_prover = AggregationCircuit::new::<SHPLONK>(
        CircuitBuilderStage::Prover,
        agg_config,
        &params,
        vec![deposit_snark],
        VerifierUniversality::Full,
    )
    .use_break_points(break_points);

    // 6. Generate aggregation proof
    println!("Generating aggregation proof...");
    let snark_path = format!("data/aggregation_proof_{}.snark", deposit_proof.deposit_id);
    let agg_snark = gen_snark_shplonk(&params, &pk, agg_circuit_prover, Some(&snark_path));

    println!("Aggregation proof generated successfully!");
    println!("Saved to: {}", snark_path);
    println!();

    // 7. Serialize aggregated proof
    let proof_bytes = bincode::serialize(&agg_snark)
        .map_err(|e| format!("Failed to serialize aggregation proof: {}", e))?;

    // 8. Return aggregated proof output
    // Note: The aggregated proof has different public instances (KZG accumulator)
    // but we keep the same deposit metadata for compatibility
    Ok(DepositProofOutput::new(
        proof_bytes,
        deposit_proof.deposit_id,
        deposit_proof.sender,
        deposit_proof.amount,
        deposit_proof.dapp_id,
        deposit_proof.an_account,
        deposit_proof.contract_address,
        deposit_proof.block_hash,
    ))
}

/// Generate Solidity verifier for aggregation proofs
///
/// This generates a much smaller verifier (~10-15KB) that can verify
/// aggregated proofs on-chain.
///
/// # Arguments
///
/// * `config` - Aggregation circuit configuration
/// * `output_path` - Path to save the Solidity verifier
///
/// # Returns
///
/// Size of the generated verifier in bytes
pub fn generate_aggregation_verifier(
    config: &AggregationConfig,
    output_path: &str,
) -> Result<usize, String> {
    let k = config.degree;

    println!("=== Generating Aggregation Verifier ===");
    println!("Circuit degree: {}", k);
    println!();

    // 1. Load KZG parameters
    println!("Loading KZG parameters...");
    let params = load_kzg_params_from_trusted_setup(k)?;

    // 2. Load proving key (must exist - run aggregate_proof first)
    let pk_path = format!("data/aggregation_k{}.pk", k);
    if !Path::new(&pk_path).exists() {
        return Err(format!(
            "Proving key not found at {}. Run aggregate_proof first to generate it.",
            pk_path
        ));
    }

    println!("Loading proving key from {}...", pk_path);

    // We need to create a dummy circuit to get the config params for reading the PK
    // This is a limitation of the current API
    let dummy_agg_config = AggregationConfigParams {
        degree: k,
        lookup_bits: config.lookup_bits,
        ..Default::default()
    };

    let mut pk_file =
        fs::File::open(&pk_path).map_err(|e| format!("Failed to open proving key: {}", e))?;
    let pk = ProvingKey::<G1Affine>::read::<_, AggregationCircuit>(
        &mut pk_file,
        halo2_base::halo2_proofs::SerdeFormat::RawBytes,
        dummy_agg_config,
    )
    .map_err(|e| format!("Failed to read proving key: {}", e))?;

    // 3. Get verifying key
    let vk = pk.get_vk();

    // 4. Determine number of instances
    // Aggregation circuit has accumulator instances
    let num_instance = vec![4]; // KZG accumulator has 4 limbs

    // 5. Generate Solidity verifier
    println!("Generating Solidity verifier...");
    let bytecode = gen_evm_verifier_shplonk::<AggregationCircuit>(
        &params,
        vk,
        num_instance,
        Some(Path::new(output_path)),
    );

    let verifier_size = bytecode.len();
    println!();
    println!("✅ Aggregation verifier generated!");
    println!("   Path: {}", output_path);
    println!(
        "   Size: {} bytes ({:.1} KB)",
        verifier_size,
        verifier_size as f64 / 1024.0
    );
    println!();

    if verifier_size > 24576 {
        println!("⚠️  WARNING: Verifier still exceeds 24KB limit!");
        println!("   Try increasing aggregation circuit degree or aggregating multiple proofs.");
    } else {
        println!("✅ Verifier fits under 24KB limit!");
    }

    // Save deployment bytecode
    let bytecode_path = output_path.replace(".sol", ".bytecode");
    fs::write(&bytecode_path, &bytecode).map_err(|e| format!("Failed to write bytecode: {}", e))?;
    println!("   Deployment bytecode saved to: {}", bytecode_path);

    let bytecode_hex_path = output_path.replace(".sol", ".bytecode.hex");
    fs::write(&bytecode_hex_path, hex::encode(&bytecode))
        .map_err(|e| format!("Failed to write hex bytecode: {}", e))?;
    println!("   Hex bytecode saved to: {}", bytecode_hex_path);

    Ok(verifier_size)
}
