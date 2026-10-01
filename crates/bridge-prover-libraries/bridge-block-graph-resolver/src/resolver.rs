use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use thiserror::Error;

use crate::{
    AnchoredResolutionRequest, BlockEdge, BlockId, BlockNode, BlockProvider, PathCacheKey,
    ResolutionPolicy, ResolutionRequest, ResolvedAnchoredBlockProof, ResolvedBlockProof,
    ResolvedPath, ResolverStore, StoreBatch, SyncStats,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoricalSearchConfig {
    /// Maximum number of consecutive thread-0 anchors examined after the
    /// timestamp lower bound.
    pub max_anchor_candidates: usize,
}

#[derive(Default)]
struct HistoricalSearchState {
    /// Greatest remaining hop budget with which a block's references have
    /// already been expanded. Re-expand only when a later anchor reaches the
    /// block with a strictly larger budget.
    expanded_remaining: HashMap<BlockId, u32>,
    /// Edges discovered by forward traversal, indexed locally by destination
    /// so a newly found suffix can be propagated to its already-seen parents.
    dependents: HashMap<BlockId, Vec<BlockEdge>>,
    /// Best currently known forward path from a block to the exact target.
    suffixes: HashMap<BlockId, Vec<BlockEdge>>,
    terminal_checked: HashSet<BlockId>,
}

impl Default for HistoricalSearchConfig {
    fn default() -> Self {
        Self {
            max_anchor_candidates: 1_000,
        }
    }
}

#[derive(Debug, Error)]
pub enum ResolutionError {
    #[error("target block {target} was not found by provider {namespace}")]
    TargetNotFound { target: BlockId, namespace: String },
    #[error(
        "no path from a thread-0 anchor to {target}; graph_version={graph_version}, \
         visited={visited}, depth_reached={depth_reached}, max_hops={max_hops}, \
         max_visited_blocks={max_visited_blocks}"
    )]
    NoPath {
        target: BlockId,
        graph_version: u64,
        visited: usize,
        depth_reached: u32,
        max_hops: u32,
        max_visited_blocks: usize,
    },
    #[error(
        "visited-block limit reached while resolving {target}; graph_version={graph_version}, \
         visited={visited}, depth_reached={depth_reached}, max_hops={max_hops}, \
         max_visited_blocks={max_visited_blocks}"
    )]
    MaxVisited {
        target: BlockId,
        graph_version: u64,
        visited: usize,
        depth_reached: u32,
        max_hops: u32,
        max_visited_blocks: usize,
    },
    #[error(
        "historical anchor search limit reached while resolving {target}; \
         graph_version={graph_version}, candidates={candidates}, \
         max_anchor_candidates={max_anchor_candidates}"
    )]
    HistoricalSearchLimit {
        target: BlockId,
        graph_version: u64,
        candidates: usize,
        max_anchor_candidates: usize,
    },
    #[error("proof material for block {block} was not found by provider {namespace}")]
    ProofBlockNotFound { block: BlockId, namespace: String },
    #[error("invalid proof material for block {block}: {reason}")]
    InvalidProofBlock { block: BlockId, reason: String },
}

pub struct GraphResolver<P, S> {
    provider: Arc<P>,
    store: Arc<S>,
    per_thread_window: usize,
    historical_search: HistoricalSearchConfig,
}

impl<P, S> GraphResolver<P, S>
where
    P: BlockProvider,
    S: ResolverStore,
{
    pub fn new(provider: Arc<P>, store: Arc<S>, per_thread_window: usize) -> Self {
        Self {
            provider,
            store,
            per_thread_window,
            historical_search: HistoricalSearchConfig::default(),
        }
    }

    pub fn with_historical_search_config(
        mut self,
        historical_search: HistoricalSearchConfig,
    ) -> Self {
        self.historical_search = historical_search;
        self
    }

    pub async fn store_version(&self) -> anyhow::Result<crate::StoreVersion> {
        self.store.version().await
    }

    pub async fn sync_latest(&self, limit: usize) -> anyhow::Result<SyncStats> {
        let blocks = self.provider.latest_blocks(limit).await?;
        validate_blocks(&blocks)?;
        let fetched = blocks.len();
        let applied = self
            .store
            .apply(StoreBatch {
                blocks,
                anchor_epoch: None,
            })
            .await?;
        let pruned = self.store.prune_recent(self.per_thread_window).await?;
        let version = self.store.version().await?;
        Ok(SyncStats {
            fetched,
            inserted: applied.inserted,
            updated: applied.updated,
            unchanged: applied.unchanged,
            pruned: pruned.pruned,
            graph_version: version.graph_version,
            anchor_epoch: version.anchor_epoch,
        })
    }

    pub async fn resolve(&self, request: ResolutionRequest) -> anyhow::Result<ResolvedPath> {
        let mut version = self.store.version().await?;
        let key = PathCacheKey {
            namespace: self.provider.namespace().to_owned(),
            target: request.target,
            policy: request.policy,
            limits: request.limits,
            anchor_epoch: version.anchor_epoch,
        };
        if let Some(path) = self.store.cached_path(&key).await? {
            let usable = request.policy == ResolutionPolicy::FirstValid
                || (path.graph_version == version.graph_version
                    && path.anchor_epoch == version.anchor_epoch);
            if usable {
                return Ok(path);
            }
        }

        let target = match self.store.block(&request.target).await? {
            Some(block) => block,
            None => {
                let block = self
                    .provider
                    .block_by_id(&request.target)
                    .await?
                    .ok_or_else(|| ResolutionError::TargetNotFound {
                        target: request.target,
                        namespace: self.provider.namespace().to_owned(),
                    })?;
                validate_blocks(std::slice::from_ref(&block))?;
                self.store
                    .apply(StoreBatch {
                        blocks: vec![block.clone()],
                        anchor_epoch: None,
                    })
                    .await?;
                version = self.store.version().await?;
                block
            },
        };

        let initial = self
            .bfs(
                &request,
                target.clone(),
                version.graph_version,
                version.anchor_epoch,
            )
            .await;
        let path = match initial {
            Ok(path) => path,
            Err(error)
                if matches!(
                    error.downcast_ref::<ResolutionError>(),
                    Some(ResolutionError::NoPath { .. })
                ) =>
            {
                self.historical_resolve(&request, target, error).await?
            },
            Err(error) => return Err(error),
        };
        self.cache(&key, &path).await?;
        Ok(path)
    }

    /// Resolve a route and hydrate every block that commits one of its edges
    /// with the canonical node-side payload needed by the proof circuit.
    ///
    /// This method does not generate a Halo2 proof. It returns a complete,
    /// circuit-neutral proof source and fails if the provider's topology and
    /// proof views disagree at any edge.
    pub async fn resolve_proof(
        &self,
        request: ResolutionRequest,
    ) -> anyhow::Result<ResolvedBlockProof> {
        let path = self.resolve(request).await?;
        self.hydrate_path(path).await
    }

    async fn hydrate_path(&self, path: ResolvedPath) -> anyhow::Result<ResolvedBlockProof> {
        let mut loaded = HashMap::<BlockId, crate::ProofBlock>::new();
        let mut ids = Vec::with_capacity(path.hops.len() + 2);
        ids.push(path.anchor);
        ids.extend(path.hops.iter().map(|edge| edge.from));
        ids.push(path.target);
        ids.sort_unstable();
        ids.dedup();

        for id in ids {
            let proof = self.provider.proof_block_by_id(&id).await?.ok_or_else(|| {
                ResolutionError::ProofBlockNotFound {
                    block: id,
                    namespace: self.provider.namespace().to_owned(),
                }
            })?;
            if proof.block.block_id != id {
                return Err(ResolutionError::InvalidProofBlock {
                    block: id,
                    reason: format!("provider returned block id {}", proof.block.block_id),
                }
                .into());
            }
            loaded.insert(id, proof);
        }

        let mut hop_blocks = Vec::with_capacity(path.hops.len());
        for edge in &path.hops {
            let block = loaded
                .get(&edge.from)
                .ok_or_else(|| anyhow::anyhow!("internal proof hydration gap at {}", edge.from))?;
            let index = usize::try_from(edge.ref_index)
                .map_err(|_| anyhow::anyhow!("reference index exceeds usize"))?;
            if block.block.refs.get(index).copied() != Some(edge.to) {
                return Err(ResolutionError::InvalidProofBlock {
                    block: edge.from,
                    reason: format!(
                        "refs[{}] does not point to route destination {}",
                        edge.ref_index, edge.to
                    ),
                }
                .into());
            }
            hop_blocks.push(block.clone());
        }

        let anchor_block = loaded
            .get(&path.anchor)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("internal anchor proof hydration gap"))?;
        let target_block = loaded
            .get(&path.target)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("internal target proof hydration gap"))?;
        Ok(ResolvedBlockProof {
            path,
            anchor_block,
            target_block,
            hop_blocks,
        })
    }

    /// Find the nearest reachable thread-0 block at or to the right of the
    /// target for which the provider supplies an opaque witness selecting one
    /// of the active verifier roots. No cryptographic hashing happens here.
    pub async fn resolve_anchored_proof(
        &self,
        request: AnchoredResolutionRequest,
    ) -> anyhow::Result<ResolvedAnchoredBlockProof> {
        let graph_request = request.resolution;
        let target_timed = self
            .provider
            .timed_block_by_id(&graph_request.target)
            .await?
            .ok_or_else(|| ResolutionError::TargetNotFound {
                target: graph_request.target,
                namespace: self.provider.namespace().to_owned(),
            })?;
        let target = target_timed.block.clone();
        let thread_zero = crate::ThreadId::ZERO;
        let tip = self
            .provider
            .latest_timed_block_in_thread(&thread_zero)
            .await?
            .ok_or_else(|| anyhow::anyhow!("thread 0 has no blocks"))?;
        let start = self
            .thread_zero_lower_bound(target_timed.gen_utime_ms, tip.block.height)
            .await?;
        let mut visited = HashSet::from([graph_request.target]);
        let mut depth = 0;
        let mut search = HistoricalSearchState::default();
        search.suffixes.insert(graph_request.target, Vec::new());
        for (count, height) in (start..=tip.block.height).enumerate() {
            if count >= self.historical_search.max_anchor_candidates {
                break;
            }
            let anchor = self
                .provider
                .timed_block_by_height(&thread_zero, height)
                .await?
                .ok_or_else(|| anyhow::anyhow!("thread 0 has no block at height {height}"))?
                .block;
            let Some(path) = self
                .incremental_path_from_anchor(
                    &graph_request,
                    Some(&target),
                    target_timed.gen_utime_ms,
                    anchor,
                    &mut visited,
                    &mut depth,
                    &mut search,
                )
                .await?
            else {
                continue;
            };
            let Some(history) = self
                .provider
                .anchor_history_witness(
                    &path.anchor,
                    &request.anchor_snapshot,
                    request.anchor_layer,
                )
                .await?
            else {
                continue;
            };
            if !crate::history_selects_snapshot_slot(&history, &request.anchor_snapshot) {
                continue;
            }
            let route = self.hydrate_path(path).await?;
            let mut hop_openings = Vec::with_capacity(route.path.hops.len());
            for (edge, block) in route.path.hops.iter().copied().zip(&route.hop_blocks) {
                let opening = self
                    .provider
                    .hop_opening(block, edge)
                    .await?
                    .ok_or_else(|| ResolutionError::InvalidProofBlock {
                        block: edge.from,
                        reason: format!(
                            "provider returned no opening for refs[{}]",
                            edge.ref_index
                        ),
                    })?;
                if opening.edge != edge {
                    return Err(ResolutionError::InvalidProofBlock {
                        block: edge.from,
                        reason: "provider returned an opening for a different edge".to_owned(),
                    }
                    .into());
                }
                hop_openings.push(opening);
            }
            return Ok(ResolvedAnchoredBlockProof {
                route,
                hop_openings,
                history,
            });
        }
        anyhow::bail!("no reachable thread-0 anchor is covered by the supplied verifier snapshot")
    }

    async fn cache(&self, key: &PathCacheKey, path: &ResolvedPath) -> anyhow::Result<()> {
        let final_key = PathCacheKey {
            anchor_epoch: path.anchor_epoch,
            ..key.clone()
        };
        self.store.cache_path(final_key, path.clone()).await?;
        Ok(())
    }

    async fn historical_resolve(
        &self,
        request: &ResolutionRequest,
        target: BlockNode,
        initial_error: anyhow::Error,
    ) -> anyhow::Result<ResolvedPath> {
        if self.historical_search.max_anchor_candidates == 0 {
            return Err(initial_error);
        }
        let Some(target_timed) = self.provider.timed_block_by_id(&request.target).await? else {
            return Err(initial_error);
        };
        anyhow::ensure!(
            target_timed.block.block_id == target.block_id,
            "provider returned mismatched timed target block"
        );
        let thread_zero = crate::ThreadId::ZERO;
        let Some(tip) = self
            .provider
            .latest_timed_block_in_thread(&thread_zero)
            .await?
        else {
            return Err(initial_error);
        };
        if tip.gen_utime_ms < target_timed.gen_utime_ms {
            return Err(initial_error);
        }
        let start_height = self
            .thread_zero_lower_bound(target_timed.gen_utime_ms, tip.block.height)
            .await?;
        let mut globally_visited = HashSet::from([request.target]);
        let mut depth_reached = 0;
        let mut search = HistoricalSearchState::default();
        search.suffixes.insert(request.target, Vec::new());
        let mut examined_anchors = Vec::new();
        for (candidates, height) in (start_height..=tip.block.height).enumerate() {
            if candidates >= self.historical_search.max_anchor_candidates {
                if let Some((anchor, hops)) =
                    best_historical_anchor(&examined_anchors, &search.suffixes)
                {
                    return self
                        .resolved_path_from_anchor(request, &anchor, hops)
                        .await?
                        .ok_or_else(|| {
                            anyhow::anyhow!("historical path unexpectedly disappeared")
                        });
                }
                let version = self.store.version().await?;
                return Err(ResolutionError::HistoricalSearchLimit {
                    target: request.target,
                    graph_version: version.graph_version,
                    candidates,
                    max_anchor_candidates: self.historical_search.max_anchor_candidates,
                }
                .into());
            }
            let anchor = if height == tip.block.height {
                tip.block.clone()
            } else {
                self.provider
                    .timed_block_by_height(&thread_zero, height)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("thread 0 has no block at height {height}"))?
                    .block
            };
            validate_blocks(std::slice::from_ref(&anchor))?;
            self.store
                .apply(StoreBatch {
                    blocks: vec![anchor.clone()],
                    anchor_epoch: None,
                })
                .await?;
            examined_anchors.push(anchor.clone());
            if let Some(path) = self
                .incremental_path_from_anchor(
                    request,
                    Some(&target),
                    target_timed.gen_utime_ms,
                    anchor,
                    &mut globally_visited,
                    &mut depth_reached,
                    &mut search,
                )
                .await?
            {
                if request.policy == ResolutionPolicy::FirstValid {
                    return Ok(path);
                }
            }
        }
        if let Some((anchor, hops)) = best_historical_anchor(&examined_anchors, &search.suffixes) {
            self.resolved_path_from_anchor(request, &anchor, hops)
                .await?
                .ok_or_else(|| anyhow::anyhow!("historical path unexpectedly disappeared"))
        } else {
            Err(initial_error)
        }
    }

    /// Find the first thread-0 height whose generation time is not older than
    /// the target. Thread-0 generation time is monotonic with height; equal
    /// timestamps are deliberately kept on the left so same-second blocks are
    /// still examined.
    async fn thread_zero_lower_bound(
        &self,
        target_time: u64,
        tip_height: u64,
    ) -> anyhow::Result<u64> {
        let thread_zero = crate::ThreadId::ZERO;
        let mut low = 0u64;
        let mut high = tip_height;
        while low < high {
            let middle = low + (high - low) / 2;
            let block = self
                .provider
                .timed_block_by_height(&thread_zero, middle)
                .await?
                .ok_or_else(|| anyhow::anyhow!("thread 0 has no block at height {middle}"))?;
            if block.gen_utime_ms < target_time {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        Ok(low)
    }

    #[allow(clippy::too_many_arguments)]
    async fn incremental_path_from_anchor(
        &self,
        request: &ResolutionRequest,
        target_thread_entry: Option<&BlockNode>,
        target_time: u64,
        anchor: BlockNode,
        globally_visited: &mut HashSet<BlockId>,
        depth_reached: &mut u32,
        search: &mut HistoricalSearchState,
    ) -> anyhow::Result<Option<ResolvedPath>> {
        let _ = self
            .record_historical_visit(request, globally_visited, anchor.block_id, *depth_reached)
            .await?;
        if let Some(hops) = search.suffixes.get(&anchor.block_id).cloned() {
            return self.resolved_path_from_anchor(request, &anchor, hops).await;
        }

        let mut frontier = vec![anchor.clone()];
        let mut remaining = request.limits.max_hops;
        loop {
            let depth = request.limits.max_hops - remaining;
            *depth_reached = (*depth_reached).max(depth);
            frontier.sort_by_key(|block| block.block_id);

            for block in &frontier {
                if let Some(target) = target_thread_entry {
                    if search.terminal_checked.insert(block.block_id)
                        && block.thread_id == target.thread_id
                        && block.height >= target.height
                    {
                        if let Some(tail) = self
                            .parent_path_to_target(
                                request,
                                target,
                                block.clone(),
                                0,
                                globally_visited,
                                depth_reached,
                            )
                            .await?
                        {
                            for index in (0..tail.len()).rev() {
                                relax_historical_suffix(
                                    search,
                                    tail[index].from,
                                    tail[index..].to_vec(),
                                    request.limits.max_hops,
                                );
                            }
                            if tail.is_empty() {
                                relax_historical_suffix(
                                    search,
                                    block.block_id,
                                    tail,
                                    request.limits.max_hops,
                                );
                            }
                        }
                    }
                }
            }

            if request.policy == ResolutionPolicy::FirstValid {
                if let Some(hops) = search.suffixes.get(&anchor.block_id).cloned() {
                    return self.resolved_path_from_anchor(request, &anchor, hops).await;
                }
            }

            if remaining == 0 {
                break;
            }

            let mut next_ids = Vec::new();
            let mut first_seen_ids = HashSet::new();
            for block in &frontier {
                if search
                    .expanded_remaining
                    .get(&block.block_id)
                    .is_some_and(|expanded| *expanded >= remaining)
                {
                    continue;
                }
                search.expanded_remaining.insert(block.block_id, remaining);
                for (ref_index, to) in block.refs.iter().copied().enumerate() {
                    let ref_index = u32::try_from(ref_index)
                        .map_err(|_| anyhow::anyhow!("reference index exceeds u32"))?;
                    let edge = BlockEdge {
                        from: block.block_id,
                        to,
                        ref_index,
                    };
                    let dependents = search.dependents.entry(to).or_default();
                    if !dependents.contains(&edge) {
                        dependents.push(edge);
                    }
                    if let Some(child_suffix) = search.suffixes.get(&to) {
                        let mut candidate = Vec::with_capacity(child_suffix.len() + 1);
                        candidate.push(edge);
                        candidate.extend_from_slice(child_suffix);
                        relax_historical_suffix(
                            search,
                            block.block_id,
                            candidate,
                            request.limits.max_hops,
                        );
                    }
                    if self
                        .record_historical_visit(request, globally_visited, to, depth)
                        .await?
                    {
                        first_seen_ids.insert(to);
                    }
                    next_ids.push(to);
                }
            }
            next_ids.sort_unstable();
            next_ids.dedup();

            if request.policy == ResolutionPolicy::FirstValid {
                if let Some(hops) = search.suffixes.get(&anchor.block_id).cloned() {
                    return self.resolved_path_from_anchor(request, &anchor, hops).await;
                }
            }

            let stored = self.store.blocks(&next_ids).await?;
            let mut next_frontier = Vec::with_capacity(next_ids.len());
            let mut fetched = Vec::new();
            for id in next_ids {
                let next = if let Some(block) = stored.get(&id) {
                    Some(block.clone())
                } else if !first_seen_ids.contains(&id) {
                    // This id was already queried in this resolve and was
                    // either absent or older than the target.
                    None
                } else if let Some(timed) = self.provider.timed_block_by_id(&id).await? {
                    if timed.gen_utime_ms >= target_time {
                        fetched.push(timed.block.clone());
                        Some(timed.block)
                    } else {
                        None
                    }
                } else if let Some(block) = self.provider.block_by_id(&id).await? {
                    fetched.push(block.clone());
                    Some(block)
                } else {
                    None
                };
                if let Some(next) = next {
                    next_frontier.push(next);
                }
            }
            validate_blocks(&fetched)?;
            if !fetched.is_empty() {
                self.store
                    .apply(StoreBatch {
                        blocks: fetched,
                        anchor_epoch: None,
                    })
                    .await?;
            }
            if next_frontier.is_empty() {
                break;
            }
            frontier = next_frontier;
            remaining -= 1;
        }

        match search.suffixes.get(&anchor.block_id).cloned() {
            Some(hops) => self.resolved_path_from_anchor(request, &anchor, hops).await,
            None => Ok(None),
        }
    }

    async fn parent_path_to_target(
        &self,
        request: &ResolutionRequest,
        target: &BlockNode,
        mut current: BlockNode,
        prefix_hops: u32,
        globally_visited: &mut HashSet<BlockId>,
        depth_reached: &mut u32,
    ) -> anyhow::Result<Option<Vec<BlockEdge>>> {
        let mut path = Vec::new();
        let mut chain = HashSet::new();
        loop {
            if current.block_id == target.block_id {
                return Ok(Some(path));
            }
            if current.thread_id != target.thread_id || current.height < target.height {
                return Ok(None);
            }
            if !chain.insert(current.block_id) {
                return Ok(None);
            }
            if prefix_hops as usize + path.len() >= request.limits.max_hops as usize {
                return Ok(None);
            }
            let Some(parent_id) = current.refs.first().copied() else {
                return Ok(None);
            };
            path.push(BlockEdge {
                from: current.block_id,
                to: parent_id,
                ref_index: 0,
            });
            *depth_reached = (*depth_reached).max(prefix_hops + path.len() as u32);
            if parent_id == target.block_id {
                return Ok(Some(path));
            }
            let first_seen = self
                .record_historical_visit(request, globally_visited, parent_id, *depth_reached)
                .await?;
            let parent = match self.store.block(&parent_id).await? {
                Some(block) => block,
                None if !first_seen => return Ok(None),
                None => {
                    let Some(block) = self.provider.block_by_id(&parent_id).await? else {
                        return Ok(None);
                    };
                    validate_blocks(std::slice::from_ref(&block))?;
                    self.store
                        .apply(StoreBatch {
                            blocks: vec![block.clone()],
                            anchor_epoch: None,
                        })
                        .await?;
                    block
                },
            };
            if parent.thread_id != target.thread_id || parent.height >= current.height {
                return Ok(None);
            }
            current = parent;
        }
    }

    async fn record_historical_visit(
        &self,
        request: &ResolutionRequest,
        globally_visited: &mut HashSet<BlockId>,
        block_id: BlockId,
        depth_reached: u32,
    ) -> anyhow::Result<bool> {
        if globally_visited.contains(&block_id) {
            return Ok(false);
        }
        if globally_visited.len() >= request.limits.max_visited_blocks {
            let version = self.store.version().await?;
            return Err(ResolutionError::MaxVisited {
                target: request.target,
                graph_version: version.graph_version,
                visited: globally_visited.len(),
                depth_reached,
                max_hops: request.limits.max_hops,
                max_visited_blocks: request.limits.max_visited_blocks,
            }
            .into());
        }
        globally_visited.insert(block_id);
        Ok(true)
    }

    async fn resolved_path_from_anchor(
        &self,
        request: &ResolutionRequest,
        anchor: &BlockNode,
        hops: Vec<BlockEdge>,
    ) -> anyhow::Result<Option<ResolvedPath>> {
        let version = self.store.version().await?;
        Ok(Some(ResolvedPath {
            anchor: anchor.block_id,
            anchor_height: anchor.height,
            target: request.target,
            hops,
            graph_version: version.graph_version,
            anchor_epoch: version.anchor_epoch,
        }))
    }

    async fn bfs(
        &self,
        request: &ResolutionRequest,
        target: BlockNode,
        graph_version: u64,
        anchor_epoch: u64,
    ) -> anyhow::Result<ResolvedPath> {
        if target.thread_id.is_zero() {
            return Ok(ResolvedPath {
                anchor: target.block_id,
                anchor_height: target.height,
                target: target.block_id,
                hops: vec![],
                graph_version,
                anchor_epoch,
            });
        }

        let max_visited = request.limits.max_visited_blocks;
        if max_visited == 0 {
            return Err(ResolutionError::MaxVisited {
                target: request.target,
                graph_version,
                visited: 0,
                depth_reached: 0,
                max_hops: request.limits.max_hops,
                max_visited_blocks: max_visited,
            }
            .into());
        }

        let mut visited = HashSet::from([request.target]);
        let mut frontier = vec![request.target];
        let mut toward_target: HashMap<BlockId, BlockEdge> = HashMap::new();
        let mut depth_reached = 0;

        for depth in 1..=request.limits.max_hops {
            frontier.sort_unstable();
            let incoming = self.store.incoming_edges(&frontier).await?;
            let mut candidates = Vec::new();
            for older in &frontier {
                if let Some(edges) = incoming.get(older) {
                    candidates.extend(edges.iter().copied());
                }
            }
            candidates.sort_unstable();
            let mut next = Vec::new();
            for edge in candidates {
                if visited.contains(&edge.from) {
                    continue;
                }
                if visited.len() >= max_visited {
                    return Err(ResolutionError::MaxVisited {
                        target: request.target,
                        graph_version,
                        visited: visited.len(),
                        depth_reached,
                        max_hops: request.limits.max_hops,
                        max_visited_blocks: max_visited,
                    }
                    .into());
                }
                visited.insert(edge.from);
                toward_target.insert(edge.from, edge);
                next.push(edge.from);
            }
            next.sort_unstable();
            next.dedup();
            depth_reached = depth;
            if next.is_empty() {
                break;
            }
            let nodes = self.store.blocks(&next).await?;
            let anchor = nodes
                .values()
                .filter(|node| node.thread_id.is_zero())
                .max_by(|a, b| {
                    a.height
                        .cmp(&b.height)
                        .then_with(|| b.block_id.cmp(&a.block_id))
                })
                .cloned();
            if let Some(anchor) = anchor {
                let mut hops = Vec::with_capacity(depth as usize);
                let mut current = anchor.block_id;
                while current != request.target {
                    let edge = *toward_target.get(&current).ok_or_else(|| {
                        anyhow::anyhow!("internal path reconstruction gap at {current}")
                    })?;
                    anyhow::ensure!(edge.from == current, "internal path edge origin mismatch");
                    hops.push(edge);
                    current = edge.to;
                }
                anyhow::ensure!(
                    hops.len() == depth as usize,
                    "internal path length mismatch"
                );
                for pair in hops.windows(2) {
                    anyhow::ensure!(
                        pair[0].to == pair[1].from,
                        "internal path continuity mismatch"
                    );
                }
                anyhow::ensure!(
                    hops.first().map(|e| e.from) == Some(anchor.block_id),
                    "internal anchor mismatch"
                );
                anyhow::ensure!(
                    hops.last().map(|e| e.to) == Some(request.target),
                    "internal target mismatch"
                );
                return Ok(ResolvedPath {
                    anchor: anchor.block_id,
                    anchor_height: anchor.height,
                    target: request.target,
                    hops,
                    graph_version,
                    anchor_epoch,
                });
            }
            frontier = next;
        }

        Err(ResolutionError::NoPath {
            target: request.target,
            graph_version,
            visited: visited.len(),
            depth_reached,
            max_hops: request.limits.max_hops,
            max_visited_blocks: max_visited,
        }
        .into())
    }
}

fn best_historical_anchor(
    anchors: &[BlockNode],
    suffixes: &HashMap<BlockId, Vec<BlockEdge>>,
) -> Option<(BlockNode, Vec<BlockEdge>)> {
    let mut best: Option<(BlockNode, Vec<BlockEdge>)> = None;
    for anchor in anchors {
        let Some(hops) = suffixes.get(&anchor.block_id) else {
            continue;
        };
        let replace = best.as_ref().is_none_or(|(current_anchor, current_hops)| {
            hops.len() < current_hops.len()
                || (hops.len() == current_hops.len()
                    && (anchor.height > current_anchor.height
                        || (anchor.height == current_anchor.height
                            && anchor.block_id < current_anchor.block_id)))
        });
        if replace {
            best = Some((anchor.clone(), hops.clone()));
        }
    }
    best
}

fn relax_historical_suffix(
    search: &mut HistoricalSearchState,
    start: BlockId,
    suffix: Vec<BlockEdge>,
    max_hops: u32,
) {
    let mut pending = VecDeque::from([(start, suffix)]);
    while let Some((block_id, candidate)) = pending.pop_front() {
        if candidate.len() > max_hops as usize
            || search
                .suffixes
                .get(&block_id)
                .is_some_and(|current| current.len() <= candidate.len())
        {
            continue;
        }
        search.suffixes.insert(block_id, candidate.clone());
        for edge in search
            .dependents
            .get(&block_id)
            .into_iter()
            .flatten()
            .copied()
        {
            let mut parent_candidate = Vec::with_capacity(candidate.len() + 1);
            parent_candidate.push(edge);
            parent_candidate.extend_from_slice(&candidate);
            pending.push_back((edge.from, parent_candidate));
        }
    }
}

fn validate_blocks(blocks: &[BlockNode]) -> anyhow::Result<()> {
    for block in blocks {
        let mut refs = HashSet::new();
        for (index, id) in block.refs.iter().enumerate() {
            anyhow::ensure!(
                refs.insert(*id),
                "block {} repeats reference {} at slot {}",
                block.block_id,
                id,
                index
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::{MemoryStore, ResolverLimits, ThreadId};

    #[derive(Default)]
    struct FakeProvider {
        blocks: Mutex<HashMap<BlockId, BlockNode>>,
        proofs: Mutex<HashMap<BlockId, crate::ProofBlock>>,
        height_queries: Mutex<Vec<u64>>,
        historical: Mutex<bool>,
    }

    impl FakeProvider {
        fn replace(&self, blocks: Vec<BlockNode>) {
            *self.blocks.lock().unwrap() = blocks.into_iter().map(|b| (b.block_id, b)).collect();
        }

        fn enable_historical(&self) {
            *self.historical.lock().unwrap() = true;
        }

        fn set_proofs(&self, proofs: Vec<crate::ProofBlock>) {
            *self.proofs.lock().unwrap() = proofs
                .into_iter()
                .map(|proof| (proof.block.block_id, proof))
                .collect();
        }
    }

    #[async_trait]
    impl BlockProvider for FakeProvider {
        async fn block_by_id(&self, id: &BlockId) -> anyhow::Result<Option<BlockNode>> {
            Ok(self.blocks.lock().unwrap().get(id).cloned())
        }

        async fn latest_blocks(&self, limit: usize) -> anyhow::Result<Vec<BlockNode>> {
            let mut blocks: Vec<_> = self.blocks.lock().unwrap().values().cloned().collect();
            blocks.sort_by_key(|b| (b.height, b.block_id));
            blocks.reverse();
            blocks.truncate(limit);
            Ok(blocks)
        }

        async fn timed_block_by_id(
            &self,
            id: &BlockId,
        ) -> anyhow::Result<Option<crate::TimedBlock>> {
            if !*self.historical.lock().unwrap() {
                return Ok(None);
            }
            Ok(self
                .blocks
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .map(|block| crate::TimedBlock {
                    gen_utime_ms: block.height * 10,
                    block,
                }))
        }

        async fn timed_block_by_height(
            &self,
            thread: &ThreadId,
            height: u64,
        ) -> anyhow::Result<Option<crate::TimedBlock>> {
            self.height_queries.lock().unwrap().push(height);
            Ok(self
                .blocks
                .lock()
                .unwrap()
                .values()
                .find(|block| block.thread_id == *thread && block.height == height)
                .cloned()
                .map(|block| crate::TimedBlock {
                    gen_utime_ms: block.height * 10,
                    block,
                }))
        }

        async fn latest_timed_block_in_thread(
            &self,
            thread: &ThreadId,
        ) -> anyhow::Result<Option<crate::TimedBlock>> {
            Ok(self
                .blocks
                .lock()
                .unwrap()
                .values()
                .filter(|block| block.thread_id == *thread)
                .max_by_key(|block| block.height)
                .cloned()
                .map(|block| crate::TimedBlock {
                    gen_utime_ms: block.height * 10,
                    block,
                }))
        }

        async fn proof_block_by_id(
            &self,
            id: &BlockId,
        ) -> anyhow::Result<Option<crate::ProofBlock>> {
            if let Some(proof) = self.proofs.lock().unwrap().get(id).cloned() {
                return Ok(Some(proof));
            }
            Ok(self
                .blocks
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .map(|block| crate::ProofBlock {
                    gen_utime_ms: block.height * 10,
                    envelope_hash: [block.height as u8; 32],
                    tracked_ext_out_messages_root: [block.refs.len() as u8; 32],
                    history_proofs: Default::default(),
                    block_merkle_tree_leaves: [[0u8; 32]; 16],
                    block,
                }))
        }

        async fn proof_block_by_height(
            &self,
            thread: &ThreadId,
            height: u64,
        ) -> anyhow::Result<Option<crate::ProofBlock>> {
            let id = self
                .blocks
                .lock()
                .unwrap()
                .values()
                .find(|block| block.thread_id == *thread && block.height == height)
                .map(|block| block.block_id);
            match id {
                Some(id) => self.proof_block_by_id(&id).await,
                None => Ok(None),
            }
        }

        async fn anchor_history_witness(
            &self,
            _anchor: &BlockId,
            snapshot: &crate::AnchorSnapshot,
            mode: crate::AnchorLayerMode,
        ) -> anyhow::Result<Option<crate::AnchorHistoryWitness>> {
            let layer = match mode {
                crate::AnchorLayerMode::Auto => snapshot
                    .layers
                    .iter()
                    .position(|slots| !slots.is_empty())
                    .map(|index| index as u8 + 1),
                crate::AnchorLayerMode::Explicit(layer) => Some(layer),
            };
            let Some(layer) = layer else {
                return Ok(None);
            };
            let Some(slot) = snapshot
                .layers
                .get(layer as usize - 1)
                .and_then(|slots| slots.first())
            else {
                return Ok(None);
            };
            Ok(Some(crate::AnchorHistoryWitness {
                anchor_epoch: snapshot.epoch,
                layer,
                final_root: slot.root,
                anchor_key_block_height: slot.height,
                block_leaf: [1; 32],
                block_tree: crate::DenseOpening {
                    leaf: [1; 32],
                    position: 0,
                    siblings: vec![],
                },
                dense_chain: vec![],
            }))
        }

        async fn hop_opening(
            &self,
            _block: &crate::ProofBlock,
            edge: BlockEdge,
        ) -> anyhow::Result<Option<crate::HopOpening>> {
            Ok(Some(crate::HopOpening {
                edge,
                block_merkle_leaf_proof_l7: [[0; 32]; 4],
                refs_tree_depth: 0,
                proof_block_ref_inner_path: vec![],
            }))
        }

        fn namespace(&self) -> &str {
            "fake"
        }
    }

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
    fn request(target: u8, policy: ResolutionPolicy) -> ResolutionRequest {
        ResolutionRequest {
            target: id(target),
            policy,
            limits: ResolverLimits {
                max_hops: 20,
                max_visited_blocks: 100,
            },
        }
    }
    async fn setup(
        blocks: Vec<BlockNode>,
    ) -> (
        Arc<FakeProvider>,
        Arc<MemoryStore>,
        GraphResolver<FakeProvider, MemoryStore>,
    ) {
        let provider = Arc::new(FakeProvider::default());
        provider.replace(blocks);
        let store = Arc::new(MemoryStore::new());
        let resolver = GraphResolver::new(provider.clone(), store.clone(), 0);
        resolver.sync_latest(100).await.unwrap();
        (provider, store, resolver)
    }

    #[tokio::test]
    async fn target_on_thread_zero_is_zero_hop() {
        let (_, _, resolver) = setup(vec![block(1, 0, 7, &[])]).await;
        let path = resolver
            .resolve(request(1, ResolutionPolicy::ShortestCurrent))
            .await
            .unwrap();
        assert_eq!(
            (path.anchor, path.target, path.hops.len()),
            (id(1), id(1), 0)
        );
    }

    #[tokio::test]
    async fn resolves_direct_ref_and_serializes_ref_index() {
        let (_, _, resolver) = setup(vec![block(1, 1, 1, &[]), block(2, 0, 9, &[9, 1])]).await;
        let path = resolver
            .resolve(request(1, ResolutionPolicy::ShortestCurrent))
            .await
            .unwrap();
        assert_eq!(path.hops, vec![BlockEdge {
            from: id(2),
            to: id(1),
            ref_index: 1
        }]);
        assert_eq!(
            serde_json::to_value(path).unwrap()["hops"][0]["ref_index"],
            1
        );
    }

    #[tokio::test]
    async fn resolves_cross_ref_then_parent_hops_and_third_thread() {
        let (_, _, resolver) = setup(vec![
            block(1, 1, 1, &[]),
            block(2, 1, 2, &[1]),
            block(3, 2, 3, &[2]),
            block(4, 0, 10, &[8, 3]),
        ])
        .await;
        let path = resolver
            .resolve(request(1, ResolutionPolicy::ShortestCurrent))
            .await
            .unwrap();
        assert_eq!(
            path.hops.iter().map(|e| e.ref_index).collect::<Vec<_>>(),
            vec![1, 0, 0]
        );
        assert_eq!(path.hops.iter().map(|e| e.from).collect::<Vec<_>>(), vec![
            id(4),
            id(3),
            id(2)
        ]);
    }

    #[tokio::test]
    async fn resolve_proof_hydrates_every_hop_and_preserves_parent_slots() {
        let (_, _, resolver) = setup(vec![
            block(1, 1, 1, &[]),
            block(2, 1, 2, &[1]),
            block(3, 2, 3, &[2]),
            block(4, 0, 10, &[8, 3]),
        ])
        .await;
        let proof = resolver
            .resolve_proof(request(1, ResolutionPolicy::ShortestCurrent))
            .await
            .unwrap();

        assert_eq!(proof.anchor_block.block.block_id, id(4));
        assert_eq!(proof.target_block.block.block_id, id(1));
        assert_eq!(
            proof
                .path
                .hops
                .iter()
                .map(|edge| edge.ref_index)
                .collect::<Vec<_>>(),
            vec![1, 0, 0]
        );
        assert_eq!(
            proof
                .hop_blocks
                .iter()
                .map(|block| block.block.block_id)
                .collect::<Vec<_>>(),
            vec![id(4), id(3), id(2)]
        );
        for (edge, source) in proof.path.hops.iter().zip(&proof.hop_blocks) {
            assert_eq!(
                source.block.refs[edge.ref_index as usize], edge.to,
                "hydrated proof source must commit the selected edge"
            );
        }
    }

    #[tokio::test]
    async fn resolves_complete_anchored_parent_proof() {
        let target_id = id(9);
        let anchor_id = id(2);
        let target = BlockNode {
            block_id: target_id,
            thread_id: thread(1),
            height: 0,
            refs: vec![],
        };
        let zero0 = block(1, 0, 0, &[]);
        let anchor = block(2, 0, 1, &[9]);
        let key = block(3, 0, 2, &[]);
        let (provider, _, resolver) = setup(vec![
            target.clone(),
            zero0.clone(),
            anchor.clone(),
            key.clone(),
        ])
        .await;
        provider.enable_historical();

        let proof = |block: BlockNode, leaves| crate::ProofBlock {
            gen_utime_ms: block.height * 10,
            envelope_hash: [block.height as u8 + 1; 32],
            tracked_ext_out_messages_root: [block.height as u8 + 11; 32],
            history_proofs: Default::default(),
            block_merkle_tree_leaves: leaves,
            block,
        };
        let target_proof = proof(target, [[0; 32]; 16]);
        let zero0_proof = proof(zero0, [[0; 32]; 16]);
        let anchor_proof = proof(anchor, [[0; 32]; 16]);
        let key_proof = proof(key, [[0; 32]; 16]);
        provider.set_proofs(vec![target_proof, zero0_proof, anchor_proof, key_proof]);

        let result = resolver
            .resolve_anchored_proof(crate::AnchoredResolutionRequest {
                resolution: request(9, ResolutionPolicy::FirstValid),
                anchor_snapshot: crate::AnchorSnapshot {
                    epoch: 1,
                    window_size: 2,
                    thinning_factor: 1,
                    layers: vec![vec![crate::AnchorSlot {
                        root: [42; 32],
                        height: 2,
                    }]],
                },
                anchor_layer: crate::AnchorLayerMode::Auto,
            })
            .await
            .unwrap();
        assert_eq!(result.route.path.anchor, anchor_id);
        assert_eq!(result.hop_openings[0].edge.ref_index, 0);
        assert_eq!(result.history.final_root, [42; 32]);
    }

    #[tokio::test]
    async fn chooses_shortest_then_highest_anchor_then_lexicographically_smallest_id() {
        let (_, _, resolver) = setup(vec![
            block(1, 1, 1, &[]),
            block(2, 2, 2, &[1]),
            block(3, 0, 20, &[2]),
            block(4, 0, 10, &[1]),
            block(5, 0, 10, &[1]),
        ])
        .await;
        let path = resolver
            .resolve(request(1, ResolutionPolicy::ShortestCurrent))
            .await
            .unwrap();
        assert_eq!(
            path.anchor,
            id(4),
            "direct route wins; lower id breaks equal-height tie"
        );
    }

    #[tokio::test]
    async fn cycles_and_duplicate_discovery_do_not_repeat_nodes() {
        let (_, _, resolver) = setup(vec![
            block(1, 1, 1, &[2]),
            block(2, 2, 2, &[1]),
            block(3, 0, 3, &[1, 2]),
        ])
        .await;
        let path = resolver
            .resolve(request(1, ResolutionPolicy::ShortestCurrent))
            .await
            .unwrap();
        assert_eq!(path.hops.len(), 1);
    }

    #[tokio::test]
    async fn reports_no_path_and_limits() {
        let (_, _, resolver) = setup(vec![
            block(1, 1, 1, &[]),
            block(2, 2, 2, &[1]),
            block(3, 0, 3, &[2]),
        ])
        .await;
        let mut no_hops = request(1, ResolutionPolicy::ShortestCurrent);
        no_hops.limits.max_hops = 1;
        let error = resolver.resolve(no_hops).await.unwrap_err().to_string();
        assert!(error.contains("no path"));
        assert!(error.contains("depth_reached=1"));

        let mut visited = request(1, ResolutionPolicy::ShortestCurrent);
        visited.limits.max_visited_blocks = 1;
        assert!(resolver
            .resolve(visited)
            .await
            .unwrap_err()
            .to_string()
            .contains("visited-block limit"));

        let (_, _, isolated) = setup(vec![block(9, 1, 1, &[])]).await;
        assert!(isolated
            .resolve(request(9, ResolutionPolicy::ShortestCurrent))
            .await
            .unwrap_err()
            .to_string()
            .contains("no path"));
    }

    #[tokio::test]
    async fn cache_policy_reacts_correctly_to_graph_growth() {
        let initial = vec![
            block(1, 1, 1, &[]),
            block(2, 2, 2, &[1]),
            block(3, 0, 3, &[2]),
        ];
        let (provider, _, resolver) = setup(initial.clone()).await;
        let old_first = resolver
            .resolve(request(1, ResolutionPolicy::FirstValid))
            .await
            .unwrap();
        let old_shortest = resolver
            .resolve(request(1, ResolutionPolicy::ShortestCurrent))
            .await
            .unwrap();
        assert_eq!(old_shortest.hops.len(), 2);

        let mut grown = initial;
        grown.push(block(4, 0, 4, &[1]));
        provider.replace(grown);
        resolver.sync_latest(100).await.unwrap();

        let cached_first = resolver
            .resolve(request(1, ResolutionPolicy::FirstValid))
            .await
            .unwrap();
        let fresh_shortest = resolver
            .resolve(request(1, ResolutionPolicy::ShortestCurrent))
            .await
            .unwrap();
        assert_eq!(cached_first, old_first);
        assert_eq!(
            (fresh_shortest.anchor, fresh_shortest.hops.len()),
            (id(4), 1)
        );
        assert!(fresh_shortest.graph_version > old_shortest.graph_version);
    }

    #[tokio::test]
    async fn sync_rejects_duplicate_reference_slots() {
        let provider = Arc::new(FakeProvider::default());
        provider.replace(vec![block(1, 1, 1, &[2, 2])]);
        let resolver = GraphResolver::new(provider, Arc::new(MemoryStore::new()), 0);
        assert!(resolver
            .sync_latest(10)
            .await
            .unwrap_err()
            .to_string()
            .contains("repeats reference"));
    }

    #[tokio::test]
    async fn cold_historical_target_uses_height_binary_search_then_walks_right() {
        let provider = Arc::new(FakeProvider::default());
        provider.enable_historical();
        let mut blocks = vec![block(1, 1, 15, &[]), block(2, 2, 16, &[1])];
        for height in 0u8..=31 {
            let refs = if height == 17 { vec![2] } else { vec![] };
            blocks.push(block(100 + height, 0, u64::from(height), &refs));
        }
        provider.replace(blocks);
        let store = Arc::new(MemoryStore::new());
        let resolver = GraphResolver::new(provider.clone(), store, 0);
        let path = resolver
            .resolve(request(1, ResolutionPolicy::FirstValid))
            .await
            .unwrap();

        assert_eq!(path.anchor, id(117));
        assert_eq!(path.anchor_height, 17);
        assert_eq!(path.hops.len(), 2);
        let queries = provider.height_queries.lock().unwrap();
        assert!(
            queries.len() <= 8,
            "expected bounded lookup, got {queries:?}"
        );
        assert!(queries.contains(&15));
    }

    #[tokio::test]
    async fn historical_fallback_enters_target_thread_then_follows_parents() {
        let provider = Arc::new(FakeProvider::default());
        provider.enable_historical();
        let mut blocks = vec![block(1, 1, 15, &[]), block(2, 1, 16, &[1])];
        for height in 0u8..=31 {
            let refs = if height == 17 { vec![116, 2] } else { vec![] };
            blocks.push(block(100 + height, 0, u64::from(height), &refs));
        }
        provider.replace(blocks);
        let resolver = GraphResolver::new(provider.clone(), Arc::new(MemoryStore::new()), 0);
        let path = resolver
            .resolve(request(1, ResolutionPolicy::FirstValid))
            .await
            .unwrap();

        assert_eq!(path.anchor, id(117));
        assert_eq!(path.anchor_height, 17);
        assert_eq!(path.hops, vec![
            BlockEdge {
                from: id(117),
                to: id(2),
                ref_index: 1,
            },
            BlockEdge {
                from: id(2),
                to: id(1),
                ref_index: 0,
            },
        ]);
        assert!(provider.height_queries.lock().unwrap().contains(&15));
    }
}
