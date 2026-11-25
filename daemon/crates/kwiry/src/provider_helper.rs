//! Experimental provider-helper mode (trial only): the search provider
//! contract (DRAFT v1, wire form §2.3) as HTTP/1.1 + JSON on a local Unix
//! domain socket, in front of `kwiry_core::ProviderStore`.
//!
//! Desktop and non-governed. No TCP listener, no outbound network, no
//! secrets, no workspace reads: the helper indexes only the bytes a host
//! pushes. Every name the contract may still rename (routes, headers, JSON
//! keys and shapes, error tokens and their HTTP statuses) lives in [`wire`];
//! the store speaks in Rust types and enums and knows none of them. The one
//! exception is the exclusion reason classes `status` counts by (for example
//! `over_transport_cap`), which are Kwiry's own and come from the store.
//! See `docs/provider-helper-trial.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::connect_info::{ConnectInfo, Connected};
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::serve::IncomingStream;
use kwiry_core::{
    PROVIDER_MATCHER, PROVIDER_MAX_ITEM_BYTES, ProviderError, ProviderErrorCode, ProviderQuery,
    ProviderStore,
};
use serde_json::{Value, json};
use tokio::net::UnixListener;

/// Everything the partner's contract may rename or reshape.
pub(crate) mod wire {
    use axum::http::StatusCode;
    use kwiry_core::{
        PROVIDER_MATCHER, PROVIDER_MATCHER_UNICODE_VERSION, PROVIDER_MAX_ITEM_BYTES,
        PROVIDER_MAX_LIMIT, PROVIDER_MAX_QUERY_PARTITIONS, ProviderAsOf, ProviderErrorCode,
        ProviderFeedMode, ProviderKind, ProviderKindStatus, ProviderMode, ProviderQueryOutcome,
        ProviderState, ProviderStatus,
    };
    use serde::Deserialize;
    use serde_json::{Value, json};

    pub const CONTRACT: &str = "search-provider/1";
    pub const PROFILE_HEADER: &str = "search-provider-profile";
    pub const PROFILE: &str = "v1";
    pub const HEADER_ITEM_VERSION: &str = "provider-item-version";
    pub const HEADER_ITEM_PATH: &str = "provider-item-path";

    pub const ROUTE_HELLO: &str = "/v1/hello";
    pub const ROUTE_QUERY: &str = "/v1/query";
    pub const ROUTE_PARTITION: &str = "/v1/partitions/{partition}";
    pub const ROUTE_BEGIN: &str = "/v1/partitions/{partition}/begin";
    pub const ROUTE_ITEM: &str = "/v1/partitions/{partition}/items/{id}";
    pub const ROUTE_CHECKPOINT: &str = "/v1/partitions/{partition}/checkpoint";
    pub const ROUTE_COMMIT: &str = "/v1/partitions/{partition}/commit";
    pub const ROUTE_STATUS: &str = "/v1/partitions/{partition}/status";

    /// Largest JSON request body (§2.3: a 4,096-byte query plus 128 terms fits).
    pub const MAX_JSON_BODY_BYTES: usize = 65_536;
    /// Search modes. `semantic` and `hybrid` are offered only with a loaded
    /// model; anything not offered is `mode_unavailable`.
    pub const LEXICAL: &str = "lexical";
    pub const SEMANTIC: &str = "semantic";
    pub const HYBRID: &str = "hybrid";
    pub const DEFAULT_DEADLINE_MS: u64 = 10_000;
    pub const MAX_DEADLINE_MS: u64 = 60_000;

    pub fn retrieval(semantic: bool) -> Vec<&'static str> {
        if semantic {
            vec![LEXICAL, SEMANTIC, HYBRID]
        } else {
            vec![LEXICAL]
        }
    }

    pub fn hello(semantic: bool) -> Value {
        let engine = crate::engine::engine();
        let (major, minor, patch) = PROVIDER_MATCHER_UNICODE_VERSION;
        json!({
            "contract": CONTRACT,
            "name": env!("CARGO_PKG_NAME"),
            "version": engine.version,
            "capabilities": {
                "retrieval": retrieval(semantic),
                "matchers": [PROVIDER_MATCHER],
                "matcherUnicodeVersion": format!("{major}.{minor}.{patch}"),
                "ordering": "relevance",
                "maxLimit": PROVIDER_MAX_LIMIT,
                "maxPartitions": PROVIDER_MAX_QUERY_PARTITIONS,
                "maxNoteBytes": PROVIDER_MAX_ITEM_BYTES,
            },
            "experimental": true,
            "build": {"commit": engine.commit, "target": engine.build},
        })
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct BeginBody {
        pub epoch: u64,
        pub mode: String,
    }

    pub fn search_mode(mode: &str) -> Option<ProviderMode> {
        match mode {
            LEXICAL => Some(ProviderMode::Lexical),
            SEMANTIC => Some(ProviderMode::Semantic),
            HYBRID => Some(ProviderMode::Hybrid),
            _ => None,
        }
    }

    pub fn mode_name(mode: ProviderMode) -> &'static str {
        match mode {
            ProviderMode::Lexical => LEXICAL,
            ProviderMode::Semantic => SEMANTIC,
            ProviderMode::Hybrid => HYBRID,
        }
    }

    /// The coverage `mode` of one kind of index (a `hybrid` answer reports
    /// a lexical and a semantic entry for every partition).
    pub fn kind_name(kind: ProviderKind) -> &'static str {
        match kind {
            ProviderKind::Lexical => LEXICAL,
            ProviderKind::Semantic => SEMANTIC,
        }
    }

    pub fn feed_mode(mode: &str) -> Option<ProviderFeedMode> {
        match mode {
            "full" => Some(ProviderFeedMode::Full),
            "delta" => Some(ProviderFeedMode::Delta),
            _ => None,
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct CheckpointBody {
        pub n: u64,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct CommitBody {}

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Question {
        pub text: String,
        #[serde(default)]
        pub mode: Option<String>,
    }

    /// The host's matcher tokens of `q.text`: a plain list (draft v1) or
    /// `{"matcher": "current-body-words-v1", "list": [...]}` (draft v2).
    #[derive(Deserialize)]
    #[serde(untagged)]
    pub enum Terms {
        List(Vec<String>),
        Named(NamedTerms),
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct NamedTerms {
        pub matcher: String,
        pub list: Vec<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    pub struct QueryBody {
        pub partitions: Vec<String>,
        pub q: Question,
        /// Authoritative for the lexical recall obligation. Absent or empty:
        /// derived from `q.text` with the same matcher.
        #[serde(default)]
        pub terms: Option<Terms>,
        pub limit: usize,
        /// Absolute Unix time in milliseconds.
        #[serde(default)]
        pub deadline_ms: Option<u64>,
    }

    /// Error tokens raised by the adapter itself, not by the store.
    pub const FORBIDDEN: &str = "forbidden";
    pub const METHOD_NOT_ALLOWED: &str = "method_not_allowed";
    pub const MODE_UNAVAILABLE: &str = "mode_unavailable";
    pub const MATCHER_UNAVAILABLE: &str = "matcher_unavailable";

    /// The contract's status and token for a store refusal.
    pub fn refusal(code: ProviderErrorCode) -> (StatusCode, &'static str) {
        match code {
            ProviderErrorCode::BadRequest => (StatusCode::BAD_REQUEST, "bad_request"),
            ProviderErrorCode::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ProviderErrorCode::NoOpenFeed => (StatusCode::CONFLICT, "no_open_feed"),
            ProviderErrorCode::EpochMismatch => (StatusCode::CONFLICT, "epoch_mismatch"),
            ProviderErrorCode::EpochRegressed => (StatusCode::CONFLICT, "epoch_regressed"),
            ProviderErrorCode::CheckpointNotIncreasing => {
                (StatusCode::CONFLICT, "checkpoint_not_increasing")
            }
            ProviderErrorCode::VersionMismatch => {
                (StatusCode::UNPROCESSABLE_ENTITY, "version_mismatch")
            }
            ProviderErrorCode::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
            ProviderErrorCode::ModeUnavailable => (StatusCode::BAD_REQUEST, MODE_UNAVAILABLE),
            ProviderErrorCode::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            ProviderErrorCode::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }

    fn state(state: ProviderState) -> &'static str {
        match state {
            ProviderState::Ready => "ready",
            ProviderState::Building => "building",
            ProviderState::Unavailable => "unavailable",
        }
    }

    pub fn as_of(as_of: Option<ProviderAsOf>) -> Value {
        as_of.map_or(
            Value::Null,
            |as_of| json!({"epoch": as_of.epoch, "n": as_of.n}),
        )
    }

    pub fn commit_reply(committed: ProviderAsOf) -> Value {
        json!({"asOf": as_of(Some(committed))})
    }

    pub fn query_reply(outcome: &ProviderQueryOutcome) -> Value {
        let coverage: Vec<_> = outcome
            .coverage
            .iter()
            .map(|c| {
                json!({"partition": c.partition, "mode": kind_name(c.kind), "state": state(c.state), "asOf": as_of(c.as_of)})
            })
            .collect();
        let refs: Vec<_> = outcome
            .refs
            .iter()
            .map(|r| json!({"partition": r.partition, "id": r.id, "version": r.version}))
            .collect();
        json!({"mode": mode_name(outcome.mode), "coverage": coverage, "refs": refs})
    }

    pub fn status_reply(status: &ProviderStatus) -> Value {
        let mut reply = json!({
            "state": state(status.state),
            "asOf": as_of(status.as_of),
            "notes": status.notes,
            "excluded": status.excluded,
            "partial": status.partial,
            "indexBytes": status.index_bytes,
        });
        // Present only when an embedding model is loaded.
        if let Some(semantic) = &status.semantic {
            reply["semantic"] = kind_status(semantic);
        }
        reply
    }

    fn kind_status(status: &ProviderKindStatus) -> Value {
        json!({
            "state": state(status.state),
            "asOf": as_of(status.as_of),
            "pendingNotes": status.pending_notes,
        })
    }

    pub fn error_body(code: &str) -> Value {
        json!({"error": {"code": code}})
    }
}

type Store = Arc<ProviderStore>;

/// The connecting process's user, from `SO_PEERCRED`.
#[derive(Clone, Copy, Debug)]
struct PeerUid(Option<u32>);

impl Connected<IncomingStream<'_, UnixListener>> for PeerUid {
    fn connect_info(stream: IncomingStream<'_, UnixListener>) -> Self {
        Self(stream.io().peer_cred().ok().map(|cred| cred.uid()))
    }
}

/// Embedding model options. Without `model_dir` the helper is lexical only.
pub(crate) struct ModelOptions {
    pub model_dir: Option<PathBuf>,
    pub embed_threads: Option<usize>,
}

pub(crate) async fn run(socket: PathBuf, data_dir: PathBuf, model: ModelOptions) -> Result<()> {
    // Everything this mode writes (index files, vector files, the marker,
    // the socket) is for this user only. The helper is its own process, so
    // the mask is set once, before the first file is created.
    // SAFETY: umask only swaps the process file-mode mask.
    unsafe { libc::umask(0o077) };
    let store = tokio::task::spawn_blocking(move || open_store(&data_dir, model)).await??;
    let listener = bind_socket(&socket)?;
    eprintln!(
        "provider helper (experimental) pid {} listening on {} (retrieval: {})",
        std::process::id(),
        socket.display(),
        wire::retrieval(store.semantic_enabled()).join(", ")
    );
    let service = router(Arc::new(store)).into_make_service_with_connect_info::<PeerUid>();
    let result = axum::serve(listener, service)
        .with_graceful_shutdown(crate::server::shutdown_signal())
        .await;
    let _ = std::fs::remove_file(&socket);
    result.context("provider helper server failed")
}

/// Opens the store. With a model directory, every pinned model file must be
/// there and byte-identical, or the helper refuses to start: it never
/// downloads a model and never silently drops to lexical only.
fn open_store(data_dir: &Path, model: ModelOptions) -> Result<ProviderStore> {
    let anyhow = |error: ProviderError| anyhow::anyhow!("{error}");
    let Some(model_dir) = model.model_dir else {
        return ProviderStore::open(data_dir).map_err(anyhow);
    };
    #[cfg(feature = "semantic-onnx")]
    {
        let index = kwiry_core::load_pinned_embedder(&model_dir, model.embed_threads)
            .map_err(anyhow)
            .context("semantic search refused")?;
        // Queries embed a few words; a small thread count keeps them off the
        // cores the background worker is using.
        let query = kwiry_core::load_pinned_embedder(&model_dir, Some(2))
            .map_err(anyhow)
            .context("semantic search refused")?;
        ProviderStore::open_with_semantic(data_dir, query, index).map_err(anyhow)
    }
    #[cfg(not(feature = "semantic-onnx"))]
    {
        let _ = (model_dir, model.embed_threads, data_dir);
        bail!("--model-dir needs a build with the semantic-onnx feature")
    }
}

/// Binds the socket with mode 0600, in a directory that only the current user
/// can write to. A stale socket file is replaced; any other file is refused.
fn bind_socket(socket: &Path) -> Result<UnixListener> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

    let parent = socket
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("socket path must name a file inside a directory")?;
    let metadata = std::fs::metadata(parent)
        .with_context(|| format!("socket directory {} is not usable", parent.display()))?;
    if !metadata.is_dir() || metadata.uid() != own_uid() || metadata.mode() & 0o022 != 0 {
        bail!(
            "socket directory {} must be a directory owned by this user and not group- or world-writable",
            parent.display()
        );
    }
    match std::fs::symlink_metadata(socket) {
        Ok(existing) if existing.file_type().is_socket() => std::fs::remove_file(socket)?,
        Ok(_) => bail!("{} exists and is not a socket", socket.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    // SAFETY: umask only swaps the process file-mode mask; restored below.
    let previous = unsafe { libc::umask(0o177) };
    let bound = UnixListener::bind(socket);
    // SAFETY: as above.
    unsafe { libc::umask(previous) };
    let listener = bound.with_context(|| format!("failed to bind {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn own_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

pub(crate) fn router(store: Store) -> Router {
    Router::new()
        .route(wire::ROUTE_HELLO, get(hello))
        .route(wire::ROUTE_QUERY, post(query))
        .route(wire::ROUTE_PARTITION, axum::routing::delete(retire))
        .route(wire::ROUTE_BEGIN, post(begin))
        .route(wire::ROUTE_ITEM, put(put_item).delete(delete_item))
        .route(wire::ROUTE_CHECKPOINT, post(checkpoint))
        .route(wire::ROUTE_COMMIT, post(commit))
        .route(wire::ROUTE_STATUS, get(status))
        .fallback(|| async { refuse(ProviderErrorCode::NotFound) })
        .method_not_allowed_fallback(|| async {
            failure(StatusCode::METHOD_NOT_ALLOWED, wire::METHOD_NOT_ALLOWED)
        })
        .layer(DefaultBodyLimit::max(wire::MAX_JSON_BODY_BYTES))
        .layer(middleware::from_fn(contract_envelope))
        .with_state(store)
}

/// Adapter rules around every exchange: same-user peers only, the profile
/// header both ways, and contract-shaped bodies for errors raised outside the
/// handlers (extractor rejections, body limits).
async fn contract_envelope(request: Request, next: Next) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<PeerUid>>()
        .map(|info| info.0.0);
    let mut response = if peer.is_some_and(|uid| uid != Some(own_uid())) {
        failure(StatusCode::FORBIDDEN, wire::FORBIDDEN)
    } else if request
        .headers()
        .get(wire::PROFILE_HEADER)
        .is_none_or(|value| value != wire::PROFILE)
    {
        refuse(ProviderErrorCode::BadRequest)
    } else {
        let response = next.run(request).await;
        let is_json = response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| value.as_bytes().starts_with(b"application/json"));
        if is_json || response.status().is_success() {
            response
        } else {
            match response.status() {
                StatusCode::NOT_FOUND => refuse(ProviderErrorCode::NotFound),
                StatusCode::METHOD_NOT_ALLOWED => {
                    failure(StatusCode::METHOD_NOT_ALLOWED, wire::METHOD_NOT_ALLOWED)
                }
                StatusCode::PAYLOAD_TOO_LARGE => refuse(ProviderErrorCode::TooLarge),
                status if status.is_server_error() => refuse(ProviderErrorCode::Internal),
                _ => refuse(ProviderErrorCode::BadRequest),
            }
        }
    };
    response.headers_mut().insert(
        wire::PROFILE_HEADER,
        HeaderValue::from_static(wire::PROFILE),
    );
    response
}

fn failure(status: StatusCode, code: &str) -> Response {
    let mut response = (status, axum::Json(wire::error_body(code))).into_response();
    if status == StatusCode::SERVICE_UNAVAILABLE {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    response
}

/// The contract-shaped reply for a store refusal.
fn refuse(code: ProviderErrorCode) -> Response {
    let (status, token) = wire::refusal(code);
    failure(status, token)
}

struct WireError(Box<Response>);

impl WireError {
    fn from_response(response: Response) -> Self {
        Self(Box::new(response))
    }
}

impl From<ProviderError> for WireError {
    fn from(error: ProviderError) -> Self {
        if error.code == ProviderErrorCode::Internal {
            eprintln!("provider helper internal error: {}", error.detail);
        }
        Self::from_response(refuse(error.code))
    }
}

impl IntoResponse for WireError {
    fn into_response(self) -> Response {
        *self.0
    }
}

fn bad_request() -> WireError {
    WireError::from_response(refuse(ProviderErrorCode::BadRequest))
}

type WireResult = std::result::Result<Response, WireError>;

/// Runs blocking store work off the executor and renders its JSON reply.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> std::result::Result<T, ProviderError> + Send + 'static,
    render: impl FnOnce(T) -> Value,
) -> WireResult {
    let value = tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| WireError::from_response(refuse(ProviderErrorCode::Internal)))??;
    Ok(axum::Json(render(value)).into_response())
}

fn empty<T>(_: T) -> Value {
    json!({})
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> std::result::Result<T, WireError> {
    serde_json::from_slice(body).map_err(|_| bad_request())
}

async fn hello(State(store): State<Store>) -> Response {
    axum::Json(wire::hello(store.semantic_enabled())).into_response()
}

async fn begin(
    State(store): State<Store>,
    UrlPath(partition): UrlPath<String>,
    body: Bytes,
) -> WireResult {
    let wire::BeginBody { epoch, mode } = parse(&body)?;
    let mode = wire::feed_mode(&mode).ok_or_else(bad_request)?;
    blocking(move || store.begin(&partition, epoch, mode), empty).await
}

async fn put_item(
    State(store): State<Store>,
    UrlPath((partition, id)): UrlPath<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> WireResult {
    let max = PROVIDER_MAX_ITEM_BYTES;
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let too_large = |store: Store, partition: String, id: String| async move {
        // Counted as excluded, never silently dropped (§2.3 size caps). With
        // no open feed there is nothing to count it in: the refusal is the
        // same `no_open_feed` any other put gets.
        let recorded =
            tokio::task::spawn_blocking(move || store.exclude_over_transport_cap(&partition, &id))
                .await;
        match recorded {
            Ok(Ok(())) => WireError::from_response(refuse(ProviderErrorCode::TooLarge)),
            Ok(Err(error)) => WireError::from(error),
            Err(_) => WireError::from_response(refuse(ProviderErrorCode::Internal)),
        }
    };
    if declared.is_some_and(|length| length > max) {
        return Err(too_large(store, partition, id).await);
    }
    let version = text_header(&headers, wire::HEADER_ITEM_VERSION)?;
    let path =
        percent_decode(&text_header(&headers, wire::HEADER_ITEM_PATH)?).ok_or_else(bad_request)?;
    // Reads at most the cap; a longer stream stops there without being drained.
    let bytes = match to_bytes(body, max as usize).await {
        Ok(bytes) => bytes,
        Err(error) => {
            let over_limit = std::error::Error::source(&error)
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>());
            return Err(if over_limit {
                too_large(store, partition, id).await
            } else {
                bad_request()
            });
        }
    };
    blocking(
        move || store.put(&partition, &id, &version, &path, &bytes),
        empty,
    )
    .await
}

async fn delete_item(
    State(store): State<Store>,
    UrlPath((partition, id)): UrlPath<(String, String)>,
) -> WireResult {
    blocking(move || store.delete(&partition, &id), empty).await
}

async fn checkpoint(
    State(store): State<Store>,
    UrlPath(partition): UrlPath<String>,
    body: Bytes,
) -> WireResult {
    let wire::CheckpointBody { n } = parse(&body)?;
    blocking(move || store.checkpoint(&partition, n), empty).await
}

async fn commit(
    State(store): State<Store>,
    UrlPath(partition): UrlPath<String>,
    body: Bytes,
) -> WireResult {
    if !body.is_empty() {
        let wire::CommitBody {} = parse(&body)?;
    }
    blocking(move || store.commit(&partition), wire::commit_reply).await
}

async fn retire(State(store): State<Store>, UrlPath(partition): UrlPath<String>) -> WireResult {
    blocking(move || store.retire(&partition), empty).await
}

async fn status(State(store): State<Store>, UrlPath(partition): UrlPath<String>) -> WireResult {
    blocking(
        move || store.status(&partition),
        |status| wire::status_reply(&status),
    )
    .await
}

async fn query(State(store): State<Store>, body: Bytes) -> WireResult {
    let wire::QueryBody {
        partitions,
        q,
        terms,
        limit,
        deadline_ms,
    } = parse(&body)?;
    let offered = wire::retrieval(store.semantic_enabled());
    let requested = q.mode.as_deref().unwrap_or(wire::LEXICAL);
    let Some(mode) = wire::search_mode(requested).filter(|_| offered.contains(&requested)) else {
        // Never a silent switch to another kind of search.
        return Err(WireError::from_response(refuse(
            ProviderErrorCode::ModeUnavailable,
        )));
    };
    let terms = match terms {
        None => None,
        Some(wire::Terms::List(list)) => Some(list),
        Some(wire::Terms::Named(named)) => {
            if named.matcher != PROVIDER_MATCHER {
                return Err(WireError::from_response(failure(
                    StatusCode::BAD_REQUEST,
                    wire::MATCHER_UNAVAILABLE,
                )));
            }
            Some(named.list)
        }
    }
    // An empty list carries no terms: derive them from the text.
    .filter(|list| !list.is_empty());
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64);
    let budget_ms = match deadline_ms {
        Some(deadline) => deadline.saturating_sub(now_ms).min(wire::MAX_DEADLINE_MS),
        None => wire::DEFAULT_DEADLINE_MS,
    };
    if budget_ms == 0 {
        return Err(WireError::from_response(refuse(
            ProviderErrorCode::Unavailable,
        )));
    }
    let text = q.text;
    let search = blocking(
        move || {
            store.ask(&ProviderQuery {
                partitions: &partitions,
                text: &text,
                mode,
                terms: terms.as_deref(),
                limit,
            })
        },
        |outcome| wire::query_reply(&outcome),
    );
    match tokio::time::timeout(Duration::from_millis(budget_ms), search).await {
        Ok(result) => result,
        // Past the deadline the host has stopped waiting; the search thread
        // finishes on its own and its answer is dropped.
        Err(_) => Err(WireError::from_response(refuse(
            ProviderErrorCode::Unavailable,
        ))),
    }
}

fn text_header(headers: &HeaderMap, name: &'static str) -> std::result::Result<String, WireError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(bad_request)
}

/// Decodes `%XX` escapes; any other byte passes through. Refuses a malformed
/// escape or a result that is not UTF-8.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = value.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use kwiry_core::{
        ProviderAsOf, ProviderCoverage, ProviderKind, ProviderKindStatus, ProviderMode,
        ProviderQueryOutcome, ProviderReference, ProviderState, ProviderStatus,
    };
    use serde_json::json;

    use super::{percent_decode, wire};

    #[test]
    fn hybrid_reply_states_each_partition_per_kind() {
        let entry = |kind, state, as_of| ProviderCoverage {
            partition: "p".to_owned(),
            kind,
            state,
            as_of,
        };
        let as_of = Some(ProviderAsOf { epoch: 1, n: 2 });
        let older = Some(ProviderAsOf { epoch: 1, n: 1 });
        let reply = wire::query_reply(&ProviderQueryOutcome {
            mode: ProviderMode::Hybrid,
            coverage: vec![
                entry(ProviderKind::Lexical, ProviderState::Ready, as_of),
                entry(ProviderKind::Semantic, ProviderState::Building, older),
            ],
            refs: vec![ProviderReference {
                partition: "p".to_owned(),
                id: "a".to_owned(),
                version: "v".to_owned(),
            }],
        });
        assert_eq!(
            reply,
            json!({"mode": "hybrid", "refs": [{"partition": "p", "id": "a", "version": "v"}], "coverage": [
                {"partition": "p", "mode": "lexical", "state": "ready", "asOf": {"epoch": 1, "n": 2}},
                {"partition": "p", "mode": "semantic", "state": "building", "asOf": {"epoch": 1, "n": 1}},
            ]})
        );
    }

    #[test]
    fn status_reply_names_the_semantic_kind_only_with_a_model() {
        let mut status = ProviderStatus {
            state: ProviderState::Ready,
            as_of: Some(ProviderAsOf { epoch: 1, n: 2 }),
            notes: 3,
            excluded: Default::default(),
            partial: Default::default(),
            index_bytes: 10,
            semantic: None,
        };
        let lexical_only = wire::status_reply(&status);
        assert!(lexical_only.get("semantic").is_none());
        assert_eq!(lexical_only["asOf"], json!({"epoch": 1, "n": 2}));
        status.semantic = Some(ProviderKindStatus {
            state: ProviderState::Building,
            as_of: None,
            pending_notes: 3,
        });
        assert_eq!(
            wire::status_reply(&status)["semantic"],
            json!({"state": "building", "asOf": null, "pendingNotes": 3})
        );
    }

    #[test]
    fn terms_accept_the_v1_list_and_the_v2_named_form() {
        let parse =
            |body: serde_json::Value| -> wire::QueryBody { serde_json::from_value(body).unwrap() };
        let v1 =
            parse(json!({"partitions": ["p"], "q": {"text": "a"}, "terms": ["a"], "limit": 1}));
        assert!(matches!(v1.terms, Some(wire::Terms::List(list)) if list == ["a"]));
        let v2 = parse(json!({"partitions": ["p"], "q": {"text": "a"},
            "terms": {"matcher": "current-body-words-v1", "list": ["a"]}, "limit": 1}));
        assert!(matches!(v2.terms, Some(wire::Terms::Named(named)) if named.list == ["a"]));
        assert!(
            serde_json::from_value::<wire::QueryBody>(
                json!({"partitions": ["p"], "q": {"text": "a"},
                "terms": {"matcher": "x", "list": [], "extra": 1}, "limit": 1})
            )
            .is_err()
        );
        assert_eq!(
            wire::hello(false)["capabilities"]["retrieval"],
            json!(["lexical"])
        );
        assert_eq!(
            wire::hello(true)["capabilities"]["retrieval"],
            json!(["lexical", "semantic", "hybrid"])
        );
        assert_eq!(
            wire::hello(true)["capabilities"]["matchers"],
            json!(["current-body-words-v1"])
        );
    }

    #[test]
    fn percent_decode_handles_utf8_and_refuses_bad_escapes() {
        assert_eq!(
            percent_decode("Areas/caf%C3%A9%20notes.md").as_deref(),
            Some("Areas/café notes.md")
        );
        assert_eq!(percent_decode("a%2"), None);
        assert_eq!(percent_decode("a%zz"), None);
        assert_eq!(percent_decode("%FF"), None);
    }
}
