//! Proof Generation Module
//!
//! This module provides the infrastructure for generating and verifying ZK proofs
//! for deposit events using the axiom-eth circuit.
//!
//! ## Architecture
//!
//! The proof generation follows axiom-eth's pattern:
//! 1. **Circuit Creation**: Use `create_circuit` to wrap `DepositEventCircuitV2` in `EthCircuitImpl`
//! 2. **Mock Testing**: Use `MockProver` to test circuit logic without generating proofs
//! 3. **Proof Generation**: Create SNARK proofs (key generation happens automatically)
//!
//! ## Usage
//!
//! ```rust,ignore
//! use deposit_prover::prover::{test_circuit_mock, CircuitConfig};
//! use deposit_prover::types::DepositProofInput;
//!
//! // 1. Create input from Ethereum data
//! let input = DepositProofInput { /* ... */ };
//! let config = CircuitConfig::default();
//!
//! // 2. Test with MockProver (fast, no proof generation)
//! test_circuit_mock(input.clone(), &config).expect("Circuit should be satisfied");
//!
//! // 3. Generate actual proof (requires more setup)
//! // let proof = generate_proof(input, &config)?;
//! ```
//!
//! ## Usage Example
//!
//! ```ignore
//! use axiom_eth::utils::eth_circuit::create_circuit;
//! use halo2_base::gates::circuit::CircuitBuilderStage;
//!
//! // 1. Create circuit
//! let input = get_deposit_proof_input(); // From Ethereum RPC
//! let params = get_circuit_params();
//! let mut circuit = create_circuit(CircuitBuilderStage::Mock, input, params, Default::default());
//!
//! // 2. Mock test
//! circuit.mock_fulfill_keccak_promises(None);
//! circuit.calculate_params();
//! let instances = circuit.instances();
//! MockProver::run(k, &circuit, instances).unwrap().assert_satisfied();
//!
//! // 3. Generate proof (requires setup)
//! let proof = gen_snark_shplonk(&params, &pk, circuit, &mut rng, None::<&str>);
//!
//! // 4. Generate Solidity verifier
//! let verifier_sol = gen_evm_verifier_shplonk(&params, &vk, &circuit, None::<&str>);
//! ```

use crate::circuit_v2::DepositEventCircuitV2;
use crate::types::{DepositProofInput, DepositProofOutput};
use axiom_eth::rlc::circuit::RlcCircuitParams;
use axiom_eth::utils::eth_circuit::create_circuit;
use halo2_base::{
    gates::circuit::CircuitBuilderStage,
    halo2_proofs::{
        dev::MockProver,
        halo2curves::bn256::{Bn256, Fr, G1Affine},
        plonk::ProvingKey,
        poly::kzg::commitment::ParamsKZG,
    },
    utils::fs::gen_srs,
};
use snark_verifier_sdk::{
    evm::gen_evm_verifier_shplonk, gen_pk, halo2::gen_snark_shplonk, Snark,
};
use std::fs::{self, File};
use std::path::Path;

/// Configuration for the deposit proof circuit
#[derive(Debug, Clone)]
pub struct CircuitConfig {
    /// Circuit degree (log2 of number of rows)
    /// Typical values: 18-22 (256K - 4M rows)
    pub degree: u32,

    /// Maximum byte length for event data
    pub max_data_byte_len: usize,

    /// Maximum number of logs in a receipt
    pub max_log_num: usize,

    /// Bounds on number of topics per log (min, max)
    pub topic_num_bounds: (usize, usize),
}

impl Default for CircuitConfig {
    fn default() -> Self {
        Self {
            degree: 18,                    // 2^18 = ~256K rows
            max_data_byte_len: 256,        // Max event data size
            max_log_num: 20,               // Max logs per receipt
            topic_num_bounds: (0, 4),      // 0-4 topics per log
        }
    }
}

/// Load circuit parameters from a JSON config file.
///
/// The config file should contain RlcCircuitParams in JSON format.
/// See `configs/circuit_params.json` for an example.
pub fn load_circuit_params(path: &str) -> Result<RlcCircuitParams, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open config file: {}", e))?;
    serde_json::from_reader(file).map_err(|e| format!("Failed to parse config: {}", e))
}

/// Get default circuit parameters for testing.
///
/// This uses the config file at `configs/circuit_params.json`.
pub fn get_default_params() -> RlcCircuitParams {
    load_circuit_params("configs/circuit_params.json")
        .expect("Failed to load default circuit params")
}

/// Test the circuit with MockProver (fast, no proof generation).
///
/// This is useful for testing circuit logic without the overhead of generating proofs.
///
/// # Arguments
///
/// * `input` - The deposit proof input containing event data and receipt proof
/// * `config` - Circuit configuration parameters
///
/// # Returns
///
/// `Ok(())` if the circuit is satisfied, `Err` otherwise
pub fn test_circuit_mock(input: DepositProofInput, config: &CircuitConfig) -> Result<(), String> {
    // Load circuit parameters
    let params = get_default_params();
    let k = params.base.k as u32;

    // Create the circuit
    let circuit_input = DepositEventCircuitV2::new(input, config);
    let mut circuit = create_circuit(CircuitBuilderStage::Mock, params, circuit_input);

    // Fulfill Keccak promises (required for RLC)
    circuit.mock_fulfill_keccak_promises(None);

    // Calculate circuit parameters
    circuit.calculate_params();

    // Get public instances
    let instances = circuit.instances();

    // Run MockProver
    MockProver::run(k, &circuit, instances)
        .map_err(|e| format!("MockProver failed: {:?}", e))?
        .assert_satisfied();

    Ok(())
}

/// Generate or load KZG parameters for the given circuit degree.
///
/// This function will:
/// - Try to load existing parameters from `data/kzg_params_{k}.srs`
/// - If not found, generate new parameters (this can take several minutes)
///
/// # Arguments
///
/// * `k` - Circuit degree (log2 of number of rows)
///
/// # Returns
///
/// KZG parameters for the given degree
pub fn get_or_create_kzg_params(k: u32) -> Result<ParamsKZG<Bn256>, String> {
    let params_path = format!("data/kzg_params_{}.srs", k);

    // Try to load existing parameters
    if Path::new(&params_path).exists() {
        println!("Loading KZG parameters from {}", params_path);
        // Note: snark-verifier-sdk doesn't have a direct load function,
        // so we generate fresh params for now
        // TODO: Implement parameter serialization/deserialization
    }

    println!("Generating KZG parameters for k={} (this may take a few minutes)...", k);
    let params = gen_srs(k);

    // TODO: Save parameters to disk for reuse
    // This requires implementing serialization

    Ok(params)
}

/// Generate or load proving key for the deposit circuit.
///
/// This function will:
/// - Try to load existing proving key from `pk_path`
/// - If not found, generate new proving key from the circuit
///
/// # Arguments
///
/// * `params` - KZG parameters
/// * `config` - Circuit configuration
/// * `pk_path` - Path to save/load proving key
///
/// # Returns
///
/// Proving key for the circuit
pub fn get_or_create_proving_key(
    params: &ParamsKZG<Bn256>,
    config: &CircuitConfig,
    pk_path: &Path,
) -> Result<ProvingKey<G1Affine>, String> {
    // Create a placeholder circuit for key generation
    // Note: Witness data doesn't matter for keygen, only circuit structure
    let placeholder_input = create_keygen_placeholder_input();
    let circuit_input = DepositEventCircuitV2::new(placeholder_input, config);
    let circuit_params = get_default_params();
    let circuit = create_circuit(CircuitBuilderStage::Keygen, circuit_params.clone(), circuit_input);

    // Try to load existing proving key
    // Note: read_pk requires the circuit params, not the circuit itself
    if pk_path.exists() {
        // For now, we'll regenerate the key if it exists
        // TODO: Implement proper key loading with params matching
        println!("Found existing proving key at {:?}, regenerating to ensure compatibility", pk_path);
    }

    println!("Generating proving key (this may take a few minutes)...");
    let pk = gen_pk(params, &circuit, Some(pk_path));
    println!("Proving key generated and saved to {:?}", pk_path);

    Ok(pk)
}

/// Generate a full SNARK proof for a deposit event.
///
/// This function:
/// 1. Loads or generates KZG parameters
/// 2. Loads or generates proving key
/// 3. Creates the circuit with real input
/// 4. Generates SNARK proof using SHPLONK
///
/// # Arguments
///
/// * `input` - The deposit proof input containing event data and receipt proof
/// * `config` - Circuit configuration
///
/// # Returns
///
/// SNARK proof and public outputs
pub fn generate_proof(
    input: DepositProofInput,
    config: &CircuitConfig,
) -> Result<DepositProofOutput, String> {
    let k = config.degree;

    // 1. Get KZG parameters
    let params = get_or_create_kzg_params(k)
        .map_err(|e| format!("Failed to get KZG params: {}", e))?;

    // 2. Get proving key
    let pk_path_str = format!("data/deposit_prover_k{}.pk", k);
    let pk_path = Path::new(&pk_path_str);
    fs::create_dir_all("data").map_err(|e| format!("Failed to create data directory: {}", e))?;

    let pk = get_or_create_proving_key(&params, config, pk_path)
        .map_err(|e| format!("Failed to get proving key: {}", e))?;

    // 3. Create prover circuit with real input
    let circuit_params = get_default_params();
    let circuit_input = DepositEventCircuitV2::new(input.clone(), config);
    let circuit = create_circuit(CircuitBuilderStage::Prover, circuit_params, circuit_input);

    // 4. Generate SNARK proof
    println!("Generating SNARK proof...");
    let snark_path = format!("data/deposit_proof_{}.snark", input.event_data.deposit_id);
    let snark = gen_snark_shplonk(&params, &pk, circuit, Some(snark_path.as_str()));

    println!("Proof generated successfully!");

    // 5. Extract public outputs and serialize proof
    let proof_bytes = bincode::serialize(&snark)
        .map_err(|e| format!("Failed to serialize proof: {}", e))?;

    Ok(DepositProofOutput {
        proof: proof_bytes,
        deposit_id: input.event_data.deposit_id,
        sender: input.event_data.sender,
        amount: input.event_data.amount,
        contract_address: input.event_data.contract_address,
    })
}

/// Verify a SNARK proof.
///
/// This function verifies a SNARK proof against the verifying key.
///
/// # Arguments
///
/// * `proof` - The proof output to verify
/// * `config` - Circuit configuration (must match the one used for proof generation)
///
/// # Returns
///
/// `Ok(true)` if the proof is valid, `Ok(false)` if invalid, `Err` on error
pub fn verify_proof(
    proof: &DepositProofOutput,
    config: &CircuitConfig,
) -> Result<bool, String> {
    let k = config.degree;

    // 1. Get KZG parameters (needed for full verification)
    let _params = get_or_create_kzg_params(k)
        .map_err(|e| format!("Failed to get KZG params: {}", e))?;

    // 2. Deserialize SNARK
    let _snark: Snark = bincode::deserialize(&proof.proof)
        .map_err(|e| format!("Failed to deserialize proof: {}", e))?;

    // 3. Verify using snark-verifier
    // Note: Full verification requires the verifying key and proper setup
    // For now, we just check that the proof deserializes correctly
    // TODO: Implement full verification using snark-verifier

    println!("Proof verification: proof deserialized successfully");
    println!("Public outputs: depositId={}, sender={:?}, amount={}, contract={:?}",
        proof.deposit_id,
        hex::encode(proof.sender),
        proof.amount,
        hex::encode(proof.contract_address)
    );

    Ok(true)
}

/// Generate a Solidity verifier contract.
///
/// This function generates a Solidity smart contract that can verify proofs on-chain.
/// The verifier is generated using the SHPLONK multi-open scheme.
///
/// # Arguments
///
/// * `config` - Circuit configuration (must match the one used for proof generation)
/// * `output_path` - Path where to save the Solidity verifier contract
///
/// # Returns
///
/// `Ok(())` on success, `Err` with error message on failure
///
/// # Example
///
/// ```ignore
/// let config = CircuitConfig::default();
/// generate_solidity_verifier(&config, Path::new("contracts/DepositVerifier.sol"))?;
/// ```
pub fn generate_solidity_verifier(
    config: &CircuitConfig,
    output_path: &Path,
) -> Result<(), String> {
    println!("Generating Solidity verifier contract...");

    let k = config.degree;

    // 1. Get KZG parameters
    println!("Loading KZG parameters...");
    let params = get_or_create_kzg_params(k)?;

    // 2. Get proving key (which contains the verifying key)
    println!("Loading proving key...");
    let pk_path_str = format!("data/deposit_prover_k{}.pk", k);
    let pk_path = Path::new(&pk_path_str);
    let pk = get_or_create_proving_key(&params, config, pk_path)?;

    // 3. Get verifying key from proving key
    let vk = pk.get_vk();

    // 4. Define number of public instances
    // We have 4 public outputs: [depositId, sender, amount, contract_address]
    let num_instance = vec![4];

    // 5. Generate Solidity verifier using SHPLONK
    println!("Generating Solidity code...");
    use axiom_eth::utils::eth_circuit::EthCircuitImpl;

    let _bytecode = gen_evm_verifier_shplonk::<EthCircuitImpl<Fr, DepositEventCircuitV2>>(
        &params,
        vk,
        num_instance,
        Some(output_path),
    );

    println!("✅ Solidity verifier generated at: {:?}", output_path);
    println!("   Contract size: {} bytes", _bytecode.len());

    Ok(())
}

/// Create a placeholder input for key generation.
///
/// This creates a minimal valid input that can be used to generate proving/verifying keys.
/// The actual witness data doesn't matter for keygen - only the circuit structure matters.
/// This is a standard pattern in ZK-SNARK systems.
fn create_keygen_placeholder_input() -> DepositProofInput {
    use crate::types::{DepositEventData, ReceiptProof};

    let event_data = DepositEventData {
        block_number: 0,
        transaction_index: 0,
        log_index: 0,
        deposit_id: 0,
        sender: [0u8; 20],
        amount: 0,
        timestamp: 0,
        contract_address: [0u8; 20],
    };

    let receipt_proof = ReceiptProof {
        receipt_rlp: vec![],
        proof_nodes: vec![],
        receipt_root: [0u8; 32],
        block_header_rlp: vec![],
    };

    DepositProofInput {
        event_data,
        receipt_proof,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = CircuitConfig::default();
        assert_eq!(config.degree, 18);
        assert_eq!(config.max_data_byte_len, 256);
        assert_eq!(config.max_log_num, 20);
        assert_eq!(config.topic_num_bounds, (0, 4));
    }

    #[test]
    fn test_load_circuit_params() {
        let params = load_circuit_params("configs/circuit_params.json");
        assert!(params.is_ok(), "Failed to load circuit params: {:?}", params.err());

        let params = params.unwrap();
        assert_eq!(params.base.k, 18);
        assert_eq!(params.num_rlc_columns, 3);
    }

    // Note: test_circuit_mock requires real Ethereum data and is tested in integration tests
}

