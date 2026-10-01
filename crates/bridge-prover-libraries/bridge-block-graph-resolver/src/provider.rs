use async_trait::async_trait;

use crate::{BlockId, BlockNode, ThreadId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimedBlock {
    pub block: BlockNode,
    pub gen_utime_ms: u64,
}

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

    /// Stable network/data-source identity used to isolate cache entries.
    fn namespace(&self) -> &str;
}
