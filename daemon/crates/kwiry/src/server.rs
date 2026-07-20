use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use kwiry_core::{
    ApiErrorEnvelope, ApiSearchRequest, ApiSearchResponse, Config, DaemonState, DaemonStatus,
    DataRoot, HealthResponse, IndexManager, Manifest, ManifestFileOutcome, ModelStatus, Paths,
    SearchMode, SearchRuntime, VaultStatus, build_index, load_config, load_or_create_token,
    update_config,
};

use crate::auth::{AuthState, require_auth};
use crate::runtime::{ManagerHandle, spawn_manager};
use crate::watcher::spawn_watcher;

#[derive(Clone)]
pub(crate) struct AppState {
    runtime: SearchRuntime,
    status: Arc<RwLock<DaemonStatus>>,
}

pub(crate) async fn serve(
    paths: Paths,
    bind_override: Option<String>,
    semantic: bool,
) -> Result<()> {
    let mut config = load_config(&paths.config)?;
    if config.auth.token_file.is_none() {
        let token_path = paths.config.with_extension("token");
        update_config(&paths.config, |config| {
            config.auth.token_file = Some(token_path.clone());
            Ok(())
        })?;
        config = load_config(&paths.config)?;
    }
    let token_path = paths.token_path(&config);
    let token = load_or_create_token(&token_path)?;
    let bind = bind_override.unwrap_or_else(|| config.server.bind.clone());
    let address: SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid bind address: {bind}"))?;
    if !address.ip().is_loopback() {
        return Err(anyhow!(
            "Vertical 2 accepts loopback bind addresses only: {address}"
        ));
    }
    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind {address}"))?;

    let data_root = DataRoot::new(&paths.data_dir);
    if data_root.active()?.is_none() {
        build_index(&config, &paths.data_dir)?;
    }

    let runtime = SearchRuntime::new();
    if semantic {
        install_semantic(&paths, &runtime)?;
    }
    let mut manager = IndexManager::open(config.clone(), &paths.data_dir, runtime.clone())?;
    let report = manager.reconcile(config.clone())?;
    let mut status = status_from_manifest(
        &config,
        manager.manifest(),
        &report.unavailable_vaults,
        runtime.generation(),
    );
    status.model = runtime.semantic_profile().map(|profile| ModelStatus {
        name: profile.model_id.clone(),
        version: profile.fingerprint(),
    });
    let state = AppState {
        runtime,
        status: Arc::new(RwLock::new(status)),
    };
    let (manager_handle, manager_task) = spawn_manager(manager);
    let watcher = spawn_watcher(
        paths.clone(),
        config,
        manager_handle.clone(),
        state.status.clone(),
    )?;
    let router = build_router(state, AuthState::new(token));
    let local_address = listener.local_addr()?;
    println!(
        "kwiry listening on http://{local_address}; bearer token file: {}",
        token_path.display()
    );

    let server_result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    watcher.shutdown().await;
    let shutdown_result = shutdown_manager(manager_handle, manager_task).await;
    server_result.context("HTTP server failed")?;
    shutdown_result?;
    Ok(())
}

#[cfg(feature = "semantic-onnx")]
fn install_semantic(paths: &Paths, runtime: &SearchRuntime) -> Result<()> {
    use std::sync::Arc;

    let cache_dir = paths.data_dir.join("models");
    let store_path = paths.data_dir.join("semantic").join("semantic.db");
    println!(
        "loading embedding model (cache: {}; first run downloads ~130 MB)",
        cache_dir.display()
    );
    let embedder = kwiry_core::FastembedEmbedder::new(&cache_dir)
        .context("failed to load the embedding model")?;
    let semantic = kwiry_core::SemanticRuntime::open(&store_path, Box::new(embedder))
        .context("failed to open the semantic store")?;
    runtime.install_semantic(Arc::new(semantic));
    Ok(())
}

#[cfg(not(feature = "semantic-onnx"))]
fn install_semantic(_paths: &Paths, _runtime: &SearchRuntime) -> Result<()> {
    Err(anyhow!(
        "this build does not include semantic support; rebuild with --features semantic-onnx"
    ))
}

pub(crate) fn build_router(state: AppState, auth: AuthState) -> Router {
    let protected = Router::new()
        .route("/v0/search", post(search))
        .route("/v0/status", get(status))
        .method_not_allowed_fallback(method_not_allowed)
        .route_layer(middleware::from_fn_with_state(auth, require_auth));
    Router::new()
        .route("/v0/health", get(health))
        .merge(protected)
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(not_found)
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse::default())
}

async fn not_found() -> HttpError {
    HttpError::new(StatusCode::NOT_FOUND, "not_found", "endpoint not found")
}

async fn method_not_allowed() -> HttpError {
    HttpError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "method not allowed",
    )
}

async fn status(State(state): State<AppState>) -> Json<DaemonStatus> {
    Json(state.status.read().await.clone())
}

async fn search(
    State(state): State<AppState>,
    payload: std::result::Result<Json<ApiSearchRequest>, JsonRejection>,
) -> std::result::Result<Json<ApiSearchResponse>, HttpError> {
    let Json(request) = payload.map_err(|error| {
        HttpError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            error.body_text(),
        )
    })?;
    request
        .validate(state.runtime.semantic_ready())
        .map_err(|error| {
            let status = match error.code {
                "mode_unavailable" | "cursor_unavailable" => StatusCode::NOT_IMPLEMENTED,
                _ => StatusCode::BAD_REQUEST,
            };
            HttpError::new(status, error.code, error.message)
        })?;
    let runtime = state.runtime.clone();
    let query = request.q.clone();
    // Semantic legs run ONNX inference; keep them off the async executor.
    let hits = tokio::task::spawn_blocking(move || match request.mode {
        SearchMode::Lexical => runtime.search_filtered(&query, request.limit, &request.filters),
        SearchMode::Semantic => runtime.search_semantic(&query, request.limit, &request.filters),
        SearchMode::Hybrid => runtime.search_hybrid(&query, request.limit, &request.filters),
    })
    .await
    .map_err(|_| {
        HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "the search task failed",
        )
    })?
    .map_err(map_core_error)?;
    Ok(Json(ApiSearchResponse {
        hits,
        next_cursor: None,
    }))
}

fn map_core_error(error: kwiry_core::Error) -> HttpError {
    match error {
        kwiry_core::Error::Query(message) => {
            HttpError::new(StatusCode::BAD_REQUEST, "invalid_query", message)
        }
        kwiry_core::Error::Index(message) if message == "index is not ready" => {
            HttpError::new(StatusCode::SERVICE_UNAVAILABLE, "index_not_ready", message)
        }
        kwiry_core::Error::SemanticUnavailable(message) => {
            HttpError::new(StatusCode::NOT_IMPLEMENTED, "mode_unavailable", message)
        }
        _ => HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "the search request could not be completed",
        ),
    }
}

pub(crate) fn status_from_manifest(
    config: &Config,
    manifest: &Manifest,
    unavailable_vaults: &[String],
    generation: Option<String>,
) -> DaemonStatus {
    let unavailable: std::collections::HashSet<_> = unavailable_vaults.iter().collect();
    let mut by_vault: BTreeMap<&str, Vec<_>> = BTreeMap::new();
    for file in manifest.files.values() {
        by_vault.entry(&file.vault_id).or_default().push(file);
    }
    let vaults = config
        .vaults
        .iter()
        .map(|vault| {
            let files = by_vault.get(vault.id.as_str()).cloned().unwrap_or_default();
            VaultStatus {
                vault_id: vault.id.clone(),
                room: vault.room.clone(),
                documents: files
                    .iter()
                    .filter(|file| file.outcome == ManifestFileOutcome::Indexed)
                    .count(),
                chunks: files.iter().map(|file| file.chunk_count).sum(),
                last_sync: manifest.last_sync.clone(),
                dirty: unavailable.contains(&vault.id),
                warning_count: files.iter().filter(|file| file.warning.is_some()).count(),
                last_error: unavailable
                    .contains(&vault.id)
                    .then(|| "vault root is unavailable".to_owned()),
            }
        })
        .collect();
    let degraded = !unavailable_vaults.is_empty();
    DaemonStatus {
        state: if degraded {
            DaemonState::Degraded
        } else {
            DaemonState::Ready
        },
        version: env!("CARGO_PKG_VERSION").to_owned(),
        generation,
        chunking_version: kwiry_core::CHUNKING_VERSION,
        documents: manifest.document_count(),
        chunks: manifest.chunk_count(),
        last_sync: manifest.last_sync.clone(),
        dirty: degraded,
        rebuilding: false,
        model: None,
        vaults,
    }
}

async fn shutdown_manager(manager: ManagerHandle, task: tokio::task::JoinHandle<()>) -> Result<()> {
    manager.shutdown().await?;
    task.await.context("index manager task failed")?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

struct HttpError {
    status: StatusCode,
    body: ApiErrorEnvelope,
}

impl HttpError {
    fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ApiErrorEnvelope::new(code, message),
        }
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    fn test_state() -> AppState {
        AppState {
            runtime: SearchRuntime::new(),
            status: Arc::new(RwLock::new(DaemonStatus::starting("0.1.0"))),
        }
    }

    #[tokio::test]
    async fn health_is_public_and_status_requires_authentication() {
        let app = build_router(test_state(), AuthState::new("secret".to_owned()));
        let health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v0/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        let status = app
            .oneshot(
                Request::builder()
                    .uri("/v0/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(status.headers().get(WWW_AUTHENTICATE).unwrap(), "Bearer");
    }

    #[tokio::test]
    async fn routing_failures_use_json_error_envelopes() {
        let app = build_router(test_state(), AuthState::new("secret".to_owned()));
        let missing = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v0/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let body = missing.into_body().collect().await.unwrap().to_bytes();
        let error: ApiErrorEnvelope = serde_json::from_slice(&body).unwrap();
        assert_eq!(error.error.code, "not_found");

        let wrong_method = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v0/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong_method.status(), StatusCode::METHOD_NOT_ALLOWED);
        let body = wrong_method.into_body().collect().await.unwrap().to_bytes();
        let error: ApiErrorEnvelope = serde_json::from_slice(&body).unwrap();
        assert_eq!(error.error.code, "method_not_allowed");
    }

    #[tokio::test]
    async fn unavailable_mode_uses_frozen_error_envelope() {
        let app = build_router(test_state(), AuthState::new("secret".to_owned()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v0/search")
                    .header(AUTHORIZATION, "Bearer secret")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"q":"notes"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let error: ApiErrorEnvelope = serde_json::from_slice(&body).unwrap();
        assert_eq!(error.error.code, "mode_unavailable");
    }
}
