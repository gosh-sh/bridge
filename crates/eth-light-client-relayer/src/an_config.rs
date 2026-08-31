//! Acki Nacki endpoint config for live `EthBeaconLightClient` submit.

use acki_nacki_interface::ExtendedAddress;
use serde::{Deserialize, Serialize};

use crate::{error::RelayerError, submitter::AnSubmitConfig};

fn default_confirm_timeout_secs() -> u64 {
    120
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnConfig {
    #[serde(default)]
    pub graphql_url: String,
    #[serde(default)]
    pub keys_path: String,
    #[serde(default)]
    pub light_client_abi_path: String,
    #[serde(default)]
    pub light_client: String,
    #[serde(default)]
    pub sender: String,
    #[serde(default = "default_confirm_timeout_secs")]
    pub confirm_timeout_secs: u64,
}

impl Default for AnConfig {
    fn default() -> Self {
        Self {
            graphql_url: String::new(),
            keys_path: String::new(),
            light_client_abi_path: String::new(),
            light_client: String::new(),
            sender: String::new(),
            confirm_timeout_secs: default_confirm_timeout_secs(),
        }
    }
}

impl AnConfig {
    pub fn is_live_submit_ready(&self) -> bool {
        !self.graphql_url.is_empty()
            && !self.keys_path.is_empty()
            && !self.light_client_abi_path.is_empty()
            && !self.light_client.is_empty()
            && !self.sender.is_empty()
    }

    pub fn to_submit_config(&self) -> AnSubmitConfig {
        AnSubmitConfig {
            from: self.sender.clone(),
            light_client: self.light_client.clone(),
            confirm_timeout_secs: self.confirm_timeout_secs,
        }
    }

    pub fn sender_address(&self) -> Result<ExtendedAddress, RelayerError> {
        ExtendedAddress::parse(&self.sender).map_err(RelayerError::from)
    }

    pub fn validate_live_graphql_endpoint(&self, allow_insecure: bool) -> Result<(), RelayerError> {
        if self.graphql_url.is_empty() {
            return Ok(());
        }
        let lower = self.graphql_url.to_ascii_lowercase();
        if lower.starts_with("https://") {
            return Ok(());
        }
        let loopback = lower.contains("127.0.0.1") || lower.contains("localhost");
        if loopback || allow_insecure {
            return Ok(());
        }
        Err(RelayerError::other(
            "AN GraphQL must be HTTPS (or loopback / --allow-insecure-graphql)",
        ))
    }
}
