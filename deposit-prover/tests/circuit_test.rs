use deposit_prover::circuit::{CircuitConfig, DepositEventCircuit};
use deposit_prover::types::{DepositEventData, DepositProofInput, ReceiptProof};

#[test]
fn test_circuit_without_witnesses() {
    println!("🧪 Testing circuit without witnesses (key generation mode)...");
    
    let config = CircuitConfig::default();
    let circuit = DepositEventCircuit::without_witnesses(config);
    
    // This should create a circuit with no witnesses
    assert!(circuit.input.is_none());
    
    println!("✅ Circuit created successfully without witnesses");
}

#[test]
fn test_circuit_with_witnesses() {
    println!("🧪 Testing circuit with witnesses...");
    
    // Create test input data
    let event_data = DepositEventData {
        block_number: 12345,
        transaction_index: 0,
        log_index: 0,
        deposit_id: 42, // Unique deposit ID
        sender: [4u8; 20],
        amount: 1000000000000000000, // 1 ETH in wei
        timestamp: 1234567890,
        contract_address: [5u8; 20],
    };

    let receipt_proof = ReceiptProof {
        receipt_rlp: vec![],
        proof_nodes: vec![],
        receipt_root: [0u8; 32],
        block_header_rlp: vec![],
    };

    let input = DepositProofInput {
        event_data,
        receipt_proof,
    };
    
    let config = CircuitConfig::default();
    let circuit = DepositEventCircuit::new(input, config);
    
    // This should create a circuit with witnesses
    assert!(circuit.input.is_some());
    
    println!("✅ Circuit created successfully with witnesses");
}

#[test]
fn test_circuit_config() {
    println!("🧪 Testing circuit configuration...");
    
    let config = CircuitConfig::default();
    
    assert_eq!(config.k, 18);
    assert_eq!(config.max_receipt_len, 2048);
    assert_eq!(config.max_proof_depth, 10);
    assert_eq!(config.max_log_num, 20);
    assert_eq!(config.max_data_byte_len, 256);
    
    println!("✅ Circuit configuration is correct");
}

#[test]
fn test_bytes_to_field_conversion() {
    println!("🧪 Testing byte to field conversion...");
    
    // Test with all zeros
    let zeros = [0u8; 32];
    let field_val = DepositEventCircuit::bytes_to_field(&zeros);
    
    // Should be zero
    use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
    use halo2_base::utils::ScalarField;
    assert_eq!(field_val, Fr::zero());
    
    println!("✅ Byte to field conversion works correctly");
}

#[test]
fn test_address_to_field_conversion() {
    println!("🧪 Testing address to field conversion...");
    
    // Test with all zeros
    let zeros = [0u8; 20];
    let field_val = DepositEventCircuit::address_to_field(&zeros);
    
    // Should be zero
    use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
    use halo2_base::utils::ScalarField;
    assert_eq!(field_val, Fr::zero());
    
    println!("✅ Address to field conversion works correctly");
}

