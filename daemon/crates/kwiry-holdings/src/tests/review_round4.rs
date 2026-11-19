//! Regression tests for the fourth review round (v2 sha256 `066a21ae…d042`).
//! Each fails without its fix.

use super::*;
use crate::client::{FetchFailure, HttpResponse};
use crate::engine::clock_hold::SEAL_FILE;
use crate::holder::ListOutcome;
use crate::profile::PROBLEM_CONTENT_TYPE;
use crate::state::{DIRECTORY_SYNC, DirError, HolderState, STATE_FILE, require_directory_sync};
use crate::testkit::{Fault, Forced};
use crate::verify::PageError;
use serde_json::json;

fn rig_with(items: &[&str]) -> (tempfile::TempDir, Rig) {
    let (dir, mut rig) = rig();
    rig.config.page_size = 1;
    rig.restart().unwrap();
    for id in items {
        rig.store.hold(PLAIN, id, true, 1);
        rig.authority.state().add_active(PLAIN, id);
    }
    applied(rig.holder().list_once());
    (dir, rig)
}

fn raise_hold(rig: &mut Rig) {
    rig.authority.state().offset_ms = 45_000;
    applied(rig.relist());
    assert!(rig.engine_gate().clock_hold());
    rig.authority.state().offset_ms = 0;
}

fn request_error(status: u16) -> Forced {
    Forced::Response(HttpResponse {
        status,
        content_type: Some(PROBLEM_CONTENT_TYPE.into()),
        retry_after_s: None,
        body: json!({"status": status, "code": "invalid_request"})
            .to_string()
            .into_bytes(),
    })
}

fn disk_state(rig: &Rig) -> HolderState {
    HolderState::parse(&std::fs::read(rig.dir.join(STATE_FILE)).unwrap()).unwrap()
}

#[test]
fn b3_a_seal_whose_directory_fsync_fails_does_not_clear() {
    // The seal renames, then its directory fsync fails: a fresh raised
    // record invalidates it.
    let (_dir, mut rig) = rig_with(&["res-a"]);
    raise_hold(&mut rig);
    rig.io_faults.plan(vec![Fault::Pass, Fault::AfterRename]);
    rig.advance(5_000);
    assert_eq!(rig.holder().probe_clock(), Some(Ok(())));
    assert!(rig.engine_gate().clock_hold(), "the clear did not count");
    let on_disk = disk_state(&rig);
    assert!(on_disk.engine.clock_behind_authority);
    assert_eq!(&on_disk, rig.holder().state());
    rig.restart().unwrap();
    assert!(rig.engine_gate().clock_hold());

    // The raised record cannot be written either: the seal is removed.
    let (_dir, mut rig) = rig_with(&["res-a"]);
    raise_hold(&mut rig);
    rig.io_faults
        .plan(vec![Fault::Pass, Fault::AfterRename, Fault::BeforeRename]);
    rig.advance(5_000);
    assert_eq!(rig.holder().probe_clock(), Some(Ok(())));
    assert!(rig.engine_gate().clock_hold());
    assert!(!rig.dir.join(SEAL_FILE).exists());
    rig.io_faults.clear();
    rig.restart().unwrap();
    assert!(rig.engine_gate().clock_hold());
}

#[test]
fn h35_a_stale_statement_with_a_malformed_signature_rejects_the_page() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    rig.authority
        .state()
        .issue_keyset(&[[0x87; 32]], &[], None, true);
    applied(rig.relist());
    {
        let mut authority = rig.authority.state();
        let statement = authority.keyset_statement.clone().unwrap();
        let (signed, _) = statement.rsplit_once('.').unwrap();
        authority.keyset_statement = Some(format!("{signed}.AA"));
    }
    assert_eq!(
        failed(rig.relist()),
        FetchFailure::Invalid(PageError::Keyset)
    );
    assert_eq!(
        crate::jws::parse(&format!(
            "{}.AA",
            rig.authority
                .state()
                .keyset_statement
                .clone()
                .unwrap()
                .rsplit_once('.')
                .unwrap()
                .0
        ))
        .err(),
        Some(crate::jws::JwsError::SignatureEncoding)
    );
}

#[test]
fn h40_a_probe_waits_out_an_active_backoff() {
    let (_dir, mut rig) = rig_with(&["res-a"]);
    raise_hold(&mut rig);
    rig.authority.state().forced.push_back(request_error(400));
    assert_eq!(
        failed(rig.relist()),
        FetchFailure::RequestInvalid { status: 400 }
    );
    rig.advance(5_000);
    assert!(
        rig.holder().probe_clock().is_none(),
        "inside the 10 s backoff"
    );
    rig.advance(5_000);
    assert_eq!(rig.holder().probe_clock(), Some(Ok(())));
}

#[test]
fn h40_a_probe_400_starts_the_backoff() {
    let (_dir, mut rig) = rig_with(&["res-a"]);
    raise_hold(&mut rig);
    rig.authority.state().forced.push_back(request_error(413));
    rig.advance(5_000);
    assert_eq!(
        rig.holder().probe_clock(),
        Some(Err(FetchFailure::RequestInvalid { status: 413 }))
    );
    assert!(rig.holder().state().list_request_invalid);
    let posts = rig.authority.state().posts();
    rig.advance(5_000);
    assert!(matches!(
        rig.holder().list_once(),
        ListOutcome::RateLimited { .. }
    ));
    assert_eq!(rig.authority.state().posts(), posts);
}

#[test]
fn h29_a_confirming_page_0_clears_once_the_whole_list_verifies() {
    let (_dir, mut rig) = rig_with(&["res-a", "res-b"]);
    raise_hold(&mut rig);
    // Page 0 confirms (lag 20 s, no transit); page 1 is slow, so its own
    // bound (5 s + 30 s) does not confirm, but it proves no lag either.
    rig.authority.state().offset_ms = 20_000;
    let posts = rig.authority.state().posts();
    rig.authority
        .state()
        .before_post(posts + 2, |state| state.latency_ms = 30_000);
    applied(rig.relist());
    assert!(!rig.engine_gate().clock_hold());
    assert!(codes(&rig).contains(&"clock_behind_authority_cleared"));
}

#[test]
fn h29_a_probe_clears_only_as_a_complete_verified_list() {
    // The probe sees page 0 of two: nothing it may use.
    let (_dir, mut rig) = rig_with(&["res-a", "res-b"]);
    raise_hold(&mut rig);
    rig.advance(5_000);
    assert_eq!(rig.holder().probe_clock(), Some(Ok(())));
    assert!(rig.engine_gate().clock_hold());

    // A terminal page whose `items_total` disagrees with its items.
    let (_dir, mut rig) = rig_with(&["res-a"]);
    raise_hold(&mut rig);
    rig.authority.state().hooks.page = Some(Box::new(|page| {
        page["page"]["items_total"] = 2.into();
    }));
    rig.advance(5_000);
    assert_eq!(
        rig.holder().probe_clock(),
        Some(Err(FetchFailure::Invalid(PageError::Consistency(
            "items_total"
        ))))
    );
    assert!(rig.engine_gate().clock_hold());
}

#[test]
fn holder_state_is_refused_without_a_real_directory_sync() {
    assert!(matches!(
        require_directory_sync(false),
        Err(DirError::NoDirectorySync)
    ));
    const { assert!(DIRECTORY_SYNC, "this suite runs on Unix only") };
    assert!(require_directory_sync(DIRECTORY_SYNC).is_ok());
}
