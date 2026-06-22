use anyhow::{bail, Context};
use serde_json::{json, Value};

/// Lightweight GraphQL client for the acki-nacki node.
pub struct GqlClient {
    http: reqwest::Client,
    url: String,
}

pub fn create_client(endpoint: &str) -> anyhow::Result<GqlClient> {
    let url = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        if endpoint.trim_end_matches('/').ends_with("/graphql") {
            endpoint.to_string()
        } else {
            format!("{}/graphql", endpoint.trim_end_matches('/'))
        }
    } else {
        format!("http://{}/graphql", endpoint)
    };
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("failed to create HTTP client")?;
    Ok(GqlClient { http, url })
}

impl GqlClient {
    pub async fn query(&self, query: &str) -> anyhow::Result<Value> {
        let resp = self
            .http
            .post(&self.url)
            .json(&json!({ "query": query }))
            .send()
            .await
            .context("GraphQL request failed")?;
        let body: Value = resp.json().await.context("failed to decode GraphQL response")?;
        if let Some(errors) = body.get("errors") {
            bail!("GraphQL error: {}", errors);
        }
        body.get("data")
            .cloned()
            .ok_or_else(|| anyhow::format_err!("no 'data' field in GraphQL response"))
    }

    /// Fetch the latest N blocks (hash + seq_no).
    pub async fn query_latest_blocks(&self, count: u32) -> anyhow::Result<Vec<(String, u64)>> {
        let q = format!(
            r#"{{ blockchain {{ blocks(last: {count}) {{ edges {{ node {{ hash seq_no }} }} }} }} }}"#
        );
        let data = self.query(&q).await?;
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
        let data = self.query(&q).await?;
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
                });
            }
        }
        Ok(updates)
    }

    /// Fetch the last N bkSetUpdates (most recent).
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
        let data = self.query(&q).await?;
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
    pub async fn query_block_metadata(
        &self,
        seq_no: u64,
    ) -> anyhow::Result<BlockMetadata> {
        let tid = "00000000000000000000000000000000000000000000000000000000000000000000";
        let q = format!(
            r#"{{ blockchain {{ blockByHeight(thread_id: "{tid}", height: {seq_no}) {{ hash envelope_hash seq_no }} }} }}"#
        );
        let data = self.query(&q).await?;
        let block = data
            .pointer("/blockchain/blockByHeight")
            .ok_or_else(|| anyhow::format_err!("blockByHeight returned null for seq_no={}", seq_no))?;
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
        let data = self.query(&q).await?;
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

#[derive(Debug, Clone)]
pub struct BkSetUpdateWithAttestations {
    pub block_id: String,
    pub bk_set_update_hex: String,
    pub height: Option<u64>,
    pub attestations: Vec<GqlAttestation>,
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
        })
    }
}

use std::collections::BTreeMap;
use crate::poseidon_dense::{compute_block_leaf_hash, LayerNumber};
use crate::types::{AccountRouting, ThreadIdentifier};

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
    pub history_proofs: BTreeMap<LayerNumber, [u8; 32]>,
    /// 8-leaf SHA-256 block-id Merkle leaves. May be absent on very old blocks.
    pub block_merkle_tree_leaves: Option<[[u8; 32]; 8]>,
}

impl GqlProofBlock {
    pub fn block_leaf_hash(&self) -> [u8; 32] {
        compute_block_leaf_hash(
            &self.block_id,
            &self.envelope_hash,
            &self.tracked_ext_out_messages_root,
        )
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
        let data = self.query(&q).await?;
        let block = data
            .pointer("/blockchain/blockByHeight")
            .ok_or_else(|| anyhow::format_err!("blockByHeight returned no field for height={height}"))?;
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
            .query(&q)
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

    /// Legacy single-attestation accessor kept for callers that still want
    /// the v2 shape. Picks the `PRIMARY`-typed entry if one exists, else
    /// returns the first parseable entry (preserving the previous "be
    /// defensive" behavior).
    pub async fn query_attestation_envelope(
        &self,
        target_seq_no: u64,
    ) -> anyhow::Result<crate::attestation_fetcher::ParsedAttestation> {
        let mut atts = self.query_attestation_envelopes(target_seq_no).await?;
        if let Some(idx) = atts.iter().position(|a| a.target_type == 0) {
            return Ok(atts.swap_remove(idx));
        }
        atts.into_iter()
            .next()
            .ok_or_else(|| anyhow::format_err!("no attestations on block {target_seq_no}"))
    }
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
    let mut history_proofs: BTreeMap<LayerNumber, [u8; 32]> = BTreeMap::new();
    if let Some(arr) = value.get("history_proofs").and_then(|v| v.as_array()) {
        for entry in arr {
            let layer = parse_u64_field(entry, "layer")?;
            anyhow::ensure!(layer <= u8::MAX as u64, "history proof layer out of range");
            let root_hash =
                decode_hash32(required_string(entry, "root_hash")?).context("history proof root_hash")?;
            history_proofs.insert(layer as u8, root_hash);
        }
    }

    // block_merkle_tree_leaves -> Option<[[u8;32]; 8]>
    let block_merkle_tree_leaves = match value.get("block_merkle_tree_leaves") {
        Some(serde_json::Value::Array(items)) => {
            anyhow::ensure!(items.len() == 8, "block_merkle_tree_leaves must have 8 entries");
            let mut out = [[0u8; 32]; 8];
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
