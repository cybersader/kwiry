//! The dedicated listener has no baseline search handler. Everything before selection
//! (including replay and method dispatch) is shared with the existing governed route.
use super::*;
use axum::extract::Request;
use axum::http::{HeaderMap, header};
use kwiry_core::{
    ExtractionCoverage, GenerationSearchResult, ResourceKey, SearchHit, SourceFormat,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

const PROFILE_HEADER: &str = "kwiry-profile";
const PROFILE: &str = "live-check-v1";
const BODY_LIMIT: usize = 2 * 1024 * 1024;

pub(super) async fn no_store(request: Request, next: middleware::Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn single<'a>(headers: &'a HeaderMap, key: &str) -> Option<&'a [u8]> {
    let mut values = headers.get_all(key).iter();
    let first = values.next()?;
    values.next().is_none().then_some(first.as_bytes())
}

pub(super) async fn search(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    request: Request,
) -> Response {
    if single(request.headers(), PROFILE_HEADER) != Some(PROFILE.as_bytes()) {
        return HttpError::new(
            StatusCode::NOT_ACCEPTABLE,
            "profile_unsupported",
            "the required search profile was not selected",
        )
        .into_response();
    }
    let mut response = selected(&state, principal, request).await.into_response();
    response
        .headers_mut()
        .insert(PROFILE_HEADER, HeaderValue::from_static(PROFILE));
    response
}

fn unavailable() -> HttpError {
    map_core_error(
        kwiry_core::Error::ResourceUnavailable,
        HostProfile::OpenClast,
    )
}
fn unbound() -> HttpError {
    let mut error = unavailable();
    error.body.error.resource_indexes = Some(vec![0]);
    error
}
fn internal() -> HttpError {
    HttpError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "the search request could not be completed",
    )
}
fn invalid_request() -> HttpError {
    HttpError::new(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "the request is not a valid live-check search",
    )
}
fn invalid_query() -> HttpError {
    HttpError::new(
        StatusCode::BAD_REQUEST,
        "invalid_query",
        "the query is not supported",
    )
}
fn search_error(error: kwiry_core::Error) -> HttpError {
    // No engine diagnostics are serialized or logged, even on unexpected variants.
    match error {
        // The literal and planner have passed stage 7. Query errors here can also
        // describe broken postings or collectors; they are stage-8 faults.
        kwiry_core::Error::UnavailableResourceIndexes(_) => unbound(),
        kwiry_core::Error::ResourceUnavailable | kwiry_core::Error::IndexBuilding => unavailable(),
        _ => internal(),
    }
}
fn global_gate(state: &AppState) -> Result<(), HttpError> {
    check_holder_gate(state)?;
    if state.live_gate.as_ref().is_some_and(|gate| gate.open())
        && state.runtime.active_room_identity_format()
        && state.runtime.freshness_basis() == IndexFreshnessBasis::StrictHash
    {
        Ok(())
    } else {
        Err(unavailable())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    q: String,
    mode: String,
    limit: usize,
}

fn literal_query(q: &str) -> bool {
    // The pinned regex tables (Unicode 16) have identical L|Nd / M membership
    // for characters assigned through 15.1. Age is cumulative in regex-syntax.
    static WORD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\A[[\pL\p{Nd}]&&\p{Age=15.1}][[\pL\p{Nd}\pM]&&\p{Age=15.1}]*\z")
            .expect("constant Unicode grammar")
    });
    !q.is_empty()
        && q.len() <= kwiry_core::MAX_QUERY_BYTES
        && !q.starts_with(' ')
        && !q.ends_with(' ')
        && q.split(' ').filter(|w| !w.is_empty()).all(|word| {
            WORD.is_match(word)
                && !["AND", "OR", "NOT"]
                    .iter()
                    .any(|op| word.eq_ignore_ascii_case(op))
        })
}

fn admit_query(q: &str) -> Result<(), HttpError> {
    if !literal_query(q) {
        return Err(invalid_query());
    }
    let plan = kwiry_core::prepare_lexical_query(q).map_err(|_| invalid_query())?;
    check_plan(&plan)
}
fn check_plan(plan: &kwiry_core::LexicalQueryPlan) -> Result<(), HttpError> {
    if plan.profile_id != "lexical-v2"
        || plan.support_probes.len() > kwiry_core::MAX_TERM_SUPPORT_PROBES
    {
        Err(invalid_query())
    } else {
        Ok(())
    }
}

async fn selected(
    state: &AppState,
    principal: Principal,
    request: Request,
) -> Result<Response, HttpError> {
    let [resource] = principal.resources.as_slice() else {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "one authorized resource is required",
        ));
    };
    global_gate(state)?;
    if !single(request.headers(), "content-type")
        .is_some_and(|v| v.eq_ignore_ascii_case(b"application/json"))
    {
        return Err(invalid_request());
    }
    let bytes = axum::body::to_bytes(request.into_body(), BODY_LIMIT)
        .await
        .map_err(|_| invalid_request())?;
    // Serde's struct decoder also accepts positional arrays. Never admit those.
    if bytes.iter().copied().find(|b| !b.is_ascii_whitespace()) != Some(b'{') {
        return Err(invalid_request());
    }
    let page: Page = serde_json::from_slice(&bytes).map_err(|_| invalid_request())?;
    if page.mode != "lexical"
        || !(1..=100).contains(&page.limit)
        || page.limit < principal.max_limit
    {
        return Err(invalid_request());
    }
    if page.limit > principal.max_limit {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "limit_exceeded",
            "the requested limit exceeds the capability constraint",
        ));
    }
    admit_query(&page.q)?;
    let runtime = state.runtime.clone();
    let signed = resource.clone();
    let result = tokio::task::spawn_blocking(move || {
        runtime.search_labeled_resource(&page.q, page.limit, &signed)
    })
    .await
    .map_err(|_| internal())?
    .map_err(search_error)?;
    let response = stage_response(resource, &result)?;
    finish(state, resource, &result.generation, response)
}

pub(super) fn finish(
    state: &AppState,
    resource: &ResourceKey,
    generation: &str,
    mut response: Response,
) -> Result<Response, HttpError> {
    global_gate(state)?;
    let freshness = state
        .runtime
        .labeled_resource_freshness(resource, generation);
    // A holder may fail-stop during resource revalidation.
    global_gate(state)?;
    let freshness = freshness.map_err(search_error)?;
    if freshness.basis != IndexFreshnessBasis::StrictHash {
        return Err(unavailable());
    }
    response.headers_mut().insert(
        INDEX_FRESHNESS_HEADER,
        HeaderValue::from_static(freshness.header_value()),
    );
    Ok(response)
}

#[derive(Serialize)]
struct BoundHit<'a> {
    #[serde(flatten)]
    hit: &'a SearchHit,
    content_sha256: &'a str,
}
#[derive(Serialize)]
struct Binding<'a> {
    resource: &'a ResourceKey,
    snapshot_id: &'a str,
}
#[derive(Serialize)]
struct Answer<'a> {
    hits: Vec<BoundHit<'a>>,
    next_cursor: Option<()>,
    extraction_policy_fingerprint: &'a str,
    resource_snapshots: [Binding<'a>; 1],
}
fn stage_response(
    signed: &ResourceKey,
    result: &GenerationSearchResult,
) -> Result<Response, HttpError> {
    result.require_snapshot_ids().map_err(|_| unbound())?;
    let mut bindings = result.snapshot_ids.iter();
    let (resource, label) = bindings.next().ok_or_else(unbound)?;
    let label = label
        .as_ref()
        .filter(|id| id.is_live_check_label())
        .ok_or_else(unbound)?;
    if resource != signed
        || bindings.next().is_some()
        || !kwiry_core::valid_tenant_identifier(&resource.tenant_id)
        || !kwiry_core::valid_room_identifier(&resource.vault_id)
        || !kwiry_core::valid_room_identifier(&resource.room_id)
    {
        return Err(unbound());
    }
    if result.hits.len() > 100 {
        return Err(internal());
    }
    // Vec<String> headings, String excerpts, f32 (nonfinite serializes as null),
    // and the closed Frontmatter projection (empty tags omitted) satisfy their schema by type.
    let hits = result
        .hits
        .iter()
        .map(|hit| {
            if !kwiry_core::valid_lower_sha256(&hit.chunk_id)
                || hit.path.is_empty()
                || hit.path.starts_with('/')
                || hit.vault_id != resource.vault_id
                || hit.room_id.as_ref() != Some(&resource.room_id)
                || hit.format != SourceFormat::Markdown
                || hit.coverage != ExtractionCoverage::IndexedComplete
                || hit.locator.is_some()
            {
                return Err(internal());
            }
            Ok(BoundHit {
                hit,
                content_sha256: hit
                    .require_content_sha256()
                    .map_err(|_| internal())?
                    .as_str(),
            })
        })
        .collect::<Result<_, _>>()?;
    let body = serde_json::to_vec(&Answer {
        hits,
        next_cursor: None,
        extraction_policy_fingerprint: kwiry_core::extraction_policy_fingerprint(),
        resource_snapshots: [Binding {
            resource,
            snapshot_id: label.as_str(),
        }],
    })
    .map_err(|_| internal())?;
    let quality = lexical_match_quality_header(SearchMode::Lexical, result.lexical_match_quality)?;
    let mut response = ([(header::CONTENT_TYPE, "application/json")], body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        GENERATION_HEADER,
        HeaderValue::from_str(&result.generation).map_err(|_| internal())?,
    );
    for (name, value) in [
        (LEXICAL_PROFILE_HEADER, "lexical-v2"),
        (LEXICAL_MATCH_QUALITY_HEADER, quality),
        (FIELD_SCOPE_HEADER, "none"),
        (FIELD_EMPHASIS_HEADER, "none"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn governed_search_live_check_success_schema_mutations() {
        use serde_json::{Value, json};
        // Obtain the opaque proof the same way production does (not via JSON).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("room");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "lavender").unwrap();
        let data = dir.path().join("data");
        let resource = ResourceKey::new(
            "tenant_AAAAAAAAAAAAAAAAAAAA",
            "A".repeat(22),
            "B".repeat(22),
        );
        let mut cfg = Config::default();
        cfg.vaults.push(kwiry_core::VaultRegistration {
            id: resource.vault_id.clone(),
            path: root,
            room: None,
            snapshot_id: None,
        });
        build_index(&cfg, &data).unwrap();
        let mut hit = kwiry_core::search_index(
            &data,
            &kwiry_core::LexicalSearchRequest {
                query: "lavender".into(),
                limit: 1,
                vault_id: None,
            },
        )
        .unwrap()
        .pop()
        .unwrap();
        hit.room_id = Some(resource.room_id.clone());
        let label = "a".repeat(64).parse().unwrap();
        let mut result = GenerationSearchResult {
            snapshot_ids: BTreeMap::from([(resource.clone(), Some(label))]),
            hits: vec![hit.clone()],
            generation: "synthetic".into(),
            lexical_match_quality: Some(LexicalMatchQuality::StandardOnly),
        };
        let schema: Value = serde_json::from_str(include_str!(
            "../../../../../docs/wire/governed-search-live-check-v1/success.schema.json"
        ))
        .unwrap();
        let validator = jsonschema::draft202012::new(&schema).unwrap();
        async fn body(response: Response) -> Value {
            assert_eq!(response.status(), StatusCode::OK);
            serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 1 << 20)
                    .await
                    .unwrap(),
            )
            .unwrap()
        }
        let good = body(
            stage_response(&resource, &result).unwrap_or_else(|e| panic!("{}", e.body.error.code)),
        )
        .await;
        validator.validate(&good).unwrap();
        // Mutate each hit field against the actual published schema. Some invalid
        // wire shapes cannot exist in Rust, or normalize away (e.g. empty tags).
        for (field, bad) in [
            ("chunk_id", json!("not-a-digest")),
            ("chunk_id", json!("A".repeat(64))),
            ("vault_id", json!("!".repeat(22))),
            ("room_id", Value::Null),
            ("path", json!("")),
            ("path", json!("/absolute")),
            ("heading_path", json!([5])),
            ("format", json!("text")),
            ("coverage", json!("indexed-partial")),
            ("locator", json!({"kind":"pdf_page","page":1})),
            ("score", json!("high")),
            ("excerpt", json!(false)),
            ("frontmatter", json!({"title":1})),
            ("frontmatter", json!({"description":1})),
            ("frontmatter", json!({"status":1})),
            ("frontmatter", json!({"date":1})),
            ("frontmatter", json!({"tags":[]})),
            ("frontmatter", json!({"tags":[1]})),
            ("frontmatter", json!({"other":1})),
            ("content_sha256", json!("bad")),
            ("extra", json!(true)),
        ] {
            let mut invalid = good.clone();
            invalid["hits"][0][field] = bad;
            assert!(!validator.is_valid(&invalid), "{field}");
            let Ok(mut candidate) = serde_json::from_value::<SearchHit>(invalid["hits"][0].clone())
            else {
                continue;
            };
            if field != "content_sha256" {
                candidate.content_sha256.clone_from(&hit.content_sha256);
            }
            result.hits = vec![hit.clone(), candidate];
            match stage_response(&resource, &result) {
                Ok(response) => {
                    assert!(matches!(field, "frontmatter" | "extra"));
                    validator.validate(&body(response).await).unwrap();
                }
                Err(e) => assert_eq!(e.status, StatusCode::INTERNAL_SERVER_ERROR),
            }
        }
        result.hits = vec![hit; 101];
        assert_eq!(
            stage_response(&resource, &result).unwrap_err().status,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn governed_search_live_check_unicode_tables_match_15_1() {
        // Drift detector over the Unicode 15.1 DerivedGeneralCategory.txt oracle:
        // FNV-1a of one byte per scalar in order (1=L/Nd, 2=M, 0=other).
        let mut signature = 0xcbf29ce484222325_u64;
        for ch in (0..=0x10ffff).filter_map(char::from_u32) {
            let mut word = [b'a'; 5];
            let n = ch.encode_utf8(&mut word[1..]).len();
            let word = std::str::from_utf8(&word[..=n]).unwrap();
            let class = if literal_query(&word[1..]) {
                1
            } else if literal_query(word) {
                2
            } else {
                0
            };
            signature = (signature ^ class).wrapping_mul(0x100000001b3);
        }
        assert_eq!(signature, 0xc1c66d8ed6bbe66b);
    }

    #[test]
    fn governed_search_live_check_planner_budget_is_not_word_count() {
        let q = (0..128)
            .map(|n| format!("word{n:03}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut plan = kwiry_core::prepare_lexical_query(&q).unwrap();
        assert_eq!(plan.support_probes.len(), 128);
        assert!(check_plan(&plan).is_ok());
        let mut extra = plan.support_probes[0].clone();
        extra.probe_id = 128;
        extra.term_index = 128;
        extra.term = "additional".into();
        plan.support_probes.push(extra);
        assert_eq!(
            check_plan(&plan).unwrap_err().body.error.code,
            "invalid_query"
        );
    }
    #[test]
    fn governed_search_live_check_refuses_wrong_or_ambiguous_binding_even_when_empty() {
        let r = ResourceKey::new("tenant", "vault", "room");
        let other = ResourceKey::new("tenant", "vault", "other");
        let label = Some("a".repeat(64).parse().unwrap());
        for map in [
            BTreeMap::new(),
            BTreeMap::from([(r.clone(), None)]),
            BTreeMap::from([(other.clone(), label.clone())]),
            BTreeMap::from([(r.clone(), label.clone()), (other.clone(), label.clone())]),
        ] {
            let result = GenerationSearchResult {
                snapshot_ids: map,
                generation: String::new(),
                hits: vec![],
                lexical_match_quality: Some(LexicalMatchQuality::None),
            };
            let err = stage_response(&r, &result).unwrap_err();
            assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(err.body.error.resource_indexes, Some(vec![0]));
        }
    }
}
