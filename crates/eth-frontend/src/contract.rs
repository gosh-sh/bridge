//! Ethereum contract interface (alloy-rs).
//!
//! Migrated from `ethers-rs` on 2026-05-17 — see workspace `Cargo.toml` for
//! the rationale (`ethers 2.0.14` is unmaintained and pulls a `rustls-webpki`
//! version flagged by RUSTSEC-2026-0098/-0099/-0104).
//!
//! ABI surface — Phase 4.3 (Decision Log 2026-05-17) retired the legacy
//! refund-style `withdraw()` plus its `processedDeposits`/`Withdrawal`
//! surface; this client now exposes the deposit + read-only views only.
//! The relayer (`crates/bridge-relayer-daemon`) holds the AN→ETH
//! `verifyBlock` ABI; a future burn-proof flow will reintroduce a real
//! cross-chain withdrawal once the corresponding circuit lands.

use alloy::{
    network::Network,
    primitives::{Address, U256},
    providers::Provider,
    rpc::types::TransactionReceipt,
    sol,
};

use crate::error::{BridgeError, Result};

sol! {
    #[sol(rpc)]
    contract AckiNackiBridge {
        function deposit() external payable;
        function treasuryBalance() external view returns (uint256);
        function depositCounter() external view returns (uint256);

        event Deposit(uint256 indexed depositId, address indexed sender, uint256 amount, uint256 timestamp);
    }
}

/// Ethereum contract interface.
///
/// Generic over an alloy [`Provider`] (HTTP, WS, IPC, signer-wrapped, …);
/// production callers wrap a `ProviderBuilder::new().wallet(..).on_http(..)`
/// instance, read-only callers can pass a plain HTTP provider.
pub struct EthereumContract<P: Provider<N>, N: Network = alloy::network::Ethereum> {
    contract: AckiNackiBridge::AckiNackiBridgeInstance<P, N>,
}

impl<P, N> EthereumContract<P, N>
where
    P: Provider<N> + Clone,
    N: Network,
{
    /// Construct a binding to the deployed bridge.
    pub fn new(contract_address: Address, provider: P) -> Self {
        let contract = AckiNackiBridge::new(contract_address, provider);
        Self {
            contract,
        }
    }

    /// Make a deposit to the bridge. Returns the mined transaction receipt.
    pub async fn deposit(&self, amount: U256) -> Result<N::ReceiptResponse> {
        let pending = self
            .contract
            .deposit()
            .value(amount)
            .send()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;

        pending
            .get_receipt()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))
    }

    /// Get the treasury balance.
    pub async fn treasury_balance(&self) -> Result<U256> {
        self.contract
            .treasuryBalance()
            .call()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))
    }

    /// Get the current deposit counter.
    pub async fn deposit_counter(&self) -> Result<U256> {
        self.contract
            .depositCounter()
            .call()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))
    }
}

// Suppress dead-code warnings for the unused parameter `TransactionReceipt`
// import (kept for downstream consumers reaching for `N::ReceiptResponse`).
#[allow(dead_code)]
fn _force_receipt_type_in_scope(_r: Option<TransactionReceipt>) {}
