//! Conformance against the authority's official golden vectors.
//!
//! The fixture under `tests/fixtures/` is vendored unmodified. Its source
//! commit, its sha256 and the authority-specific wire constants it was
//! generated under live in the provenance note next to it; nothing in this
//! runner names an authority.
//!
//! Every vector the holder side can consume is driven through the real
//! holder paths: `list_once` (request signing, page verification, key-set
//! handling, assembly, admit and apply), the serving gates, purge, and the
//! classification of report answers with its floors, receipts and uncertain
//! attempts. Authority-signed bytes are never modified. The only state built
//! here is holder-local state a vector describes in prose (attempts it says
//! were sent, a lost `sent/` directory, held units); each is labelled where
//! it is built.
//!
//! [`REGISTRY`] accounts for every vector exactly once: run here, deferred
//! to a named later slice, or authority-only.

mod lists;
mod reports;
mod status;

use crate::b64;
use crate::canonical::sha256_tagged;
use crate::client::{FetchFailure, HttpResponse, Timeouts, Transport, TransportError};
use crate::engine::stale_serve::DEFAULT_STALE_SERVE_S;
use crate::engine::{
    ArtifactStamp, Cut, EngineProfile, EngineQuery, EngineSettings, RootObservation,
};
use crate::events::MemorySink;
use crate::gate::Refusal;
use crate::holder::{DEFAULT_PAGE_SIZE, Holder, HolderConfig, ListOutcome};
use crate::keys::HolderKey;
use crate::obligations::{
    Attempt, Channel, Completion, Counts, Pending, PendingStatus, ReportBody,
};
use crate::profile::{PROBLEM_CONTENT_TYPE, ROUTE_LIST, WireProfile};
use crate::report::Answer;
use crate::startup::{self, EnrollParams};
use crate::state::{FsIo, HolderDir};
use crate::testkit::{FakeClock, FakeDataRoot, MemoryPurger, MemoryStore, TEST_HOLDER_KEY_SEED};
use crate::wire::{ListRef, ReportState, ResourceKey, Rev};
use serde_json::{Map, Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../kwiry/tests/fixtures/holdings/lifecycle-vectors.v1.json"
));
const PROVENANCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../kwiry/tests/fixtures/holdings/lifecycle-vectors.v1.provenance.json"
));
/// The sha256 the vendored fixture must have. A change to the fixture is a
/// deliberate re-vendoring: update this, the provenance note and the
/// registry together.
const FIXTURE_SHA256: &str = "6849181dbf41286ecdc18942a7b6194de3f843733c9d9431b0652600ccfe9da0";

// ------------------------------------------------------------- registry

/// Where a vector is accounted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// Driven through holder code by the named test (in this module tree).
    Run(&'static str),
    /// The holder side lands in a later slice.
    Later {
        slice: &'static str,
        reason: &'static str,
    },
    /// Only the authority can check it: the holder never sees or cannot
    /// compute what it asserts.
    AuthorityOnly(&'static str),
}

/// Cross-cutting checks over artefacts that every list vector carries.
const CROSS_CUTTING: [&str; 2] = ["artifact.jcs", "artifact.state_digest_and_list_tag"];

/// Every vector in the fixture, by dotted path. A path may be split into
/// parts (`path.part`) when its parts land in different buckets.
pub const REGISTRY: &[(&str, Disposition)] = &[
    (
        "keys",
        Disposition::Run(
            "lists::keys_pin_the_authority_set_and_the_holder_thumbprint_signs_every_request",
        ),
    ),
    (
        "artifact.jcs",
        Disposition::Run("lists::every_jcs_input_is_canonical_and_matches_its_signed_list"),
    ),
    (
        "artifact.state_digest_and_list_tag",
        Disposition::AuthorityOnly(
            "HMAC under the authority's digest root (§3.5: the holder cannot compute digests); the holder stores and echoes them",
        ),
    ),
    (
        "fixed_lists.section_3_2",
        Disposition::Run("lists::section_3_2_signed_page"),
    ),
    (
        "fixed_lists.section_3_2_with_pin",
        Disposition::Run("lists::section_3_2_with_pin"),
    ),
    (
        "fixed_lists.section_3_2_route",
        Disposition::Run("lists::section_3_2_route"),
    ),
    (
        "fixed_lists.mixed_pins",
        Disposition::Run("lists::mixed_pins_null_and_pinned"),
    ),
    (
        "fixed_lists.mixed_pins_alternate",
        Disposition::Run("lists::mixed_pins_alternate"),
    ),
    (
        "fixed_lists.two_pages",
        Disposition::Run("lists::two_pages_share_one_nonce"),
    ),
    (
        "cases.V1",
        Disposition::Run(
            "reports::v1 + reports::v1_the_holders_own_report_matches_the_recorded_request",
        ),
    ),
    ("cases.V2", Disposition::Run("reports::v2")),
    ("cases.V3", Disposition::Run("reports::v3")),
    ("cases.V4", Disposition::Run("reports::v4")),
    (
        "cases.V5",
        Disposition::Run(
            "reports::v5 + reports::v5_an_ack_under_another_key_is_convergence_without_the_header",
        ),
    ),
    (
        "cases.V6.lists_and_report",
        Disposition::Run("reports::v6_lists_and_report"),
    ),
    (
        "cases.V6.status_and_release",
        Disposition::Run("status::v6_status_and_release"),
    ),
    ("cases.V7", Disposition::Run("status::v7")),
    ("cases.V8", Disposition::Run("status::v8")),
    ("cases.V9", Disposition::Run("status::v9")),
    ("cases.V10", Disposition::Run("reports::v10")),
    ("cases.V11", Disposition::Run("reports::v11_*")),
    (
        "cases.V12.first_new_epoch_list",
        Disposition::Run("lists::v12_first_new_epoch_list"),
    ),
    (
        "cases.V12.epoch_acceptance",
        Disposition::Later {
            slice: "S5",
            reason: "backup epoch to new epoch by verified chain; interim refusal checked by lists::new_epoch_lists_fail_closed_until_epoch_acceptance",
        },
    ),
    (
        "cases.V12.first_new_epoch_status",
        Disposition::Run("status::v12_first_new_epoch_status"),
    ),
    (
        "cases.V12.expect.new_mint",
        Disposition::AuthorityOnly("capability minting is authority-side (H.1)"),
    ),
    ("cases.V13", Disposition::Run("reports::v13")),
    ("cases.V14", Disposition::Run("lists::v14")),
    (
        "H50",
        Disposition::Run("reports::h50_absence_needs_page_zero_strictly_after_d"),
    ),
    (
        "H53.refused.report",
        Disposition::Run("reports::h53_refused_report"),
    ),
    (
        "H53.refused.status",
        Disposition::Run("status::h53_refused_status"),
    ),
    (
        "H53.accepted_current.report",
        Disposition::Run("reports::h53_accepted_current_report"),
    ),
    (
        "H53.accepted_current.status",
        Disposition::Run("status::h53_accepted_current_status"),
    ),
    (
        "H53.first_current_list",
        Disposition::Later {
            slice: "S5",
            reason: "resolving earlier-epoch attempts at the first current-epoch list needs epoch acceptance (chain in epoch_link); interim refusal checked by lists::new_epoch_lists_fail_closed_until_epoch_acceptance",
        },
    ),
    (
        "H53.expect.mint_gate_opened",
        Disposition::AuthorityOnly("the mint gate is authority-side (H.25)"),
    ),
    (
        "H16_10.pins",
        Disposition::Run("lists::mixed_pins_null_and_pinned"),
    ),
    (
        "H16_10.pin_only_difference",
        Disposition::AuthorityOnly("asserts a state_digest difference (§3.5)"),
    ),
    (
        "H16_10.cursor_across_attestation",
        Disposition::Run("lists::h16_10_cursor_across_attestation"),
    ),
    (
        "H16_10.reattestation",
        Disposition::Run("lists::h16_10_reattestation"),
    ),
    ("H16_10.unpin.list", Disposition::Run("lists::h16_10_unpin")),
    (
        "H16_10.unpin.journal_and_reservation",
        Disposition::AuthorityOnly("pin journal and pin reservation"),
    ),
    (
        "H16_10.aborted_append.list",
        Disposition::Run("lists::h16_10_aborted_append"),
    ),
    (
        "H16_10.aborted_append.journal",
        Disposition::AuthorityOnly("pin journal replay"),
    ),
    (
        "H16_10.replay_refused_reuse",
        Disposition::AuthorityOnly("pin journal replay audit"),
    ),
    (
        "H16_10.attest_refusals",
        Disposition::AuthorityOnly("attestation refusals"),
    ),
    (
        "H16_10.journal_lost.list",
        Disposition::Run("lists::h16_10_journal_lost_list"),
    ),
    (
        "H16_10.journal_lost.epoch_acceptance",
        Disposition::Later {
            slice: "S5",
            reason: "earlier epoch to the restore-journal-lost epoch by verified chain; interim refusal checked by lists::new_epoch_lists_fail_closed_until_epoch_acceptance",
        },
    ),
    (
        "H16_10.journal_lost.recovery",
        Disposition::AuthorityOnly(
            "restore record, reset journal, pin-history projection, prior completion and readiness",
        ),
    ),
    (
        "H16_10.orphaned",
        Disposition::AuthorityOnly("orphaned pin reservation"),
    ),
    (
        "H16_10.missing_id_space",
        Disposition::AuthorityOnly("journal chain grammar"),
    ),
    (
        "must_reject.AP17_missing",
        Disposition::Run("lists::must_reject_ap17_missing"),
    ),
    (
        "must_reject.AP17_malformed_pin_id",
        Disposition::Run("lists::must_reject_ap17_malformed_pin_id"),
    ),
    (
        "must_reject.AP17_malformed_pin_ref",
        Disposition::Run("lists::must_reject_ap17_malformed_pin_ref"),
    ),
    (
        "must_reject.H16_unknown_pin_member",
        Disposition::Run("lists::must_reject_h16_unknown_pin_member"),
    ),
    (
        "must_reject.H16_half_null_id",
        Disposition::Run("lists::must_reject_h16_half_null_id"),
    ),
    (
        "must_reject.H16_half_null_ref",
        Disposition::Run("lists::must_reject_h16_half_null_ref"),
    ),
    (
        "must_reject.H16_half_absent_id",
        Disposition::Run("lists::must_reject_h16_half_absent_id"),
    ),
    (
        "must_reject.H16_half_absent_ref",
        Disposition::Run("lists::must_reject_h16_half_absent_ref"),
    ),
    (
        "must_reject.H16_pin_on_non_room",
        Disposition::Run("lists::must_reject_h16_pin_on_non_room"),
    ),
    (
        "must_reject.H16_pin_on_non_engine",
        Disposition::Run("lists::must_reject_h16_pin_on_non_engine"),
    ),
    (
        "must_reject.H35_unknown_top_member",
        Disposition::Run("lists::must_reject_h35_unknown_top_member"),
    ),
    (
        "must_reject.H35_duplicate_pin_member",
        Disposition::Run("lists::must_reject_h35_duplicate_pin_member"),
    ),
    (
        "must_reject.H35_duplicate_header_member",
        Disposition::Run("lists::must_reject_h35_duplicate_header_member"),
    ),
    (
        "must_reject.H35_unknown_item_member",
        Disposition::Run("lists::must_reject_h35_unknown_item_member"),
    ),
    (
        "must_reject.H35_missing_subject",
        Disposition::Run("lists::must_reject_h35_missing_subject"),
    ),
    (
        "must_reject.H35_wrong_type",
        Disposition::Run("lists::must_reject_h35_wrong_type"),
    ),
    (
        "must_reject.bad_signature",
        Disposition::Run("lists::must_reject_bad_signature"),
    ),
    (
        "must_reject.wrong_issuer",
        Disposition::Run("lists::must_reject_wrong_issuer"),
    ),
    (
        "must_reject.wrong_audience",
        Disposition::Run("lists::must_reject_wrong_audience"),
    ),
    (
        "must_reject.wrong_subject",
        Disposition::Run("lists::must_reject_wrong_subject"),
    ),
    (
        "must_reject.wrong_nonce",
        Disposition::Run("lists::must_reject_wrong_nonce"),
    ),
    (
        "must_reject.expired",
        Disposition::Run("lists::must_reject_expired"),
    ),
    (
        "must_reject.future_iat",
        Disposition::Run("lists::must_reject_future_iat"),
    ),
    (
        "must_reject.rollback_seq",
        Disposition::Run("lists::must_reject_rollback_seq"),
    ),
    (
        "must_reject.impossible_page_position",
        Disposition::Run("lists::must_reject_impossible_page_position"),
    ),
    (
        "must_reject.repeated_holding",
        Disposition::Run("lists::must_reject_repeated_holding"),
    ),
    (
        "must_reject.terminating_without_obligation",
        Disposition::Run("lists::must_reject_terminating_without_obligation"),
    ),
];

/// Fixture paths that are indexes into other vectors, not vectors.
const ALIASES: [&str; 1] = ["AP17"];

/// The top-level fixture members that hold no vectors.
const METADATA: [&str; 4] = ["schema", "status", "wire_profile", "clock_base"];

fn vector_roots() -> Vec<String> {
    let mut roots = Vec::new();
    for (name, value) in fixture().as_object().expect("fixture object") {
        match name.as_str() {
            name if METADATA.contains(&name) || ALIASES.contains(&name) => {}
            "keys" | "H50" | "H53" => roots.push(name.to_owned()),
            group => {
                for member in value.as_object().expect("vector group").keys() {
                    roots.push(format!("{group}.{member}"));
                }
            }
        }
    }
    roots
}

#[test]
fn the_registry_accounts_for_every_vector_exactly_once() {
    let roots = vector_roots();
    let ids: Vec<&str> = REGISTRY.iter().map(|(id, _)| *id).collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "a registry id appears twice");
    for root in &roots {
        assert!(
            ids.iter()
                .any(|id| id == root || id.starts_with(&format!("{root}."))),
            "vector {root} is not accounted for"
        );
    }
    for id in &ids {
        if CROSS_CUTTING.contains(id) {
            continue;
        }
        assert!(
            roots
                .iter()
                .any(|root| id == root || id.starts_with(&format!("{root}."))),
            "registry id {id} names no vector in the fixture"
        );
        // A split root is split all the way: never both whole and in parts.
        let whole = roots.iter().any(|root| root == id);
        let parted = ids.iter().any(|other| other.starts_with(&format!("{id}.")));
        assert!(!(whole && parted), "{id} is registered whole and in parts");
    }
    // The alias index names only must-reject vectors.
    for alias in ALIASES {
        for target in fixture()[alias].as_array().expect("alias list") {
            let target = target.as_str().expect("alias target");
            assert!(
                fixture()["must_reject"].get(target).is_some(),
                "{alias} names {target}"
            );
        }
    }
}

#[test]
fn ledger() {
    let (mut run, mut later, mut authority) = (0, 0, 0);
    for (id, disposition) in REGISTRY {
        match disposition {
            Disposition::Run(test) => {
                run += 1;
                println!("RUN       {id:45} {test}");
            }
            Disposition::Later { slice, reason } => {
                later += 1;
                println!("LATER {slice:3} {id:45} {reason}");
            }
            Disposition::AuthorityOnly(reason) => {
                authority += 1;
                println!("AUTHORITY {id:45} {reason}");
            }
        }
    }
    println!(
        "vectors: {} total, {run} run, {later} later slices, {authority} authority-only",
        REGISTRY.len()
    );
    assert_eq!(run + later + authority, REGISTRY.len());
}

// -------------------------------------------------------- fixture access

pub fn fixture() -> &'static Value {
    static PARSED: OnceLock<Value> = OnceLock::new();
    PARSED.get_or_init(|| serde_json::from_str(FIXTURE).expect("fixture is JSON"))
}

pub fn provenance() -> &'static Value {
    static PARSED: OnceLock<Value> = OnceLock::new();
    PARSED.get_or_init(|| serde_json::from_str(PROVENANCE).expect("provenance is JSON"))
}

/// The fixture value at a dotted path; a string value that names another
/// case (`"V1.initial_list"`) is followed.
#[track_caller]
pub fn at(path: &str) -> &'static Value {
    let value = path.split('.').fold(fixture(), |value, member| {
        match (value, member.parse::<usize>()) {
            (Value::Array(items), Ok(index)) => &items[index],
            _ => &value[member],
        }
    });
    assert!(!value.is_null(), "no vector at {path}");
    follow(value)
}

/// Follows a by-name reference to another case.
pub fn follow(value: &'static Value) -> &'static Value {
    match value.as_str() {
        Some(reference)
            if fixture()["cases"]
                .get(reference.split('.').next().unwrap_or(""))
                .is_some() =>
        {
            at(&format!("cases.{reference}"))
        }
        _ => value,
    }
}

#[track_caller]
pub fn text(value: &Value) -> &str {
    value.as_str().expect("string")
}

/// The claims of a compact JWS, decoded leniently (duplicate members keep
/// the last value). Used only to read a vector, never to verify one.
#[track_caller]
pub fn claims(compact: &str) -> Map<String, Value> {
    let payload = compact.split('.').nth(1).expect("payload segment");
    serde_json::from_slice(&b64::decode(payload).expect("base64url")).expect("claims")
}

#[track_caller]
pub fn header(compact: &str) -> Map<String, Value> {
    let header = compact.split('.').next().expect("header segment");
    serde_json::from_slice(&b64::decode(header).expect("base64url")).expect("header")
}

pub fn issuer() -> &'static str {
    text(&fixture()["keys"]["issuer"])
}

pub fn authority_jwks() -> Value {
    json!({ "keys": fixture()["keys"]["authority_jwks"] })
}

fn wire(name: &str) -> &'static str {
    text(&provenance()["wire_profile"][name])
}

fn wire_list(name: &str) -> Vec<String> {
    provenance()["wire_profile"][name]
        .as_array()
        .expect(name)
        .iter()
        .map(|v| text(v).to_owned())
        .collect()
}

/// The wire profile the fixture was generated under. With `engine`, the
/// search-engine holder profile; without, the generic core alone.
pub fn profile(engine: bool) -> WireProfile {
    WireProfile {
        typ_list_request: wire("typ_list_request").into(),
        typ_list_page: wire("typ_list_page").into(),
        typ_keyset: wire("typ_keyset").into(),
        typ_report_request: wire("typ_report_request").into(),
        typ_report_ack: wire("typ_report_ack").into(),
        typ_status_request: wire("typ_status_request").into(),
        typ_status_ack: wire("typ_status_ack").into(),
        outcome_header: wire("outcome_header").into(),
        audience_prefix: wire("audience_prefix").into(),
        authority_key_id_prefix: wire("authority_key_id_prefix").into(),
        resource_kinds: wire_list("resource_kinds"),
        accepted_resource_kinds: wire_list("accepted_resource_kinds"),
        obligation_reasons: wire_list("obligation_reasons"),
        engine: engine.then(|| EngineProfile {
            holder_kind: wire("engine_holder_kind").into(),
            pinned_resource_kind: wire("engine_pinned_resource_kind").into(),
        }),
    }
}

pub fn engine_kind() -> &'static str {
    wire("engine_holder_kind")
}

#[test]
fn the_fixture_matches_its_recorded_sha256_and_provenance() {
    assert_eq!(
        sha256_tagged(FIXTURE.as_bytes()),
        format!("sha256:{FIXTURE_SHA256}")
    );
    assert_eq!(text(&provenance()["sha256"]), FIXTURE_SHA256);
    assert_eq!(text(&provenance()["fixture"]), "lifecycle-vectors.v1.json");
    let commit = text(&provenance()["source"]["commit"]);
    assert!(commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(text(&fixture()["status"]).contains("TEST ONLY"));
}

/// The fixture carries no private key material anywhere, embedded JWS
/// payloads and headers included.
#[test]
fn the_fixture_carries_only_public_test_keys() {
    fn walk(value: &Value, seen: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                for (name, member) in object {
                    if ["d", "p", "q", "dp", "dq", "qi", "k", "oth"].contains(&name.as_str()) {
                        seen.push(name.clone());
                    }
                    walk(member, seen);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| walk(item, seen)),
            Value::String(text) => {
                for token in text.split(|c: char| !(c.is_ascii_alphanumeric() || "-_.".contains(c)))
                {
                    let parts: Vec<&str> = token.split('.').collect();
                    if parts.len() == 3 && parts[0].starts_with("ey") {
                        for part in &parts[..2] {
                            if let Some(inner) = b64::decode(part)
                                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                            {
                                walk(&inner, seen);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut private = Vec::new();
    walk(fixture(), &mut private);
    assert!(private.is_empty(), "private key members: {private:?}");
}

/// The provenance note's wire constants agree with the fixture's own bytes.
#[test]
fn the_wire_profile_agrees_with_the_fixture() {
    let wire = profile(true);
    wire.validate().expect("a coherent profile");
    let mut exchanges = Vec::new();
    collect_exchanges(fixture(), &mut exchanges);
    assert!(exchanges.len() > 30, "found {}", exchanges.len());
    let known_requests = [
        wire.list_request_content_type(),
        wire.report_request_content_type(),
        wire.status_request_content_type(),
    ];
    let mut checked = 0;
    for exchange in exchanges {
        let request_type = text(&exchange["request_content_type"]);
        let request_header = header(text(&exchange["request"]));
        assert_eq!(
            request_type,
            format!("application/{}", text(&request_header["typ"]))
        );
        assert!(
            known_requests.iter().any(|known| known == request_type),
            "unknown request media type {request_type}"
        );
        checked += 1;
        let request_claims = claims(text(&exchange["request"]));
        assert_eq!(text(&request_claims["aud"]), issuer());
        assert!(text(&exchange["url"]).starts_with(issuer()));
        assert_eq!(text(&request_claims["htu"]), text(&exchange["url"]));
        let response = &exchange["response"];
        if response["status"] == 200 {
            let body = text(&response["body"]);
            let answer_header = header(body);
            let typ = text(&answer_header["typ"]);
            let content_type = text(&response["content_type"]);
            assert_eq!(content_type, format!("application/{typ}"));
            assert!(
                typ == wire.typ_list_page
                    || typ == wire.typ_report_ack
                    || typ == wire.typ_status_ack,
                "unexpected answer typ {typ}"
            );
            let answer = claims(body);
            assert_eq!(
                text(&answer["aud"]),
                wire.holder_audience(text(&request_claims["sub"]))
            );
            assert!(text(&answer_header["kid"]).starts_with(&wire.authority_key_id_prefix));
        } else {
            assert_eq!(text(&response["content_type"]), PROBLEM_CONTENT_TYPE);
        }
    }
    assert!(checked > 30, "checked {checked}");
}

/// Every recorded request/response pair in the fixture, at any depth.
pub fn collect_exchanges(value: &'static Value, out: &mut Vec<&'static Value>) {
    match value {
        Value::Object(object) if object.contains_key("request_content_type") => out.push(value),
        Value::Object(object) => object.values().for_each(|v| collect_exchanges(v, out)),
        Value::Array(items) => items.iter().for_each(|v| collect_exchanges(v, out)),
        _ => {}
    }
}

// ------------------------------------------------------------ transport

/// One scripted answer: the URL the holder must have called (if the vector
/// records it) and the recorded response.
pub struct Scripted {
    pub url: Option<String>,
    pub response: HttpResponse,
}

#[derive(Default)]
pub struct Wire {
    answers: VecDeque<Scripted>,
    /// Every POST: URL, content type and exact body.
    pub posts: Vec<(String, String, Vec<u8>)>,
    pub gets: Vec<String>,
}

/// A transport that replays recorded authority answers in order.
#[derive(Clone, Default)]
pub struct Replay(pub Arc<Mutex<Wire>>);

impl Replay {
    pub fn script(&self, answers: Vec<Scripted>) {
        self.wire().answers.extend(answers);
    }

    pub fn wire(&self) -> std::sync::MutexGuard<'_, Wire> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn unused(&self) -> usize {
        self.wire().answers.len()
    }

    /// The claims of every POST the holder made, in order.
    pub fn posted_claims(&self) -> Vec<Map<String, Value>> {
        self.wire()
            .posts
            .iter()
            .map(|(_, _, body)| claims(std::str::from_utf8(body).expect("compact JWS")))
            .collect()
    }
}

impl Transport for Replay {
    fn post(
        &self,
        url: &str,
        content_type: &str,
        body: &[u8],
        _timeouts: Timeouts,
    ) -> Result<HttpResponse, TransportError> {
        let mut wire = self.wire();
        wire.posts
            .push((url.to_owned(), content_type.to_owned(), body.to_vec()));
        let answer = wire
            .answers
            .pop_front()
            .ok_or(TransportError::Unreachable)?;
        if let Some(expected) = &answer.url {
            assert_eq!(url, expected, "the holder called the recorded route");
        }
        Ok(answer.response)
    }

    fn get(&self, url: &str, _timeouts: Timeouts) -> Result<HttpResponse, TransportError> {
        self.wire().gets.push(url.to_owned());
        Ok(HttpResponse {
            status: 404,
            content_type: Some(PROBLEM_CONTENT_TYPE.into()),
            body: br#"{"code":"not_found"}"#.to_vec(),
            ..HttpResponse::default()
        })
    }
}

/// The recorded response of an exchange, as the holder's transport returns
/// it. The vector's `outcome` is the value of the profile's outcome header.
pub fn response(exchange: &Value) -> HttpResponse {
    let recorded = &exchange["response"];
    let headers = recorded["outcome"]
        .as_str()
        .map(|outcome| vec![(wire("outcome_header").to_owned(), outcome.to_owned())])
        .unwrap_or_default();
    HttpResponse {
        status: u16::try_from(recorded["status"].as_u64().expect("status")).expect("u16"),
        content_type: recorded["content_type"].as_str().map(str::to_owned),
        retry_after_s: recorded["retry_after"].as_u64(),
        headers,
        body: text(&recorded["body"]).as_bytes().to_vec(),
    }
}

pub fn scripted(exchange: &Value) -> Scripted {
    Scripted {
        url: Some(text(&exchange["url"]).to_owned()),
        response: response(exchange),
    }
}

// ---------------------------------------------------------- golden lists

/// A list vector in either of its two shapes: a route exchange per page, or
/// a single signed page.
pub struct GoldenList {
    pub pages: Vec<String>,
    pub answers: Vec<Scripted>,
}

impl GoldenList {
    pub fn of(value: &'static Value) -> Self {
        let value = follow(value);
        if let Some(page) = value.get("signed_page") {
            let page = text(page).to_owned();
            return Self {
                answers: vec![Scripted {
                    url: None,
                    response: HttpResponse {
                        status: 200,
                        content_type: Some(profile(true).list_page_content_type()),
                        body: page.as_bytes().to_vec(),
                        ..HttpResponse::default()
                    },
                }],
                pages: vec![page],
            };
        }
        let exchanges = value["pages"].as_array().expect("pages");
        Self {
            pages: exchanges
                .iter()
                .map(|e| text(&e["response"]["body"]).to_owned())
                .collect(),
            answers: exchanges.iter().map(scripted).collect(),
        }
    }

    pub fn page0(&self) -> Map<String, Value> {
        claims(&self.pages[0])
    }

    pub fn holder_id(&self) -> String {
        text(&self.page0()["sub"]).to_owned()
    }

    pub fn nonce(&self) -> String {
        text(&self.page0()["nonce"]).to_owned()
    }

    pub fn iat_s(&self) -> i64 {
        self.page0()["iat"].as_i64().expect("iat")
    }

    pub fn rev(&self) -> Rev {
        rev_of(&self.page0()["rev"])
    }

    pub fn list_ref(&self) -> ListRef {
        list_ref_of(&Value::Object(self.page0()))
    }
}

pub fn rev_of(value: &Value) -> Rev {
    Rev {
        epoch: text(&value["epoch"]).to_owned(),
        seq: value["seq"].as_u64().expect("seq"),
    }
}

pub fn list_ref_of(value: &Value) -> ListRef {
    ListRef {
        list_id: text(&value["list_id"]).to_owned(),
        rev: rev_of(&value["rev"]),
        nonce: text(&value["nonce"]).to_owned(),
        state_digest: text(&value["state_digest"]).to_owned(),
        list_tag: text(&value["list_tag"]).to_owned(),
    }
}

/// Every list vector in the fixture, by path.
pub fn every_list() -> Vec<(String, &'static Value)> {
    fn walk(path: &str, value: &'static Value, out: &mut Vec<(String, &'static Value)>) {
        let Value::Object(object) = value else {
            return;
        };
        let is_list = object.contains_key("digest_jcs")
            && (object.contains_key("pages") || object.contains_key("signed_page"));
        if is_list {
            out.push((path.to_owned(), value));
            return;
        }
        for (name, member) in object {
            let path = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}.{name}")
            };
            walk(&path, member, out);
        }
    }
    let mut out = Vec::new();
    walk("", fixture(), &mut out);
    out
}

// ------------------------------------------------------------------ rig

/// One enrolled holder under the fixture's authority key set, wired to a
/// replaying transport, a fake clock, a memory store and a memory purger.
pub struct GoldenRig {
    pub holder: Holder<MemoryStore, Replay>,
    pub clock: Arc<FakeClock>,
    pub events: Arc<MemorySink>,
    pub store: MemoryStore,
    pub purger: MemoryPurger,
    pub transport: Replay,
    dir: tempfile::TempDir,
}

impl GoldenRig {
    /// A search-engine holder enrolled as `holder_id`, its wall clock at
    /// `wall_s`.
    pub fn engine(holder_id: &str, wall_s: i64) -> Self {
        Self::enrolled(holder_id, engine_kind(), profile(true), wall_s)
    }

    /// A holder of `holder_kind` under `profile`.
    pub fn enrolled(holder_id: &str, holder_kind: &str, profile: WireProfile, wall_s: i64) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("holder");
        let clock = Arc::new(FakeClock::new(wall_s * 1000, 1_000_000));
        // The holder's own key is a local test key: the authority binds a
        // holder by `holder_id`, and nothing here re-sends recorded requests.
        let key = HolderKey::from_seed(TEST_HOLDER_KEY_SEED);
        {
            let holder_dir = HolderDir::open(&path, &[]).expect("holder dir");
            startup::write_key(&holder_dir, &FsIo, &key).expect("key");
            startup::enroll(
                holder_dir,
                Box::new(FsIo),
                clock.clone(),
                &EnrollParams {
                    holder_id,
                    holder_kind,
                    issuer: issuer(),
                    jwks: &authority_jwks(),
                    authority_key_id_prefix: &profile.authority_key_id_prefix,
                },
            )
            .expect("enroll");
        }
        let events = Arc::new(MemorySink::default());
        let started = startup::start(
            HolderDir::open(&path, &[]).expect("holder dir"),
            Box::new(FsIo),
            clock.clone(),
            &FakeDataRoot::default(),
            events.as_ref(),
        )
        .expect("start");
        let engine = profile
            .engine
            .as_ref()
            .map(|_| EngineSettings::new(30, DEFAULT_STALE_SERVE_S).expect("engine settings"));
        let config = HolderConfig {
            profile,
            page_size: DEFAULT_PAGE_SIZE,
            engine,
        };
        let store = MemoryStore::default();
        let transport = Replay::default();
        let holder = Holder::new(
            started,
            config,
            store.clone(),
            transport.clone(),
            clock.clone(),
            events.clone(),
        )
        .expect("holder");
        Self {
            holder,
            clock,
            events,
            purger: MemoryPurger::for_store(&store),
            store,
            transport,
            dir,
        }
    }

    /// An engine holder for the list's subject, its clock at the list's
    /// page-0 `iat`, with the list applied.
    pub fn applied(list: &'static Value) -> Self {
        let golden = GoldenList::of(list);
        let mut rig = Self::engine(&golden.holder_id(), golden.iat_s());
        let outcome = rig.deliver(list);
        assert!(
            matches!(outcome, ListOutcome::Applied(_)),
            "the list applies: {outcome:?}"
        );
        rig
    }

    pub fn set_wall(&self, wall_s: i64) {
        use crate::clock::Clock;
        self.clock.step_wall(wall_s * 1000 - self.clock.wall_ms());
    }

    /// Moves only the monotonic clock.
    pub fn advance_mono(&self, ms: u64) {
        self.clock.advance(ms);
        self.clock
            .step_wall(-i64::try_from(ms).expect("small advance"));
    }

    /// One `list_once` at `wall_s`: the holder signs its request, the
    /// transport answers with `answers` in order and the session nonces are
    /// the recorded ones.
    pub fn list_with(
        &mut self,
        nonces: &[&str],
        answers: Vec<Scripted>,
        wall_s: i64,
    ) -> ListOutcome {
        use crate::clock::Clock;
        self.set_wall(wall_s);
        let now = self.clock.mono_ms();
        let start_at = self.holder.scheduler().next_start_at();
        self.advance_mono(
            start_at
                .saturating_sub(now)
                .max(crate::schedule::LIST_START_SPACING_MS),
        );
        for nonce in nonces {
            crate::client::scripted_nonces::push(nonce);
        }
        self.transport.script(answers);
        let outcome = self.holder.list_once();
        assert_eq!(
            crate::client::scripted_nonces::pending(),
            0,
            "every recorded nonce was used"
        );
        outcome
    }

    /// Delivers a list vector at its page-0 `iat`.
    pub fn deliver(&mut self, list: &'static Value) -> ListOutcome {
        let iat = GoldenList::of(list).iat_s();
        self.deliver_at(list, iat)
    }

    pub fn deliver_at(&mut self, list: &'static Value, wall_s: i64) -> ListOutcome {
        let golden = GoldenList::of(list);
        let outcome = self.list_with(&[&golden.nonce()], golden.answers, wall_s);
        assert_eq!(
            self.transport.unused(),
            0,
            "every recorded page was requested"
        );
        outcome
    }

    pub fn codes(&self) -> Vec<&'static str> {
        self.events.codes()
    }

    /// The engine gate's answer for a pinned resource with an artifact
    /// stamped `pin_ref` and a matching root.
    pub fn serve_pinned(&self, id: &str, pin_ref: &str) -> Result<(), Refusal> {
        self.holder.engine_gate().expect("engine profile").check(
            &EngineQuery {
                resource_id: id,
                capability: None,
                artifact: Some(ArtifactStamp {
                    pin_ref,
                    validated_under_matching_root: true,
                }),
                root: RootObservation::Matches,
            },
            Cut::Admission,
        )
    }

    /// The engine gate's answer to a request carrying a 60 s capability
    /// issued at `now_s`, with no artifact.
    pub fn serve_with_capability(&self, id: &str, now_s: i64) -> Result<(), Refusal> {
        self.holder.engine_gate().expect("engine profile").check(
            &EngineQuery {
                resource_id: id,
                capability: Some(crate::engine::CapabilityTimes {
                    iat_s: now_s,
                    exp_s: now_s + 60,
                }),
                artifact: None,
                root: RootObservation::Matches,
            },
            Cut::Admission,
        )
    }

    /// The engine gate's answer with no artifact (any pinned resource).
    pub fn serve_unstamped(&self, id: &str) -> Result<(), Refusal> {
        self.holder.engine_gate().expect("engine profile").check(
            &EngineQuery {
                resource_id: id,
                capability: None,
                artifact: None,
                root: RootObservation::Matches,
            },
            Cut::Admission,
        )
    }

    pub fn floor(&self, obligation_id: &str) -> Option<ReportState> {
        let state = self.holder.state();
        let epoch = &state.high_water.as_ref()?.epoch;
        state
            .obligations
            .channels
            .get(obligation_id)?
            .floor_in(epoch)
    }

    pub fn pending(&self, obligation_id: &str) -> Option<Pending> {
        self.holder
            .state()
            .obligations
            .channels
            .get(obligation_id)?
            .pending
            .clone()
    }

    pub fn files(&self, prefix: &str) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir.path().join("holder"))
            .expect("holder dir")
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with(prefix))
            .collect();
        names.sort();
        names
    }

    pub fn file(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.dir.path().join("holder").join(name)).expect("holder file")
    }

    /// Records, as holder-local state, a report the vector says this holder
    /// sent: one pending report under the request's key with one attempt
    /// per recorded request, and (unless `sent_lost`) each request's exact
    /// bytes, as the holder writes them before the first byte.
    pub fn record_sent(&mut self, requests: &[&str], sent_lost: bool) -> Pending {
        let pending = pending_from(requests, sent_lost);
        let ob = pending.body.obligation_id.clone();
        let resource = self.listed_resource(&ob);
        let mut book = self.holder.state().obligations.clone();
        book.channels
            .entry(ob)
            .or_insert_with(|| Channel::new(&pending.body.holding_id, resource))
            .pending = Some(pending.clone());
        self.holder
            .writer
            .update_obligations(book)
            .expect("record the pending report");
        if !sent_lost {
            for compact in requests {
                self.holder
                    .writer
                    .write_side_file(
                        &crate::report::sent_file(&sha256_tagged(compact.as_bytes())),
                        compact.as_bytes(),
                    )
                    .expect("sent bytes");
            }
        }
        pending
    }

    /// The resource the applied list names for an obligation.
    pub fn listed_resource(&self, obligation_id: &str) -> ResourceKey {
        self.holder
            .applied
            .as_ref()
            .expect("a list applied in this process")
            .items
            .iter()
            .find(|item| {
                item.obligation
                    .as_ref()
                    .is_some_and(|o| o.obligation_id == obligation_id)
            })
            .map(|item| item.resource.clone())
            .expect("the applied list names the obligation")
    }

    /// Classifies a recorded answer to the pending report.
    pub fn answer(&mut self, pending: &Pending, exchange: &Value) -> Answer {
        let ob = pending.body.obligation_id.clone();
        self.holder.classify(&ob, pending, Ok(response(exchange)))
    }
}

/// The holder-local report record for `requests` (all under one key).
pub fn pending_from(requests: &[&str], sent_lost: bool) -> Pending {
    let first = claims(requests[0]);
    let key = text(&first["idempotency_key"]).to_owned();
    for request in requests {
        let mut claims = claims(request);
        assert_eq!(text(&claims["idempotency_key"]), key, "one key");
        let mut a = first.clone();
        for volatile in ["jti", "iat", "exp"] {
            a.remove(volatile);
            claims.remove(volatile);
        }
        assert_eq!(a, claims, "attempts of one key carry one body");
    }
    let state = ReportState::from_wire(text(&first["state"])).expect("report state");
    let counts = &first["counts"];
    let body = ReportBody {
        obligation_id: text(&first["obligation_id"]).to_owned(),
        holding_id: text(&first["holding_id"]).to_owned(),
        state,
        list_ref: list_ref_of(&first["list_ref"]),
        counts: Counts {
            units_purged: counts["units_purged"].as_u64().expect("count"),
            units_remaining: counts["units_remaining"].as_u64().expect("count"),
            quarantined_unattributed: counts["quarantined_unattributed"].as_u64().expect("count"),
            quarantine_manifest_digest: text(&first["quarantine_manifest_digest"]).to_owned(),
        },
        completion: first.get("completed_at").map(|completed_at| Completion {
            method: text(&first["method"]).to_owned(),
            completed_at: text(completed_at).to_owned(),
            evidence_digest: first
                .get("evidence")
                .map_or_else(String::new, |e| text(&e["digest"]).to_owned()),
        }),
    };
    let attempts = if sent_lost {
        Vec::new()
    } else {
        requests.iter().map(|compact| attempt_of(compact)).collect()
    };
    Pending {
        key,
        epoch: body.list_ref.rev.epoch.clone(),
        body,
        status: PendingStatus::Armed,
        attempts,
    }
}

pub fn attempt_of(compact: &str) -> Attempt {
    let claims = claims(compact);
    Attempt {
        jti: text(&claims["jti"]).to_owned(),
        digest: sha256_tagged(compact.as_bytes()),
        exp_s: claims["exp"].as_i64().expect("exp"),
    }
}

#[track_caller]
pub fn failed(outcome: ListOutcome) -> FetchFailure {
    match outcome {
        ListOutcome::Failed(failure) => failure,
        other => panic!("expected a refused list, got {other:?}"),
    }
}

pub fn exchange_request(exchange: &Value) -> &str {
    text(&exchange["request"])
}

pub fn route_list_url() -> String {
    format!("{}{ROUTE_LIST}", issuer())
}
