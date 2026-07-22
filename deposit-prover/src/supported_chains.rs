//! Allowlist of EVM chains the deposit prover may fetch witnesses from.
//!
//! One WITHVK VkBlob verifies deposits from any of these networks; the
//! AN-side `USDCBridge` allowlists `(chainId → expected bridge Fr)`.
//! Unsupported chain IDs are rejected at fetch time.

use anyhow::{bail, Result};

/// Ethereum Sepolia (shellnet / fixture network).
pub const CHAIN_ID_SEPOLIA: u64 = 11_155_111;
/// OP Mainnet.
pub const CHAIN_ID_OP_MAINNET: u64 = 10;
/// World Chain.
pub const CHAIN_ID_WORLD_CHAIN: u64 = 480;
/// Mantle.
pub const CHAIN_ID_MANTLE: u64 = 5_000;
/// Base.
pub const CHAIN_ID_BASE: u64 = 8_453;
/// Arbitrum One.
pub const CHAIN_ID_ARBITRUM_ONE: u64 = 42_161;
/// Blast.
pub const CHAIN_ID_BLAST: u64 = 81_457;

/// Supported deposit-source networks (six L2s + Sepolia).
pub const SUPPORTED_DEPOSIT_CHAIN_IDS: &[u64] = &[
    CHAIN_ID_OP_MAINNET,
    CHAIN_ID_WORLD_CHAIN,
    CHAIN_ID_MANTLE,
    CHAIN_ID_BASE,
    CHAIN_ID_ARBITRUM_ONE,
    CHAIN_ID_BLAST,
    CHAIN_ID_SEPOLIA,
];

/// Human-readable name for a supported chain id, if known.
pub fn supported_deposit_chain_name(chain_id: u64) -> Option<&'static str> {
    match chain_id {
        CHAIN_ID_OP_MAINNET => Some("OP Mainnet"),
        CHAIN_ID_WORLD_CHAIN => Some("World Chain"),
        CHAIN_ID_MANTLE => Some("Mantle"),
        CHAIN_ID_BASE => Some("Base"),
        CHAIN_ID_ARBITRUM_ONE => Some("Arbitrum One"),
        CHAIN_ID_BLAST => Some("Blast"),
        CHAIN_ID_SEPOLIA => Some("Sepolia"),
        _ => None,
    }
}

/// Returns `Ok(chain_id)` if `chain_id` is in [`SUPPORTED_DEPOSIT_CHAIN_IDS`].
pub fn require_supported_deposit_chain(chain_id: u64) -> Result<u64> {
    if SUPPORTED_DEPOSIT_CHAIN_IDS.contains(&chain_id) {
        Ok(chain_id)
    } else {
        bail!(
            "unsupported deposit chain id {chain_id}; supported: {:?}",
            SUPPORTED_DEPOSIT_CHAIN_IDS
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_covers_six_l2s_and_sepolia() {
        assert_eq!(SUPPORTED_DEPOSIT_CHAIN_IDS.len(), 7);
        for id in [
            CHAIN_ID_ARBITRUM_ONE,
            CHAIN_ID_BASE,
            CHAIN_ID_OP_MAINNET,
            CHAIN_ID_MANTLE,
            CHAIN_ID_WORLD_CHAIN,
            CHAIN_ID_BLAST,
            CHAIN_ID_SEPOLIA,
        ] {
            assert!(require_supported_deposit_chain(id).is_ok());
            assert!(supported_deposit_chain_name(id).is_some());
        }
        assert!(require_supported_deposit_chain(1).is_err());
        assert!(require_supported_deposit_chain(31337).is_err());
    }
}
