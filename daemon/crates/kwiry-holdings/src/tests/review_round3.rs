//! Regression tests for the third review round and amendments H.41–H.44
//! (v2 sha256 `066a21ae…d042`). Each fails without its fix.

use super::*;
use crate::client::{FetchFailure, HttpResponse};
use crate::engine::RootObservation::*;
use crate::engine::clock_hold::SEAL_FILE;
use crate::holder::ListOutcome;
use crate::profile::PROBLEM_CONTENT_TYPE;
use crate::state::STATE_FILE;
use crate::testkit::{Fault, Forced, TestCapabilityVerifier};
use crate::verify::PageError;
use serde_json::{Value, json};

fn hold_one(rig: &mut Rig) {
    rig.store.hold(PLAIN, "res-a", true, 1);
    rig.authority.state().add_active(PLAIN, "res-a");
    applied(rig.holder().list_once());
}

fn raise_hold(rig: &mut Rig) {
    rig.authority.state().offset_ms = 45_000;
    applied(rig.relist());
    assert!(rig.engine_gate().clock_hold());
    rig.authority.state().offset_ms = 0;
}

fn pin_ref_of(rig: &Rig, holding: &str) -> String {
    rig.authority.state().items[holding]["root_pin"]["pin_ref"]
        .as_str()
        .expect("pinned")
        .to_owned()
}

fn two_page_rig() -> (tempfile::TempDir, Rig) {
    let (dir, mut rig) = rig();
    rig.config.page_size = 1;
    rig.restart().unwrap();
    for id in ["res-a", "res-b"] {
        rig.store.hold(PLAIN, id, true, 1);
        rig.authority.state().add_active(PLAIN, id);
    }
    (dir, rig)
}

#[test]
fn b3_every_fault_in_a_clear_leaves_a_restart_agreeing_with_memory() {
    // Every Pass/BeforeRename/AfterRename outcome for the clear's writes:
    // the record, any rollback, the seal, and whatever invalidates a seal.
    let outcomes = [Fault::Pass, Fault::BeforeRename, Fault::AfterRename];
    let mut plans = vec![Vec::new()];
    for _ in 0..4 {
        plans = plans
            .into_iter()
            .flat_map(|plan: Vec<Fault>| {
                outcomes.iter().map(move |fault| {
                    let mut next = plan.clone();
                    next.push(*fault);
                    next
                })
            })
            .collect();
    }
    for plan in plans {
        let (_dir, mut rig) = rig();
        hold_one(&mut rig);
        raise_hold(&mut rig);
        // A probe applies nothing: the clear sequence is all it writes.
        rig.io_faults.plan(plan.clone());
        rig.advance(5_000);
        let probe = rig.holder().probe_clock();
        // A write that cannot be put back leaves durable state ambiguous: the
        // holder fail-stops (v2 H.45) instead of carrying on.
        if rig.holder().fail_stopped() {
            assert_eq!(
                probe,
                Some(Err(crate::client::FetchFailure::HolderStateUnwritable)),
                "{plan:?}"
            );
        } else {
            assert_eq!(probe, Some(Ok(())), "{plan:?}");
        }
        let in_memory = rig.engine_gate().clock_hold();
        // Unless a rollback itself failed, a failed write was put back: the
        // state file equals the committed state.
        if !rig.holder().writer_poisoned() {
            let on_disk =
                crate::state::HolderState::parse(&std::fs::read(rig.dir.join(STATE_FILE)).unwrap())
                    .unwrap();
            assert_eq!(&on_disk, rig.holder().state(), "{plan:?}");
        }
        rig.io_faults.clear();
        rig.restart().unwrap();
        assert_eq!(rig.engine_gate().clock_hold(), in_memory, "{plan:?}");
        if !in_memory {
            // The clear counted only with a durable record and a seal whose
            // directory fsync succeeded.
            assert_eq!(plan[..2], [Fault::Pass, Fault::Pass], "{plan:?}");
        }
    }
}

#[test]
fn b3_a_cleared_record_without_its_seal_loads_as_raised() {
    let (_dir, mut rig) = rig();
    hold_one(&mut rig);
    let path = rig.dir.join(STATE_FILE);
    let mut state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    state["engine"]["clock_behind_authority"] = false.into();
    state["engine"]["clock_clear_token"] = "token-one".into();
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    for (seal, raised) in [
        (None, true),
        (Some("token-two"), true),
        (Some("token-one"), false),
    ] {
        match seal {
            None => {
                let _ = std::fs::remove_file(rig.dir.join(SEAL_FILE));
            }
            Some(token) => std::fs::write(rig.dir.join(SEAL_FILE), token).unwrap(),
        }
        rig.restart().unwrap();
        assert_eq!(rig.engine_gate().clock_hold(), raised, "{seal:?}");
    }
}

#[test]
fn b6_a_delayed_report_for_an_earlier_pin_cannot_end_the_current_latch() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PINNED, "res-a", true, 1);
    let holding = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().attest(&holding, "dir-a0");
    applied(rig.holder().list_once());
    let pr1 = pin_ref_of(&rig, &holding);
    let pr2 = rig.authority.state().attest(&holding, "dir-a1");
    applied(rig.relist());
    let now = || {
        use crate::clock::Clock;
        rig.clock.mono_ms()
    };
    let detected = now();
    let mismatch = Mismatch {
        detected_mono_ms: detected,
    };
    assert_eq!(pinned(&rig, "res-a", &pr2, mismatch), Ok(()));
    for _ in 0..4 {
        rig.advance(895_000);
        applied(rig.relist());
    }
    rig.advance(1);
    let refused = Err(Refusal::Engine(EngineRefusal::RootPinMismatch));
    assert_eq!(pinned(&rig, "res-a", &pr2, Matches), refused);
    // A delayed report about the earlier pin, and a walk under it.
    let late = {
        use crate::clock::Clock;
        rig.clock.mono_ms()
    };
    rig.engine_gate().report_root_mismatch("res-a", &pr1, late);
    assert_eq!(pinned(&rig, "res-a", &pr2, Matches), refused);
    rig.engine_gate().record_successful_walk("res-a", &pr1);
    assert_eq!(pinned(&rig, "res-a", &pr2, Matches), refused);
    rig.engine_gate().record_successful_walk("res-a", &pr2);
    assert_eq!(pinned(&rig, "res-a", &pr2, Matches), Ok(()));
}

#[test]
fn h35_a_malformed_stale_keyset_statement_rejects_the_page() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    rig.authority
        .state()
        .issue_keyset(&[[0x84; 32]], &[], None, true);
    applied(rig.relist());
    // The same (now stale) sequence, with an extra member.
    {
        let mut authority = rig.authority.state();
        let statement = authority.keyset_statement.clone().unwrap();
        let mut claims = crate::jws::parse(&statement).unwrap().claims;
        claims.insert("note".into(), "x".into());
        let typ = authority.config.profile.typ_keyset.clone();
        authority.keyset_statement = Some(authority.sign_with_active(&typ, &Value::Object(claims)));
    }
    assert_eq!(
        failed(rig.relist()),
        FetchFailure::Invalid(PageError::Keyset)
    );
}

#[test]
fn h36_signature_typ_kid_and_iss_gate_a_raise_and_aud_sub_do_not() {
    let foreign = p256::ecdsa::SigningKey::from_slice(&[0x77; 32]).unwrap();
    type Case = (&'static str, Box<dyn Fn(&mut crate::testkit::Hooks)>, bool);
    let ahead = |page: &mut serde_json::Map<String, Value>| {
        let iat = page["iat"].as_i64().unwrap();
        page.insert("iat".into(), (iat + 45).into());
    };
    let cases: Vec<Case> = vec![
        (
            "typ",
            Box::new(|h| h.typ = Some("example-other+jwt".into())),
            false,
        ),
        (
            "kid",
            Box::new(|h| h.kid = Some("ak-not-pinned".into())),
            false,
        ),
        (
            "signature",
            Box::new(move |h| h.foreign_key = Some(foreign.clone())),
            false,
        ),
        (
            "iss",
            Box::new(move |h| {
                h.page = Some(Box::new(move |p| {
                    ahead(p);
                    p.insert("iss".into(), "https://elsewhere.example.test/v1".into());
                }))
            }),
            false,
        ),
        (
            "aud",
            Box::new(move |h| {
                h.page = Some(Box::new(move |p| {
                    ahead(p);
                    p.insert(
                        "aud".into(),
                        "example-lifecycle:hd_OTHERholder00000000001".into(),
                    );
                }))
            }),
            true,
        ),
        (
            "sub",
            Box::new(move |h| {
                h.page = Some(Box::new(move |p| {
                    ahead(p);
                    p.insert("sub".into(), "hd_OTHERholder00000000001".into());
                }))
            }),
            true,
        ),
    ];
    for (name, hook, raises) in cases {
        let (_dir, mut rig) = rig();
        hook(&mut rig.authority.state().hooks);
        if rig.authority.state().hooks.page.is_none() {
            rig.authority.state().hooks.page = Some(Box::new(ahead));
        }
        failed(rig.holder().list_once());
        assert_eq!(rig.engine_gate().clock_hold(), raises, "{name}");
    }
}

#[test]
fn h36_only_a_verified_capability_can_raise_the_hold() {
    let (_dir, mut rig) = rig();
    hold_one(&mut rig);
    let ahead = now_s(&rig) + 3_600;
    let forged = TestCapabilityVerifier::forge(ahead, ahead + 60);
    assert_eq!(
        rig.holder()
            .observe_capability(&TestCapabilityVerifier, &forged),
        Err("signature")
    );
    assert!(!rig.engine_gate().clock_hold());
    let genuine = TestCapabilityVerifier::issue(ahead, ahead + 60);
    rig.holder()
        .observe_capability(&TestCapabilityVerifier, &genuine)
        .unwrap();
    assert!(rig.engine_gate().clock_hold());
}

#[test]
fn h37_buffered_pages_are_reverified_when_a_later_statement_retires_their_signer() {
    let (_dir, mut rig) = two_page_rig();
    applied(rig.holder().list_once());
    let second = rig
        .authority
        .state()
        .issue_keyset(&[[0x85; 32]], &[], None, true)
        .remove(0);
    applied(rig.relist());
    {
        let mut authority = rig.authority.state();
        authority.use_key(&second);
        authority.attach_keyset = false;
    }
    applied(rig.relist());
    // Page 0 is signed by `second`; before page 1, a statement retires it
    // and page 1 is signed by a new key only the key-set route introduces.
    let posts = rig.authority.state().posts();
    let retiring = second.clone();
    rig.authority.state().before_post(posts + 2, move |state| {
        let third = state.issue_keyset(&[[0x86; 32]], &[retiring.as_str()], None, false);
        state.use_key(&third[0]);
    });
    assert_eq!(failed(rig.relist()), FetchFailure::UnknownKid);
    assert!(!rig.holder().pinned_keys().keys.contains_key(&second));
}

#[test]
fn h40_every_list_start_waits_out_the_backoff() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    rig.authority
        .state()
        .forced
        .push_back(Forced::Response(HttpResponse {
            status: 400,
            content_type: Some(PROBLEM_CONTENT_TYPE.into()),
            retry_after_s: None,
            headers: Vec::new(),
            body: json!({"status": 400, "code": "invalid_request"})
                .to_string()
                .into_bytes(),
        }));
    assert_eq!(
        failed(rig.relist()),
        FetchFailure::RequestInvalid { status: 400 }
    );
    let posts = rig.authority.state().posts();
    rig.advance(5_000);
    assert!(matches!(
        rig.holder().list_once(),
        ListOutcome::RateLimited { .. }
    ));
    assert_eq!(
        rig.authority.state().posts(),
        posts,
        "no request inside the backoff"
    );
}

#[test]
fn h40_the_request_invalid_alert_survives_a_restart_until_a_list_applies() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    rig.authority
        .state()
        .forced
        .push_back(Forced::Response(HttpResponse {
            status: 413,
            content_type: Some(PROBLEM_CONTENT_TYPE.into()),
            retry_after_s: None,
            headers: Vec::new(),
            body: json!({"status": 413, "code": "payload_too_large"})
                .to_string()
                .into_bytes(),
        }));
    failed(rig.relist());
    rig.restart().unwrap();
    assert!(
        rig.holder()
            .raised_alerts()
            .contains(&"lifecycle_list_request_invalid")
    );
    applied(rig.relist());
    rig.restart().unwrap();
    assert!(
        !rig.holder()
            .raised_alerts()
            .contains(&"lifecycle_list_request_invalid")
    );
}

#[test]
fn h29_a_clear_waits_for_the_whole_list_to_verify() {
    // A first page claiming index 1 passes page checks but fails the list.
    let (_dir, mut rig) = two_page_rig();
    applied(rig.holder().list_once());
    raise_hold(&mut rig);
    rig.authority.state().hooks.page = Some(Box::new(|page| {
        if page["page"]["index"] == 0 {
            page["page"]["index"] = 1.into();
            page.insert("next_cursor".into(), Value::Null);
        }
    }));
    assert_eq!(
        failed(rig.relist()),
        FetchFailure::Invalid(PageError::Consistency("first page index"))
    );
    assert!(rig.engine_gate().clock_hold());

    // A good page 0 followed by a bad page 1: still no clear.
    let (_dir, mut rig) = two_page_rig();
    applied(rig.holder().list_once());
    raise_hold(&mut rig);
    rig.authority.state().hooks.page = Some(Box::new(|page| {
        if page["page"]["index"] == 1 {
            page["page"]["items_total"] = 3.into();
        }
    }));
    failed(rig.relist());
    assert!(rig.engine_gate().clock_hold());
    rig.authority.state().hooks = Default::default();
    applied(rig.relist());
    assert!(
        !rig.engine_gate().clock_hold(),
        "a whole verified list clears"
    );
}

#[test]
fn h41_a_cursor_sealed_in_another_epoch_gets_410() {
    let (_dir, mut rig) = two_page_rig();
    let report = applied(rig.holder().list_once());
    let posts = rig.authority.state().posts();
    rig.authority
        .state()
        .before_post(posts + 2, |state| state.switch_epoch_quietly());
    assert_eq!(failed(rig.relist()), FetchFailure::UnexpectedEpoch);
    let statuses: Vec<u16> = rig
        .authority
        .state()
        .requests
        .iter()
        .skip(posts)
        .map(|r| r.status)
        .collect();
    assert_eq!(statuses, [200, 410, 200]);
    assert_eq!(rig.holder().state().high_water.as_ref(), Some(&report.rev));
}
