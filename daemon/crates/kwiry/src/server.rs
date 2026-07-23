use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use kwiry_core::{
    ApiErrorEnvelope, ApiSearchRequest, ApiSearchResponse, Config, ConnectionDescriptor,
    DaemonState, DaemonStatus, DataRoot, HealthResponse, HostProfile, IndexManager, Manifest,
    ManifestFileOutcome, ModelStatus, Paths, Principal, Scope, SearchMode, SearchRuntime,
    VaultStatus, bootstrap_desktop, build_index, load_config, write_connection_descriptor,
};

use crate::auth::{AuthState, require_auth};
use crate::capability::CapabilityVerifier;
use crate::logging::Redacted;
use crate::runtime::{ManagerHandle, spawn_manager};
use crate::watcher::spawn_watcher;

#[derive(Clone)]
pub(crate) struct AppState {
    profile: HostProfile,
    runtime: SearchRuntime,
    status: Arc<RwLock<DaemonStatus>>,
}

enum StartupIdentity {
    Desktop { token_path: std::path::PathBuf },
    OpenClast,
}

pub(crate) async fn serve(
    paths: Paths,
    bind_override: Option<String>,
    semantic: bool,
) -> Result<()> {
    let _logging = crate::logging::init(&paths.logs_dir())?;
    tracing::info!(
        config = %paths.config.display(),
        data_dir = %paths.data_dir.display(),
        "starting kwiry daemon"
    );
    let initial_config = load_config(&paths.config)?;
    let profile = initial_config.server.profile;
    let (config, auth, identity) = match profile {
        HostProfile::Desktop => {
            let bootstrap = bootstrap_desktop(&paths)?;
            tracing::debug!(
                token = %Redacted::new(bootstrap.token()),
                "desktop authentication initialized"
            );
            let auth = AuthState::desktop(bootstrap.token().to_owned());
            (
                bootstrap.config,
                auth,
                StartupIdentity::Desktop {
                    token_path: bootstrap.token_path,
                },
            )
        }
        HostProfile::OpenClast => {
            if semantic {
                return Err(anyhow!(
                    "semantic and hybrid search are unavailable in the openclast profile"
                ));
            }
            let auth_config = initial_config.auth.openclast.as_ref().ok_or_else(|| {
                anyhow!("openclast profile requires auth.openclast configuration")
            })?;
            let verifier =
                CapabilityVerifier::load(auth_config).map_err(|message| anyhow!(message))?;
            (
                initial_config,
                AuthState::openclast(verifier),
                StartupIdentity::OpenClast,
            )
        }
    };
    let bind = bind_override.unwrap_or_else(|| config.server.bind.clone());
    let address: SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid bind address: {bind}"))?;
    if profile == HostProfile::Desktop && !address.ip().is_loopback() {
        return Err(anyhow!(
            "kwiry desktop profile accepts loopback bind addresses only: {address}"
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
    if profile == HostProfile::Desktop && (semantic || config.semantic.enabled) {
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
        profile,
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
    let router = build_router(state, auth, profile);
    let local_address = listener.local_addr()?;
    match identity {
        StartupIdentity::Desktop { token_path } => {
            let connection_path = paths.connection_path();
            let descriptor = ConnectionDescriptor::new(
                local_address,
                token_path.clone(),
                env!("CARGO_PKG_VERSION"),
            );
            write_connection_descriptor(&connection_path, &descriptor)?;
            tracing::info!(
                url = %descriptor.url,
                token_file = %token_path.display(),
                connection_file = %connection_path.display(),
                "kwiry daemon ready"
            );
            println!(
                "kwiry listening on http://{local_address}; bearer token file: {}",
                token_path.display()
            );
        }
        StartupIdentity::OpenClast => {
            tracing::info!(address = %local_address, "kwiry OpenClast sidecar ready");
            println!("kwiry OpenClast sidecar listening on http://{local_address}");
        }
    }

    let server_result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    watcher.shutdown().await;
    let shutdown_result = shutdown_manager(manager_handle, manager_task).await;
    server_result.context("HTTP server failed")?;
    shutdown_result?;
    tracing::info!("kwiry daemon stopped");
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

pub(crate) fn build_router(state: AppState, auth: AuthState, profile: HostProfile) -> Router {
    let mut protected = Router::new().route("/v0/search", post(search));
    if profile == HostProfile::Desktop {
        protected = protected.route("/v0/status", get(status));
    }
    protected = protected
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
    Extension(principal): Extension<Principal>,
    payload: std::result::Result<Json<ApiSearchRequest>, JsonRejection>,
) -> std::result::Result<Json<ApiSearchResponse>, HttpError> {
    if principal.profile != state.profile || !principal.has_scope(Scope::Search) {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "the authenticated principal cannot search this profile",
        ));
    }
    let Json(request) = payload.map_err(|error| {
        HttpError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            error.body_text(),
        )
    })?;
    let semantic_available =
        state.profile == HostProfile::Desktop && state.runtime.semantic_ready();
    request.validate(semantic_available).map_err(|error| {
        let status = match error.code {
            "mode_unavailable" | "cursor_unavailable" => StatusCode::NOT_IMPLEMENTED,
            _ => StatusCode::BAD_REQUEST,
        };
        HttpError::new(status, error.code, error.message)
    })?;
    if request.limit > principal.max_limit {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "limit_exceeded",
            "the requested limit exceeds the capability constraint",
        ));
    }

    let runtime = state.runtime.clone();
    let query = request.q.clone();
    let profile = state.profile;
    let resources = principal.resources.clone();
    // Semantic legs run ONNX inference; keep them off the async executor.
    let hits = tokio::task::spawn_blocking(move || match profile {
        HostProfile::Desktop => match request.mode {
            SearchMode::Lexical => runtime.search_filtered(&query, request.limit, &request.filters),
            SearchMode::Semantic => {
                runtime.search_semantic(&query, request.limit, &request.filters)
            }
            SearchMode::Hybrid => runtime.search_hybrid(&query, request.limit, &request.filters),
        },
        HostProfile::OpenClast => {
            runtime.search_authorized(&query, request.limit, &request.filters, &resources)
        }
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
    if state.profile == HostProfile::OpenClast {
        tracing::info!(
            jti = principal.jti.as_deref().unwrap_or("missing"),
            subject = %subject_digest(&principal.subject),
            actor = %principal.actor,
            resources = principal.resources.len(),
            results = hits.len(),
            "OpenClast search enforced"
        );
    }
    Ok(Json(ApiSearchResponse {
        hits,
        next_cursor: None,
    }))
}

fn subject_digest(subject: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"kwiry-subject-log-v1\0");
    digest.update(subject.as_bytes());
    format!("{:x}", digest.finalize())
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
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    use axum::body::Body;
    use axum::http::Request;
    use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
    use http_body_util::BodyExt;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use kwiry_core::{OpenClastAuthConfig, ResourceKey, VaultRegistration};
    use serde_json::json;
    use tempfile::tempdir;
    use tower::ServiceExt;

    use super::*;

    const ED25519_PRIVATE_DER: &[u8] = &[
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20, 0x6a, 0xc3, 0xfd, 0xee, 0xee, 0x29, 0x8a, 0x92, 0x63, 0x8b, 0x70, 0x0c, 0x4b, 0x11,
        0x7c, 0xc3, 0x2e, 0x2d, 0x2a, 0xce, 0x0d, 0xfd, 0x78, 0x76, 0x94, 0xe2, 0x4c, 0xae, 0x8a,
        0xd5, 0x82, 0x34,
    ];
    const ED25519_JWKS: &str = r#"{"keys":[{
      "kty":"OKP","crv":"Ed25519","x":"2-Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8",
      "use":"sig","key_ops":["verify"],"alg":"EdDSA","kid":"ed01"
    }]}"#;

    fn openclast_auth_config(directory: &Path) -> OpenClastAuthConfig {
        let jwks_file = directory.join("search.jwks.json");
        fs::write(&jwks_file, ED25519_JWKS).unwrap();
        OpenClastAuthConfig {
            tenant_id: "tenant-a".into(),
            issuer: "issuer".into(),
            audience: "kwiry-search".into(),
            jwks_file,
            max_token_ttl_seconds: 60,
        }
    }

    fn search_capability(resource: &ResourceKey, max_limit: usize) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = json!({
            "iss": "issuer",
            "aud": "kwiry-search",
            "sub": "user-a",
            "actor": "openclast-orchestrator",
            "jti": "decision-a",
            "iat": now,
            "nbf": now.saturating_sub(1),
            "exp": now + 60,
            "tenant": "tenant-a",
            "actions": ["search:lexical"],
            "resources": [resource],
            "constraints": { "max_limit": max_limit }
        });
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some("ed01".into());
        encode(
            &header,
            &claims,
            &EncodingKey::from_ed_der(ED25519_PRIVATE_DER),
        )
        .unwrap()
    }

    fn test_state() -> AppState {
        AppState {
            profile: HostProfile::Desktop,
            runtime: SearchRuntime::new(),
            status: Arc::new(RwLock::new(DaemonStatus::starting("0.1.0"))),
        }
    }

    #[tokio::test]
    async fn health_is_public_and_status_requires_authentication() {
        let app = build_router(
            test_state(),
            AuthState::desktop("secret".to_owned()),
            HostProfile::Desktop,
        );
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
        let app = build_router(
            test_state(),
            AuthState::desktop("secret".to_owned()),
            HostProfile::Desktop,
        );
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
    async fn lexical_search_keeps_the_frozen_success_shape() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("note.md"), "# Search\nshapeprobe").unwrap();
        let config = Config {
            vaults: vec![VaultRegistration {
                id: "fixture".into(),
                path: vault,
                room: None,
            }],
            ..Config::default()
        };
        build_index(&config, &data).unwrap();
        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config, &data, runtime.clone()).unwrap();
        let app = build_router(
            AppState {
                profile: HostProfile::Desktop,
                runtime,
                status: Arc::new(RwLock::new(DaemonStatus::starting("0.1.0"))),
            },
            AuthState::desktop("secret".to_owned()),
            HostProfile::Desktop,
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v0/search")
                    .header(AUTHORIZATION, "Bearer secret")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"q":"shapeprobe","mode":"lexical","limit":20}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let response_keys: BTreeSet<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(response_keys, BTreeSet::from(["hits", "next_cursor"]));
        let hit_keys: BTreeSet<_> = value["hits"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            hit_keys,
            BTreeSet::from([
                "chunk_id",
                "excerpt",
                "frontmatter",
                "heading_path",
                "path",
                "score",
                "vault_id",
            ])
        );
        manager.shutdown().unwrap();
    }

    #[tokio::test]
    async fn unavailable_mode_uses_frozen_error_envelope() {
        let app = build_router(
            test_state(),
            AuthState::desktop("secret".to_owned()),
            HostProfile::Desktop,
        );
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

    #[tokio::test]
    async fn openclast_surface_has_no_desktop_auth_or_status_fallback() {
        let temporary = tempdir().unwrap();
        let auth_config = openclast_auth_config(temporary.path());
        let verifier = CapabilityVerifier::load(&auth_config).unwrap();
        let resource = ResourceKey {
            tenant_id: "tenant-a".into(),
            vault_id: "fixture".into(),
            room_id: "room-a".into(),
        };
        let token = search_capability(&resource, 20);
        let app = build_router(
            AppState {
                profile: HostProfile::OpenClast,
                runtime: SearchRuntime::new(),
                status: Arc::new(RwLock::new(DaemonStatus::starting("0.1.0"))),
            },
            AuthState::openclast(verifier),
            HostProfile::OpenClast,
        );

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
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v0/status")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::NOT_FOUND);

        let desktop_token = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v0/search")
                    .header(AUTHORIZATION, "Bearer secret")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"q":"notes","mode":"lexical","limit":20}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(desktop_token.status(), StatusCode::UNAUTHORIZED);

        let semantic = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v0/search")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"q":"notes","mode":"semantic","limit":20}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(semantic.status(), StatusCode::NOT_IMPLEMENTED);
        let body = semantic.into_body().collect().await.unwrap().to_bytes();
        let error: ApiErrorEnvelope = serde_json::from_slice(&body).unwrap();
        assert_eq!(error.error.code, "mode_unavailable");
    }

    #[tokio::test]
    async fn openclast_search_enforces_signed_resources_and_limit() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("note.md"), "# Search\nauthorizedprobe").unwrap();
        let auth_config = openclast_auth_config(temporary.path());
        let resource = ResourceKey {
            tenant_id: "tenant-a".into(),
            vault_id: "fixture".into(),
            room_id: "room-a".into(),
        };
        let mut config = Config::default();
        config.server.profile = HostProfile::OpenClast;
        config.auth.openclast = Some(auth_config.clone());
        config.vaults = vec![VaultRegistration {
            id: resource.vault_id.clone(),
            path: vault,
            room: Some(resource.room_id.clone()),
        }];
        build_index(&config, &data).unwrap();
        let runtime = SearchRuntime::new();
        let manager = IndexManager::open(config, &data, runtime.clone()).unwrap();
        let app = build_router(
            AppState {
                profile: HostProfile::OpenClast,
                runtime,
                status: Arc::new(RwLock::new(DaemonStatus::starting("0.1.0"))),
            },
            AuthState::openclast(CapabilityVerifier::load(&auth_config).unwrap()),
            HostProfile::OpenClast,
        );
        let token = search_capability(&resource, 20);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v0/search")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"q":"authorizedprobe","mode":"lexical","limit":20}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let response: ApiSearchResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].vault_id, "fixture");

        let excessive_limit = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v0/search")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"q":"authorizedprobe","mode":"lexical","limit":21}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(excessive_limit.status(), StatusCode::FORBIDDEN);
        let body = excessive_limit
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let error: ApiErrorEnvelope = serde_json::from_slice(&body).unwrap();
        assert_eq!(error.error.code, "limit_exceeded");
        manager.shutdown().unwrap();
    }
}
