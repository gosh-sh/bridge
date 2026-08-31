//! Beacon REST source (`/eth/v1/beacon/light_client/finality_update`).

use std::sync::Mutex;

use async_trait::async_trait;
use reqwest::Client;

use crate::{
    ancestry::ExecLink,
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
        let mut update = parse_finality_update(&body)?;
        let prev = update.period().saturating_sub(1);
        update.committee_json = self.fetch_period_update(prev).await?;
        Ok(update)
    }
}

impl HttpBeaconSource {
    /// `GET /eth/v1/beacon/light_client/updates?start_period=P&count=1`
    /// (`next_sync_committee` of period P is the **current** committee of P+1).
    pub async fn fetch_period_update(&self, period: u64) -> Result<String, RelayerError> {
        let url = format!(
            "{}/eth/v1/beacon/light_client/updates?start_period={period}&count=1",
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
        Ok(body)
    }

    /// `GET /eth/v2/beacon/blocks/{slot}` → execution `block_hash` /
    /// `parent_hash`.
    pub async fn fetch_exec_link_at_slot(&self, slot: u64) -> Result<ExecLink, RelayerError> {
        let url = format!("{}/eth/v2/beacon/blocks/{slot}", self.base_url);
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
        parse_exec_link(&body)
    }
}

fn hex32_exec(s: &str) -> Result<[u8; 32], RelayerError> {
    let raw = hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| RelayerError::beacon(format!("hex: {e}")))?;
    raw.try_into()
        .map_err(|_| RelayerError::beacon("expected 32-byte hash"))
}

fn parse_exec_link(json: &str) -> Result<ExecLink, RelayerError> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| RelayerError::beacon(format!("block JSON: {e}")))?;
    let payload = &v["data"]["message"]["body"]["execution_payload"];
    let block_hash = hex32_exec(
        payload["block_hash"]
            .as_str()
            .ok_or_else(|| RelayerError::beacon("missing execution_payload.block_hash"))?,
    )?;
    let parent_hash = hex32_exec(
        payload["parent_hash"]
            .as_str()
            .ok_or_else(|| RelayerError::beacon("missing execution_payload.parent_hash"))?,
    )?;
    Ok(ExecLink {
        block_hash,
        parent_hash,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_exec_link_reads_payload_hashes() {
        let json = r#"{
          "data":{"message":{"body":{"execution_payload":{
            "block_hash":"0x1111111111111111111111111111111111111111111111111111111111111111",
            "parent_hash":"0x2222222222222222222222222222222222222222222222222222222222222222"
          }}}}
        }"#;
        let link = parse_exec_link(json).unwrap();
        assert_eq!(link.block_hash, [0x11; 32]);
        assert_eq!(link.parent_hash, [0x22; 32]);
    }

    #[test]
    fn parse_exec_link_rejects_missing_payload() {
        assert!(parse_exec_link(r#"{"data":{}}"#).is_err());
    }
}
