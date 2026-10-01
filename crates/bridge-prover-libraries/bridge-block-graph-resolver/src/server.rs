use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use tokio::sync::{Mutex, RwLock};

use crate::{
    ErrorBody, GraphResolver, GraphqlBlockProvider, ResolutionError, ResolutionRequest,
    ResolvedBlockProof, ResolvedPath, ServiceStatus, SqliteStore, StoreVersion, SyncStats,
};

type PersistentResolver = GraphResolver<GraphqlBlockProvider, SqliteStore>;

#[derive(Clone)]
pub struct ResolverApi {
    resolver: Arc<PersistentResolver>,
    scan_window: usize,
    namespace: Arc<str>,
    sync_lock: Arc<Mutex<()>>,
    status: Arc<RwLock<MutableStatus>>,
}

#[derive(Clone, Debug, Default)]
struct MutableStatus {
    last_sync: Option<SyncStats>,
    last_sync_unix_seconds: Option<u64>,
    last_error: Option<String>,
}

impl ResolverApi {
    pub fn new(
        resolver: Arc<PersistentResolver>,
        scan_window: usize,
        namespace: impl Into<String>,
    ) -> Self {
        Self {
            resolver,
            scan_window,
            namespace: namespace.into().into(),
            sync_lock: Arc::new(Mutex::new(())),
            status: Arc::new(RwLock::new(MutableStatus::default())),
        }
    }

    pub async fn sync_once(&self) -> anyhow::Result<SyncStats> {
        let _guard = self.sync_lock.lock().await;
        match self.resolver.sync_latest(self.scan_window).await {
            Ok(stats) => {
                let mut status = self.status.write().await;
                status.last_sync = Some(stats);
                status.last_sync_unix_seconds = Some(unix_seconds());
                status.last_error = None;
                Ok(stats)
            },
            Err(error) => {
                self.status.write().await.last_error = Some(error.to_string());
                Err(error)
            },
        }
    }

    pub async fn resolve(&self, request: ResolutionRequest) -> anyhow::Result<ResolvedPath> {
        self.resolver.resolve(request).await
    }

    pub async fn resolve_proof(
        &self,
        request: ResolutionRequest,
    ) -> anyhow::Result<ResolvedBlockProof> {
        self.resolver.resolve_proof(request).await
    }

    pub async fn status(&self) -> anyhow::Result<ServiceStatus> {
        let version = self.resolver_store_version().await?;
        let status = self.status.read().await.clone();
        Ok(ServiceStatus {
            healthy: status.last_error.is_none(),
            namespace: self.namespace.to_string(),
            store_version: version,
            last_sync: status.last_sync,
            last_sync_unix_seconds: status.last_sync_unix_seconds,
            last_error: status.last_error,
        })
    }

    async fn resolver_store_version(&self) -> anyhow::Result<StoreVersion> {
        // Keep store access behind GraphResolver in normal operation. The API
        // owns the concrete persistent resolver, so this small accessor is
        // supplied by GraphResolver rather than duplicating a DB handle here.
        self.resolver.store_version().await
    }
}

pub fn router(state: ResolverApi) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/status", get(status))
        .route("/v1/sync", post(sync))
        .route("/v1/resolve", post(resolve))
        .route("/v1/resolve-proof", post(resolve_proof))
        .with_state(state)
}

async fn health(State(api): State<ResolverApi>) -> Response {
    match api.status().await {
        Ok(status) => {
            let code = if status.healthy {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            (code, Json(status)).into_response()
        },
        Err(error) => ApiError::internal(error).into_response(),
    }
}

async fn status(State(api): State<ResolverApi>) -> Result<Json<ServiceStatus>, ApiError> {
    Ok(Json(api.status().await.map_err(ApiError::internal)?))
}

async fn sync(State(api): State<ResolverApi>) -> Result<Json<SyncStats>, ApiError> {
    Ok(Json(api.sync_once().await.map_err(ApiError::internal)?))
}

async fn resolve(
    State(api): State<ResolverApi>,
    Json(request): Json<ResolutionRequest>,
) -> Result<Json<ResolvedPath>, ApiError> {
    Ok(Json(
        api.resolve(request).await.map_err(ApiError::resolution)?,
    ))
}

async fn resolve_proof(
    State(api): State<ResolverApi>,
    Json(request): Json<ResolutionRequest>,
) -> Result<Json<ResolvedBlockProof>, ApiError> {
    Ok(Json(
        api.resolve_proof(request)
            .await
            .map_err(ApiError::resolution)?,
    ))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

struct ApiError {
    status: StatusCode,
    kind: &'static str,
    error: anyhow::Error,
}

impl ApiError {
    fn internal(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            kind: "internal",
            error,
        }
    }

    fn resolution(error: anyhow::Error) -> Self {
        let (status, kind) = match error.downcast_ref::<ResolutionError>() {
            Some(ResolutionError::TargetNotFound {
                ..
            }) => (StatusCode::NOT_FOUND, "target-not-found"),
            Some(ResolutionError::NoPath {
                ..
            }) => (StatusCode::NOT_FOUND, "no-path"),
            Some(ResolutionError::MaxVisited {
                ..
            }) => (StatusCode::UNPROCESSABLE_ENTITY, "max-visited"),
            Some(ResolutionError::HistoricalSearchLimit {
                ..
            }) => (StatusCode::UNPROCESSABLE_ENTITY, "historical-search-limit"),
            Some(ResolutionError::ProofBlockNotFound {
                ..
            }) => (StatusCode::NOT_FOUND, "proof-block-not-found"),
            Some(ResolutionError::InvalidProofBlock {
                ..
            }) => (StatusCode::BAD_GATEWAY, "invalid-proof-block"),
            None => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        Self {
            status,
            kind,
            error,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                kind: self.kind,
                error: self.error.to_string(),
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    use super::*;
    use crate::{
        BlockId, BlockNode, ResolutionPolicy, ResolverLimits, ResolverStore, StoreBatch, ThreadId,
    };

    fn id(n: u8) -> BlockId {
        BlockId::from_bytes([n; 32])
    }

    fn thread(n: u8) -> ThreadId {
        ThreadId::from_bytes([n; 34])
    }

    async fn test_api() -> (tempfile::TempDir, ResolverApi) {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SqliteStore::open(directory.path().join("resolver.sqlite"), "test-network")
                .await
                .unwrap(),
        );
        store
            .apply(StoreBatch {
                blocks: vec![
                    BlockNode {
                        block_id: id(1),
                        thread_id: thread(1),
                        height: 1,
                        refs: vec![],
                    },
                    BlockNode {
                        block_id: id(2),
                        thread_id: ThreadId::ZERO,
                        height: 2,
                        refs: vec![id(1)],
                    },
                    BlockNode {
                        block_id: id(3),
                        thread_id: thread(2),
                        height: 3,
                        refs: vec![],
                    },
                ],
            })
            .await
            .unwrap();
        let provider = Arc::new(GraphqlBlockProvider::new("127.0.0.1:9").unwrap());
        let resolver = Arc::new(
            GraphResolver::new(provider, store, 100).with_historical_search_config(
                crate::HistoricalSearchConfig {
                    max_anchor_candidates: 0,
                },
            ),
        );
        (directory, ResolverApi::new(resolver, 100, "test-network"))
    }

    fn resolution_request(target: BlockId) -> ResolutionRequest {
        ResolutionRequest {
            target,
            policy: ResolutionPolicy::ShortestCurrent,
            limits: ResolverLimits {
                max_hops: 10,
                max_visited_blocks: 100,
            },
        }
    }

    #[tokio::test]
    async fn health_and_status_return_versioned_json() {
        let (_directory, api) = test_api().await;
        let response = router(api)
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status["healthy"], true);
        assert_eq!(status["namespace"], "test-network");
        assert_eq!(status["store_version"]["graph_version"], 1);
    }

    #[tokio::test]
    async fn resolve_returns_path_and_structured_no_path_error() {
        let (_directory, api) = test_api().await;
        let request = Request::post("/v1/resolve")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&resolution_request(id(1))).unwrap(),
            ))
            .unwrap();
        let response = router(api.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let path: ResolvedPath = serde_json::from_slice(&body).unwrap();
        assert_eq!(path.anchor, id(2));
        assert_eq!(path.hops[0].ref_index, 0);

        let request = Request::post("/v1/resolve")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&resolution_request(id(3))).unwrap(),
            ))
            .unwrap();
        let response = router(api).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let error: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(error["kind"], "no-path");
        assert!(error["error"].as_str().unwrap().contains("graph_version=1"));
    }
}
