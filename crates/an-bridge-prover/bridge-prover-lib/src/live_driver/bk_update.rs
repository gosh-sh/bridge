//! Single-iteration bk-set-update handler.
//!
//! Ported from `bridge-prover-daemon/src/main.rs:418-709` (the "Phase 3
//! BK-set-update drain" loop). This function does exactly one drain step:
//! it looks for the next rotation event past
//! `state.stored_last_bk_set_update_seq_no`, validates the L2/L3 Merkle
//! constraints, generates the Circuit 1A/1B proof against the OLD set, and
//! returns the artifacts. Caller advances the driver's cursor via
//! [`super::LiveProverDriver::ack_bk_update`] after successful downstream
//! verification.
//!
//! Differences from the pre-refactor main.rs:
//!
//! * No `ipc::write_bk_update_request` / `wait_for_bk_update_result` — the
//!   caller drives IPC / on-chain submission and passes the outcome to
//!   `ack_bk_update`.
//! * No `state.apply_bk_set_update` / `prover_bk_set.rotate` / in-memory
//!   `bk_set` refresh — those all move into `ack_bk_update`.
//! * All state mutation is deferred to ack, so a caller that drops the
//!   returned artifacts (e.g. downstream verification failed) leaves the
//!   driver ready to re-emit the same event on the next poll — that's the
//!   intended retry semantics.

use anyhow::Context;
use tracing::{error, info, warn};
use std::time::Instant;

use bridge_gql_fetcher::attestation_fetcher::{self, AttestationEvidence};
use bridge_gql_fetcher::bk_set_fetcher::{self, BK_CHANGE_VARIANT_ADDED, BK_CHANGE_VARIANT_REMOVED};
use crate::block_id_tree::BlockIdMerkleTree;
use bridge_poseidon as poseidon;
use crate::prover;

use super::{BkUpdateProofArtifacts, BundleFinalizationType, LiveProverDriver};

/// Drive one bk-set-update step. Returns `Some(artifacts)` when a rotation
/// is ready for downstream submission, `None` when the prover is caught up
/// (no pending rotation).
///
/// On any transient GQL / decode failure, returns `Ok(None)` after logging
/// so the caller can retry on the next poll cycle. Only genuinely fatal
/// errors (proof gen failure, structural mismatch) propagate.
pub(super) async fn drive_next_bk_update(
    driver: &mut LiveProverDriver,
) -> anyhow::Result<Option<BkUpdateProofArtifacts>> {
    let cursor = driver.state().stored_last_bk_set_update_seq_no;
    let upd = match bk_set_fetcher::next_update_after(driver.gql(), cursor).await {
        Ok(Some(u)) => u,
        Ok(None) => return Ok(None), // caught up on rotations
        Err(e) => {
            warn!(
                "bk-update drain: next_update_after failed ({}); returning None so caller retries",
                e,
            );
            return Ok(None);
        }
    };
    let upd_seqno = match upd.height {
        Some(h) if h > cursor => h,
        _ => {
            warn!("bk-update drain: skipping event with missing/stale height");
            return Ok(None);
        }
    };

    info!("=== bk-update drain: processing event at seq_no {} ===", upd_seqno);

    // Fetch the bk-update block's 16 Merkle leaves to derive L2/L3 and the
    // three open siblings h01 / h4_7 / h8_15 for the depth-4 fold.
    let upd_block = driver
        .gql()
        .query_proof_block_by_seqno(upd_seqno)
        .await
        .with_context(|| format!("bk-update {}: GQL block fetch", upd_seqno))?;
    let leaves = upd_block
        .block_merkle_tree_leaves
        .ok_or_else(|| {
            anyhow::anyhow!(
                "bk-update {}: block has no block_merkle_tree_leaves",
                upd_seqno,
            )
        })?;
    let tree = BlockIdMerkleTree::from_leaves(leaves);
    let l2 = tree.leaves[2];
    let l3 = tree.leaves[3];

    // Cross-check: L2 must equal our currently-applied commitment. If not,
    // the prover is out-of-sync with the chain's bk-set history — bail so
    // an operator investigates.
    let cur_commitment = driver.prover_bk_set().commitment;
    if l2 != cur_commitment {
        anyhow::bail!(
            "bk-update {}: L2 {} != prover_bk_set.commitment {} — prover out of sync",
            upd_seqno,
            hex::encode(l2),
            hex::encode(cur_commitment),
        );
    }

    // Apply the bk_set_update_hex deltas to the OLD pubkey table to derive
    // the NEW one, and verify it hashes to L3.
    let blob = hex::decode(&upd.bk_set_update_hex)
        .with_context(|| format!("bk-update {}: bk_set_update_hex decode", upd_seqno))?;
    let changes = bk_set_fetcher::parse_bk_set_changes_pub(&blob);
    if changes.is_empty() {
        anyhow::bail!("bk-update {}: parsed 0 changes from blob", upd_seqno);
    }
    let cur_pubkeys = driver
        .prover_bk_set()
        .pubkeys()
        .with_context(|| format!("bk-update {}: prover_bk_set.pubkeys()", upd_seqno))?;
    let mut new_pubkeys = cur_pubkeys.clone();
    for (variant, idx, pk) in &changes {
        match *variant {
            BK_CHANGE_VARIANT_ADDED => {
                new_pubkeys.insert(*idx, pk.clone());
            }
            BK_CHANGE_VARIANT_REMOVED => {
                new_pubkeys.remove(idx);
            }
            other => {
                warn!(
                    "bk-update {}: ignoring unknown change variant {}",
                    upd_seqno, other,
                );
            }
        }
    }
    // Delta entries arrive as 96-byte uncompressed BLS pubkeys; the base
    // set is 48-byte compressed. Normalize before feeding to any BLS-aware
    // helper.
    let new_pubkeys = bk_set_fetcher::normalize_bk_set_pubkeys(new_pubkeys)
        .with_context(|| format!("bk-update {}: pubkey normalization", upd_seqno))?;
    let (_, recomp_c) = poseidon::compute_bk_set_poseidon(&new_pubkeys);
    if recomp_c != l3 {
        anyhow::bail!(
            "bk-update {}: Poseidon(new_pubkeys) {} != L3 {}",
            upd_seqno,
            hex::encode(recomp_c),
            hex::encode(l3),
        );
    }

    // Fetch attestation evidence for the bk-update block. These signatures
    // are produced by the OLD signer set (the block that *announces* the
    // rotation is itself signed by the prior set), so the Circuit 1A/1B
    // witness uses `cur_pubkeys`.
    let upd_evidence =
        match attestation_fetcher::fetch_attestation_evidence(driver.gql(), upd_seqno as u32).await
        {
            Ok(ev) => ev,
            Err(e) => {
                warn!(
                    "bk-update {}: attestation fetch failed ({}); returning None so caller retries",
                    upd_seqno, e,
                );
                return Ok(None);
            }
        };

    // Generate Circuit 1A/1B proof, on-demand PK load/unload to stay within
    // the single-PK memory envelope.
    let last_seen_for_upd = driver.state().stored_last_bk_set_update_seq_no as u32;
    let t_upd_proof = Instant::now();
    let (fin_type, upd_proof) = match &upd_evidence {
        AttestationEvidence::Primary(att) => {
            info!("bk-update {}: PRIMARY path → Circuit 1a", upd_seqno);
            driver.key_manager_mut().load_primary_pk().with_context(|| {
                format!("bk-update {}: load_primary_pk", upd_seqno)
            })?;
            let res = prover::generate_primary_proof(
                driver.key_manager_mut(),
                &att.raw_bytes,
                &cur_pubkeys,
                last_seen_for_upd,
            );
            driver.key_manager_mut().unload_primary_pk();
            match res {
                Ok(o) => (BundleFinalizationType::Primary, o),
                Err(e) => {
                    error!("bk-update {}: Circuit 1a proof failed: {}", upd_seqno, e);
                    return Err(e);
                }
            }
        }
        AttestationEvidence::Fallback { primary, fallback } => {
            info!("bk-update {}: FALLBACK path → Circuit 1b", upd_seqno);
            driver.key_manager_mut().load_fallback_pk().with_context(|| {
                format!("bk-update {}: load_fallback_pk", upd_seqno)
            })?;
            let res = prover::generate_fallback_proof(
                driver.key_manager_mut(),
                &primary.raw_bytes,
                &fallback.raw_bytes,
                &cur_pubkeys,
                last_seen_for_upd,
            );
            driver.key_manager_mut().unload_fallback_pk();
            match res {
                Ok(o) => (BundleFinalizationType::Fallback, o),
                Err(e) => {
                    error!("bk-update {}: Circuit 1b proof failed: {}", upd_seqno, e);
                    return Err(e);
                }
            }
        }
    };
    let primary_proof_gen_ms = t_upd_proof.elapsed().as_millis() as u64;
    info!(
        "bk-update {}: {} proof generated in {} ms",
        upd_seqno,
        fin_type.as_str(),
        primary_proof_gen_ms,
    );

    // The pre-refactor daemon assembled an `ipc::BkUpdateRequest` here; we
    // return the same fields as a transport-agnostic payload. The caller
    // maps this into the transport of its choice. Since schema v6 there is a
    // single `block_id_be` (raw 32-byte SHA-256 root); the Fr form is
    // derived on demand by the verifier via `ipc::hash_hex_to_fr` and by the
    // on-chain Yul via `mod(calldataload, f_q)`. Debug-assert that the
    // circuit's committed Fr agrees with the fold of the raw hash so a
    // byte-order regression pages loudly at build time.
    debug_assert_eq!(
        crate::ipc::fold_hash_be_to_fr(&tree.root),
        upd_proof.block_id_fr,
        "bk-update: fold(reverse(tree.root)) must equal Circuit 1's committed \
         block_id_fr; a mismatch means the wire hash and the proof disagree",
    );
    let l2_l3_siblings = tree.siblings_for_l2_l3();
    Ok(Some(BkUpdateProofArtifacts {
        block_seq_no: upd_seqno,
        block_height: upd_block.height,
        last_seen_bk_update_seq_no: last_seen_for_upd as u64,
        block_id_be: tree.root,
        fin_type,
        old_bk_set_commitment_be: l2,
        new_bk_set_commitment_be: l3,
        merkle_sibling_h01_be: l2_l3_siblings[0],
        merkle_sibling_h4_7_be: l2_l3_siblings[1],
        merkle_sibling_h8_15_be: l2_l3_siblings[2],
        attestation_proof: upd_proof.proof_bytes,
        new_pubkeys,
        primary_proof_gen_ms,
    }))
}
