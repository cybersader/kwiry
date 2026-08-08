use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use kwiry_core::{
    ApiErrorEnvelope, ApiSearchRequest, ApiSearchResponse, Config, ConnectionDescriptor,
    DaemonState, DaemonStatus, DataRoot, HealthResponse, HostProfile, IndexFreshness,
    IndexFreshnessBasis, IndexFreshnessState, IndexManager, Manifest, ManifestFileOutcome,
    ModelStatus, Paths, Principal, ReconcileScope, Scope, SearchMode, SearchRuntime, VaultStatus,
    bootstrap_desktop, build_index, load_config, write_connection_descriptor,
};

use crate::auth::{AuthState, require_auth};
use crate::capability::CapabilityVerifier;
use crate::logging::Redacted;
use crate::runtime::{ManagerHandle, spawn_manager};
use crate::watcher::spawn_watcher;

const INDEX_FRESHNESS_HEADER: &str = "x-kwiry-index-freshness";
const GENERATION_HEADER: &str = "x-kwiry-generation";

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
    // A core-identity mismatch discards the whole generation here, and the
    // rebuild that follows is far larger than the per-format eviction reported
    // below — so it must not be the quieter of the two. The gate's message is
    // produced and then classified away inside `prepare`; this is where it is
    // handed back to the operator.
    if let Some(discarded) = data_root.prepare()? {
        let generation = discarded.generation.as_deref().unwrap_or("<unreadable>");
        tracing::warn!(
            generation,
            reason = %discarded.reason,
            "the stored index cannot be reused by this build; rebuilding from scratch"
        );
        println!(
            "kwiry: the stored index cannot be reused by this build ({}); rebuilding every source from scratch",
            discarded.reason
        );
    }

    let runtime = SearchRuntime::new();
    let semantic_enabled = profile == HostProfile::Desktop && (semantic || config.semantic.enabled);
    if semantic_enabled && cfg!(not(feature = "semantic-onnx")) {
        return Err(anyhow!(
            "this build does not include semantic support; rebuild with --features semantic-onnx"
        ));
    }

    let status = Arc::new(RwLock::new(DaemonStatus::starting(env!(
        "CARGO_PKG_VERSION"
    ))));
    let state = AppState {
        profile,
        runtime: runtime.clone(),
        status: status.clone(),
    };
    let router = build_router(state, auth, profile);
    let local_address = listener.local_addr()?;

    // Owner ruling (KWIRY-Q-0022, 2026-07-25): the desktop profile serves
    // immediately — searches answered from the last complete generation are
    // labeled stale, and a daemon with no generation yet returns the typed
    // 503 index_building. OpenClast keeps boot-before-serve until its
    // layout-vs-config classification equality check exists.
    let mut serving = None;
    let mut pending_server = None;
    if profile == HostProfile::Desktop {
        serving = Some(spawn_server(listener, router));
    } else {
        pending_server = Some((listener, router));
    }

    let (manager, freshly_built) = match boot_index(&config, &paths, &runtime).await {
        Ok(bootstrapped) => bootstrapped,
        Err(error) => {
            let Some(server) = serving else {
                // OpenClast is not serving yet; fail fast exactly as before.
                return Err(error);
            };
            // Desktop keeps the surface alive: health answers, search
            // returns the typed index_building state, and status says
            // degraded until an operator fixes registration and the
            // daemon is restarted.
            tracing::warn!(
                error = %error,
                "index bootstrap failed; serving without an index"
            );
            {
                let mut current = status.write().await;
                current.state = DaemonState::Degraded;
                current.dirty = true;
                current.rebuilding = false;
            }
            println!("kwiry listening on http://{local_address}; index unavailable: {error}");
            let server_result = server.await;
            server_result.context("HTTP server task failed")??;
            tracing::info!("kwiry daemon stopped");
            return Ok(());
        }
    };

    // Open-time eviction removed rows whose per-format identity moved. It is a
    // narrowing, not a silent one: the formats it touched are named here so the
    // reindex that follows is explicable rather than mysterious.
    {
        let evictions = manager.open_evictions();
        if !evictions.is_empty() {
            let formats = evictions
                .by_format
                .iter()
                .map(|(format, count)| format!("{}={count}", format.as_str()))
                .collect::<Vec<_>>()
                .join(", ");
            tracing::warn!(
                sources = evictions.total(),
                formats = %formats,
                "extraction identity changed; evicted those formats' rows and will reindex their sources"
            );
            println!(
                "kwiry: extraction identity changed for {formats}; {} source(s) evicted and will be reindexed",
                evictions.total()
            );
        }
    }

    // Seed observable counts from the manifest while the boot pass runs.
    // status.generation stays None until the boot pass finishes: a served
    // previous generation must read as stale, never current.
    let boot_manifest = manager.manifest().clone();
    {
        let mut seeded = status_from_manifest(&config, &boot_manifest, &[], None);
        seeded.state = DaemonState::Starting;
        seeded.generation = None;
        seeded.dirty = true;
        seeded.rebuilding = !freshly_built;
        *status.write().await = seeded;
    }

    let (manager_handle, manager_task) = spawn_manager(manager);
    // The watcher is armed before the boot pass so nothing observed while
    // it runs is lost; the manager actor serializes the passes.
    let watcher = spawn_watcher(
        paths.clone(),
        config.clone(),
        manager_handle.clone(),
        status.clone(),
    )?;
    if semantic_enabled {
        // Owner ruling (KWIRY-Q-0022): the embedding model loads in the
        // background; semantic and hybrid answer an honest mode_unavailable
        // until it is ready, then a backfill pass embeds the corpus.
        spawn_semantic_loader(
            paths.clone(),
            runtime.clone(),
            status.clone(),
            manager_handle.clone(),
            config.clone(),
        );
    }

    // Boot reconciliation catches offline changes against a pre-existing
    // generation. A generation built moments ago in this same process has
    // none, so it publishes directly as current.
    if freshly_built {
        let next = status_with_model(
            status_from_manifest(&config, &boot_manifest, &[], runtime.generation()),
            &status,
        )
        .await;
        *status.write().await = next;
    } else {
        match manager_handle
            .reconcile_scoped(config.clone(), ReconcileScope::Full)
            .await
        {
            Ok(report) => {
                let next = status_with_model(
                    status_from_manifest(
                        &config,
                        &report.manifest,
                        &report.unavailable_vaults,
                        report.generation,
                    ),
                    &status,
                )
                .await;
                *status.write().await = next;
            }
            // A failed boot pass must not kill a daemon holding a complete
            // serviceable generation: serve it, report degraded/stale, and
            // let the watcher's retry and safety passes recover.
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "boot reconciliation failed; serving the last complete generation as degraded"
                );
                let mut next = status_with_model(
                    status_from_manifest(&config, &boot_manifest, &[], None),
                    &status,
                )
                .await;
                next.state = DaemonState::Degraded;
                next.dirty = true;
                next.rebuilding = false;
                *status.write().await = next;
            }
        }
    }

    if let Some((listener, router)) = pending_server {
        serving = Some(spawn_server(listener, router));
    }
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

    let server_result = serving
        .expect("the HTTP server is started in every non-degraded path")
        .await;
    watcher.shutdown().await;
    let shutdown_result = shutdown_manager(manager_handle, manager_task).await;
    server_result.context("HTTP server task failed")??;
    shutdown_result?;
    tracing::info!("kwiry daemon stopped");
    Ok(())
}

fn spawn_server(
    listener: TcpListener,
    router: Router,
) -> tokio::task::JoinHandle<std::result::Result<(), std::io::Error>> {
    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal())
            .await
    })
}

/// Builds the first generation when none exists and opens the manager,
/// off the async executor because both are blocking filesystem work.
async fn boot_index(
    config: &Config,
    paths: &Paths,
    runtime: &SearchRuntime,
) -> Result<(IndexManager, bool)> {
    let config = config.clone();
    let data_dir = paths.data_dir.clone();
    let runtime = runtime.clone();
    tokio::task::spawn_blocking(move || -> Result<(IndexManager, bool)> {
        let data_root = DataRoot::new(&data_dir);
        let freshly_built = if data_root.active()?.is_none() {
            build_index(&config, &data_dir)?;
            true
        } else {
            false
        };
        let manager = IndexManager::open(config, &data_dir, runtime)?;
        Ok((manager, freshly_built))
    })
    .await
    .context("index bootstrap task failed")?
}

async fn status_with_model(
    mut next: DaemonStatus,
    status: &Arc<RwLock<DaemonStatus>>,
) -> DaemonStatus {
    // The model identity is owned by the semantic loader.
    next.model = status.read().await.model.clone();
    next
}

fn spawn_semantic_loader(
    paths: Paths,
    runtime: SearchRuntime,
    status: Arc<RwLock<DaemonStatus>>,
    manager: ManagerHandle,
    config: Config,
) {
    tokio::spawn(async move {
        let loaded = tokio::task::spawn_blocking({
            let paths = paths.clone();
            let runtime = runtime.clone();
            move || install_semantic(&paths, &runtime)
        })
        .await;
        match loaded {
            Ok(Ok(())) => {
                if let Some(profile) = runtime.semantic_profile() {
                    status.write().await.model = Some(ModelStatus {
                        name: profile.model_id.clone(),
                        version: profile.fingerprint(),
                    });
                }
                // Backfill embeddings for the already-committed lexical
                // corpus; failures stay warnings and never degrade lexical.
                if let Err(error) = manager.reconcile_scoped(config, ReconcileScope::Full).await {
                    tracing::warn!(
                        error = %error,
                        "semantic backfill reconciliation failed; lexical unaffected"
                    );
                }
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    error = %error,
                    "semantic model load failed; semantic and hybrid stay explicitly unavailable"
                );
            }
            Err(error) => {
                tracing::warn!(error = %error, "semantic loader task failed");
            }
        }
    });
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
) -> std::result::Result<Response, HttpError> {
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
    let result = tokio::task::spawn_blocking(move || match profile {
        HostProfile::Desktop => match request.mode {
            SearchMode::Lexical => {
                runtime.search_filtered_with_generation(&query, request.limit, &request.filters)
            }
            SearchMode::Semantic => {
                runtime.search_semantic_with_generation(&query, request.limit, &request.filters)
            }
            SearchMode::Hybrid => {
                runtime.search_hybrid_with_generation(&query, request.limit, &request.filters)
            }
        },
        HostProfile::OpenClast => runtime.search_authorized_with_generation(
            &query,
            request.limit,
            &request.filters,
            &resources,
        ),
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
            results = result.hits.len(),
            "OpenClast search enforced"
        );
    }

    let status = state.status.read().await;
    let freshness =
        response_freshness(&status, &result.generation, state.runtime.freshness_basis());
    let mut response = Json(ApiSearchResponse {
        hits: result.hits,
        next_cursor: None,
        extraction_policy_fingerprint: kwiry_core::extraction_policy_fingerprint().to_owned(),
    })
    .into_response();
    response.headers_mut().insert(
        INDEX_FRESHNESS_HEADER,
        HeaderValue::from_static(freshness.header_value()),
    );
    response.headers_mut().insert(
        GENERATION_HEADER,
        HeaderValue::from_str(&result.generation).map_err(|_| {
            HttpError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "the search generation could not be represented safely",
            )
        })?,
    );
    Ok(response)
}

fn response_freshness(
    status: &DaemonStatus,
    generation: &str,
    basis: IndexFreshnessBasis,
) -> IndexFreshness {
    let state = if status.generation.as_deref() != Some(generation) {
        IndexFreshnessState::Stale
    } else if status.rebuilding {
        IndexFreshnessState::Reconciling
    } else if status.dirty {
        IndexFreshnessState::Stale
    } else {
        IndexFreshnessState::Current
    };
    IndexFreshness::new(state, basis)
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
        kwiry_core::Error::IndexBuilding => HttpError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "index_building",
            "the index is still building",
        ),
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
        extraction_policy_fingerprint: kwiry_core::extraction_policy_fingerprint().to_owned(),
        extraction_policy: kwiry_core::active_extraction_policy(),
        format_identities: kwiry_core::owned_format_identities(),
        documents: manifest.document_count(),
        chunks: manifest.chunk_count(),
        source_format_counts: manifest.source_format_counts(),
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

    #[test]
    fn freshness_tracks_the_generation_and_reconciliation_state() {
        let mut status = DaemonStatus::starting("0.1.0");
        status.generation = Some("generation-a".to_owned());
        status.dirty = false;
        assert_eq!(
            response_freshness(&status, "generation-a", IndexFreshnessBasis::StrictHash)
                .header_value(),
            "current; basis=strict_hash"
        );
        assert_eq!(
            response_freshness(&status, "generation-a", IndexFreshnessBasis::MetadataAudit)
                .header_value(),
            "current; basis=metadata_audit"
        );

        status.rebuilding = true;
        status.dirty = true;
        assert_eq!(
            response_freshness(&status, "generation-a", IndexFreshnessBasis::StrictHash)
                .header_value(),
            "reconciling; basis=strict_hash"
        );

        status.rebuilding = false;
        assert_eq!(
            response_freshness(&status, "generation-a", IndexFreshnessBasis::StrictHash)
                .header_value(),
            "stale; basis=strict_hash"
        );
        assert_eq!(
            response_freshness(&status, "generation-b", IndexFreshnessBasis::StrictHash)
                .header_value(),
            "stale; basis=strict_hash"
        );
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
        let generation = runtime.generation().unwrap();
        let mut status = DaemonStatus::starting("0.1.0");
        status.state = DaemonState::Ready;
        status.generation = Some(generation.clone());
        status.dirty = false;
        let app = build_router(
            AppState {
                profile: HostProfile::Desktop,
                runtime,
                status: Arc::new(RwLock::new(status)),
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
        assert_eq!(
            response.headers().get(INDEX_FRESHNESS_HEADER).unwrap(),
            "current; basis=strict_hash"
        );
        assert_eq!(
            response.headers().get(GENERATION_HEADER).unwrap(),
            generation.as_str()
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let response_keys: BTreeSet<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        // The envelope gains exactly one field: the policy identity of the
        // index these hits came out of. Deliberately not per-hit — an index is
        // a single-profile artifact, so a per-hit copy could only repeat itself.
        assert_eq!(
            response_keys,
            BTreeSet::from(["extraction_policy_fingerprint", "hits", "next_cursor"])
        );
        assert_eq!(
            value["extraction_policy_fingerprint"],
            kwiry_core::extraction_policy_fingerprint()
        );
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
                "coverage",
                "excerpt",
                "format",
                "frontmatter",
                "heading_path",
                "locator",
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
    async fn missing_generation_returns_typed_index_building_error() {
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
                    .body(Body::from(r#"{"q":"notes","mode":"lexical","limit":20}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let error: ApiErrorEnvelope = serde_json::from_slice(&body).unwrap();
        assert_eq!(error.error.code, "index_building");
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
        let generation = runtime.generation().unwrap();
        let mut status = DaemonStatus::starting("0.1.0");
        status.state = DaemonState::Ready;
        status.generation = Some(generation.clone());
        status.dirty = false;
        let app = build_router(
            AppState {
                profile: HostProfile::OpenClast,
                runtime,
                status: Arc::new(RwLock::new(status)),
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
        assert_eq!(
            response.headers().get(INDEX_FRESHNESS_HEADER).unwrap(),
            "current; basis=strict_hash"
        );
        assert_eq!(
            response.headers().get(GENERATION_HEADER).unwrap(),
            generation.as_str()
        );
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
