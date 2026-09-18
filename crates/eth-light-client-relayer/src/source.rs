//! Beacon REST source (`/eth/v1/beacon/light_client/finality_update`).
//!
//! Also resolves the network's signing domain once per process
//! (`/eth/v1/beacon/genesis` + the fork schedule from `/eth/v1/config/spec`)
//! so the prover gets the `fork_version` that was active at each update's
//! `signature_slot` instead of a compiled-in mainnet constant.

use std::sync::Mutex;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

use crate::{
    ancestry::ExecLink,
    error::RelayerError,
    types::{parse_finality_update, BeaconChainParams, FinalityUpdate},
};

#[async_trait]
pub trait BeaconSource: Send + Sync {
    async fn fetch_finality(&self) -> Result<FinalityUpdate, RelayerError>;
}

/// Execution parent-hash walk used after an accepted checkpoint.
#[async_trait]
pub trait ExecutionSource: Send + Sync {
    async fn ancestry_headers(
        &self,
        checkpoint: [u8; 32],
        max: usize,
    ) -> Result<Vec<Vec<u8>>, RelayerError>;
}

/// In-memory epoch chain for tests (`Relayer::with_execution`).
pub struct InMemoryExecution {
    chains: Mutex<std::collections::HashMap<[u8; 32], Vec<Vec<u8>>>>,
}

impl InMemoryExecution {
    pub fn single(checkpoint: [u8; 32], headers: Vec<Vec<u8>>) -> Self {
        let mut chains = std::collections::HashMap::new();
        chains.insert(checkpoint, headers);
        Self {
            chains: Mutex::new(chains),
        }
    }
}

#[async_trait]
impl ExecutionSource for InMemoryExecution {
    async fn ancestry_headers(
        &self,
        checkpoint: [u8; 32],
        max: usize,
    ) -> Result<Vec<Vec<u8>>, RelayerError> {
        let chains = self.chains.lock().expect("poisoned");
        let headers = chains
            .get(&checkpoint)
            .cloned()
            .ok_or_else(|| RelayerError::other("in-memory execution: unknown checkpoint"))?;
        let max = max.clamp(2, 32);
        Ok(headers.into_iter().take(max).collect())
    }
}

/// Network constants that never change for a beacon node: cached after the
/// first successful fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainSpec {
    pub genesis_validators_root: [u8; 32],
    /// `(activation_epoch, fork_version)` sorted by epoch; unscheduled forks
    /// (`FAR_FUTURE_EPOCH`) are dropped.
    pub fork_schedule: Vec<(u64, [u8; 4])>,
    pub slots_per_epoch: u64,
}

impl ChainSpec {
    /// Fork version active at `slot`.
    pub fn fork_version_at_slot(&self, slot: u64) -> Result<[u8; 4], RelayerError> {
        let epoch = slot / self.slots_per_epoch.max(1);
        fork_version_at_epoch(&self.fork_schedule, epoch).ok_or_else(|| {
            RelayerError::beacon(format!("no fork version scheduled at epoch {epoch}"))
        })
    }

    pub fn params_at_slot(&self, slot: u64) -> Result<BeaconChainParams, RelayerError> {
        Ok(BeaconChainParams {
            fork_version: self.fork_version_at_slot(slot)?,
            genesis_validators_root: self.genesis_validators_root,
        })
    }
}

/// `GET /eth/v1/beacon/genesis` → `data.genesis_validators_root`.
pub fn parse_genesis_validators_root(json: &str) -> Result<[u8; 32], RelayerError> {
    let v: Value = serde_json::from_str(json)
        .map_err(|e| RelayerError::beacon(format!("genesis JSON: {e}")))?;
    let s = v["data"]["genesis_validators_root"]
        .as_str()
        .ok_or_else(|| RelayerError::beacon("missing data.genesis_validators_root"))?;
    let raw = hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| RelayerError::beacon(format!("genesis_validators_root hex: {e}")))?;
    raw.try_into()
        .map_err(|_| RelayerError::beacon("genesis_validators_root must be 32 bytes"))
}

/// `GET /eth/v1/config/spec` → fork schedule. Every `<NAME>_FORK_VERSION` is
/// paired with `<NAME>_FORK_EPOCH` (`GENESIS_FORK_VERSION` activates at epoch
/// 0). Versions without an epoch, or with `FAR_FUTURE_EPOCH`, are skipped.
pub fn parse_fork_schedule(spec_json: &str) -> Result<Vec<(u64, [u8; 4])>, RelayerError> {
    let v: Value = serde_json::from_str(spec_json)
        .map_err(|e| RelayerError::beacon(format!("spec JSON: {e}")))?;
    let data = v["data"]
        .as_object()
        .ok_or_else(|| RelayerError::beacon("spec: missing data object"))?;
    let mut out = Vec::new();
    for (key, val) in data {
        let Some(name) = key.strip_suffix("_FORK_VERSION") else {
            continue;
        };
        let version_hex = val
            .as_str()
            .ok_or_else(|| RelayerError::beacon(format!("spec: {key} is not a string")))?;
        let raw = hex::decode(version_hex.trim_start_matches("0x"))
            .map_err(|e| RelayerError::beacon(format!("spec: {key} hex: {e}")))?;
        let version: [u8; 4] = raw
            .try_into()
            .map_err(|_| RelayerError::beacon(format!("spec: {key} must be 4 bytes")))?;
        let epoch = if name == "GENESIS" {
            0
        } else {
            let Some(e) = data.get(&format!("{name}_FORK_EPOCH")) else {
                continue;
            };
            let e = match e {
                Value::String(s) => s.parse::<u64>().ok(),
                Value::Number(n) => n.as_u64(),
                _ => None,
            };
            match e {
                Some(e) if e != u64::MAX => e,
                _ => continue,
            }
        };
        out.push((epoch, version));
    }
    if out.is_empty() {
        return Err(RelayerError::beacon("spec: no *_FORK_VERSION entries"));
    }
    out.sort();
    Ok(out)
}

/// Latest scheduled fork whose activation epoch is `<= epoch`.
pub fn fork_version_at_epoch(schedule: &[(u64, [u8; 4])], epoch: u64) -> Option<[u8; 4]> {
    schedule
        .iter()
        .filter(|(e, _)| *e <= epoch)
        .max_by_key(|(e, _)| *e)
        .map(|(_, v)| *v)
}

fn parse_slots_per_epoch(spec_json: &str) -> u64 {
    serde_json::from_str::<Value>(spec_json)
        .ok()
        .and_then(|v| {
            v["data"]["SLOTS_PER_EPOCH"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| v["data"]["SLOTS_PER_EPOCH"].as_u64())
        })
        .unwrap_or(32)
}

pub struct HttpBeaconSource {
    client: Client,
    base_url: String,
    spec: Mutex<Option<ChainSpec>>,
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
            spec: Mutex::new(None),
        })
    }

    async fn get_text(&self, url: &str) -> Result<String, RelayerError> {
        let resp = self
            .client
            .get(url)
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

    /// Genesis root + fork schedule of the connected network (cached).
    pub async fn chain_spec(&self) -> Result<ChainSpec, RelayerError> {
        if let Some(s) = self.spec.lock().expect("poisoned").as_ref() {
            return Ok(s.clone());
        }
        let genesis = self
            .get_text(&format!("{}/eth/v1/beacon/genesis", self.base_url))
            .await?;
        let spec = self
            .get_text(&format!("{}/eth/v1/config/spec", self.base_url))
            .await?;
        let resolved = ChainSpec {
            genesis_validators_root: parse_genesis_validators_root(&genesis)?,
            fork_schedule: parse_fork_schedule(&spec)?,
            slots_per_epoch: parse_slots_per_epoch(&spec),
        };
        *self.spec.lock().expect("poisoned") = Some(resolved.clone());
        Ok(resolved)
    }

    /// Signing domain for an update signed at `signature_slot`.
    pub async fn chain_params_at(
        &self,
        signature_slot: u64,
    ) -> Result<BeaconChainParams, RelayerError> {
        self.chain_spec().await?.params_at_slot(signature_slot)
    }
}

#[async_trait]
impl BeaconSource for HttpBeaconSource {
    async fn fetch_finality(&self) -> Result<FinalityUpdate, RelayerError> {
        let url = format!(
            "{}/eth/v1/beacon/light_client/finality_update",
            self.base_url
        );
        let body = self.get_text(&url).await?;
        let mut update = parse_finality_update(&body)?;
        let prev = update.period().saturating_sub(1);
        update.committee_json = self.fetch_period_update(prev).await?;
        update.chain = Some(self.chain_params_at(update.signature_slot).await?);
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

/// Execution-layer JSON-RPC (`eth_getBlockByHash`) for header RLP.
pub struct EthExecutionRpc {
    client: Client,
    url: String,
}

impl EthExecutionRpc {
    pub fn new(url: impl Into<String>) -> Result<Self, RelayerError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| RelayerError::beacon(e.to_string()))?;
        Ok(Self {
            client,
            url: url.into(),
        })
    }

    pub async fn header_rlp(&self, block_hash: [u8; 32]) -> Result<Vec<u8>, RelayerError> {
        let hx = format!("0x{}", hex::encode(block_hash));
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_getBlockByHash",
            "params": [hx, false]
        });
        let resp = self
            .client
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| RelayerError::beacon(format!("eth_getBlockByHash: {e}")))?;
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| RelayerError::beacon(format!("eth_getBlockByHash json: {e}")))?;
        let result = v
            .get("result")
            .ok_or_else(|| RelayerError::beacon("eth_getBlockByHash missing result"))?;
        if result.is_null() {
            return Err(RelayerError::beacon(format!("unknown block {hx}")));
        }
        crate::header_rlp::encode_header_rlp(result)
    }

    /// Checkpoint header first, then parents, ≤ `max` (≤ 32).
    pub async fn ancestry_headers(
        &self,
        checkpoint: [u8; 32],
        max: usize,
    ) -> Result<Vec<Vec<u8>>, RelayerError> {
        let max = max.clamp(2, 32);
        let mut headers = Vec::new();
        let mut h = checkpoint;
        for _ in 0..max {
            let rlp = self.header_rlp(h).await?;
            let got = crate::header_rlp::keccak256(&rlp);
            if got != h {
                return Err(RelayerError::other(
                    "eth_getBlockByHash RLP does not keccak to the requested hash",
                ));
            }
            let parent = crate::header_rlp::rlp_parent_hash(&rlp)?;
            headers.push(rlp);
            if headers.len() >= max {
                break;
            }
            h = parent;
        }
        Ok(headers)
    }
}

#[async_trait]
impl ExecutionSource for EthExecutionRpc {
    async fn ancestry_headers(
        &self,
        checkpoint: [u8; 32],
        max: usize,
    ) -> Result<Vec<Vec<u8>>, RelayerError> {
        EthExecutionRpc::ancestry_headers(self, checkpoint, max).await
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

    // Trimmed `GET /eth/v1/config/spec` of Sepolia (2026-09-04): Fulu live,
    // Gloas/Heze unscheduled (FAR_FUTURE_EPOCH).
    const SEPOLIA_SPEC: &str = r#"{"data":{
      "GENESIS_FORK_VERSION":"0x90000069",
      "ALTAIR_FORK_VERSION":"0x90000070","ALTAIR_FORK_EPOCH":"50",
      "BELLATRIX_FORK_VERSION":"0x90000071","BELLATRIX_FORK_EPOCH":"100",
      "CAPELLA_FORK_VERSION":"0x90000072","CAPELLA_FORK_EPOCH":"56832",
      "DENEB_FORK_VERSION":"0x90000073","DENEB_FORK_EPOCH":"132608",
      "ELECTRA_FORK_VERSION":"0x90000074","ELECTRA_FORK_EPOCH":"222464",
      "FULU_FORK_VERSION":"0x90000075","FULU_FORK_EPOCH":"272640",
      "GLOAS_FORK_VERSION":"0x90000076","GLOAS_FORK_EPOCH":"18446744073709551615",
      "HEZE_FORK_VERSION":"0x08000000","HEZE_FORK_EPOCH":"18446744073709551615",
      "SLOTS_PER_EPOCH":"32","SYNC_COMMITTEE_SIZE":"512"
    }}"#;

    #[test]
    fn fork_schedule_drops_unscheduled_and_sorts() {
        let s = parse_fork_schedule(SEPOLIA_SPEC).unwrap();
        assert_eq!(s.len(), 7);
        assert_eq!(s[0], (0, [0x90, 0, 0, 0x69]));
        assert_eq!(s[6], (272640, [0x90, 0, 0, 0x75]));
        assert!(s.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn fork_version_picks_latest_activated() {
        let s = parse_fork_schedule(SEPOLIA_SPEC).unwrap();
        assert_eq!(fork_version_at_epoch(&s, 0), Some([0x90, 0, 0, 0x69]));
        assert_eq!(fork_version_at_epoch(&s, 49), Some([0x90, 0, 0, 0x69]));
        assert_eq!(fork_version_at_epoch(&s, 50), Some([0x90, 0, 0, 0x70]));
        assert_eq!(fork_version_at_epoch(&s, 272639), Some([0x90, 0, 0, 0x74]));
        assert_eq!(fork_version_at_epoch(&s, 272640), Some([0x90, 0, 0, 0x75]));
        assert_eq!(
            fork_version_at_epoch(&s, u64::MAX - 1),
            Some([0x90, 0, 0, 0x75])
        );
    }

    #[test]
    fn chain_spec_resolves_params_at_signature_slot() {
        let spec = ChainSpec {
            genesis_validators_root: [0xd8; 32],
            fork_schedule: parse_fork_schedule(SEPOLIA_SPEC).unwrap(),
            slots_per_epoch: parse_slots_per_epoch(SEPOLIA_SPEC),
        };
        assert_eq!(spec.slots_per_epoch, 32);
        // Sepolia slot 11_065_463 → epoch 345_795 → Fulu.
        let p = spec.params_at_slot(11_065_463).unwrap();
        assert_eq!(p.fork_version_hex(), "0x90000075");
        assert_eq!(p.genesis_validators_root, [0xd8; 32]);
        // Last Electra slot: 272640*32 - 1.
        let p = spec.params_at_slot(272_640 * 32 - 1).unwrap();
        assert_eq!(p.fork_version_hex(), "0x90000074");
    }

    #[test]
    fn fork_schedule_rejects_spec_without_versions() {
        assert!(parse_fork_schedule(r#"{"data":{"SLOTS_PER_EPOCH":"32"}}"#).is_err());
        assert!(parse_fork_schedule(r#"{"data":{"X_FORK_VERSION":"0x0102"}}"#).is_err());
    }

    #[test]
    fn genesis_root_parses() {
        let j = r#"{"data":{"genesis_validators_root":"0xd8ea171f3c94aea21ebc42a1ed61052acf3f9209c00e4efbaaddac09ed9b8078","genesis_time":"1655733600"}}"#;
        let g = parse_genesis_validators_root(j).unwrap();
        assert_eq!(g[0], 0xd8);
        assert_eq!(g[31], 0x78);
        assert!(parse_genesis_validators_root(r#"{"data":{}}"#).is_err());
    }
}
