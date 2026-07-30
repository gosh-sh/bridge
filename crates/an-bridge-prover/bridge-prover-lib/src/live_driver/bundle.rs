//! Single-iteration Circuit 1A/1B + Circuit 2 bundle producer.
//!
//! Ported from `bridge-prover-daemon/src/main.rs:712-855` +
//! `main.rs:1093-1195` (the `generate_layer_proof_for_key_block` helper).
//! Produces one [`BundleProofArtifacts`] for the given target seqno; the
//! caller advances the driver's cursor via
//! [`super::LiveProverDriver::ack_bundle`] after successful downstream
//! verification.
//!
//! Behavioural differences from the pre-refactor main.rs:
//!
//! * On "attestation evidence not ready yet" — returns `Ok(None)` so the
//!   caller retries on the next poll. The pre-refactor daemon slept +
//!   `continue`d; here that's the caller's job.
//! * On "signers not in current BK-set" — returns `Ok(None)`. The
//!   pre-refactor daemon silently advanced the cursor and continued; that
//!   was arguably wrong because a stale set can be repaired by draining a
//!   pending bk-update, so returning `None` (leaving the cursor alone) is
//!   the safer default. If the mismatch is genuine the caller can add a
//!   policy layer on top.
//! * On proof-generation failure — propagates the error. The pre-refactor
//!   daemon silently advanced past failed proofs; that hid real bugs. The
//!   caller decides restart / retry policy.

use anyhow::Context;
use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;
use tracing::{info, warn};
use std::time::Instant;

use bridge_gql_fetcher::attestation_fetcher::{self, AttestationEvidence};
use crate::block_id_tree;
use crate::bridge_state::MAX_LAYERS;
use crate::layer_prover;
use crate::prover;
use crate::real_chain_builder;

use super::{BundleFinalizationType, BundleProofArtifacts, DriverError, DriverResult, LiveProverDriver};

/// Drive one Circuit 1A/1B + Circuit 2 bundle. Returns `Some(artifacts)`
/// on success, `None` on transient conditions the caller should retry
/// (attestation not ready, signer mismatch). Fatal errors are classified
/// into [`DriverError`] variants at each site so consumers can
/// pattern-match on failure kind (proof gen vs GQL schema vs state
/// inconsistency) instead of inspecting messages.
pub(super) async fn drive_next_bundle(
    driver: &mut LiveProverDriver,
    target_seqno: u64,
) -> DriverResult<Option<BundleProofArtifacts>> {
    info!("=== Processing key block at seq_no {} ===", target_seqno);

    // Fetch and classify attestation evidence.
    let evidence = match attestation_fetcher::fetch_attestation_evidence(
        driver.gql(),
        target_seqno as u32,
    )
    .await
    {
        Ok(ev) => ev,
        Err(e) => {
            warn!(
                "key block {}: attestation evidence not ready ({}); returning None so caller retries",
                target_seqno, e,
            );
            return Ok(None);
        }
    };

    // Decode the prover's BK pubkey table once from `prover_bk_set`
    // (the sole in-driver source of truth). `pubkeys()` re-hexes
    // ~N × 48 bytes — negligible next to ~75-second proof generation
    // that follows. Owned local so it coexists with `key_manager_mut()`
    // borrows below without an extra clone.
    let bk_set = driver
        .prover_bk_set()
        .pubkeys()
        .context("prover_bk_set.pubkeys() decode failed")
        .map_err(DriverError::state_inconsistent)?;

    // Defensive BK-set-membership warning. In-circuit checks are
    // authoritative; this surfaces obviously-stale sets earlier.
    let signer_indices = evidence.signer_indices();
    let missing: Vec<u16> = signer_indices
        .iter()
        .filter(|idx| !bk_set.contains_key(idx))
        .copied()
        .collect();
    if !missing.is_empty() {
        warn!(
            "key block {}: signers {:?} not in BK set — returning None so caller can drain \
             pending bk-update or refresh the set",
            target_seqno, missing,
        );
        return Ok(None);
    }

    let last_seen_seqno_u32 = driver.state().stored_last_seen_block_seq_no as u32;

    // ---- Circuit 1a / 1b: attestation proof ----
    // Load PK on demand, unload immediately after to stay inside the
    // single-PK memory envelope before we load the layer PK.
    let t_primary = Instant::now();
    let (fin_type, primary_proof) = match &evidence {
        AttestationEvidence::Primary(att) => {
            info!("key block {}: PRIMARY path → Circuit 1a", target_seqno);
            driver
                .key_manager_mut()
                .load_primary_pk()
                .with_context(|| format!("key block {}: load_primary_pk", target_seqno))
                .map_err(|e| DriverError::proof_gen(target_seqno, e))?;
            let res = prover::generate_primary_proof(
                driver.key_manager_mut(),
                &att.raw_bytes,
                &bk_set,
                last_seen_seqno_u32,
            );
            driver.key_manager_mut().unload_primary_pk();
            (
                BundleFinalizationType::Primary,
                res.map_err(|e| DriverError::proof_gen(target_seqno, e))?,
            )
        }
        AttestationEvidence::Fallback { primary, fallback } => {
            info!("key block {}: FALLBACK path → Circuit 1b", target_seqno);
            driver
                .key_manager_mut()
                .load_fallback_pk()
                .with_context(|| format!("key block {}: load_fallback_pk", target_seqno))
                .map_err(|e| DriverError::proof_gen(target_seqno, e))?;
            let res = prover::generate_fallback_proof(
                driver.key_manager_mut(),
                &primary.raw_bytes,
                &fallback.raw_bytes,
                &bk_set,
                last_seen_seqno_u32,
            );
            driver.key_manager_mut().unload_fallback_pk();
            (
                BundleFinalizationType::Fallback,
                res.map_err(|e| DriverError::proof_gen(target_seqno, e))?,
            )
        }
    };
    let primary_proof_gen_ms = t_primary.elapsed().as_millis() as u64;
    info!(
        "key block {}: {} proof generated in {} ms",
        target_seqno,
        fin_type.as_str(),
        primary_proof_gen_ms,
    );

    // ---- Circuit 2: layer hashes movement proof ----
    driver
        .key_manager_mut()
        .load_layer_pk()
        .with_context(|| format!("key block {}: load_layer_pk", target_seqno))
        .map_err(|e| DriverError::proof_gen(target_seqno, e))?;
    let t_layer = Instant::now();
    let layer_result = generate_layer_proof_for_key_block(driver, target_seqno).await;
    driver.key_manager_mut().unload_layer_pk();
    let (layer_proof, state_layer_hashes, observed_height, block_id_be) = layer_result?;
    let layer_proof_gen_ms = t_layer.elapsed().as_millis() as u64;
    info!(
        "key block {}: Circuit 2 proof generated in {} ms",
        target_seqno, layer_proof_gen_ms,
    );

    // Post-2026-07-22 both circuits emit `block_id_fr = uint256(bytes32(root))`.
    // Debug-assert three-way agreement between:
    //   * Circuit 1 witness   (`primary_proof.block_id_fr`)
    //   * Circuit 2 witness   (`layer_proof.block_id_fr`)
    //   * raw hash reduction  (`ipc::fold_hash_be_to_fr(block_id_be)`)
    // so a byte-order regression on either circuit — or a divergence between
    // the wire hash and either circuit's committed Fr — pages loudly at
    // proof-build time instead of silently mis-mirroring state on-chain.
    debug_assert_eq!(
        primary_proof.block_id_fr, layer_proof.block_id_fr,
        "Circuit 1 and Circuit 2 must agree on block_id_fr; a mismatch means \
         one of the circuits regressed to the pre-fix byte-order convention",
    );
    debug_assert_eq!(
        crate::ipc::fold_hash_be_to_fr(&block_id_be),
        primary_proof.block_id_fr,
        "fold(reverse(raw_hash)) must equal Circuit 1's committed block_id_fr; \
         a mismatch means bundle.block_id_be is not the raw chain hash BE",
    );
    let bk_set_commitment_be: [u8; 32] = driver.bk_set_commitment_fr().to_repr();
    let mut layer_hashes_be: [[u8; 32]; MAX_LAYERS] = [[0u8; 32]; MAX_LAYERS];
    for (i, fr) in layer_proof.layer_hash_frs.iter().enumerate() {
        layer_hashes_be[i] = fr.to_repr();
    }
    let prev_max_level_layer_hash_be: [u8; 32] = layer_proof
        .prev_max_level_layer_hash_fr
        .to_repr();

    Ok(Some(BundleProofArtifacts {
        block_seq_no: target_seqno,
        block_height: observed_height,
        last_seen_block_seq_no: driver.state().stored_last_seen_block_seq_no,
        block_id_be,
        fin_type,
        bk_set_commitment_be,
        num_layers: layer_proof.num_layers,
        layer_hashes_be,
        prev_max_level_layer_hash_be,
        attestation_proof: primary_proof.proof_bytes,
        layer_hashes_proof: layer_proof.proof_bytes,
        primary_proof_gen_ms,
        layer_proof_gen_ms,
        state_layer_hashes,
    }))
}

/// Port of `generate_layer_proof_for_key_block` from the pre-refactor
/// `main.rs:1093-1195`. Additionally returns the per-layer bundle
/// (`state_layer_hashes`), the authoritative block height, and the raw
/// 32-byte BE chain block hash (SHA-256 root of the 16-leaf depth-4 tree), all
/// needed by the bundle assembler and by
/// [`super::LiveProverDriver::ack_bundle`] to advance the in-memory
/// [`crate::bridge_state::BridgeState`].
async fn generate_layer_proof_for_key_block(
    driver: &LiveProverDriver,
    target_seqno: u64,
) -> DriverResult<(
    layer_prover::LayerProofOutput,
    Vec<([u8; 32], u8)>,
    u64,
    [u8; 32],
)> {
    info!("fetching block proof data for seq={}...", target_seqno);
    let block = driver
        .gql()
        .query_proof_block_by_seqno(target_seqno)
        .await
        .context("failed to fetch block proof data")
        .map_err(DriverError::gql_transient)?;

    let leaves = block.block_merkle_tree_leaves.ok_or_else(|| {
        DriverError::gql_schema(anyhow::anyhow!(
            "block {} has no block_merkle_tree_leaves in GQL — node must expose them",
            target_seqno,
        ))
    })?;

    info!(
        "parsed: history_proofs={} layers",
        block.history_proofs.len(),
    );
    if block.history_proofs.is_empty() {
        return Err(DriverError::gql_schema(anyhow::anyhow!(
            "block {} has no history_proofs",
            target_seqno,
        )));
    }

    // 1. Build layer_hashes_preimage from history_proofs.
    let num_layers = block.history_proofs.len() as u8;
    let mut root_hashes: Vec<[u8; 32]> = Vec::with_capacity(MAX_LAYERS);
    for i in 1..=MAX_LAYERS as u8 {
        if let Some(root) = block.history_proofs.get(&i) {
            root_hashes.push(*root);
        } else {
            root_hashes.push([0u8; 32]);
        }
    }
    let preimage = block_id_tree::build_layer_hashes_preimage(num_layers as usize, &root_hashes);

    // 2. Build the 16-leaf depth-4 SHA-256 Merkle tree from the GQL leaves
    //    and pull the four siblings that open L0 up to `block_id`.
    let tree = block_id_tree::BlockIdMerkleTree::from_leaves(leaves);

    // Structural sanity: the tree we just folded must agree with the
    // block_id the node reports in the same GQL response. The on-chain
    // verifier already rejects mismatched openings, so this is not a
    // correctness fix — it's fail-fast (skip Circuit 2 witness build +
    // proof gen + IPC when leaves are broken) and release-build parity.
    // Mirror of the same check on the bk-update path in bk_update.rs.
    if tree.root != block.block_id {
        return Err(DriverError::gql_schema(anyhow::anyhow!(
            "layer {}: reconstructed tree.root {} != block.block_id {} — \
             GQL leaves inconsistent with block header",
            target_seqno,
            hex::encode(tree.root),
            hex::encode(block.block_id),
        )));
    }

    let siblings = tree.siblings_for_l0();
    info!(
        "block_id from GQL leaves merkle root: {}",
        hex::encode(tree.block_id()),
    );

    // 3. BK-set Poseidon hash. Cross-check that `leaves[2]` matches our
    // currently-applied commitment — same fail-fast that the pre-refactor
    // `generate_layer_proof_for_key_block` did.
    let bk_hash_bytes: [u8; 32] = driver.bk_set_commitment_fr().to_repr();
    if bk_hash_bytes != leaves[2] {
        return Err(DriverError::state_inconsistent(anyhow::anyhow!(
            "loaded BK set Poseidon commitment ({}) does not match block.leaves[2] ({}) — \
             stale BK set or the chain rotated keys",
            hex::encode(bk_hash_bytes),
            hex::encode(leaves[2]),
        )));
    }
    let bk_set_hash_fr = driver.bk_set_commitment_fr();

    // 4. Build REAL chain proofs from intermediate block data.
    let chain_result = real_chain_builder::build_real_chain(
        driver.gql(),
        driver.state(),
        &block.history_proofs,
        target_seqno,
        driver.cfg().history_window_size,
    )
    .await
    .context("failed to build real chain proofs")
    .map_err(DriverError::gql_transient)?;
    info!("using REAL chain proofs ({} steps)", chain_result.num_steps);

    let prev_hash_fr = gosh_dense_balanced_tree::bytes_to_fr(&chain_result.prev_hash);

    // 5. Generate Circuit 2 proof.
    let layer_proof = layer_prover::generate_layer_proof(
        driver.key_manager(),
        &preimage,
        &siblings,
        prev_hash_fr,
        chain_result.num_steps,
        &chain_result.chain_links,
        bk_set_hash_fr,
    )
    .map_err(|e| DriverError::proof_gen(target_seqno, e))?;

    // 6. Extract the per-layer bundle + authoritative block height for
    //    ack_bundle to feed BridgeState::append_bundle. Also surface the raw
    //    SHA-256 root (= chain `Block.id`) so the caller can populate
    //    `BundleProofArtifacts.block_id_be` from the ground-truth hash, not
    //    from any circuit's `Fr::to_repr()` (which would lose the top 2 bits
    //    when the hash `>= p`).
    let state_layer_hashes: Vec<([u8; 32], u8)> = block
        .history_proofs
        .iter()
        .map(|(&layer, root)| (*root, layer))
        .collect();

    Ok((layer_proof, state_layer_hashes, block.height, tree.block_id()))
}
