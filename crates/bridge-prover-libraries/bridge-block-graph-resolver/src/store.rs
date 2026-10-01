use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{BlockEdge, BlockId, BlockNode};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoreVersion {
    pub graph_version: u64,
    pub anchor_epoch: u64,
}

#[derive(Clone, Debug, Default)]
pub struct StoreBatch {
    pub blocks: Vec<BlockNode>,
    /// When set, atomically replaces the anchor eligibility epoch.
    pub anchor_epoch: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApplyStats {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub graph_version: u64,
    pub anchor_epoch: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PruneStats {
    pub pruned: usize,
    pub graph_version: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResolutionPolicy {
    FirstValid,
    ShortestCurrent,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ResolverLimits {
    pub max_hops: u32,
    pub max_visited_blocks: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolutionRequest {
    pub target: BlockId,
    pub policy: ResolutionPolicy,
    pub limits: ResolverLimits,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedPath {
    pub anchor: BlockId,
    pub anchor_height: u64,
    pub target: BlockId,
    pub hops: Vec<BlockEdge>,
    pub graph_version: u64,
    pub anchor_epoch: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PathCacheKey {
    pub namespace: String,
    pub target: BlockId,
    pub policy: ResolutionPolicy,
    pub limits: ResolverLimits,
    pub anchor_epoch: u64,
}

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

#[async_trait]
pub trait ResolverStore: Send + Sync + 'static {
    async fn version(&self) -> anyhow::Result<StoreVersion>;
    async fn block(&self, id: &BlockId) -> anyhow::Result<Option<BlockNode>>;
    async fn blocks(&self, ids: &[BlockId]) -> anyhow::Result<HashMap<BlockId, BlockNode>>;
    async fn incoming_edges(
        &self,
        ids: &[BlockId],
    ) -> anyhow::Result<HashMap<BlockId, Vec<BlockEdge>>>;
    async fn apply(&self, batch: StoreBatch) -> anyhow::Result<ApplyStats>;
    async fn prune_recent(&self, per_thread: usize) -> anyhow::Result<PruneStats>;
    async fn cached_path(&self, key: &PathCacheKey) -> anyhow::Result<Option<ResolvedPath>>;
    async fn cache_path(&self, key: PathCacheKey, path: ResolvedPath) -> anyhow::Result<()>;
}
