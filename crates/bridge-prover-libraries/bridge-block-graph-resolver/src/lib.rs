//! Stateful reverse-graph resolution from an event block to a newer thread-0
//! anchor.

pub mod memory;
pub mod model;
pub mod provider;
#[cfg(feature = "graphql")]
pub mod providers;
pub mod resolver;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod store;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_generator;

pub use memory::MemoryStore;
pub use model::{BlockEdge, BlockId, BlockNode, ThreadId};
pub use provider::{BlockProvider, TimedBlock};
#[cfg(feature = "graphql")]
pub use providers::graphql::GraphqlBlockProvider;
pub use resolver::{GraphResolver, HistoricalSearchConfig, ResolutionAlgorithm, ResolutionError};
#[cfg(feature = "server")]
pub use server::{router as http_router, ResolverApi, ServiceStatus};
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteStore;
pub use store::{
    ApplyStats, PathCacheKey, PruneStats, ResolutionPolicy, ResolutionRequest, ResolvedPath,
    ResolverLimits, ResolverStore, StoreBatch, StoreVersion, SyncStats,
};
