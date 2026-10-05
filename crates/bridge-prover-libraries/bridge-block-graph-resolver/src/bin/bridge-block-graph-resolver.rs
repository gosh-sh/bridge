use std::{path::PathBuf, str::FromStr, sync::Arc};

use anyhow::Context;
use bridge_block_graph_resolver::{
    BlockId, BlockProvider, GraphResolver, GraphqlBlockProvider, HistoricalSearchConfig,
    MemoryStore, ResolutionPolicy, ResolutionRequest, ResolverLimits, ResolverStore, SqliteStore,
    SyncStats,
};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(about = "Resolve finalized Acki Nacki block-reference paths to thread 0")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Load a rolling graph window and resolve an anchor-to-target path.
    Resolve {
        #[arg(long)]
        gql_url: String,
        #[arg(long, value_parser = parse_block_id)]
        block_id: BlockId,
        #[arg(long, default_value_t = 1000)]
        scan_window: usize,
        #[arg(long, default_value_t = 1000)]
        per_thread_window: usize,
        /// Persist graph/cache state here; omit for an in-memory one-shot run.
        #[arg(long)]
        database: Option<PathBuf>,
        #[arg(long, default_value_t = 300)]
        max_hops: u32,
        #[arg(long, default_value_t = 100_000)]
        max_visited_blocks: usize,
        /// Maximum thread-0 blocks examined after the timestamp lower bound.
        #[arg(long, default_value_t = 1000)]
        max_anchor_candidates: usize,
        #[arg(long, value_enum, default_value_t = CliPolicy::ShortestCurrent)]
        policy: CliPolicy,
        #[arg(long)]
        pretty: bool,
        /// Include canonical proof material for the anchor, target and every
        /// hop source block.
        #[arg(long)]
        proof_material: bool,
    },
    /// Load and validate a rolling graph window.
    Sync {
        #[arg(long)]
        gql_url: String,
        #[arg(long, default_value_t = 1000)]
        scan_window: usize,
        #[arg(long, default_value_t = 1000)]
        per_thread_window: usize,
        /// Persist graph/cache state here; omit for an in-memory diagnostic
        /// run.
        #[arg(long)]
        database: Option<PathBuf>,
        #[arg(long)]
        pretty: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliPolicy {
    FirstValid,
    ShortestCurrent,
}

impl From<CliPolicy> for ResolutionPolicy {
    fn from(value: CliPolicy) -> Self {
        match value {
            CliPolicy::FirstValid => Self::FirstValid,
            CliPolicy::ShortestCurrent => Self::ShortestCurrent,
        }
    }
}

fn parse_block_id(value: &str) -> Result<BlockId, String> {
    BlockId::from_str(value).map_err(|error| error.to_string())
}

fn print_json(value: &impl Serialize, pretty: bool) -> anyhow::Result<()> {
    let output = if pretty {
        serde_json::to_string_pretty(value)?
    } else {
        serde_json::to_string(value)?
    };
    println!("{output}");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn resolve_with_store<S: ResolverStore>(
    provider: Arc<GraphqlBlockProvider>,
    store: Arc<S>,
    per_thread_window: usize,
    scan_window: usize,
    max_anchor_candidates: usize,
    request: ResolutionRequest,
    proof_material: bool,
) -> anyhow::Result<serde_json::Value> {
    let resolver = GraphResolver::new(provider, store, per_thread_window)
        .with_historical_search_config(HistoricalSearchConfig {
            max_anchor_candidates,
        });
    resolver
        .sync_latest(scan_window)
        .await
        .context("sync graph window")?;
    if proof_material {
        serde_json::to_value(
            resolver
                .resolve_proof(request)
                .await
                .context("resolve block proof material")?,
        )
        .context("serialize resolved block proof")
    } else {
        serde_json::to_value(
            resolver
                .resolve(request)
                .await
                .context("resolve block path")?,
        )
        .context("serialize resolved block path")
    }
}

async fn sync_with_store<S: ResolverStore>(
    provider: Arc<GraphqlBlockProvider>,
    store: Arc<S>,
    per_thread_window: usize,
    scan_window: usize,
) -> anyhow::Result<SyncStats> {
    GraphResolver::new(provider, store, per_thread_window)
        .sync_latest(scan_window)
        .await
        .context("sync graph window")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Resolve {
            gql_url,
            block_id,
            scan_window,
            per_thread_window,
            database,
            max_hops,
            max_visited_blocks,
            max_anchor_candidates,
            policy,
            pretty,
            proof_material,
        } => {
            let provider =
                Arc::new(GraphqlBlockProvider::new(&gql_url).context("create GraphQL provider")?);
            let request = ResolutionRequest {
                target: block_id,
                policy: policy.into(),
                limits: ResolverLimits {
                    max_hops,
                    max_visited_blocks,
                },
            };
            let path = if let Some(database) = database {
                let store = Arc::new(
                    SqliteStore::open(database, provider.namespace())
                        .await
                        .context("open resolver database")?,
                );
                resolve_with_store(
                    provider,
                    store,
                    per_thread_window,
                    scan_window,
                    max_anchor_candidates,
                    request,
                    proof_material,
                )
                .await?
            } else {
                resolve_with_store(
                    provider,
                    Arc::new(MemoryStore::new()),
                    per_thread_window,
                    scan_window,
                    max_anchor_candidates,
                    request,
                    proof_material,
                )
                .await?
            };
            print_json(&path, pretty)?;
        },
        Command::Sync {
            gql_url,
            scan_window,
            per_thread_window,
            database,
            pretty,
        } => {
            let provider =
                Arc::new(GraphqlBlockProvider::new(&gql_url).context("create GraphQL provider")?);
            let stats = if let Some(database) = database {
                let store = Arc::new(
                    SqliteStore::open(database, provider.namespace())
                        .await
                        .context("open resolver database")?,
                );
                sync_with_store(provider, store, per_thread_window, scan_window).await?
            } else {
                sync_with_store(
                    provider,
                    Arc::new(MemoryStore::new()),
                    per_thread_window,
                    scan_window,
                )
                .await?
            };
            print_json(&stats, pretty)?;
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn help_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn invalid_id_is_rejected_during_argument_parsing() {
        let error = Cli::try_parse_from([
            "resolver",
            "resolve",
            "--gql-url",
            "unused",
            "--block-id",
            "bad",
        ])
        .unwrap_err();
        assert!(error.to_string().contains("invalid block id"));
    }

    #[test]
    fn database_option_is_accepted() {
        Cli::try_parse_from([
            "resolver",
            "sync",
            "--gql-url",
            "node:8600",
            "--database",
            "resolver.sqlite",
        ])
        .unwrap();
    }
}
