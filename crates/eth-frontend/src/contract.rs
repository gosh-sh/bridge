//! Ethereum contract interface (alloy-rs).
//!
//! Migrated from `ethers-rs` on 2026-05-17 — see workspace `Cargo.toml` for
//! the rationale (`ethers 2.0.14` is unmaintained and pulls a `rustls-webpki`
//! version flagged by RUSTSEC-2026-0098/-0099/-0104).
//!
//! ABI surface — Phase 4.3 (Decision Log 2026-05-17) retired the legacy
//! refund-style `withdraw()` plus its `processedDeposits`/`Withdrawal`
//! surface; this client now exposes the USDC deposit + read-only views only.
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

use crate::deposit::sepolia;
use crate::error::{BridgeError, Result};

sol! {
    #[sol(rpc)]
    contract IERC20 {
        function approve(address spender, uint256 amount) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);
        function balanceOf(address account) external view returns (uint256);
    }

    #[sol(rpc)]
    contract AaveFaucet {
        function mint(address token, address to, uint256 amount) external returns (uint256);
    }

    #[sol(rpc)]
    contract AckiNackiBridge {
        function deposit(uint256 amount) external;
        function treasuryBalance() external view returns (uint256);
        function depositCounter() external view returns (uint256);
        function usdc() external view returns (address);

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
    usdc: IERC20::IERC20Instance<P, N>,
    provider: P,
}

impl<P, N> EthereumContract<P, N>
where
    P: Provider<N> + Clone,
    N: Network,
{
    /// Construct a binding to the deployed bridge. Reads `usdc()` from chain.
    pub async fn new(contract_address: Address, provider: P) -> Result<Self> {
        let contract = AckiNackiBridge::new(contract_address, provider.clone());
        let usdc_addr = contract
            .usdc()
            .call()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;
        let usdc = IERC20::new(usdc_addr, provider.clone());
        Ok(Self {
            contract,
            usdc,
            provider,
        })
    }

    /// USDC token address wired into the bridge.
    pub fn usdc_address(&self) -> Address {
        *self.usdc.address()
    }

    /// Mint test USDC from the Aave Sepolia faucet into `recipient`.
    /// Use before E2E deposits when the wallet has no USDC.
    pub async fn fund_usdc_from_aave_faucet(
        &self,
        recipient: Address,
        amount: U256,
    ) -> Result<N::ReceiptResponse> {
        let faucet = AaveFaucet::new(sepolia::AAVE_FAUCET, self.provider.clone());
        let pending = faucet
            .mint(sepolia::USDC, recipient, amount)
            .send()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;
        pending
            .get_receipt()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))
    }

    /// Ensure the signer has approved the bridge for `amount` USDC.
    pub async fn ensure_usdc_approval(&self, owner: Address, amount: U256) -> Result<bool> {
        let bridge_addr = *self.contract.address();
        let current = self
            .usdc
            .allowance(owner, bridge_addr)
            .call()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;
        if current >= amount {
            return Ok(false);
        }
        let pending = self
            .usdc
            .approve(bridge_addr, amount)
            .send()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;
        pending
            .get_receipt()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;
        Ok(true)
    }

    /// Approve (if needed) and deposit USDC. Returns the deposit tx receipt.
    pub async fn deposit(&self, owner: Address, amount: U256) -> Result<N::ReceiptResponse> {
        self.ensure_usdc_approval(owner, amount).await?;
        let pending = self
            .contract
            .deposit(amount)
            .send()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))?;
        pending
            .get_receipt()
            .await
            .map_err(|e| BridgeError::ContractError(e.to_string()))
    }

    /// E2E helper: faucet → approve → deposit.
    pub async fn fund_faucet_and_deposit(
        &self,
        recipient: Address,
        amount: U256,
    ) -> Result<N::ReceiptResponse> {
        self.fund_usdc_from_aave_faucet(recipient, amount).await?;
        self.deposit(recipient, amount).await
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

#[allow(dead_code)]
fn _force_receipt_type_in_scope(_r: Option<TransactionReceipt>) {}
