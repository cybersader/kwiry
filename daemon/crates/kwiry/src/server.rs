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
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use kwiry_core::{
    ApiErrorEnvelope, ApiSearchRequest, ApiSearchResponse, Config, ConnectionDescriptor,
    DaemonState, DaemonStatus, DataRoot, HealthResponse, HostProfile, IndexFreshness,
    IndexFreshnessBasis, IndexFreshnessState, IndexManager, LexicalMatchQuality, Manifest,
    ManifestFileOutcome, ModelStatus, Paths, Principal, ReconcileScope, Scope, SearchMode,
    SearchRuntime, VaultStatus, bootstrap_desktop, build_index, load_config_for_paths,
    probe_openclast_ingestion, write_connection_descriptor,
};

use crate::auth::{AuthState, require_auth};
use crate::capability::{CapabilityVerifier, ROOM_FORMAT};
use crate::logging::Redacted;
use crate::runtime::{ManagerHandle, spawn_manager};
use crate::watcher::spawn_watcher;

const INDEX_FRESHNESS_HEADER: &str = "x-kwiry-index-freshness";
const GENERATION_HEADER: &str = "x-kwiry-generation";
const LEXICAL_PROFILE_HEADER: &str = "x-kwiry-lexical-profile";
const LEXICAL_MATCH_QUALITY_HEADER: &str = "x-kwiry-lexical-match-quality";
const FIELD_SCOPE_HEADER: &str = "x-kwiry-field-scope";
const FIELD_EMPHASIS_HEADER: &str = "x-kwiry-field-emphasis";

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
    let initial_config = load_config_for_paths(&paths)?;
    let profile = initial_config.server.profile;
    if profile == HostProfile::OpenClast {
        probe_openclast_ingestion(&paths.data_dir, &initial_config.vaults)?;
    }
    let _logging = crate::logging::init(&paths.logs_dir())?;
    tracing::info!(
        config = %paths.config.display(),
        data_dir = %paths.data_dir.display(),
        "starting kwiry daemon"
    );
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
            let verifier = CapabilityVerifier::load(auth_config, &paths.data_dir)
                .map_err(|message| anyhow!(message))?;
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
    if profile == HostProfile::OpenClast {
        runtime.set_openclast_registrations(&config);
    }
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

    let (manager_handle, manager_task) = spawn_manager(manager, runtime.clone());
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
    if freshly_built && config.server.profile == HostProfile::Desktop {
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
            // OpenClast must not serve an inherited generation unless the
            // boot pass checked its registrations and ingestion boundaries.
            Err(error) if profile == HostProfile::OpenClast => return Err(error.into()),
            // Desktop can serve its prior generation as degraded/stale.
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
        let can_rebuild_identity = data_root.needs_room_identity_rebuild()?;
        let freshly_built = if can_rebuild_identity || data_root.active()?.is_none() {
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
        .route_layer(middleware::from_fn_with_state(
            auth.with_runtime(state.runtime.clone()),
            require_auth,
        ));
    if profile == HostProfile::OpenClast {
        protected = protected.route_layer(middleware::from_fn(room_format_headers));
    }
    Router::new()
        .route("/v0/health", get(health))
        .merge(protected)
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(not_found)
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Response {
    if state.profile == HostProfile::OpenClast {
        return (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            ROOM_FORMAT.health_body(),
        )
            .into_response();
    }
    Json(HealthResponse::default()).into_response()
}

async fn room_format_headers(request: axum::extract::Request, next: middleware::Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("kwiry-id-scheme", HeaderValue::from_static("room-v1"));
    headers.insert(
        "kwiry-multi-room",
        HeaderValue::from_static(ROOM_FORMAT.multi_room_header()),
    );
    response
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
    let prepared_query = request
        .prepare_query()
        .map_err(|error| HttpError::new(StatusCode::BAD_REQUEST, error.code, error.message))?;
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
    let mode = request.mode;
    let limit = request.limit;
    let filters = request.filters.clone();
    let policy = prepared_query.policy.clone();
    let lexical_query = prepared_query.lexical_query;
    let semantic_query = prepared_query.semantic_query;
    let profile = state.profile;
    let resources = principal.resources.clone();
    // Semantic legs run ONNX inference; keep them off the async executor.
    let result = tokio::task::spawn_blocking(move || match profile {
        HostProfile::Desktop => match mode {
            SearchMode::Lexical => {
                runtime.search_filtered_with_generation(&lexical_query, limit, &filters)
            }
            SearchMode::Semantic => {
                runtime.search_semantic_with_generation(&semantic_query, limit, &filters)
            }
            SearchMode::Hybrid => runtime.search_hybrid_prepared_with_generation(
                &lexical_query,
                &semantic_query,
                limit,
                &filters,
            ),
        },
        HostProfile::OpenClast => {
            runtime.search_authorized_with_generation(&lexical_query, limit, &filters, &resources)
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
            resources = principal.resources.len(),
            results = result.hits.len(),
            "OpenClast search enforced"
        );
    }

    let freshness = match state.profile {
        HostProfile::Desktop => {
            let status = state.status.read().await;
            response_freshness(&status, &result.generation, state.runtime.freshness_basis())
        }
        HostProfile::OpenClast => state
            .runtime
            .authorized_freshness(&request.filters, &principal.resources, &result.generation)
            .map_err(map_core_error)?,
    };
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
    let lexical_profile = if mode == SearchMode::Semantic {
        "none"
    } else {
        policy.profile_id.as_str()
    };
    response.headers_mut().insert(
        LEXICAL_PROFILE_HEADER,
        policy_header_value(lexical_profile)?,
    );
    let lexical_match_quality = lexical_match_quality_header(mode, result.lexical_match_quality)?;
    response.headers_mut().insert(
        LEXICAL_MATCH_QUALITY_HEADER,
        policy_header_value(lexical_match_quality)?,
    );
    response.headers_mut().insert(
        FIELD_SCOPE_HEADER,
        policy_header_value(policy.scope.map_or("none", |field| field.as_str()))?,
    );
    response.headers_mut().insert(
        FIELD_EMPHASIS_HEADER,
        policy_header_value(policy.emphasis.map_or("none", |field| field.as_str()))?,
    );
    Ok(response)
}

fn lexical_match_quality_header(
    mode: SearchMode,
    quality: Option<LexicalMatchQuality>,
) -> std::result::Result<&'static str, HttpError> {
    match (mode, quality) {
        (SearchMode::Lexical, Some(LexicalMatchQuality::StandardOnly)) => Ok("standard_only"),
        (SearchMode::Lexical, Some(LexicalMatchQuality::Mixed)) => Ok("mixed"),
        (SearchMode::Lexical, Some(LexicalMatchQuality::PartialOnly)) => Ok("partial_only"),
        (SearchMode::Lexical, Some(LexicalMatchQuality::None)) => Ok("none"),
        (SearchMode::Lexical, None) => Err(HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "the lexical match quality was unavailable",
        )),
        (SearchMode::Semantic | SearchMode::Hybrid, _) => Ok("not_applicable"),
    }
}

fn policy_header_value(value: &str) -> std::result::Result<HeaderValue, HttpError> {
    HeaderValue::from_str(value).map_err(|_| {
        HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "the effective query policy could not be represented safely",
        )
    })
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
        kwiry_core::Error::UnavailableResourceIndexes(indexes) => {
            let mut error = map_core_error(kwiry_core::Error::ResourceUnavailable);
            error.body.error.resource_indexes = Some(indexes);
            error
        }
        kwiry_core::Error::ResourceUnavailable => HttpError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "resource_unavailable",
            "an authorized resource is not available for search",
        ),
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
            tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
            issuer: "issuer".into(),
            audience: "kwiry-search".into(),
            jwks_file,
            max_token_ttl_seconds: 60,
            workload_subject_allowlist: Vec::new(),
            legacy_identity_accept_until: None,
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
            "tenant": "tenant_AAAAAAAAAAAAAAAAAAAA",
            "actions": ["search:lexical"],
            "resources": [resource],
            "constraints": { "max_limit": max_limit }
        });
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some("ed01".into());
        header.typ = None;
        encode(
            &header,
            &claims,
            &EncodingKey::from_ed_der(ED25519_PRIVATE_DER),
        )
        .unwrap()
    }

    fn assert_open_room_headers(response: &Response) {
        let headers = response.headers();
        assert_eq!(
            headers
                .get_all("kwiry-id-scheme")
                .iter()
                .collect::<Vec<_>>(),
            ["room-v1"]
        );
        assert_eq!(
            headers
                .get_all("kwiry-multi-room")
                .iter()
                .collect::<Vec<_>>(),
            ["open"]
        );
    }

    #[cfg(target_os = "linux")]
    async fn assert_open_health(app: &Router) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v0/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            r#"{"status":"ok","id_scheme":"room-v1","multi_room":"open"}"#
        );
    }

    fn test_state() -> AppState {
        AppState {
            profile: HostProfile::Desktop,
            runtime: SearchRuntime::new(),
            status: Arc::new(RwLock::new(DaemonStatus::starting("0.1.0"))),
        }
    }

    #[test]
    fn unavailable_authorized_resource_is_an_explicit_nonrevealing_503() {
        let error = map_core_error(kwiry_core::Error::ResourceUnavailable);
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.body.error.code, "resource_unavailable");
        assert!(!error.body.error.message.contains("room"));
    }

    #[tokio::test]
    async fn unavailable_resource_indexes_are_exact_and_nonrevealing() {
        let response = map_core_error(kwiry_core::Error::UnavailableResourceIndexes(vec![0, 2]))
            .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            r#"{"error":{"code":"resource_unavailable","message":"an authorized resource is not available for search","resource_indexes":[0,2]}}"#
        );
    }

    #[test]
    fn lexical_match_quality_header_is_closed_and_mode_aware() {
        fn value(mode: SearchMode, quality: Option<LexicalMatchQuality>) -> &'static str {
            let Ok(value) = lexical_match_quality_header(mode, quality) else {
                panic!("valid match quality must map to a header value");
            };
            value
        }

        for (quality, expected) in [
            (LexicalMatchQuality::StandardOnly, "standard_only"),
            (LexicalMatchQuality::Mixed, "mixed"),
            (LexicalMatchQuality::PartialOnly, "partial_only"),
            (LexicalMatchQuality::None, "none"),
        ] {
            assert_eq!(value(SearchMode::Lexical, Some(quality)), expected);
        }
        assert_eq!(value(SearchMode::Semantic, None), "not_applicable");
        assert_eq!(
            value(SearchMode::Hybrid, Some(LexicalMatchQuality::PartialOnly)),
            "not_applicable"
        );
        let Err(error) = lexical_match_quality_header(SearchMode::Lexical, None) else {
            panic!("missing lexical quality must fail closed");
        };
        assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error.body.error.code, "internal_error");
    }

    #[test]
    fn freshness_tracks_the_generation_and_reconciliation_state() {
        assert!(
            Principal::desktop().resources.is_empty(),
            "desktop must take its explicit profile branch even with resources: []"
        );
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
                        r#"{"q":">body shapeprobe","mode":"lexical","limit":20}"#,
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
        assert_eq!(
            response.headers().get(LEXICAL_PROFILE_HEADER).unwrap(),
            "lexical-v2"
        );
        assert_eq!(
            response
                .headers()
                .get(LEXICAL_MATCH_QUALITY_HEADER)
                .unwrap(),
            "standard_only"
        );
        assert_eq!(response.headers().get(FIELD_SCOPE_HEADER).unwrap(), "none");
        assert_eq!(
            response.headers().get(FIELD_EMPHASIS_HEADER).unwrap(),
            "body"
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
    async fn lexical_search_discloses_partial_only_best_attempts_in_a_header() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("target.md"), "amberstone birchwood dunefield").unwrap();
        fs::write(vault.join("support.md"), "cedarleaf").unwrap();
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
        status.generation = Some(generation);
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
                        r#"{"q":"amberstone birchwood cedarleaf dunefield","mode":"lexical","limit":20}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(LEXICAL_MATCH_QUALITY_HEADER)
                .unwrap(),
            "partial_only"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let response: ApiSearchResponse = serde_json::from_slice(&body).unwrap();
        // The bounded exploratory partial pass surfaces every candidate that
        // carries at least one optional term, not only the best-attempt
        // preselected winner: target.md (three of the four terms) and
        // support.md (one term) both surface, with more overlap ranking
        // first.
        assert_eq!(response.hits.len(), 2);
        assert_eq!(response.hits[0].path, "target.md");
        assert_eq!(response.hits[1].path, "support.md");
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
    async fn field_controls_are_rejected_by_mode_before_mode_availability() {
        let app = build_router(
            test_state(),
            AuthState::desktop("secret".to_owned()),
            HostProfile::Desktop,
        );
        for body in [
            r#"{"q":"in:title Vendor7 meeting","mode":"hybrid","limit":20}"#,
            r#"{"q":">body Vendor7 meeting","mode":"semantic","limit":20}"#,
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v0/search")
                        .header(AUTHORIZATION, "Bearer secret")
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let error: ApiErrorEnvelope = serde_json::from_slice(&body).unwrap();
            assert_eq!(error.error.code, "invalid_field_control");
        }
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

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[tokio::test]
    async fn openclast_surface_has_no_desktop_auth_or_status_fallback() {
        let temporary = tempdir().unwrap();
        let auth_config = openclast_auth_config(temporary.path());
        let verifier = CapabilityVerifier::load(&auth_config, temporary.path()).unwrap();
        let resource = ResourceKey {
            tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
            vault_id: "VaultOne_0123456789abc".into(),
            room_id: "RoomAaaa_0123456789abc".into(),
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
        assert_eq!(
            health.into_body().collect().await.unwrap().to_bytes(),
            r#"{"status":"ok","id_scheme":"room-v1","multi_room":"open"}"#
        );

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
        assert_open_room_headers(&desktop_token); // N32: 401 inside admission component

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

    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
    )]
    #[tokio::test]
    async fn openclast_search_enforces_signed_resources_and_limit() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        let data = temporary.path().join("data");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("note.md"), "# Search\nauthorizedprobe").unwrap();
        let auth_config = openclast_auth_config(temporary.path());
        let resource = ResourceKey {
            tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
            vault_id: "VaultOne_0123456789abc".into(),
            room_id: "RoomAaaa_0123456789abc".into(),
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
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        manager.reconcile(config).unwrap();
        let generation = runtime
            .search_authorized_with_generation(
                "authorizedprobe",
                20,
                &kwiry_core::SearchFilters::default(),
                std::slice::from_ref(&resource),
            )
            .unwrap()
            .generation;
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
            AuthState::openclast(CapabilityVerifier::load(&auth_config, temporary.path()).unwrap()),
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
        assert_eq!(response.headers()["kwiry-id-scheme"], "room-v1");
        assert_eq!(
            response.headers().get(INDEX_FRESHNESS_HEADER).unwrap(),
            "current; basis=strict_hash"
        );
        assert_eq!(
            response.headers().get(GENERATION_HEADER).unwrap(),
            generation.as_str()
        );
        assert_eq!(
            response.headers().get(LEXICAL_PROFILE_HEADER).unwrap(),
            "lexical-v2"
        );
        assert_eq!(
            response
                .headers()
                .get(LEXICAL_MATCH_QUALITY_HEADER)
                .unwrap(),
            "standard_only"
        );
        assert_eq!(response.headers().get(FIELD_SCOPE_HEADER).unwrap(), "none");
        assert_eq!(
            response.headers().get(FIELD_EMPHASIS_HEADER).unwrap(),
            "none"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let response: ApiSearchResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].vault_id, resource.vault_id);
        assert_eq!(
            response.hits[0].room_id.as_deref(),
            Some(resource.room_id.as_str())
        );

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
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn component_holdings_format_p1_to_p4_p9_p15_p17_p18_n32() {
        let directory = tempdir().unwrap();
        let data = directory.path().join("data");
        let mut config = Config::default();
        config.server.profile = HostProfile::OpenClast;
        let mut auth_config = openclast_auth_config(directory.path());
        auth_config.legacy_identity_accept_until = Some(0);
        auth_config.workload_subject_allowlist = vec!["workload:synthetic-search".into()];
        config.auth.openclast = Some(auth_config.clone());
        for index in 0..256 {
            let path = directory.path().join(format!("room-{index}"));
            fs::create_dir(&path).unwrap();
            if index < 7 {
                fs::write(
                    path.join("index.md"),
                    format!("shared synthetic room {index}"),
                )
                .unwrap();
            }
            let (vault, room) = match index {
                0 => (
                    "VaultOne_0123456789abc".into(),
                    "RoomAaaa_0123456789abc".into(),
                ),
                1 => (
                    "VaultOne_0123456789abc".into(),
                    "RoomBbbb_0123456789abc".into(),
                ),
                2 => (
                    "VaultTwo_0123456789abc".into(),
                    "RoomDddd_0123456789abc".into(),
                ),
                3 => (
                    "VaultOne_0123456789abc".into(),
                    "roomAaaa_0123456789abc".into(),
                ),
                4 => (
                    "VaultOne_0123456789abc".into(),
                    "RoomXxxx_0123456789abc".into(),
                ),
                5 => (
                    "VaultOne_0123456789abc".into(),
                    "RoomXxxx_0123456789abd".into(),
                ),
                _ => ("VaultTwo_0123456789abc".into(), format!("Room_{index:017}")),
            };
            config.vaults.push(VaultRegistration {
                id: vault,
                path,
                room: Some(room),
            });
        }
        let resources: Vec<_> = config
            .vaults
            .iter()
            .map(|vault| config.resource_key(vault).unwrap())
            .collect();
        build_index(&config, &data).unwrap();
        let runtime = SearchRuntime::new();
        runtime.set_openclast_registrations(&config);
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        manager.reconcile(config.clone()).unwrap();
        let status = Arc::new(RwLock::new(DaemonStatus::starting("0.1.0")));
        let verifier = CapabilityVerifier::load(&auth_config, &data).unwrap();
        let epoch: u64 = fs::read_to_string(data.join("replay-epoch"))
            .unwrap()
            .parse()
            .unwrap();
        let app = build_router(
            AppState {
                profile: HostProfile::OpenClast,
                runtime: runtime.clone(),
                status: status.clone(),
            },
            AuthState::openclast(verifier),
            HostProfile::OpenClast,
        );
        assert_open_health(&app).await; // P17: populated generation
        status.write().await.rebuilding = true;
        assert_open_health(&app).await; // P17: rebuilding, not a production Holdings integration
        let iat = loop {
            let now = crate::capability::unix_time().unwrap();
            if now > epoch {
                break now;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        let sign = |set: &[ResourceKey], jti: usize| {
            let mut set = set.to_vec();
            set.sort();
            let claims = json!({
                "iss": "issuer", "aud": "kwiry-search", "sub": "workload:synthetic-search",
                "jti": format!("component-{jti}"), "iat": iat, "nbf": iat, "exp": iat + 60,
                "tenant": "tenant_AAAAAAAAAAAAAAAAAAAA", "actions": ["search:lexical"],
                "resources": set, "constraints": {"max_limit": 20},
            });
            let mut header = Header::new(Algorithm::EdDSA);
            header.kid = Some("ed01".into());
            header.typ = Some("kwiry-cap+jwt".into());
            encode(
                &header,
                &claims,
                &EncodingKey::from_ed_der(ED25519_PRIVATE_DER),
            )
            .unwrap()
        };
        let request = |set: &[ResourceKey], jti, limit| {
            Request::builder()
                .method("POST")
                .uri("/v0/search")
                .header(AUTHORIZATION, format!("Bearer {}", sign(set, jti)))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"q":"shared", "mode":"lexical", "limit":limit}).to_string(),
                ))
                .unwrap()
        };
        for (jti, set, hits) in [
            (1, resources[..1].to_vec(), 1),
            (2, resources[..2].to_vec(), 2),
            (3, resources[..3].to_vec(), 3),
            (4, resources.clone(), 7),
            (9, vec![resources[0].clone(), resources[3].clone()], 2),
            (15, resources[4..=5].to_vec(), 2),
        ] {
            let response = app.clone().oneshot(request(&set, jti, 20)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "P{jti}");
            assert_open_room_headers(&response);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let response: ApiSearchResponse = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(response.hits.len(), hits);
            let ids: std::collections::BTreeSet<_> =
                response.hits.iter().map(|hit| &hit.chunk_id).collect();
            assert_eq!(ids.len(), hits); // P2 ties verifier admission to the real id derivation.
            for hit in response.hits {
                assert!(set.iter().any(|key| hit.vault_id == key.vault_id
                    && hit.room_id.as_ref() == Some(&key.room_id)));
                assert_eq!(hit.path, "index.md");
            }
        }
        let response = app
            .clone()
            .oneshot(request(&resources[..2], 40, 21))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_open_room_headers(&response);
        // R17: removing a registration refuses the whole set even while the
        // prior generation still has that partition and a hit from another room.
        config.vaults.remove(1);
        runtime.set_openclast_registrations(&config);
        let response = app.oneshot(request(&resources[..2], 41, 20)).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_open_room_headers(&response);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            r#"{"error":{"code":"resource_unavailable","message":"an authorized resource is not available for search","resource_indexes":[1]}}"#
        );
        manager.shutdown().unwrap();
    }
}
