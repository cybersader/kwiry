//! HOLDINGS-WIRE §15 H.26–H.31 (design revision 11.2). Each load-bearing
//! rule is a model fix; removing it reproduces the counterexample it closes,
//! and the same exploration holds with it.

use kwiry_lifecycle::action::{Action, Action::*, FaultCode, faults};
use kwiry_lifecycle::checker::{Profile, Report, check_from, check_from_with_goal};
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
const D: Design = Design::REV11_2;

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
        max_states: 2_000_000,
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
}

/// With the fix the exploration holds; without it, `expected` is the first
/// violation found.
fn ablate(fix: Fix, expected: Prop, p: Profile, s: State, actions: &[Action]) -> (Report, Report) {
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
    eprintln!("{}{}", with.render(), without.render());
    (with, without)
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

// ------------------------------------------------------------------ H.26

const ASK: Action = StartCapability {
    room: 0,
    iat: 0,
    exp: 60,
};
/// A capability whose `iat` is 40 s ahead of the engine clock: proof of lag.
const AHEAD: Action = StartCapability {
    room: 1,
    iat: 40,
    exp: 100,
};

/// The engine clock lags 40 s, nothing has proved it yet, and both rooms
/// serve.
fn lagging() -> Fake {
    let mut f = Fake::new();
    f.s.env.probe = Probe::Exposure;
    f.s.env.capability_skew = 30;
    f.s.env.lag = 40;
    f.boot();
    f.list();
    assert!(!clock_held(&f.s) && servable(&f.s, 0));
    f
}

/// H.26 "Raising": the refusal takes effect in memory immediately, before
/// the durable write. Under the reading that the refusal waits for the
/// write, a capability admitted in between is answered while the lag is
/// proven (I1).
#[test]
fn h26_the_refusal_takes_effect_before_its_durable_write() {
    let f = lagging();
    let mut g = Fake {
        s: f.s.clone(),
        ..Fake::new()
    };
    let out = g.step(AHEAD);
    assert!(out.rows.iter().any(|r| r == RowId::C19));
    assert!(g.s.vol.clock_unpersisted && !g.s.dur.clock_behind);
    let refused = g.outcome(ASK).unwrap();
    assert!(refused.obs.contains(&Some(Obs::CapabilityRefused(
        CapabilityRefusal::ClockBehind
    ))));
    assert!(g.rows(PersistClockHold).contains(&RowId::C19));
    assert!(g.s.dur.clock_behind && clock_held(&g.s));
    ablate(
        Fix::ClockHoldMemoryFirst,
        Prop::I1CapabilityExposure,
        profile("h26-memory-first"),
        f.s,
        &[AHEAD, ASK, PersistClockHold, FinishCapability],
    );
}

/// H.26: the flag is holder state, so a restart (no confirmed correction)
/// keeps it. Under the reading that it lives only in memory, the restarted
/// engine answers while the lag is proven.
#[test]
fn h26_the_hold_survives_a_restart() {
    let mut f = lagging();
    f.step(AHEAD);
    let raised = f.s.clone();
    f.step(PersistClockHold);
    f.run(&[Crash, Boot, Markers, Narrow]);
    assert!(clock_held(&f.s), "a restart is no correction");
    f.list();
    let out = f.outcome(ASK).unwrap();
    assert!(out.obs.contains(&Some(Obs::CapabilityRefused(
        CapabilityRefusal::ClockBehind
    ))));
    ablate(
        Fix::DurableClockHold,
        Prop::I1CapabilityExposure,
        profile("h26-durable"),
        raised,
        &[
            PersistClockHold,
            Crash,
            Boot,
            Markers,
            Narrow,
            FetchStart,
            ListDeliver,
            Apply,
            ASK,
            FinishCapability,
        ],
    );
}

/// H.26 residual and "No lockout from persistence": a crash before the
/// write loses only the flag, and the next process's list is measured
/// again; with clocks that agree, the first nonce-bound page of every start
/// clears a persisted flag. Holder-state loss and trust on first use start
/// with it clear.
#[test]
fn h26_crash_before_the_write_and_no_lockout_from_persistence() {
    // Crash between C19's in-memory raise and its W.
    let mut f = lagging();
    f.step(AHEAD);
    f.run(&[Crash, Boot, Markers, Narrow]);
    assert!(!clock_held(&f.s) && !f.s.ghost.lag_proven);
    f.step(FetchStart);
    // The startup list's own page (nonce-bound, 5 s round trip) shows the
    // lag again: the hold is raised again.
    assert!(
        f.rows(VerifyListPage {
            iat: 40,
            rtt: Some(5)
        })
        .contains(&RowId::C19)
    );
    assert!(clock_held(&f.s));

    // A persisted flag and a clock fixed while the engine was down: the
    // first page of the next start confirms the correction (C20).
    let mut f = lagging();
    f.run(&[AHEAD, PersistClockHold, Crash, ClockStep { lag: 0 }]);
    f.boot();
    f.step(FetchStart);
    assert!(
        f.rows(VerifyListPage {
            iat: 0,
            rtt: Some(5)
        })
        .contains(&RowId::C20)
    );
    assert!(!clock_held(&f.s) && !f.s.dur.clock_behind);

    // Trust on first use (S4): a fresh holder state has the flag clear.
    let mut f = Fake::new();
    f.s.dur.clock_behind = true;
    f.s.dur.state_holder = None;
    f.s.dur.root_holder = None;
    f.s.dur.units = [UDur::Deleted; UNITS];
    assert!(f.rows(Boot).contains(&RowId::S4));
    assert!(!f.s.dur.clock_behind);
    // Holder-state loss.
    let mut f = Fake::new();
    f.s.env.probe = Probe::Root;
    f.s.dur.clock_behind = true;
    f.step(LoseHolderState);
    assert!(!f.s.dur.clock_behind);
}

// ------------------------------------------------------------------ H.27

/// R0 is retiring, the holder consumed a new epoch's acceptance (H6), and its
/// fresh-epoch status is on the wire.
fn status_first() -> Fake {
    let mut f = Fake::new();
    f.boot();
    f.list();
    f.step(AuthRetire(0));
    f.list();
    f.run(&[AuthBackup, AuthRestore, FetchStart, ListDeliver]);
    f.step(OpAcceptEpoch);
    f.boot();
    f.list();
    assert!(status_gate_shut(&f.s, D));
    f.request(2);
    f
}

/// The status bug-halted (T6c): the gate is open.
fn status_halted() -> Fake {
    let mut f = status_first();
    f.step(AuthFault {
        c: 2,
        idx: 0,
        code: FaultCode::Bug,
    });
    assert!(f.rows(Deliver { c: 2, idx: 0 }).contains(&RowId::T6c));
    assert!(!status_gate_shut(&f.s, D));
    f
}

/// H.27: the gate is read from durable holder state. N5 moves the status
/// from `halted` to `invalidated`, which closes the gate until the
/// re-derived status has a current-epoch ack. Reports already sent while it
/// was open stand; a report derived but never sent waits.
#[test]
fn h27_the_gate_is_derived_from_durable_state() {
    let mut f = status_halted();
    // Open: R0's report is derived and sent.
    f.request(0);
    let sent = f.s.dur.slots[0].unwrap();
    assert_eq!(sent.st, DurSt::Armed);
    f.step(OpClearHalt(2));
    assert_eq!(f.s.dur.slots[2].unwrap().st, DurSt::Invalidated);
    f.boot();
    f.list();
    assert!(status_gate_shut(&f.s, D), "N5 closed the gate again");
    assert!(!f.enabled(Enqueue(0)));
    // The report sent while the gate was open stands: it is retried.
    assert_eq!(f.s.dur.slots[0].unwrap().key, sent.key);
    assert!(f.enabled(Transmit(0)), "a sent report is retried");
    // The re-derived status reopens it.
    f.request(2);
    f.step(AuthProcess { c: 2, idx: 0 });
    f.step(Deliver { c: 2, idx: 0 });
    assert!(!status_gate_shut(&f.s, D));

    // A report derived while the gate was open but never sent waits.
    let mut f = status_halted();
    f.step(Enqueue(0));
    assert_eq!(f.s.dur.slots[0].unwrap().st, DurSt::Ready);
    f.step(OpClearHalt(2));
    f.boot();
    f.list();
    assert_eq!(f.s.dur.slots[0].unwrap().st, DurSt::Ready);
    assert!(status_gate_shut(&f.s, D));
    assert!(!f.enabled(Transmit(0)), "the shut gate holds it back");
}

/// H.27: a durable `halted` keeps the gate open across a restart. Under the
/// reading that the release is an event of the process, the restart shuts
/// it again, and with the operator's status resume removed the retirement
/// never completes (L1).
#[test]
fn h27_a_restart_keeps_the_gate_open_while_the_status_is_halted() {
    let mut f = status_halted();
    f.step(Crash);
    let crashed = f.s.clone();
    f.boot();
    f.list();
    assert!(!status_gate_shut(&f.s, D));
    let mut s = crashed;
    s.budget = NONE;
    let p = Profile {
        liveness: true,
        ..profile("h27-no-status-resume")
    };
    let no_resume: Vec<Action> = Action::candidates()
        .into_iter()
        .filter(|a| *a != OpClearHalt(2))
        .collect();
    let goal = props::retirements_done;
    let with = check_from_with_goal(&p, s.clone(), no_resume.clone(), goal);
    assert!(with.violation.is_none(), "{}", with.render());
    let without = check_from_with_goal(
        &Profile {
            design: D.without(Fix::DurableStatusGate),
            ..p
        },
        s,
        no_resume,
        goal,
    );
    assert_eq!(
        without.violation.as_ref().map(|v| v.prop),
        Some(Prop::L1Liveness),
        "{}",
        without.render()
    );
}

// ------------------------------------------------------------------ H.28

fn pins_running() -> Fake {
    let mut f = Fake::new();
    f.s.env.probe = Probe::Pin;
    f.boot();
    f.list();
    assert!(servable(&f.s, 0) && servable(&f.s, 1));
    f
}

/// H.28: H10.5 bounded stale service covers only artifacts published under
/// the current `pin_ref`. R0 is re-attested at another directory while its
/// root still resolves to the old one: A0 refuses the change, and after A4
/// R0's artifacts (stamped with the earlier `pin_ref`) never serve, not even
/// inside the stale bound. Under the reading that the bound covers every
/// previously validated artifact, they serve (I1).
#[test]
fn h28_old_ref_artifacts_never_serve() {
    let mut f = pins_running();
    f.step(AttestRoot {
        room: 0,
        dir: Some(2),
        stale: false,
    });
    let attested = f.s.clone();
    f.step(FetchStart);
    assert!(f.rows(ListDeliver).contains(&RowId::P1));
    while f.s.vol.apply.is_some() {
        f.step(Apply);
    }
    assert!(!f.s.env.stale_expired);
    assert!(!servable(&f.s, 0), "old-ref artifacts never serve");
    assert!(servable(&f.s, 1));
    ablate(
        Fix::SameRefStaleService,
        Prop::I1RootPin,
        profile("h28-same-ref"),
        attested,
        &[FetchStart, ListDeliver, Apply],
    );

    // Same `pin_ref`, the observation no longer matches: same-ref
    // artifacts serve until the bound, then R17.
    let mut f = pins_running();
    f.step(RepointRoot { room: 0, dir: 2 });
    assert!(servable(&f.s, 0));
    assert!(f.rows(StaleDeadline).contains(&RowId::P5));
    assert!(!servable(&f.s, 0));
}

// ------------------------------------------------------------------ H.29

/// The hold is raised and persisted; then the engine clock was stepped to 20
/// s ahead of the authority, and a clock probe is outstanding. The network
/// round trip is 40 s.
fn ahead_slow_network() -> Fake {
    let mut f = lagging();
    f.run(&[AHEAD, PersistClockHold, ClockStep { lag: -20 }, ClockProbe]);
    assert!(clock_held(&f.s) && !f.s.ghost.lag_proven);
    f
}

/// Pages a 40 s round trip allows for an engine 20 s ahead: `iat − now_recv`
/// is between −60 and −20, so `(iat − now_recv) + rtt` is between −20 and 20.
fn slow_pages() -> Vec<Action> {
    [-60, -40, -20]
        .into_iter()
        .map(|iat| VerifyListPage { iat, rtt: Some(40) })
        .collect()
}

/// H.29: the inequality alone decides, whatever `rtt` is, and an engine
/// clock running ahead is safe. A separate `rtt > 30 s` refusal never
/// clears the hold of an engine that is ahead on a slow network (an outage:
/// the hold must eventually clear, L1 with that goal), while the inequality
/// clears it and nothing is over-served (I1).
#[test]
fn h29_the_inequality_decides_whatever_the_round_trip() {
    let f = ahead_slow_network();
    let mut g = Fake {
        s: f.s.clone(),
        ..Fake::new()
    };
    // `(iat − now_recv) + rtt = −20 + 40 = 20 ≤ 30` although `rtt > 30 s`.
    let out = g.step(VerifyListPage {
        iat: -20,
        rtt: Some(40),
    });
    assert!(out.rows.iter().any(|r| r == RowId::C20));
    assert!(!clock_held(&g.s));
    let p = Profile {
        liveness: true,
        ..profile("h29-slow-network")
    };
    let goal: fn(&State) -> bool = |s| !clock_held(s);
    let with = check_from_with_goal(&p, f.s.clone(), slow_pages(), goal);
    assert!(with.violation.is_none(), "{}", with.render());
    let without = check_from_with_goal(
        &Profile {
            design: D.without(Fix::InequalityDecidesClear),
            ..p
        },
        f.s.clone(),
        slow_pages(),
        goal,
    );
    assert_eq!(
        without.violation.as_ref().map(|v| v.prop),
        Some(Prop::L1Liveness),
        "{}",
        without.render()
    );
    // Safety: whether the engine is ahead or behind, with any round trip,
    // the inequality never clears a hold while more than 30 s of lag is
    // possible, and capabilities answer only within their bound.
    let mut c = slow_pages();
    c.extend([
        ClockStep { lag: 40 },
        ClockStep { lag: -20 },
        ClockProbe,
        ProbeWindow,
        ASK,
        FinishCapability,
    ]);
    for iat in [0, 10, 40] {
        for rtt in [5, 40] {
            c.push(VerifyListPage {
                iat,
                rtt: Some(rtt),
            });
        }
    }
    let safe = check_from(&profile("h29-safety"), f.s, c);
    assert!(safe.violation.is_none(), "{}", safe.render());
}

// ------------------------------------------------------------- H.30, H.31

/// The modeled holder serves its epoch-0 list; two other engine holders of
/// the id-space are not enrolled yet. Minting is open.
fn two_holders(bad_journal: bool, symmetry: bool) -> State {
    let mut f = Fake::new();
    f.s.env.probe = Probe::EpochMint;
    f.s.env.capability_skew = 30;
    f.s.env.peer_symmetry = symmetry;
    f.boot();
    f.list();
    f.s.env.bad_journal = bad_journal;
    f.s.budget = Budget {
        restores: 1,
        ..NONE
    };
    f.s
}

/// Every action of the H.30 fixture: the authority's backup and restore, the
/// modeled holder's epoch acceptance and fresh status, the other holders'
/// roster changes, epoch moves, reports, silence and answers, the clock, the
/// lease timeout and the refused override.
fn gate_candidates() -> Vec<Action> {
    let mut c = vec![
        AuthBackup,
        AuthRestore,
        OpRestoreJournalLost,
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
        LeaseTimeout,
        OpMintOverride,
        FinishCapability,
    ];
    for p in 0..PEERS as u8 {
        c.extend([
            PeerEnroll(p),
            PeerDisable(p),
            PeerEnable(p),
            PeerRotate(p),
            PeerMove(p),
            PeerReport(p),
            PeerSilent(p),
            PeerAnswer(p),
        ]);
    }
    for t in [60, 91] {
        c.push(TickExposure(t));
    }
    for iat in [0, 91] {
        c.push(StartCapability {
            room: 0,
            iat,
            exp: iat + 60,
        });
    }
    c
}

fn gate_profile(name: &'static str) -> Profile {
    Profile {
        liveness: true,
        ..profile(name)
    }
}

/// H.30 and H.31, exhaustively over the fixture: after a supported restore
/// and after `restore_journal_lost`, nothing is answered from an
/// earlier-epoch list more than 90 s after minting stopped (I1), and minting
/// resumes, through member reports or the lease timeout, including when a
/// member never reports (L1).
#[test]
fn h30_gate_holds_exposure_and_always_resumes() {
    for lost in [false, true] {
        let r = check_from(
            &gate_profile(if lost {
                "h30-journal-lost"
            } else {
                "h30-supported"
            }),
            two_holders(lost, true),
            gate_candidates(),
        );
        eprintln!("{}", r.render());
        assert!(r.violation.is_none(), "{}", r.render());
        for row in [RowId::H14, RowId::H15, RowId::H16] {
            assert!(r.rows_seen[row.index()], "{}", row.name());
        }
        if !lost {
            assert!(r.rows_seen[RowId::H17.index()]);
        }
    }
}

/// The peer-symmetry reduction keeps the verdicts and the rows seen. Sorting
/// can relabel peers along fair edges, so L1 under per-index fairness is
/// compared with the unreduced graph too: with every fix, without the lease
/// timeout (a fair cycle) and for the safety ablations.
#[test]
fn h30_peer_symmetry_preserves_verdicts() {
    for (lost, fix) in [
        (false, None),
        (true, None),
        (false, Some(Fix::MintLeaseTimeout)),
        (false, Some(Fix::RosterJournal)),
        (true, Some(Fix::JournalLostMintWait)),
    ] {
        let p = Profile {
            design: fix.map_or(D, |x| D.without(x)),
            ..gate_profile("h30-symmetry")
        };
        let full = check_from(&p, two_holders(lost, false), gate_candidates());
        let reduced = check_from(&p, two_holders(lost, true), gate_candidates());
        eprintln!("{}{}", full.render(), reduced.render());
        assert_eq!(
            full.violation.as_ref().map(|v| v.prop),
            reduced.violation.as_ref().map(|v| v.prop)
        );
        if full.violation.is_none() {
            assert_eq!(full.rows_seen, reduced.rows_seen);
            assert!(reduced.states < full.states);
        }
    }
}

/// H.30.1: membership is frozen at the new epoch's first transaction. Under
/// the reading that "every enrolled, enabled holder" is read live,
/// disabling a member that never reports reopens minting while it still
/// serves its earlier-epoch list (I1).
#[test]
fn h30_frozen_membership_is_necessary() {
    ablate(
        Fix::FrozenMintMembership,
        Prop::I1EpochExposure,
        gate_profile("h30-frozen"),
        two_holders(false, true),
        &gate_candidates(),
    );
}

/// A supported restore with the modeled holder as the only member; then a
/// holder is enrolled in the new epoch and never reports.
fn late_enrollee() -> Fake {
    let mut f = Fake {
        s: two_holders(false, true),
        ..Fake::new()
    };
    assert!(f.rows(AuthBackup).is_empty());
    assert!(f.rows(AuthRestore).contains(&RowId::H14));
    f.run(&[PeerEnroll(0), PeerSilent(0)]);
    assert_eq!(f.s.peers[0].seat, Seat::Absent, "enrolled later: no member");
    f
}

/// H.30.1: a holder enrolled later in the epoch can never have held an
/// earlier-epoch list and can't stall minting. Before the lease timeout,
/// the modeled holder's report resumes minting; under the live reading the
/// silent later enrollee keeps it closed (L1 with the timeout withheld).
#[test]
fn h30_a_later_enrollee_cannot_stall_minting() {
    let f = late_enrollee();
    let c: Vec<Action> = gate_candidates()
        .into_iter()
        .filter(|a| *a != LeaseTimeout)
        .collect();
    let p = gate_profile("h30-late-enrollee");
    let with = check_from(&p, f.s.clone(), c.clone());
    assert!(with.violation.is_none(), "{}", with.render());
    let without = check_from(
        &Profile {
            design: D.without(Fix::FrozenMintMembership),
            ..p
        },
        f.s.clone(),
        c,
    );
    assert_eq!(
        without.violation.as_ref().map(|v| v.prop),
        Some(Prop::L1Liveness),
        "{}",
        without.render()
    );

    // Scripted: the modeled holder reports, and minting resumes at once.
    let mut f = late_enrollee();
    f.run(&[FetchStart, ListDeliver, OpAcceptEpoch]);
    f.boot();
    f.list();
    f.request(2);
    assert!(f.rows(AuthProcess { c: 2, idx: 0 }).contains(&RowId::H15));
    assert!(mintable(&f.s, 0));
}

/// H.30.2(b) and H.30.3: without the lease timeout, a member that never
/// reports blocks minting forever (L1). With it, the member clears at
/// `T_stop + serve_seconds_ceiling + 150 s` with the
/// `cleared_by_lease` event and the H.11 alert, and the earlier-epoch lists
/// have lapsed by then.
#[test]
fn h30_the_lease_timeout_clears_a_member_that_never_reports() {
    ablate(
        Fix::MintLeaseTimeout,
        Prop::L1Liveness,
        gate_profile("h30-timeout"),
        two_holders(false, true),
        &gate_candidates(),
    );
    let mut f = Fake {
        s: two_holders(false, true),
        ..Fake::new()
    };
    f.run(&[PeerEnroll(0), AuthBackup, AuthRestore, PeerSilent(0)]);
    assert_eq!(f.s.peers[0].seat, Seat::Pending);
    assert_eq!(f.s.peers[0].lease, Some(0));
    f.run(&[FetchStart, ListDeliver, OpAcceptEpoch]);
    f.boot();
    f.list();
    f.request(2);
    f.step(AuthProcess { c: 2, idx: 0 });
    assert!(!mintable(&f.s, 0), "the silent member is pending");
    let out = f.step(LeaseTimeout);
    assert!(out.rows.iter().any(|r| r == RowId::H16));
    assert!(out.obs.contains(&Some(Obs::MintGateClearedByLease {
        holder: false,
        peers: 1
    })));
    assert!(f.s.env.exposure_elapsed > 1050);
    assert_eq!(f.s.peers[0].lease, None, "its earlier-epoch lease lapsed");
    assert!(mintable(&f.s, 0));
}

/// H.30.3: there is no admin override. Under the reading that an admin can
/// clear a member early, minting reopens while a member still serves its
/// earlier-epoch list (I1).
#[test]
fn h30_no_admin_override() {
    ablate(
        Fix::NoMintOverride,
        Prop::I1EpochExposure,
        gate_profile("h30-override"),
        two_holders(false, true),
        &gate_candidates(),
    );
    let mut f = Fake {
        s: two_holders(false, true),
        ..Fake::new()
    };
    f.run(&[AuthBackup, AuthRestore]);
    let out = f.step(OpMintOverride);
    assert!(out.obs.contains(&Some(Obs::MintOverrideRefused)));
    assert!(!mintable(&f.s, 0));
}

/// H.30.4: after `restore_journal_lost`, enrollments after the restore
/// point are lost, so a holder still on an earlier-epoch list may be
/// missing from the snapshot. Minting waits for the id-space-wide lease
/// timeout too; without that, the unknown holder answers after the members
/// reported (I1).
#[test]
fn h30_journal_lost_minting_waits_for_the_id_space_timeout() {
    ablate(
        Fix::JournalLostMintWait,
        Prop::I1EpochExposure,
        gate_profile("h30-journal-lost-wait"),
        two_holders(true, true),
        &gate_candidates(),
    );
}

/// H.31: roster changes are journal entries, so a supported restore
/// replays the complete roster before the snapshot. Without them, a holder
/// enrolled after the backup is missing from the restored roster and is no
/// member, and it answers from its earlier-epoch list after the modeled
/// holder reported (I1). A key rotated after the backup is replayed too, so
/// the holder's report is accepted at once rather than at the lease timeout.
#[test]
fn h31_the_roster_is_journaled_and_replayed() {
    ablate(
        Fix::RosterJournal,
        Prop::I1EpochExposure,
        gate_profile("h31-roster"),
        two_holders(false, true),
        &gate_candidates(),
    );
    for design in [D, D.without(Fix::RosterJournal)] {
        let mut f = Fake {
            s: two_holders(false, true),
            ..Fake::new()
        };
        f.cfg.design = design;
        f.run(&[PeerEnroll(0), AuthBackup, PeerRotate(0)]);
        let rows = f.rows(AuthRestore);
        let replayed = design.has(Fix::RosterJournal);
        assert_eq!(rows.contains(&RowId::H17), replayed);
        assert_eq!(f.s.peers[0].seat, Seat::Pending);
        assert_eq!(f.enabled(PeerMove(0)), replayed, "the rotated key");
        if replayed {
            f.run(&[PeerMove(0), PeerReport(0)]);
            assert_eq!(f.s.peers[0].seat, Seat::Cleared);
        }
    }
}

/// The ambiguity reported to OpenClast: H.30.1 excludes a holder disabled at
/// the snapshot because "a disabled holder receives `403` and halts". The
/// model takes that premise (a holder disabled in the old lineage halts at
/// once). If its `403` has not arrived when lists stop (disabled within one
/// relist interval of `T_stop`, or a disabled holder with open obligations,
/// which HW §4.5 does not refuse), it still serves its earlier-epoch list
/// after the members clear.
#[test]
fn h30_ambiguity_a_holder_disabled_before_the_snapshot_without_its_403() {
    let mut f = Fake {
        s: two_holders(false, true),
        ..Fake::new()
    };
    f.run(&[PeerEnroll(0), PeerDisable(0)]);
    assert_eq!(f.s.peers[0].lease, None, "the premise: it halted on 403");
    // Without the premise: its 403 has not arrived.
    f.s.peers[0].lease = Some(0);
    f.run(&[AuthBackup, AuthRestore]);
    assert_eq!(f.s.peers[0].seat, Seat::Absent);
    f.run(&[FetchStart, ListDeliver, OpAcceptEpoch]);
    f.boot();
    f.list();
    f.request(2);
    f.run(&[AuthProcess { c: 2, idx: 0 }, TickExposure(91)]);
    assert!(mintable(&f.s, 0));
    assert_eq!(
        first_violation(D, &f.s, &[PeerAnswer(0)]),
        Some(Prop::I1EpochExposure)
    );
}
