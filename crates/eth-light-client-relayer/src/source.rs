//! Beacon REST source (`/eth/v1/beacon/light_client/finality_update`).

use std::sync::Mutex;

use async_trait::async_trait;
use reqwest::Client;

use crate::{
    error::RelayerError,
    types::{parse_finality_update, FinalityUpdate},
};

#[async_trait]
pub trait BeaconSource: Send + Sync {
    async fn fetch_finality(&self) -> Result<FinalityUpdate, RelayerError>;
}

pub struct HttpBeaconSource {
    client: Client,
    base_url: String,
}

impl HttpBeaconSource {
    pub fn new(base_url: impl Into<String>) -> Result<Self, RelayerError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| RelayerError::beacon(e.to_string()))?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        })
    }
}

#[async_trait]
impl BeaconSource for HttpBeaconSource {
    async fn fetch_finality(&self) -> Result<FinalityUpdate, RelayerError> {
        let url = format!(
            "{}/eth/v1/beacon/light_client/finality_update",
            self.base_url
        );
        let resp = self
            .client
            .get(&url)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| RelayerError::beacon(format!("GET {url}: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| RelayerError::beacon(format!("read {url}: {e}")))?;
        if !status.is_success() {
            return Err(RelayerError::beacon(format!(
                "GET {url} -> {status}: {}",
                body.chars().take(200).collect::<String>()
            )));
        }
        parse_finality_update(&body)
    }
}

/// Test source: pop from a queue of updates.
pub struct InMemoryBeaconSource {
    inner: Mutex<Vec<FinalityUpdate>>,
}

impl InMemoryBeaconSource {
    pub fn new(updates: Vec<FinalityUpdate>) -> Self {
        Self {
            inner: Mutex::new(updates),
        }
    }

    pub fn push(&self, update: FinalityUpdate) {
        self.inner.lock().expect("poisoned").push(update);
    }
}

#[async_trait]
impl BeaconSource for InMemoryBeaconSource {
    async fn fetch_finality(&self) -> Result<FinalityUpdate, RelayerError> {
        let mut q = self.inner.lock().expect("poisoned");
        if q.is_empty() {
            return Err(RelayerError::beacon("in-memory beacon exhausted"));
        }
        if q.len() == 1 {
            return Ok(q[0].clone());
        }
        Ok(q.remove(0))
    }
}
