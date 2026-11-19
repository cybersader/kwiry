//! A holder whose wire profile has no engine section (a device, a sync
//! client, a backup target) runs on the generic core alone: no pins, no
//! capability bounds, no clock hold.

use super::*;
use crate::client::FetchFailure;
use crate::engine::EngineState;
use crate::store::{Disposition, ResourceKey};
use crate::testkit::neutral_profile;
use crate::verify::PageError;

fn core_rig() -> (tempfile::TempDir, Rig) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut profile = neutral_profile();
    profile.engine = None;
    let rig = Rig::with_profile(&dir.path().join("holder"), profile, "example-lifecycle/v1");
    (dir, rig)
}

#[test]
fn a_core_only_holder_lists_applies_serves_and_quarantines() {
    let (_dir, mut rig) = core_rig();
    assert!(rig.holder().engine_gate().is_none());
    for (kind, id) in [
        (PINNED, "res-c"),
        (PLAIN, "res-a"),
        (PLAIN, "res-b"),
        (PLAIN, "res-x"),
    ] {
        rig.store.hold(kind, id, true, 2);
    }
    rig.authority.state().add_active(PINNED, "res-c");
    rig.authority.state().add_active(PLAIN, "res-a");
    let b = rig.authority.state().add_active(PLAIN, "res-b");
    rig.authority.state().retire(&b);
    let report = applied(rig.holder().list_once());
    // The engine's pinned kind is an ordinary kind here: no pin member.
    assert_eq!(plain(&rig, "res-c"), Ok(()));
    assert_eq!(plain(&rig, "res-a"), Ok(()));
    assert_eq!(plain(&rig, "res-b"), Err(Refusal::Tombstoned));
    assert_eq!(plain(&rig, "res-x"), Err(Refusal::NotListedActive));
    let unknown = ResourceKey {
        kind: PLAIN.into(),
        id: "res-x".into(),
    };
    assert_eq!(
        report.plan.dispositions[&unknown],
        Disposition::WithheldUnknown
    );
    assert_eq!(report.plan.unattributed_units, 2);
    for id in ["res-a", "res-b", "res-x"] {
        assert!(rig.store.bytes_present(PLAIN, id));
    }
    rig.advance(900_000);
    assert_eq!(plain(&rig, "res-a"), Err(Refusal::LeaseLapsed));
    assert_eq!(rig.holder().state().engine, EngineState::default());
}

#[test]
fn a_core_only_holder_ignores_clock_evidence_and_keeps_no_engine_state() {
    let (_dir, mut rig) = core_rig();
    rig.authority.state().offset_ms = 45_000;
    applied(rig.holder().list_once());
    let now = now_s(&rig);
    rig.holder().observe_capability_iat(now + 3_600);
    assert!(rig.holder().probe_clock().is_none());
    assert!(!codes(&rig).iter().any(|c| c.starts_with("clock_")));
    assert_eq!(rig.holder().state().engine, EngineState::default());
}

#[test]
fn a_pin_member_is_refused_for_a_holder_without_the_engine_profile() {
    let (_dir, mut rig) = core_rig();
    rig.authority.state().add_active(PINNED, "res-c");
    rig.authority.state().hooks.item = Some(Box::new(|item| {
        item.insert("root_pin".into(), serde_json::Value::Null);
    }));
    assert_eq!(
        failed(rig.holder().list_once()),
        FetchFailure::Invalid(PageError::RootPin("member on an unpinned item"))
    );
}

#[test]
fn engine_settings_must_match_the_profile() {
    let (_dir, mut rig) = core_rig();
    rig.config.engine = Some(crate::engine::EngineSettings::new(30, 3_600).unwrap());
    let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rig.restart()));
    assert!(
        error.is_err(),
        "a core-only profile refuses engine settings"
    );
}
