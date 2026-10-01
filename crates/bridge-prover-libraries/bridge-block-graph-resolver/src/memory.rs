use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::{
    ApplyStats, BlockEdge, BlockId, BlockNode, PathCacheKey, PruneStats, ResolvedPath,
    ResolverStore, StoreBatch, StoreVersion, ThreadId,
};

#[derive(Default)]
struct MemoryState {
    blocks: HashMap<BlockId, BlockNode>,
    incoming: HashMap<BlockId, Vec<BlockEdge>>,
    paths: HashMap<PathCacheKey, ResolvedPath>,
    graph_version: u64,
    anchor_epoch: u64,
}

#[derive(Default)]
pub struct MemoryStore {
    state: RwLock<MemoryState>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn remove_outgoing(state: &mut MemoryState, block: &BlockNode) {
    for (index, target) in block.refs.iter().enumerate() {
        if let Some(edges) = state.incoming.get_mut(target) {
            edges.retain(|edge| !(edge.from == block.block_id && edge.ref_index == index as u32));
            if edges.is_empty() {
                state.incoming.remove(target);
            }
        }
    }
}

fn add_outgoing(state: &mut MemoryState, block: &BlockNode) {
    for (index, target) in block.refs.iter().enumerate() {
        state.incoming.entry(*target).or_default().push(BlockEdge {
            from: block.block_id,
            to: *target,
            ref_index: index as u32,
        });
    }
    for target in &block.refs {
        if let Some(edges) = state.incoming.get_mut(target) {
            edges.sort_unstable();
            edges.dedup();
        }
    }
}

#[async_trait]
impl ResolverStore for MemoryStore {
    async fn version(&self) -> anyhow::Result<StoreVersion> {
        let state = self.state.read().await;
        Ok(StoreVersion {
            graph_version: state.graph_version,
            anchor_epoch: state.anchor_epoch,
        })
    }

    async fn block(&self, id: &BlockId) -> anyhow::Result<Option<BlockNode>> {
        Ok(self.state.read().await.blocks.get(id).cloned())
    }

    async fn blocks(&self, ids: &[BlockId]) -> anyhow::Result<HashMap<BlockId, BlockNode>> {
        let state = self.state.read().await;
        Ok(ids
            .iter()
            .filter_map(|id| state.blocks.get(id).cloned().map(|block| (*id, block)))
            .collect())
    }

    async fn incoming_edges(
        &self,
        ids: &[BlockId],
    ) -> anyhow::Result<HashMap<BlockId, Vec<BlockEdge>>> {
        let state = self.state.read().await;
        Ok(ids
            .iter()
            .filter_map(|id| state.incoming.get(id).cloned().map(|edges| (*id, edges)))
            .collect())
    }

    async fn apply(&self, batch: StoreBatch) -> anyhow::Result<ApplyStats> {
        let mut state = self.state.write().await;
        let mut stats = ApplyStats::default();
        let mut changed = false;

        for block in batch.blocks {
            match state.blocks.get(&block.block_id).cloned() {
                Some(existing) if existing == block => stats.unchanged += 1,
                Some(existing) => {
                    remove_outgoing(&mut state, &existing);
                    add_outgoing(&mut state, &block);
                    state.blocks.insert(block.block_id, block);
                    stats.updated += 1;
                    changed = true;
                },
                None => {
                    add_outgoing(&mut state, &block);
                    state.blocks.insert(block.block_id, block);
                    stats.inserted += 1;
                    changed = true;
                },
            }
        }
        if let Some(epoch) = batch.anchor_epoch {
            state.anchor_epoch = epoch;
        }
        if changed {
            state.graph_version = state
                .graph_version
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("memory store graph version overflow"))?;
        }
        stats.graph_version = state.graph_version;
        stats.anchor_epoch = state.anchor_epoch;
        Ok(stats)
    }

    async fn prune_recent(&self, per_thread: usize) -> anyhow::Result<PruneStats> {
        let mut state = self.state.write().await;
        if per_thread == 0 {
            return Ok(PruneStats {
                graph_version: state.graph_version,
                ..PruneStats::default()
            });
        }

        let mut by_thread: HashMap<ThreadId, Vec<(u64, BlockId)>> = HashMap::new();
        for block in state.blocks.values() {
            by_thread
                .entry(block.thread_id)
                .or_default()
                .push((block.height, block.block_id));
        }
        let mut remove = HashSet::new();
        for blocks in by_thread.values_mut() {
            blocks.sort_unstable_by(|a, b| b.cmp(a));
            remove.extend(blocks.iter().skip(per_thread).map(|(_, id)| *id));
        }
        for id in &remove {
            if let Some(block) = state.blocks.remove(id) {
                remove_outgoing(&mut state, &block);
            }
        }
        if !remove.is_empty() {
            state.graph_version = state
                .graph_version
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("memory store graph version overflow"))?;
        }
        Ok(PruneStats {
            pruned: remove.len(),
            graph_version: state.graph_version,
        })
    }

    async fn cached_path(&self, key: &PathCacheKey) -> anyhow::Result<Option<ResolvedPath>> {
        Ok(self.state.read().await.paths.get(key).cloned())
    }

    async fn cache_path(&self, key: PathCacheKey, path: ResolvedPath) -> anyhow::Result<()> {
        self.state.write().await.paths.insert(key, path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ResolutionPolicy, ResolverLimits};

    fn id(n: u8) -> BlockId {
        BlockId::from_bytes([n; 32])
    }

    fn thread(n: u8) -> ThreadId {
        ThreadId::from_bytes([n; 34])
    }

    fn block(n: u8, t: u8, height: u64, refs: &[u8]) -> BlockNode {
        BlockNode {
            block_id: id(n),
            thread_id: thread(t),
            height,
            refs: refs.iter().copied().map(id).collect(),
        }
    }

    #[tokio::test]
    async fn store_contract_apply_lookup_replace_version_and_edges() {
        let store = MemoryStore::new();
        let one = block(1, 1, 1, &[2, 3]);
        let stats = store
            .apply(StoreBatch {
                blocks: vec![one.clone()],
                anchor_epoch: None,
            })
            .await
            .unwrap();
        assert_eq!((stats.inserted, stats.graph_version), (1, 1));
        assert_eq!(store.block(&id(1)).await.unwrap(), Some(one.clone()));
        assert_eq!(store.blocks(&[id(1), id(9)]).await.unwrap().len(), 1);
        let incoming = store.incoming_edges(&[id(2), id(3)]).await.unwrap();
        assert_eq!(incoming[&id(2)][0].ref_index, 0);
        assert_eq!(incoming[&id(3)][0].ref_index, 1);

        let repeated = store
            .apply(StoreBatch {
                blocks: vec![one],
                anchor_epoch: None,
            })
            .await
            .unwrap();
        assert_eq!((repeated.unchanged, repeated.graph_version), (1, 1));

        let epoch = store
            .apply(StoreBatch {
                blocks: vec![],
                anchor_epoch: Some(7),
            })
            .await
            .unwrap();
        assert_eq!((epoch.graph_version, epoch.anchor_epoch), (1, 7));

        let replacement = block(1, 1, 1, &[4]);
        let replaced = store
            .apply(StoreBatch {
                blocks: vec![replacement],
                anchor_epoch: None,
            })
            .await
            .unwrap();
        assert_eq!((replaced.updated, replaced.graph_version), (1, 2));
        assert!(store.incoming_edges(&[id(2)]).await.unwrap().is_empty());
        assert_eq!(
            store.incoming_edges(&[id(4)]).await.unwrap()[&id(4)][0].ref_index,
            0
        );
    }

    #[tokio::test]
    async fn store_contract_pruning_keeps_highest_per_thread_and_target_keys() {
        let store = MemoryStore::new();
        store
            .apply(StoreBatch {
                blocks: vec![
                    block(1, 1, 1, &[9]),
                    block(2, 1, 2, &[1]),
                    block(3, 2, 1, &[]),
                ],
                anchor_epoch: None,
            })
            .await
            .unwrap();
        let pruned = store.prune_recent(1).await.unwrap();
        assert_eq!(pruned.pruned, 1);
        assert!(store.block(&id(1)).await.unwrap().is_none());
        assert!(store.block(&id(2)).await.unwrap().is_some());
        assert_eq!(
            store.incoming_edges(&[id(1)]).await.unwrap()[&id(1)][0].from,
            id(2)
        );
        assert!(store.incoming_edges(&[id(9)]).await.unwrap().is_empty());
        assert_eq!(store.prune_recent(0).await.unwrap().pruned, 0);
    }

    #[tokio::test]
    async fn store_contract_cache_key_includes_policy_limits_and_epoch() {
        let store = MemoryStore::new();
        let base = PathCacheKey {
            namespace: "test".into(),
            target: id(1),
            policy: ResolutionPolicy::FirstValid,
            limits: ResolverLimits {
                max_hops: 2,
                max_visited_blocks: 3,
            },
            anchor_epoch: 4,
        };
        let path = ResolvedPath {
            anchor: id(2),
            anchor_height: 2,
            target: id(1),
            hops: vec![],
            graph_version: 1,
            anchor_epoch: 4,
        };
        store.cache_path(base.clone(), path.clone()).await.unwrap();
        assert_eq!(store.cached_path(&base).await.unwrap(), Some(path));
        let mut other = base;
        other.anchor_epoch += 1;
        assert!(store.cached_path(&other).await.unwrap().is_none());
        other = PathCacheKey {
            anchor_epoch: 4,
            policy: ResolutionPolicy::ShortestCurrent,
            ..other
        };
        assert!(store.cached_path(&other).await.unwrap().is_none());
        other = PathCacheKey {
            policy: ResolutionPolicy::FirstValid,
            limits: ResolverLimits {
                max_hops: 99,
                max_visited_blocks: 3,
            },
            ..other
        };
        assert!(store.cached_path(&other).await.unwrap().is_none());
    }
}
