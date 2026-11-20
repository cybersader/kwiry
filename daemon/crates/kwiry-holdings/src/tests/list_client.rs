//! List fetch, verification order, pagination, retries and key rotation.

use super::*;
use crate::client::{FetchFailure, HttpResponse};
use crate::holder::{AuthHalt, ListOutcome, RouteHealth};
use crate::profile::PROBLEM_CONTENT_TYPE;
use crate::testkit::Forced;
use crate::verify::PageError;
use serde_json::{Value, json};

fn problem(status: u16, code: &str) -> Forced {
    Forced::Response(HttpResponse {
        status,
        content_type: Some(PROBLEM_CONTENT_TYPE.into()),
        retry_after_s: (status == 429).then_some(7),
        headers: Vec::new(),
        body: json!({ "status": status, "code": code })
            .to_string()
            .into_bytes(),
    })
}

#[test]
fn the_fifth_consecutive_410_is_a_livelock_and_applies_nothing() {
    let (_dir, mut rig) = rig();
    rig.config.page_size = 1;
    rig.restart().unwrap();
    rig.authority.state().add_active(PLAIN, "res-a");
    rig.authority.state().add_active(PLAIN, "res-b");
    for n in 0..5 {
        rig.authority.state().before_post(2 * n + 2, |s| s.commit());
    }
    assert_eq!(failed(rig.holder().list_once()), FetchFailure::Livelock);
    assert!(codes(&rig).contains(&"lifecycle_list_livelock"));
    assert!(rig.store.view().published.is_empty());
    assert_eq!(rig.holder().state().high_water, None);

    // Four 410s are still within the limit.
    let (_dir, mut rig) = super::rig();
    rig.config.page_size = 1;
    rig.restart().unwrap();
    rig.authority.state().add_active(PLAIN, "res-a");
    rig.authority.state().add_active(PLAIN, "res-b");
    for n in 0..4 {
        rig.authority.state().before_post(2 * n + 2, |s| s.commit());
    }
    applied(rig.holder().list_once());
}

#[test]
fn h17_an_unverifiable_witness_is_ignored_and_the_list_served() {
    let (_dir, mut rig) = rig();
    rig.authority.state().add_active(PLAIN, "res-a");
    applied(rig.holder().list_once());
    rig.authority.state().digest_root = [0x99; 32];
    applied(rig.relist());
    assert_eq!(rig.authority.state().audit, ["witness_unverifiable"]);
    assert!(
        rig.authority
            .state()
            .requests
            .iter()
            .all(|r| r.status == 200)
    );
}

/// The golden must-reject set covers another holder's `aud` and `sub` and
/// another `iss`; the broadcast audience is not in it.
#[test]
fn h6_the_broadcast_audience_never_binds_a_list_page() {
    let (_dir, mut rig) = rig();
    rig.authority.state().add_active(PLAIN, "res-a");
    rig.authority.state().hooks.page = Some(Box::new(|page| {
        page.insert("aud".into(), "example-lifecycle:*".into());
    }));
    assert_eq!(
        failed(rig.holder().list_once()),
        FetchFailure::Invalid(PageError::Audience)
    );
    assert_eq!(plain(&rig, "res-a"), Err(Refusal::NoListThisProcess));
}

#[test]
fn page_iat_must_be_within_120_seconds_and_exp_in_the_future() {
    for (offset_ms, ok) in [
        (120_000, true),
        (121_000, false),
        (-120_000, true),
        (-121_000, false),
    ] {
        let (_dir, mut rig) = rig();
        rig.authority.state().add_active(PLAIN, "res-a");
        rig.authority.state().offset_ms = offset_ms;
        let outcome = rig.holder().list_once();
        if ok {
            applied(outcome);
        } else {
            // A 121 s offset also fails the authority's own request check;
            // either way, nothing applies and nothing halts.
            assert!(matches!(
                failed(outcome),
                FetchFailure::Invalid(PageError::Time) | FetchFailure::ProofExpiredTwice
            ));
            assert_eq!(rig.holder().route_health(), RouteHealth::Normal);
        }
    }
    // The page's own iat, with the authority clock in agreement.
    for (shift, ok) in [(120, true), (121, false), (-120, true), (-121, false)] {
        let (_dir, mut rig) = rig();
        rig.authority.state().add_active(PLAIN, "res-a");
        rig.authority.state().hooks.page = Some(Box::new(move |page| {
            let iat = page["iat"].as_i64().unwrap();
            page.insert("iat".into(), (iat + shift).into());
        }));
        let outcome = rig.holder().list_once();
        if ok {
            applied(outcome);
        } else {
            assert_eq!(
                failed(outcome),
                FetchFailure::Invalid(PageError::Time),
                "{shift}"
            );
        }
    }
    let (_dir, mut rig) = rig();
    rig.authority.state().add_active(PLAIN, "res-a");
    rig.authority.state().hooks.page = Some(Box::new(|page| {
        let iat = page["iat"].as_i64().unwrap();
        page.insert("exp".into(), iat.into());
    }));
    assert_eq!(
        failed(rig.holder().list_once()),
        FetchFailure::Invalid(PageError::Time)
    );
}

#[test]
fn h12_every_list_route_answer_has_one_holder_action() {
    let cases: Vec<(Forced, FetchFailure, &str)> = vec![
        (
            problem(503, "lifecycle_unavailable"),
            FetchFailure::LifecycleUnavailable {
                retry_after_s: None,
            },
            "lifecycle_unreachable",
        ),
        (
            problem(429, "rate_limited"),
            FetchFailure::Unreachable {
                retry_after_s: Some(7),
            },
            "lifecycle_unreachable",
        ),
        (
            Forced::Unreachable,
            FetchFailure::Unreachable {
                retry_after_s: None,
            },
            "lifecycle_unreachable",
        ),
        (
            Forced::Timeout,
            FetchFailure::Deadline,
            "lifecycle_list_deadline",
        ),
        (
            problem(400, "invalid_request"),
            FetchFailure::RequestInvalid { status: 400 },
            "lifecycle_list_request_invalid",
        ),
        (
            problem(413, "payload_too_large"),
            FetchFailure::RequestInvalid { status: 413 },
            "lifecycle_list_request_invalid",
        ),
        (
            problem(409, "stale_list_ref"),
            FetchFailure::Rejected { status: 409 },
            "lifecycle_list_rejected",
        ),
        (
            problem(503, "authority_epoch_mismatch"),
            FetchFailure::RollbackSuspected,
            "authority_rollback_suspected",
        ),
    ];
    for (forced, expected, code) in cases {
        let (_dir, mut rig) = rig();
        rig.authority.state().forced.push_back(forced);
        assert_eq!(failed(rig.holder().list_once()), expected);
        assert_eq!(codes(&rig), [code]);
        assert_eq!(rig.holder().route_health(), RouteHealth::Normal);
        applied(rig.relist());
    }
}

#[test]
fn h12_a_bad_holder_proof_halts_traffic_but_serving_runs_down_its_lease() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-a", true, 1);
    rig.authority.state().add_active(PLAIN, "res-a");
    applied(rig.holder().list_once());
    rig.authority
        .state()
        .forced
        .push_back(problem(401, "invalid_holder_proof"));
    assert_eq!(failed(rig.relist()), FetchFailure::Unauthorized);
    assert_eq!(
        rig.holder().route_health(),
        RouteHealth::AuthHalt(AuthHalt::InvalidProof)
    );
    let posts = rig.authority.state().posts();
    assert_eq!(rig.relist(), ListOutcome::AuthHalted);
    assert_eq!(rig.authority.state().posts(), posts, "no lifecycle traffic");
    assert_eq!(plain(&rig, "res-a"), Ok(()));
    rig.advance(900_000);
    assert_eq!(plain(&rig, "res-a"), Err(Refusal::LeaseLapsed));
    // An operator restart resumes.
    rig.restart().unwrap();
    applied(rig.relist());
}

#[test]
fn h12_403_makes_nothing_servable_until_restart() {
    let (_dir, mut rig) = rig();
    rig.authority.state().add_active(PLAIN, "res-a");
    applied(rig.holder().list_once());
    rig.authority.state().disabled = true;
    assert_eq!(failed(rig.relist()), FetchFailure::NotAuthorized);
    assert_eq!(plain(&rig, "res-a"), Err(Refusal::AuthHalt));
    assert_eq!(rig.relist(), ListOutcome::AuthHalted);
    assert!(codes(&rig).contains(&"holder_not_authorized"));
}

#[test]
fn replay_and_proof_expired_are_retried_once_and_never_halt() {
    let (_dir, mut rig) = rig();
    rig.authority
        .state()
        .forced
        .push_back(problem(401, "replay"));
    applied(rig.holder().list_once());
    assert_eq!(rig.authority.state().posts(), 2);

    let (_dir, mut rig) = super::rig();
    for _ in 0..2 {
        rig.authority
            .state()
            .forced
            .push_back(problem(401, "replay"));
    }
    assert_eq!(
        failed(rig.holder().list_once()),
        FetchFailure::Unreachable {
            retry_after_s: None
        }
    );

    let (_dir, mut rig) = super::rig();
    rig.authority
        .state()
        .forced
        .push_back(problem(401, "proof_expired"));
    applied(rig.holder().list_once());

    // A holder clock 200 s behind: every proof is expired at the authority.
    let (_dir, mut rig) = super::rig();
    rig.authority.state().offset_ms = 200_000;
    assert_eq!(
        failed(rig.holder().list_once()),
        FetchFailure::ProofExpiredTwice
    );
    assert_eq!(rig.authority.state().posts(), 2, "one re-signed retry");
    let claims = rig.authority.state().requests.clone();
    assert!(claims.iter().all(|r| r.status == 401));
    assert!(codes(&rig).contains(&"lifecycle_proof_expired"));
    assert_eq!(rig.holder().route_health(), RouteHealth::Normal);
}

#[test]
fn keyset_proof_expiry_shares_the_list_retry_budget() {
    let (_dir, mut rig) = rig();
    {
        let mut authority = rig.authority.state();
        authority.rotate_key([37; 32], false, true);
        authority.keyset_response = Some(authority.problem_answer(401, "proof_expired", None));
        authority.before_post(2, |a| a.forced.push_back(problem(401, "proof_expired")));
    }
    assert_eq!(
        failed(rig.holder().list_once()),
        FetchFailure::ProofExpiredTwice
    );
    assert_eq!(rig.authority.state().keyset_gets, 1);
    assert_eq!(rig.authority.state().posts(), 2);
    assert!(codes(&rig).contains(&"lifecycle_proof_expired"));
    assert_eq!(rig.holder().route_health(), RouteHealth::Normal);
}

#[test]
fn h8_counted_list_starts_are_five_seconds_apart() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    let outcome = rig.holder().list_once();
    assert!(matches!(outcome, ListOutcome::RateLimited { .. }));
    rig.advance(4_999);
    assert!(matches!(
        rig.holder().list_once(),
        ListOutcome::RateLimited { .. }
    ));
    rig.advance(1);
    applied(rig.holder().list_once());
    assert_eq!(rig.authority.state().posts(), 2);
}

#[test]
fn the_whole_list_budget_discards_a_slow_list() {
    let (_dir, mut rig) = rig();
    rig.config.page_size = 1;
    rig.restart().unwrap();
    for n in 0..3 {
        rig.authority.state().add_active(PLAIN, &format!("res-{n}"));
    }
    rig.authority.state().latency_ms = 61_000;
    assert_eq!(failed(rig.holder().list_once()), FetchFailure::Deadline);
    assert!(rig.store.view().published.is_empty());
    assert_eq!(rig.holder().state().high_water, None);
}

#[test]
fn h6_an_attached_keyset_statement_is_pinned_durably_before_use() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    let new_kid = rig.authority.state().rotate_key([0x62; 32], true, true);
    applied(rig.relist());
    let pins = rig.holder().pinned_keys().clone();
    assert_eq!(pins.keyset_seq, Some(1));
    assert_eq!(pins.keys.keys().collect::<Vec<_>>(), [&new_kid]);
    assert!(codes(&rig).contains(&"lifecycle_keyset_accepted"));
    // Durable: a restarted holder verifies pages signed by the new key.
    rig.authority.state().attach_keyset = false;
    rig.authority.state().keyset_statement = None;
    rig.restart().unwrap();
    applied(rig.relist());
}

#[test]
fn h6_an_unknown_kid_fetches_the_keyset_route_once() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    rig.authority.state().rotate_key([0x63; 32], false, false);
    applied(rig.relist());
    assert_eq!(rig.holder().pinned_keys().keys.len(), 2);
}

#[test]
fn h6_keyset_statements_need_a_pinned_signer_and_an_increasing_sequence() {
    // Self-signed by the new key: refused.
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    {
        let mut authority = rig.authority.state();
        authority.rotate_key([0x64; 32], false, true);
        let statement = authority.keyset_statement.clone().unwrap();
        let claims = crate::jws::parse(&statement).unwrap().claims;
        let typ = authority.config.profile.typ_keyset.clone();
        authority.keyset_statement = Some(authority.sign_with_active(&typ, &Value::Object(claims)));
    }
    assert_eq!(failed(rig.relist()), FetchFailure::UnknownKid);

    // A replayed sequence number: refused.
    let (_dir, mut rig) = super::rig();
    applied(rig.holder().list_once());
    rig.authority.state().rotate_key([0x65; 32], true, false);
    applied(rig.relist());
    {
        let mut authority = rig.authority.state();
        authority.rotate_key([0x66; 32], true, false);
        let statement = authority.keyset_statement.clone().unwrap();
        let mut claims = crate::jws::parse(&statement).unwrap().claims;
        claims.insert("keyset_seq".into(), 1.into());
        let typ = authority.config.profile.typ_keyset.clone();
        let signer = statement_signer(&statement);
        authority.keyset_statement = Some(authority.sign_as(&signer, &typ, &Value::Object(claims)));
    }
    assert_eq!(failed(rig.relist()), FetchFailure::UnknownKid);
}

fn statement_signer(statement: &str) -> String {
    crate::jws::parse(statement).unwrap().kid
}

#[test]
fn h5_typ_alg_and_key_swaps_are_refused() {
    let foreign = p256::ecdsa::SigningKey::from_slice(&[0x71; 32]).unwrap();
    type HookCase = (Box<dyn Fn(&mut crate::testkit::Hooks)>, FetchFailure);
    let cases: Vec<HookCase> = vec![
        (
            Box::new(|h| h.typ = Some("example-obligation-ack+jwt".into())),
            FetchFailure::Invalid(PageError::Typ),
        ),
        (
            Box::new(|h| h.typ = Some("example-lifecycle-keyset+jwt".into())),
            FetchFailure::Invalid(PageError::Typ),
        ),
        (
            Box::new(|h| h.typ = Some("example-holdings-listreq+jwt".into())),
            FetchFailure::Invalid(PageError::Typ),
        ),
        (
            Box::new(|h| h.typ = Some("example-capability+jwt".into())),
            FetchFailure::Invalid(PageError::Typ),
        ),
        (
            Box::new(|h| h.typ = Some("secevent+jwt".into())),
            FetchFailure::Invalid(PageError::Typ),
        ),
        (
            Box::new(|h| h.alg = Some("EdDSA".into())),
            FetchFailure::Invalid(PageError::Alg),
        ),
        (
            Box::new(|h| h.alg = Some("none".into())),
            FetchFailure::Invalid(PageError::Alg),
        ),
        (
            Box::new(|h| h.alg = Some("HS256".into())),
            FetchFailure::Invalid(PageError::Alg),
        ),
        (
            Box::new(move |h| h.foreign_key = Some(foreign.clone())),
            FetchFailure::Invalid(PageError::Signature),
        ),
        (
            Box::new(|h| h.kid = Some("tenant-key-1".into())),
            FetchFailure::UnknownKid,
        ),
        (
            Box::new(|h| h.content_type = Some("application/json".into())),
            FetchFailure::Invalid(PageError::Grammar("content-type")),
        ),
    ];
    for (hook, expected) in cases {
        let (_dir, mut rig) = rig();
        rig.authority.state().add_active(PLAIN, "res-a");
        hook(&mut rig.authority.state().hooks);
        assert_eq!(failed(rig.holder().list_once()), expected);
        assert_eq!(plain(&rig, "res-a"), Err(Refusal::NoListThisProcess));
    }
}
