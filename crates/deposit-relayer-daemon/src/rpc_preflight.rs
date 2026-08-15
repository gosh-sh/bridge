//! Startup validation for source vs prover Ethereum RPC `eth_chainId` (TD-30).

use alloy::providers::Provider;

use crate::{
    error::RelayerError,
    is_supported_deposit_chain,
    supported_deposit_chains_display,
};

/// Validate a resolved `eth_chainId` against the deposit allowlist and optional CLI flag.
pub fn ensure_supported_chain_id_value(
    chain_id: u64,
    expect_chain_id: Option<u64>,
) -> Result<(), RelayerError> {
    if !is_supported_deposit_chain(chain_id) {
        return Err(RelayerError::other(format!(
            "RPC eth_chainId {chain_id} is not a supported deposit chain; supported: {}",
            supported_deposit_chains_display()
        )));
    }
    if let Some(expected) = expect_chain_id {
        if chain_id != expected {
            return Err(RelayerError::other(format!(
                "RPC eth_chainId {chain_id} != --expect-chain-id {expected}"
            )));
        }
    }
    Ok(())
}

/// Source and prover RPC must agree on `eth_chainId` before proving starts.
pub fn ensure_matching_source_prover_chain_ids(
    source_chain_id: u64,
    prover_chain_id: u64,
) -> Result<(), RelayerError> {
    if source_chain_id != prover_chain_id {
        return Err(RelayerError::other(format!(
            "prover RPC eth_chainId {prover_chain_id} != source RPC eth_chainId \
             {source_chain_id}; align --rpc-url and --prover-rpc-url"
        )));
    }
    Ok(())
}

/// Resolve `eth_chainId` from a provider and validate allowlist / `--expect-chain-id`.
pub async fn resolve_supported_chain_id<P: Provider>(
    provider: &P,
    expect_chain_id: Option<u64>,
) -> Result<u64, RelayerError> {
    let id = provider
        .get_chain_id()
        .await
        .map_err(|e| RelayerError::eth(format!("eth_chainId failed: {e}")))?;
    ensure_supported_chain_id_value(id, expect_chain_id)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEPOLIA: u64 = 11_155_111;
    const BASE: u64 = 8_453;

    #[test]
    fn matching_chain_ids_ok() {
        ensure_matching_source_prover_chain_ids(SEPOLIA, SEPOLIA).unwrap();
    }

    #[test]
    fn mismatch_chain_ids_err() {
        let err = ensure_matching_source_prover_chain_ids(SEPOLIA, BASE).unwrap_err();
        assert!(err.to_string().contains("prover RPC eth_chainId"));
        assert!(err.to_string().contains("8453"));
        assert!(err.to_string().contains("11155111"));
    }
}
