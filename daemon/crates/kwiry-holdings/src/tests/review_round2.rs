//! Regression tests for the second review round and contract amendments
//! H.35–H.40 (v2 sha256 `43a4d299…4291`). Each fails without its fix.

use super::*;
use crate::client::{FetchFailure, HttpResponse};
use crate::engine::RootObservation::*;
use crate::holder::{ConfigError, ListOutcome};
use crate::profile::PROBLEM_CONTENT_TYPE;
use crate::state::{ENROLLMENT_FILE, HolderState, STATE_FILE};
use crate::testkit::{Forced, RigError};
use crate::verify::PageError;
use serde_json::json;
use std::sync::{Arc, Mutex};

fn pin_ref_of(rig: &Rig, holding: &str) -> String {
    rig.authority.state().items[holding]["root_pin"]["pin_ref"]
        .as_str()
        .expect("pinned")
        .to_owned()
}

fn raise_hold(rig: &mut Rig) {
    rig.authority.state().offset_ms = 45_000;
    applied(rig.relist());
    assert!(rig.engine_gate().clock_hold());
    rig.authority.state().offset_ms = 0;
}

fn active_kid(rig: &Rig) -> String {
    rig.authority.state().active_kid.clone()
}

#[test]
fn b1_a_gate_held_across_a_holder_replacement_is_revoked() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-a", true, 1);
    let holding = rig.authority.state().add_active(PLAIN, "res-a");
    applied(rig.holder().list_once());
    let old_gate = rig.gate();
    let old_engine = rig.engine_gate();
    assert_eq!(old_gate.check("res-a"), Ok(()));

    rig.restart().unwrap();
    rig.authority.state().retire(&holding);
    applied(rig.relist());
    assert_eq!(rig.gate().check("res-a"), Err(Refusal::Tombstoned));
    assert_eq!(old_gate.check("res-a"), Err(Refusal::Revoked));
    let query = EngineQuery {
        resource_id: "res-a",
        capability: None,
        artifact: None,
        root: Unobserved,
    };
    assert_eq!(
        old_engine.check(&query, Cut::Release),
        Err(Refusal::Revoked)
    );
}

#[test]
fn b2_clock_evidence_is_applied_once_in_observation_order() {
    let (_dir, mut rig) = rig();
    rig.config.page_size = 1;
    rig.restart().unwrap();
    for id in ["res-a", "res-b"] {
        rig.store.hold(PLAIN, id, true, 1);
        rig.authority.state().add_active(PLAIN, id);
    }
    applied(rig.holder().list_once());
    raise_hold(&mut rig);
    rig.events.clear();
    // Page 0 would confirm a correction; page 1 proves a 40 s lag.
    rig.authority.state().hooks.page = Some(Box::new(|page| {
        if page["page"]["index"] == 1 {
            let iat = page["iat"].as_i64().unwrap();
            page.insert("iat".into(), (iat + 40).into());
        }
    }));
    let state_file = rig.dir.join(STATE_FILE);
    let on_disk_when_page_1_was_requested = Arc::new(Mutex::new(None));
    let seen = on_disk_when_page_1_was_requested.clone();
    let posts = rig.authority.state().posts();
    rig.authority.state().before_post(posts + 2, move |_| {
        let state = HolderState::parse(&std::fs::read(&state_file).unwrap()).unwrap();
        *seen.lock().unwrap() = Some(state.engine.clock_behind_authority);
    });
    applied(rig.relist());
    assert_eq!(
        *on_disk_when_page_1_was_requested.lock().unwrap(),
        Some(true),
        "earlier evidence never clears ahead of the whole list"
    );
    assert!(rig.engine_gate().clock_hold(), "the later raise stands");
    assert!(rig.holder().state().engine.clock_behind_authority);
    assert!(
        !codes(&rig).contains(&"clock_behind_authority_cleared"),
        "earlier evidence never undoes a later raise"
    );
}

#[test]
fn b4_an_attached_statement_on_a_page_signed_by_a_pinned_key_is_applied() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    let added = rig
        .authority
        .state()
        .issue_keyset(&[[0x81; 32]], &[], None, true);
    applied(rig.relist());
    let second = added[0].clone();
    assert!(rig.holder().pinned_keys().keys.contains_key(&second));

    rig.authority
        .state()
        .issue_keyset(&[], &[second.as_str()], None, true);
    applied(rig.relist());
    assert!(!rig.holder().pinned_keys().keys.contains_key(&second));
    rig.authority.state().use_key(&second);
    assert_eq!(failed(rig.relist()), FetchFailure::UnknownKid);
}

#[test]
fn b5_an_enrollment_kind_the_engine_profile_does_not_bind_refuses_to_start() {
    let (_dir, mut rig) = rig();
    let path = rig.dir.join(ENROLLMENT_FILE);
    let mut enrollment: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    enrollment["holder_kind"] = "device".into();
    std::fs::write(&path, serde_json::to_vec(&enrollment).unwrap()).unwrap();
    assert!(matches!(
        rig.restart(),
        Err(RigError::Config(ConfigError::HolderKind))
    ));
}

#[test]
fn b6_after_the_stale_window_a_matching_root_needs_a_successful_walk() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PINNED, "res-a", true, 1);
    let holding = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().attest(&holding, "dir-a0");
    applied(rig.holder().list_once());
    let pr1 = pin_ref_of(&rig, &holding);
    let detected = {
        use crate::clock::Clock;
        rig.clock.mono_ms()
    };
    let mismatch = Mismatch {
        detected_mono_ms: detected,
    };
    assert_eq!(pinned(&rig, "res-a", &pr1, mismatch), Ok(()));
    for _ in 0..4 {
        rig.advance(895_000);
        applied(rig.relist());
    }
    rig.advance(1);
    // The original root is back, but the window ran out: a walk must run.
    assert_eq!(
        pinned(&rig, "res-a", &pr1, Matches),
        Err(Refusal::Engine(EngineRefusal::RootPinMismatch))
    );
    rig.engine_gate()
        .record_successful_walk("res-a", "pr_0000000000000000000099");
    assert_eq!(
        pinned(&rig, "res-a", &pr1, Matches),
        Err(Refusal::Engine(EngineRefusal::RootPinMismatch)),
        "a walk under another pin_ref does not count"
    );
    rig.engine_gate().record_successful_walk("res-a", &pr1);
    assert_eq!(pinned(&rig, "res-a", &pr1, Matches), Ok(()));
}

#[test]
fn b6_a_walk_reported_mismatch_latches_without_any_request() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PINNED, "res-a", true, 1);
    let holding = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().attest(&holding, "dir-a0");
    applied(rig.holder().list_once());
    let pr1 = pin_ref_of(&rig, &holding);
    let now = {
        use crate::clock::Clock;
        rig.clock.mono_ms()
    };
    rig.engine_gate().report_root_mismatch("res-a", &pr1, now);
    for _ in 0..4 {
        rig.advance(895_000);
        applied(rig.relist());
    }
    rig.advance(1);
    assert_eq!(
        pinned(&rig, "res-a", &pr1, Matches),
        Err(Refusal::Engine(EngineRefusal::RootPinMismatch))
    );
}

#[test]
fn h36_a_signed_page_raises_the_hold_even_when_its_audience_fails() {
    let (_dir, mut rig) = rig();
    rig.authority.state().hooks.page = Some(Box::new(|page| {
        let iat = page["iat"].as_i64().unwrap();
        page.insert("iat".into(), (iat + 45).into());
        page.insert(
            "aud".into(),
            "example-lifecycle:hd_OTHERholder00000000001".into(),
        );
    }));
    assert_eq!(
        failed(rig.holder().list_once()),
        FetchFailure::Invalid(PageError::Audience)
    );
    assert!(rig.engine_gate().clock_hold());
}

#[test]
fn h37_not_after_old_retires_every_earlier_key_not_listed_at_that_instant() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    let first = active_kid(&rig);
    let limit_s = now_s(&rig) + 1_800;
    let added = rig
        .authority
        .state()
        .issue_keyset(&[[0x82; 32]], &[], Some(limit_s), true);
    applied(rig.relist());
    assert_eq!(
        rig.holder().pinned_keys().keys[&first].not_after_ms,
        Some(limit_s * 1000)
    );
    // Pages are still signed by the earlier key; at the instant it retires.
    let wall = {
        use crate::clock::Clock;
        rig.clock.wall_ms()
    };
    rig.advance(u64::try_from(limit_s * 1000 - wall).unwrap() - 5_000);
    assert_eq!(failed(rig.relist()), FetchFailure::UnknownKid);
    rig.authority.state().use_key(&added[0]);
    applied(rig.relist());
}

#[test]
fn h37_retire_kids_is_immediate_even_with_not_after_old() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    let first = active_kid(&rig);
    let limit_s = now_s(&rig) + 1_800;
    let added =
        rig.authority
            .state()
            .issue_keyset(&[[0x83; 32]], &[first.as_str()], Some(limit_s), true);
    rig.authority.state().use_key(&added[0]);
    applied(rig.relist());
    assert!(!rig.holder().pinned_keys().keys.contains_key(&first));
}

#[test]
fn h37_a_statement_that_would_leave_no_pinned_key_is_refused_whole() {
    for not_after_old in [false, true] {
        let (_dir, mut rig) = rig();
        applied(rig.holder().list_once());
        let first = active_kid(&rig);
        let before = rig.holder().pinned_keys().clone();
        let (retire, limit) = if not_after_old {
            (Vec::new(), Some(now_s(&rig) + 1_800))
        } else {
            (vec![first.as_str()], None)
        };
        rig.authority
            .state()
            .issue_keyset(&[], &retire, limit, true);
        assert_eq!(failed(rig.relist()), FetchFailure::KeysetInvalid);
        assert!(codes(&rig).contains(&"keyset_invalid"));
        assert_eq!(rig.holder().pinned_keys(), &before);
    }
}

#[test]
fn h38_a_pin_under_two_resources_refuses_only_the_active_items_carrying_it() {
    let (_dir, mut rig) = rig();
    let mut holdings = std::collections::HashMap::new();
    for (id, dir) in [
        ("res-a", "dir-a"),
        ("res-b", "dir-b"),
        ("res-c", "dir-c"),
        ("res-r", "dir-r"),
        ("res-e", "dir-e"),
    ] {
        rig.store.hold(PINNED, id, true, 1);
        let holding = rig.authority.state().add_active(PINNED, id);
        rig.authority.state().attest(&holding, dir);
        holdings.insert(id, holding);
    }
    rig.authority.state().retire(&holdings["res-r"]);
    applied(rig.holder().list_once());
    let pr_c = pin_ref_of(&rig, &holdings["res-c"]);
    assert_eq!(pinned(&rig, "res-c", &pr_c, Matches), Ok(()));

    // res-b takes res-a's directory; res-e takes the retired res-r's.
    let pr_b = rig.authority.state().attest(&holdings["res-b"], "dir-a");
    let pr_e = rig.authority.state().attest(&holdings["res-e"], "dir-r");
    let pr_a = pin_ref_of(&rig, &holdings["res-a"]);
    let previously_listed = Err(Refusal::Engine(EngineRefusal::RootPinPreviouslyListed));
    // At admit (the apply fails here), with the precise reason per item.
    rig.io_faults.fail_writes(1);
    assert_eq!(rig.relist(), ListOutcome::ApplyFailed { step: "A1" });
    assert_eq!(pinned(&rig, "res-a", &pr_a, Matches), previously_listed);
    assert_eq!(pinned(&rig, "res-b", &pr_b, Matches), previously_listed);
    assert_eq!(pinned(&rig, "res-e", &pr_e, Matches), previously_listed);
    assert_eq!(pinned(&rig, "res-c", &pr_c, Matches), Ok(()));

    // The rest of the list applies; its pairs join the history.
    let report = applied(rig.relist());
    assert_eq!(rig.holder().state().high_water.as_ref(), Some(&report.rev));
    let stale = Mismatch {
        detected_mono_ms: 0,
    };
    for (id, pr) in [("res-a", &pr_a), ("res-b", &pr_b), ("res-e", &pr_e)] {
        assert_eq!(pinned(&rig, id, pr, Matches), previously_listed, "{id}");
        assert_eq!(pinned(&rig, id, pr, stale), previously_listed, "{id}");
    }
    assert_eq!(pinned(&rig, "res-c", &pr_c, Matches), Ok(()));
    let history = &rig.holder().state().engine.pin_history;
    for (dir, id) in [("dir-a", "res-b"), ("dir-r", "res-e")] {
        assert!(
            history
                .iter()
                .any(|p| p.pin_id == crate::testkit::pin_id(dir) && p.resource_id == id)
        );
    }
}

#[test]
fn h40_a_list_request_error_backs_off_to_relist_seconds_until_a_list_applies() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    let bodies_before = rig.authority.state().bodies.len();
    let mut waits = Vec::new();
    for status in [400, 413, 400, 400, 413, 400] {
        rig.authority
            .state()
            .forced
            .push_back(Forced::Response(HttpResponse {
                status,
                content_type: Some(PROBLEM_CONTENT_TYPE.into()),
                retry_after_s: None,
                body: json!({ "status": status, "code": "invalid_request" })
                    .to_string()
                    .into_bytes(),
            }));
        assert_eq!(
            failed(rig.relist()),
            FetchFailure::RequestInvalid { status }
        );
        let now = {
            use crate::clock::Clock;
            rig.clock.mono_ms()
        };
        waits.push(rig.holder().scheduler().backoff_until() - now);
        assert!(
            rig.holder()
                .raised_alerts()
                .contains(&"lifecycle_list_request_invalid")
        );
    }
    assert_eq!(waits, [10_000, 20_000, 40_000, 80_000, 160_000, 300_000]);
    applied(rig.relist());
    assert!(
        !rig.holder()
            .raised_alerts()
            .contains(&"lifecycle_list_request_invalid")
    );
    assert!(codes(&rig).contains(&"lifecycle_list_request_invalid_cleared"));
    // Every attempt was rebuilt: fresh bytes, nonce and jti each time.
    let bodies = rig.authority.state().bodies[bodies_before..].to_vec();
    let claims: Vec<_> = bodies
        .iter()
        .map(|b| {
            crate::jws::parse(std::str::from_utf8(b).unwrap())
                .unwrap()
                .claims
        })
        .collect();
    let unique = |field: &str| {
        claims
            .iter()
            .map(|c| c[field].to_string())
            .collect::<std::collections::HashSet<_>>()
            .len()
    };
    assert_eq!(bodies.len(), 7);
    assert_eq!(unique("nonce"), 7);
    assert_eq!(unique("jti"), 7);
}
