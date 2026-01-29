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
use halo2_base::gates::circuit::CircuitBuilderStage;
use halo2_base::halo2_proofs::dev::MockProver;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use std::fs::File;

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

/// TODO: Implement full proof generation
///
/// This function will:
/// 1. Load or generate KZG parameters
/// 2. Create the circuit using `create_circuit`
/// 3. Generate proving/verifying keys
/// 4. Create SNARK proof using SHPLONK
/// 5. Return proof bytes and public inputs
///
/// For now, use `test_circuit_mock()` to test circuit logic.
pub fn generate_proof(
    _input: DepositProofInput,
    _config: &CircuitConfig,
) -> Result<DepositProofOutput, String> {
    Err("Full proof generation not yet implemented. Use test_circuit_mock() for testing."
        .to_string())
}

/// TODO: Implement proof verification
///
/// This function will verify a SNARK proof against the verifying key.
/// Returns true if the proof is valid, false otherwise.
pub fn verify_proof(
    _proof: &DepositProofOutput,
    _config: &CircuitConfig,
) -> Result<bool, String> {
    Err("Proof verification not yet implemented.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DepositEventData, ReceiptProof};

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

