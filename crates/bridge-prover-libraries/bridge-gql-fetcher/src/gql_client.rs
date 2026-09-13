use std::{
    collections::BTreeMap,
    sync::Once,
    time::{Duration, Instant},
};

use anyhow::{bail, Context};
/// Canonical block-id Merkle leaf count (protocol-fixed = 16). Sourced from
/// the circuits repo so the GraphQL projection stays in lock-step with the
/// circuit witness layout — no local `= 16` literal to drift out of sync.
pub use bridge_test_data_gen::layer_hashes::BLOCK_ID_TREE_LEAF_COUNT;
use metrics::{counter, describe_counter, describe_histogram, histogram};
use serde_json::{json, Value};
use tracing::{error, warn};

use crate::types::{AccountRouting, ThreadIdentifier};

/// Largest height accepted by the live GraphQL schema. Although Rust-side
/// block heights are `u64`, GraphQL exposes `height_end` as a signed `Int`.
pub const GRAPHQL_SIGNED_INT_MAX: u64 = i64::MAX as u64;

/// Lightweight GraphQL client for the acki-nacki node.
///
/// Holds an ordered endpoint list: `[0]` is the primary (the historical
/// single `BRIDGE_GQL_ENDPOINT`), the rest are failover targets. Every
/// request starts at the primary, retries it `retries_per_endpoint` times
/// `retry_delay` apart, then moves to the next endpoint, and cycles through
/// the whole list until one attempt succeeds (or `max_rounds` is reached).
/// There is no stickiness: the next request starts from the primary again.
///
/// Every failure counts the same way — transport error, timeout, non-2xx
/// status, undecodable body, a GraphQL `errors` array, or a `null` where the
/// caller declared a required object (see [`GqlClient::query_op`]). The daemon
/// cannot make progress without the data anyway, so a request that keeps
/// failing is meant to be caught by the `relayer_gql_*` metrics, not by an
/// error return.
pub struct GqlClient {
    http: reqwest::Client,
    endpoints: Vec<String>,
    cfg: GqlClientConfig,
}

/// Default whole-request timeout (connect + send + response body).
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Default TCP connect timeout. Keeps a black-holed endpoint from eating the
/// full request timeout on every attempt.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default attempts per endpoint before failing over to the next one.
pub const DEFAULT_RETRIES_PER_ENDPOINT: u32 = 3;
/// Default sleep between attempts and between endpoint switches.
pub const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Environment overrides consumed by [`GqlClientConfig::from_env`].
pub const ENV_REQUEST_TIMEOUT_SECS: &str = "BRIDGE_GQL_REQUEST_TIMEOUT_SECS";
pub const ENV_CONNECT_TIMEOUT_SECS: &str = "BRIDGE_GQL_CONNECT_TIMEOUT_SECS";
pub const ENV_RETRIES_PER_ENDPOINT: &str = "BRIDGE_GQL_RETRIES_PER_ENDPOINT";
pub const ENV_RETRY_DELAY_MS: &str = "BRIDGE_GQL_RETRY_DELAY_MS";
pub const ENV_MAX_ROUNDS: &str = "BRIDGE_GQL_MAX_ROUNDS";

/// Retry / failover policy and HTTP timeouts for [`GqlClient`].
#[derive(Debug, Clone)]
pub struct GqlClientConfig {
    /// Whole-request timeout (connect + send + response body).
    pub request_timeout: Duration,
    /// TCP connect timeout.
    pub connect_timeout: Duration,
    /// Attempts per endpoint before failing over to the next one. Minimum 1.
    pub retries_per_endpoint: u32,
    /// Sleep between attempts and between endpoint switches.
    pub retry_delay: Duration,
    /// How many full passes over the endpoint list to make before giving
    /// up. `None` = loop forever (the daemon default).
    pub max_rounds: Option<u32>,
}

impl Default for GqlClientConfig {
    fn default() -> Self {
        Self {
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            retries_per_endpoint: DEFAULT_RETRIES_PER_ENDPOINT,
            retry_delay: DEFAULT_RETRY_DELAY,
            max_rounds: None,
        }
    }
}

impl GqlClientConfig {
    /// One attempt, no retry, error returned to the caller — the behaviour
    /// [`create_client`] always had. Used by one-shot tools and the CLI,
    /// where hanging on a bad endpoint is worse than failing.
    pub fn single_attempt() -> Self {
        Self {
            retries_per_endpoint: 1,
            max_rounds: Some(1),
            ..Self::default()
        }
    }

    /// Defaults overridden by `BRIDGE_GQL_REQUEST_TIMEOUT_SECS`,
    /// `BRIDGE_GQL_CONNECT_TIMEOUT_SECS`, `BRIDGE_GQL_RETRIES_PER_ENDPOINT`,
    /// `BRIDGE_GQL_RETRY_DELAY_MS` and `BRIDGE_GQL_MAX_ROUNDS` (`0` or unset
    /// = loop forever) when they are set and non-empty.
    pub fn from_env() -> anyhow::Result<Self> {
        let mut cfg = Self::default();
        if let Some(secs) = env_parse::<u64>(ENV_REQUEST_TIMEOUT_SECS)? {
            cfg.request_timeout = Duration::from_secs(secs);
        }
        if let Some(secs) = env_parse::<u64>(ENV_CONNECT_TIMEOUT_SECS)? {
            cfg.connect_timeout = Duration::from_secs(secs);
        }
        if let Some(n) = env_parse::<u32>(ENV_RETRIES_PER_ENDPOINT)? {
            cfg.retries_per_endpoint = n;
        }
        if let Some(ms) = env_parse::<u64>(ENV_RETRY_DELAY_MS)? {
            cfg.retry_delay = Duration::from_millis(ms);
        }
        if let Some(rounds) = env_parse::<u32>(ENV_MAX_ROUNDS)? {
            cfg.max_rounds = (rounds > 0).then_some(rounds);
        }
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.retries_per_endpoint >= 1,
            "{ENV_RETRIES_PER_ENDPOINT} must be >= 1"
        );
        anyhow::ensure!(
            self.request_timeout > Duration::ZERO,
            "{ENV_REQUEST_TIMEOUT_SECS} must be > 0"
        );
        anyhow::ensure!(
            self.connect_timeout > Duration::ZERO,
            "{ENV_CONNECT_TIMEOUT_SECS} must be > 0"
        );
        Ok(())
    }
}

fn env_parse<T>(name: &str) -> anyhow::Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(name) {
        Ok(raw) if !raw.trim().is_empty() => raw
            .trim()
            .parse::<T>()
            .map(Some)
            .map_err(|e| anyhow::format_err!("{name}={raw:?}: {e}")),
        _ => Ok(None),
    }
}

/// One failed attempt against one endpoint. [`AttemptError::kind`] is the
/// low-cardinality label used by `relayer_gql_errors_total`.
#[derive(Debug)]
enum AttemptError {
    Transport(reqwest::Error),
    HttpStatus(reqwest::StatusCode),
    Decode(reqwest::Error),
    GraphqlErrors(String),
    MissingData,
    NullData(String),
}

impl AttemptError {
    fn kind(&self) -> &'static str {
        match self {
            AttemptError::Transport(e) if e.is_timeout() => "timeout",
            AttemptError::Transport(_) => "transport",
            AttemptError::HttpStatus(_) => "http_status",
            AttemptError::Decode(_) => "decode",
            AttemptError::GraphqlErrors(_) => "graphql_error",
            AttemptError::MissingData | AttemptError::NullData(_) => "null_data",
        }
    }
}

impl std::fmt::Display for AttemptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttemptError::Transport(e) => write!(f, "transport: {e}"),
            AttemptError::HttpStatus(s) => write!(f, "HTTP status {s}"),
            AttemptError::Decode(e) => write!(f, "undecodable response body: {e}"),
            AttemptError::GraphqlErrors(s) => write!(f, "GraphQL errors: {s}"),
            AttemptError::MissingData => write!(f, "response has no `data` field"),
            AttemptError::NullData(p) => write!(f, "required object `{p}` is null"),
        }
    }
}

/// Keep logged GraphQL error payloads bounded.
const MAX_LOGGED_ERROR_CHARS: usize = 1000;

fn truncate_for_log(s: &str) -> String {
    if s.chars().count() <= MAX_LOGGED_ERROR_CHARS {
        s.to_string()
    } else {
        let head: String = s.chars().take(MAX_LOGGED_ERROR_CHARS).collect();
        format!("{head}…(truncated)")
    }
}

fn describe_metrics_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        describe_counter!(
            "relayer_gql_requests_total",
            "GraphQL request attempts, by endpoint and operation."
        );
        describe_counter!(
            "relayer_gql_errors_total",
            "Failed GraphQL request attempts, by endpoint, operation and error kind."
        );
        describe_counter!(
            "relayer_gql_failovers_total",
            "Switches to the next GraphQL endpoint after the previous one exhausted its retries."
        );
        describe_counter!(
            "relayer_gql_full_rounds_total",
            "Full passes over every configured GraphQL endpoint without a successful attempt."
        );
        describe_histogram!(
            "relayer_gql_request_duration_seconds",
            "Wall time of one GraphQL request attempt."
        );
    });
}

/// Turn a bare `host[:port]`, a base URL or a full `/graphql` URL into the
/// POST target the client uses.
pub fn normalize_endpoint(endpoint: &str) -> String {
    let endpoint = endpoint.trim();
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        if endpoint.trim_end_matches('/').ends_with("/graphql") {
            endpoint.to_string()
        } else {
            format!("{}/graphql", endpoint.trim_end_matches('/'))
        }
    } else {
        format!("http://{}/graphql", endpoint)
    }
}

/// Split a comma-separated endpoint list (`BRIDGE_GQL_FAILOVER_ENDPOINTS`).
/// Surrounding whitespace is trimmed and empty items are dropped, so an
/// unset or empty variable yields no failover endpoints.
pub fn parse_endpoint_list(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Block metadata used for computing block leaf hashes.
#[derive(Debug, Clone)]
pub struct BlockMetadata {
    /// TVM block representation hash (legacy) as hex string.
    pub hash: String,
    /// Envelope hash (SHA-256 of BLS envelope) as hex string.
    pub envelope_hash: String,
    /// Block sequence number / height.
    pub seq_no: u64,
}

#[derive(Debug, Clone)]
pub struct GqlAttestation {
    pub block_id: String,
    pub parent_block_id: String,
    pub target_type: u8,
    pub envelope_hash: String,
    pub aggregated_signature: String,
    pub signature_occurrences: std::collections::HashMap<u16, u16>,
}

#[derive(Debug, Clone)]
pub struct BkSetUpdateWithAttestations {
    pub block_id: String,
    pub bk_set_update_hex: String,
    pub height: Option<u64>,
    pub attestations: Vec<GqlAttestation>,
    /// AN's canonical block ordering key. Present on queries that ask for it
    /// (e.g. `query_bk_set_updates_paged`); `None` on the light/legacy queries
    /// that never fetched it. Used as the opaque `after` cursor for
    /// Relay-style pagination over `bkSetUpdates`.
    pub chain_order: Option<String>,
}

/// GraphQL-fetched proof block — replaces `Envelope<AckiNackiBlock>` as the
/// authoritative source of per-block proof data. Mirrors
/// `acki-nacki/helpers/proof_helper/src/blockchain.rs::GqlProofBlock`.
#[derive(Clone, Debug)]
pub struct GqlProofBlock {
    pub id: String,
    pub block_id: [u8; 32],
    pub thread_id: ThreadIdentifier,
    pub height: u64,
    pub envelope_hash: [u8; 32],
    pub tracked_ext_out_messages_root: [u8; 32],
    pub tracked_ext_out_messages: BTreeMap<AccountRouting, Vec<[u8; 32]>>,
    pub history_proofs: BTreeMap<u8, [u8; 32]>,
    /// 16-leaf SHA-256 block-id Merkle leaves (canonical depth-4 tree, see
    /// `bridge-prover-lib::block_id_tree`). May be absent on very old blocks
    /// that predate the node's exposure of this field. Leaf count is pinned
    /// to [`BLOCK_ID_TREE_LEAF_COUNT`] so it stays in lock-step with the
    /// circuits repo.
    pub block_merkle_tree_leaves: Option<[[u8; 32]; BLOCK_ID_TREE_LEAF_COUNT]>,
}

/// Single-endpoint client with the historical one-attempt semantics
/// ([`GqlClientConfig::single_attempt`]): a failed request returns an error
/// to the caller instead of retrying.
pub fn create_client(endpoint: &str) -> anyhow::Result<GqlClient> {
    create_client_with_failover(endpoint, &[], GqlClientConfig::single_attempt())
}

/// Client with a primary endpoint plus ordered failover endpoints and the
/// retry policy in `cfg`. Duplicates of the primary (or of earlier failover
/// entries) are dropped; empty entries are ignored.
pub fn create_client_with_failover(
    primary: &str,
    failover: &[String],
    cfg: GqlClientConfig,
) -> anyhow::Result<GqlClient> {
    anyhow::ensure!(
        !primary.trim().is_empty(),
        "primary GraphQL endpoint is empty"
    );
    cfg.validate()?;
    let mut endpoints = vec![normalize_endpoint(primary)];
    for candidate in failover {
        if candidate.trim().is_empty() {
            continue;
        }
        let url = normalize_endpoint(candidate);
        if !endpoints.contains(&url) {
            endpoints.push(url);
        }
    }
    let http = reqwest::Client::builder()
        .timeout(cfg.request_timeout)
        .connect_timeout(cfg.connect_timeout)
        .build()
        .context("failed to create HTTP client")?;
    describe_metrics_once();
    Ok(GqlClient {
        http,
        endpoints,
        cfg,
    })
}

impl GqlClient {
    /// Ordered endpoint list; `[0]` is the primary.
    pub fn endpoints(&self) -> &[String] {
        &self.endpoints
    }

    pub fn config(&self) -> &GqlClientConfig {
        &self.cfg
    }

    /// Run a raw query with the client's retry/failover policy. Prefer
    /// [`Self::query_op`] from typed helpers so metrics carry an operation
    /// label and `null` results can be declared as failures.
    pub async fn query(&self, query: &str) -> anyhow::Result<Value> {
        self.query_op("raw", query, &[]).await
    }

    /// Run `query` and return its `data` object. `op` is the low-cardinality
    /// operation label for metrics and logs. Every JSON pointer in
    /// `required_non_null` (e.g. `"/blockchain/blockByHeight"`) must resolve
    /// to a non-null value, otherwise the attempt counts as a `null_data`
    /// failure and the retry/failover loop continues — a block that one
    /// endpoint does not have is exactly the case failover exists for.
    ///
    /// Returns `Err` only when [`GqlClientConfig::max_rounds`] is set and
    /// exhausted; with the daemon default (`None`) this loops until an
    /// attempt succeeds.
    pub async fn query_op(
        &self,
        op: &'static str,
        query: &str,
        required_non_null: &[&str],
    ) -> anyhow::Result<Value> {
        let retries = self.cfg.retries_per_endpoint.max(1);
        let count = self.endpoints.len();
        let mut round: u32 = 0;
        let mut last_error: Option<AttemptError> = None;
        loop {
            for (idx, endpoint) in self.endpoints.iter().enumerate() {
                for attempt in 1..=retries {
                    let started = Instant::now();
                    let result = self.query_once(endpoint, query, required_non_null).await;
                    let elapsed = started.elapsed().as_secs_f64();
                    counter!(
                        "relayer_gql_requests_total",
                        "endpoint" => endpoint.clone(),
                        "op" => op
                    )
                    .increment(1);
                    match result {
                        Ok(data) => {
                            histogram!(
                                "relayer_gql_request_duration_seconds",
                                "endpoint" => endpoint.clone(),
                                "op" => op,
                                "outcome" => "ok"
                            )
                            .record(elapsed);
                            return Ok(data);
                        },
                        Err(e) => {
                            histogram!(
                                "relayer_gql_request_duration_seconds",
                                "endpoint" => endpoint.clone(),
                                "op" => op,
                                "outcome" => "error"
                            )
                            .record(elapsed);
                            counter!(
                                "relayer_gql_errors_total",
                                "endpoint" => endpoint.clone(),
                                "op" => op,
                                "kind" => e.kind()
                            )
                            .increment(1);
                            warn!(
                                endpoint = %endpoint,
                                op,
                                attempt,
                                retries,
                                round,
                                error = %e,
                                "GraphQL request failed",
                            );
                            last_error = Some(e);
                            if attempt < retries {
                                tokio::time::sleep(self.cfg.retry_delay).await;
                            }
                        },
                    }
                }
                if idx + 1 < count {
                    let next = &self.endpoints[idx + 1];
                    counter!(
                        "relayer_gql_failovers_total",
                        "from" => endpoint.clone(),
                        "to" => next.clone()
                    )
                    .increment(1);
                    warn!(
                        from = %endpoint,
                        to = %next,
                        op,
                        "GraphQL endpoint exhausted its retries; failing over",
                    );
                    tokio::time::sleep(self.cfg.retry_delay).await;
                }
            }
            round += 1;
            counter!("relayer_gql_full_rounds_total", "op" => op).increment(1);
            if let Some(max_rounds) = self.cfg.max_rounds {
                if round >= max_rounds {
                    let reason = last_error
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "no attempt was made".to_string());
                    bail!(
                        "GraphQL `{op}` failed on all {count} endpoint(s) after {round} round(s): \
                         {reason}"
                    );
                }
            }
            error!(
                op,
                round,
                endpoints = count,
                "every GraphQL endpoint failed; retrying from the primary",
            );
            tokio::time::sleep(self.cfg.retry_delay).await;
        }
    }

    async fn query_once(
        &self,
        endpoint: &str,
        query: &str,
        required_non_null: &[&str],
    ) -> Result<Value, AttemptError> {
        let resp = self
            .http
            .post(endpoint)
            .json(&json!({ "query": query }))
            .send()
            .await
            .map_err(AttemptError::Transport)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AttemptError::HttpStatus(status));
        }
        let body: Value = resp.json().await.map_err(|e| {
            if e.is_decode() {
                AttemptError::Decode(e)
            } else {
                AttemptError::Transport(e)
            }
        })?;
        if let Some(errors) = body.get("errors") {
            return Err(AttemptError::GraphqlErrors(truncate_for_log(
                &errors.to_string(),
            )));
        }
        let data = match body.get("data") {
            Some(Value::Null) | None => return Err(AttemptError::MissingData),
            Some(data) => data.clone(),
        };
        for pointer in required_non_null {
            match data.pointer(pointer) {
                Some(Value::Null) | None => {
                    return Err(AttemptError::NullData((*pointer).to_string()));
                },
                Some(_) => {},
            }
        }
        Ok(data)
    }

    /// Fetch the latest N blocks (hash + seq_no).
    pub async fn query_latest_blocks(&self, count: u32) -> anyhow::Result<Vec<(String, u64)>> {
        let q = format!(
            r#"{{ blockchain {{ blocks(last: {count}) {{ edges {{ node {{ hash seq_no }} }} }} }} }}"#
        );
        let data = self
            .query_op("latest_blocks", &q, &["/blockchain/blocks"])
            .await?;
        let edges = data
            .pointer("/blockchain/blocks/edges")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut blocks = Vec::new();
        for edge in &edges {
            let node = edge.get("node").unwrap_or(&Value::Null);
            let hash = node.get("hash").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let seq_no = node.get("seq_no").and_then(|v| v.as_u64()).unwrap_or(0);
            blocks.push((hash, seq_no));
        }
        Ok(blocks)
    }

    /// Fetch bkSetUpdates (light — no attestation subfields).
    /// `first_or_last`: true = first N (oldest), false = last N (newest).
    ///
    /// NOT for cold-start BK-set reconstruction. This is a windowed peek — it
    /// returns at most N events from one end of the log, with no continuation
    /// cursor. Combining "first N" + "last N" to synthesize the full history
    /// is the deleted `fetch_bk_set` antipattern (truncated non-contiguous
    /// sample, folded from ∅ which misses the un-emitted genesis committee).
    /// For cold-start, use `bk_set_fetcher::bk_set_at_height` (which pages via
    /// [`Self::query_bk_set_updates_paged`]) against a caller-supplied
    /// genesis snapshot.
    pub async fn query_bk_set_updates_light(
        &self,
        count: u32,
        first: bool,
    ) -> anyhow::Result<Vec<BkSetUpdateWithAttestations>> {
        let pagination = if first {
            format!("first: {count}")
        } else {
            format!("last: {count}")
        };
        let q = format!(
            r#"{{
              blockchain {{
                bkSetUpdates({pagination}) {{
                  edges {{
                    node {{
                      block_id
                      bk_set_update
                      height
                    }}
                  }}
                }}
              }}
            }}"#
        );
        let data = self
            .query_op("bk_set_updates_light", &q, &["/blockchain/bkSetUpdates"])
            .await?;
        let edges = data
            .pointer("/blockchain/bkSetUpdates/edges")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut updates = Vec::new();
        for edge in &edges {
            if let Some(node) = edge.get("node") {
                let block_id = node.get("block_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let bk_set_update_hex = node.get("bk_set_update").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let height = node.get("height").and_then(|v| v.as_u64());
                updates.push(BkSetUpdateWithAttestations {
                    block_id,
                    bk_set_update_hex,
                    height,
                    attestations: Vec::new(), // no attestations in light query
                    chain_order: None,        // not queried in the light path
                });
            }
        }
        Ok(updates)
    }

    /// Fetch the last N bkSetUpdates (most recent), including full
    /// attestation subfields.
    ///
    /// Intended for diagnostic/probe use: dashboards, incident triage, and
    /// monitoring recent rotation cadence + who signed each rotation. NOT
    /// for cold-start BK-set reconstruction — see
    /// [`Self::query_bk_set_updates_paged`] and
    /// `bk_set_fetcher::bk_set_at_height` for that path.
    pub async fn query_bk_set_updates_last(
        &self,
        last: u32,
    ) -> anyhow::Result<Vec<BkSetUpdateWithAttestations>> {
        let q = format!(
            r#"{{
              blockchain {{
                bkSetUpdates(last: {last}) {{
                  edges {{
                    node {{
                      block_id
                      bk_set_update
                      height
                      attestations {{
                        block_id
                        parent_block_id
                        target_type
                        envelope_hash
                        aggregated_signature
                        signature_occurrences
                      }}
                    }}
                  }}
                }}
              }}
            }}"#
        );
        let data = self
            .query_op("bk_set_updates_last", &q, &["/blockchain/bkSetUpdates"])
            .await?;
        let edges = data
            .pointer("/blockchain/bkSetUpdates/edges")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut updates = Vec::new();
        for edge in &edges {
            if let Some(node) = edge.get("node") {
                updates.push(BkSetUpdateWithAttestations::from_json(node)?);
            }
        }
        Ok(updates)
    }

    /// Fetch block metadata by seq_no: hash, envelope_hash, seq_no.
    /// Used for computing block leaf hashes in chain proof construction.
    pub async fn query_block_metadata(&self, seq_no: u64) -> anyhow::Result<BlockMetadata> {
        let tid = "00000000000000000000000000000000000000000000000000000000000000000000";
        let q = format!(
            r#"{{ blockchain {{ blockByHeight(thread_id: "{tid}", height: {seq_no}) {{ hash envelope_hash seq_no }} }} }}"#
        );
        let data = self
            .query_op("block_metadata", &q, &["/blockchain/blockByHeight"])
            .await?;
        let block = data.pointer("/blockchain/blockByHeight").ok_or_else(|| {
            anyhow::format_err!("blockByHeight returned null for seq_no={}", seq_no)
        })?;
        if block.is_null() {
            anyhow::bail!("block at seq_no={} not found", seq_no);
        }
        Ok(BlockMetadata {
            hash: block.get("hash").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            envelope_hash: block.get("envelope_hash").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            seq_no: block.get("seq_no").and_then(|v| v.as_u64()).unwrap_or(0),
        })
    }

    /// Fetch metadata for a range of blocks [from_seq..=to_seq].
    pub async fn query_blocks_metadata_range(
        &self,
        from_seq: u64,
        to_seq: u64,
    ) -> anyhow::Result<Vec<BlockMetadata>> {
        let mut results = Vec::new();
        for seq in from_seq..=to_seq {
            match self.query_block_metadata(seq).await {
                Ok(meta) => results.push(meta),
                Err(e) => {
                    tracing::warn!("failed to fetch block metadata for seq={}: {}", seq, e);
                    // Still push a placeholder to keep alignment
                    results.push(BlockMetadata {
                        hash: String::new(),
                        envelope_hash: String::new(),
                        seq_no: seq,
                    });
                }
            }
        }
        Ok(results)
    }

    /// Fetch `bkSetUpdates` with height <= `height_end`, forward-paginated by
    /// `chain_order` cursor. Returns the next-page cursor if more results
    /// exist (i.e. the last node's `chain_order`), or `None` when the page
    /// is exhausted.
    ///
    /// Uses the "light" projection (no `attestations` subfields) because
    /// this path is optimized for cold-start BK-set replay, where only the
    /// `bk_set_update` blob + `height` + `chain_order` are needed.
    ///
    /// Note: page size in the underlying `bkSetUpdates` connection is
    /// bounded server-side; callers should treat `first` as a hint.
    pub async fn query_bk_set_updates_paged(
        &self,
        height_end: u64,
        first: u32,
        after: Option<&str>,
    ) -> anyhow::Result<(Vec<BkSetUpdateWithAttestations>, Option<String>)> {
        let height_end = i64::try_from(height_end).context(
            "bkSetUpdates height_end exceeds the live GraphQL signed Int range",
        )?;
        let after_arg = match after {
            Some(cur) => format!(r#", after: "{}""#, cur.replace('"', "\\\"")),
            None => String::new(),
        };
        let q = format!(
            r#"{{
              blockchain {{
                bkSetUpdates(first: {first}, height_end: {height_end}{after_arg}) {{
                  edges {{
                    node {{
                      block_id
                      bk_set_update
                      height
                      chain_order
                    }}
                  }}
                }}
              }}
            }}"#
        );
        let data = self
            .query_op("bk_set_updates_paged", &q, &["/blockchain/bkSetUpdates"])
            .await?;
        let edges = data
            .pointer("/blockchain/bkSetUpdates/edges")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut updates = Vec::new();
        let mut last_cursor: Option<String> = None;
        for edge in &edges {
            if let Some(node) = edge.get("node") {
                let block_id = node
                    .get("block_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let bk_set_update_hex = node
                    .get("bk_set_update")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let height = node.get("height").and_then(|v| v.as_u64());
                let chain_order = node
                    .get("chain_order")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let Some(ref c) = chain_order {
                    last_cursor = Some(c.clone());
                }
                updates.push(BkSetUpdateWithAttestations {
                    block_id,
                    bk_set_update_hex,
                    height,
                    attestations: Vec::new(),
                    chain_order,
                });
            }
        }
        // "More data available" heuristic: server returned a full page.
        // (The `has_next_page` flag isn't in our projection; comparing
        // len == first is close enough and only affects when we stop paging.)
        let next = if updates.len() as u32 >= first {
            last_cursor
        } else {
            None
        };
        Ok((updates, next))
    }

    /// Fetch the first N bkSetUpdates (oldest first).
    pub async fn query_bk_set_updates(
        &self,
        first: u32,
    ) -> anyhow::Result<Vec<BkSetUpdateWithAttestations>> {
        let q = format!(
            r#"{{
              blockchain {{
                bkSetUpdates(first: {first}) {{
                  edges {{
                    node {{
                      block_id
                      bk_set_update
                      height
                      attestations {{
                        block_id
                        parent_block_id
                        target_type
                        envelope_hash
                        aggregated_signature
                        signature_occurrences
                      }}
                    }}
                  }}
                }}
              }}
            }}"#
        );
        let data = self
            .query_op("bk_set_updates", &q, &["/blockchain/bkSetUpdates"])
            .await?;
        let edges = data
            .pointer("/blockchain/bkSetUpdates/edges")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut updates = Vec::new();
        for edge in &edges {
            if let Some(node) = edge.get("node") {
                updates.push(BkSetUpdateWithAttestations::from_json(node)?);
            }
        }
        Ok(updates)
    }
}

impl GqlAttestation {
    pub fn from_json(v: &Value) -> anyhow::Result<Self> {
        let block_id = v
            .get("block_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let parent_block_id = v
            .get("parent_block_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let target_type = match v
            .get("target_type")
            .and_then(|v| v.as_str())
            .unwrap_or("PRIMARY")
        {
            "PRIMARY" | "Primary" => 0,
            "FALLBACK" | "Fallback" => 1,
            other => bail!("unknown target_type: {}", other),
        };
        let envelope_hash = v
            .get("envelope_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let aggregated_signature = v
            .get("aggregated_signature")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let sig_occ_json = v.get("signature_occurrences").cloned().unwrap_or(Value::Null);
        let mut signature_occurrences = std::collections::HashMap::new();
        if let Some(obj) = sig_occ_json.as_object() {
            for (k, v) in obj {
                let signer_idx: u16 = k.parse().context("invalid signer index")?;
                let count = v.as_u64().unwrap_or(0) as u16;
                signature_occurrences.insert(signer_idx, count);
            }
        }
        Ok(Self {
            block_id,
            parent_block_id,
            target_type,
            envelope_hash,
            aggregated_signature,
            signature_occurrences,
        })
    }
}

impl BkSetUpdateWithAttestations {
    pub fn from_json(v: &Value) -> anyhow::Result<Self> {
        let block_id = v
            .get("block_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let bk_set_update_hex = v
            .get("bk_set_update")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let height = v.get("height").and_then(|v| v.as_u64());
        let att_json = v
            .get("attestations")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut attestations = Vec::new();
        for a in &att_json {
            attestations.push(GqlAttestation::from_json(a)?);
        }
        Ok(Self {
            block_id,
            bk_set_update_hex,
            height,
            attestations,
            chain_order: None, // legacy heavy queries don't fetch it
        })
    }
}



/// Default thread_id used by the single-thread testbed.
pub const DEFAULT_THREAD_ID_HEX: &str =
    "00000000000000000000000000000000000000000000000000000000000000000000";

const PROOF_BLOCK_FRAGMENT: &str = r#"
  fragment ProofBlockFields on Block {
    id
    block_id
    thread_id
    height
    envelope_hash
    tracked_ext_out_messages_root
    tracked_ext_out_message_hashes {
      routing
      message_hashes
    }
    history_proofs {
      layer
      root_hash
    }
    block_merkle_tree_leaves
  }
"#;

impl GqlClient {
    /// Fetch a `GqlProofBlock` by (thread_id, height). thread_id_hex should be
    /// the 68-hex-char form the node emits (34 bytes).
    pub async fn query_block_by_height(
        &self,
        thread_id_hex: &str,
        height: u64,
    ) -> anyhow::Result<GqlProofBlock> {
        let q = format!(
            r#"{{
              blockchain {{
                blockByHeight(thread_id: "{thread_id_hex}", height: {height}) {{
                  id block_id thread_id height envelope_hash
                  tracked_ext_out_messages_root
                  tracked_ext_out_message_hashes {{ routing message_hashes }}
                  history_proofs {{ layer root_hash }}
                  block_merkle_tree_leaves
                }}
              }}
            }}"#,
        );
        let _ = PROOF_BLOCK_FRAGMENT; // keep fragment as documentation
        let data = self
            .query_op("proof_block", &q, &["/blockchain/blockByHeight"])
            .await?;
        let block = data.pointer("/blockchain/blockByHeight").ok_or_else(|| {
            anyhow::format_err!("blockByHeight returned no field for height={height}")
        })?;
        if block.is_null() {
            anyhow::bail!("block at thread_id={thread_id_hex} height={height} not found");
        }
        parse_proof_block(block)
    }

    /// Fetch a `GqlProofBlock` on the default single-thread testbed by seq_no.
    pub async fn query_proof_block_by_seqno(&self, seqno: u64) -> anyhow::Result<GqlProofBlock> {
        self.query_block_by_height(DEFAULT_THREAD_ID_HEX, seqno).await
    }

    /// Fetch ALL `Block.attestations[]` entries for the block at
    /// `target_seq_no` and parse each into a `ParsedAttestation`.
    ///
    /// On primary-finalized blocks this returns exactly one entry of
    /// `target_type=PRIMARY` (≥66% signers). On fallback-finalized blocks it
    /// returns two entries sharing the same `block_id`: one `PRIMARY`
    /// prefinalization (≥50%+1) and one `FALLBACK` target proof (≥50%+1).
    /// Higher-level shape classification lives in
    /// `attestation_fetcher::fetch_attestation_evidence`.
    ///
    /// The resolver filters `Block.attestations[]` so every entry already
    /// targets `self.id`; we still surface any unparseable entry as an error
    /// rather than silently dropping it so a malformed row can't masquerade
    /// as a primary-only block.
    pub async fn query_attestation_envelopes(
        &self,
        target_seq_no: u64,
    ) -> anyhow::Result<Vec<crate::attestation_fetcher::ParsedAttestation>> {
        let q = format!(
            r#"{{ blockchain {{ blockByHeight(thread_id: "{DEFAULT_THREAD_ID_HEX}", height: {target_seq_no}) {{ seq_no attestations {{ block_id parent_block_id target_type envelope_hash aggregated_signature signature_occurrences }} }} }} }}"#
        );
        let data = self
            .query_op("attestations", &q, &["/blockchain/blockByHeight"])
            .await
            .with_context(|| format!("blockByHeight({target_seq_no})"))?;
        let block = data
            .pointer("/blockchain/blockByHeight")
            .ok_or_else(|| anyhow::format_err!("blockByHeight({target_seq_no}) returned null"))?;
        if block.is_null() {
            anyhow::bail!("block at seq_no={target_seq_no} not found");
        }
        let atts = block
            .get("attestations")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::format_err!("block {target_seq_no} missing attestations field"))?;
        if atts.is_empty() {
            anyhow::bail!(
                "Block.attestations[] is empty for block seq_no={target_seq_no} — \
                 producer has not yet committed attestations for this block. \
                 Retry once a few blocks have been produced past it."
            );
        }
        atts.iter()
            .map(parse_block_attestation)
            .collect::<anyhow::Result<Vec<_>>>()
            .with_context(|| format!("parse_block_attestation for block {target_seq_no}"))
    }

    // ─────────────────────────────────────────────────────────────────
    // withdraw-E2E orchestrator helpers
    //
    // These three queries mirror the Python driver in
    // `crates/bridge-prover-libraries/python/helper/bridge_e2e.py` (`GqlClient`).
    // The withdraw-E2E orchestrator in `bridge-relayer-daemon` calls them
    // to discover a newly-emitted `WithdrawalInitiated` ExtOut event and
    // resolve the block context the Circuit-4 witness exporter needs.
    // ─────────────────────────────────────────────────────────────────

    /// Fetch recent ExtOut messages emitted by a specific bridge account.
    /// Returns the last `limit` messages in schema order (oldest first).
    ///
    /// `Message.block_id` is `null` on ExtOut in this GQL schema; we fall
    /// back to `src_transaction.block_id`, matching the Python driver.
    pub async fn query_bridge_extouts(
        &self,
        account_id_hex: &str,
        dapp_id_hex: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<BridgeExtOutMessage>> {
        let q = format!(
            r#"{{
              blockchain {{
                account(account_id: "{account_id_hex}", dapp_id: "{dapp_id_hex}") {{
                  messages(msg_type: [ExtOut], last: {limit}) {{
                    edges {{ node {{
                      id boc dst created_at
                      block_id src_dapp_id
                      src_transaction {{ block_id }}
                    }} }}
                  }}
                }}
              }}
            }}"#
        );
        let data = self.query_op("bridge_extouts", &q, &[]).await?;
        let edges = data
            .pointer("/blockchain/account/messages/edges")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::with_capacity(edges.len());
        for edge in &edges {
            let node = edge.get("node").unwrap_or(&Value::Null);
            let id = node.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let boc = node.get("boc").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let dst = node.get("dst").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let created_at = node.get("created_at").and_then(|v| v.as_u64());
            let src_dapp_id = node
                .get("src_dapp_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let block_id = node
                .get("block_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    node.get("src_transaction")
                        .and_then(|tx| tx.get("block_id"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                });
            out.push(BridgeExtOutMessage {
                id,
                boc,
                dst,
                created_at,
                src_dapp_id,
                block_id,
            });
        }
        Ok(out)
    }

    /// Resolve the on-chain `dapp_id` for an account. Zerostate-deployed
    /// contracts typically return the same value as the input; surfaced so
    /// the orchestrator can pass the exporter what the exporter expects.
    /// Returns `None` if the field is absent or empty.
    pub async fn query_account_dapp_id(
        &self,
        account_id_hex: &str,
        dapp_id_hex: &str,
    ) -> anyhow::Result<Option<String>> {
        let q = format!(
            r#"{{ blockchain {{ account(account_id: "{account_id_hex}", dapp_id: "{dapp_id_hex}") {{ info {{ dapp_id }} }} }} }}"#
        );
        let data = self.query_op("account_dapp_id", &q, &[]).await?;
        Ok(data
            .pointer("/blockchain/account/info/dapp_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty()))
    }

    /// Resolve a block via direct `block(hash:)` lookup.
    ///
    /// Note: what `Message.src_transaction.block_id` returns is the block's
    /// `hash` field (BOC hash), NOT its consensus `block_id`. Use this to
    /// obtain the canonical block_id, envelope_hash, seq_no, and height.
    pub async fn query_block_by_hash(&self, block_hash: &str) -> anyhow::Result<GqlBlockByHash> {
        let q = format!(
            r#"{{ blockchain {{ block(hash: "{block_hash}") {{ hash block_id seq_no height envelope_hash key_block }} }} }}"#
        );
        let data = self.query_op("block_by_hash", &q, &[]).await?;
        let block = data
            .pointer("/blockchain/block")
            .ok_or_else(|| anyhow::format_err!("block(hash:{block_hash}) returned no field"))?;
        if block.is_null() {
            anyhow::bail!("block(hash:{block_hash}) not found");
        }
        Ok(GqlBlockByHash {
            hash: block
                .get("hash")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            block_id: block
                .get("block_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            seq_no: block.get("seq_no").and_then(|v| v.as_u64()).unwrap_or(0),
            height: block.get("height").and_then(|v| v.as_u64()).unwrap_or(0),
            envelope_hash: block
                .get("envelope_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            key_block: block.get("key_block").and_then(|v| v.as_bool()).unwrap_or(false),
        })
    }

    /// Fetch the outbound messages of a transaction by its hash. Returns
    /// `Ok(None)` if the tx is not (yet) visible via GQL — the caller is
    /// expected to retry. Empty `Vec` means the tx was found but produced
    /// no outbound messages (spec-legal but not what a multisig-forward
    /// path expects).
    pub async fn query_tx_out_messages(
        &self,
        tx_hash: &str,
    ) -> anyhow::Result<Option<Vec<GqlOutMessageStub>>> {
        // shellnet GQL rejects `0x`-prefixed hashes on `transaction(hash:)`.
        let tx_hash = tx_hash.trim_start_matches("0x");
        let q = format!(
            r#"{{ blockchain {{ transaction(hash: "{tx_hash}") {{ out_messages {{ id dst }} }} }} }}"#
        );
        let data = self.query_op("tx_out_messages", &q, &[]).await?;
        let tx = data
            .pointer("/blockchain/transaction")
            .unwrap_or(&Value::Null);
        if tx.is_null() {
            return Ok(None);
        }
        let arr = tx
            .get("out_messages")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let out: Vec<GqlOutMessageStub> = arr
            .iter()
            .filter_map(|m| {
                let id = m.get("id").and_then(|v| v.as_str())?.to_string();
                let dst = m.get("dst").and_then(|v| v.as_str()).unwrap_or("").to_string();
                Some(GqlOutMessageStub { id, dst })
            })
            .collect();
        Ok(Some(out))
    }

    /// Fetch the outbound messages of the *destination* transaction of an
    /// internal message — i.e. the tx that consumed `msg_id`. Returns
    /// `Ok(None)` if either the message itself or its `dst_transaction`
    /// is not yet visible (the receiving contract hasn't run yet). The
    /// caller polls until `Some(_)` appears.
    pub async fn query_msg_dst_tx_out_messages(
        &self,
        msg_id: &str,
    ) -> anyhow::Result<Option<Vec<GqlOutMessageStub>>> {
        let msg_id = msg_id.trim_start_matches("0x");
        // Two-step: shellnet's schema returns `null` for the nested
        // `dst_transaction.out_messages` even when it fully populates the
        // outer `transaction(hash: …) { out_messages }`. Fetch the tx id
        // first, then re-query the tx directly.
        let q1 = format!(
            r#"{{ blockchain {{ message(hash: "{msg_id}") {{ dst_transaction {{ id }} }} }} }}"#
        );
        let data = self.query_op("msg_dst_transaction", &q1, &[]).await?;
        let msg = data.pointer("/blockchain/message").unwrap_or(&Value::Null);
        if msg.is_null() {
            return Ok(None);
        }
        let dst_tx = msg.get("dst_transaction").unwrap_or(&Value::Null);
        if dst_tx.is_null() {
            return Ok(None);
        }
        let tx_id = match dst_tx.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return Ok(None),
        };
        // Reuse `query_tx_out_messages` — same field access, correct
        // fallback semantics, and it handles the `0x` prefix trim.
        self.query_tx_out_messages(&tx_id).await
    }

    /// Fetch a single ExtOut message by id in the same shape as
    /// [`Self::query_bridge_extouts`] rows. Returns `Ok(None)` if the
    /// message is not (yet) visible via GQL. Used by the targeted-capture
    /// path after chain-walking to the WithdrawalInitiated msg_id.
    pub async fn query_bridge_extout_by_id(
        &self,
        msg_id: &str,
    ) -> anyhow::Result<Option<BridgeExtOutMessage>> {
        let msg_id = msg_id.trim_start_matches("0x");
        let q = format!(
            r#"{{ blockchain {{ message(hash: "{msg_id}") {{ id boc dst created_at block_id src_dapp_id src_transaction {{ block_id }} }} }} }}"#
        );
        let data = self.query_op("bridge_extout_by_id", &q, &[]).await?;
        let m = data.pointer("/blockchain/message").unwrap_or(&Value::Null);
        if m.is_null() {
            return Ok(None);
        }
        let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let boc = m.get("boc").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let dst = m.get("dst").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let created_at = m.get("created_at").and_then(|v| v.as_u64());
        let src_dapp_id = m
            .get("src_dapp_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let block_id = m
            .get("block_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                m.get("src_transaction")
                    .and_then(|tx| tx.get("block_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
        Ok(Some(BridgeExtOutMessage {
            id,
            boc,
            dst,
            created_at,
            src_dapp_id,
            block_id,
        }))
    }
}

/// One ExtOut message row from
/// [`GqlClient::query_bridge_extouts`]. `block_id` is `None` only if both
/// `Message.block_id` and `src_transaction.block_id` were absent.
#[derive(Debug, Clone)]
pub struct BridgeExtOutMessage {
    pub id: String,
    pub boc: String,
    pub dst: String,
    pub created_at: Option<u64>,
    pub src_dapp_id: Option<String>,
    pub block_id: Option<String>,
}

/// Bare `(id, dst)` pair from a transaction's `out_messages` list. Used
/// by the targeted-capture chain walk (multisig tx → USDCBridge internal
/// msg → USDCBridge tx → WithdrawalInitiated ExtOut) so each hop can
/// pick the "right" outgoing message by destination without dragging the
/// full `BridgeExtOutMessage` shape.
#[derive(Debug, Clone)]
pub struct GqlOutMessageStub {
    pub id: String,
    pub dst: String,
}

/// Block metadata returned by [`GqlClient::query_block_by_hash`].
#[derive(Debug, Clone)]
pub struct GqlBlockByHash {
    pub hash: String,
    pub block_id: String,
    pub seq_no: u64,
    pub height: u64,
    pub envelope_hash: String,
    pub key_block: bool,
}

/// Convert one `BlockAttestation` GraphQL object into a `ParsedAttestation`
/// whose `raw_bytes` field matches the exact byte layout of
/// `bincode(Envelope<AttestationData>)`:
///
/// ```text
/// u64(192) || 192 sig bytes                       <- aggregated_signature
/// u64(num_signers) || (u16 idx, u16 cnt) * N      <- signature_occurrences
/// u64(32) || 32 parent_block_id                   <- AttestationData section
/// u64(32) || 32 block_id
/// u32 LE seq_no                                       (NOT present in GQL — derived elsewhere)
/// 32 envelope_hash (transparent, no length prefix)
/// u32 LE target_type
/// ```
///
/// The GraphQL `BlockAttestation` object exposes `aggregated_signature` and
/// `signature_occurrences` as hex strings of the *bincoded* (length-prefixed)
/// forms — we splice them verbatim into the assembled bytes. `block_id`,
/// `parent_block_id`, `envelope_hash`, `target_type` come as plain hex / enum.
///
/// `block_seq_no` is not exposed on `BlockAttestation`; the caller matches by
/// `block_id` after walking forward, then this function fills in the u32 from
/// the matched record. (Here we set it from the AttestationData JSON if the
/// schema is later extended; for now we record 0 and the caller will replace it.)
fn parse_block_attestation(
    v: &serde_json::Value,
) -> anyhow::Result<crate::attestation_fetcher::ParsedAttestation> {
    let block_id = decode_hash32(required_string(v, "block_id")?).context("att.block_id")?;
    let parent_block_id =
        decode_hash32(required_string(v, "parent_block_id")?).context("att.parent_block_id")?;
    let envelope_hash =
        decode_hash32(required_string(v, "envelope_hash")?).context("att.envelope_hash")?;

    let target_type = match v.get("target_type").and_then(|x| x.as_str()).unwrap_or("PRIMARY") {
        "PRIMARY" | "Primary" => 0u32,
        "FALLBACK" | "Fallback" => 1u32,
        other => anyhow::bail!("unknown target_type: {other}"),
    };

    // aggregated_signature and signature_occurrences come back as hex strings
    // of the bincode-prefixed forms (200 bytes and 8+4N bytes respectively).
    let sig_hex = required_string(v, "aggregated_signature")?;
    let sig_bytes = hex::decode(sig_hex.trim_start_matches("0x"))
        .context("decode att.aggregated_signature hex")?;
    anyhow::ensure!(
        sig_bytes.len() == 200,
        "aggregated_signature must be 200 bytes (u64(192) + 192 sig), got {}",
        sig_bytes.len(),
    );

    // signature_occurrences here is the Json object form from the resolver:
    // {"signer_idx": count, ...}. We rebuild bincode-prefixed bytes.
    let sig_occ_json = v
        .get("signature_occurrences")
        .ok_or_else(|| anyhow::format_err!("missing signature_occurrences"))?;
    let mut occurrences = std::collections::HashMap::new();
    if let Some(obj) = sig_occ_json.as_object() {
        for (k, val) in obj {
            let idx: u16 = k.parse().context("signer index parse")?;
            let cnt = val.as_u64().context("signer count parse")? as u16;
            occurrences.insert(idx, cnt);
        }
    } else {
        anyhow::bail!("signature_occurrences must be a JSON object");
    }
    let mut sorted: Vec<(u16, u16)> = occurrences.iter().map(|(k, v)| (*k, *v)).collect();
    sorted.sort_by_key(|(k, _)| *k);
    let mut occ_bytes = Vec::with_capacity(8 + sorted.len() * 4);
    occ_bytes.extend_from_slice(&(sorted.len() as u64).to_le_bytes());
    for (idx, cnt) in &sorted {
        occ_bytes.extend_from_slice(&idx.to_le_bytes());
        occ_bytes.extend_from_slice(&cnt.to_le_bytes());
    }

    // We don't have block_seq_no on the GQL attestation row; this is fine for
    // the upstream consumer because compute_block_seq_no in prover.rs reads it
    // from raw_bytes. We synthesize 0 here; if the caller needs a real seq,
    // it should be patched in after matching by block_id.
    let block_seq_no: u32 = 0;

    let mut raw_bytes = Vec::with_capacity(sig_bytes.len() + occ_bytes.len() + 120);
    raw_bytes.extend_from_slice(&sig_bytes);
    raw_bytes.extend_from_slice(&occ_bytes);
    raw_bytes.extend_from_slice(&32u64.to_le_bytes());
    raw_bytes.extend_from_slice(&parent_block_id);
    raw_bytes.extend_from_slice(&32u64.to_le_bytes());
    raw_bytes.extend_from_slice(&block_id);
    raw_bytes.extend_from_slice(&block_seq_no.to_le_bytes());
    raw_bytes.extend_from_slice(&envelope_hash);
    raw_bytes.extend_from_slice(&target_type.to_le_bytes());

    Ok(crate::attestation_fetcher::ParsedAttestation {
        raw_bytes,
        parent_block_id,
        block_id,
        block_seq_no,
        envelope_hash,
        target_type,
        signature_occurrences: occurrences,
    })
}

fn parse_proof_block(value: &serde_json::Value) -> anyhow::Result<GqlProofBlock> {
    use std::str::FromStr;
    let id = required_string(value, "id")?.to_string();
    let block_id = decode_hash32(required_string(value, "block_id")?).context("block_id")?;
    let thread_id =
        ThreadIdentifier::try_from(required_string(value, "thread_id")?.to_string())
            .context("thread_id")?;
    let height = parse_u64_field(value, "height")?;
    let envelope_hash =
        decode_hash32(required_string(value, "envelope_hash")?).context("envelope_hash")?;
    let tracked_ext_out_messages_root =
        decode_hash32(required_string(value, "tracked_ext_out_messages_root")?)
            .context("tracked_ext_out_messages_root")?;

    // tracked_ext_out_message_hashes -> BTreeMap<AccountRouting, Vec<[u8;32]>>
    let mut tracked_ext_out_messages: BTreeMap<AccountRouting, Vec<[u8; 32]>> = BTreeMap::new();
    if let Some(arr) = value.get("tracked_ext_out_message_hashes").and_then(|v| v.as_array()) {
        for entry in arr {
            let routing = AccountRouting::from_str(required_string(entry, "routing")?)
                .context("tracked_ext_out_messages routing")?;
            let mh = entry
                .get("message_hashes")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow::format_err!("message_hashes is not an array"))?;
            let mut hashes = Vec::with_capacity(mh.len());
            for h in mh {
                let h = h
                    .as_str()
                    .ok_or_else(|| anyhow::format_err!("message hash is not a string"))?;
                hashes.push(decode_hash32(h).context("tracked message hash")?);
            }
            tracked_ext_out_messages.insert(routing, hashes);
        }
    }

    // history_proofs -> BTreeMap<u8, [u8;32]>
    let mut history_proofs: BTreeMap<u8, [u8; 32]> = BTreeMap::new();
    if let Some(arr) = value.get("history_proofs").and_then(|v| v.as_array()) {
        for entry in arr {
            let layer = parse_u64_field(entry, "layer")?;
            anyhow::ensure!(layer <= u8::MAX as u64, "history proof layer out of range");
            let root_hash =
                decode_hash32(required_string(entry, "root_hash")?).context("history proof root_hash")?;
            history_proofs.insert(layer as u8, root_hash);
        }
    }

    // block_merkle_tree_leaves -> Option<[[u8;32]; BLOCK_ID_TREE_LEAF_COUNT]>
    let block_merkle_tree_leaves = match value.get("block_merkle_tree_leaves") {
        Some(serde_json::Value::Array(items)) => {
            anyhow::ensure!(
                items.len() == BLOCK_ID_TREE_LEAF_COUNT,
                "block_merkle_tree_leaves must have {} entries, got {}",
                BLOCK_ID_TREE_LEAF_COUNT,
                items.len(),
            );
            let mut out = [[0u8; 32]; BLOCK_ID_TREE_LEAF_COUNT];
            for (i, item) in items.iter().enumerate() {
                let s = item.as_str().ok_or_else(|| {
                    anyhow::format_err!("block_merkle_tree_leaves[{i}] is not a string")
                })?;
                out[i] = decode_hash32(s).with_context(|| format!("block_merkle_tree_leaves[{i}]"))?;
            }
            Some(out)
        }
        _ => None,
    };

    Ok(GqlProofBlock {
        id,
        block_id,
        thread_id,
        height,
        envelope_hash,
        tracked_ext_out_messages_root,
        tracked_ext_out_messages,
        history_proofs,
        block_merkle_tree_leaves,
    })
}

fn required_string<'a>(v: &'a serde_json::Value, field: &str) -> anyhow::Result<&'a str> {
    v.get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::format_err!("missing or non-string field `{field}`"))
}

fn parse_u64_field(v: &serde_json::Value, field: &str) -> anyhow::Result<u64> {
    let val = v.get(field).ok_or_else(|| anyhow::format_err!("missing field `{field}`"))?;
    if let Some(n) = val.as_u64() {
        return Ok(n);
    }
    if let Some(n) = val.as_i64() {
        return u64::try_from(n).with_context(|| format!("{field} is negative"));
    }
    if let Some(n) = val.as_f64() {
        anyhow::ensure!(n.is_finite() && n >= 0.0 && n.fract() == 0.0, "{field} not an integer");
        return Ok(n as u64);
    }
    anyhow::bail!("{field} is not a number")
}

fn decode_hash32(s: &str) -> anyhow::Result<[u8; 32]> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).map_err(|e| anyhow::format_err!("invalid hex: {e}"))?;
    anyhow::ensure!(bytes.len() == 32, "expected 32 bytes, got {}", bytes.len());
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

#[cfg(test)]
mod height_end_tests {
    use super::*;

    #[test]
    fn unbounded_height_fits_live_graphql_signed_int() {
        assert_eq!(i64::try_from(GRAPHQL_SIGNED_INT_MAX).unwrap(), i64::MAX);
    }

    #[test]
    fn u64_max_does_not_fit_live_graphql_signed_int() {
        assert!(i64::try_from(u64::MAX).is_err());
    }
}

#[cfg(test)]
mod failover_tests {
    //! Retry / failover behaviour against scripted local HTTP servers. No
    //! network, no recorder: `metrics` macros are no-ops without one, so the
    //! assertions count accepted connections instead.

    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    const OK_BLOCKS: &str =
        r#"{"data":{"blockchain":{"blocks":{"edges":[{"node":{"hash":"aa","seq_no":7}}]}}}}"#;
    const NULL_BLOCK: &str = r#"{"data":{"blockchain":{"blockByHeight":null}}}"#;
    const SOME_BLOCK: &str = r#"{"data":{"blockchain":{"blockByHeight":{"seq_no":7}}}}"#;
    const GQL_ERRORS: &str = r#"{"errors":[{"message":"boom"}],"data":null}"#;

    #[derive(Clone)]
    enum Reply {
        Json(&'static str),
        Status(u16),
        Drop,
    }

    struct MockServer {
        url: String,
        hits: Arc<AtomicUsize>,
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// Scripted HTTP/1.1 responder: each accepted connection consumes the
    /// next script entry (the last one repeats forever). `Connection: close`
    /// makes reqwest open a new connection per attempt, so `hits` counts
    /// attempts exactly.
    async fn spawn(script: Vec<Reply>) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_in_task = Arc::clone(&hits);
        tokio::spawn(async move {
            let mut served = 0usize;
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                hits_in_task.fetch_add(1, Ordering::SeqCst);
                let reply = script[served.min(script.len() - 1)].clone();
                served += 1;

                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(head_end) = find(&buf, b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
                        let content_length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        while buf.len() < head_end + 4 + content_length {
                            let n = sock.read(&mut chunk).await.unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            buf.extend_from_slice(&chunk[..n]);
                        }
                        break;
                    }
                }

                let response = match reply {
                    Reply::Drop => None,
                    Reply::Status(code) => Some(format!(
                        "HTTP/1.1 {code} Scripted\r\nContent-Length: 0\r\nConnection: \
                         close\r\n\r\n"
                    )),
                    Reply::Json(body) => Some(format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \
                         {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )),
                };
                if let Some(response) = response {
                    let _ = sock.write_all(response.as_bytes()).await;
                }
                let _ = sock.shutdown().await;
            }
        });
        MockServer {
            url: format!("http://{addr}/graphql"),
            hits,
        }
    }

    fn fast(max_rounds: Option<u32>) -> GqlClientConfig {
        GqlClientConfig {
            retry_delay: Duration::from_millis(5),
            request_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(5),
            max_rounds,
            ..GqlClientConfig::default()
        }
    }

    fn client(primary: &MockServer, failover: &[&MockServer], cfg: GqlClientConfig) -> GqlClient {
        let failover: Vec<String> = failover.iter().map(|s| s.url.clone()).collect();
        create_client_with_failover(&primary.url, &failover, cfg).unwrap()
    }

    #[tokio::test]
    async fn fails_over_after_retries_on_http_500() {
        let primary = spawn(vec![Reply::Status(500)]).await;
        let backup = spawn(vec![Reply::Json(OK_BLOCKS)]).await;
        let gql = client(&primary, &[&backup], fast(None));

        let blocks = gql.query_latest_blocks(1).await.unwrap();
        assert_eq!(blocks, vec![("aa".to_string(), 7)]);
        assert_eq!(
            primary.hits.load(Ordering::SeqCst),
            3,
            "3 attempts on the primary"
        );
        assert_eq!(
            backup.hits.load(Ordering::SeqCst),
            1,
            "1 attempt on the failover"
        );
    }

    #[tokio::test]
    async fn null_required_object_counts_as_failure_and_fails_over() {
        let primary = spawn(vec![Reply::Json(NULL_BLOCK)]).await;
        let backup = spawn(vec![Reply::Json(SOME_BLOCK)]).await;
        let gql = client(&primary, &[&backup], fast(None));

        let data = gql
            .query_op("test", "{ x }", &["/blockchain/blockByHeight"])
            .await
            .unwrap();
        assert_eq!(
            data.pointer("/blockchain/blockByHeight/seq_no"),
            Some(&json!(7))
        );
        assert_eq!(primary.hits.load(Ordering::SeqCst), 3);
        assert_eq!(backup.hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn graphql_errors_and_dropped_connections_are_retried_on_the_same_endpoint() {
        let primary = spawn(vec![
            Reply::Json(GQL_ERRORS),
            Reply::Drop,
            Reply::Json(OK_BLOCKS),
        ])
        .await;
        let backup = spawn(vec![Reply::Json(OK_BLOCKS)]).await;
        let gql = client(&primary, &[&backup], fast(None));

        gql.query_latest_blocks(1).await.unwrap();
        assert_eq!(
            primary.hits.load(Ordering::SeqCst),
            3,
            "errors, drop, then success"
        );
        assert_eq!(backup.hits.load(Ordering::SeqCst), 0, "no failover needed");
    }

    #[tokio::test]
    async fn every_request_starts_from_the_primary_again() {
        let primary = spawn(vec![
            Reply::Status(503),
            Reply::Status(503),
            Reply::Status(503),
            Reply::Json(OK_BLOCKS),
        ])
        .await;
        let backup = spawn(vec![Reply::Json(OK_BLOCKS)]).await;
        let gql = client(&primary, &[&backup], fast(None));

        gql.query_latest_blocks(1).await.unwrap();
        assert_eq!(primary.hits.load(Ordering::SeqCst), 3);
        assert_eq!(backup.hits.load(Ordering::SeqCst), 1);

        // Primary recovered: the next request must not stick to the backup.
        gql.query_latest_blocks(1).await.unwrap();
        assert_eq!(primary.hits.load(Ordering::SeqCst), 4);
        assert_eq!(backup.hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn single_endpoint_loops_until_it_recovers() {
        let only = spawn(vec![
            Reply::Status(502),
            Reply::Status(502),
            Reply::Status(502),
            Reply::Status(502),
            Reply::Status(502),
            Reply::Json(OK_BLOCKS),
        ])
        .await;
        let gql = client(&only, &[], fast(None));

        gql.query_latest_blocks(1).await.unwrap();
        assert_eq!(
            only.hits.load(Ordering::SeqCst),
            6,
            "two rounds of 3, success on the 6th"
        );
    }

    #[tokio::test]
    async fn connection_refused_primary_fails_over() {
        // Bind then drop: the port is free again and connections are refused.
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_url = format!("http://{}/graphql", dead.local_addr().unwrap());
        drop(dead);
        let backup = spawn(vec![Reply::Json(OK_BLOCKS)]).await;
        let gql =
            create_client_with_failover(&dead_url, std::slice::from_ref(&backup.url), fast(None))
                .unwrap();

        gql.query_latest_blocks(1).await.unwrap();
        assert_eq!(backup.hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn max_rounds_returns_an_error_instead_of_looping() {
        let primary = spawn(vec![Reply::Status(500)]).await;
        let backup = spawn(vec![Reply::Status(500)]).await;
        let gql = client(&primary, &[&backup], fast(Some(2)));

        let err = gql.query_latest_blocks(1).await.unwrap_err().to_string();
        assert!(err.contains("after 2 round(s)"), "{err}");
        assert!(err.contains("HTTP status 500"), "{err}");
        assert_eq!(primary.hits.load(Ordering::SeqCst), 6);
        assert_eq!(backup.hits.load(Ordering::SeqCst), 6);
    }

    #[tokio::test]
    async fn legacy_create_client_makes_exactly_one_attempt() {
        let only = spawn(vec![Reply::Status(500), Reply::Json(OK_BLOCKS)]).await;
        let gql = create_client(&only.url).unwrap();

        assert!(gql.query_latest_blocks(1).await.is_err());
        assert_eq!(only.hits.load(Ordering::SeqCst), 1);
        gql.query_latest_blocks(1).await.unwrap();
        assert_eq!(only.hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn endpoint_normalization_and_list_parsing() {
        assert_eq!(normalize_endpoint("bm1:8080"), "http://bm1:8080/graphql");
        assert_eq!(
            normalize_endpoint("https://x.example/"),
            "https://x.example/graphql"
        );
        assert_eq!(
            normalize_endpoint(" https://x.example/graphql "),
            "https://x.example/graphql"
        );
        assert_eq!(parse_endpoint_list(" a , ,b,, "), vec![
            "a".to_string(),
            "b".to_string()
        ]);
        assert!(parse_endpoint_list("").is_empty());

        let gql = create_client_with_failover(
            "https://p.example",
            &[
                "https://p.example/graphql".to_string(),
                "".to_string(),
                "bm2".to_string(),
            ],
            GqlClientConfig::default(),
        )
        .unwrap();
        assert_eq!(gql.endpoints(), &[
            "https://p.example/graphql".to_string(),
            "http://bm2/graphql".to_string()
        ]);
    }

    #[test]
    fn config_from_env_rejects_zero_retries() {
        std::env::set_var(ENV_RETRIES_PER_ENDPOINT, "0");
        let err = GqlClientConfig::from_env().unwrap_err().to_string();
        std::env::remove_var(ENV_RETRIES_PER_ENDPOINT);
        assert!(err.contains(ENV_RETRIES_PER_ENDPOINT), "{err}");
    }
}
