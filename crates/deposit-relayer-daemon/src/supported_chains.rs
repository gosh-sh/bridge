//! Supported EVM deposit-source chain IDs.
//!
//! Re-exported verbatim from the dependency-free `deposit-chain-ids` crate,
//! which `deposit-prover` depends on too — the relayer cannot accept a chain
//! the prover would refuse to build a witness for.

pub use deposit_chain_ids::{
    is_supported_deposit_chain, supported_deposit_chain_name, supported_deposit_chains_display,
    CHAIN_ID_ARBITRUM_ONE, CHAIN_ID_BASE, CHAIN_ID_BLAST, CHAIN_ID_MANTLE, CHAIN_ID_OP_MAINNET,
    CHAIN_ID_SEPOLIA, CHAIN_ID_WORLD_CHAIN, SUPPORTED_DEPOSIT_CHAIN_IDS,
};

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
