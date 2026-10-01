use async_trait::async_trait;

use crate::{
    AnchorHistoryWitness, AnchorLayerMode, AnchorSnapshot, BlockEdge, BlockId, BlockNode,
    HopOpening, ProofBlock, ThreadId, TimedBlock,
};

#[async_trait]
pub trait BlockProvider: Send + Sync + 'static {
    /// Return a finalized block, or `None` when it is unknown.
    async fn block_by_id(&self, id: &BlockId) -> anyhow::Result<Option<BlockNode>>;

    /// Return a rolling window containing finalized blocks only.
    async fn latest_blocks(&self, limit: usize) -> anyhow::Result<Vec<BlockNode>>;

    /// Return a block and its generation time when historical height lookup
    /// is supported. `None` also covers legacy rows without generation time.
    async fn timed_block_by_id(&self, _id: &BlockId) -> anyhow::Result<Option<TimedBlock>> {
        Ok(None)
    }

    /// Return the block at an exact `(thread, height)` with generation time.
    async fn timed_block_by_height(
        &self,
        _thread: &ThreadId,
        _height: u64,
    ) -> anyhow::Result<Option<TimedBlock>> {
        Ok(None)
    }

    /// Return the highest available block in `thread` with generation time.
    async fn latest_timed_block_in_thread(
        &self,
        _thread: &ThreadId,
    ) -> anyhow::Result<Option<TimedBlock>> {
        Ok(None)
    }

    /// Return complete circuit-witness material for a finalized block.
    /// Providers that only support topology may leave the default in place;
    /// [`crate::GraphResolver::resolve_proof`] then fails explicitly.
    async fn proof_block_by_id(&self, _id: &BlockId) -> anyhow::Result<Option<ProofBlock>> {
        Ok(None)
    }

    async fn proof_block_by_height(
        &self,
        _thread: &ThreadId,
        _height: u64,
    ) -> anyhow::Result<Option<ProofBlock>> {
        Ok(None)
    }

    /// Return a ready history witness connecting `anchor` to an active root
    /// from `snapshot`. Building and cryptographically validating this witness
    /// belongs to the provider/prover boundary; the graph resolver never runs
    /// Poseidon itself.
    async fn anchor_history_witness(
        &self,
        _anchor: &BlockId,
        _snapshot: &AnchorSnapshot,
        _mode: AnchorLayerMode,
    ) -> anyhow::Result<Option<AnchorHistoryWitness>> {
        Ok(None)
    }

    /// Return a ready circuit opening for an edge selected by the resolver.
    /// The provider must use the parent domain for slot zero and the
    /// cross-reference domain for every other slot.
    async fn hop_opening(
        &self,
        _block: &ProofBlock,
        _edge: BlockEdge,
    ) -> anyhow::Result<Option<HopOpening>> {
        Ok(None)
    }

    /// Stable network/data-source identity used to isolate cache entries.
    fn namespace(&self) -> &str;
}
