//! Single-iteration bk-set-update handler.
//! This function does exactly one drain step:
//! it looks for the next rotation event past
//! `state.stored_last_bk_set_update_seq_no` and prepares it — fetches the
//! rotation block, checks its block-id tree, and applies the block's changes
//! to the driver's set the way the node does, checking leaf `[2]` before and
//! leaf `[3]` after. A rotation the contract has already applied stops there
//! and comes back as a [`BkUpdateDelta`] without a proof; any other one gets
//! the Circuit 1A/1B proof against the OLD set. Caller advances the driver's
//! cursor via [`super::LiveProverDriver::ack_bk_update`] or
//! [`super::LiveProverDriver::ack_bk_delta`] after successful downstream
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

use std::{collections::HashMap, time::Instant};

use anyhow::Context;
use bridge_gql_fetcher::{
    attestation_fetcher::{self, AttestationEvidence},
    bk_set_fetcher,
};
use tracing::{error, info, warn};

use super::{
    BkUpdateDelta, BkUpdateProofArtifacts, BundleFinalizationType, DriverError, DriverResult,
    LiveBkUpdateEvent, LiveProverDriver,
};
use crate::{
    bk_set_replay::{self, ReplayError},
    block_id_tree::BlockIdMerkleTree,
    prover,
};

/// `lastSeen` of a rotation proof: the driver's layer cursor, but below the
/// rotation. Circuit 1A/1B requires the rotation's height above `lastSeen`,
/// and `applyBkSetUpdate` takes any value below it; the bundle at the
/// rotation's own height may have landed before the rotation.
pub(crate) fn rotation_last_seen(layer_cursor: u64, rotation_seq_no: u64) -> u64 {
    layer_cursor.min(rotation_seq_no.saturating_sub(1))
}

/// The set after a rotation row. `current` must hash to `l2`; the row's
/// changes are applied the way the node does (every removal, then every
/// addition), and the result must hash to `l3`. `height` labels errors.
pub(crate) fn next_set_for_row(
    current: &HashMap<u16, Vec<u8>>,
    blob_hex: &str,
    l2: [u8; 32],
    l3: [u8; 32],
    height: u64,
) -> Result<HashMap<u16, Vec<u8>>, ReplayError> {
    expect_commitment(height, "l2-before-apply", l2, current)?;
    let changes = bk_set_replay::parse_changes(blob_hex)?;
    let mut next = current.clone();
    bk_set_replay::apply_block_changes(&mut next, &changes)?;
    expect_commitment(height, "l3-after-apply", l3, &next)?;
    Ok(next)
}

fn expect_commitment(
    height: u64,
    step: &'static str,
    expected: [u8; 32],
    set: &HashMap<u16, Vec<u8>>,
) -> Result<(), ReplayError> {
    let actual = bk_set_replay::commitment(set);
    if actual == expected {
        Ok(())
    } else {
        Err(ReplayError::Mismatch {
            height,
            step,
            expected,
            actual,
        })
    }
}

/// A row that does not lead from the driver's set to the block's leaf `[3]`.
/// A wrong leaf `[2]` means the driver is out of sync with the chain; any
/// other failure is bad data from the node.
fn row_error(seq_no: u64, e: ReplayError) -> DriverError {
    match &e {
        ReplayError::Mismatch {
            step: "l2-before-apply",
            ..
        } => DriverError::state_inconsistent(anyhow::anyhow!(
            "bk-update {seq_no}: {e} — prover out of sync"
        )),
        _ => DriverError::gql_schema(anyhow::anyhow!("bk-update {seq_no}: {e}")),
    }
}

/// The next rotation row past the driver's cursor: its height and change
/// blob. `None` when caught up, and when the lookup failed (logged; the
/// caller retries on the next poll).
async fn next_row(driver: &LiveProverDriver) -> Option<(u64, String)> {
    let cursor = driver.state().stored_last_bk_set_update_seq_no;
    let upd = match bk_set_fetcher::next_update_after(driver.gql(), cursor).await {
        Ok(Some(u)) => u,
        Ok(None) => return None, // caught up on rotations
        Err(e) => {
            warn!(
                "bk-update drain: next_update_after failed ({}); returning None so caller retries",
                e,
            );
            return None;
        }
    };
    match upd.height {
        Some(h) if h > cursor => Some((h, upd.bk_set_update_hex)),
        _ => {
            warn!("bk-update drain: skipping event with missing/stale height");
            None
        },
    }
}

/// A rotation checked against its block, not proved.
struct PreparedRotation {
    seq_no: u64,
    block_height: u64,
    tree: BlockIdMerkleTree,
    /// The set that signs the rotation block.
    old_pubkeys: HashMap<u16, Vec<u8>>,
    new_pubkeys: HashMap<u16, Vec<u8>>,
}

impl PreparedRotation {
    fn l2(&self) -> [u8; 32] {
        self.tree.leaves[2]
    }

    fn l3(&self) -> [u8; 32] {
        self.tree.leaves[3]
    }

    fn into_delta(self) -> BkUpdateDelta {
        BkUpdateDelta {
            block_seq_no: self.seq_no,
            block_height: self.block_height,
            old_bk_set_commitment_be: self.l2(),
            new_bk_set_commitment_be: self.l3(),
            new_pubkeys: self.new_pubkeys,
        }
    }
}

/// Drive one bk-set-update step: [`LiveBkUpdateEvent::Applied`] when the
/// next rotation is at or below `applied_on_chain_up_to` (prepared, not
/// proved), [`LiveBkUpdateEvent::BkUpdate`] when it is ready for downstream
/// submission, [`LiveBkUpdateEvent::Nothing`] when the prover is caught up
/// (no pending rotation).
///
/// On any transient GQL / decode failure, returns `Nothing` after logging
/// so the caller can retry on the next poll cycle. Only genuinely fatal
/// errors (proof gen failure, structural mismatch) propagate. Fatal errors
/// are classified into [`DriverError`] variants at each site so consumers
/// can pattern-match on failure kind without inspecting the message.
pub(super) async fn drive_next_bk_update(
    driver: &mut LiveProverDriver,
    applied_on_chain_up_to: u64,
) -> DriverResult<LiveBkUpdateEvent> {
    let Some((upd_seqno, blob_hex)) = next_row(driver).await else {
        return Ok(LiveBkUpdateEvent::Nothing);
    };

    info!("=== bk-update drain: processing event at seq_no {} ===", upd_seqno);

    let rotation = prepare(driver, upd_seqno, &blob_hex).await?;
    if upd_seqno <= applied_on_chain_up_to {
        info!(
            "bk-update {}: applied on chain (up to {}); taken without a proof",
            upd_seqno, applied_on_chain_up_to,
        );
        return Ok(LiveBkUpdateEvent::Applied(rotation.into_delta()));
    }
    Ok(match prove(driver, rotation).await? {
        Some(artifacts) => LiveBkUpdateEvent::BkUpdate(artifacts),
        None => LiveBkUpdateEvent::Nothing,
    })
}

/// The next rotation past the driver's cursor, prepared, if it is at or
/// below `applied_on_chain_up_to`. Never proves; a rotation above that
/// height is not prepared either.
pub(super) async fn drive_next_applied_bk_update(
    driver: &LiveProverDriver,
    applied_on_chain_up_to: u64,
) -> DriverResult<Option<BkUpdateDelta>> {
    let Some((upd_seqno, blob_hex)) = next_row(driver).await else {
        return Ok(None);
    };
    if upd_seqno > applied_on_chain_up_to {
        return Ok(None);
    }
    Ok(Some(
        prepare(driver, upd_seqno, &blob_hex).await?.into_delta(),
    ))
}

/// Fetch the rotation block, check its tree against the header, and build
/// the set after it from the driver's set ([`next_set_for_row`]). Reads no
/// keys and changes no driver state.
async fn prepare(
    driver: &LiveProverDriver,
    upd_seqno: u64,
    blob_hex: &str,
) -> DriverResult<PreparedRotation> {
    // Fetch the bk-update block's 16 Merkle leaves to derive L2/L3 and the
    // three open siblings h01 / h4_7 / h8_15 for the depth-4 fold.
    let upd_block = driver
        .gql()
        .query_proof_block_by_seqno(upd_seqno)
        .await
        .with_context(|| format!("bk-update {}: GQL block fetch", upd_seqno))
        .map_err(DriverError::gql_transient)?;
    let leaves = upd_block
        .block_merkle_tree_leaves
        .ok_or_else(|| {
            DriverError::gql_schema(anyhow::anyhow!(
                "bk-update {}: block has no block_merkle_tree_leaves",
                upd_seqno,
            ))
        })?;
    let tree = BlockIdMerkleTree::from_leaves(leaves);

    // Structural sanity: the tree we just folded must agree with the
    // block_id the node reports in the same GQL response. The verifier's
    // step 2b (fold(root) == circuit-committed block_id_fr) already guards
    // the security invariant end-to-end, so this is not a correctness fix.
    // The value is fail-fast (skip Circuit 1a proof gen + IPC roundtrip on
    // broken leaves) and release-build parity (the `debug_assert_eq!`
    // further down is compiled out in release; this bail! stays in).
    if tree.root != upd_block.block_id {
        return Err(DriverError::gql_schema(anyhow::anyhow!(
            "bk-update {}: reconstructed tree.root {} != upd_block.block_id {} — \
             GQL leaves inconsistent with block header",
            upd_seqno,
            hex::encode(tree.root),
            hex::encode(upd_block.block_id),
        )));
    }

    // The OLD set must hash to L2; the block's changes, applied to it in the
    // node's order, must hash to L3. A wrong L2 means the prover is
    // out-of-sync with the chain's bk-set history — bail so an operator
    // investigates.
    let old_pubkeys = driver
        .prover_bk_set()
        .pubkeys()
        .with_context(|| format!("bk-update {}: prover_bk_set.pubkeys()", upd_seqno))
        .map_err(DriverError::state_inconsistent)?;
    let new_pubkeys = next_set_for_row(
        &old_pubkeys,
        blob_hex,
        tree.leaves[2],
        tree.leaves[3],
        upd_seqno,
    )
    .map_err(|e| row_error(upd_seqno, e))?;

    Ok(PreparedRotation {
        seq_no: upd_seqno,
        block_height: upd_block.height,
        tree,
        old_pubkeys,
        new_pubkeys,
    })
}

/// Prove a prepared rotation with Circuit 1A/1B against the OLD set.
/// `Ok(None)` when its attestations cannot be fetched yet.
async fn prove(
    driver: &mut LiveProverDriver,
    rotation: PreparedRotation,
) -> DriverResult<Option<BkUpdateProofArtifacts>> {
    let upd_seqno = rotation.seq_no;
    let cur_pubkeys = &rotation.old_pubkeys;

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
    // the single-PK memory envelope. Pick the Fiat–Shamir flavour from the
    // driver config so a Poseidon-configured driver emits aggregator-ready
    // bytes here — same one-flavour-per-poll discipline as the bundle path.
    // The attestation `lastSeen` instance is the driver's layer cursor
    // at prove time, capped below the rotation ([`rotation_last_seen`]).
    // The caller forwards that word as `attestationLastSeen`;
    // `applyBkSetUpdate` does not substitute `storedLastSeenBlockSeqNo`.
    let last_seen_for_upd =
        rotation_last_seen(driver.state().stored_last_seen_block_seq_no, upd_seqno) as u32;
    let transcript = driver.cfg().transcript;
    let t_upd_proof = Instant::now();
    let (fin_type, upd_proof) = match &upd_evidence {
        AttestationEvidence::Primary(att) => {
            info!(
                "bk-update {}: PRIMARY path → Circuit 1a (transcript={:?})",
                upd_seqno, transcript,
            );
            driver
                .key_manager_mut()
                .load_primary_pk()
                .with_context(|| format!("bk-update {}: load_primary_pk", upd_seqno))
                .map_err(|e| DriverError::proof_gen(upd_seqno, e))?;
            let res = prover::generate_primary_proof_with_transcript(
                driver.key_manager_mut(),
                &att.raw_bytes,
                cur_pubkeys,
                last_seen_for_upd,
                transcript,
            );
            driver.key_manager_mut().unload_primary_pk();
            match res {
                Ok(o) => (BundleFinalizationType::Primary, o),
                Err(e) => {
                    error!("bk-update {}: Circuit 1a proof failed: {}", upd_seqno, e);
                    return Err(DriverError::proof_gen(upd_seqno, e));
                }
            }
        }
        AttestationEvidence::Fallback { primary, fallback } => {
            info!(
                "bk-update {}: FALLBACK path → Circuit 1b (transcript={:?})",
                upd_seqno, transcript,
            );
            driver
                .key_manager_mut()
                .load_fallback_pk()
                .with_context(|| format!("bk-update {}: load_fallback_pk", upd_seqno))
                .map_err(|e| DriverError::proof_gen(upd_seqno, e))?;
            let res = prover::generate_fallback_proof_with_transcript(
                driver.key_manager_mut(),
                &primary.raw_bytes,
                &fallback.raw_bytes,
                cur_pubkeys,
                last_seen_for_upd,
                transcript,
            );
            driver.key_manager_mut().unload_fallback_pk();
            match res {
                Ok(o) => (BundleFinalizationType::Fallback, o),
                Err(e) => {
                    error!("bk-update {}: Circuit 1b proof failed: {}", upd_seqno, e);
                    return Err(DriverError::proof_gen(upd_seqno, e));
                }
            }
        }
    };
    let attestation_proof_gen_ms = t_upd_proof.elapsed().as_millis() as u64;
    info!(
        "bk-update {}: {} proof generated in {} ms",
        upd_seqno,
        fin_type.as_str(),
        attestation_proof_gen_ms,
    );
    // The proof commits to the height it read from the attestation bytes; the
    // calldata carries `upd_seqno`. A mismatch is a bug on this side, and the
    // chain would only say `AttestationProofRejected()`.
    if u64::from(upd_proof.block_seq_no) != upd_seqno {
        return Err(DriverError::proof_gen(
            upd_seqno,
            anyhow::anyhow!(
                "bk-update {upd_seqno}: the proof carries block_seq_no {}, not the event's",
                upd_proof.block_seq_no
            ),
        ));
    }

    // The pre-refactor daemon assembled an `ipc::BkUpdateRequest` here; we
    // return the same fields as a transport-agnostic payload. The caller
    // maps this into the transport of its choice. Since schema v6 there is a
    // single `block_id_be` (raw 32-byte SHA-256 root); the Fr form is
    // derived on demand by the verifier via `ipc::hash_hex_to_fr`, and by the
    // relayer's `block_id_to_field` (`% BN254_R`) before submission — the R15
    // SHPLONK adapter on-chain does NOT auto-reduce, it byte-compares against
    // a canonical `Fr` instance. Debug-assert that the circuit's committed Fr
    // agrees with the fold of the raw hash so a byte-order regression pages
    // loudly at build time.
    debug_assert_eq!(
        crate::ipc::fold_hash_be_to_fr(&rotation.tree.root),
        upd_proof.block_id_fr,
        "bk-update: fold(reverse(tree.root)) must equal Circuit 1's committed \
         block_id_fr; a mismatch means the wire hash and the proof disagree",
    );
    let l2_l3_siblings = rotation.tree.siblings_for_l2_l3();
    Ok(Some(BkUpdateProofArtifacts {
        block_seq_no: upd_seqno,
        block_height: rotation.block_height,
        last_seen_bk_update_seq_no: last_seen_for_upd as u64,
        block_id_be: rotation.tree.root,
        fin_type,
        old_bk_set_commitment_be: rotation.l2(),
        new_bk_set_commitment_be: rotation.l3(),
        merkle_sibling_h01_be: l2_l3_siblings[0],
        merkle_sibling_h4_7_be: l2_l3_siblings[1],
        merkle_sibling_h8_15_be: l2_l3_siblings[2],
        transcript_kind: transcript,
        attestation_proof: upd_proof.proof_bytes,
        new_pubkeys: rotation.new_pubkeys,
        attestation_proof_gen_ms,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use halo2_base::halo2_proofs::halo2curves::{
        bls12_381::{Fr as BlsScalar, G1Affine, G1},
        group::Curve,
    };

    use super::*;
    use crate::bk_set_replay::commitment;

    const ADD: u32 = 0;
    const REMOVE: u32 = 1;

    fn point(seed: u64) -> G1Affine {
        (G1::generator() * BlsScalar::from(seed)).to_affine()
    }

    /// Compressed key, as the sets hold them.
    fn key(seed: u64) -> Vec<u8> {
        point(seed).to_compressed_be().to_vec()
    }

    fn set(entries: &[(u16, u64)]) -> HashMap<u16, Vec<u8>> {
        entries.iter().map(|(i, s)| (*i, key(*s))).collect()
    }

    /// A blob in the node's wire format with 96-byte uncompressed keys.
    fn blob(changes: &[(u32, u16, u64)]) -> String {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(changes.len() as u64).to_le_bytes());
        for (variant, index, seed) in changes {
            buf.extend_from_slice(&variant.to_le_bytes());
            buf.extend_from_slice(&index.to_le_bytes());
            buf.extend_from_slice(&96u64.to_le_bytes());
            buf.extend_from_slice(&point(*seed).to_uncompressed_be());
        }
        hex::encode(buf)
    }

    #[test]
    fn rotation_last_seen_never_reaches_the_rotation() {
        // The bundle at the boundary landed before the rotation at the same height.
        assert_eq!(rotation_last_seen(32768, 32768), 32767);
        assert_eq!(rotation_last_seen(16384, 20000), 16384);
    }

    #[test]
    fn next_set_for_row_uses_node_order() {
        // In blob order the addition would be undone by the removal that follows it.
        let current = set(&[(0, 5), (1, 1)]);
        let expected = set(&[(0, 5), (1, 9)]);
        let next = next_set_for_row(
            &current,
            &blob(&[(ADD, 1, 9), (REMOVE, 1, 1)]),
            commitment(&current),
            commitment(&expected),
            100,
        )
        .unwrap();
        assert_eq!(next[&1], key(9));
        assert_eq!(next, expected);
    }

    #[test]
    fn next_set_for_row_rejects_wrong_l2() {
        let current = set(&[(0, 5), (1, 1)]);
        let after = set(&[(0, 5), (1, 1), (2, 3)]);
        let err = next_set_for_row(
            &current,
            &blob(&[(ADD, 2, 3)]),
            [9; 32],
            commitment(&after),
            100,
        )
        .unwrap_err();
        assert!(matches!(err, ReplayError::Mismatch {
            height: 100,
            step: "l2-before-apply",
            ..
        }));
    }

    #[test]
    fn next_set_for_row_rejects_wrong_l3() {
        let current = set(&[(0, 5), (1, 1)]);
        let err = next_set_for_row(
            &current,
            &blob(&[(ADD, 2, 3)]),
            commitment(&current),
            [9; 32],
            100,
        )
        .unwrap_err();
        assert!(matches!(err, ReplayError::Mismatch {
            height: 100,
            step: "l3-after-apply",
            ..
        }));
    }

    #[test]
    fn a_wrong_l2_is_a_state_mismatch_and_other_row_errors_are_schema_errors() {
        let l2 = ReplayError::Mismatch {
            height: 100,
            step: "l2-before-apply",
            expected: [1; 32],
            actual: [2; 32],
        };
        assert!(matches!(
            row_error(100, l2),
            DriverError::StateInconsistent(_)
        ));
        let l3 = ReplayError::Mismatch {
            height: 100,
            step: "l3-after-apply",
            expected: [1; 32],
            actual: [2; 32],
        };
        assert!(matches!(row_error(100, l3), DriverError::GqlSchema(_)));
        let bad = ReplayError::BadRow {
            reason: "blob holds no changes".into(),
        };
        let err = row_error(100, bad);
        assert!(matches!(err, DriverError::GqlSchema(_)));
        assert!(err.to_string().contains("bk-update 100"), "{err}");
    }
}
