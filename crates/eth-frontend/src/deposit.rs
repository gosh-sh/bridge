//! USDT deposit flow helpers.
//!
//! Users must hold USDT before depositing. Swap ETH → USDT on Uniswap (or another
//! DEX) in your wallet first — the bridge does not perform the swap.

/// Instruction shown in UIs before the user initiates a deposit.
pub const PRE_DEPOSIT_USER_INSTRUCTION: &str =
    "Swap your ETH to USDT on Uniswap before depositing. The bridge only accepts USDT (ERC-20).";

/// Aave V3 Sepolia faucet — mints test USDT for E2E / dev wallets.
pub mod sepolia {
    use alloy::primitives::{address, Address, U256};

    /// Aave permissionless testnet faucet (`AaveV3Sepolia.FAUCET`).
    pub const AAVE_FAUCET: Address = address!("0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D");

    /// Aave-faucet USDT underlying on Sepolia (6 decimals).
    pub const USDT: Address = address!("0xaA8E23Fb1079EA71e0a56F48a2aA51851D8433D0");

    /// Default E2E mint amount: 100 USDT.
    pub const DEFAULT_FAUCET_AMOUNT: U256 = U256::from_limbs([100_000_000, 0, 0, 0]);
}

/// Deposit manager — coordinates approve + deposit for USDT.
pub struct DepositManager;

impl DepositManager {
    pub fn new() -> Self {
        Self
    }

    /// Human-readable pre-deposit instruction for frontends.
    pub fn user_instruction() -> &'static str {
        PRE_DEPOSIT_USER_INSTRUCTION
    }
}

impl Default for DepositManager {
    fn default() -> Self {
        Self::new()
    }
}
