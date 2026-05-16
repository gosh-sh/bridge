//! Ethereum contract interface

use std::sync::Arc;

use ethers::prelude::*;

use crate::error::{BridgeError, Result};

// ABI subset for the AckiNackiBridge contract. Phase 4.3 (Decision Log
// 2026-05-17) retired the legacy refund-style `withdraw()` plus its
// `processedDeposits`/`Withdrawal` surface; this client now exposes the
// deposit + read-only views only. The relayer (`crates/bridge-relayer-daemon`)
// holds the AN→ETH `verifyBlock` ABI; a future burn-proof flow will reintroduce
// a real cross-chain withdrawal once the corresponding circuit lands.
abigen!(
    AckiNackiBridge,
    r#"[
        function deposit() external payable
        function treasuryBalance() external view returns (uint256)
        function depositCounter() external view returns (uint256)
        event Deposit(uint256 indexed depositId, address indexed sender, uint256 amount, uint256 timestamp)
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

}
