use std::collections::HashMap;

use async_trait::async_trait;

use crate::{
    ApplyStats, BlockEdge, BlockId, BlockNode, PathCacheKey, PruneStats, ResolvedPath, StoreBatch,
    StoreVersion,
};

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
