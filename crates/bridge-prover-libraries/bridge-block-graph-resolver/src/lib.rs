//! Stateful reverse-graph resolution from an event block to a newer thread-0
//! anchor.

pub mod model;
pub mod provider;
#[cfg(feature = "graphql")]
pub mod providers;
pub mod resolver;
pub mod store;
pub mod stores;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_generator;

pub use model::{
    ApplyStats, BlockEdge, BlockId, BlockNode, PathCacheKey, ProofBlock, PruneStats,
    ResolutionPolicy, ResolutionRequest, ResolvedBlockProof, ResolvedPath, ResolverLimits,
    StoreBatch, StoreVersion, SyncStats, ThreadId, TimedBlock,
};
pub use provider::BlockProvider;
#[cfg(feature = "graphql")]
pub use providers::graphql::GraphqlBlockProvider;
pub use resolver::{GraphResolver, HistoricalSearchConfig, ResolutionError};
pub use store::ResolverStore;
pub use stores::MemoryStore;
#[cfg(feature = "sqlite")]
pub use stores::SqliteStore;
