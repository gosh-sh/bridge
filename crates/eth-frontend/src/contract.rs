//! Ethereum contract interface

use std::sync::Arc;

use ethers::prelude::*;

use crate::error::{BridgeError, Result};

// ABI for the AckiNackiBridge contract (matches actual deployed contract)
abigen!(
    AckiNackiBridge,
    r#"[
        function deposit() external payable
        function withdraw(address payable recipient, uint256 amount, uint256 depositId, bytes calldata proof) external
        function treasuryBalance() external view returns (uint256)
        function depositCounter() external view returns (uint256)
        function processedDeposits(uint256 depositId) external view returns (bool)
        event Deposit(uint256 indexed depositId, address indexed sender, uint256 amount, uint256 timestamp)
        event Withdrawal(uint256 indexed depositId, address indexed recipient, uint256 amount, uint256 timestamp)
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
        Self {
            contract,
            _client: client,
        }
    }

    /// Make a deposit to the bridge
    pub async fn deposit(&self, amount: U256) -> Result<TransactionReceipt> {
        let tx = self.contract.deposit().value(amount);

        let pending_tx = tx
            .send()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        let receipt = pending_tx
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?
            .ok_or_else(|| BridgeError::ContractError("No receipt".to_string()))?;

        Ok(receipt)
    }

    /// Withdraw from the bridge using ZK proof
    pub async fn withdraw(
        &self,
        recipient: Address,
        amount: U256,
        deposit_id: U256,
        proof: Vec<u8>,
    ) -> Result<TransactionReceipt> {
        let tx = self
            .contract
            .withdraw(recipient, amount, deposit_id, proof.into());

        let pending_tx = tx
            .send()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        let receipt = pending_tx
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?
            .ok_or_else(|| BridgeError::ContractError("No receipt".to_string()))?;

        Ok(receipt)
    }

    /// Get the treasury balance
    pub async fn treasury_balance(&self) -> Result<U256> {
        let balance = self
            .contract
            .treasury_balance()
            .call()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        Ok(balance)
    }

    /// Get the current deposit counter
    pub async fn deposit_counter(&self) -> Result<U256> {
        let counter = self
            .contract
            .deposit_counter()
            .call()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        Ok(counter)
    }

    /// Check if a deposit has been processed
    pub async fn is_deposit_processed(&self, deposit_id: U256) -> Result<bool> {
        let processed = self
            .contract
            .processed_deposits(deposit_id)
            .call()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        Ok(processed)
    }
}
