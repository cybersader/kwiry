//! HOLDINGS-WIRE §15 H.16 (authority-held root pins) and H.19 item 5. These
//! focused fixtures cover the AP/OA vectors of MULTI-ROOM-CAPABILITY r17 §7.7.2
//! that the model can express; each `Fix` ablation is proved necessary by the
//! checker on a bounded focused graph. Filesystem, walk, uid, grammar,
//! pagination and multi-holder vectors are conformance obligations for the
//! consumer build (design §5.8 M11).

use kwiry_lifecycle::action::{Action, Action::*, faults};
use kwiry_lifecycle::checker::{Profile, check_from};
use kwiry_lifecycle::design::{Design, Fix};
use kwiry_lifecycle::props::{self, Prop};
use kwiry_lifecycle::row_id::RowId;
use kwiry_lifecycle::state::*;
use kwiry_lifecycle::step::{CapabilityRefusal, Config, Obs, Outcome, PinRefusal, exec, servable};

const D: Design = Design::REV11_5;
const BUDGET: Budget = Budget {
    retire_rooms: 0b11,
    change_rooms: 0b10,
    crashes: 0,
    faults: 1,
    changes: 6,
    retires: 1,
    drops: 0,
    restores: 1,
};

struct Fake {
    s: State,
    cfg: Config,
}

impl Fake {
    fn new() -> Self {
        let mut s = State::initial(BUDGET, false);
        s.env.probe = Probe::Pin;
        Self {
            s,
            cfg: Config {
                design: D,
                faults: faults::ALL,
                same_state: false,
            },
        }
    }
    fn outcome(&self, a: Action) -> Option<Outcome> {
        let out = exec(&self.s, a, &self.cfg).unwrap()?;
        assert_eq!(props::check_step(&self.s, &out, 0), None, "{a:?}");
        assert_eq!(props::check_state(&out.next, 0), None, "{a:?}");
        Some(out)
    }
    fn step(&mut self, a: Action) -> Outcome {
        let out = self.outcome(a).unwrap_or_else(|| panic!("{a:?} disabled"));
        self.s = out.next.clone();
        out
    }
    fn run(&mut self, actions: &[Action]) {
        for a in actions {
            self.step(*a);
        }
    }
    fn boot(&mut self) {
        self.run(&[Boot, Markers, Narrow]);
    }
    fn list(&mut self) {
        self.run(&[FetchStart, ListDeliver]);
        while self.s.vol.apply.is_some() {
            self.step(Apply);
        }
    }
    fn attest(&mut self, room: u8, dir: Option<u8>) -> Outcome {
        self.step(AttestRoot {
            room,
            dir,
            stale: false,
        })
    }
    fn pin(&self, r: usize) -> Option<Pin> {
        self.s.auth.core.pins[r]
    }
}

fn running() -> Fake {
    let mut f = Fake::new();
    f.boot();
    f.list();
    assert!(servable(&f.s, 0) && servable(&f.s, 1));
    f
}

/// The fix holds on the focused graph, and removing it violates `expected`.
fn ablate(fix: Fix, expected: Prop, s: State, actions: &[Action]) {
    let p = Profile {
        name: fix.finding(),
        design: D,
        budget: s.budget,
        faults: faults::ALL,
        garbage: false,
        same_state: false,
        liveness: false,
        skip: Prop::I7Deadlock.bit(),
        max_states: 100_000,
    };
    let with = check_from(&p, s.clone(), actions.to_vec());
    assert!(with.violation.is_none(), "{}", with.render());
    let without = check_from(
        &Profile {
            design: D.without(fix),
            ..p
        },
        s,
        actions.to_vec(),
    );
    assert_eq!(
        without.violation.as_ref().map(|v| v.prop),
        Some(expected),
        "{}",
        without.render()
    );
}

/// AP2, AP4, OA1, H.16.4: a re-attestation advances the revision, the list
/// carries the new pin, the old `pin_ref` is never served again, and the room
/// serves after the rebuild under the new ref.
#[test]
fn h16_reattestation_advances_the_list_and_serves_only_after_rebuild() {
    let mut f = running();
    let before = (f.s.auth.core.seq, f.s.auth.core.holder_rev);
    f.step(RepointRoot { room: 0, dir: 2 }); // storage-move to a fresh directory
    let out = f.attest(0, Some(2));
    assert!(out.rows.iter().any(|r| r == RowId::P6));
    assert_eq!(f.pin(0), Some(Pin { id: 2, pin_ref: 3 }), "fresh pin_ref");
    assert!(f.s.auth.core.seq > before.0 && f.s.auth.core.holder_rev > before.1);
    assert_eq!(
        f.s.auth.core.pin_history[2],
        Some(PinOwner::Room(0)),
        "id-space history"
    );
    // Two lists that differ only in `root_pin` differ (state_digest input).
    let old = f.s.vol.applied.unwrap();
    f.run(&[FetchStart, ListDeliver]);
    assert_ne!(f.s.vol.admitted.unwrap().pins, old.pins);
    assert_eq!(f.s.vol.admitted.unwrap().items, old.items);
    assert!(!servable(&f.s, 0), "A0 refuses the changed pin at once");
    assert!(servable(&f.s, 1), "an unchanged room keeps serving");
    while f.s.vol.apply.is_some() {
        f.step(Apply);
    }
    assert!(
        !servable(&f.s, 0),
        "R17 until the new-ref rebuild publishes"
    );
    assert_eq!(
        f.s.dur.pin_history[2],
        Some(0),
        "the pair persisted with hw"
    );
    assert!(f.step(PinRebuild(0)).rows.iter().any(|r| r == RowId::P4));
    assert_eq!(f.s.dur.derived[0], Some(3));
    assert!(servable(&f.s, 0));
    // AP5: re-attesting the same directory mints a new ref and rebuilds.
    f.attest(0, Some(2));
    f.list();
    assert!(!servable(&f.s, 0));
    f.step(PinRebuild(0));
    assert!(servable(&f.s, 0));
}

/// AP22, AP25: A0 refuses a changed or null pin for retrieval, readiness and
/// end-of-request release; the refusal survives an apply failure; an
/// in-flight query releases nothing. Without the fence the old pin serves.
#[test]
fn h16_a0_refusal_survives_apply_failure_and_in_flight_release() {
    for dir in [Some(2), None] {
        let mut f = running();
        if dir.is_some() {
            f.step(RepointRoot { room: 0, dir: 2 });
        }
        f.step(StartCapability {
            room: 0,
            iat: 0,
            exp: 60,
        });
        f.attest(0, dir);
        f.step(FetchStart);
        f.s.budget.changes = 0;
        ablate(
            Fix::RootPinFence,
            Prop::I1RootPin,
            f.s.clone(),
            &[ListDeliver],
        );
        assert!(f.step(ListDeliver).rows.iter().any(|r| r == RowId::P1));
        let out = f.step(FinishCapability);
        assert!(
            out.obs
                .contains(&Some(Obs::CapabilityRefused(CapabilityRefusal::NotActive))),
            "the in-flight query releases nothing"
        );
        // Every lifecycle write fails from here: A1 aborts (K2x).
        f.s.env.disk_full = true;
        f.s.dur.reserve = false;
        f.s.vol.rsv = Rsv::Partial;
        assert!(f.step(Apply).rows.iter().any(|r| r == RowId::K2x));
        assert!(f.s.vol.apply.is_none() && !servable(&f.s, 0));
        assert!(servable(&f.s, 1));
    }
}

/// AP3, AP8, AP19 and the H10.5 bound: a walk that observes another
/// directory indexes nothing; same-ref artifacts serve only until the stale
/// bound. Without the observation check the walk indexes another room's bytes.
#[test]
fn h16_root_mismatch_fails_closed_and_stale_service_is_bounded() {
    let mut f = running();
    f.step(RepointRoot { room: 0, dir: 1 }); // R0's path now resolves to RB's root
    assert!(
        servable(&f.s, 0),
        "H10.5: validated same-ref artifacts only"
    );
    assert!(f.step(StaleDeadline).rows.iter().any(|r| r == RowId::P5));
    assert!(!servable(&f.s, 0), "then R17");
    assert!(servable(&f.s, 1));

    let mut f = running();
    f.s.dur.derived[0] = None; // AP19: data root wiped
    f.s.ghost.derived_ref[0] = None;
    f.step(RepointRoot { room: 0, dir: 1 });
    assert!(f.outcome(PinRebuild(0)).is_none(), "root_pin_mismatch");
    ablate(
        Fix::RootPinServing,
        Prop::I1RootPin,
        f.s.clone(),
        &[PinRebuild(0)],
    );
    f.step(RepointRoot { room: 0, dir: 0 });
    f.step(PinRebuild(0));
    assert_eq!(f.s.dur.derived[0], Some(1), "no re-attestation needed");
    assert!(servable(&f.s, 0));
}

/// AP24: a list that attests R1 to a `pin_id` the holder's pin-history holds
/// under R0 is refused (`root_pin_previously_listed`), even where the
/// authority's history lost the pair (the H.19 named residual).
#[test]
fn h16_pin_history_refuses_another_rooms_directory() {
    let mut f = running();
    // R0 moves to D2 and back; D2 keeps R0's old bytes.
    f.step(RepointRoot { room: 0, dir: 2 });
    f.attest(0, Some(2));
    f.list();
    f.step(PinRebuild(0));
    f.step(RepointRoot { room: 0, dir: 0 });
    f.attest(0, Some(0));
    f.list();
    f.step(PinRebuild(0));
    assert_eq!(f.s.dur.pin_history, [Some(0), Some(1), Some(0)]);
    // The authority history lost the D2 pair; R1's path now resolves to D2.
    f.s.auth.core.pin_history[2] = None;
    f.step(RepointRoot { room: 1, dir: 2 });
    let attack = [
        AttestRoot {
            room: 1,
            dir: Some(2),
            stale: false,
        },
        FetchStart,
        ListDeliver,
        Apply,
        PinRebuild(1),
    ];
    f.s.budget.changes = 1;
    ablate(Fix::RootPinHistory, Prop::I1RootPin, f.s.clone(), &attack);
    f.attest(1, Some(2));
    f.run(&[FetchStart, ListDeliver]);
    assert!(!servable(&f.s, 1), "refused at A0");
    while f.s.vol.apply.is_some() {
        f.step(Apply);
    }
    assert_eq!(
        f.s.dur.pin_history[2],
        Some(0),
        "first pair wins; never replaced"
    );
    assert!(!servable(&f.s, 1) && f.outcome(PinRebuild(1)).is_none());
    assert!(servable(&f.s, 0));
}

/// OA1–OA4, H.16.2: refusals change nothing; `pin_id: none` keeps history.
#[test]
fn h16_attestation_refusals_change_nothing() {
    let mut f = running();
    let refused = |f: &mut Fake, room, dir, stale, code| {
        let before = f.s.auth.clone();
        let out = f.step(AttestRoot { room, dir, stale });
        assert!(out.obs.contains(&Some(Obs::PinAttestRefused(code))));
        assert_eq!(f.s.auth, before, "nothing changes");
    };
    refused(&mut f, 0, Some(1), false, PinRefusal::Reused); // OA3: RB's pin_id
    refused(&mut f, 0, Some(2), true, PinRefusal::Conflict); // OA2
    f.attest(0, Some(0)); // OA8 second case: a room's own pin_id is allowed
    f.attest(0, None);
    assert_eq!(f.pin(0), None);
    assert_eq!(
        f.s.auth.core.pin_history[0],
        Some(PinOwner::Room(0)),
        "kept"
    );
    f.list();
    assert!(!servable(&f.s, 0), "AP1/AP18: a null pin withholds");
    assert_eq!(f.s.dur.units[0], UDur::Present, "and deletes nothing");
    f.step(AuthRetire(1));
    refused(&mut f, 1, Some(2), false, PinRefusal::HoldingInactive); // OA4
    assert_eq!(
        f.pin(1),
        Some(Pin { id: 1, pin_ref: 2 }),
        "terminating keeps its last pin"
    );
    f.list();
    assert!(!servable(&f.s, 1), "AP16");
}

/// AP6, AP7: nothing serves before a list of this process lifetime, and
/// rolled-back artifacts under an older `pin_ref` are never opened.
#[test]
fn h16_holder_rollback_never_serves_an_older_ref() {
    let mut f = running();
    f.step(RepointRoot { room: 0, dir: 2 });
    f.attest(0, Some(2));
    f.list();
    f.step(PinRebuild(0));
    f.s.budget.crashes = 1;
    f.step(Crash);
    f.s.dur.derived[0] = Some(1); // holder dir and data root rolled back
    f.s.ghost.derived_ref[0] = Some(1);
    f.boot();
    assert!(!servable(&f.s, 0), "AP7: no list in this process");
    f.list();
    assert!(!servable(&f.s, 0), "AP6: pr1 artifacts are never opened");
    f.step(PinRebuild(0));
    assert!(servable(&f.s, 0));
}

/// OA7, H.19 item 5: `restore_journal_lost` nulls every active pin, keeps
/// inactive pins and the history; only a fresh attestation and rebuild serve
/// again. Without the reset a pre-loss artifact serves.
#[test]
fn h16_journal_lost_nulls_active_pins() {
    let mut f = running();
    f.step(AuthRetire(1));
    f.list();
    f.step(AuthBackup);
    f.s.env.bad_journal = true;
    f.step(AuthRestore);
    assert!(f.s.auth.failed_restore);
    f.s.budget.changes = 1;
    let recover = [
        OpRestoreJournalLost,
        FetchStart,
        ListDeliver,
        OpAcceptEpoch,
        Boot,
        Markers,
        Narrow,
        Apply,
    ];
    ablate(
        Fix::JournalLostPinReset,
        Prop::I1RootPin,
        f.s.clone(),
        &recover,
    );
    assert!(
        f.step(OpRestoreJournalLost)
            .rows
            .iter()
            .any(|r| r == RowId::P8)
    );
    assert_eq!(f.pin(0), None, "active pin nulled");
    assert_eq!(
        f.pin(1),
        Some(Pin { id: 1, pin_ref: 2 }),
        "terminating keeps it"
    );
    assert_eq!(f.s.auth.core.pin_history[0], Some(PinOwner::Room(0)));
    assert!(!f.s.auth.rollback_suspected, "suspicion clears");
    f.run(&[FetchStart, ListDeliver, OpAcceptEpoch]);
    f.boot();
    f.list();
    assert!(!servable(&f.s, 0));
    f.attest(0, Some(0)); // holder-reset, expect_current: none
    f.list();
    f.step(PinRebuild(0));
    assert!(servable(&f.s, 0), "a new pin_ref serves again");
}

/// OA5, OA11, H.16.3: replay restores the later `pin_ref`; an attestation
/// whose holding did not survive replay reserves its `pin_id` as `"*"`, so
/// even a re-enrolled room cannot reuse the directory.
#[test]
fn h16_replay_restores_later_pins_and_reserves_orphans() {
    let mut f = running();
    f.step(AuthBackup);
    f.step(RepointRoot { room: 0, dir: 2 });
    f.attest(0, Some(2));
    f.step(AuthRestore);
    assert_eq!(f.pin(0), Some(Pin { id: 2, pin_ref: 3 }), "OA5");

    let mut f = Fake::new();
    f.s.auth.core.rooms[1].phase = APhase::Unlisted; // R1's holding does not exist yet
    f.s.auth.core.pins[1] = None;
    f.boot();
    f.list();
    f.run(&[AuthBackup, AuthRelist(1), RepointRoot { room: 1, dir: 2 }]);
    f.attest(1, Some(2));
    // Both designs restore from here; the re-enrolled room then tries D2.
    f.s.budget.changes = 2;
    let reenrol = [
        AuthRestore,
        AuthRelist(1),
        AttestRoot {
            room: 1,
            dir: Some(2),
            stale: false,
        },
        FetchStart,
        ListDeliver,
        OpAcceptEpoch,
        Boot,
        Markers,
        Narrow,
        Apply,
        PinRebuild(1),
    ];
    ablate(
        Fix::OrphanPinReserve,
        Prop::I1RootPin,
        f.s.clone(),
        &reenrol,
    );
    let out = f.step(AuthRestore);
    assert!(out.obs.contains(&Some(Obs::PinReplaySkipped)));
    assert_eq!(f.s.auth.core.pin_history[2], Some(PinOwner::Orphan));
    f.step(AuthRelist(1));
    let out = f.step(AttestRoot {
        room: 1,
        dir: Some(2),
        stale: false,
    });
    assert!(
        out.obs
            .contains(&Some(Obs::PinAttestRefused(PinRefusal::Reused)))
    );
}
