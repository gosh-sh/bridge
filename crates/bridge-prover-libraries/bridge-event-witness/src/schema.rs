//! On-disk schema for the exported private witness JSON.
//!
//! Producers: the `bridge-event-private-witness-export` and
//! `bridge-event-witness-builder` binaries (both in the `bridge-event-witness`
//! crate), eventually also the Python orchestration driver.
//! Consumers: `bridge_event_prover_lib` (Track C), the
//! `bridge-event-halo2-prover` one-shot CLI (Track D).
//!
//! The schema is intentionally JSON-friendly: byte arrays are lowercase hex,
//! all integers fit in u64 or u128. Pinned `schema_version` so producer and
//! consumer can refuse mismatched runs the same way Phase 1 does for
//! `proof_*.json`.

use serde::{Deserialize, Serialize};

/// On-disk schema version. Bump whenever the JSON shape changes in a
/// non-backwards-compatible way.
///
/// v2: `PrivateWitness.h07_sibling_hex` added for the multi-thread
/// `BridgeEventFinalProof` circuit (13 public inputs). The circuit
/// reconstructs `x_block_id` from `(ext_out_root, h07_sibling)` via the
/// depth-4 SHA-256 L8 opening — there is no way to derive `h07_sibling`
/// from the other fields, so it must be supplied. Same-thread claims
/// still use `y_block_id = block_id_hex` (auto-derived by the prover);
/// cross-thread claims will carry `y_block_id` through the future
/// `MultiHopBundleWitnessJson` wire.
pub const SCHEMA_VERSION: u32 = 2;

/// Top-level export record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivateWitness {
    pub schema_version: u32,

    /// Hex repr_hash of the ExtOut message wrapper cell — primary identifier
    /// of this event.
    pub event_message_hash_hex: String,

    /// Block containing the emitted event message.
    pub block_id_hex: String,
    /// `seq_no` of the block above (for daemon-side anchor selection).
    pub block_seq_no: u64,

    /// Decoded event fields for human-readable diagnostics and to cross-check
    /// against the BOC walk. Not consumed by the circuit (the circuit
    /// rederives these from the raw bytes).
    pub event: WithdrawalInitiated,

    /// Flat 4-cell BFS walk of the ExtOut message DAG:
    /// `[wrapper, body, recipient, sender]`. Cell records here have exactly
    /// the layout `bridge-event-prove-circuit::boc_helper::BocFlattenData`
    /// expects.
    pub entries: [CellRecord; 4],

    /// Block-level context. The daemon (Track D) supplies these via the
    /// `BlockContext` overrides — the exporter on its own only knows what is
    /// in the ExtOut message BOC.
    pub block_context: BlockContext,

    /// Events-tree Merkle proof from this event's `ext_msg_leaf` to the
    /// block's `ext_out_messages_root`. `None` until populated by the daemon.
    pub events_tree_proof: Option<MerkleProofData>,

    /// Block-tree Merkle proof from this block's `block_leaf` to the
    /// history window's `root_1`. `None` until populated by the daemon.
    pub block_tree_proof: Option<MerkleProofData>,

    /// Anchor to a layer hash already verified by `bridge-verifier-daemon`.
    /// `None` until populated by the daemon.
    pub anchor: Option<AnchorRef>,

    /// Opaque left-aggregate sibling of the depth-4 SHA-256 block-id tree
    /// (aggregate of leaves 0..=7 of the X-block's block-id tree).
    ///
    /// Required by the multi-thread `BridgeEventFinalProof` circuit: the
    /// in-circuit L8 opening reconstructs `x_block_id` from
    /// `(ext_out_root, h07_sibling)`; this value cannot be derived from
    /// the other witness fields — it must be fetched from the source
    /// block (GraphQL / node RPC).
    ///
    /// 32-byte lowercase hex. Defaults to all-zeros so witnesses produced
    /// by the pre-v2 exporter (before GQL wiring is complete) still parse;
    /// enrichment code owns populating the real value.
    #[serde(default)]
    pub h07_sibling_hex: String,
}

/// Mirror of `bridge-event-prove-circuit::boc_helper::BocFlattenData` with
/// serde derive. Field meanings are identical.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellRecord {
    /// 32-byte SHA-256 `repr_hash` of this cell, lowercase hex.
    pub repr_hash_hex: String,
    /// Number of child references (`refs_count`).
    pub refs_count: u8,
    /// For each child, the byte offset within `cell_repr_data` at which that
    /// child's repr_hash bytes begin. Absent when `refs_count == 0`.
    pub childs_repr_hashes_offset: Option<Vec<u16>>,
    /// SHA-256 preimage whose hash equals `repr_hash`, lowercase hex.
    pub cell_repr_data_hex: String,
}

/// Block-level context needed to anchor the event into the history Merkle
/// chain. The exporter cannot derive these from just the event BOC — they
/// must be supplied by the caller (daemon or CLI flags for testing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockContext {
    pub account_dapp_id_hex: String,
    pub account_id_hex: String,
    pub envelope_hash_hex: String,
}

/// Decoded `WithdrawalInitiated` event fields. All hex strings are
/// lowercase, big-endian byte order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawalInitiated {
    /// 32 BE bytes (uint256). Stored as hex because Rust's `u128` is too
    /// narrow for the full ABI type, even though current fixtures all fit.
    pub dst_chain_id_hex: String,
    /// 16 BE bytes (uint128).
    pub amount_hex: String,
    /// 4 BE bytes (uint32).
    pub token_id: u32,
    /// 20 BE bytes (Ethereum address). Variable-length recipient support
    /// (up to 64 bytes) is documented as future work in the circuit's
    /// `EVENT_LAYOUT_COMPARISON.md` §5.6.
    pub recipient_hex: String,
    /// 34 BE bytes of the sender cell payload (`std_addr$10` + anycast flag +
    /// workchain(8) + acc_id(256)), i.e. `entries[3].cell_repr_data[2..]`.
    /// Diagnostic only — the sender is cryptographically committed via
    /// `sha256(sender cell)` inside the body cell and the ExtOut wrapper's
    /// message hash, both of which are consumed by the circuit.
    #[serde(default)]
    pub sender_hex: String,
}

/// Generic Merkle proof data — used for both the events tree and the
/// block tree (and the dense chain step proofs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProofData {
    pub position: u32,
    /// Bottom-up sibling hashes, each 32 bytes hex.
    pub siblings_hex: Vec<String>,
}

/// Reference to a layer hash already mirrored by the verifier — the
/// "anchor" the event proof binds to. The daemon (Track D) populates this
/// from `state/verifier_state.json`. circuit exposes a single `final_root`
/// public input, so the daemon only needs to supply the matching anchor hash
/// (and the dense chain to rebuild it inside the circuit).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorRef {
    /// Which layer in `layer_windows` we chose (0 = L1, 1 = L2, ...).
    pub layer_idx: u32,
    /// Slot's `last_height` — for human verification.
    pub height: u64,
    /// Selected layer hash, 32 bytes hex. This is the value the prover
    /// will publish as `PUB_FINAL_ROOT`, and the verifier checks it
    /// against its current `layer_windows` snapshot off-circuit.
    pub layer_hash_hex: String,
    /// Dense chain (history window proof) — daemon-side artifact.
    pub dense_chain: Vec<DenseChainLinkSer>,
    /// How many of `dense_chain` are active (rest are inactive padding to
    /// `MAX_CHAIN_LEN`).
    pub num_active_chain_steps: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenseChainLinkSer {
    pub active: bool,
    pub position: u32,
    pub siblings_hex: Vec<String>,
    pub leaf_hex: String,
}

// ---------------------------------------------------------------------------
// Cross-thread bundle witness — JSON mirrors of
// `bridge_event_prove_circuit::multi_hop_witness::{BlockWitness, HopWitness,
// MultiHopProofWitness}`.
//
// Constants below duplicate the canonical values in that module because this
// crate deliberately avoids a circuit-crate dep at the schema layer (see the
// mirror pattern used by `CellRecord` above). Keep in sync — a mismatch will
// surface at circuit ingest, not JSON parse.
// ---------------------------------------------------------------------------

/// SHA-256 leaves in the per-block outer merkle. Mirrors
/// `multi_hop_witness::BLOCK_MERKLE_LEAF_COUNT`.
pub const BLOCK_MERKLE_LEAF_COUNT: usize = 16;

/// Depth of the per-block SHA-256 merkle. Mirrors
/// `multi_hop_witness::BLOCK_MERKLE_DEPTH`.
pub const BLOCK_MERKLE_DEPTH: usize = 4;

/// Hops per BridgeMultiHopProof snark. Mirrors
/// `multi_hop_witness::H_HOPS_PER_PROOF`.
pub const H_HOPS_PER_PROOF: usize = 1;

/// Maximum BridgeMultiHopProof snarks per bundle at the prototype cap.
/// Mirrors `multi_hop_witness::N_BUNDLE_MAX`.
pub const N_BUNDLE_MAX: usize = 20;

/// Depth of the in-circuit L7 inner-path fold (padded regardless of the
/// per-hop `refs_tree_depth`). Mirrors
/// `multi_hop_witness::MAX_PROOF_BLOCK_REFS_DEPTH`.
pub const MAX_PROOF_BLOCK_REFS_DEPTH: usize = 8;

/// JSON mirror of `multi_hop_witness::BlockWitness`. All 32-byte fields are
/// lowercase hex.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockWitnessJson {
    /// 32-byte block id (`BlockWitness::block_id`), lowercase hex.
    pub block_id_hex: String,
    /// The 16 SHA-256 leaves L0..L15
    /// (`BlockWitness::block_merkle_tree_leaves`), each 32 bytes hex.
    pub block_merkle_tree_leaves_hex: [String; BLOCK_MERKLE_LEAF_COUNT],
    /// Variable-length referenced-block-id list
    /// (`BlockWitness::proof_block_refs`), each 32 bytes hex. Length ≤
    /// `multi_hop_witness::MAX_PROOF_BLOCK_REFS` (protocol cap; enforced by
    /// the circuit ingest, not by JSON parse).
    pub proof_block_refs_hex: Vec<String>,
}

/// JSON mirror of `multi_hop_witness::HopWitness`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HopWitnessJson {
    /// Real-vs-padding flag (`HopWitness::is_active`). Padded hops satisfy
    /// `hop_start_block_id_hex == hop_end_block_id_hex`.
    pub is_active: bool,
    /// The hop's **start** block (`HopWitness::block`) under Direction (a):
    /// `block.block_id_hex` == `hop_start_block_id_hex` (current/newer). The
    /// hop's end (older ref) lives in `block.proof_block_refs_hex[ref_index]`.
    pub block: BlockWitnessJson,
    /// SHA-256 merkle opening for L7 against `block.block_id_hex`,
    /// `BLOCK_MERKLE_DEPTH = 4` siblings.
    pub block_merkle_leaf_proof_l7_hex: [String; BLOCK_MERKLE_DEPTH],
    /// Index of the referenced parent within `block.proof_block_refs_hex`.
    /// Must be ≥ 1 for the bridge L7 walk (slot 0 is same-thread).
    pub ref_index: u32,
    /// Real depth of the L7 dense-merkle tree for this hop, in
    /// `[0, MAX_PROOF_BLOCK_REFS_DEPTH]`.
    pub refs_tree_depth: u8,
    /// Dense-merkle siblings for the L7 opening, padded to
    /// `MAX_PROOF_BLOCK_REFS_DEPTH = 8`. Only the first `refs_tree_depth`
    /// entries are used inside the circuit.
    pub proof_block_ref_inner_path_hex: [String; MAX_PROOF_BLOCK_REFS_DEPTH],
    /// Hop's start endpoint as clear bytes, hex — Direction (a): the current
    /// (newer) block whose L7 walk this hop closes. Equal to
    /// `block.block_id_hex` for active hops.
    pub hop_start_block_id_hex: String,
    /// Hop's end endpoint as clear bytes, hex — Direction (a): the older ref
    /// extracted from `block.proof_block_refs_hex[ref_index]`. Threads into
    /// the next hop's `hop_start_block_id_hex` as intra-bundle continuity.
    pub hop_end_block_id_hex: String,
}

/// JSON mirror of `multi_hop_witness::MultiHopProofWitness` — one
/// BridgeMultiHopProof snark's worth of hops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiHopProofWitnessJson {
    pub hops: [HopWitnessJson; H_HOPS_PER_PROOF],
}

/// A whole cross-thread bundle: up to `N_BUNDLE_MAX` snarks. Same-thread
/// claims serialize as `{ "snarks": [] }` (a zero-length bundle is the
/// signal that the payload is not cross-thread).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MultiHopBundleWitnessJson {
    pub snarks: Vec<MultiHopProofWitnessJson>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h32(byte: u8) -> String {
        hex::encode([byte; 32])
    }

    fn sample_block(seed: u8) -> BlockWitnessJson {
        let leaves: [String; BLOCK_MERKLE_LEAF_COUNT] =
            std::array::from_fn(|i| h32(seed ^ i as u8));
        BlockWitnessJson {
            block_id_hex: h32(seed),
            block_merkle_tree_leaves_hex: leaves,
            proof_block_refs_hex: vec![h32(seed ^ 0xF0), h32(seed ^ 0xF1)],
        }
    }

    fn sample_hop(seed: u8, active: bool) -> HopWitnessJson {
        let siblings: [String; BLOCK_MERKLE_DEPTH] =
            std::array::from_fn(|i| h32(seed ^ 0x10 ^ i as u8));
        let inner: [String; MAX_PROOF_BLOCK_REFS_DEPTH] =
            std::array::from_fn(|i| h32(seed ^ 0x20 ^ i as u8));
        HopWitnessJson {
            is_active: active,
            block: sample_block(seed),
            block_merkle_leaf_proof_l7_hex: siblings,
            ref_index: 1,
            refs_tree_depth: 1,
            proof_block_ref_inner_path_hex: inner,
            hop_start_block_id_hex: h32(seed ^ 0xA0),
            hop_end_block_id_hex: h32(seed ^ 0xA1),
        }
    }

    fn sample_snark(seed: u8) -> MultiHopProofWitnessJson {
        let hops: [HopWitnessJson; H_HOPS_PER_PROOF] =
            std::array::from_fn(|i| sample_hop(seed ^ i as u8, i < 3));
        MultiHopProofWitnessJson {
            hops,
        }
    }

    #[test]
    fn multi_hop_proof_json_roundtrip() {
        let s = sample_snark(0x42);
        let raw = serde_json::to_string(&s).expect("serialize");
        let back: MultiHopProofWitnessJson = serde_json::from_str(&raw).expect("deserialize");
        // Spot-check a few fields — array PartialEq is not derived, so
        // compare via re-serialisation.
        assert_eq!(back.hops.len(), H_HOPS_PER_PROOF);
        assert_eq!(
            back.hops[0].block.block_id_hex,
            s.hops[0].block.block_id_hex
        );
        assert_eq!(
            back.hops[0].proof_block_ref_inner_path_hex,
            s.hops[0].proof_block_ref_inner_path_hex
        );
        let raw2 = serde_json::to_string(&back).expect("re-serialize");
        assert_eq!(raw, raw2);
    }

    #[test]
    fn multi_hop_bundle_json_roundtrip() {
        let bundle = MultiHopBundleWitnessJson {
            snarks: (0..N_BUNDLE_MAX)
                .map(|i| sample_snark(0x10 + i as u8))
                .collect(),
        };
        let raw = serde_json::to_string(&bundle).expect("serialize");
        let back: MultiHopBundleWitnessJson = serde_json::from_str(&raw).expect("deserialize");
        assert_eq!(back.snarks.len(), N_BUNDLE_MAX);
        let raw2 = serde_json::to_string(&back).expect("re-serialize");
        assert_eq!(raw, raw2);
    }

    #[test]
    fn empty_bundle_signals_same_thread() {
        let bundle = MultiHopBundleWitnessJson::default();
        let raw = serde_json::to_string(&bundle).expect("serialize");
        assert_eq!(raw, r#"{"snarks":[]}"#);
        let back: MultiHopBundleWitnessJson = serde_json::from_str(&raw).expect("deserialize");
        assert!(back.snarks.is_empty());
    }
}
