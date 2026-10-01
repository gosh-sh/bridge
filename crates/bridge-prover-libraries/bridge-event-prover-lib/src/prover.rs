//! Circuit 4 (Event Prove — `WithdrawalInitiated`) proof generation.
//!
//! Bridges the JSON schema produced by `bridge-event-private-witness-export`
//! to the multi-thread `bridge-event-prove-circuit::bridge_event_final_proof::
//! BridgeEventFinalProof` halo2 circuit.
//!
//! Two responsibilities:
//!   1. **Input conversion** — deserialize the witness JSON, hex-decode cell
//!      records, and assemble a `BridgeEventFinalProof` instance.
//!   2. **Public instance derivation** — build the 13-slot vector `[token_id,
//!      amount, recipient_hi, recipient_lo, dst_chain_id, sender_acc_fr,
//!      dapp_fr, acc_fr, nullifier, final_root, anchor_layer, x_block_id,
//!      y_block_id]` that the verifier checks.
//!
//! ### Anchor binding contract
//!
//! The witness's `anchor.layer_hash_hex` becomes the proof's
//! `PUB_FINAL_ROOT` instance slot, and `anchor.layer_idx + 1` (1-indexed)
//! becomes `PUB_ANCHOR_LAYER`. The circuit computes `final_root` by
//! climbing the supplied dense chain and range-checks the layer. The
//! on-chain adapter checks `final_root` against that layer's window.
//!
//! ### Cross-thread vs same-thread
//!
//! `y_block_id` is derived from the accompanying [`MultiHopBundleWitnessJson`]:
//! same-thread callers pass `&MultiHopBundleWitnessJson::default()` (an empty
//! `snarks: []`) and get `y_block_id = x_block_id = block_id_hex` with
//! `is_same_thread = true`; cross-thread callers pass a `Y → … → X` bundle.
//! Its first hop supplies `y_block_id`; its last hop must end at the exact
//! event `block_id_hex` (`x_block_id`).

use std::convert::TryInto;

use anyhow::{bail, Context, Result};
use bridge_event_prove_circuit::{
    boc_helper::BocFlattenData,
    bridge_event_final_proof::{BridgeEventFinalProof, TOTAL_PUBLIC_INPUTS},
    event_primitives::{
        be_bytes_to_fr, EVENT_AMOUNT_END, EVENT_AMOUNT_START, EVENT_DST_CHAIN_ID_END,
        EVENT_DST_CHAIN_ID_START, MAX_ANCHOR_LAYER, MAX_EVENTS_TREE_DEPTH, RECIPIENT_HI_END,
        RECIPIENT_HI_START, RECIPIENT_LO_END, RECIPIENT_LO_START,
    },
    multi_hop_proof::{BridgeMultiHopProof, MULTI_HOP_PUBLIC_LEN},
    multi_hop_witness::{
        BlockWitness as CircuitBlockWitness, HopWitness as CircuitHopWitness,
        MultiHopProofWitness as CircuitMultiHopProofWitness, BLOCK_MERKLE_DEPTH,
        BLOCK_MERKLE_LEAF_COUNT, H_HOPS_PER_PROOF, MAX_PROOF_BLOCK_REFS_DEPTH,
    },
    test_helpers::{
        decode_sender_account_id_from_cell, multi_hop_base_circuit_params, nullifier_native,
    },
};
// Re-export the witness JSON types so the daemon doesn't need to pull
// `bridge-event-witness` directly.
pub use bridge_event_witness::schema::{
    AnchorRef, BlockContext, CellRecord, DenseChainLinkSer, HopWitnessJson, MerkleProofData,
    MultiHopBundleWitnessJson, MultiHopProofWitnessJson, PrivateWitness, WithdrawalInitiated,
    SCHEMA_VERSION,
};
use bridge_prover_lib::{
    keys::{EventKeyManager, MultiHopKeyManager},
    transcript::{PoseidonWrite, TranscriptKind},
};
use gosh_dense_balanced_tree::{bytes_to_fr, DenseChainLink, MAX_CHAIN_LEN};
// Re-export so consumers can construct circuit params without an extra dep.
pub use halo2_base::gates::circuit::BaseCircuitParams;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine},
    plonk::create_proof,
    poly::kzg::{commitment::KZGCommitmentScheme, multiopen::ProverSHPLONK},
    transcript::{Blake2bWrite, Challenge255, TranscriptWriterBuffer},
};
use rand::rngs::OsRng;
use tracing::info;

use crate::bundle::EventBundle;

/// Conservative base-circuit params for first-cut Circuit 4 work. Mirrors
/// `bridge-event-prove-circuit::test_helpers::base_circuit_params`. Future
/// work: tighten as dark-dex did (K=14, ~110 advice columns).
pub fn default_event_circuit_params() -> BaseCircuitParams {
    BaseCircuitParams {
        k: 19,
        num_advice_per_phase: vec![16],
        num_fixed: 1,
        num_lookup_advice_per_phase: vec![2],
        lookup_bits: Some(18),
        num_instance_columns: 1,
    }
}

/// Bundle of everything the verifier needs once a Circuit 4 proof is generated.
pub struct EventProofInputs {
    pub circuit: BridgeEventFinalProof,
    pub public_instances: Vec<Fr>,
}

/// Decode a hex byte string into a fixed-size byte array. The leading "0x"
/// prefix is tolerated.
fn parse_hex_array<const N: usize>(label: &str, s: &str) -> Result<[u8; N]> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).with_context(|| format!("{label}: invalid hex"))?;
    if bytes.len() != N {
        bail!("{label}: expected {N} bytes, got {}", bytes.len());
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn cell_record_to_flat(rec: &CellRecord, label: &str) -> Result<BocFlattenData> {
    let repr_hash = parse_hex_array::<32>(&format!("{label}.repr_hash_hex"), &rec.repr_hash_hex)?;
    let cell_repr_data = hex::decode(&rec.cell_repr_data_hex)
        .with_context(|| format!("{label}.cell_repr_data_hex: invalid hex"))?;
    Ok(BocFlattenData {
        repr_hash,
        refs_count: rec.refs_count,
        childs_repr_hashes_offset: rec.childs_repr_hashes_offset.clone(),
        cell_repr_data,
    })
}

fn merkle_proof_to_native(proof: &MerkleProofData, label: &str) -> Result<(Vec<[u8; 32]>, usize)> {
    let mut siblings = Vec::with_capacity(proof.siblings_hex.len());
    for (i, s) in proof.siblings_hex.iter().enumerate() {
        siblings.push(parse_hex_array::<32>(
            &format!("{label}.siblings_hex[{i}]"),
            s,
        )?);
    }
    Ok((siblings, proof.position as usize))
}

fn dense_chain_to_native(links: &[DenseChainLinkSer]) -> Result<Vec<DenseChainLink>> {
    if links.len() != MAX_CHAIN_LEN {
        bail!(
            "anchor.dense_chain length {} != MAX_CHAIN_LEN ({MAX_CHAIN_LEN})",
            links.len(),
        );
    }
    let mut out = Vec::with_capacity(MAX_CHAIN_LEN);
    for (i, link) in links.iter().enumerate() {
        let leaf_native =
            parse_hex_array::<32>(&format!("dense_chain[{i}].leaf_hex"), &link.leaf_hex)?;
        let mut siblings = Vec::with_capacity(link.siblings_hex.len());
        for (j, s) in link.siblings_hex.iter().enumerate() {
            siblings.push(parse_hex_array::<32>(
                &format!("dense_chain[{i}].siblings_hex[{j}]"),
                s,
            )?);
        }
        out.push(DenseChainLink {
            active: link.active,
            siblings,
            position: link.position as usize,
            leaf_native,
        });
    }
    Ok(out)
}

/// Build a `BridgeEventFinalProof` + public-instance vector from a fully
/// populated [`PrivateWitness`].
///
/// "Fully populated" means `events_tree_proof`, `block_tree_proof`, and
/// `anchor` are all `Some(_)` — the per-tx exporter leaves them `None` and
/// the daemon fills them in from verifier state.
///
/// `hop_bundle` supplies the cross-thread route in `Y -> ... -> X` order.
/// Same-thread callers pass an empty bundle and get `y_block_id = x_block_id`.
pub fn build_proof_inputs(
    witness: &PrivateWitness,
    hop_bundle: &MultiHopBundleWitnessJson,
    base_circuit_params: BaseCircuitParams,
) -> Result<EventProofInputs> {
    if witness.schema_version != SCHEMA_VERSION {
        bail!(
            "private witness schema_version={} but event_prover expects {SCHEMA_VERSION}",
            witness.schema_version,
        );
    }

    let entries_ref = &witness.entries;
    let entries: [BocFlattenData; 4] = [
        cell_record_to_flat(&entries_ref[0], "entries[0]")?,
        cell_record_to_flat(&entries_ref[1], "entries[1]")?,
        cell_record_to_flat(&entries_ref[2], "entries[2]")?,
        cell_record_to_flat(&entries_ref[3], "entries[3]")?,
    ];

    let events_tree = witness
        .events_tree_proof
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("events_tree_proof missing — daemon-side step not run"))?;
    let block_tree = witness
        .block_tree_proof
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("block_tree_proof missing — daemon-side step not run"))?;
    let anchor = witness
        .anchor
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("anchor missing — daemon-side step not run"))?;

    let (events_siblings, events_pos) = merkle_proof_to_native(events_tree, "events_tree_proof")?;
    if events_siblings.len() > MAX_EVENTS_TREE_DEPTH {
        bail!(
            "events_tree_proof depth {} exceeds MAX_EVENTS_TREE_DEPTH ({MAX_EVENTS_TREE_DEPTH})",
            events_siblings.len(),
        );
    }
    // Mirror `BridgeEventFinalProof::assert_invariants`'s events_pos range
    // check here so an out-of-range witness surfaces as a decoded error with
    // context rather than a panic. `assert_invariants` fires from
    // `BridgeEventFinalProof::new` further down in this same function
    // (`build_proof_inputs`), so without this mirror an out-of-range witness
    // would panic on the caller's thread, not on a worker.
    //
    // Shift is safe: we already bounded `events_siblings.len()` by
    // `MAX_EVENTS_TREE_DEPTH` above, which fits comfortably inside `usize::BITS`.
    let events_pos_max = 1usize << events_siblings.len();
    if events_pos >= events_pos_max {
        bail!(
            "events_tree_proof position {events_pos} out of range for depth {} \
             (max={events_pos_max})",
            events_siblings.len(),
        );
    }

    let (block_siblings, block_pos) = merkle_proof_to_native(block_tree, "block_tree_proof")?;
    // Same range mirror for the block-tree position; the circuit constructor
    // does not currently panic on this axis but a future refactor could add
    // an equivalent bounds check, and reporting here keeps both axes
    // symmetric under a decoded error path.
    //
    // Unlike `events_siblings.len()` above there is no explicit `MAX_*_DEPTH`
    // cap on `block_siblings.len()` in the circuit, so a pathological witness
    // could otherwise trip Rust's shift-by-≥-`usize::BITS` handling: a debug
    // build panics with `attempt to shift left with overflow`; a release
    // build silently masks the shift amount modulo `usize::BITS`. Take
    // `siblings.len() == 64` on a 64-bit target — the mask reduces the
    // shift to `64 % 64 = 0`, so `1usize << 64` evaluates to `1` and
    // `block_pos_max` becomes `1`. The subsequent `block_pos >= block_pos_max`
    // check then over-rejects (positions in `1..2^32` — the actual range a
    // witness can encode, since `MerkleProofData::position` is `u32` — are
    // turned away instead of accepted for a 64-deep tree) but still lets
    // `block_pos == 0` through — a pathological deep-tree witness with a
    // zero position would silently pass this check on release. `checked_shl`
    // fails closed at that boundary in every profile and stops the
    // zero-position case too.
    let block_pos_max = 1usize
        .checked_shl(block_siblings.len() as u32)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "block_tree_proof depth {} is not representable as a usize width shift",
                block_siblings.len(),
            )
        })?;
    if block_pos >= block_pos_max {
        bail!(
            "block_tree_proof position {block_pos} out of range for depth {} (max={block_pos_max})",
            block_siblings.len(),
        );
    }

    let account_dapp_id = parse_hex_array::<32>(
        "block_context.account_dapp_id_hex",
        &witness.block_context.account_dapp_id_hex,
    )?;
    let account_id = parse_hex_array::<32>(
        "block_context.account_id_hex",
        &witness.block_context.account_id_hex,
    )?;
    let envelope_hash = parse_hex_array::<32>(
        "block_context.envelope_hash_hex",
        &witness.block_context.envelope_hash_hex,
    )?;
    let block_id = parse_hex_array::<32>("block_id_hex", &witness.block_id_hex)?;

    let dense_chain = dense_chain_to_native(&anchor.dense_chain)?;
    let num_active_chain_steps = anchor.num_active_chain_steps as usize;
    if num_active_chain_steps > MAX_CHAIN_LEN {
        bail!(
            "anchor.num_active_chain_steps={num_active_chain_steps} exceeds MAX_CHAIN_LEN \
             ({MAX_CHAIN_LEN})",
        );
    }

    let final_root_bytes = parse_hex_array::<32>("anchor.layer_hash_hex", &anchor.layer_hash_hex)?;
    let final_root_fr = bytes_to_fr(&final_root_bytes);

    let anchor_layer = anchor
        .layer_idx
        .checked_add(1)
        .filter(|&l| l <= u32::from(MAX_ANCHOR_LAYER))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "anchor.layer_idx={} is not a 0-indexed layer in 0..={MAX_ANCHOR_LAYER}-1",
                anchor.layer_idx
            )
        })?;
    let anchor_layer_u8 = u8::try_from(anchor_layer).expect("checked against MAX_ANCHOR_LAYER");
    let anchor_layer_fr = Fr::from(u64::from(anchor_layer));

    // 13-slot public-instance layout (see
    // `bridge-event-prove-circuit::bridge_event_final_proof` PUB_* constants):
    //   [0] token_id, [1] amount, [2] recipient_hi, [3] recipient_lo,
    //   [4] dst_chain_id, [5] sender_acc_fr, [6] dapp_fr, [7] acc_fr,
    //   [8] nullifier, [9] final_root, [10] anchor_layer,
    //   [11] x_block_id, [12] y_block_id.
    let body = &entries[1].cell_repr_data;
    let recipient_payload = &entries[2].cell_repr_data;
    let sender_payload = &entries[3].cell_repr_data;

    let token_id_fr = derive_token_id_fr(&entries[1])?;
    let amount_fr = be_bytes_to_fr(&body[EVENT_AMOUNT_START..EVENT_AMOUNT_END]);
    let dst_chain_id_fr = be_bytes_to_fr(&body[EVENT_DST_CHAIN_ID_START..EVENT_DST_CHAIN_ID_END]);
    let recipient_hi_fr = be_bytes_to_fr(&recipient_payload[RECIPIENT_HI_START..RECIPIENT_HI_END]);
    let recipient_lo_fr = be_bytes_to_fr(&recipient_payload[RECIPIENT_LO_START..RECIPIENT_LO_END]);
    let sender_account_id = decode_sender_account_id_from_cell(sender_payload);
    let sender_acc_fr = bytes_to_fr(&sender_account_id);
    let dapp_fr = bytes_to_fr(&account_dapp_id);
    let acc_fr = bytes_to_fr(&account_id);
    let block_id_fr = bytes_to_fr(&block_id);
    let nullifier_fr = nullifier_native(
        block_id_fr,
        token_id_fr,
        amount_fr,
        recipient_hi_fr,
        recipient_lo_fr,
        sender_acc_fr,
        Fr::from(events_pos as u64),
    );

    // Multi-thread witness fields. `x_block_id` is always the event block;
    // `y_block_id` is either the same (same-thread claim, `hop_bundle.snarks`
    // is empty) or the head of the hop chain. The chain tail must equal the
    // exact event block X. `h07_sibling` completes the L8
    // opening pair — the circuit reconstructs `x_block_id` from
    // `(ext_out_root, h07_sibling)` via a depth-4 SHA opening and
    // copy-constrains equality with the witness.
    let h07_sibling = parse_hex_array::<32>("h07_sibling_hex", &witness.h07_sibling_hex)?;
    let y_tracked_ext_out_messages_root = parse_hex_array::<32>(
        "y_tracked_ext_out_messages_root_hex",
        &witness.y_tracked_ext_out_messages_root_hex,
    )?;
    let x_block_id = block_id;
    let x_block_id_fr = block_id_fr;
    let (y_block_id, is_same_thread) = match hop_bundle.snarks.first() {
        None => (block_id, true),
        Some(first_snark) => {
            let first_hop = &first_snark.hops[0];
            let y = parse_hex_array::<32>(
                "hop_bundle.snarks.first.hops.first.hop_start_block_id_hex",
                &first_hop.hop_start_block_id_hex,
            )?;
            let mut previous_end: Option<[u8; 32]> = None;
            for (index, snark) in hop_bundle.snarks.iter().enumerate() {
                let hop = &snark.hops[0];
                if !hop.is_active {
                    bail!("hop_bundle.snarks[{index}] contains an inactive route hop");
                }
                let start = parse_hex_array::<32>(
                    &format!("hop_bundle.snarks[{index}].hops[0].hop_start_block_id_hex"),
                    &hop.hop_start_block_id_hex,
                )?;
                let end = parse_hex_array::<32>(
                    &format!("hop_bundle.snarks[{index}].hops[0].hop_end_block_id_hex"),
                    &hop.hop_end_block_id_hex,
                )?;
                if let Some(previous) = previous_end {
                    if previous != start {
                        bail!("hop bundle is discontinuous before snark {index}");
                    }
                }
                previous_end = Some(end);
            }
            if previous_end != Some(block_id) {
                bail!("hop bundle tail does not equal PrivateWitness.block_id_hex");
            }
            (y, false)
        },
    };
    let y_block_id_fr = bytes_to_fr(&y_block_id);

    let mut public_instances = Vec::with_capacity(TOTAL_PUBLIC_INPUTS);
    public_instances.push(token_id_fr);
    public_instances.push(amount_fr);
    public_instances.push(recipient_hi_fr);
    public_instances.push(recipient_lo_fr);
    public_instances.push(dst_chain_id_fr);
    public_instances.push(sender_acc_fr);
    public_instances.push(dapp_fr);
    public_instances.push(acc_fr);
    public_instances.push(nullifier_fr);
    public_instances.push(final_root_fr);
    public_instances.push(anchor_layer_fr);
    public_instances.push(x_block_id_fr);
    public_instances.push(y_block_id_fr);

    let circuit = BridgeEventFinalProof::new(
        entries,
        events_siblings,
        events_pos,
        account_dapp_id,
        account_id,
        x_block_id,
        y_block_id,
        h07_sibling,
        is_same_thread,
        y_tracked_ext_out_messages_root,
        envelope_hash,
        block_siblings,
        block_pos,
        dense_chain,
        num_active_chain_steps,
        anchor_layer_u8,
        base_circuit_params,
    );

    Ok(EventProofInputs {
        circuit,
        public_instances,
    })
}

/// Token ID is `BE_pack(body[54..58))` — same derivation as
/// `bridge_event_prove_circuit::bridge_event_final_proof::extract_event_public_fields`,
/// but accepting the decoded `BocFlattenData` directly so the caller doesn't
/// need to construct the full `[BocFlattenData; 4]` array twice.
fn derive_token_id_fr(body: &BocFlattenData) -> Result<Fr> {
    const TOKEN_ID_START: usize = 54;
    const TOKEN_ID_END: usize = 58;
    if body.cell_repr_data.len() < TOKEN_ID_END {
        bail!(
            "body cell payload too short ({} bytes) to extract tokenId",
            body.cell_repr_data.len(),
        );
    }
    let slice = &body.cell_repr_data[TOKEN_ID_START..TOKEN_ID_END];
    // BE-pack 4 bytes into Fr.
    let arr: [u8; 4] = slice.try_into().expect("checked length above");
    Ok(Fr::from(u32::from_be_bytes(arr) as u64))
}

/// Output of a Circuit 4 proof generation pass.
///
/// `proof_bytes` is the SHPLONK/Blake2b-encoded proof; `public_instances`
/// is the 13-slot vector
/// `[token_id, amount, recipient_hi, recipient_lo, dst_chain_id,
/// sender_acc_fr, dapp_fr, acc_fr, nullifier, final_root, anchor_layer,
/// x_block_id, y_block_id]` — what the verifier (or the on-chain bridge)
/// checks against.
#[derive(Clone)]
pub struct EventProofOutput {
    pub proof_bytes: Vec<u8>,
    pub public_instances: Vec<Fr>,
}

/// Generate a Circuit 4 proof from a fully-populated [`PrivateWitness`].
///
/// `hop_bundle` supplies the `y_block_id` endpoint — pass
/// `&MultiHopBundleWitnessJson::default()` for same-thread claims.
///
/// Caller is responsible for ensuring the event PK is loaded into memory
/// before calling — i.e. `event_km.load_pk()?` first, then
/// `event_km.unload_pk()` after. The same on-demand pattern
/// `bridge-prover-daemon` uses for the primary and layer PKs.
pub fn generate_event_proof(
    event_km: &EventKeyManager,
    witness: &PrivateWitness,
    hop_bundle: &MultiHopBundleWitnessJson,
) -> Result<EventProofOutput> {
    generate_event_proof_with_transcript(event_km, witness, hop_bundle, TranscriptKind::Blake2b)
}

/// Generate a Circuit 4 proof from a fully-populated [`PrivateWitness`] with
/// the chosen Fiat–Shamir transcript. See
/// [`bridge_prover_lib::prover::generate_primary_proof_with_transcript`] for
/// transcript semantics — `Poseidon` here is what the R15 ETH-side
/// aggregator consumes.
pub fn generate_event_proof_with_transcript(
    event_km: &EventKeyManager,
    witness: &PrivateWitness,
    hop_bundle: &MultiHopBundleWitnessJson,
    transcript: TranscriptKind,
) -> Result<EventProofOutput> {
    let inputs = build_proof_inputs(witness, hop_bundle, event_km.config().clone())
        .context("build_proof_inputs failed (translating witness JSON → circuit)")?;
    let EventProofInputs {
        circuit,
        public_instances,
    } = inputs;
    generate_event_proof_from_circuit_with_transcript(
        event_km,
        circuit,
        public_instances,
        transcript,
    )
}

/// Lower-level entry point: prove an already-built [`BridgeEventFinalProof`]
/// against its public instances. Used by the `--selftest` mode of the
/// `bridge-event-prove` binary, which gets its circuit from
/// `bridge-event-prove-circuit::test_helpers::build_synthetic_final_proof_keygen_inputs`
/// rather than from a daemon-side [`PrivateWitness`].
pub fn generate_event_proof_from_circuit(
    event_km: &EventKeyManager,
    circuit: BridgeEventFinalProof,
    public_instances: Vec<Fr>,
) -> Result<EventProofOutput> {
    generate_event_proof_from_circuit_with_transcript(
        event_km,
        circuit,
        public_instances,
        TranscriptKind::Blake2b,
    )
}

/// Lower-level entry point with the chosen Fiat–Shamir transcript. Same
/// contract as [`generate_event_proof_from_circuit`] otherwise. Selecting
/// [`TranscriptKind::Poseidon`] here produces proof bytes byte-for-byte
/// compatible with `snark-verifier-sdk`'s
/// `PoseidonTranscript<NativeLoader, _>` — what the R15 ETH-side aggregator
/// pipeline requires. `Blake2b` is the AN-facing default (accepted by the
/// `ZKHALO2VERIFYWITHVK` opcode).
pub fn generate_event_proof_from_circuit_with_transcript(
    event_km: &EventKeyManager,
    circuit: BridgeEventFinalProof,
    public_instances: Vec<Fr>,
    transcript: TranscriptKind,
) -> Result<EventProofOutput> {
    let instance_refs: &[&[Fr]] = &[&public_instances];
    info!(
        "generating Circuit 4 proof: {} public instances, transcript={:?}",
        public_instances.len(),
        transcript,
    );

    let proof_bytes = match transcript {
        TranscriptKind::Blake2b => {
            let mut t = Blake2bWrite::<_, G1Affine, Challenge255<_>>::init(vec![]);
            create_proof::<
                KZGCommitmentScheme<Bn256>,
                ProverSHPLONK<'_, Bn256>,
                Challenge255<G1Affine>,
                _,
                Blake2bWrite<Vec<u8>, G1Affine, Challenge255<G1Affine>>,
                _,
            >(
                event_km.srs(),
                event_km.pk(),
                &[circuit],
                &[instance_refs],
                OsRng,
                &mut t,
            )
            .context("Circuit 4 proof generation failed (Blake2b transcript)")?;
            t.finalize()
        },
        TranscriptKind::Poseidon => {
            let mut t = PoseidonWrite::init(vec![]);
            create_proof::<
                KZGCommitmentScheme<Bn256>,
                ProverSHPLONK<'_, Bn256>,
                _,
                _,
                PoseidonWrite<Vec<u8>>,
                _,
            >(
                event_km.srs(),
                event_km.pk(),
                &[circuit],
                &[instance_refs],
                OsRng,
                &mut t,
            )
            .context("Circuit 4 proof generation failed (Poseidon transcript)")?;
            t.finalize()
        },
    };
    info!("Circuit 4 proof generated: {} bytes", proof_bytes.len());

    Ok(EventProofOutput {
        proof_bytes,
        public_instances,
    })
}

// ---------------------------------------------------------------------------
// BridgeMultiHopProof (cross-thread hop-chain snark)
// ---------------------------------------------------------------------------

/// Output of a `BridgeMultiHopProof` generation pass.
///
/// `proof_bytes` is the SHPLONK/Blake2b-encoded proof; `public_instances`
/// is the 2-slot vector `[hopStartBlockId, hopEndBlockId]` (see
/// `bridge_event_prove_circuit::multi_hop_proof::MULTI_HOP_PUBLIC_LEN`).
#[derive(Clone)]
pub struct MultiHopProofOutput {
    pub proof_bytes: Vec<u8>,
    pub public_instances: Vec<Fr>,
}

/// Native: assemble one `MultiHopProofWitness` from its JSON mirror.
///
/// Hex-decodes every `_hex` field, populates the fixed-size `HopWitness`
/// slots and re-asserts the JSON-side length invariants that the circuit
/// crate exposes as `assert_*` helpers. Bailing here rather than panicking
/// downstream keeps the daemon error path decoded.
pub fn build_multi_hop_witness_from_json(
    json: &MultiHopProofWitnessJson,
) -> Result<CircuitMultiHopProofWitness> {
    if json.hops.len() != H_HOPS_PER_PROOF {
        bail!(
            "multi_hop_witness.hops length {} != H_HOPS_PER_PROOF ({H_HOPS_PER_PROOF})",
            json.hops.len(),
        );
    }
    let hops: [CircuitHopWitness; H_HOPS_PER_PROOF] = std::array::from_fn(|_| {
        // Placeholder — overwritten in the loop below. We can't use
        // `from_fn` with `?` directly because the closure isn't fallible.
        default_hop_witness()
    });
    let mut hops = hops;
    for (i, hop_json) in json.hops.iter().enumerate() {
        hops[i] = build_hop_witness_from_json(hop_json, i)?;
    }
    Ok(CircuitMultiHopProofWitness {
        hops,
    })
}

fn default_hop_witness() -> CircuitHopWitness {
    CircuitHopWitness {
        is_active: false,
        block: CircuitBlockWitness {
            block_id: [0u8; 32],
            block_merkle_tree_leaves: [[0u8; 32]; BLOCK_MERKLE_LEAF_COUNT],
            proof_block_refs: Vec::new(),
        },
        block_merkle_leaf_proof_l7: [[0u8; 32]; BLOCK_MERKLE_DEPTH],
        ref_index: 0,
        refs_tree_depth: 0,
        proof_block_ref_inner_path: [[0u8; 32]; MAX_PROOF_BLOCK_REFS_DEPTH],
        hop_start_block_id: [0u8; 32],
        hop_end_block_id: [0u8; 32],
    }
}

fn build_hop_witness_from_json(json: &HopWitnessJson, index: usize) -> Result<CircuitHopWitness> {
    let label = |field: &str| format!("hops[{index}].{field}");

    let block_id = parse_hex_array::<32>(&label("block.block_id_hex"), &json.block.block_id_hex)?;

    if json.block.block_merkle_tree_leaves_hex.len() != BLOCK_MERKLE_LEAF_COUNT {
        bail!(
            "{}.block.block_merkle_tree_leaves_hex length {} != BLOCK_MERKLE_LEAF_COUNT \
             ({BLOCK_MERKLE_LEAF_COUNT})",
            label(""),
            json.block.block_merkle_tree_leaves_hex.len(),
        );
    }
    let mut block_merkle_tree_leaves = [[0u8; 32]; BLOCK_MERKLE_LEAF_COUNT];
    for (i, leaf_hex) in json.block.block_merkle_tree_leaves_hex.iter().enumerate() {
        block_merkle_tree_leaves[i] = parse_hex_array::<32>(
            &label(&format!("block.block_merkle_tree_leaves_hex[{i}]")),
            leaf_hex,
        )?;
    }

    let mut proof_block_refs = Vec::with_capacity(json.block.proof_block_refs_hex.len());
    for (i, ref_hex) in json.block.proof_block_refs_hex.iter().enumerate() {
        proof_block_refs.push(parse_hex_array::<32>(
            &label(&format!("block.proof_block_refs_hex[{i}]")),
            ref_hex,
        )?);
    }

    if json.block_merkle_leaf_proof_l7_hex.len() != BLOCK_MERKLE_DEPTH {
        bail!(
            "{}.block_merkle_leaf_proof_l7_hex length {} != BLOCK_MERKLE_DEPTH \
             ({BLOCK_MERKLE_DEPTH})",
            label(""),
            json.block_merkle_leaf_proof_l7_hex.len(),
        );
    }
    let mut block_merkle_leaf_proof_l7 = [[0u8; 32]; BLOCK_MERKLE_DEPTH];
    for (i, s) in json.block_merkle_leaf_proof_l7_hex.iter().enumerate() {
        block_merkle_leaf_proof_l7[i] =
            parse_hex_array::<32>(&label(&format!("block_merkle_leaf_proof_l7_hex[{i}]")), s)?;
    }

    if json.proof_block_ref_inner_path_hex.len() != MAX_PROOF_BLOCK_REFS_DEPTH {
        bail!(
            "{}.proof_block_ref_inner_path_hex length {} != MAX_PROOF_BLOCK_REFS_DEPTH \
             ({MAX_PROOF_BLOCK_REFS_DEPTH})",
            label(""),
            json.proof_block_ref_inner_path_hex.len(),
        );
    }
    let mut proof_block_ref_inner_path = [[0u8; 32]; MAX_PROOF_BLOCK_REFS_DEPTH];
    for (i, s) in json.proof_block_ref_inner_path_hex.iter().enumerate() {
        proof_block_ref_inner_path[i] =
            parse_hex_array::<32>(&label(&format!("proof_block_ref_inner_path_hex[{i}]")), s)?;
    }

    let hop_start_block_id = parse_hex_array::<32>(
        &label("hop_start_block_id_hex"),
        &json.hop_start_block_id_hex,
    )?;
    let hop_end_block_id =
        parse_hex_array::<32>(&label("hop_end_block_id_hex"), &json.hop_end_block_id_hex)?;

    let refs_tree_depth_usize = json.refs_tree_depth as usize;
    if refs_tree_depth_usize > MAX_PROOF_BLOCK_REFS_DEPTH {
        bail!(
            "{}.refs_tree_depth {} exceeds MAX_PROOF_BLOCK_REFS_DEPTH \
             ({MAX_PROOF_BLOCK_REFS_DEPTH})",
            label(""),
            json.refs_tree_depth,
        );
    }

    Ok(CircuitHopWitness {
        is_active: json.is_active,
        block: CircuitBlockWitness {
            block_id,
            block_merkle_tree_leaves,
            proof_block_refs,
        },
        block_merkle_leaf_proof_l7,
        ref_index: json.ref_index as usize,
        refs_tree_depth: json.refs_tree_depth,
        proof_block_ref_inner_path,
        hop_start_block_id,
        hop_end_block_id,
    })
}

/// Derive the 2-slot `BridgeMultiHopProof` public instances
/// `[hopStartBlockId, hopEndBlockId]` from the raw witness endpoints:
/// first hop's `hop_start_block_id`, last hop's `hop_end_block_id`.
fn multi_hop_public_instances(witness: &CircuitMultiHopProofWitness) -> Vec<Fr> {
    let start = bytes_to_fr(&witness.hops[0].hop_start_block_id);
    let end = bytes_to_fr(&witness.hops[H_HOPS_PER_PROOF - 1].hop_end_block_id);
    vec![start, end]
}

/// Generate a `BridgeMultiHopProof` from a JSON witness (Blake2b transcript).
///
/// Caller is responsible for loading the multi-hop PK before calling
/// (`mh_km.load_pk()?` first, `mh_km.unload_pk()` after) — same on-demand
/// pattern the daemon uses for the primary / layer / event PKs.
pub fn generate_multi_hop_proof(
    mh_km: &MultiHopKeyManager,
    witness_json: &MultiHopProofWitnessJson,
) -> Result<MultiHopProofOutput> {
    generate_multi_hop_proof_with_transcript(mh_km, witness_json, TranscriptKind::Blake2b)
}

/// Generate a `BridgeMultiHopProof` from a JSON witness with the chosen
/// Fiat–Shamir transcript. See
/// [`generate_event_proof_with_transcript`] for transcript semantics.
pub fn generate_multi_hop_proof_with_transcript(
    mh_km: &MultiHopKeyManager,
    witness_json: &MultiHopProofWitnessJson,
    transcript: TranscriptKind,
) -> Result<MultiHopProofOutput> {
    let native_witness = build_multi_hop_witness_from_json(witness_json)
        .context("build_multi_hop_witness_from_json failed (JSON → native)")?;
    let public_instances = multi_hop_public_instances(&native_witness);
    let circuit = BridgeMultiHopProof::new(native_witness.hops, mh_km.config().clone());
    generate_multi_hop_proof_from_circuit_with_transcript(
        mh_km,
        circuit,
        public_instances,
        transcript,
    )
}

/// Lower-level entry point: prove an already-built `BridgeMultiHopProof`
/// against its public instances (Blake2b transcript).
pub fn generate_multi_hop_proof_from_circuit(
    mh_km: &MultiHopKeyManager,
    circuit: BridgeMultiHopProof,
    public_instances: Vec<Fr>,
) -> Result<MultiHopProofOutput> {
    generate_multi_hop_proof_from_circuit_with_transcript(
        mh_km,
        circuit,
        public_instances,
        TranscriptKind::Blake2b,
    )
}

/// Lower-level entry point with the chosen Fiat–Shamir transcript. Same
/// contract as [`generate_multi_hop_proof_from_circuit`] otherwise.
pub fn generate_multi_hop_proof_from_circuit_with_transcript(
    mh_km: &MultiHopKeyManager,
    circuit: BridgeMultiHopProof,
    public_instances: Vec<Fr>,
    transcript: TranscriptKind,
) -> Result<MultiHopProofOutput> {
    if public_instances.len() != MULTI_HOP_PUBLIC_LEN {
        bail!(
            "multi-hop public instances length {} != MULTI_HOP_PUBLIC_LEN ({MULTI_HOP_PUBLIC_LEN})",
            public_instances.len(),
        );
    }
    let instance_refs: &[&[Fr]] = &[&public_instances];
    info!(
        "generating BridgeMultiHopProof: {} public instances, transcript={:?}",
        public_instances.len(),
        transcript,
    );

    let proof_bytes = match transcript {
        TranscriptKind::Blake2b => {
            let mut t = Blake2bWrite::<_, G1Affine, Challenge255<_>>::init(vec![]);
            create_proof::<
                KZGCommitmentScheme<Bn256>,
                ProverSHPLONK<'_, Bn256>,
                Challenge255<G1Affine>,
                _,
                Blake2bWrite<Vec<u8>, G1Affine, Challenge255<G1Affine>>,
                _,
            >(
                mh_km.srs(),
                mh_km.pk(),
                &[circuit],
                &[instance_refs],
                OsRng,
                &mut t,
            )
            .context("BridgeMultiHopProof proof generation failed (Blake2b transcript)")?;
            t.finalize()
        },
        TranscriptKind::Poseidon => {
            let mut t = PoseidonWrite::init(vec![]);
            create_proof::<
                KZGCommitmentScheme<Bn256>,
                ProverSHPLONK<'_, Bn256>,
                _,
                _,
                PoseidonWrite<Vec<u8>>,
                _,
            >(
                mh_km.srs(),
                mh_km.pk(),
                &[circuit],
                &[instance_refs],
                OsRng,
                &mut t,
            )
            .context("BridgeMultiHopProof proof generation failed (Poseidon transcript)")?;
            t.finalize()
        },
    };
    info!(
        "BridgeMultiHopProof proof generated: {} bytes",
        proof_bytes.len()
    );

    // Silence dead-code warning for the params helper re-export — it's kept
    // around so callers can build ad-hoc circuits at the same shape used by
    // `MultiHopKeyManager::new`.
    let _ = multi_hop_base_circuit_params;

    Ok(MultiHopProofOutput {
        proof_bytes,
        public_instances,
    })
}

// ---------------------------------------------------------------------------
// End-to-end `EventBundle` generation (final + optional hop chain)
// ---------------------------------------------------------------------------

/// Generate an [`EventBundle`] for a `WithdrawalInitiated` event.
///
/// * Same-thread claim (`hop_bundle.snarks.is_empty()`): produces exactly one
///   Circuit 4 (`BridgeEventFinalProof`) snark; `hop_blobs` and
///   `hop_public_instances` are empty.
/// * Cross-thread claim (`hop_bundle.snarks` non-empty): produces the final
///   snark plus one `BridgeMultiHopProof` per element in `hop_bundle.snarks`,
///   in order.
///
/// The caller is responsible for loading both PKs into memory before calling
/// (`event_km.load_pk()?` and `mh_km.load_pk()?` when hops present) — same
/// on-demand pattern the daemon uses for the primary and layer PKs. Both may
/// be unloaded after the call returns.
///
/// This function does **not** verify the produced bundle. Call
/// [`crate::verifier::verify_bundle`] for a full self-check.
pub fn generate_event_bundle(
    event_km: &EventKeyManager,
    mh_km: &MultiHopKeyManager,
    witness: &PrivateWitness,
    hop_bundle: &MultiHopBundleWitnessJson,
) -> Result<EventBundle> {
    let final_out = generate_event_proof(event_km, witness, hop_bundle)
        .context("generate_event_proof failed inside generate_event_bundle")?;

    let mut hop_blobs = Vec::with_capacity(hop_bundle.snarks.len());
    let mut hop_public_instances = Vec::with_capacity(hop_bundle.snarks.len());
    for (i, snark_json) in hop_bundle.snarks.iter().enumerate() {
        let mh_out = generate_multi_hop_proof(mh_km, snark_json)
            .with_context(|| format!("generate_multi_hop_proof failed for hop snark #{i}"))?;
        hop_blobs.push(mh_out.proof_bytes);
        hop_public_instances.push(mh_out.public_instances);
    }

    Ok(EventBundle {
        final_blob: final_out.proof_bytes,
        final_public_instances: final_out.public_instances,
        hop_blobs,
        hop_public_instances,
    })
}
