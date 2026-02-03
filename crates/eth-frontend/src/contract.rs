//! Ethereum contract interface

use crate::error::{BridgeError, Result};
use crypto::Hash;
use ethers::prelude::*;
use std::sync::Arc;

// ABI for the AckiNackiBridge contract
abigen!(
    AckiNackiBridge,
    r#"[
        function deposit(bytes32 commitment, uint256 amount) external payable
        function withdraw(bytes32 nullifier, address recipient, uint256 amount, bytes32 root, bytes proof) external
        function getRoot() external view returns (bytes32)
        function isNullifierUsed(bytes32 nullifier) external view returns (bool)
        function getLeafCount() external view returns (uint256)
        function getLeaf(uint256 index) external view returns (bytes32)
        function nextIndex() external view returns (uint256)
        function treasuryBalance() external view returns (uint256)
        event Deposit(bytes32 indexed commitment, bytes32 indexed commitmentWithAmount, uint256 leafIndex, uint256 amount, uint256 timestamp)
        event Withdrawal(bytes32 indexed nullifier, address indexed recipient, uint256 amount, uint256 timestamp)
    ]"#
);

/// Ethereum contract interface
pub struct EthereumContract<M: Middleware> {
    contract: AckiNackiBridge<M>,
    _client: Arc<M>,
}

impl<M: Middleware> EthereumContract<M> {
    /// Create a new contract interface
    pub fn new(contract_address: Address, client: Arc<M>) -> Self {
        let contract = AckiNackiBridge::new(contract_address, client.clone());
        Self { contract, _client: client }
    }

    /// Make a deposit to the bridge
    pub async fn deposit(&self, commitment: Hash, amount: U256) -> Result<TransactionReceipt> {
        let commitment_bytes: [u8; 32] = commitment.into();

        let tx = self.contract
            .deposit(commitment_bytes, amount)
            .value(amount);

        let pending_tx = tx.send().await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        let receipt = pending_tx.await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?
            .ok_or_else(|| BridgeError::ContractError("No receipt".to_string()))?;

        Ok(receipt)
    }

    /// Withdraw from the bridge
    pub async fn withdraw(
        &self,
        nullifier: Hash,
        recipient: Address,
        amount: U256,
        root: Hash,
        proof: Vec<u8>,
    ) -> Result<TransactionReceipt> {
        let nullifier_bytes: [u8; 32] = nullifier.into();
        let root_bytes: [u8; 32] = root.into();

        let tx = self.contract.withdraw(
            nullifier_bytes,
            recipient,
            amount,
            root_bytes,
            proof.into(),
        );

        let pending_tx = tx.send().await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        let receipt = pending_tx.await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?
            .ok_or_else(|| BridgeError::ContractError("No receipt".to_string()))?;

        Ok(receipt)
    }

    /// Get the current Merkle root
    pub async fn get_root(&self) -> Result<Hash> {
        let root = self.contract.get_root().call().await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        Ok(Hash::new(root))
    }

    /// Check if a nullifier has been used
    pub async fn is_nullifier_used(&self, nullifier: Hash) -> Result<bool> {
        let nullifier_bytes: [u8; 32] = nullifier.into();

        let used = self.contract.is_nullifier_used(nullifier_bytes).call().await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        Ok(used)
    }

    /// Get the number of leaves in the tree
    pub async fn get_leaf_count(&self) -> Result<u64> {
        let count = self.contract.get_leaf_count().call().await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        Ok(count.as_u64())
    }

    /// Get a leaf at a specific index
    pub async fn get_leaf(&self, index: u64) -> Result<Hash> {
        let leaf = self.contract.get_leaf(U256::from(index)).call().await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        Ok(Hash::new(leaf))
    }

    /// Get the next index
    pub async fn next_index(&self) -> Result<u64> {
        let index = self.contract.next_index().call().await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        Ok(index.as_u64())
    }

    /// Get the treasury balance
    pub async fn treasury_balance(&self) -> Result<U256> {
        let balance = self.contract.treasury_balance().call().await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        Ok(balance)
    }
}

