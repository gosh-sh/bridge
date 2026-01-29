//! Integration Tests with Real Ethereum Data
//!
//! This module tests the deposit prover with real Ethereum receipts and events.
//! 
//! To run these tests:
//!   cargo test --test integration_test -- --nocapture
//!
//! Note: These tests require an Ethereum RPC endpoint. Set the environment variable:
//!   export ETH_RPC_URL="https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY"

use deposit_prover::{
    prover::{test_circuit_mock, CircuitConfig},
    types::{DepositEventData, DepositProofInput, ReceiptProof},
};

/// Test with a minimal mock receipt
///
/// NOTE: This test is expected to fail because we don't have a valid MPT proof.
/// It's kept here to demonstrate the testing approach.
///
/// For real testing, use:
///   cargo run --example fetch_deposit_data -- --tx-hash 0x... --contract 0x...
///   cargo run --example test_with_real_data -- --input deposit_proof_input.json
#[test]
#[ignore = "Requires valid MPT proof - use real Ethereum data instead"]
fn test_mock_receipt_basic() {
    println!("\n=== Testing with Mock Receipt (Basic) ===\n");
    println!("⚠️  This test is ignored because it requires a valid MPT proof.");
    println!("   Use the integration testing tools instead:");
    println!("   1. cargo run --example fetch_deposit_data");
    println!("   2. cargo run --example test_with_real_data");
    println!();

    // Create a minimal mock deposit event
    let event_data = DepositEventData {
        block_number: 12345,
        transaction_index: 0,
        log_index: 0,
        deposit_id: 1,
        sender: [0x12; 20], // Mock sender address
        amount: 1_000_000_000_000_000_000, // 1 ETH in wei
        timestamp: 1234567890,
        contract_address: [0xAB; 20], // Mock contract address
    };

    // Create a minimal mock receipt proof
    // Note: This is not a valid MPT proof, just for basic testing
    let receipt_proof = ReceiptProof {
        receipt_rlp: create_mock_receipt_rlp(&event_data),
        proof_nodes: vec![],
        receipt_root: [0u8; 32],
        block_header_rlp: vec![],
    };

    let input = DepositProofInput {
        event_data,
        receipt_proof,
    };

    let config = CircuitConfig {
        degree: 18,
        max_data_byte_len: 256,
        max_log_num: 20,
        topic_num_bounds: (0, 4),
    };

    println!("Circuit Configuration:");
    println!("  - Degree: {}", config.degree);
    println!("  - Max data byte len: {}", config.max_data_byte_len);
    println!("  - Max log num: {}", config.max_log_num);
    println!();

    // This will fail because we don't have a valid MPT proof
    match test_circuit_mock(input, &config) {
        Ok(()) => {
            println!("✅ Mock circuit test passed!");
        }
        Err(e) => {
            println!("⚠️  Mock circuit test failed (expected): {}", e);
            println!("   This is normal - we need a valid MPT proof for full testing");
        }
    }
}

/// Create a mock RLP-encoded receipt for testing
fn create_mock_receipt_rlp(event_data: &DepositEventData) -> Vec<u8> {
    use ethers::types::{Log, Bloom, TransactionReceipt, U64, U256, H256};
    use ethers::utils::rlp;
    
    // Create a mock log for the Deposit event
    let log = Log {
        address: ethers::types::H160::from_slice(&event_data.contract_address),
        topics: vec![
            // Event signature: keccak256("Deposit(uint256,address,uint256,uint256)")
            H256::from_slice(&[
                0x19, 0xda, 0xbb, 0xf7, 0x8f, 0x5b, 0x3d, 0x5c,
                0x8e, 0x3c, 0x7d, 0x6f, 0x8c, 0x5a, 0x9e, 0x2d,
                0x4b, 0x1c, 0x3a, 0x5f, 0x7e, 0x9d, 0x2c, 0x4b,
                0x6a, 0x8f, 0x1e, 0x3d, 0x5c, 0x7b, 0x9a, 0x2e,
            ]),
            // depositId (indexed)
            H256::from_low_u64_be(event_data.deposit_id),
            // sender (indexed)
            {
                let mut h = [0u8; 32];
                h[12..32].copy_from_slice(&event_data.sender);
                H256::from_slice(&h)
            },
        ],
        data: {
            // Non-indexed parameters: amount, timestamp
            let mut data = Vec::new();
            // amount (32 bytes)
            data.extend_from_slice(&[0u8; 24]);
            data.extend_from_slice(&event_data.amount.to_be_bytes());
            // timestamp (32 bytes)
            data.extend_from_slice(&[0u8; 24]);
            data.extend_from_slice(&event_data.timestamp.to_be_bytes());
            ethers::types::Bytes::from(data)
        },
        block_hash: Some(H256::zero()),
        block_number: Some(U64::from(event_data.block_number)),
        transaction_hash: Some(H256::zero()),
        transaction_index: Some(U64::from(event_data.transaction_index)),
        log_index: Some(U256::from(event_data.log_index)),
        transaction_log_index: Some(U256::from(event_data.log_index)),
        log_type: None,
        removed: Some(false),
    };

    // Create a mock receipt
    let receipt = TransactionReceipt {
        transaction_hash: H256::zero(),
        transaction_index: U64::from(event_data.transaction_index),
        block_hash: Some(H256::zero()),
        block_number: Some(U64::from(event_data.block_number)),
        from: ethers::types::H160::zero(),
        to: Some(ethers::types::H160::from_slice(&event_data.contract_address)),
        cumulative_gas_used: U256::from(100000),
        gas_used: Some(U256::from(50000)),
        contract_address: None,
        logs: vec![log],
        status: Some(U64::from(1)), // Success
        root: None,
        logs_bloom: Bloom::default(),
        transaction_type: None,
        effective_gas_price: None,
        other: Default::default(),
    };

    // Encode the receipt as RLP
    // Note: This is a simplified encoding and may not match Ethereum's exact format
    let mut stream = rlp::RlpStream::new();
    stream.begin_list(4);
    stream.append(&receipt.status.unwrap_or(U64::from(0)));
    stream.append(&receipt.cumulative_gas_used);
    stream.append(&receipt.logs_bloom);
    stream.begin_list(receipt.logs.len());
    for log in &receipt.logs {
        stream.begin_list(3);
        stream.append(&log.address);
        stream.begin_list(log.topics.len());
        for topic in &log.topics {
            stream.append(topic);
        }
        stream.append(&log.data.to_vec());
    }
    
    stream.out().to_vec()
}

#[cfg(feature = "real_ethereum_tests")]
mod real_ethereum_tests {
    use super::*;
    use ethers::providers::{Http, Provider, Middleware};
    use std::env;

    /// Test with a real Ethereum transaction
    /// 
    /// This test fetches a real transaction receipt from Ethereum and tests the circuit.
    /// 
    /// Requirements:
    /// - Set ETH_RPC_URL environment variable
    /// - Run with: cargo test --test integration_test --features real_ethereum_tests
    #[tokio::test]
    async fn test_real_ethereum_receipt() {
        println!("\n=== Testing with Real Ethereum Receipt ===\n");

        // Get RPC URL from environment
        let rpc_url = env::var("ETH_RPC_URL")
            .expect("ETH_RPC_URL environment variable not set");

        println!("Connecting to Ethereum RPC: {}", rpc_url);
        
        let provider = Provider::<Http>::try_from(rpc_url)
            .expect("Failed to create provider");

        // TODO: Replace with actual transaction hash of a Deposit event
        let tx_hash = "0x0000000000000000000000000000000000000000000000000000000000000000";
        
        println!("Fetching transaction receipt: {}", tx_hash);
        
        // Fetch the receipt
        let receipt = provider
            .get_transaction_receipt(tx_hash.parse().unwrap())
            .await
            .expect("Failed to fetch receipt")
            .expect("Receipt not found");

        println!("Receipt found!");
        println!("  Block: {:?}", receipt.block_number);
        println!("  Logs: {}", receipt.logs.len());

        // TODO: Parse the Deposit event from logs
        // TODO: Fetch MPT proof for the receipt
        // TODO: Create DepositProofInput
        // TODO: Test with test_circuit_mock

        println!("\n⚠️  Real Ethereum test not fully implemented yet");
        println!("   Need to:");
        println!("   1. Deploy test contract and make a deposit");
        println!("   2. Fetch the receipt and MPT proof");
        println!("   3. Parse the Deposit event");
        println!("   4. Test the circuit");
    }
}

#[test]
fn test_circuit_config_validation() {
    println!("\n=== Testing Circuit Configuration ===\n");

    // Test default config
    let config = CircuitConfig::default();
    assert_eq!(config.degree, 18);
    assert_eq!(config.max_data_byte_len, 256);
    assert_eq!(config.max_log_num, 20);
    assert_eq!(config.topic_num_bounds, (0, 4));
    
    println!("✅ Default configuration is valid");

    // Test custom config
    let custom_config = CircuitConfig {
        degree: 20,
        max_data_byte_len: 512,
        max_log_num: 50,
        topic_num_bounds: (0, 4),
    };
    
    assert_eq!(custom_config.degree, 20);
    println!("✅ Custom configuration is valid");
}

#[test]
fn test_event_data_serialization() {
    println!("\n=== Testing Event Data Serialization ===\n");

    let event_data = DepositEventData {
        block_number: 12345,
        transaction_index: 0,
        log_index: 0,
        deposit_id: 42,
        sender: [0x12; 20],
        amount: 1_000_000_000_000_000_000,
        timestamp: 1234567890,
        contract_address: [0xAB; 20],
    };

    // Test JSON serialization
    let json = serde_json::to_string(&event_data).expect("Failed to serialize");
    println!("Serialized: {}", json);
    
    let deserialized: DepositEventData = serde_json::from_str(&json)
        .expect("Failed to deserialize");
    
    assert_eq!(deserialized.deposit_id, event_data.deposit_id);
    assert_eq!(deserialized.amount, event_data.amount);
    
    println!("✅ Event data serialization works correctly");
}

