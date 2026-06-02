//! Live Acki Nacki BK-set HTTP client.
//!
//! Hits the AN node's stable REST surface (`/v2/bk_set`, `/v2/bk_set_update`)
//! to fetch the **current** and **future** Block Keeper sets. Probed against
//! the public test node `http://94.156.178.19:8600` on 2026-05-18 — see
//! `tests/live_bk_set.rs` for the gated live-network integration test.
//!
//! This is the first real (non-mock) AN node integration in the bridge.
//! Higher-level pieces (relayer, Circuit 1B / Circuit 3 witness assembly,
//! Poseidon BK-set commitment computation) layer on top.
//!
//! ## Endpoint contracts (probed 2026-05-18)
//!
//! ### `GET /v2/bk_set`
//!
//! Compact view, suitable for "who's currently signing":
//!
//! ```json
//! {
//!   "bk_set": [
//!     { "node_id": "<32-byte hex>",
//!       "node_owner_pk": "<32-byte hex>",
//!       "epoch_start_seq_no": 55692804 }
//!   ],
//!   "future_bk_set": [],
//!   "seq_no": 55692366
//! }
//! ```
//!
//! ### `GET /v2/bk_set_update`
//!
//! Full per-node view, with the **BLS12-381 G1 compressed pubkey** required
//! for Circuit 1B's attestation verification and the `signer_index → pubkey`
//! map the BK-set Poseidon commitment is computed over:
//!
//! ```json
//! {
//!   "seq_no": 55692395,
//!   "current": [{
//!     "pubkey": "<48-byte hex; BLS12-381 G1 compressed>",
//!     "epoch_finish_seq_no": 55692804,
//!     "wait_step": 180,
//!     "status": "Active",
//!     "address": "0:<TVM address hex>",
//!     "stake": "<u128 as string>",
//!     "owner_address": "<32-byte hex; matches node_id>",
//!     "signer_index": 21033,
//!     "owner_pubkey": "<32-byte hex; matches node_owner_pk>",
//!     "protocol_version_support": "<...>"
//!   }],
//!   "future": []
//! }
//! ```
//!
//! ## What's NOT here yet
//!
//! - GraphQL queries for historical layer-hash siblings (Misha's `poseidon_dex`
//!   branch addition; still being validated by Alina). Will live in a separate
//!   `graphql_client.rs` module once the schema stabilises.
//! - Block-level queries (`block_by_height`, etc.) — `/v2/` REST surface is
//!   intentionally narrow on the test node; full coverage needs the GraphQL
//!   endpoint or a private RPC.

use std::{collections::HashMap, time::Duration};

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::{AckiNackiError, Result};

const DEFAULT_USER_AGENT: &str = concat!("acki-nacki-bridge/", env!("CARGO_PKG_VERSION"));

/// BLS12-381 G1 compressed pubkey length, in bytes (BK signing key).
pub const BLS_PUBKEY_LEN: usize = 48;

/// 32-byte identity (`node_id` / `node_owner_pk` / `owner_address`).
pub const ID32_LEN: usize = 32;

/// Live REST client for `/v2/bk_set` and `/v2/bk_set_update`.
///
/// Cheap to clone — wraps a `reqwest::Client` (itself an `Arc`-backed handle)
/// and an immutable base URL. Safe to share across tasks.
#[derive(Clone, Debug)]
pub struct BkSetClient {
    http: reqwest::Client,
    base_url: String,
}

impl BkSetClient {
    /// Construct a client pinned to a base URL such as
    /// `http://94.156.178.19:8600` (no trailing slash; if you pass one it's
    /// stripped). Uses a 10-second total request timeout — enough for the
    /// public test node's worst observed latency, short enough that a stalled
    /// relayer loop notices quickly.
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into();
        let base_url = base_url.trim_end_matches('/').to_string();

        let http = reqwest::Client::builder()
            .user_agent(DEFAULT_USER_AGENT)
            .timeout(Duration::from_secs(10))
            .build()?;

        Ok(Self { http, base_url })
    }

    /// `GET /v2/bk_set` — compact BK set view.
    pub async fn fetch_bk_set(&self) -> Result<BkSetResponse> {
        self.get_json("/v2/bk_set").await
    }

    /// `GET /v2/bk_set_update` — full per-node view with BLS pubkeys.
    pub async fn fetch_bk_set_update(&self) -> Result<BkSetUpdateResponse> {
        self.get_json("/v2/bk_set_update").await
    }

    /// Convenience helper that fetches `/v2/bk_set_update` and converts the
    /// `current` array into the `signer_index → 48-byte BLS pubkey` map
    /// expected by `bridge-prover-orchestrator::generate_fallback_proof`
    /// and `compute_bk_set_poseidon`.
    ///
    /// Returns an error if any `pubkey` hex doesn't decode to exactly 48 bytes,
    /// or if two BK share the same `signer_index` (the node should never emit
    /// such a payload — bridge would reject the proof anyway, but we surface
    /// it early).
    pub async fn fetch_signer_index_bk_set(&self) -> Result<HashMap<u32, Vec<u8>>> {
        let update = self.fetch_bk_set_update().await?;
        let mut out = HashMap::with_capacity(update.current.len());
        for entry in &update.current {
            let pubkey = decode_hex_fixed(
                &entry.pubkey,
                "bk_set_update.current[].pubkey",
                BLS_PUBKEY_LEN,
            )?;
            if out.insert(entry.signer_index, pubkey).is_some() {
                return Err(AckiNackiError::InvalidTransaction(format!(
                    "duplicate signer_index {} in /v2/bk_set_update.current",
                    entry.signer_index,
                )));
            }
        }
        debug!(
            bk_count = out.len(),
            seq_no = update.seq_no,
            "fetched signer-index-keyed BK set"
        );
        Ok(out)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        debug!(url = %url, "GET");
        let resp = self.http.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AckiNackiError::Http(format!(
                "{} returned HTTP {}",
                url, status
            )));
        }
        let body = resp.text().await?;
        let parsed: T = serde_json::from_str(&body).map_err(|e| {
            AckiNackiError::JsonParse(format!(
                "{}: {} (first 200 chars of body: {:?})",
                url,
                e,
                &body.chars().take(200).collect::<String>(),
            ))
        })?;
        Ok(parsed)
    }
}

// ---------- `/v2/bk_set` schema ----------

/// Response shape of `GET /v2/bk_set`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BkSetResponse {
    /// Snapshot seq_no the BK set is valid at (i.e. last processed block).
    pub seq_no: u64,
    /// Currently-active Block Keepers.
    pub bk_set: Vec<BkEntry>,
    /// Block Keepers whose epoch hasn't started yet (look-ahead window).
    pub future_bk_set: Vec<BkEntry>,
}

/// Single entry of `/v2/bk_set`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BkEntry {
    /// 32-byte node identity (hex-encoded).
    pub node_id: String,
    /// 32-byte node owner pubkey (hex-encoded). Matches
    /// `bk_set_update.owner_pubkey`.
    pub node_owner_pk: String,
    /// First seq_no this BK is part of the active committee.
    pub epoch_start_seq_no: u64,
}

// ---------- `/v2/bk_set_update` schema ----------

/// Response shape of `GET /v2/bk_set_update`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BkSetUpdateResponse {
    /// Snapshot seq_no the update is valid at.
    pub seq_no: u64,
    /// Currently-active Block Keepers (richer view than `/v2/bk_set`).
    pub current: Vec<BkUpdateEntry>,
    /// Future BK (epoch hasn't started).
    pub future: Vec<BkUpdateEntry>,
}

/// Single entry of `/v2/bk_set_update`.
///
/// Carries the **BLS12-381 G1 compressed pubkey** that Circuit 1B
/// (Fallback attestation) and Circuit 3 (BK-set rotation) verify against,
/// plus the `signer_index` keying the bridge's `bk_set` map.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BkUpdateEntry {
    /// 48-byte BLS12-381 G1 compressed pubkey (hex). Used for attestation
    /// signature verification.
    pub pubkey: String,
    /// Seq_no at which this BK's current epoch ends.
    pub epoch_finish_seq_no: u64,
    /// Slot-window wait step (config knob; not used by the bridge).
    pub wait_step: u64,
    /// BK status (e.g. `"Active"`).
    pub status: String,
    /// TVM address of the BK contract (`"0:<hex>"`).
    pub address: String,
    /// Stake as a base-10 stringified `u128` (json-safe encoding).
    pub stake: String,
    /// 32-byte node identity (hex). Matches `bk_set.node_id`.
    pub owner_address: String,
    /// Stable identifier the orchestrator's `bk_set: HashMap<idx, pubkey>` is
    /// keyed by. NOT a sequence number — these are sparse u32 indices assigned
    /// at BK enrollment and never reused.
    pub signer_index: u32,
    /// 32-byte node owner pubkey (hex). Matches `bk_set.node_owner_pk`.
    pub owner_pubkey: String,
    /// Protocol version transition support string. Opaque to the bridge.
    pub protocol_version_support: String,
}

fn decode_hex_fixed(s: &str, field: &'static str, expected: usize) -> Result<Vec<u8>> {
    let bytes = hex::decode(s)
        .map_err(|e| AckiNackiError::JsonParse(format!("hex-decode failed for {field}: {e}")))?;
    if bytes.len() != expected {
        return Err(AckiNackiError::InvalidLength {
            field,
            expected,
            actual: bytes.len(),
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    const SAMPLE_BK_SET: &str = r#"{
        "bk_set": [
            {
                "node_id": "991b891fd9e5db10461e01d385b01312bf4ec5bb78bd2540165bc22cd0674c5c",
                "node_owner_pk": "55a1e8693a0beee3d8c3905b63ca01abf9d640af45ad937940b666f9e0946e33",
                "epoch_start_seq_no": 55692804
            }
        ],
        "future_bk_set": [],
        "seq_no": 55692366
    }"#;

    const SAMPLE_BK_SET_UPDATE: &str = r#"{
        "seq_no": 55692395,
        "current": [
            {
                "pubkey": "a12892e1cc80b331074731cb43d749f1e1cae0c4475febc525c73a8df47f16eb122e88968f2fa8749fb451359e4f32fd",
                "epoch_finish_seq_no": 55692804,
                "wait_step": 180,
                "status": "Active",
                "address": "0:3a2cfb83d680e43eb966bd69d0f942073aab571ef4ed0afed4c5a4bc37fe5f77",
                "stake": "90481439095387",
                "owner_address": "991b891fd9e5db10461e01d385b01312bf4ec5bb78bd2540165bc22cd0674c5c",
                "signer_index": 21033,
                "owner_pubkey": "55a1e8693a0beee3d8c3905b63ca01abf9d640af45ad937940b666f9e0946e33",
                "protocol_version_support": "x_1.0.3_0->x_1.0.4_0"
            }
        ],
        "future": []
    }"#;

    #[test]
    fn parses_bk_set_response() {
        let r: BkSetResponse = serde_json::from_str(SAMPLE_BK_SET).unwrap();
        assert_eq!(r.seq_no, 55692366);
        assert_eq!(r.bk_set.len(), 1);
        assert_eq!(r.bk_set[0].epoch_start_seq_no, 55692804);
        assert!(r.future_bk_set.is_empty());
    }

    #[test]
    fn parses_bk_set_update_response() {
        let r: BkSetUpdateResponse = serde_json::from_str(SAMPLE_BK_SET_UPDATE).unwrap();
        assert_eq!(r.seq_no, 55692395);
        assert_eq!(r.current.len(), 1);
        let e = &r.current[0];
        assert_eq!(e.signer_index, 21033);
        assert_eq!(e.status, "Active");
        assert_eq!(e.pubkey.len(), BLS_PUBKEY_LEN * 2); // hex chars
        let pk_bytes = hex::decode(&e.pubkey).unwrap();
        assert_eq!(pk_bytes.len(), BLS_PUBKEY_LEN);
    }

    #[test]
    fn decode_hex_fixed_rejects_wrong_length() {
        let err = decode_hex_fixed("aa", "test_field", 32).unwrap_err();
        match err {
            AckiNackiError::InvalidLength {
                field,
                expected,
                actual,
            } => {
                assert_eq!(field, "test_field");
                assert_eq!(expected, 32);
                assert_eq!(actual, 1);
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn decode_hex_fixed_rejects_non_hex() {
        let err = decode_hex_fixed("not-hex", "f", 4).unwrap_err();
        assert!(matches!(err, AckiNackiError::JsonParse(_)));
    }

    #[test]
    fn client_strips_trailing_slash() {
        let c = BkSetClient::new("http://example.com:8600/").unwrap();
        assert_eq!(c.base_url, "http://example.com:8600");
    }
}
