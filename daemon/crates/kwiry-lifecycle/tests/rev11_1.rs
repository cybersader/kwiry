//! HOLDINGS-WIRE §15 H.22–H.25 (design revision 11.1). Each amendment is a
//! model fix; removing it reproduces the counterexample it closes, and the
//! same exploration holds with it.

use kwiry_lifecycle::action::{Action, Action::*, FaultCode, faults};
use kwiry_lifecycle::checker::{Profile, check_from, check_from_with_goal};
use kwiry_lifecycle::design::{Design, Fix};
use kwiry_lifecycle::props::{self, Prop};
use kwiry_lifecycle::row_id::RowId;
use kwiry_lifecycle::state::*;
use kwiry_lifecycle::step::{
    CapabilityRefusal, Config, Obs, Outcome, clock_held, exec, mintable, servable, status_gate_shut,
};

const BUDGET: Budget = Budget {
    retire_rooms: 1,
    change_rooms: 2,
    crashes: 1,
    faults: 2,
    changes: 1,
    retires: 1,
    drops: 0,
    restores: 1,
};
const NONE: Budget = Budget {
    retire_rooms: 0,
    change_rooms: 0,
    crashes: 0,
    faults: 0,
    changes: 0,
    retires: 0,
    drops: 0,
    restores: 0,
};
const D: Design = Design::REV11_3;

fn profile(name: &'static str) -> Profile {
    Profile {
        name,
        design: D,
        budget: BUDGET,
        faults: faults::ALL,
        garbage: false,
        same_state: false,
        liveness: false,
        skip: Prop::I7Deadlock.bit(),
        max_states: 200_000,
    }
}

struct Fake {
    s: State,
    cfg: Config,
}

impl Fake {
    fn new() -> Self {
        Fake {
            s: State::initial(BUDGET, false),
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
    fn enabled(&self, a: Action) -> bool {
        exec(&self.s, a, &self.cfg).unwrap().is_some()
    }
    fn step(&mut self, a: Action) -> Outcome {
        let out = self.outcome(a).unwrap_or_else(|| {
            panic!(
                "{a:?} disabled: {}",
                kwiry_lifecycle::checker::summary(&self.s)
            )
        });
        self.s = out.next.clone();
        out
    }
    fn rows(&mut self, a: Action) -> Vec<RowId> {
        self.step(a).rows.iter().collect()
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
    fn request(&mut self, c: u8) {
        self.run(&[Enqueue(c), Transmit(c), Handoff(c), Dispatch(c)]);
    }
    fn reply(&mut self, c: u8) -> RespKind {
        self.step(AuthProcess { c, idx: 0 });
        let kind = self.s.net.resps[c as usize].get(0).unwrap().kind;
        self.step(Deliver { c, idx: 0 });
        kind
    }
}

/// With the fix the exploration holds; without it, `expected` is the first
/// violation found.
fn ablate(fix: Fix, expected: Prop, s: State, actions: &[Action]) {
    let p = profile(fix.finding());
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

/// Run `actions` in order under `design` and return the first property any
/// step violates. Every step must be enabled.
fn first_violation(design: Design, s: &State, actions: &[Action]) -> Option<Prop> {
    let cfg = Config {
        design,
        faults: faults::ALL,
        same_state: false,
    };
    let mut s = s.clone();
    for &a in actions {
        let out = exec(&s, a, &cfg)
            .unwrap()
            .unwrap_or_else(|| panic!("{a:?} disabled under {design:?}"));
        if let Some(p) = props::check_step(&s, &out, 0).or_else(|| props::check_state(&out.next, 0))
        {
            return Some(p);
        }
        s = out.next;
    }
    None
}

// ------------------------------------------------------------------ H.22

/// R0 is retiring, the holder consumed a new epoch's acceptance (H6), and its
/// fresh-epoch status is on the wire.
fn status_first() -> Fake {
    let mut f = Fake::new();
    f.boot();
    f.list();
    f.step(AuthRetire(0));
    f.list();
    f.run(&[AuthBackup, AuthRestore, FetchStart, ListDeliver]);
    assert_eq!(f.s.vol.alert_epoch, Some(1));
    f.step(OpAcceptEpoch);
    f.boot();
    f.list();
    assert!(status_gate_shut(&f.s, D));
    assert!(!f.enabled(Enqueue(0)), "obligation reports wait (H.22.1)");
    f.request(2);
    f
}

/// H.22.2: every authority-side refusal of the status is transient or halts
/// every route; only a bug halt releases the gate (H.22.3).
#[test]
fn h22_transient_status_answers_keep_the_gate_and_a_bug_halt_releases_it() {
    for (kind, shut) in [
        (RespKind::Transient, true), // 429, 503 and network failure
        (RespKind::Unauthorized { replay: true }, true),
        (RespKind::ProofExpired, true),
        (RespKind::StaleListRef, true), // relist, then send again
        (RespKind::EpochMismatch, true),
        (RespKind::Bug, false),
    ] {
        let mut f = status_first();
        f.step(AuthProcess { c: 2, idx: 0 });
        f.s.net.resps[2].map_in_place(|r| {
            r.kind = kind;
            r.healthy = kind != RespKind::Bug;
        });
        f.step(Deliver { c: 2, idx: 0 });
        assert_eq!(status_gate_shut(&f.s, D), shut, "{kind:?}");
        assert_eq!(f.enabled(Enqueue(0)), !shut, "{kind:?}");
        if !shut {
            // The halt keeps its operator resume, and survives a restart.
            assert_eq!(f.s.dur.slots[2].unwrap().st, DurSt::Halted);
            f.step(Crash);
            f.boot();
            f.list();
            assert!(!status_gate_shut(&f.s, D));
            assert!(f.enabled(OpClearHalt(2)));
        }
    }
    // 401 invalid_holder_proof and 403 halt every route (M10) anyway.
    for kind in [
        RespKind::Unauthorized { replay: false },
        RespKind::Forbidden,
    ] {
        let mut f = status_first();
        f.step(AuthProcess { c: 2, idx: 0 });
        f.s.net.resps[2].map_in_place(|r| {
            r.kind = kind;
            r.healthy = false;
        });
        f.step(Deliver { c: 2, idx: 0 });
        assert_ne!(f.s.vol.route, Route::Normal, "{kind:?}");
        assert!(!f.enabled(FetchStart) && !f.enabled(Transmit(0)));
    }
}

/// H.22.3: "a status serialization bug must not also stop purge receipts".
/// With the operator's status resume (N5) removed, the retirement still
/// completes; without the release it never does. With every action, L1
/// holds with the gate.
#[test]
fn h22_purge_receipts_never_wait_for_a_halted_status() {
    let mut f = status_first();
    f.step(AuthFault {
        c: 2,
        idx: 0,
        code: FaultCode::Bug,
    });
    assert!(f.rows(Deliver { c: 2, idx: 0 }).contains(&RowId::T6c));
    f.s.budget = NONE;
    let p = Profile {
        liveness: true,
        ..profile("h22-no-status-resume")
    };
    let no_resume: Vec<Action> = Action::candidates()
        .into_iter()
        .filter(|a| *a != OpClearHalt(2))
        .collect();
    let goal = props::retirements_done;
    let with = check_from_with_goal(&p, f.s.clone(), no_resume.clone(), goal);
    assert!(with.violation.is_none(), "{}", with.render());
    let without = check_from_with_goal(
        &Profile {
            design: D.without(Fix::StatusGateRelease),
            ..p
        },
        f.s.clone(),
        no_resume,
        goal,
    );
    assert_eq!(
        without.violation.as_ref().map(|v| v.prop),
        Some(Prop::L1Liveness),
        "{}",
        without.render()
    );
    let full = check_from(&Profile { skip: 0, ..p }, f.s, Action::candidates());
    assert!(full.violation.is_none(), "{}", full.render());
}

/// H.22.2 amends H.4.2: the status `list_ref` floor is the last accepted
/// status of the current epoch only, so an earlier epoch's floor never makes
/// a tag-valid current-epoch status stale.
#[test]
fn h22_status_floor_restarts_every_epoch() {
    let mut f = status_first();
    let sent = f.s.dur.slots[2].unwrap().key.status().unwrap();
    f.s.auth.core.status = Some(SKey {
        list_ref: Rev {
            epoch: 0,
            seq: u8::MAX,
        },
        ..sent
    });
    assert!(matches!(f.reply(2), RespKind::Accepted { epoch: 1, .. }));
    assert!(!status_gate_shut(&f.s, D));
}

// ------------------------------------------------------------------ H.23

/// The engine clock lags 40 s; a verified page proved it, so
/// `clock_behind_authority` is raised and written to holder state (H.26).
fn clock_raised() -> Fake {
    let mut f = Fake::new();
    f.s.env.probe = Probe::Exposure;
    f.s.env.capability_skew = 30;
    f.s.env.lag = 40;
    f.boot();
    f.run(&[FetchStart]);
    assert!(
        f.rows(VerifyListPage { iat: 40, rtt: None })
            .contains(&RowId::C19)
    );
    assert!(f.rows(PersistClockHold).contains(&RowId::C19));
    assert!(f.s.dur.clock_behind);
    f.step(ListDeliver);
    while f.s.vol.apply.is_some() {
        f.step(Apply);
    }
    assert!(clock_held(&f.s) && f.s.ghost.lag_proven);
    f
}

const ASK: Action = StartCapability {
    room: 0,
    iat: 0,
    exp: 60,
};

/// H.23 "what doesn't clear it": a capability, a page without this
/// process's nonce, an older or replayed timestamp, a page whose round trip
/// leaves the lag unbounded, an NTP or clock step, elapsed time and an
/// operator command. Under the rejected reading each one clears the hold
/// while the lag is still above 30 s, and the next capability is answered
/// (I1).
#[test]
fn h23_nothing_but_a_confirmed_correction_clears_the_hold() {
    let f = clock_raised();
    let probe = |page: Action| vec![ClockProbe, page, ASK];
    let sources: [(&str, Vec<Action>); 7] = [
        ("a capability", vec![ASK]),
        (
            "a page without this process's nonce",
            probe(VerifyListPage { iat: 0, rtt: None }),
        ),
        (
            "an older or replayed page",
            probe(VerifyListPage {
                iat: -50,
                rtt: None,
            }),
        ),
        (
            "a nonce-bound page whose round trip leaves 40 s of lag possible",
            probe(VerifyListPage {
                iat: 25,
                rtt: Some(15),
            }),
        ),
        ("a clock step", vec![ClockStep { lag: 35 }, ASK]),
        ("elapsed time", vec![ProbeWindow, ASK]),
        ("an operator command", vec![OpClearClock, ASK]),
    ];
    let s = f.s.clone();
    for (what, actions) in &sources {
        assert_eq!(first_violation(D, &s, actions), None, "{what}");
        assert_eq!(
            first_violation(D.without(Fix::ConfirmedClockCorrection), &s, actions),
            Some(Prop::I1CapabilityExposure),
            "{what}"
        );
    }
    let mut every: Vec<Action> = sources.iter().flat_map(|(_, a)| a.clone()).collect();
    every.sort();
    every.dedup();
    ablate(
        Fix::ConfirmedClockCorrection,
        Prop::I1CapabilityExposure,
        s,
        &every,
    );

    // A restart is no correction either: the hold is in holder state, and a
    // page can only answer a request of the running process.
    let mut f = clock_raised();
    f.step(ClockProbe);
    f.step(Crash);
    f.boot();
    assert!(clock_held(&f.s));
    assert!(!f.enabled(VerifyListPage {
        iat: 30,
        rtt: Some(10)
    }));
    let out = f.outcome(ASK).unwrap();
    assert!(out.obs.contains(&Some(Obs::CapabilityRefused(
        CapabilityRefusal::ClockBehind
    ))));
}

/// The confirmation, the probe and the lockout: with clocks that agree and a
/// round trip under 30 s the probe confirms; otherwise the hold stays and
/// alerts with the measured bound and `rtt`. Lists continue while it holds.
#[test]
fn h23_the_probe_confirms_agreeing_clocks_and_alerts_otherwise() {
    let mut f = clock_raised();
    // Still 40 s behind: no consistent page can confirm.
    f.step(ClockProbe);
    assert!(!f.enabled(ClockProbe), "one probe at a time");
    let out = f.step(VerifyListPage {
        iat: 30,
        rtt: Some(10),
    });
    assert!(
        out.obs
            .contains(&Some(Obs::ClockLagAlert { bound: 40, rtt: 10 }))
    );
    assert!(clock_held(&f.s));
    assert!(!f.enabled(ClockProbe), "at most one probe per 60 s");
    // The clock is fixed, but the round trip is 31 s.
    f.run(&[ClockStep { lag: 0 }, ProbeWindow, ClockProbe]);
    let out = f.step(VerifyListPage {
        iat: 0,
        rtt: Some(31),
    });
    assert!(
        out.obs
            .contains(&Some(Obs::ClockLagAlert { bound: 31, rtt: 31 }))
    );
    assert!(clock_held(&f.s));
    // Lists, applies and the probe's own page continue while it holds; the
    // probe's result is not applied.
    f.step(AuthUnlist(1));
    f.list();
    assert_eq!(f.s.vol.applied.unwrap().items[1], Item::Absent);
    // Agreeing clocks and a 29 s round trip: confirmed (C20).
    f.run(&[ProbeWindow, ClockProbe]);
    let admitted = f.s.vol.admitted;
    assert!(
        f.rows(VerifyListPage {
            iat: 0,
            rtt: Some(29),
        })
        .contains(&RowId::C20)
    );
    assert!(!clock_held(&f.s));
    assert_eq!(f.s.vol.admitted, admitted, "the probe applies nothing");
    assert!(f.step(ASK).next.vol.capability.is_some());
}

// ------------------------------------------------------------------ H.24

fn pins_running() -> Fake {
    let mut f = Fake::new();
    f.s.env.probe = Probe::Pin;
    f.boot();
    f.list();
    assert!(servable(&f.s, 0) && servable(&f.s, 1));
    f
}

/// H.24: at process start the baseline is empty, so every pin of the first
/// list is changed and serves only after A4; whether artifacts were derived
/// under the pin is read from the durable record, so a pin that changed
/// while the holder was down serves no old-ref artifact until a rebuild.
#[test]
fn h24_restart_baseline_is_empty_and_the_durable_record_gates_artifacts() {
    let mut f = pins_running();
    f.step(Crash);
    f.step(RepointRoot { room: 0, dir: 2 });
    f.step(AttestRoot {
        room: 0,
        dir: Some(2),
        stale: false,
    });
    let down = f.s.clone();
    f.boot();
    f.step(FetchStart);
    assert!(f.rows(ListDeliver).contains(&RowId::P1));
    assert_eq!(
        f.s.vol.block, 0b11,
        "both pins of the first list are changed"
    );
    while f.s.vol.apply.is_some() {
        assert!(!servable(&f.s, 0) && !servable(&f.s, 1));
        f.step(Apply);
    }
    assert!(servable(&f.s, 1), "an unchanged pin serves after A4");
    assert!(!servable(&f.s, 0), "old-ref artifacts never serve (R17)");
    assert_eq!(f.s.dur.derived[0], Some(1));
    f.step(PinRebuild(0));
    assert!(servable(&f.s, 0));
    ablate(
        Fix::DurablePinDerivation,
        Prop::I1RootPin,
        down,
        &[Boot, Markers, Narrow, FetchStart, ListDeliver, Apply],
    );
}

// ------------------------------------------------------------------ H.25

/// The holder serves its epoch-0 list; a restore began at t = 0 (H19 confirmed
/// the old authority stopped) and its journal failed verification (H12j), so
/// minting stopped.
fn minting_stopped() -> Fake {
    let mut f = Fake::new();
    f.s.env.probe = Probe::EpochMint;
    f.s.env.capability_skew = 30;
    f.boot();
    f.list();
    f.step(AuthBackup);
    f.s.env.bad_journal = true;
    f.run(&[RestoreBegin, AuthRestore]);
    assert_eq!(f.s.ghost.mint_stop, Some(0));
    assert!(!mintable(&f.s, 0));
    f
}

fn epoch_candidates() -> Vec<Action> {
    let mut c = vec![
        OpRestoreJournalLost,
        FinishCapability,
        FetchStart,
        ListDeliver,
        Apply,
        OpAcceptEpoch,
        Boot,
        Markers,
        Narrow,
        Enqueue(2),
        Transmit(2),
        Handoff(2),
        Dispatch(2),
        AuthProcess { c: 2, idx: 0 },
        Deliver { c: 2, idx: 0 },
        // H.30.2(b): time passes the lease timeout (fair).
        LeaseTimeout,
    ];
    for t in [60, 90, 91, 150] {
        c.push(TickExposure(t));
    }
    for iat in [0, 60, 90, 91, 150] {
        c.push(StartCapability {
            room: 0,
            iat,
            exp: iat + 60,
        });
    }
    c
}

/// H.25 bound: a holder still on its earlier-epoch list serves only under
/// capabilities minted before minting stopped, so at most 90 s after the
/// stop, whatever remains of its lease. Without the mint gate, R0's pin is
/// nulled by `restore_journal_lost` and still serves past 90 s. With it,
/// L1 also proves search recovers: after `restore_journal_lost` minting waits
/// for the H.30.4 id-space-wide lease timeout as well as the holder's report.
#[test]
fn h25_minting_waits_for_every_holder_and_exposure_stays_within_90_s() {
    let f = minting_stopped();
    let c = epoch_candidates();
    ablate(Fix::EpochMintGate, Prop::I1EpochExposure, f.s.clone(), &c);
    let live = check_from(
        &Profile {
            liveness: true,
            ..profile("h25-search-recovers")
        },
        f.s.clone(),
        c,
    );
    assert!(live.violation.is_none(), "{}", live.render());

    // The counterexample, scripted: the nulled pin serves at t = 150.
    let late = StartCapability {
        room: 0,
        iat: 150,
        exp: 210,
    };
    let script = [
        OpRestoreJournalLost,
        TickExposure(150),
        late,
        FinishCapability,
    ];
    assert_eq!(
        first_violation(D.without(Fix::EpochMintGate), &f.s, &script),
        Some(Prop::I1EpochExposure)
    );

    // The bound itself: a capability minted at the stop answers until 90 s.
    let mut g = Fake {
        s: f.s.clone(),
        ..Fake::new()
    };
    g.run(&[OpRestoreJournalLost, ASK, TickExposure(90)]);
    assert_eq!(g.s.auth.core.pins[0], None, "the new epoch nulled R0's pin");
    assert!(g.rows(FinishCapability).contains(&RowId::C17));
    g.run(&[ASK, TickExposure(91)]);
    let out = g.step(FinishCapability);
    assert!(
        out.obs
            .contains(&Some(Obs::CapabilityRefused(CapabilityRefusal::Expired)))
    );
}

/// H.25 cost: search for the id-space stays down from the new epoch until
/// the last holder has accepted it and reported; then minting resumes. A
/// supported restore closes the gate too. After `restore_journal_lost`,
/// minting also waits for the id-space-wide lease timeout (H.30.4).
#[test]
fn h25_search_is_down_until_the_last_holder_reports() {
    for lost in [true, false] {
        let mut f = Fake::new();
        f.s.env.probe = Probe::EpochMint;
        f.boot();
        f.list();
        f.step(AuthBackup);
        if lost {
            f.s.env.bad_journal = true;
            f.run(&[RestoreBegin, AuthRestore, OpRestoreJournalLost]);
        } else {
            f.step(RestoreBegin);
            assert!(f.rows(AuthRestore).contains(&RowId::H14));
        }
        assert!(!mintable(&f.s, 0) && !mintable(&f.s, 1), "minting is down");
        f.run(&[FetchStart, ListDeliver, OpAcceptEpoch]);
        f.boot();
        f.list();
        assert_eq!(f.s.dur.hw.unwrap().epoch, 1);
        assert!(!mintable(&f.s, 0), "accepting the epoch is not reporting");
        f.request(2);
        f.step(AuthProcess { c: 2, idx: 0 });
        if lost {
            assert!(!mintable(&f.s, 0), "the id-space-wide timeout is pending");
            assert!(f.rows(LeaseTimeout).contains(&RowId::H15));
        }
        assert!(
            mintable(&f.s, 0) && mintable(&f.s, 1),
            "the last holder reported"
        );
        assert!(!f.s.ghost.search_due);
        assert_eq!(servable(&f.s, 0), !lost, "a nulled pin serves nothing");
    }
}
