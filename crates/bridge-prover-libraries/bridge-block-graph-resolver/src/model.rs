//! Data types exchanged between resolver components and exposed by its API.
//!
//! Provider and store modules define behavior only; their inputs, outputs and
//! persistent/cache keys live here so library, CLI and server users share one
//! representation.

use std::{collections::BTreeMap, fmt, str::FromStr};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

macro_rules! hex_id {
    ($(#[$meta:meta])* $name:ident, $len:expr, $description:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name([u8; $len]);

        impl $name {
            pub const ZERO: Self = Self([0; $len]);

            pub const fn from_bytes(bytes: [u8; $len]) -> Self {
                Self(bytes)
            }

            pub const fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }

            pub fn is_zero(&self) -> bool {
                self.0 == [0; $len]
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&hex::encode(self.0))
            }
        }

        impl FromStr for $name {
            type Err = anyhow::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let bytes = hex::decode(value)
                    .map_err(|e| anyhow::anyhow!("invalid {} hex: {e}", $description))?;
                let bytes: [u8; $len] = bytes.try_into().map_err(|v: Vec<u8>| {
                    anyhow::anyhow!(
                        "invalid {} length: expected {} bytes, got {}",
                        $description,
                        $len,
                        v.len()
                    )
                })?;
                Ok(Self(bytes))
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(D::Error::custom)
            }
        }
    };
}

hex_id!(
    /// Canonical 256-bit block identifier, serialized as lowercase hex.
    BlockId,
    32,
    "block id"
);
hex_id!(
    /// Canonical 272-bit thread identifier, serialized as lowercase hex.
    ThreadId,
    34,
    "thread id"
);

/// Minimal finalized-block topology stored in the resolver's reverse index.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlockNode {
    pub block_id: BlockId,
    pub thread_id: ThreadId,
    /// Height within `thread_id`.
    pub height: u64,
    /// References in their original slots: parent at index 0, cross-thread
    /// references at indices 1 and above.
    pub refs: Vec<BlockId>,
}

/// Directed reference committed by a newer block to an older block.
///
/// Resolved paths are returned in proof direction (`from` to `to`): from a
/// newer thread-0 anchor toward the older target.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlockEdge {
    /// Newer block that commits this edge.
    pub from: BlockId,
    /// Referenced older block.
    pub to: BlockId,
    /// `0` is the parent; `1+` are cross-thread references.
    pub ref_index: u32,
}

impl Ord for BlockEdge {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.from, self.ref_index, self.to).cmp(&(other.from, other.ref_index, other.to))
    }
}

impl PartialOrd for BlockEdge {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Topology block accompanied by its generation timestamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimedBlock {
    pub block: BlockNode,
    /// Unix generation time in milliseconds.
    pub gen_utime_ms: u64,
}

/// Complete node-side material needed to turn a graph edge into a circuit
/// witness. This deliberately stays out of [`BlockNode`] and the reverse
/// index: proof payloads are fetched only for blocks selected for a result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProofBlock {
    pub block: BlockNode,
    /// Unix generation time in milliseconds.
    pub gen_utime_ms: u64,
    /// Hash of the serialized block envelope used by Circuit 4.
    pub envelope_hash: [u8; 32],
    /// Root of the block's tracked external outbound messages.
    pub tracked_ext_out_messages_root: [u8; 32],
    /// History roots keyed by their layer number.
    pub history_proofs: BTreeMap<u8, [u8; 32]>,
    /// Fixed leaves of the block-level Merkle tree.
    pub block_merkle_tree_leaves: [[u8; 32]; 16],
}

/// Selects which history layer may anchor the resolved graph path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnchorLayerMode {
    /// Let the provider choose a valid active layer.
    Auto,
    /// Require this one-based history layer.
    Explicit(u8),
}

/// Active history root and its key-block height.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AnchorSlot {
    pub root: [u8; 32],
    pub height: u64,
}

/// Current set of history roots against which an anchor may be proven.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AnchorSnapshot {
    /// Changes whenever the eligible history-root set changes.
    pub epoch: u64,
    /// Number of key blocks represented by the densest history layer.
    pub window_size: u64,
    /// Ratio between adjacent history layers.
    pub thinning_factor: u64,
    /// Index 0 contains L1 slots, index 1 L2 slots, and so on.
    pub layers: Vec<Vec<AnchorSlot>>,
}

/// One Merkle opening in a dense history layer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DenseOpening {
    pub leaf: [u8; 32],
    /// Zero-based leaf position in this tree.
    pub position: usize,
    /// Sibling hashes ordered from leaf level toward the root.
    pub siblings: Vec<[u8; 32]>,
}

/// Opaque history witness supplied by the data provider.
///
/// The resolver checks that it selects an active snapshot slot, but does not
/// hash or cryptographically verify it. Circuit/witness integration owns that
/// validation and the Poseidon implementation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AnchorHistoryWitness {
    /// Snapshot epoch for which this witness was built.
    pub anchor_epoch: u64,
    /// One-based selected history layer.
    pub layer: u8,
    /// Active history root selected from the snapshot.
    pub final_root: [u8; 32],
    /// Height of the key block containing the anchor leaf.
    pub anchor_key_block_height: u64,
    pub block_leaf: [u8; 32],
    /// Opening of `block_leaf` inside its key block.
    pub block_tree: DenseOpening,
    /// Openings connecting the key block to `final_root`.
    pub dense_chain: Vec<DenseOpening>,
}

/// Opaque circuit-ready opening for one selected graph edge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HopOpening {
    /// Graph edge whose reference commitment is opened.
    pub edge: BlockEdge,
    /// Opening of the reference-tree root in the source block's L7 tree.
    pub block_merkle_leaf_proof_l7: [[u8; 32]; 4],
    pub refs_tree_depth: u8,
    /// Opening of `edge.to` at `edge.ref_index` in the source block's
    /// reference tree.
    pub proof_block_ref_inner_path: Vec<[u8; 32]>,
}

/// Monotonic versions used to invalidate cached paths and anchor witnesses.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoreVersion {
    /// Increments whenever stored topology changes.
    pub graph_version: u64,
    /// Identifies the currently eligible anchor snapshot.
    pub anchor_epoch: u64,
}

/// Atomic update applied to a [`crate::ResolverStore`].
#[derive(Clone, Debug, Default)]
pub struct StoreBatch {
    pub blocks: Vec<BlockNode>,
    /// When set, atomically replaces the anchor eligibility epoch.
    pub anchor_epoch: Option<u64>,
}

/// Result of applying a [`StoreBatch`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApplyStats {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub graph_version: u64,
    pub anchor_epoch: u64,
}

/// Result of pruning old per-thread topology from a store.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PruneStats {
    pub pruned: usize,
    pub graph_version: u64,
}

/// Chooses how the resolver ranks valid paths.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResolutionPolicy {
    /// Return the first deterministic valid path discovered.
    FirstValid,
    /// Search the current indexed graph for the shortest valid path.
    ShortestCurrent,
}

/// Resource limits for one resolution attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ResolverLimits {
    /// Maximum number of edges in the returned path.
    pub max_hops: u32,
    /// Maximum number of distinct blocks examined by graph traversal.
    pub max_visited_blocks: usize,
}

/// Request to resolve a thread-0 anchor-to-target graph path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolutionRequest {
    pub target: BlockId,
    pub policy: ResolutionPolicy,
    pub limits: ResolverLimits,
}

/// Resolved path in proof direction: `anchor → ... → target`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedPath {
    pub anchor: BlockId,
    pub anchor_height: u64,
    pub target: BlockId,
    /// Ordered edges where the first starts at `anchor`, the last ends at
    /// `target`, and adjacent edges join.
    pub hops: Vec<BlockEdge>,
    /// Store versions against which this path was resolved.
    pub graph_version: u64,
    pub anchor_epoch: u64,
}

/// A resolved route together with all canonical block payloads required to
/// derive and verify the per-edge Merkle openings. `hop_blocks[i]` is the
/// newer/source block committing `path.hops[i]`; `target_block` is kept
/// separately because it commits no outgoing edge in the route.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedBlockProof {
    pub path: ResolvedPath,
    pub anchor_block: ProofBlock,
    pub target_block: ProofBlock,
    /// Source block for each edge; has the same length and order as
    /// `path.hops`.
    pub hop_blocks: Vec<ProofBlock>,
}

/// Request for a graph path plus its complete active-history witness.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AnchoredResolutionRequest {
    pub resolution: ResolutionRequest,
    pub anchor_snapshot: AnchorSnapshot,
    pub anchor_layer: AnchorLayerMode,
}

/// Circuit-ready result combining topology, edge openings and history proof.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedAnchoredBlockProof {
    pub route: ResolvedBlockProof,
    /// One opening per `route.path.hops` entry, in identical order.
    pub hop_openings: Vec<HopOpening>,
    pub history: AnchorHistoryWitness,
}

/// Complete identity of a reusable cached resolution.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PathCacheKey {
    /// Stable provider/network identity preventing cross-network cache reuse.
    pub namespace: String,
    pub target: BlockId,
    pub policy: ResolutionPolicy,
    pub limits: ResolverLimits,
    pub anchor_epoch: u64,
}

/// Combined provider fetch, store update and pruning statistics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncStats {
    pub fetched: usize,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub pruned: usize,
    pub graph_version: u64,
    pub anchor_epoch: u64,
}

/// JSON status returned by the HTTP service.
#[derive(Clone, Debug, Serialize)]
pub struct ServiceStatus {
    pub healthy: bool,
    pub namespace: String,
    pub store_version: StoreVersion,
    pub last_sync: Option<SyncStats>,
    pub last_sync_unix_seconds: Option<u64>,
    pub last_error: Option<String>,
}

/// Stable JSON body returned for HTTP API failures.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// Machine-readable error category.
    pub kind: &'static str,
    /// Human-readable diagnostic detail.
    pub error: String,
}

/// Checks whether a provider-supplied history witness selects a root that is
/// present in the given snapshot at the declared layer and height.
///
/// This is structural validation only; it does not verify Merkle hashes.
pub fn history_selects_snapshot_slot(
    witness: &AnchorHistoryWitness,
    snapshot: &AnchorSnapshot,
) -> bool {
    witness.anchor_epoch == snapshot.epoch
        && witness.layer > 0
        && snapshot
            .layers
            .get(witness.layer as usize - 1)
            .into_iter()
            .flatten()
            .any(|slot| {
                slot.height == witness.anchor_key_block_height && slot.root == witness.final_root
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_parse_display_and_serde_as_lowercase_hex() {
        let block: BlockId = "AA".repeat(32).parse().unwrap();
        assert_eq!(block.to_string(), "aa".repeat(32));
        assert_eq!(
            serde_json::to_string(&block).unwrap(),
            format!("\"{}\"", "aa".repeat(32))
        );
        assert_eq!(
            serde_json::from_str::<BlockId>(&serde_json::to_string(&block).unwrap()).unwrap(),
            block
        );

        let thread: ThreadId = "01".repeat(34).parse().unwrap();
        assert_eq!(thread.to_string(), "01".repeat(34));
        assert_eq!(
            serde_json::from_str::<ThreadId>(&format!("\"{}\"", thread)).unwrap(),
            thread
        );
    }

    #[test]
    fn ids_reject_bad_hex_and_length() {
        assert!("zz".repeat(32).parse::<BlockId>().is_err());
        assert!("00".repeat(31).parse::<BlockId>().is_err());
        assert!("00".repeat(33).parse::<ThreadId>().is_err());
    }
}
