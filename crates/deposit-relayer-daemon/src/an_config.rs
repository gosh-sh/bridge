//! Acki Nacki endpoint configuration + live connectivity.
//!
//! This is the "config where AN endpoints are used" layer. It holds the AN
//! node REST base URL, the `TokenBridge` address, and the relayer's sender
//! account, and it actually *exercises* the reachable AN endpoints:
//!
//! - [`AnConfig::bk_set_client`] builds the live [`BkSetClient`] against
//!   `node_url`.
//! - [`AnConfig::preflight`] hits `GET /v2/bk_set` to confirm the node is
//!   reachable and returns a summary (current BK-set seq_no + sizes). The
//!   daemon runs this on startup so an unreachable / mis-typed endpoint fails
//!   fast instead of silently idling.
//!
//! ## Scope boundary
//!
//! The AN `/v2/` REST surface is **read-only** (BK-set views only); there is
//! no transaction-submission endpoint there. Live submission uses
//! [`acki_nacki_interface::TvmAckiNacki`] (tvm-sdk feature) via
//! [`crate::submitter::AnInterfaceSubmitter`] when the daemon runs without
//! `--dry-run` and `AnConfig::is_live_submit_ready()` is satisfied.

use acki_nacki_interface::{BkSetClient, ExtendedAddress};
use serde::{Deserialize, Serialize};

use crate::{error::RelayerError, submitter::AnSubmitConfig};

/// Default public AN test node (read-only `/v2/` surface).
pub const DEFAULT_AN_NODE_URL: &str = "http://94.156.178.19:8600";

/// Node0 REST API of the local 5-node AN cluster, used for E2E testing.
///
/// Bring it up with `cd ../acki-nacki/nock && docker-compose build &&
/// docker-compose up -d` (build with the `history_proofs` feature for layer
/// hash data). Point `AnConfig::node_url` (or `--an-node-url`) here for an
/// end-to-end run that exercises the full listen → prove → submit path
/// against a real, locally-controlled chain rather than the shared test node.
pub const DEFAULT_LOCAL_AN_NODE_URL: &str = "http://127.0.0.1:11000";

fn default_node_url() -> String {
    DEFAULT_AN_NODE_URL.to_string()
}
fn default_confirm_timeout_secs() -> u64 {
    60
}

/// Endpoint + account configuration for the Acki Nacki side of the bridge.
///
/// Loadable from a JSON file (`AnConfig::from_file`) or constructed directly;
/// the CLI also exposes the fields as flags / env vars.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnConfig {
    /// AN node REST base URL, e.g. `http://94.156.178.19:8600` (trailing
    /// slash optional).
    #[serde(default = "default_node_url")]
    pub node_url: String,
    /// GraphQL endpoint for tvm_client 3.0 (e.g. `http://127.0.0.1:11000/graphql`).
    /// Required for live submission; distinct from the REST `node_url`.
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
            node_url: default_node_url(),
            graphql_url: String::new(),
            keys_path: String::new(),
            bridge_abi_path: String::new(),
            token_bridge: String::new(),
            sender: String::new(),
            confirm_timeout_secs: default_confirm_timeout_secs(),
        }
    }
}

/// Result of a live AN connectivity probe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnPreflight {
    /// The node URL that was probed.
    pub node_url: String,
    /// Snapshot seq_no the BK set is valid at.
    pub seq_no: u64,
    /// Number of currently-active Block Keepers.
    pub bk_count: usize,
    /// Number of look-ahead (future) Block Keepers.
    pub future_bk_count: usize,
}

impl AnConfig {
    /// Construct from just a node URL, leaving the submit-side fields at
    /// their defaults (useful for the read-only `an-preflight` path).
    pub fn from_node_url(node_url: impl Into<String>) -> Self {
        Self {
            node_url: node_url.into(),
            ..Self::default()
        }
    }

    /// Load from a JSON config file.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, RelayerError> {
        let bytes = std::fs::read(path.as_ref())?;
        let cfg: AnConfig = serde_json::from_slice(&bytes)?;
        Ok(cfg)
    }

    /// Build the live BK-set REST client against `node_url`.
    pub fn bk_set_client(&self) -> Result<BkSetClient, RelayerError> {
        BkSetClient::new(self.node_url.clone()).map_err(RelayerError::from)
    }

    /// Probe the AN node's `/v2/bk_set` endpoint and summarise the response.
    /// This is a real network round-trip against `node_url`.
    pub async fn preflight(&self) -> Result<AnPreflight, RelayerError> {
        let client = self.bk_set_client()?;
        let resp = client.fetch_bk_set().await.map_err(RelayerError::from)?;
        Ok(AnPreflight {
            node_url: self.node_url.clone(),
            seq_no: resp.seq_no,
            bk_count: resp.bk_set.len(),
            future_bk_count: resp.future_bk_set.len(),
        })
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_point_at_test_node() {
        let cfg = AnConfig::default();
        assert_eq!(cfg.node_url, DEFAULT_AN_NODE_URL);
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
            "node_url": "http://127.0.0.1:11000",
            "graphql_url": "http://127.0.0.1:11000/graphql",
            "token_bridge": "{dapp}::{bridge_acc}",
            "sender": "{dapp}::{sender_acc}"
        }}"#
        );
        let cfg: AnConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg.node_url, "http://127.0.0.1:11000");
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
            node_url: DEFAULT_AN_NODE_URL.to_string(),
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
    fn bk_set_client_builds() {
        let cfg = AnConfig::from_node_url("http://example.com:8600/");
        assert!(cfg.bk_set_client().is_ok());
    }
}
