//! Allowlist of EVM chains the deposit bridge accepts deposits from.
//!
//! One WITHVK VkBlob verifies deposits from any of these networks; the AN-side
//! `USDCBridge` allowlists `(chainId → expected bridge Fr)`.
//!
//! The prover and the relayer live in separate cargo trees and used to carry
//! their own copy of this list. That is exactly the shape of the
//! `HISTORY_PROOF_WINDOW_SIZE` drift (see `AGENTS.md`): two literals that must
//! agree, with nothing forcing them to. Both crates now depend on this one, so
//! the lists cannot diverge.

#![forbid(unsafe_code)]

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

/// Testnet-only chains (shellnet / fixtures). Must not appear on production
/// deploy paths.
pub const TESTNET_ONLY_DEPOSIT_CHAIN_IDS: &[u64] = &[CHAIN_ID_SEPOLIA];

/// Production deposit sources (six L2 mainnets — no Sepolia).
pub const PRODUCTION_DEPOSIT_CHAIN_IDS: &[u64] = &[
    CHAIN_ID_OP_MAINNET,
    CHAIN_ID_WORLD_CHAIN,
    CHAIN_ID_MANTLE,
    CHAIN_ID_BASE,
    CHAIN_ID_ARBITRUM_ONE,
    CHAIN_ID_BLAST,
];

/// Deployment profile for ops policy gates (TD-16).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DepositDeploymentProfile {
    /// Mainnet L2 production — Sepolia forbidden.
    Production,
    /// Shellnet, dev, audit — Sepolia allowed.
    ShellnetOrDev,
}

/// Parse `PROFILE` env values used by ops scripts and runbooks.
pub fn parse_deployment_profile(profile: &str) -> Option<DepositDeploymentProfile> {
    match profile.trim().to_ascii_lowercase().as_str() {
        "prod" | "production" => Some(DepositDeploymentProfile::Production),
        "shellnet" | "dev" | "testnet" | "audit" => Some(DepositDeploymentProfile::ShellnetOrDev),
        _ => None,
    }
}

/// Whether `chain_id` is allowed on production deposit deployments.
pub fn is_production_deposit_chain(chain_id: u64) -> bool {
    PRODUCTION_DEPOSIT_CHAIN_IDS.contains(&chain_id)
}

/// Testnet-only chains (today: Sepolia) — never production mint paths.
pub fn is_testnet_only_deposit_chain(chain_id: u64) -> bool {
    TESTNET_ONLY_DEPOSIT_CHAIN_IDS.contains(&chain_id)
}

/// `prod ∩ testnet_only` must be empty (DEP-T16 / TD-16).
pub fn production_and_testnet_only_disjoint() -> bool {
    PRODUCTION_DEPOSIT_CHAIN_IDS
        .iter()
        .all(|id| !is_testnet_only_deposit_chain(*id))
}

/// Ops policy: which `eth_chainId` values are valid for a deployment profile.
pub fn validate_chain_for_deployment_profile(
    chain_id: u64,
    profile: DepositDeploymentProfile,
) -> Result<(), String> {
    match profile {
        DepositDeploymentProfile::Production => {
            if is_testnet_only_deposit_chain(chain_id) {
                return Err(format!(
                    "chainId {chain_id} (testnet-only, e.g. Sepolia) is forbidden for production \
                     deposit deployments — free testnet USDC mint risk"
                ));
            }
            if !is_production_deposit_chain(chain_id) {
                return Err(format!(
                    "chainId {chain_id} is not in PRODUCTION_DEPOSIT_CHAIN_IDS: {}",
                    production_deposit_chains_display()
                ));
            }
            Ok(())
        },
        DepositDeploymentProfile::ShellnetOrDev => {
            if !is_supported_deposit_chain(chain_id) {
                return Err(format!(
                    "chainId {chain_id} is not supported; allowed: {}",
                    supported_deposit_chains_display()
                ));
            }
            Ok(())
        },
    }
}

/// Render production allowlist for error messages.
pub fn production_deposit_chains_display() -> String {
    PRODUCTION_DEPOSIT_CHAIN_IDS
        .iter()
        .map(|id| match supported_deposit_chain_name(*id) {
            Some(name) => format!("{id} ({name})"),
            None => id.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

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

/// Whether `chain_id` is in [`SUPPORTED_DEPOSIT_CHAIN_IDS`].
pub fn is_supported_deposit_chain(chain_id: u64) -> bool {
    SUPPORTED_DEPOSIT_CHAIN_IDS.contains(&chain_id)
}

/// Rendered allowlist for error messages: `10 (OP Mainnet), 480 (World Chain),
/// …`.
pub fn supported_deposit_chains_display() -> String {
    SUPPORTED_DEPOSIT_CHAIN_IDS
        .iter()
        .map(|id| match supported_deposit_chain_name(*id) {
            Some(name) => format!("{id} ({name})"),
            None => id.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_covers_six_l2s_and_sepolia() {
        assert_eq!(SUPPORTED_DEPOSIT_CHAIN_IDS.len(), 7);
        for id in SUPPORTED_DEPOSIT_CHAIN_IDS {
            assert!(is_supported_deposit_chain(*id));
            assert!(supported_deposit_chain_name(*id).is_some());
        }
        assert!(!is_supported_deposit_chain(1));
        assert!(!is_supported_deposit_chain(31337));
    }

    #[test]
    fn every_id_is_listed_once() {
        let mut sorted = SUPPORTED_DEPOSIT_CHAIN_IDS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), SUPPORTED_DEPOSIT_CHAIN_IDS.len());
    }

    #[test]
    fn display_names_every_chain() {
        let rendered = supported_deposit_chains_display();
        assert!(rendered.contains("42161 (Arbitrum One)"));
        assert_eq!(
            rendered.matches(',').count(),
            SUPPORTED_DEPOSIT_CHAIN_IDS.len() - 1
        );
    }

    #[test]
    fn td_16_prod_testnet_only_disjoint() {
        assert!(production_and_testnet_only_disjoint());
        assert!(!is_production_deposit_chain(CHAIN_ID_SEPOLIA));
        assert!(is_testnet_only_deposit_chain(CHAIN_ID_SEPOLIA));
    }

    #[test]
    fn td_16_prod_profile_rejects_sepolia() {
        let err = validate_chain_for_deployment_profile(
            CHAIN_ID_SEPOLIA,
            DepositDeploymentProfile::Production,
        )
        .unwrap_err();
        assert!(err.contains("testnet-only"));
    }

    #[test]
    fn td_16_shellnet_profile_accepts_sepolia() {
        validate_chain_for_deployment_profile(
            CHAIN_ID_SEPOLIA,
            DepositDeploymentProfile::ShellnetOrDev,
        )
        .unwrap();
    }

    #[test]
    fn td_16_prod_profile_accepts_base() {
        validate_chain_for_deployment_profile(CHAIN_ID_BASE, DepositDeploymentProfile::Production)
            .unwrap();
    }
}
