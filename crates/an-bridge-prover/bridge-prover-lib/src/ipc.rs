//! File-based IPC for proof exchange between prover and verifier daemons.
//!
//! Supports both Circuit 1a (primary attestation) and Circuit 2 (layer hashes).

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;
use serde::{Deserialize, Serialize};

const PROOFS_DIR: &str = "proofs";

/// Current `ProofRequest` schema version. Bumped to 2 when `block_height` was
/// added; bumped to 3 when `attestation_circuit` was added so the verifier
/// knows which VK (Primary 1a vs Fallback 1b) to use; bumped to 4 alongside
/// the introduction of the bk-set-update bundle (`BkUpdateRequest`). The
/// layer-bundle wire shape is unchanged between v3 and v4 — the bump just
/// keeps the two file kinds version-synced so the verifier can reject one
/// commit/the-other mismatches loudly instead of silently re-interpreting
/// fields. The verifier rejects mismatched versions instead of silently
/// re-interpreting fields.
pub const PROOF_REQUEST_SCHEMA_VERSION: u32 = 4;

fn default_schema_version() -> u32 { PROOF_REQUEST_SCHEMA_VERSION }

/// Discriminates which attestation circuit produced `primary_proof_hex`.
///
/// Both circuits share the 4-public-instance layout; the verifier picks the
/// matching VK based on this tag. Default (for legacy proof files written
/// before schema v3) is `Primary`, matching the v2 wire shape where the
/// prover only ever emitted Circuit 1a.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AttestationCircuit {
    /// Circuit 1a — single PRIMARY-type attestation, ≥2N/3 signers.
    #[default]
    Primary,
    /// Circuit 1b — paired (PRIMARY prefinalization, FALLBACK target) over
    /// the same block_id, each >N/2 signers. Activated when consensus
    /// fallback path fires (primary deadline β missed).
    Fallback,
}

fn default_attestation_circuit() -> AttestationCircuit { AttestationCircuit::Primary }

/// JSON structure for combined proof files (Circuit 1a or 1b + Circuit 2).
#[derive(Serialize, Deserialize, Debug)]
pub struct ProofRequest {
    /// Wire-format version. v2 added `block_height`. Older (v1) files
    /// implicitly map to `schema_version = 1` via `#[serde(default = ...)]`
    /// but only after they're shaped to fit — see `read_proof_request`.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// Key block sequence number.
    pub block_seq_no: u32,
    /// Thread-anchored `BlockHeight.height` of the key block. In Acki Nacki
    /// `height` resets on thread crossings, so it is NOT interchangeable with
    /// `block_seq_no` in multi-thread chains. This is the value the contract
    /// mirror's per-layer `heights[W]` slot stores.
    pub block_height: u64,
    /// Sequence number of the previously proved key block.
    pub last_seen_block_seqno: u32,
    /// Block ID from the attestation circuit as hex Fr. Same value whether
    /// 1a or 1b emitted the proof — Circuit 1b's same-block_id constraint
    /// guarantees the two attestations in the fallback pair agree.
    pub block_id_hex: String,

    // ---- Attestation circuit (1a Primary or 1b Fallback) ----
    /// Which attestation circuit produced `primary_proof_hex` — discriminates
    /// the verifying key (Primary vs Fallback). Legacy v2 proof files without
    /// this field deserialize as `Primary`.
    #[serde(default = "default_attestation_circuit")]
    pub attestation_circuit: AttestationCircuit,
    /// Hex-encoded attestation-circuit proof bytes (1a or 1b — discriminated
    /// by `attestation_circuit`). Field name preserved across the v3 bump for
    /// backwards-readable JSON; the verifier picks the matching VK by tag.
    pub primary_proof_hex: String,

    // ---- Circuit 2 (Layer Hashes Movement) ----
    /// Hex-encoded Circuit 2 proof bytes.
    pub layer_proof_hex: String,
    /// Block ID from Circuit 2 (Merkle tree root) as hex Fr.
    pub layer_block_id_hex: String,
    /// BK set Poseidon commitment (from node) as hex Fr.
    pub bk_set_poseidon_hash_hex: String,
    /// Number of active layers (1..=10).
    pub num_layers: u8,
    /// Layer hash Fr values (10 entries, inactive = zero Fr) as hex.
    pub layer_hash_frs_hex: Vec<String>,
    /// Previous max-level layer hash as hex Fr.
    pub prev_max_level_layer_hash_hex: String,

    // ---- Proof generation timings (added: per-circuit wall-clock, ms) ----
    /// Wall-clock time spent generating the Circuit 1a (primary) proof, in
    /// milliseconds. Excludes PK load/unload. `#[serde(default)]` so older
    /// proof JSONs (without this field) still deserialize as 0.
    #[serde(default)]
    pub primary_proof_gen_ms: u64,
    /// Wall-clock time spent generating the Circuit 2 (layer) proof, in
    /// milliseconds. Excludes PK load/unload.
    #[serde(default)]
    pub layer_proof_gen_ms: u64,
}

/// JSON structure for verification result files.
#[derive(Serialize, Deserialize, Debug)]
pub struct VerifyResult {
    pub block_seq_no: u32,
    pub primary_verified: bool,
    pub layer_verified: bool,
    pub error: Option<String>,
}

pub fn proof_file_path(seq_no: u32) -> String {
    format!("{}/proof_{:06}.json", PROOFS_DIR, seq_no)
}

pub fn result_file_path(seq_no: u32) -> String {
    format!("{}/result_{:06}.json", PROOFS_DIR, seq_no)
}

pub fn ensure_proofs_dir() {
    std::fs::create_dir_all(PROOFS_DIR).ok();
}

/// Write a combined proof (Circuit 1a + Circuit 2) for the verifier.
pub fn write_combined_proof(request: &ProofRequest) -> anyhow::Result<()> {
    ensure_proofs_dir();
    let json = serde_json::to_string_pretty(request)?;
    std::fs::write(proof_file_path(request.block_seq_no), json)?;
    Ok(())
}

/// Wait for a verifier result file to appear.
pub async fn wait_for_result(seq_no: u32, timeout: Duration) -> anyhow::Result<VerifyResult> {
    let path = result_file_path(seq_no);
    let start = std::time::Instant::now();
    loop {
        if Path::new(&path).exists() {
            let data = std::fs::read_to_string(&path).context("failed to read result file")?;
            return serde_json::from_str(&data).context("failed to parse result JSON");
        }
        if start.elapsed() > timeout {
            anyhow::bail!("timeout waiting for verifier result for seq_no={}", seq_no);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Read a proof request file (used by verifier). Rejects schema versions the
/// daemon was not built for — a mismatch almost certainly means the prover
/// and verifier are on different commits, which would silently mis-mirror
/// state if we just re-interpreted fields.
pub fn read_proof_request(seq_no: u32) -> anyhow::Result<ProofRequest> {
    let path = proof_file_path(seq_no);
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read proof file: {}", path))?;
    let req: ProofRequest = serde_json::from_str(&data)
        .context("failed to parse proof request JSON")?;
    if req.schema_version != PROOF_REQUEST_SCHEMA_VERSION {
        anyhow::bail!(
            "proof file {} has schema_version={} but verifier expects {}",
            path,
            req.schema_version,
            PROOF_REQUEST_SCHEMA_VERSION
        );
    }
    Ok(req)
}

/// Write a verification result (used by verifier).
pub fn write_result(result: &VerifyResult) -> anyhow::Result<()> {
    ensure_proofs_dir();
    let json = serde_json::to_string_pretty(result)?;
    std::fs::write(result_file_path(result.block_seq_no), json)?;
    Ok(())
}

/// Parse an Fr from a hex string (32-byte LE representation).
pub fn fr_from_hex(hex_str: &str) -> anyhow::Result<Fr> {
    let bytes = hex::decode(hex_str).context("invalid hex")?;
    if bytes.len() != 32 {
        anyhow::bail!("expected 32 bytes for Fr, got {}", bytes.len());
    }
    let mut repr = [0u8; 32];
    repr.copy_from_slice(&bytes);
    Option::from(Fr::from_repr(repr)).ok_or_else(|| anyhow::format_err!("invalid Fr repr"))
}

/// Encode an Fr value as hex string (32-byte LE representation).
pub fn fr_to_hex(fr: &Fr) -> String {
    hex::encode(fr.to_repr().as_ref())
}

// ---------------------------------------------------------------------------
// BK-set update IPC bundle (schema v4)
// ---------------------------------------------------------------------------

/// File-name prefix for bk-set-update bundles. The prover writes
/// `proofs/bkupd_NNNNNN.json` (six-digit zero-padded `block_seq_no`) and the
/// verifier writes the matching `proofs/bkupd_result_NNNNNN.json`. The
/// distinct prefix lets the verifier scan layer bundles and bk-update
/// bundles independently — no in-file kind discriminator needed.
const BKUPD_PREFIX: &str = "bkupd";

/// JSON structure for a bk-set-update bundle. Carries exactly what the
/// future Ethereum bridge contract's `applyBkSetUpdate` entry point will
/// receive: the Circuit 1a/1b attestation proof (binding `block_id` under
/// the OLD commitment) plus the open SHA-256 Merkle siblings revealing
/// `L2 = old_bk_set_poseidon_hash` and `L3 = new_bk_set_poseidon_hash` as
/// leaves of the 8-leaf block-id tree.
///
/// **No pubkey list.** The verifier daemon mirrors the contract state,
/// which only stores the commitment. The full pubkey table is the prover's
/// private working data (`ProverBkSet`) and never travels in IPC.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BkUpdateRequest {
    /// Wire-format version. Same constant as `ProofRequest`, so a mismatch
    /// reliably signals a prover/verifier commit drift.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// Bk-set-update block sequence number. The seq_no of the block that
    /// announces the new BK set (i.e. the block whose `L2 != L3`).
    pub block_seq_no: u32,
    /// Thread-anchored `BlockHeight.height` of the bk-update block.
    pub block_height: u64,
    /// Seq_no of the previously applied bk-update (i.e. the verifier's
    /// `stored_last_bk_set_update_seq_no` at the time this bundle is
    /// produced). The verifier checks `block_seq_no > this`.
    pub last_seen_bk_update_seqno: u32,
    /// Block ID emitted by the attestation circuit, as hex Fr (32-byte LE
    /// `Fr::to_repr()`). Bound by the Circuit 1a/1b proof as its first public
    /// instance. NOT used for the SHA-256 Merkle check (Fr reduction can lose
    /// the top 2 bits when the chain hash exceeds the Fr modulus).
    pub block_id_hex: String,
    /// Chain's raw 32-byte block hash (= GraphQL `Block.id` = SHA-256 root of
    /// the 8-leaf `block_merkle_tree_leaves`). Used by the verifier to check
    /// `root(L2, L3, H0, H23) == this`. This is the value the future
    /// `applyBkSetUpdate` Solidity entry point will receive on-chain.
    #[serde(default)]
    pub block_id_hash_hex: String,

    // ---- Attestation circuit (1a Primary or 1b Fallback) ----
    /// Which attestation circuit produced `primary_proof_hex`.
    #[serde(default = "default_attestation_circuit")]
    pub attestation_circuit: AttestationCircuit,
    /// Hex-encoded attestation-circuit proof bytes. Verified against
    /// public instances `[block_id, L2, block_seq_no, last_seen]` —
    /// `L2` must equal the verifier's `stored_bk_set_commitment`, which is
    /// what authorises the update.
    pub primary_proof_hex: String,

    // ---- OPEN bk-set update payload ----
    /// L2 = old BK-set Poseidon commitment, as hex (32 bytes LE).
    /// At verify time: `L2 == state.stored_bk_set_commitment`.
    pub old_bk_set_poseidon_hash_hex: String,
    /// L3 = new BK-set Poseidon commitment, as hex (32 bytes LE).
    /// At verify time: becomes the new `stored_bk_set_commitment`.
    pub new_bk_set_poseidon_hash_hex: String,
    /// Merkle sibling H0 = SHA256(L0 ‖ L1), as hex (32 bytes).
    pub merkle_sibling_h0_hex: String,
    /// Merkle sibling H23 = SHA256(H2 ‖ H3), as hex (32 bytes).
    pub merkle_sibling_h23_hex: String,

    // ---- Timings (optional, for the prover's heartbeat log) ----
    #[serde(default)]
    pub primary_proof_gen_ms: u64,
}

/// JSON structure for a bk-update verification result. Mirrors
/// `VerifyResult` but explicitly carries the three independent checks so
/// the prover (or operator) can tell which one failed.
#[derive(Serialize, Deserialize, Debug)]
pub struct BkUpdateResult {
    pub block_seq_no: u32,
    /// Circuit 1a/1b attestation verification passed.
    pub attestation_verified: bool,
    /// `root = SHA(SHA(H0‖SHA(L2‖L3))‖H23) == block_id` check passed.
    pub merkle_verified: bool,
    /// `block_seq_no > stored_last_bk_set_update_seq_no` check passed.
    pub monotonicity_ok: bool,
    /// Convenience: all three above are true.
    pub verify_ok: bool,
    pub error: Option<String>,
}

/// Path of the proof JSON file the prover writes for a bk-update bundle.
pub fn bkupd_file_path(seq_no: u32) -> String {
    format!("{}/{}_{:06}.json", PROOFS_DIR, BKUPD_PREFIX, seq_no)
}

/// Path of the result JSON file the verifier writes for a bk-update bundle.
pub fn bkupd_result_file_path(seq_no: u32) -> String {
    format!("{}/{}_result_{:06}.json", PROOFS_DIR, BKUPD_PREFIX, seq_no)
}

/// Write a bk-update bundle (prover side).
pub fn write_bk_update_request(req: &BkUpdateRequest) -> anyhow::Result<()> {
    ensure_proofs_dir();
    let json = serde_json::to_string_pretty(req)?;
    std::fs::write(bkupd_file_path(req.block_seq_no), json)?;
    Ok(())
}

/// Read a bk-update bundle (verifier side). Same strict version check as
/// `read_proof_request` — a mismatch almost certainly means daemons are on
/// different commits.
pub fn read_bk_update_request(seq_no: u32) -> anyhow::Result<BkUpdateRequest> {
    let path = bkupd_file_path(seq_no);
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read bk-update file: {}", path))?;
    let req: BkUpdateRequest = serde_json::from_str(&data)
        .context("failed to parse bk-update request JSON")?;
    if req.schema_version != PROOF_REQUEST_SCHEMA_VERSION {
        anyhow::bail!(
            "bk-update file {} has schema_version={} but verifier expects {}",
            path,
            req.schema_version,
            PROOF_REQUEST_SCHEMA_VERSION
        );
    }
    Ok(req)
}

/// Write a bk-update verification result (verifier side).
pub fn write_bk_update_result(r: &BkUpdateResult) -> anyhow::Result<()> {
    ensure_proofs_dir();
    let json = serde_json::to_string_pretty(r)?;
    std::fs::write(bkupd_result_file_path(r.block_seq_no), json)?;
    Ok(())
}

/// Wait for a verifier bk-update result file to appear. Same poll/timeout
/// semantics as `wait_for_result`.
pub async fn wait_for_bk_update_result(
    seq_no: u32,
    timeout: Duration,
) -> anyhow::Result<BkUpdateResult> {
    let path = bkupd_result_file_path(seq_no);
    let start = std::time::Instant::now();
    loop {
        if Path::new(&path).exists() {
            let data = std::fs::read_to_string(&path)
                .context("failed to read bk-update result file")?;
            return serde_json::from_str(&data)
                .context("failed to parse bk-update result JSON");
        }
        if start.elapsed() > timeout {
            anyhow::bail!(
                "timeout waiting for verifier bk-update result for seq_no={}",
                seq_no
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bkupd_paths_match_pattern() {
        assert_eq!(bkupd_file_path(42), "proofs/bkupd_000042.json");
        assert_eq!(bkupd_result_file_path(42), "proofs/bkupd_result_000042.json");
    }

    #[test]
    fn bkupd_request_roundtrip_preserves_v4() {
        let req = BkUpdateRequest {
            schema_version: PROOF_REQUEST_SCHEMA_VERSION,
            block_seq_no: 1024,
            block_height: 1024,
            last_seen_bk_update_seqno: 0,
            block_id_hex: "ab".repeat(32),
            attestation_circuit: AttestationCircuit::Primary,
            primary_proof_hex: "00".to_string(),
            old_bk_set_poseidon_hash_hex: "cc".repeat(32),
            new_bk_set_poseidon_hash_hex: "dd".repeat(32),
            merkle_sibling_h0_hex: "ee".repeat(32),
            merkle_sibling_h23_hex: "ff".repeat(32),
            primary_proof_gen_ms: 12345,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: BkUpdateRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema_version, 4);
        assert_eq!(back.block_seq_no, 1024);
        assert_eq!(back.attestation_circuit, AttestationCircuit::Primary);
        assert_eq!(back.old_bk_set_poseidon_hash_hex, "cc".repeat(32));
        assert_eq!(back.new_bk_set_poseidon_hash_hex, "dd".repeat(32));
    }
}
