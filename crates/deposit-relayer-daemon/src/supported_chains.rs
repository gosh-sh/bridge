//! Supported EVM deposit-source chain IDs (mirror of
//! `deposit-prover::supported_chains`). One WITHVK VkBlob verifies all;
//! AN allowlists `(chainId → bridge Fr)`.

/// Ethereum Sepolia (shellnet / fixtures).
pub const CHAIN_ID_SEPOLIA: u64 = 11_155_111;
pub const CHAIN_ID_OP_MAINNET: u64 = 10;
pub const CHAIN_ID_WORLD_CHAIN: u64 = 480;
pub const CHAIN_ID_MANTLE: u64 = 5_000;
pub const CHAIN_ID_BASE: u64 = 8_453;
pub const CHAIN_ID_ARBITRUM_ONE: u64 = 42_161;
pub const CHAIN_ID_BLAST: u64 = 81_457;

pub const SUPPORTED_DEPOSIT_CHAIN_IDS: &[u64] = &[
    CHAIN_ID_OP_MAINNET,
    CHAIN_ID_WORLD_CHAIN,
    CHAIN_ID_MANTLE,
    CHAIN_ID_BASE,
    CHAIN_ID_ARBITRUM_ONE,
    CHAIN_ID_BLAST,
    CHAIN_ID_SEPOLIA,
];

pub fn is_supported_deposit_chain(chain_id: u64) -> bool {
    SUPPORTED_DEPOSIT_CHAIN_IDS.contains(&chain_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_l2s_plus_sepolia() {
        assert_eq!(SUPPORTED_DEPOSIT_CHAIN_IDS.len(), 7);
        assert!(is_supported_deposit_chain(CHAIN_ID_BASE));
        assert!(!is_supported_deposit_chain(1));
    }
}
