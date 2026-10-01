use async_trait::async_trait;
use bridge_gql_fetcher::gql_client::{create_client, GqlClient, GqlGraphBlock};

use crate::{BlockId, BlockNode, BlockProvider, ProofBlock, ThreadId, TimedBlock};

/// GraphQL adapter for an endpoint that returns finalized Acki Nacki blocks.
/// The live schema has no explicit finality filter, so endpoint selection is
/// part of this provider's trust/configuration contract.
pub struct GraphqlBlockProvider {
    client: GqlClient,
    namespace: String,
}

impl GraphqlBlockProvider {
    pub fn new(endpoint: &str) -> anyhow::Result<Self> {
        Ok(Self {
            client: create_client(endpoint)?,
            namespace: bridge_gql_fetcher::gql_client::normalize_endpoint(endpoint),
        })
    }

    pub fn from_client(client: GqlClient, namespace: impl Into<String>) -> Self {
        Self {
            client,
            namespace: namespace.into(),
        }
    }
}

fn project(block: GqlGraphBlock) -> BlockNode {
    BlockNode {
        block_id: BlockId::from_bytes(block.block_id),
        thread_id: ThreadId::from_bytes(*block.thread_id.as_bytes()),
        height: block.height,
        refs: block
            .proof_block_refs
            .into_iter()
            .map(BlockId::from_bytes)
            .collect(),
    }
}

fn project_timed(block: GqlGraphBlock) -> Option<TimedBlock> {
    let gen_utime_ms = block.gen_utime_ms?;
    Some(TimedBlock {
        block: project(block),
        gen_utime_ms,
    })
}

#[async_trait]
impl BlockProvider for GraphqlBlockProvider {
    async fn block_by_id(&self, id: &BlockId) -> anyhow::Result<Option<BlockNode>> {
        Ok(self
            .client
            .query_graph_block_by_id(&id.to_string())
            .await?
            .map(project))
    }

    async fn latest_blocks(&self, limit: usize) -> anyhow::Result<Vec<BlockNode>> {
        let count = u32::try_from(limit)
            .map_err(|_| anyhow::anyhow!("scan window {limit} exceeds GraphQL u32 limit"))?;
        Ok(self
            .client
            .query_latest_graph_blocks(count)
            .await?
            .into_iter()
            .map(project)
            .collect())
    }

    async fn timed_block_by_id(&self, id: &BlockId) -> anyhow::Result<Option<TimedBlock>> {
        Ok(self
            .client
            .query_graph_block_by_id(&id.to_string())
            .await?
            .and_then(project_timed))
    }

    async fn timed_block_by_height(
        &self,
        thread: &ThreadId,
        height: u64,
    ) -> anyhow::Result<Option<TimedBlock>> {
        Ok(self
            .client
            .query_graph_block_by_height(&thread.to_string(), height)
            .await?
            .and_then(project_timed))
    }

    async fn latest_timed_block_in_thread(
        &self,
        thread: &ThreadId,
    ) -> anyhow::Result<Option<TimedBlock>> {
        Ok(self
            .client
            .query_latest_graph_block_in_thread(&thread.to_string())
            .await?
            .and_then(project_timed))
    }

    async fn proof_block_by_id(&self, id: &BlockId) -> anyhow::Result<Option<ProofBlock>> {
        let Some(graph) = self.client.query_graph_block_by_id(&id.to_string()).await? else {
            return Ok(None);
        };
        let gen_utime_ms = graph
            .gen_utime_ms
            .ok_or_else(|| anyhow::anyhow!("proof block {id} has no generation timestamp"))?;
        let proof = self.client.query_proof_block_by_id(&id.to_string()).await?;
        anyhow::ensure!(
            proof.block_id == *id.as_bytes(),
            "proof block id mismatch for {id}"
        );
        anyhow::ensure!(
            proof.thread_id.as_bytes() == graph.thread_id.as_bytes(),
            "proof/graph thread mismatch for {id}"
        );
        anyhow::ensure!(
            proof.height == graph.height,
            "proof/graph height mismatch for {id}"
        );
        anyhow::ensure!(
            proof.proof_block_refs == graph.proof_block_refs,
            "proof/graph references mismatch for {id}"
        );
        let leaves = proof
            .block_merkle_tree_leaves
            .ok_or_else(|| anyhow::anyhow!("proof block {id} has no block_merkle_tree_leaves"))?;
        Ok(Some(ProofBlock {
            block: project(graph),
            gen_utime_ms,
            envelope_hash: proof.envelope_hash,
            tracked_ext_out_messages_root: proof.tracked_ext_out_messages_root,
            history_proofs: proof.history_proofs,
            block_merkle_tree_leaves: leaves,
        }))
    }

    async fn proof_block_by_height(
        &self,
        thread: &ThreadId,
        height: u64,
    ) -> anyhow::Result<Option<ProofBlock>> {
        let Some(graph) = self
            .client
            .query_graph_block_by_height(&thread.to_string(), height)
            .await?
        else {
            return Ok(None);
        };
        let id = BlockId::from_bytes(graph.block_id);
        let proof = self.proof_block_by_id(&id).await?;
        anyhow::ensure!(
            proof.as_ref().is_some_and(|proof| {
                proof.block.thread_id == *thread && proof.block.height == height
            }),
            "proof block lookup mismatch at thread {thread}, height {height}"
        );
        Ok(proof)
    }

    fn namespace(&self) -> &str {
        &self.namespace
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_bare_endpoint_for_namespace_and_client() {
        let provider = GraphqlBlockProvider::new("node.example:8600").unwrap();
        assert_eq!(provider.namespace(), "http://node.example:8600/graphql");
    }
}
