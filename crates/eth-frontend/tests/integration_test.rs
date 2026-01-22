//! Integration tests for Ethereum frontend with actual smart contract
//!
//! These tests require a local Ethereum node (e.g., Anvil) to be running.
//! Run with: `anvil` in a separate terminal, then `cargo test --test integration_test`

use crypto::{Hash, SecureRng, hash_commitment};
use eth_frontend::EthereumContract;
use ethers::prelude::*;
use std::sync::Arc;

// Contract bytecode and ABI would be loaded from the compiled Solidity contract
// For now, we'll use a placeholder that assumes the contract is already deployed

const ANVIL_ENDPOINT: &str = "http://localhost:8545";

/// Helper to setup test environment
async fn setup_test_env() -> (Arc<SignerMiddleware<Provider<Http>, LocalWallet>>, Address) {
    // Connect to local Anvil node
    let provider = Provider::<Http>::try_from(ANVIL_ENDPOINT)
        .expect("Failed to connect to Anvil");
    
    // Use the first Anvil account
    let wallet: LocalWallet = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
        .parse::<LocalWallet>()
        .expect("Failed to parse wallet")
        .with_chain_id(31337u64); // Anvil chain ID
    
    let client = Arc::new(SignerMiddleware::new(provider, wallet));
    
    // TODO: Deploy contract here
    // For now, assume contract is deployed at a known address
    let contract_address = Address::zero(); // Placeholder
    
    (client, contract_address)
}

#[tokio::test]
#[ignore] // Ignore by default since it requires Anvil to be running
async fn test_deposit_integration() {
    let (client, contract_address) = setup_test_env().await;
    let contract = EthereumContract::new(contract_address, client.clone());
    
    // Generate commitment
    let mut rng = SecureRng::new();
    let withdrawal_hash = rng.random_hash();
    let nullifier = rng.random_hash();
    let commitment = hash_commitment(&withdrawal_hash, &nullifier);
    
    // Make deposit
    let amount = U256::from(1_000_000_000_000_000_000u64); // 1 ETH
    let receipt = contract.deposit(commitment, amount).await
        .expect("Deposit failed");
    
    assert!(receipt.status == Some(U64::from(1)), "Transaction should succeed");
    
    // Verify state
    let leaf_count = contract.get_leaf_count().await.expect("Failed to get leaf count");
    assert_eq!(leaf_count, 1, "Should have 1 leaf");
    
    let leaf = contract.get_leaf(0).await.expect("Failed to get leaf");
    assert_eq!(leaf, commitment, "Leaf should match commitment");
    
    let treasury = contract.treasury_balance().await.expect("Failed to get treasury");
    assert_eq!(treasury, amount, "Treasury should have deposit amount");
}

#[tokio::test]
#[ignore]
async fn test_multiple_deposits_integration() {
    let (client, contract_address) = setup_test_env().await;
    let contract = EthereumContract::new(contract_address, client.clone());
    
    let mut rng = SecureRng::new();
    let amount = U256::from(1_000_000_000_000_000_000u64); // 1 ETH
    
    // Make 5 deposits
    for i in 0..5 {
        let withdrawal_hash = rng.random_hash();
        let nullifier = rng.random_hash();
        let commitment = hash_commitment(&withdrawal_hash, &nullifier);
        
        let receipt = contract.deposit(commitment, amount).await
            .expect(&format!("Deposit {} failed", i));
        
        assert!(receipt.status == Some(U64::from(1)));
    }
    
    // Verify state
    let leaf_count = contract.get_leaf_count().await.expect("Failed to get leaf count");
    assert_eq!(leaf_count, 5, "Should have 5 leaves");
    
    let treasury = contract.treasury_balance().await.expect("Failed to get treasury");
    assert_eq!(treasury, amount * 5, "Treasury should have 5x deposit amount");
}

#[tokio::test]
#[ignore]
async fn test_merkle_root_changes_integration() {
    let (client, contract_address) = setup_test_env().await;
    let contract = EthereumContract::new(contract_address, client.clone());
    
    let mut rng = SecureRng::new();
    let amount = U256::from(1_000_000_000_000_000_000u64);
    
    // Get initial root
    let root0 = contract.get_root().await.expect("Failed to get root");
    
    // Make first deposit
    let commitment1 = hash_commitment(&rng.random_hash(), &rng.random_hash());
    contract.deposit(commitment1, amount).await.expect("Deposit 1 failed");
    
    let root1 = contract.get_root().await.expect("Failed to get root");
    assert_ne!(root0, root1, "Root should change after deposit");
    
    // Make second deposit
    let commitment2 = hash_commitment(&rng.random_hash(), &rng.random_hash());
    contract.deposit(commitment2, amount).await.expect("Deposit 2 failed");
    
    let root2 = contract.get_root().await.expect("Failed to get root");
    assert_ne!(root1, root2, "Root should change after second deposit");
}

#[tokio::test]
#[ignore]
async fn test_withdrawal_integration() {
    let (client, contract_address) = setup_test_env().await;
    let contract = EthereumContract::new(contract_address, client.clone());
    
    let mut rng = SecureRng::new();
    let withdrawal_hash = rng.random_hash();
    let nullifier = rng.random_hash();
    let commitment = hash_commitment(&withdrawal_hash, &nullifier);
    let amount = U256::from(1_000_000_000_000_000_000u64);
    
    // Make deposit
    contract.deposit(commitment, amount).await.expect("Deposit failed");
    
    // Get root
    let root = contract.get_root().await.expect("Failed to get root");
    
    // Make withdrawal
    let recipient = Address::random();
    let proof = vec![1, 2, 3]; // Dummy proof for now
    
    let receipt = contract.withdraw(nullifier, recipient, amount, root, proof).await
        .expect("Withdrawal failed");
    
    assert!(receipt.status == Some(U64::from(1)), "Withdrawal should succeed");
    
    // Verify nullifier is used
    let is_used = contract.is_nullifier_used(nullifier).await
        .expect("Failed to check nullifier");
    assert!(is_used, "Nullifier should be marked as used");
    
    // Verify treasury is empty
    let treasury = contract.treasury_balance().await.expect("Failed to get treasury");
    assert_eq!(treasury, U256::zero(), "Treasury should be empty");
}

#[tokio::test]
#[ignore]
async fn test_double_spend_prevention_integration() {
    let (client, contract_address) = setup_test_env().await;
    let contract = EthereumContract::new(contract_address, client.clone());
    
    let mut rng = SecureRng::new();
    let withdrawal_hash = rng.random_hash();
    let nullifier = rng.random_hash();
    let commitment = hash_commitment(&withdrawal_hash, &nullifier);
    let amount = U256::from(1_000_000_000_000_000_000u64);
    
    // Make deposit
    contract.deposit(commitment, amount).await.expect("Deposit failed");
    
    // Get root
    let root = contract.get_root().await.expect("Failed to get root");
    
    // First withdrawal
    let recipient = Address::random();
    let proof = vec![1, 2, 3];
    
    contract.withdraw(nullifier, recipient, amount, root, proof.clone()).await
        .expect("First withdrawal failed");
    
    // Try second withdrawal with same nullifier (should fail)
    let result = contract.withdraw(nullifier, recipient, amount, root, proof).await;
    assert!(result.is_err(), "Second withdrawal should fail");
}

#[tokio::test]
#[ignore]
async fn test_invalid_root_rejection_integration() {
    let (client, contract_address) = setup_test_env().await;
    let contract = EthereumContract::new(contract_address, client.clone());
    
    let mut rng = SecureRng::new();
    let withdrawal_hash = rng.random_hash();
    let nullifier = rng.random_hash();
    let commitment = hash_commitment(&withdrawal_hash, &nullifier);
    let amount = U256::from(1_000_000_000_000_000_000u64);
    
    // Make deposit
    contract.deposit(commitment, amount).await.expect("Deposit failed");
    
    // Use wrong root
    let wrong_root = Hash::new([0u8; 32]);
    let recipient = Address::random();
    let proof = vec![1, 2, 3];
    
    // Try withdrawal with wrong root (should fail)
    let result = contract.withdraw(nullifier, recipient, amount, wrong_root, proof).await;
    assert!(result.is_err(), "Withdrawal with wrong root should fail");
}

#[tokio::test]
#[ignore]
async fn test_empty_proof_rejection_integration() {
    let (client, contract_address) = setup_test_env().await;
    let contract = EthereumContract::new(contract_address, client.clone());
    
    let mut rng = SecureRng::new();
    let withdrawal_hash = rng.random_hash();
    let nullifier = rng.random_hash();
    let commitment = hash_commitment(&withdrawal_hash, &nullifier);
    let amount = U256::from(1_000_000_000_000_000_000u64);
    
    // Make deposit
    contract.deposit(commitment, amount).await.expect("Deposit failed");
    
    // Get root
    let root = contract.get_root().await.expect("Failed to get root");
    
    // Try withdrawal with empty proof (should fail)
    let recipient = Address::random();
    let empty_proof = vec![];
    
    let result = contract.withdraw(nullifier, recipient, amount, root, empty_proof).await;
    assert!(result.is_err(), "Withdrawal with empty proof should fail");
}

