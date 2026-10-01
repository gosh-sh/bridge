//! Stateful reverse-graph resolution from an event block to a newer thread-0
//! anchor.

pub mod model;
pub mod provider;
#[cfg(feature = "graphql")]
pub mod providers;
pub mod resolver;
#[cfg(feature = "server")]
pub mod server;
pub mod store;
pub mod stores;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_generator;

pub use model::{
    ApplyStats, BlockEdge, BlockId, BlockNode, ErrorBody, PathCacheKey, ProofBlock, PruneStats,
    ResolutionPolicy, ResolutionRequest, ResolvedBlockProof, ResolvedPath, ResolverLimits,
    ServiceStatus, StoreBatch, StoreVersion, SyncStats, ThreadId, TimedBlock,
};
pub use provider::BlockProvider;
#[cfg(feature = "graphql")]
pub use providers::graphql::GraphqlBlockProvider;
pub use resolver::{GraphResolver, HistoricalSearchConfig, ResolutionError};
#[cfg(feature = "server")]
pub use server::{router as http_router, ResolverApi};
pub use store::ResolverStore;
pub use stores::MemoryStore;
#[cfg(feature = "sqlite")]
pub use stores::SqliteStore;
