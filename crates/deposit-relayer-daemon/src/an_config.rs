//! Acki Nacki endpoint configuration for live submission.
//!
//! Holds the GraphQL endpoint used by [`acki_nacki_interface::TvmAckiNacki`],
//! the AN-side `TokenBridge` address, and the relayer's signing account. Used
//! by [`crate::submitter::AnInterfaceSubmitter`] when the daemon runs without
//! `--dry-run` and [`AnConfig::is_live_submit_ready`] is satisfied.
//!
//! The old REST-based reachability probe (`/v2/bk_set` on port 8600) was
//! removed on 2026-07-30: the endpoint has been internal-only on public
//! shellnet since AN v0.16.3 (2026-06-16), and `deposit-relayer-daemon`
//! never consumed the BK set for anything cryptographic — the probe was
//! purely cosmetic reachability metadata.

use acki_nacki_interface::ExtendedAddress;
use serde::{Deserialize, Serialize};

use crate::{error::RelayerError, submitter::AnSubmitConfig};

fn default_confirm_timeout_secs() -> u64 {
    60
}

/// Endpoint + account configuration for the Acki Nacki side of the bridge.
///
/// Loadable from a JSON file (`AnConfig::from_file`) or constructed directly;
/// the CLI also exposes the fields as flags / env vars.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnConfig {
    /// GraphQL endpoint for tvm_client 3.0 (e.g. `http://127.0.0.1:11000/graphql`).
    /// Required for live submission.
    #[serde(default)]
    pub graphql_url: String,
    /// Path to the relayer's tvm-cli keys JSON (signer for `finalizeDeposit`).
    #[serde(default)]
    pub keys_path: String,
    /// Path to the AN-side bridge contract ABI (`USDCBridge.abi.json`).
    #[serde(default)]
    pub bridge_abi_path: String,
    /// AN-side bridge contract in SDK 3.0 `dapp_id::account_id` form.
    #[serde(default)]
    pub token_bridge: String,
    /// Relayer signer account in SDK 3.0 `dapp_id::account_id` form.
    #[serde(default)]
    pub sender: String,
    /// Seconds to wait for a finalize tx to confirm.
    #[serde(default = "default_confirm_timeout_secs")]
    pub confirm_timeout_secs: u64,
}

impl Default for AnConfig {
    fn default() -> Self {
        Self {
            graphql_url: String::new(),
            keys_path: String::new(),
            bridge_abi_path: String::new(),
            token_bridge: String::new(),
            sender: String::new(),
            confirm_timeout_secs: default_confirm_timeout_secs(),
        }
    }
}

impl AnConfig {
    /// Load from a JSON config file.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, RelayerError> {
        let bytes = std::fs::read(path.as_ref())?;
        let cfg: AnConfig = serde_json::from_slice(&bytes)?;
        Ok(cfg)
    }

    /// Parse `token_bridge` as an SDK 3.0 extended address.
    pub fn token_bridge_address(&self) -> Result<ExtendedAddress, RelayerError> {
        if self.token_bridge.is_empty() {
            return Err(RelayerError::other(
                "token_bridge is required (dapp_id::account_id form)",
            ));
        }
        ExtendedAddress::parse(&self.token_bridge).map_err(RelayerError::from)
    }

    /// Parse `sender` as an SDK 3.0 extended address.
    pub fn sender_address(&self) -> Result<ExtendedAddress, RelayerError> {
        if self.sender.is_empty() {
            return Err(RelayerError::other(
                "sender is required (dapp_id::account_id form)",
            ));
        }
        ExtendedAddress::parse(&self.sender).map_err(RelayerError::from)
    }

    /// Whether the config has enough fields for live tvm_client submission.
    pub fn is_live_submit_ready(&self) -> bool {
        !self.graphql_url.is_empty()
            && !self.keys_path.is_empty()
            && !self.bridge_abi_path.is_empty()
            && !self.token_bridge.is_empty()
            && !self.sender.is_empty()
    }

    /// Produce the submitter configuration (maps `sender → from`).
    pub fn to_submit_config(&self) -> AnSubmitConfig {
        AnSubmitConfig {
            from: self.sender.clone(),
            token_bridge: self.token_bridge.clone(),
            confirm_timeout_secs: self.confirm_timeout_secs,
        }
    }

    /// Validate the GraphQL endpoint for live submission (QC-OFF-04).
    ///
    /// Production endpoints must use HTTPS unless loopback or
    /// `allow_insecure` is set.
    pub fn validate_live_graphql_endpoint(&self, allow_insecure: bool) -> Result<(), RelayerError> {
        if self.graphql_url.is_empty() {
            return Ok(());
        }
        let lower = self.graphql_url.to_ascii_lowercase();
        if lower.starts_with("https://") {
            return Ok(());
        }
        if lower.starts_with("http://127.0.0.1") || lower.starts_with("http://localhost") {
            return Ok(());
        }
        if allow_insecure {
            return Ok(());
        }
        Err(RelayerError::other(format!(
            "GraphQL URL must use HTTPS for live submit (got {}). \
             Local dev may use http://127.0.0.1; pass --allow-insecure-graphql to override.",
            self.graphql_url
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_empty_with_60s_timeout() {
        let cfg = AnConfig::default();
        assert!(cfg.graphql_url.is_empty());
        assert_eq!(cfg.confirm_timeout_secs, 60);
    }

    #[test]
    fn parses_from_json_with_partial_fields() {
        // Only the fields the operator cares about; the rest default.
        let dapp = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let bridge_acc = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let sender_acc = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let json = format!(
            r#"{{
            "graphql_url": "http://127.0.0.1:11000/graphql",
            "token_bridge": "{dapp}::{bridge_acc}",
            "sender": "{dapp}::{sender_acc}"
        }}"#
        );
        let cfg: AnConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg.graphql_url, "http://127.0.0.1:11000/graphql");
        assert_eq!(cfg.token_bridge, format!("{dapp}::{bridge_acc}"));
        assert_eq!(cfg.confirm_timeout_secs, 60); // defaulted
    }

    #[test]
    fn to_submit_config_maps_sender_to_from() {
        let dapp = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        let bridge = format!("{dapp}::{dapp}");
        let sender =
            format!("{dapp}::dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd");
        let cfg = AnConfig {
            token_bridge: bridge.clone(),
            sender: sender.clone(),
            confirm_timeout_secs: 7,
            ..AnConfig::default()
        };
        let sc = cfg.to_submit_config();
        assert_eq!(sc.from, sender);
        assert_eq!(sc.token_bridge, bridge);
        assert_eq!(sc.confirm_timeout_secs, 7);
    }

    #[test]
    fn graphql_https_ok() {
        let cfg = AnConfig {
            graphql_url: "https://shellnet.ackinacki.org/graphql".into(),
            ..AnConfig::default()
        };
        assert!(cfg.validate_live_graphql_endpoint(false).is_ok());
    }

    #[test]
    fn graphql_http_rejected_without_override() {
        let cfg = AnConfig {
            graphql_url: "http://an-node.example:8600/graphql".into(),
            ..AnConfig::default()
        };
        assert!(cfg.validate_live_graphql_endpoint(false).is_err());
        assert!(cfg.validate_live_graphql_endpoint(true).is_ok());
    }

    #[test]
    fn graphql_loopback_http_allowed() {
        let cfg = AnConfig {
            graphql_url: "http://127.0.0.1:11000/graphql".into(),
            ..AnConfig::default()
        };
        assert!(cfg.validate_live_graphql_endpoint(false).is_ok());
    }
}
