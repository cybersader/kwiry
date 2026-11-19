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
        body: json!({ "status": status, "code": code })
            .to_string()
            .into_bytes(),
    })
}

#[test]
fn first_contact_applies_then_every_request_echoes_the_witness() {
    let (_dir, mut rig) = rig();
    rig.authority.state().add_active(PLAIN, "res-a");
    let report = applied(rig.holder().list_once());
    assert_eq!(rig.holder().state().high_water.as_ref(), Some(&report.rev));
    let applied_ref = rig.holder().state().last_list_ref.clone().expect("ref");
    applied(rig.relist());

    let claims = rig.authority.state().list_claims();
    assert_eq!(claims.len(), 2);
    assert!(
        !claims[0].contains_key("last_list_ref"),
        "omitted on first contact"
    );
    assert_eq!(claims[0]["high_water"], Value::Null);
    assert_eq!(claims[1]["last_list_ref"], applied_ref.to_value());
    assert_eq!(claims[1]["high_water"], report.rev.to_value());
    for request in &claims {
        let (iat, exp) = (request["iat"].as_i64(), request["exp"].as_i64());
        assert_eq!(exp.unwrap() - iat.unwrap(), 300, "exp = iat + 300");
        assert_eq!(request["page_size"], 500);
    }
    assert_ne!(
        claims[0]["nonce"], claims[1]["nonce"],
        "never reused across lists"
    );
}

#[test]
fn pages_share_one_nonce_and_assemble_atomically() {
    let (_dir, mut rig) = rig();
    rig.config.page_size = 2;
    rig.restart().unwrap();
    for n in 0..5 {
        rig.authority.state().add_active(PLAIN, &format!("res-{n}"));
    }
    for n in 0..5 {
        rig.store.hold(PLAIN, &format!("res-{n}"), true, 1);
    }
    let report = applied(rig.holder().list_once());
    assert_eq!(report.plan.dispositions.len(), 5);
    let claims = rig.authority.state().list_claims();
    assert_eq!(claims.len(), 3);
    assert!(claims.iter().all(|c| c["nonce"] == claims[0]["nonce"]));
    assert_eq!(claims[0]["cursor"], Value::Null);
    assert!(claims[1]["cursor"].is_string() && claims[2]["cursor"].is_string());
    assert_eq!(rig.store.view().published.len(), 1);
    for n in 0..5 {
        assert_eq!(plain(&rig, &format!("res-{n}")), Ok(()));
    }
}

#[test]
fn h8_and_oa10_410_mid_list_discards_pages_and_relists_with_a_fresh_nonce() {
    let (_dir, mut rig) = rig();
    rig.config.page_size = 1;
    rig.restart().unwrap();
    let holding = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().add_active(PLAIN, "res-b");
    // An attestation commits while the cursor is mid-pagination.
    rig.authority.state().before_post(2, move |s| {
        s.attest(&holding, "dir-a");
    });
    let report = applied(rig.holder().list_once());
    let statuses: Vec<u16> = rig
        .authority
        .state()
        .requests
        .iter()
        .map(|r| r.status)
        .collect();
    assert_eq!(statuses, [200, 410, 200, 200]);
    let claims = rig.authority.state().list_claims();
    assert_eq!(claims[1]["nonce"], claims[0]["nonce"]);
    assert_ne!(
        claims[2]["nonce"], claims[0]["nonce"],
        "fresh nonce after 410"
    );
    assert_eq!(claims[2]["cursor"], Value::Null, "restart from page 1");
    assert_eq!(rig.store.view().published.len(), 1, "no partial apply");
    // The fresh list carries the attested pin.
    let state = rig.holder().state();
    assert_eq!(state.engine.pin_history.len(), 1);
    assert_eq!(state.high_water.as_ref(), Some(&report.rev));
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
fn h3_same_epoch_regression_is_refused_and_leases_run() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-a", true, 3);
    rig.authority.state().add_active(PLAIN, "res-a");
    let report = applied(rig.holder().list_once());
    // Make the witness unverifiable (a digest-root rotation), then roll back.
    {
        let mut authority = rig.authority.state();
        authority.digest_root = [0x77; 32];
        authority.raw_restore(report.rev.seq - 1);
    }
    assert_eq!(failed(rig.relist()), FetchFailure::Regressed);
    assert!(codes(&rig).contains(&"lifecycle_regressed"));
    assert_eq!(rig.holder().state().high_water.as_ref(), Some(&report.rev));
    assert_eq!(
        plain(&rig, "res-a"),
        Ok(()),
        "serving unchanged until the lease"
    );
    rig.advance(900_000);
    assert_eq!(plain(&rig, "res-a"), Err(Refusal::LeaseLapsed));
    assert!(rig.store.bytes_present(PLAIN, "res-a"));
}

#[test]
fn v10_and_oa6_tag_valid_witness_ahead_of_the_store_is_rollback_suspected() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-a", true, 1);
    rig.authority.state().add_active(PLAIN, "res-a");
    let report = applied(rig.holder().list_once());
    rig.authority.state().raw_restore(report.rev.seq - 5);
    assert_eq!(failed(rig.relist()), FetchFailure::RollbackSuspected);
    assert!(rig.authority.state().rollback_suspected);
    assert_eq!(rig.authority.state().audit, ["restore_detected"]);
    assert!(codes(&rig).contains(&"authority_rollback_suspected"));
    assert_eq!(rig.holder().route_health(), RouteHealth::Normal);
    // Every later list answers 503; leases run down; nothing is deleted.
    assert_eq!(failed(rig.relist()), FetchFailure::RollbackSuspected);
    assert_eq!(plain(&rig, "res-a"), Ok(()));
    rig.advance(900_000);
    assert_eq!(plain(&rig, "res-a"), Err(Refusal::LeaseLapsed));
    assert!(rig.store.bytes_present(PLAIN, "res-a"));
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

#[test]
fn h4_wrong_nonce_echo_and_a_replayed_older_list_are_refused() {
    let (_dir, mut rig) = rig();
    rig.authority.state().add_active(PLAIN, "res-a");
    applied(rig.holder().list_once());
    let old = rig.authority.state().last_page.clone().expect("page");
    rig.authority
        .state()
        .forced
        .push_back(Forced::Response(old));
    assert_eq!(
        failed(rig.relist()),
        FetchFailure::Invalid(PageError::Nonce)
    );
    rig.authority.state().hooks.page = Some(Box::new(|page| {
        page.insert("nonce".into(), "n".repeat(43).into());
    }));
    assert_eq!(
        failed(rig.relist()),
        FetchFailure::Invalid(PageError::Nonce)
    );
    assert!(codes(&rig).contains(&"lifecycle_list_invalid"));
}

#[test]
fn h6_audience_subject_and_issuer_bind_the_page_to_this_holder() {
    for (member, value, expected) in [
        (
            "aud",
            "example-lifecycle:hd_OTHERholder00000000001",
            PageError::Audience,
        ),
        ("aud", "example-lifecycle:*", PageError::Audience),
        ("sub", "hd_OTHERholder00000000001", PageError::Subject),
        (
            "iss",
            "https://elsewhere.example.test/api/lifecycle/v1",
            PageError::Issuer,
        ),
    ] {
        let (_dir, mut rig) = rig();
        rig.authority.state().add_active(PLAIN, "res-a");
        rig.authority.state().hooks.page = Some(Box::new(move |page| {
            page.insert(member.into(), value.into());
        }));
        assert_eq!(
            failed(rig.holder().list_once()),
            FetchFailure::Invalid(expected),
            "{member}"
        );
        assert_eq!(plain(&rig, "res-a"), Err(Refusal::NoListThisProcess));
    }
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
            FetchFailure::Unreachable {
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
fn v14_a_new_key_without_a_valid_keyset_statement_is_unknown_kid() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-a", true, 1);
    rig.authority.state().add_active(PLAIN, "res-a");
    applied(rig.holder().list_once());
    let pins = rig.holder().pinned_keys().clone();
    {
        let mut authority = rig.authority.state();
        authority.rotate_key([0x61; 32], false, false);
        authority.keyset_statement = None;
    }
    assert_eq!(failed(rig.relist()), FetchFailure::UnknownKid);
    assert!(codes(&rig).contains(&"unknown_kid"));
    assert_eq!(rig.holder().pinned_keys(), &pins);
    assert_eq!(plain(&rig, "res-a"), Ok(()), "leases keep running");
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
