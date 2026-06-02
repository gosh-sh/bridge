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
//! no transaction-submission endpoint there, and no live
//! [`acki_nacki_interface::IAckiNacki`] implementation exists yet (only the
//! mock — the AN team ships the `tvm-sdk`-backed client). So this config wires
//! and verifies the *read* path against real endpoints; the actual
//! `finalizeDeposit` *send* still flows through
//! [`crate::submitter::AnInterfaceSubmitter`] over a (currently mock)
//! `IAckiNacki`. [`AnConfig::to_submit_config`] produces the submitter config
//! so the two halves share one source of truth.

use acki_nacki_interface::BkSetClient;
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
fn default_gas_limit() -> u64 {
    1_000_000
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
    /// TVM address of the AN-side `TokenBridge` contract (the
    /// `finalizeDeposit` target), e.g. `0:<hex>`. Empty until known.
    #[serde(default)]
    pub token_bridge: String,
    /// The relayer's AN account address (the `from` of the finalize tx).
    #[serde(default)]
    pub sender: String,
    /// Gas limit for the `finalizeDeposit` call.
    #[serde(default = "default_gas_limit")]
    pub gas_limit: u64,
    /// Seconds to wait for a finalize tx to confirm.
    #[serde(default = "default_confirm_timeout_secs")]
    pub confirm_timeout_secs: u64,
}

impl Default for AnConfig {
    fn default() -> Self {
        Self {
            node_url: default_node_url(),
            token_bridge: String::new(),
            sender: String::new(),
            gas_limit: default_gas_limit(),
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

    /// Produce the submitter configuration (maps `sender → from`).
    pub fn to_submit_config(&self) -> AnSubmitConfig {
        AnSubmitConfig {
            from: self.sender.clone(),
            token_bridge: self.token_bridge.clone(),
            gas_limit: self.gas_limit,
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
        assert_eq!(cfg.gas_limit, 1_000_000);
    }

    #[test]
    fn parses_from_json_with_partial_fields() {
        // Only the fields the operator cares about; the rest default.
        let json = r#"{
            "node_url": "http://127.0.0.1:11000",
            "token_bridge": "0:abc",
            "sender": "0:relayer"
        }"#;
        let cfg: AnConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.node_url, "http://127.0.0.1:11000");
        assert_eq!(cfg.token_bridge, "0:abc");
        assert_eq!(cfg.confirm_timeout_secs, 60); // defaulted
    }

    #[test]
    fn to_submit_config_maps_sender_to_from() {
        let cfg = AnConfig {
            node_url: DEFAULT_AN_NODE_URL.to_string(),
            token_bridge: "0:bridge".to_string(),
            sender: "0:me".to_string(),
            gas_limit: 42,
            confirm_timeout_secs: 7,
        };
        let sc = cfg.to_submit_config();
        assert_eq!(sc.from, "0:me");
        assert_eq!(sc.token_bridge, "0:bridge");
        assert_eq!(sc.gas_limit, 42);
        assert_eq!(sc.confirm_timeout_secs, 7);
    }

    #[test]
    fn bk_set_client_builds() {
        let cfg = AnConfig::from_node_url("http://example.com:8600/");
        assert!(cfg.bk_set_client().is_ok());
    }
}
