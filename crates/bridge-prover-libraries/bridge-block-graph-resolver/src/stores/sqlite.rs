use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Transaction};

use crate::{
    ApplyStats, BlockEdge, BlockId, BlockNode, PathCacheKey, PruneStats, ResolutionPolicy,
    ResolvedPath, ResolverStore, StoreBatch, StoreVersion, ThreadId,
};

const SCHEMA_VERSION: u32 = 4;
const QUERY_CHUNK: usize = 500;

/// Persistent resolver storage bound to one provider namespace.
///
/// Opening an existing database with a different namespace fails before any
/// graph data is read or written. This prevents block/cache data from two
/// networks or endpoints being mixed accidentally.
#[derive(Clone)]
pub struct SqliteStore {
    connection: Arc<Mutex<Connection>>,
    namespace: Arc<str>,
}

impl SqliteStore {
    pub async fn open(
        path: impl AsRef<Path>,
        namespace: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let path = path.as_ref().to_owned();
        let namespace = namespace.into();
        let namespace_for_open = namespace.clone();
        let connection = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            let mut connection = Connection::open(&path)?;
            initialize(&mut connection, &namespace_for_open)?;
            Ok(connection)
        })
        .await
        .map_err(|error| anyhow::anyhow!("SQLite initialization task failed: {error}"))??;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            namespace: namespace.into(),
        })
    }

    #[cfg(test)]
    async fn open_in_memory(namespace: &str) -> anyhow::Result<Self> {
        let namespace = namespace.to_owned();
        let namespace_for_open = namespace.clone();
        let connection = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            let mut connection = Connection::open_in_memory()?;
            initialize(&mut connection, &namespace_for_open)?;
            Ok(connection)
        })
        .await
        .map_err(|error| anyhow::anyhow!("SQLite initialization task failed: {error}"))??;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            namespace: namespace.into(),
        })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    async fn call<T, F>(&self, operation: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> anyhow::Result<T> + Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| anyhow::anyhow!("SQLite connection lock is poisoned"))?;
            operation(&mut connection)
        })
        .await
        .map_err(|error| anyhow::anyhow!("SQLite operation task failed: {error}"))?
    }
}

fn initialize(connection: &mut Connection, namespace: &str) -> anyhow::Result<()> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS resolver_metadata (
             singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
             schema_version INTEGER NOT NULL,
             namespace TEXT NOT NULL,
             graph_version BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS blocks (
             block_id BLOB PRIMARY KEY,
             thread_id BLOB NOT NULL,
             height_be BLOB NOT NULL
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS block_refs (
             from_id BLOB NOT NULL REFERENCES blocks(block_id) ON DELETE CASCADE,
             ref_index INTEGER NOT NULL,
             to_id BLOB NOT NULL,
             PRIMARY KEY (from_id, ref_index)
         ) WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS block_refs_to_id
             ON block_refs(to_id, from_id, ref_index);
         CREATE TABLE IF NOT EXISTS paths (
             namespace TEXT NOT NULL,
             target BLOB NOT NULL,
             policy INTEGER NOT NULL,
             max_hops INTEGER NOT NULL,
             max_visited_be BLOB NOT NULL,
             path_json TEXT NOT NULL,
             PRIMARY KEY (
                 namespace, target, policy, max_hops,
                 max_visited_be
             )
         ) WITHOUT ROWID;",
    )?;

    let tx = connection.transaction()?;
    let existing: Option<(u32, String)> = tx
        .query_row(
            "SELECT schema_version, namespace FROM resolver_metadata WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match existing {
        Some((version, stored_namespace)) => {
            anyhow::ensure!(
                stored_namespace == namespace,
                "resolver database belongs to namespace {stored_namespace:?}, not {namespace:?}"
            );
            match version {
                1 => migrate_to_v4(&tx)?,
                2 => {
                    let legacy_policy: i64 = tx.query_row(
                        "SELECT edge_policy FROM resolver_metadata WHERE singleton = 1",
                        [],
                        |row| row.get(0),
                    )?;
                    if legacy_policy != 0 {
                        tx.execute("DELETE FROM paths", [])?;
                        tx.execute("DELETE FROM blocks", [])?;
                        tx.execute(
                            "UPDATE resolver_metadata SET graph_version = ?1 WHERE singleton = 1",
                            [encode_u64(0)],
                        )?;
                    }
                    tx.execute("ALTER TABLE resolver_metadata DROP COLUMN edge_policy", [])?;
                    migrate_to_v4(&tx)?;
                },
                3 => migrate_to_v4(&tx)?,
                SCHEMA_VERSION => {},
                _ => anyhow::bail!(
                    "unsupported resolver SQLite schema version {version}; expected \
                     {SCHEMA_VERSION}"
                ),
            }
        },
        None => {
            tx.execute(
                "INSERT INTO resolver_metadata
                 (singleton, schema_version, namespace, graph_version)
                 VALUES (1, ?1, ?2, ?3)",
                params![SCHEMA_VERSION, namespace, encode_u64(0)],
            )?;
        },
    }
    tx.commit()?;
    Ok(())
}

fn migrate_to_v4(transaction: &Transaction<'_>) -> anyhow::Result<()> {
    if table_has_column(transaction, "paths", "anchor_epoch")? {
        transaction.execute_batch(
            "CREATE TABLE paths_v4 (
                 namespace TEXT NOT NULL,
                 target BLOB NOT NULL,
                 policy INTEGER NOT NULL,
                 max_hops INTEGER NOT NULL,
                 max_visited_be BLOB NOT NULL,
                 path_json TEXT NOT NULL,
                 PRIMARY KEY (namespace, target, policy, max_hops, max_visited_be)
             ) WITHOUT ROWID;
             INSERT OR REPLACE INTO paths_v4
                 (namespace, target, policy, max_hops, max_visited_be, path_json)
             SELECT namespace, target, policy, max_hops, max_visited_be, path_json FROM paths;
             DROP TABLE paths;
             ALTER TABLE paths_v4 RENAME TO paths;",
        )?;
    }
    if table_has_column(transaction, "resolver_metadata", "anchor_epoch")? {
        transaction.execute("ALTER TABLE resolver_metadata DROP COLUMN anchor_epoch", [])?;
    }
    transaction.execute(
        "UPDATE resolver_metadata SET schema_version = ?1 WHERE singleton = 1",
        [SCHEMA_VERSION],
    )?;
    Ok(())
}

fn table_has_column(
    transaction: &Transaction<'_>,
    table: &str,
    column: &str,
) -> anyhow::Result<bool> {
    let mut statement = transaction.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns.iter().any(|candidate| candidate == column))
}

fn encode_u64(value: u64) -> [u8; 8] {
    value.to_be_bytes()
}

fn decode_u64(value: Vec<u8>, field: &str) -> anyhow::Result<u64> {
    let bytes: [u8; 8] = value.try_into().map_err(|value: Vec<u8>| {
        anyhow::anyhow!("{field} must be 8 bytes, got {}", value.len())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn decode_block_id(value: Vec<u8>, field: &str) -> anyhow::Result<BlockId> {
    let bytes: [u8; 32] = value.try_into().map_err(|value: Vec<u8>| {
        anyhow::anyhow!("{field} must be 32 bytes, got {}", value.len())
    })?;
    Ok(BlockId::from_bytes(bytes))
}

fn decode_thread_id(value: Vec<u8>) -> anyhow::Result<ThreadId> {
    let bytes: [u8; 34] = value.try_into().map_err(|value: Vec<u8>| {
        anyhow::anyhow!("thread_id must be 34 bytes, got {}", value.len())
    })?;
    Ok(ThreadId::from_bytes(bytes))
}

fn version_in(transaction: &Transaction<'_>) -> anyhow::Result<StoreVersion> {
    let graph: Vec<u8> = transaction.query_row(
        "SELECT graph_version FROM resolver_metadata WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    Ok(StoreVersion {
        graph_version: decode_u64(graph, "graph_version")?,
    })
}

fn load_block(connection: &Connection, id: &BlockId) -> anyhow::Result<Option<BlockNode>> {
    let row: Option<(Vec<u8>, Vec<u8>)> = connection
        .query_row(
            "SELECT thread_id, height_be FROM blocks WHERE block_id = ?1",
            [id.as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((thread_id, height)) = row else {
        return Ok(None);
    };
    let mut statement =
        connection.prepare("SELECT to_id FROM block_refs WHERE from_id = ?1 ORDER BY ref_index")?;
    let refs = statement
        .query_map([id.as_bytes().as_slice()], |row| row.get::<_, Vec<u8>>(0))?
        .map(|value| decode_block_id(value?, "to_id"))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Some(BlockNode {
        block_id: *id,
        thread_id: decode_thread_id(thread_id)?,
        height: decode_u64(height, "height_be")?,
        refs,
    }))
}

fn load_blocks(
    connection: &Connection,
    ids: &[BlockId],
) -> anyhow::Result<HashMap<BlockId, BlockNode>> {
    let mut blocks = HashMap::new();
    for chunk in ids.chunks(QUERY_CHUNK) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT block_id, thread_id, height_be FROM blocks WHERE block_id IN ({placeholders})"
        );
        let mut statement = connection.prepare(&sql)?;
        let values = chunk.iter().map(|id| id.as_bytes().as_slice());
        let mut rows = statement.query(params_from_iter(values))?;
        while let Some(row) = rows.next()? {
            let block_id = decode_block_id(row.get(0)?, "block_id")?;
            blocks.insert(block_id, BlockNode {
                block_id,
                thread_id: decode_thread_id(row.get(1)?)?,
                height: decode_u64(row.get(2)?, "height_be")?,
                refs: Vec::new(),
            });
        }
    }

    for chunk in ids.chunks(QUERY_CHUNK) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT from_id, to_id FROM block_refs WHERE from_id IN ({placeholders}) ORDER BY \
             from_id, ref_index"
        );
        let mut statement = connection.prepare(&sql)?;
        let values = chunk.iter().map(|id| id.as_bytes().as_slice());
        let mut rows = statement.query(params_from_iter(values))?;
        while let Some(row) = rows.next()? {
            let from = decode_block_id(row.get(0)?, "from_id")?;
            let to = decode_block_id(row.get(1)?, "to_id")?;
            let block = blocks
                .get_mut(&from)
                .ok_or_else(|| anyhow::anyhow!("reference origin {from} has no block row"))?;
            block.refs.push(to);
        }
    }
    Ok(blocks)
}

fn policy_code(policy: ResolutionPolicy) -> i64 {
    match policy {
        ResolutionPolicy::FirstValid => 0,
        ResolutionPolicy::ShortestCurrent => 1,
    }
}

#[async_trait]
impl ResolverStore for SqliteStore {
    async fn version(&self) -> anyhow::Result<StoreVersion> {
        self.call(|connection| {
            let tx = connection.transaction()?;
            let version = version_in(&tx)?;
            tx.commit()?;
            Ok(version)
        })
        .await
    }

    async fn block(&self, id: &BlockId) -> anyhow::Result<Option<BlockNode>> {
        let id = *id;
        self.call(move |connection| load_block(connection, &id))
            .await
    }

    async fn blocks(&self, ids: &[BlockId]) -> anyhow::Result<HashMap<BlockId, BlockNode>> {
        let ids = ids.to_vec();
        self.call(move |connection| load_blocks(connection, &ids))
            .await
    }

    async fn incoming_edges(
        &self,
        ids: &[BlockId],
    ) -> anyhow::Result<HashMap<BlockId, Vec<BlockEdge>>> {
        let ids = ids.to_vec();
        self.call(move |connection| {
            let mut result: HashMap<BlockId, Vec<BlockEdge>> = HashMap::new();
            for chunk in ids.chunks(QUERY_CHUNK) {
                let placeholders = std::iter::repeat_n("?", chunk.len())
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT from_id, to_id, ref_index FROM block_refs WHERE to_id IN \
                     ({placeholders}) ORDER BY to_id, from_id, ref_index"
                );
                let mut statement = connection.prepare(&sql)?;
                let values = chunk.iter().map(|id| id.as_bytes().as_slice());
                let mut rows = statement.query(params_from_iter(values))?;
                while let Some(row) = rows.next()? {
                    let from = decode_block_id(row.get(0)?, "from_id")?;
                    let to = decode_block_id(row.get(1)?, "to_id")?;
                    let ref_index = u32::try_from(row.get::<_, i64>(2)?)
                        .map_err(|_| anyhow::anyhow!("ref_index is outside u32 range"))?;
                    result.entry(to).or_default().push(BlockEdge {
                        from,
                        to,
                        ref_index,
                    });
                }
            }
            Ok(result)
        })
        .await
    }

    async fn apply(&self, batch: StoreBatch) -> anyhow::Result<ApplyStats> {
        self.call(move |connection| {
            let tx = connection.transaction()?;
            let mut stats = ApplyStats::default();
            let mut changed = false;
            for block in batch.blocks {
                match load_block(&tx, &block.block_id)? {
                    Some(existing) if existing == block => stats.unchanged += 1,
                    Some(_) => {
                        tx.execute("DELETE FROM block_refs WHERE from_id = ?1", [block
                            .block_id
                            .as_bytes()
                            .as_slice()])?;
                        tx.execute(
                            "UPDATE blocks SET thread_id = ?2, height_be = ?3 WHERE block_id = ?1",
                            params![
                                block.block_id.as_bytes().as_slice(),
                                block.thread_id.as_bytes().as_slice(),
                                encode_u64(block.height)
                            ],
                        )?;
                        insert_refs(&tx, &block)?;
                        stats.updated += 1;
                        changed = true;
                    },
                    None => {
                        tx.execute(
                            "INSERT INTO blocks(block_id, thread_id, height_be) VALUES (?1, ?2, \
                             ?3)",
                            params![
                                block.block_id.as_bytes().as_slice(),
                                block.thread_id.as_bytes().as_slice(),
                                encode_u64(block.height)
                            ],
                        )?;
                        insert_refs(&tx, &block)?;
                        stats.inserted += 1;
                        changed = true;
                    },
                }
            }
            let mut version = version_in(&tx)?;
            if changed {
                version.graph_version = version
                    .graph_version
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("SQLite graph version overflow"))?;
            }
            tx.execute(
                "UPDATE resolver_metadata SET graph_version = ?1 WHERE singleton = 1",
                [encode_u64(version.graph_version)],
            )?;
            tx.commit()?;
            stats.graph_version = version.graph_version;
            Ok(stats)
        })
        .await
    }

    async fn prune_recent(&self, per_thread: usize) -> anyhow::Result<PruneStats> {
        self.call(move |connection| {
            let tx = connection.transaction()?;
            let mut version = version_in(&tx)?;
            if per_thread == 0 {
                tx.commit()?;
                return Ok(PruneStats {
                    pruned: 0,
                    graph_version: version.graph_version,
                });
            }
            let mut statement = tx.prepare(
                "SELECT block_id, thread_id, height_be FROM blocks
                 ORDER BY thread_id, height_be DESC, block_id DESC",
            )?;
            let mut rows = statement.query([])?;
            let mut counts: HashMap<ThreadId, usize> = HashMap::new();
            let mut remove = Vec::new();
            while let Some(row) = rows.next()? {
                let block_id = decode_block_id(row.get(0)?, "block_id")?;
                let thread_id = decode_thread_id(row.get(1)?)?;
                let count = counts.entry(thread_id).or_default();
                *count += 1;
                if *count > per_thread {
                    remove.push(block_id);
                }
            }
            drop(rows);
            drop(statement);
            for id in &remove {
                tx.execute("DELETE FROM blocks WHERE block_id = ?1", [id
                    .as_bytes()
                    .as_slice()])?;
            }
            if !remove.is_empty() {
                version.graph_version = version
                    .graph_version
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("SQLite graph version overflow"))?;
                tx.execute(
                    "UPDATE resolver_metadata SET graph_version = ?1 WHERE singleton = 1",
                    [encode_u64(version.graph_version)],
                )?;
            }
            tx.commit()?;
            Ok(PruneStats {
                pruned: remove.len(),
                graph_version: version.graph_version,
            })
        })
        .await
    }

    async fn cached_path(&self, key: &PathCacheKey) -> anyhow::Result<Option<ResolvedPath>> {
        let key = key.clone();
        self.call(move |connection| {
            let json: Option<String> = connection
                .query_row(
                    "SELECT path_json FROM paths WHERE namespace = ?1 AND target = ?2 AND policy \
                     = ?3 AND max_hops = ?4 AND max_visited_be = ?5",
                    params![
                        key.namespace,
                        key.target.as_bytes().as_slice(),
                        policy_code(key.policy),
                        i64::from(key.limits.max_hops),
                        encode_u64(key.limits.max_visited_blocks as u64)
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            json.map(|json| serde_json::from_str(&json).map_err(Into::into))
                .transpose()
        })
        .await
    }

    async fn cache_path(&self, key: PathCacheKey, path: ResolvedPath) -> anyhow::Result<()> {
        self.call(move |connection| {
            let json = serde_json::to_string(&path)?;
            connection.execute(
                "INSERT INTO paths(namespace, target, policy, max_hops, max_visited_be, path_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(namespace, target, policy, max_hops, max_visited_be)
                 DO UPDATE SET path_json = excluded.path_json",
                params![
                    key.namespace,
                    key.target.as_bytes().as_slice(),
                    policy_code(key.policy),
                    i64::from(key.limits.max_hops),
                    encode_u64(key.limits.max_visited_blocks as u64),
                    json
                ],
            )?;
            Ok(())
        })
        .await
    }
}

fn insert_refs(transaction: &Transaction<'_>, block: &BlockNode) -> anyhow::Result<()> {
    let mut seen = HashSet::new();
    for (index, target) in block.refs.iter().enumerate() {
        anyhow::ensure!(
            seen.insert(*target),
            "block {} repeats reference {} at slot {}",
            block.block_id,
            target,
            index
        );
        transaction.execute(
            "INSERT INTO block_refs(from_id, ref_index, to_id) VALUES (?1, ?2, ?3)",
            params![
                block.block_id.as_bytes().as_slice(),
                i64::try_from(index)?,
                target.as_bytes().as_slice()
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ResolverLimits, StoreBatch};

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
    async fn store_contract_apply_replace_prune_and_cache() {
        let store = SqliteStore::open_in_memory("network-a").await.unwrap();
        let original = block(1, 1, 1, &[2, 3]);
        let applied = store
            .apply(StoreBatch {
                blocks: vec![original.clone(), block(4, 1, 2, &[1])],
            })
            .await
            .unwrap();
        assert_eq!((applied.inserted, applied.graph_version), (2, 1));
        assert_eq!(store.block(&id(1)).await.unwrap(), Some(original.clone()));
        assert_eq!(store.blocks(&[id(1), id(9)]).await.unwrap().len(), 1);
        let incoming = store.incoming_edges(&[id(2), id(3)]).await.unwrap();
        assert_eq!(incoming[&id(2)][0].ref_index, 0);
        assert_eq!(incoming[&id(3)][0].ref_index, 1);

        let unchanged = store
            .apply(StoreBatch {
                blocks: vec![original],
            })
            .await
            .unwrap();
        assert_eq!((unchanged.unchanged, unchanged.graph_version), (1, 1));

        let replacement = block(1, 1, 1, &[5]);
        let updated = store
            .apply(StoreBatch {
                blocks: vec![replacement],
            })
            .await
            .unwrap();
        assert_eq!((updated.updated, updated.graph_version), (1, 2));
        assert!(store.incoming_edges(&[id(2)]).await.unwrap().is_empty());
        assert_eq!(
            store.incoming_edges(&[id(5)]).await.unwrap()[&id(5)][0].ref_index,
            0
        );

        let key = PathCacheKey {
            namespace: "network-a".into(),
            target: id(5),
            policy: ResolutionPolicy::ShortestCurrent,
            limits: ResolverLimits {
                max_hops: 10,
                max_visited_blocks: 100,
            },
        };
        let path = ResolvedPath {
            anchor: id(1),
            anchor_height: 1,
            target: id(5),
            hops: vec![BlockEdge {
                from: id(1),
                to: id(5),
                ref_index: 0,
            }],
            graph_version: 2,
        };
        store.cache_path(key.clone(), path.clone()).await.unwrap();
        assert_eq!(store.cached_path(&key).await.unwrap(), Some(path));

        let pruned = store.prune_recent(1).await.unwrap();
        assert_eq!((pruned.pruned, pruned.graph_version), (1, 3));
        assert!(store.block(&id(1)).await.unwrap().is_none());
        assert_eq!(
            store.incoming_edges(&[id(1)]).await.unwrap()[&id(1)][0].from,
            id(4)
        );
        assert!(store.incoming_edges(&[id(5)]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn persists_state_and_rejects_a_different_namespace() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("resolver.sqlite");
        let cache_key = PathCacheKey {
            namespace: "network-a".into(),
            target: id(1),
            policy: ResolutionPolicy::FirstValid,
            limits: ResolverLimits {
                max_hops: 2,
                max_visited_blocks: 3,
            },
        };
        let cached_path = ResolvedPath {
            anchor: id(1),
            anchor_height: u64::MAX,
            target: id(1),
            hops: vec![],
            graph_version: 1,
        };
        {
            let store = SqliteStore::open(&path, "network-a").await.unwrap();
            store
                .apply(StoreBatch {
                    blocks: vec![block(1, 2, u64::MAX, &[])],
                })
                .await
                .unwrap();
            store
                .cache_path(cache_key.clone(), cached_path.clone())
                .await
                .unwrap();
        }
        let reopened = SqliteStore::open(&path, "network-a").await.unwrap();
        assert_eq!(
            reopened.block(&id(1)).await.unwrap().unwrap().height,
            u64::MAX
        );
        assert_eq!(
            reopened.cached_path(&cache_key).await.unwrap(),
            Some(cached_path)
        );
        drop(reopened);

        let error = match SqliteStore::open(&path, "network-b").await {
            Ok(_) => panic!("database must reject another namespace"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("network-a"));
        assert!(error.to_string().contains("network-b"));
    }

    #[tokio::test]
    async fn migrates_legacy_cross_thread_policy_to_unconditional_parent_refs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("resolver.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE resolver_metadata (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    schema_version INTEGER NOT NULL,
                    namespace TEXT NOT NULL,
                    edge_policy INTEGER NOT NULL,
                    graph_version BLOB NOT NULL,
                    anchor_epoch BLOB NOT NULL
                 );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO resolver_metadata VALUES (1, 2, 'network-a', 1, ?1, ?2)",
                params![encode_u64(7), encode_u64(9)],
            )
            .unwrap();
        drop(connection);

        let store = SqliteStore::open(&path, "network-a").await.unwrap();
        assert_eq!(store.version().await.unwrap(), StoreVersion::default());
        let columns = store
            .call(|connection| {
                let mut statement = connection.prepare("PRAGMA table_info(resolver_metadata)")?;
                let columns = statement
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(columns)
            })
            .await
            .unwrap();
        assert!(!columns.iter().any(|column| column == "edge_policy"));
        assert!(!columns.iter().any(|column| column == "anchor_epoch"));
    }

    #[tokio::test]
    async fn migrates_v3_cache_without_anchor_epoch() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("resolver.sqlite");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE resolver_metadata (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    schema_version INTEGER NOT NULL,
                    namespace TEXT NOT NULL,
                    graph_version BLOB NOT NULL,
                    anchor_epoch BLOB NOT NULL
                 );
                 CREATE TABLE paths (
                    namespace TEXT NOT NULL,
                    target BLOB NOT NULL,
                    policy INTEGER NOT NULL,
                    max_hops INTEGER NOT NULL,
                    max_visited_be BLOB NOT NULL,
                    anchor_epoch BLOB NOT NULL,
                    path_json TEXT NOT NULL,
                    PRIMARY KEY (
                        namespace, target, policy, max_hops,
                        max_visited_be, anchor_epoch
                    )
                 ) WITHOUT ROWID;",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO resolver_metadata VALUES (1, 3, 'network-a', ?1, ?2)",
                params![encode_u64(1), encode_u64(9)],
            )
            .unwrap();
        let key = PathCacheKey {
            namespace: "network-a".into(),
            target: id(1),
            policy: ResolutionPolicy::FirstValid,
            limits: ResolverLimits {
                max_hops: 2,
                max_visited_blocks: 3,
            },
        };
        let path = ResolvedPath {
            anchor: id(2),
            anchor_height: 1,
            target: id(1),
            hops: vec![],
            graph_version: 1,
        };
        let mut legacy_json = serde_json::to_value(&path).unwrap();
        legacy_json["anchor_epoch"] = serde_json::json!(9);
        connection
            .execute(
                "INSERT INTO paths VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    &key.namespace,
                    key.target.as_bytes().as_slice(),
                    policy_code(key.policy),
                    i64::from(key.limits.max_hops),
                    encode_u64(key.limits.max_visited_blocks as u64),
                    encode_u64(9),
                    legacy_json.to_string(),
                ],
            )
            .unwrap();
        drop(connection);

        let store = SqliteStore::open(&database, "network-a").await.unwrap();
        assert_eq!(store.cached_path(&key).await.unwrap(), Some(path));
        let columns = store
            .call(|connection| {
                let mut statement = connection.prepare("PRAGMA table_info(paths)")?;
                let columns = statement
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(columns)
            })
            .await
            .unwrap();
        assert!(!columns.iter().any(|column| column == "anchor_epoch"));
    }

    #[tokio::test]
    async fn failed_batch_is_atomic() {
        let store = SqliteStore::open_in_memory("network-a").await.unwrap();
        let error = store
            .apply(StoreBatch {
                blocks: vec![block(1, 1, 1, &[]), block(2, 1, 2, &[3, 3])],
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("repeats reference"));
        assert!(store.block(&id(1)).await.unwrap().is_none());
        assert_eq!(store.version().await.unwrap(), StoreVersion::default());
    }
}
