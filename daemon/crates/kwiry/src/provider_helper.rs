//! Experimental provider-helper mode (trial only): the search provider
//! contract (DRAFT v1, wire form §2.3) as HTTP/1.1 + JSON on a local Unix
//! domain socket, in front of `kwiry_core::ProviderStore`.
//!
//! Desktop and non-governed. No TCP listener, no outbound network, no
//! secrets, no workspace reads: the helper indexes only the bytes a host
//! pushes. Every name the contract may still rename (routes, headers, JSON
//! keys, error tokens) lives in [`wire`]; the store knows none of them.
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
    PROVIDER_MAX_ITEM_BYTES, ProviderError, ProviderErrorKind, ProviderFeedMode, ProviderStore,
};
use serde_json::{Value, json};
use tokio::net::UnixListener;

/// Everything the partner's contract may rename or reshape.
pub(crate) mod wire {
    use kwiry_core::{
        PROVIDER_MAX_ITEM_BYTES, PROVIDER_MAX_LIMIT, PROVIDER_MAX_QUERY_PARTITIONS,
        ProviderQueryOutcome,
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
    /// Search modes this helper offers. Anything else is `mode_unavailable`.
    pub const LEXICAL: &str = "lexical";
    pub const DEFAULT_DEADLINE_MS: u64 = 10_000;
    pub const MAX_DEADLINE_MS: u64 = 60_000;

    pub fn hello() -> Value {
        let engine = crate::engine::engine();
        json!({
            "contract": CONTRACT,
            "name": env!("CARGO_PKG_NAME"),
            "version": engine.version,
            "capabilities": {
                "retrieval": [LEXICAL],
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

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    pub struct QueryBody {
        pub partitions: Vec<String>,
        pub q: Question,
        /// `current-body-words-v1` tokens, a hint. This helper ranks on
        /// `q.text` with its own analyser and does not use them.
        #[serde(default)]
        #[allow(dead_code)]
        pub terms: Vec<String>,
        pub limit: usize,
        /// Absolute Unix time in milliseconds.
        #[serde(default)]
        pub deadline_ms: Option<u64>,
    }

    pub fn query_reply(outcome: &ProviderQueryOutcome) -> Value {
        let coverage: Vec<_> = outcome
            .coverage
            .iter()
            .map(|c| json!({"partition": c.partition, "mode": LEXICAL, "state": c.state, "asOf": c.as_of}))
            .collect();
        json!({"mode": LEXICAL, "coverage": coverage, "refs": outcome.refs})
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

pub(crate) async fn run(socket: PathBuf, data_dir: PathBuf) -> Result<()> {
    let store = tokio::task::spawn_blocking(move || ProviderStore::open(&data_dir))
        .await?
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let listener = bind_socket(&socket)?;
    eprintln!(
        "provider helper (experimental) pid {} listening on {}",
        std::process::id(),
        socket.display()
    );
    let service = router(Arc::new(store)).into_make_service_with_connect_info::<PeerUid>();
    let result = axum::serve(listener, service)
        .with_graceful_shutdown(crate::server::shutdown_signal())
        .await;
    let _ = std::fs::remove_file(&socket);
    result.context("provider helper server failed")
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
        .fallback(|| async { failure(StatusCode::NOT_FOUND, "not_found") })
        .method_not_allowed_fallback(|| async {
            failure(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
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
        failure(StatusCode::FORBIDDEN, "forbidden")
    } else if request
        .headers()
        .get(wire::PROFILE_HEADER)
        .is_none_or(|value| value != wire::PROFILE)
    {
        failure(StatusCode::BAD_REQUEST, "bad_request")
    } else {
        let response = next.run(request).await;
        let is_json = response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| value.as_bytes().starts_with(b"application/json"));
        if is_json || response.status().is_success() {
            response
        } else {
            let status = response.status();
            let code = match status {
                StatusCode::NOT_FOUND => "not_found",
                StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
                StatusCode::PAYLOAD_TOO_LARGE => "too_large",
                status if status.is_server_error() => "internal",
                _ => "bad_request",
            };
            let status = if code == "bad_request" {
                StatusCode::BAD_REQUEST
            } else {
                status
            };
            failure(status, code)
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

struct WireError(Box<Response>);

impl WireError {
    fn from_response(response: Response) -> Self {
        Self(Box::new(response))
    }
}

impl From<ProviderError> for WireError {
    fn from(error: ProviderError) -> Self {
        let status = match error.kind {
            ProviderErrorKind::BadRequest => StatusCode::BAD_REQUEST,
            ProviderErrorKind::NotFound => StatusCode::NOT_FOUND,
            ProviderErrorKind::Conflict => StatusCode::CONFLICT,
            ProviderErrorKind::Unprocessable => StatusCode::UNPROCESSABLE_ENTITY,
            ProviderErrorKind::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ProviderErrorKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ProviderErrorKind::Internal => {
                eprintln!("provider helper internal error: {}", error.detail);
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        Self::from_response(failure(status, error.code))
    }
}

impl IntoResponse for WireError {
    fn into_response(self) -> Response {
        *self.0
    }
}

fn bad_request() -> WireError {
    WireError::from_response(failure(StatusCode::BAD_REQUEST, "bad_request"))
}

type WireResult = std::result::Result<Response, WireError>;

/// Runs blocking store work off the executor and renders its JSON reply.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> std::result::Result<T, ProviderError> + Send + 'static,
    render: impl FnOnce(T) -> Value,
) -> WireResult {
    let value = tokio::task::spawn_blocking(work).await.map_err(|_| {
        WireError::from_response(failure(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
    })??;
    Ok(axum::Json(render(value)).into_response())
}

fn empty<T>(_: T) -> Value {
    json!({})
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> std::result::Result<T, WireError> {
    serde_json::from_slice(body).map_err(|_| bad_request())
}

async fn hello() -> Response {
    axum::Json(wire::hello()).into_response()
}

async fn begin(
    State(store): State<Store>,
    UrlPath(partition): UrlPath<String>,
    body: Bytes,
) -> WireResult {
    let wire::BeginBody { epoch, mode } = parse(&body)?;
    let mode = match mode.as_str() {
        "full" => ProviderFeedMode::Full,
        "delta" => ProviderFeedMode::Delta,
        _ => return Err(bad_request()),
    };
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
        // Counted as excluded, never silently dropped (§2.3 size caps).
        let _ =
            tokio::task::spawn_blocking(move || store.exclude_over_transport_cap(&partition, &id))
                .await;
        WireError::from_response(failure(StatusCode::PAYLOAD_TOO_LARGE, "too_large"))
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
    blocking(
        move || store.commit(&partition),
        |as_of| json!({"asOf": as_of}),
    )
    .await
}

async fn retire(State(store): State<Store>, UrlPath(partition): UrlPath<String>) -> WireResult {
    blocking(move || store.retire(&partition), empty).await
}

async fn status(State(store): State<Store>, UrlPath(partition): UrlPath<String>) -> WireResult {
    blocking(
        move || store.status(&partition),
        |status| serde_json::to_value(status).unwrap_or_else(|_| json!({})),
    )
    .await
}

async fn query(State(store): State<Store>, body: Bytes) -> WireResult {
    let wire::QueryBody {
        partitions,
        q,
        limit,
        deadline_ms,
        ..
    } = parse(&body)?;
    if q.mode.as_deref().unwrap_or(wire::LEXICAL) != wire::LEXICAL {
        // Never a silent switch to another kind of search.
        return Err(WireError::from_response(failure(
            StatusCode::BAD_REQUEST,
            "mode_unavailable",
        )));
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64);
    let budget_ms = match deadline_ms {
        Some(deadline) => deadline.saturating_sub(now_ms).min(wire::MAX_DEADLINE_MS),
        None => wire::DEFAULT_DEADLINE_MS,
    };
    if budget_ms == 0 {
        return Err(WireError::from_response(failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        )));
    }
    let text = q.text;
    let search = blocking(
        move || store.query(&partitions, &text, limit),
        |outcome| wire::query_reply(&outcome),
    );
    match tokio::time::timeout(Duration::from_millis(budget_ms), search).await {
        Ok(result) => result,
        // Past the deadline the host has stopped waiting; the search thread
        // finishes on its own and its answer is dropped.
        Err(_) => Err(WireError::from_response(failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
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
    use super::percent_decode;

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
