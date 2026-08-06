//! Allowlist of EVM chains the deposit prover may fetch witnesses from.
//!
//! The list itself lives in the dependency-free `deposit-chain-ids` crate so
//! the prover and the relayer physically share one definition; this module only
//! adds the prover's `anyhow`-flavoured accessor.

pub use deposit_chain_ids::{
    is_supported_deposit_chain, supported_deposit_chain_name, supported_deposit_chains_display,
    CHAIN_ID_ARBITRUM_ONE, CHAIN_ID_BASE, CHAIN_ID_BLAST, CHAIN_ID_MANTLE, CHAIN_ID_OP_MAINNET,
    CHAIN_ID_SEPOLIA, CHAIN_ID_WORLD_CHAIN, SUPPORTED_DEPOSIT_CHAIN_IDS,
};

use anyhow::{bail, Result};

/// Returns `Ok(chain_id)` if `chain_id` is in [`SUPPORTED_DEPOSIT_CHAIN_IDS`].
pub fn require_supported_deposit_chain(chain_id: u64) -> Result<u64> {
    if is_supported_deposit_chain(chain_id) {
        Ok(chain_id)
    } else {
        bail!(
            "unsupported deposit chain id {chain_id}; supported: {}",
            supported_deposit_chains_display()
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
