//! Classification, A0–A5 and the serving gate, including storage pins.

use super::*;
use crate::client::FetchFailure;
use crate::engine::{
    ArtifactStamp, CapabilityTimes, EngineQuery, EngineSettings, RootObservation::*,
};
use crate::holder::ListOutcome;
use crate::schedule::EarlyRelist;
use crate::store::{Disposition, ResourceKey};
use crate::testkit::{Forced, pin_id};

fn key(kind: &str, id: &str) -> ResourceKey {
    ResourceKey {
        kind: kind.into(),
        id: id.into(),
    }
}

fn pin_ref_of(rig: &Rig, holding: &str) -> String {
    rig.authority.state().items[holding]["root_pin"]["pin_ref"]
        .as_str()
        .expect("pinned")
        .to_owned()
}

#[test]
fn h1_serves_only_listed_active_and_withholds_unknown_data_without_deleting() {
    let (_dir, mut rig) = rig();
    for (id, units) in [("res-a", 2), ("res-b", 3), ("res-c", 5), ("res-x", 7)] {
        rig.store.hold(PLAIN, id, true, units);
    }
    {
        let mut authority = rig.authority.state();
        authority.add_active(PLAIN, "res-a");
        let b = authority.add_active(PLAIN, "res-b");
        let c = authority.add_active(PLAIN, "res-c");
        authority.retire(&b);
        authority.retire(&c);
        authority.set_obligation_state(&c, "complete");
    }
    let report = applied(rig.holder().list_once());
    let dispositions = &report.plan.dispositions;
    assert!(matches!(
        dispositions[&key(PLAIN, "res-a")],
        Disposition::Mapped { .. }
    ));
    assert!(matches!(
        dispositions[&key(PLAIN, "res-b")],
        Disposition::Obligated { .. }
    ));
    assert!(matches!(
        dispositions[&key(PLAIN, "res-c")],
        Disposition::RetiredComplete { .. }
    ));
    assert_eq!(
        dispositions[&key(PLAIN, "res-x")],
        Disposition::WithheldUnknown
    );
    assert_eq!(report.plan.unattributed_units, 7);
    assert_eq!(plain(&rig, "res-a"), Ok(()));
    assert_eq!(plain(&rig, "res-b"), Err(Refusal::Tombstoned));
    assert_eq!(plain(&rig, "res-c"), Err(Refusal::Tombstoned));
    assert_eq!(plain(&rig, "res-x"), Err(Refusal::NotListedActive));
    for id in ["res-a", "res-b", "res-c", "res-x"] {
        assert!(rig.store.bytes_present(PLAIN, id), "{id}");
    }
    let tombstoned: Vec<_> = rig
        .holder()
        .state()
        .tombstones
        .iter()
        .map(|t| t.resource_id.clone())
        .collect();
    assert_eq!(tombstoned, ["res-b", "res-c"]);
    assert_eq!(rig.store.view().revoked.len(), 1);
}

#[test]
fn h7_h17_leases_lapse_on_either_clock_and_a_fresh_list_restores_serving() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-a", true, 1);
    rig.authority.state().add_active(PLAIN, "res-a");
    applied(rig.holder().list_once());
    rig.advance(899_999);
    assert_eq!(plain(&rig, "res-a"), Ok(()));
    rig.advance(1);
    assert_eq!(plain(&rig, "res-a"), Err(Refusal::LeaseLapsed));
    rig.authority.state().forced.push_back(Forced::Unreachable);
    assert!(matches!(
        failed(rig.relist()),
        FetchFailure::Unreachable { .. }
    ));
    assert_eq!(plain(&rig, "res-a"), Err(Refusal::LeaseLapsed));
    assert!(rig.store.bytes_present(PLAIN, "res-a"));
    applied(rig.relist());
    assert_eq!(plain(&rig, "res-a"), Ok(()), "no rebuild needed");

    // A wall clock stepped back an hour does not extend the lease.
    rig.clock.step_wall(-3_600_000);
    rig.advance(900_000);
    assert_eq!(plain(&rig, "res-a"), Err(Refusal::LeaseLapsed));
}

#[test]
fn h13_a_tombstoned_resource_listed_active_again_stays_refused() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-a", true, 4);
    let holding = rig.authority.state().add_active(PLAIN, "res-a");
    applied(rig.holder().list_once());
    rig.authority.state().retire(&holding);
    applied(rig.relist());
    {
        let mut authority = rig.authority.state();
        let item = authority.items.get_mut(&holding).unwrap();
        item["phase"] = "active".into();
        item["obligation"] = serde_json::Value::Null;
        authority.commit();
    }
    let report = applied(rig.relist());
    assert_eq!(report.tombstone_conflicts, 1);
    assert!(matches!(
        report.plan.dispositions[&key(PLAIN, "res-a")],
        Disposition::TombstoneConflict { .. }
    ));
    assert_eq!(report.plan.unattributed_units, 4);
    assert_eq!(plain(&rig, "res-a"), Err(Refusal::Tombstoned));
    assert!(codes(&rig).contains(&"tombstone_conflict"));
}

#[test]
fn i4_tombstones_only_grow_and_unlisted_tombstoned_data_is_quarantined() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-a", true, 6);
    let holding = rig.authority.state().add_active(PLAIN, "res-a");
    rig.authority.state().retire(&holding);
    applied(rig.holder().list_once());
    rig.authority
        .state()
        .set_obligation_state(&holding, "complete");
    applied(rig.relist());
    rig.authority.state().remove(&holding);
    let report = applied(rig.relist());
    assert_eq!(
        report.plan.dispositions[&key(PLAIN, "res-a")],
        Disposition::QuarantinedUnattributed
    );
    assert_eq!(report.plan.unattributed_units, 6);
    assert_eq!(rig.holder().state().tombstones.len(), 1);
    assert!(rig.store.bytes_present(PLAIN, "res-a"));
    assert_eq!(plain(&rig, "res-a"), Err(Refusal::Tombstoned));
}

#[test]
fn v13_a_blocked_obligation_is_terminating_and_authorizes_nothing() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-a", true, 1);
    let holding = rig.authority.state().add_active(PLAIN, "res-a");
    rig.authority.state().retire(&holding);
    rig.authority
        .state()
        .set_obligation_state(&holding, "blocked");
    let report = applied(rig.holder().list_once());
    assert!(matches!(
        report.plan.dispositions[&key(PLAIN, "res-a")],
        Disposition::Obligated { .. }
    ));
    assert_eq!(plain(&rig, "res-a"), Err(Refusal::Tombstoned));
    assert!(rig.store.bytes_present(PLAIN, "res-a"));
}

#[test]
fn q8_retired_escalated_is_obligated() {
    let (_dir, mut rig) = rig();
    let holding = rig.authority.state().add_active(PLAIN, "res-a");
    rig.authority.state().retire(&holding);
    {
        let mut authority = rig.authority.state();
        authority.items.get_mut(&holding).unwrap()["phase"] = "retired".into();
        authority.set_obligation_state(&holding, "escalated");
    }
    let report = applied(rig.holder().list_once());
    assert!(matches!(
        report.plan.dispositions[&key(PLAIN, "res-a")],
        Disposition::Obligated { .. }
    ));
}

#[test]
fn c13_listed_active_but_unconfigured_is_withheld_and_attributed() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-z", false, 9);
    rig.authority.state().add_active(PLAIN, "res-z");
    let report = applied(rig.holder().list_once());
    assert!(matches!(
        report.plan.dispositions[&key(PLAIN, "res-z")],
        Disposition::WithheldUnconfigured { .. }
    ));
    assert_eq!(report.plan.unattributed_units, 0);
    assert_eq!(plain(&rig, "res-z"), Err(Refusal::NotMapped));
}

#[test]
fn ap1_a_null_pin_never_serves() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PINNED, "res-free", true, 1);
    rig.authority.state().add_active(PINNED, "res-free");
    applied(rig.holder().list_once());
    assert_eq!(
        pinned(&rig, "res-free", "pr_0000000000000000000000", Matches),
        Err(Refusal::Engine(EngineRefusal::RootPinMissing))
    );
}

#[test]
fn ap2_ap3_pinned_resources_serve_only_stamped_artifacts_from_a_matching_root() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PINNED, "res-a", true, 1);
    let holding = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().attest(&holding, "dir-a0");
    applied(rig.holder().list_once());
    let pr1 = pin_ref_of(&rig, &holding);
    assert_eq!(pinned(&rig, "res-a", &pr1, Matches), Ok(()));
    assert_eq!(
        pinned(&rig, "res-a", "pr_9999999999999999999999", Matches),
        Err(Refusal::Engine(EngineRefusal::ArtifactPinRef))
    );
    assert_eq!(
        pinned(&rig, "res-a", &pr1, Unobserved),
        Err(Refusal::Engine(EngineRefusal::RootPinMismatch))
    );
    // AP3: root swapped; bounded stale service of validated same-ref artifacts.
    let detected = {
        use crate::clock::Clock;
        rig.clock.mono_ms()
    };
    let mismatch = Mismatch {
        detected_mono_ms: detected,
    };
    assert_eq!(pinned(&rig, "res-a", &pr1, mismatch), Ok(()));
    let unvalidated = rig.engine_gate().check(
        &EngineQuery {
            resource_id: "res-a",
            capability: None,
            artifact: Some(ArtifactStamp {
                pin_ref: &pr1,
                validated_under_matching_root: false,
            }),
            root: mismatch,
        },
        Cut::Admission,
    );
    assert_eq!(
        unvalidated,
        Err(Refusal::Engine(EngineRefusal::RootPinMismatch))
    );
    assert_eq!(
        pinned(&rig, "res-a", "pr_9999999999999999999999", mismatch),
        Err(Refusal::Engine(EngineRefusal::ArtifactPinRef)),
        "another ref never serves, inside the stale bound included"
    );
    // Keep the lease alive while the stale window runs out.
    for _ in 0..4 {
        rig.advance(895_000);
        applied(rig.relist());
    }
    rig.advance(1);
    assert_eq!(
        pinned(&rig, "res-a", &pr1, mismatch),
        Err(Refusal::Engine(EngineRefusal::RootPinMismatch)),
        "past max_stale_serve_seconds (3600 s)"
    );
}

#[test]
fn ap4_ap5_ap25_reattestation_refuses_the_old_ref_from_a0_even_when_apply_fails() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PINNED, "res-a", true, 1);
    let holding = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().attest(&holding, "dir-a0");
    applied(rig.holder().list_once());
    let pr1 = pin_ref_of(&rig, &holding);
    let in_flight = |rig: &Rig, pin_ref: &str| {
        rig.engine_gate().check(
            &EngineQuery {
                resource_id: "res-a",
                capability: None,
                artifact: Some(ArtifactStamp {
                    pin_ref,
                    validated_under_matching_root: true,
                }),
                root: Matches,
            },
            Cut::Release,
        )
    };
    assert_eq!(in_flight(&rig, &pr1), Ok(()));

    // AP5: re-attest onto the same directory; A1 fails (injected ENOSPC).
    rig.authority.state().attest(&holding, "dir-a0");
    let pr2 = pin_ref_of(&rig, &holding);
    rig.io_faults.fail_writes(1);
    assert_eq!(rig.relist(), ListOutcome::ApplyFailed { step: "A1" });
    assert_eq!(in_flight(&rig, &pr1), Err(Refusal::RefusedAtAdmit));
    assert_eq!(
        pinned(&rig, "res-a", &pr2, Matches),
        Err(Refusal::RefusedAtAdmit)
    );
    // A3 fails on the retry: still refused.
    rig.store.view().fail_publish = true;
    assert_eq!(rig.relist(), ListOutcome::ApplyFailed { step: "A3" });
    assert_eq!(in_flight(&rig, &pr1), Err(Refusal::RefusedAtAdmit));
    rig.store.view().fail_publish = false;
    // A4 succeeds: only the new ref broadens.
    applied(rig.relist());
    assert_eq!(
        pinned(&rig, "res-a", &pr1, Matches),
        Err(Refusal::Engine(EngineRefusal::ArtifactPinRef))
    );
    assert_eq!(pinned(&rig, "res-a", &pr2, Matches), Ok(()));

    // AP4 + AP25: unpin, with A1 failing; then succeeding stays refused.
    rig.authority.state().unpin(&holding);
    rig.io_faults.fail_writes(1);
    assert_eq!(rig.relist(), ListOutcome::ApplyFailed { step: "A1" });
    assert_eq!(
        pinned(&rig, "res-a", &pr2, Matches),
        Err(Refusal::Engine(EngineRefusal::RootPinMissing))
    );
    applied(rig.relist());
    assert_eq!(
        pinned(&rig, "res-a", &pr2, Matches),
        Err(Refusal::Engine(EngineRefusal::RootPinMissing))
    );
    assert!(codes(&rig).contains(&"lifecycle_apply_failed"));
}

#[test]
fn ap6_ap7_h24_after_restart_nothing_serves_until_a_list_of_this_process() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PINNED, "res-a", true, 1);
    rig.store.hold(PLAIN, "res-p", true, 1);
    let holding = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().add_active(PLAIN, "res-p");
    rig.authority.state().attest(&holding, "dir-a0");
    applied(rig.holder().list_once());
    let pr1 = pin_ref_of(&rig, &holding);
    assert_eq!(pinned(&rig, "res-a", &pr1, Matches), Ok(()));

    // AP7: restart with the authority unreachable; leases would still hold.
    rig.restart().unwrap();
    rig.authority.state().forced.push_back(Forced::Unreachable);
    assert!(matches!(
        failed(rig.relist()),
        FetchFailure::Unreachable { .. }
    ));
    assert_eq!(
        pinned(&rig, "res-a", &pr1, Matches),
        Err(Refusal::NoListThisProcess)
    );
    assert_eq!(plain(&rig, "res-p"), Err(Refusal::NoListThisProcess));

    // AP6: the pin changed while the holder was down; its first list
    // carries pr2 and pr1 artifacts never serve.
    rig.authority.state().attest(&holding, "dir-a1");
    let pr2 = pin_ref_of(&rig, &holding);
    rig.restart().unwrap();
    applied(rig.relist());
    assert_eq!(
        pinned(&rig, "res-a", &pr1, Matches),
        Err(Refusal::Engine(EngineRefusal::ArtifactPinRef))
    );
    assert_eq!(pinned(&rig, "res-a", &pr2, Matches), Ok(()));
}

#[test]
fn ap16_ap18_terminating_pinned_resources_never_serve_and_nothing_is_deleted() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PINNED, "res-a", true, 2);
    let holding = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().attest(&holding, "dir-a0");
    applied(rig.holder().list_once());
    let pr1 = pin_ref_of(&rig, &holding);
    rig.authority.state().retire(&holding);
    applied(rig.relist());
    assert_eq!(
        pinned(&rig, "res-a", &pr1, Matches),
        Err(Refusal::Tombstoned)
    );
    // Removed from configuration while still listed: withheld, not deleted.
    let (_dir, mut rig) = super::rig();
    rig.store.hold(PINNED, "res-b", false, 2);
    let holding = rig.authority.state().add_active(PINNED, "res-b");
    rig.authority.state().attest(&holding, "dir-b0");
    applied(rig.holder().list_once());
    assert_eq!(
        pinned(&rig, "res-b", &pin_ref_of(&rig, &holding), Matches),
        Err(Refusal::NotMapped)
    );
    assert!(rig.store.bytes_present(PINNED, "res-b"));
}

#[test]
fn ap24_ap23_a_pin_held_under_another_resource_is_refused_without_stale_service() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PINNED, "res-a", true, 1);
    rig.store.hold(PINNED, "res-b", true, 1);
    let b = rig.authority.state().add_active(PINNED, "res-b");
    rig.authority.state().attest(&b, "dir-shared");
    applied(rig.holder().list_once());
    let a = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().attest(&a, "dir-shared");
    applied(rig.relist());
    let pr = pin_ref_of(&rig, &a);
    assert_eq!(
        pinned(&rig, "res-a", &pr, Matches),
        Err(Refusal::Engine(EngineRefusal::RootPinPreviouslyListed))
    );
    assert_eq!(
        pinned(
            &rig,
            "res-a",
            &pr,
            Mismatch {
                detected_mono_ms: 0
            }
        ),
        Err(Refusal::Engine(EngineRefusal::RootPinPreviouslyListed))
    );
    assert!(
        rig.engine_gate()
            .pin_previously_listed(&pin_id("dir-shared"), "res-a")
    );
    assert!(
        !rig.engine_gate()
            .pin_previously_listed(&pin_id("dir-other"), "res-a")
    );
}

#[test]
fn ap11_walk_inputs_come_from_the_list_and_the_pin_history() {
    let (_dir, mut rig) = rig();
    let a = rig.authority.state().add_active(PINNED, "res-a");
    let b = rig.authority.state().add_active(PINNED, "res-b");
    rig.authority.state().attest(&a, "dir-a0");
    rig.authority.state().attest(&b, "dir-b0");
    applied(rig.holder().list_once());
    let inputs = rig.engine_gate().walk_inputs("res-a");
    assert!(
        inputs.nesting_pins.contains(&pin_id("dir-b0")),
        "AP12 nesting"
    );
    assert!(!inputs.nesting_pins.contains(&pin_id("dir-a0")));
    assert!(inputs.history_available);

    // AP11b: B re-attested; its old directory is skipped from history.
    rig.authority.state().attest(&b, "dir-b1");
    applied(rig.relist());
    let inputs = rig.engine_gate().walk_inputs("res-a");
    assert!(inputs.skip_pins.contains(&pin_id("dir-b0")));
    assert!(inputs.nesting_pins.contains(&pin_id("dir-b1")));
    assert!(!inputs.skip_pins.contains(&pin_id("dir-a0")));

    // AP11: B retired: its last pin is skipped while listed.
    rig.authority.state().retire(&b);
    applied(rig.relist());
    assert!(
        rig.engine_gate()
            .walk_inputs("res-a")
            .skip_pins
            .contains(&pin_id("dir-b1"))
    );

    // AP11c: B dropped off the list: the history still skips it.
    rig.authority.state().remove(&b);
    applied(rig.relist());
    let inputs = rig.engine_gate().walk_inputs("res-a");
    assert!(inputs.skip_pins.contains(&pin_id("dir-b1")));
    assert!(inputs.skip_pins.contains(&pin_id("dir-b0")));
}

#[test]
fn ap22_ap27_an_unpin_unreachable_runs_out_its_lease_and_failed_lists_never_renew() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PINNED, "res-a", true, 1);
    let a = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().attest(&a, "dir-a0");
    applied(rig.holder().list_once());
    let pr = pin_ref_of(&rig, &a);
    rig.authority.state().unpin(&a);
    for round in 0..4 {
        if round == 3 {
            assert_eq!(
                pinned(&rig, "res-a", &pr, Matches),
                Ok(()),
                "serves until its lease"
            );
        }
        rig.authority.state().forced.push_back(Forced::Unreachable);
        rig.advance(295_000);
        assert!(matches!(
            failed(rig.holder().list_once()),
            FetchFailure::Unreachable { .. }
        ));
    }
    assert_eq!(
        pinned(&rig, "res-a", &pr, Matches),
        Err(Refusal::LeaseLapsed)
    );
    assert_eq!(
        codes(&rig)
            .iter()
            .filter(|c| **c == "lifecycle_unreachable")
            .count(),
        4
    );
}

#[test]
fn ap27_the_pin_history_ceiling_is_raised_before_persisting_and_never_evicts() {
    let (_dir, mut rig) = rig();
    // Lower the durable ceiling, as an operator setting would.
    let path = rig.dir.join(crate::state::STATE_FILE);
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    state["engine"]["pin_history_ceiling"] = 2.into();
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    rig.restart().unwrap();
    for (id, dir) in [("res-a", "d-a"), ("res-b", "d-b"), ("res-c", "d-c")] {
        let holding = rig.authority.state().add_active(PINNED, id);
        rig.authority.state().attest(&holding, dir);
    }
    applied(rig.holder().list_once());
    assert!(
        rig.events
            .events()
            .contains(&crate::events::HolderEvent::Engine(
                crate::engine::EngineEvent::PinHistoryCeilingRaised { from: 2, to: 4 }
            ))
    );
    assert_eq!(rig.holder().state().engine.pin_history.len(), 3);
    assert_eq!(rig.holder().state().engine.pin_history_ceiling, 4);
}

#[test]
fn c17_c18_h21_capability_lifetime_future_iat_and_expiry_at_both_cuts() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-a", true, 1);
    rig.authority.state().add_active(PLAIN, "res-a");
    applied(rig.holder().list_once());
    let now = now_s(&rig);
    let check = |rig: &Rig, iat, exp, cut| with_capability(rig, "res-a", iat, exp, cut);
    assert_eq!(check(&rig, now, now + 60, Cut::Admission), Ok(()));
    assert_eq!(
        check(&rig, now, now + 61, Cut::Admission),
        Err(Refusal::Engine(EngineRefusal::CapabilityLifetime))
    );
    assert_eq!(check(&rig, now + 30, now + 60, Cut::Admission), Ok(()));
    assert_eq!(
        check(&rig, now + 31, now + 60, Cut::Admission),
        Err(Refusal::Engine(EngineRefusal::CapabilityNotYetValid))
    );
    assert_eq!(check(&rig, now - 90, now - 30, Cut::Release), Ok(()));
    assert_eq!(
        check(&rig, now - 91, now - 31, Cut::Release),
        Err(Refusal::Engine(EngineRefusal::CapabilityExpired))
    );
    assert_eq!(
        check(&rig, now - 91, now - 31, Cut::Admission),
        Err(Refusal::Engine(EngineRefusal::CapabilityExpired))
    );
    assert_eq!(
        EngineSettings::new(31, 3600),
        Err("clock_skew_allowance above 30 s")
    );
    assert_eq!(
        EngineSettings::new(30, 86_401),
        Err("max_stale_serve_seconds above 86400")
    );
    let _ = CapabilityTimes { iat_s: 0, exp_s: 0 };
}

#[test]
fn amendment_b_early_relists_are_for_unlisted_untombstoned_resources_and_coalesce() {
    let (_dir, mut rig) = rig();
    let holding = rig.authority.state().add_active(PLAIN, "res-a");
    let gone = rig.authority.state().add_active(PLAIN, "res-gone");
    rig.authority.state().retire(&gone);
    applied(rig.holder().list_once());
    assert_eq!(rig.holder().request_early_relist("res-a"), None);
    assert_eq!(rig.holder().request_early_relist("res-gone"), None);
    assert!(matches!(
        rig.holder().request_early_relist("res-new"),
        Some(EarlyRelist::Scheduled { .. })
    ));
    assert_eq!(
        rig.holder().request_early_relist("res-new2"),
        Some(EarlyRelist::Coalesced)
    );
    assert_eq!(plain(&rig, "res-new"), Err(Refusal::NotListedActive));
    let _ = holding;
}

#[test]
fn ap19_a_wiped_data_root_is_not_a_reset() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PINNED, "res-a", true, 1);
    let a = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().attest(&a, "dir-a0");
    applied(rig.holder().list_once());
    let before = rig.holder().state().clone();
    {
        let mut root = rig.data_root.view();
        root.marker = None;
    }
    rig.restart().unwrap();
    assert!(!codes(&rig).contains(&"holder_state_reset"));
    let after = rig.holder().state();
    assert_eq!(after.high_water, before.high_water);
    assert_eq!(after.engine.pin_history, before.engine.pin_history);
    assert!(!after.reset_pending);
    let pr = pin_ref_of(&rig, &a);
    applied(rig.relist());
    assert_eq!(pinned(&rig, "res-a", &pr, Matches), Ok(()));
}
