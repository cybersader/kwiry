//! HOLDINGS-WIRE-v2 H.50–H.52 (design revision 11.6).
//!
//! - H.50.1 is a model fix. Revision 11.5 did **not** already hold back a
//!   lower state: R1 checked the floor and the current slot, not the
//!   `uncertain` attempts an R3 or R14 left behind. Without the fix a lower
//!   report is accepted first and a delayed higher attempt then commits, a
//!   stale claim at the authority (I3). R15's time clause (H.50.2, the exact
//!   `D = exp + 120 s`) and the early relist (H.50.4) are what let the lower
//!   state go out once the attempt is proven absent.
//! - H.51 renames journal members the model does not carry; no test.
//! - H.52 is a model fix in two rules: the key set arrives with
//!   `accept-epoch` (H.52.2) and is in effect before the first new-epoch
//!   admit (H.52.1). Removing either reproduces the revision-11.5
//!   counterexample: a member clears by the lease timeout and answers an
//!   old-instance capability. If the set can't be supplied the engine fails
//!   closed. A wrong `unchanged` confirmation is the named residual: audited
//!   and recorded, never a gate guarantee.

use kwiry_lifecycle::action::{Action, Action::*, faults};
use kwiry_lifecycle::checker::{Counterexample, Profile, Report, check_from};
use kwiry_lifecycle::design::{Design, Fix};
use kwiry_lifecycle::props::{self, Prop};
use kwiry_lifecycle::row_id::RowId;
use kwiry_lifecycle::state::*;
use kwiry_lifecycle::step::{Config, Obs, Outcome, exec, mintable, servable};

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
const D: Design = Design::REV11_6;

struct Fake {
    s: State,
    cfg: Config,
}

impl Fake {
    fn new(s: State, design: Design) -> Self {
        Fake {
            s,
            cfg: Config {
                design,
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
    /// Fetch, deliver and apply one list; the rows every step cited.
    fn list(&mut self) -> Vec<RowId> {
        let mut rows = self.rows(FetchStart);
        rows.extend(self.rows(ListDeliver));
        while self.s.vol.apply.is_some() {
            rows.extend(self.rows(Apply));
        }
        rows
    }
    fn request(&mut self, c: u8) {
        self.run(&[Enqueue(c), Transmit(c), Handoff(c), Dispatch(c)]);
    }
    /// The modeled holder accepts the new epoch, restarts, applies a list of
    /// it and has its fresh status accepted: its seat clears (H15).
    fn holder_reports(&mut self) {
        self.run(&[FetchStart, ListDeliver, OpAcceptEpoch]);
        self.boot();
        self.list();
        self.request(2);
        assert!(
            self.rows(AuthProcess { c: 2, idx: 0 })
                .contains(&RowId::H15)
        );
    }
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

/// With the fix the exploration holds; without it, `expected` is the first
/// violation found. Returns the counterexample.
fn ablate(fix: Fix, expected: Prop, p: Profile, s: State, actions: &[Action]) -> Counterexample {
    let with = check_from(&p, s.clone(), actions.to_vec());
    assert!(with.violation.is_none(), "{}", with.render());
    let without: Report = check_from(
        &Profile {
            design: D.without(fix),
            ..p
        },
        s,
        actions.to_vec(),
    );
    eprintln!("{}{}", with.render(), without.render());
    let cex = without
        .violation
        .unwrap_or_else(|| panic!("{fix:?}: no counterexample without the fix"));
    assert_eq!(cex.prop, expected, "{}", cex.render());
    cex
}

fn observed(out: &Outcome, o: Obs) -> bool {
    out.obs.contains(&Some(o))
}

// ------------------------------------------------------------------ H.50

/// R0 retired and its retirement applied; R1 may be dropped from the
/// projection once (its unit then counts as unattributed), and one network
/// failure may cut a report's connection, leaving its request in flight.
const H50_BUDGET: Budget = Budget {
    change_rooms: 0b10,
    changes: 1,
    faults: 1,
    ..NONE
};

fn retired(design: Design) -> Fake {
    let mut f = Fake::new(
        State::initial(
            Budget {
                retires: 1,
                retire_rooms: 0b01,
                ..H50_BUDGET
            },
            false,
        ),
        design,
    );
    f.boot();
    f.list();
    f.step(AuthRetire(0));
    f.list();
    f
}

/// `complete` for R0 goes on the wire; its connection fails, so its outcome
/// is unknown while the request is still in flight. R1 is then dropped, so
/// the retry's transmission guard fails (R3): `complete` is an uncertain
/// attempt, and the next pass derives `incomplete`.
fn complete_uncertain(design: Design) -> Fake {
    let mut f = retired(design);
    f.run(&[PurgeDelete { u: 0, fail: false }, PurgeScan(0)]);
    f.request(0);
    let complete = f.s.net.reqs[0].get(0).unwrap().key;
    assert_eq!(complete.obl().map(|k| k.state), Some(RState::Complete));
    f.step(NetFail(0));
    f.step(AuthUnlist(1));
    f.list();
    assert_ne!(f.s.vol.inv, 0, "R1's unit is unattributed");
    assert!(f.rows(Transmit(0)).contains(&RowId::R3));
    assert!(f.s.dur.uncertain[0].contains(&complete));
    f.step(PurgeScan(0));
    f
}

/// H.50.1. Without the fix R1 derives `incomplete` while `complete` is
/// uncertain; the authority accepts it, then the delayed `complete` commits
/// inside its deadline and the authority holds a claim the holder no longer
/// makes (I3). `409 state_regression` protects only the opposite order.
/// Revision 11.5 behaved like this: the model did not already wait.
#[test]
fn h50_a_lower_state_sent_while_a_higher_one_is_uncertain_commits_a_stale_claim() {
    for design in [
        D.without(Fix::LowerWaitsForUncertain),
        // Revision 11.5 as shipped.
        Design::REV11_5,
    ] {
        let mut f = complete_uncertain(design);
        assert!(f.enabled(Enqueue(0)), "{design:?} derives the lower state");
        f.request(0);
        // The network keeps requests sorted: find `incomplete`, the lower
        // report, and let the authority accept it first; the delayed
        // `complete` is then the only request left.
        let lower = f.s.net.reqs[0]
            .iter()
            .position(|q| q.key.obl().is_some_and(|k| k.state == RState::Incomplete))
            .expect("the lower report is in flight") as u8;
        assert_ne!(f.s.net.reqs[0].get(lower as usize).unwrap().retracts, 0);
        assert_eq!(
            first_violation(
                design,
                &f.s,
                &[
                    AuthProcess { c: 0, idx: lower },
                    AuthProcess { c: 0, idx: 0 },
                ]
            ),
            Some(Prop::I3StaleClaim),
            "{design:?}"
        );
    }

    let f = complete_uncertain(D);
    assert!(!f.enabled(Enqueue(0)), "the lower state waits");
}

/// H.50.2: the uncertain attempt resolves either way, and only then does a
/// lower state go out. (a) It commits: the next list's `last_accepted`
/// covers it (R15), the floor rises to `complete` (R13), and no lower state
/// is ever derived. (b) It is lost: once no copy can still commit (page 0
/// past `D = exp + 120 s`), an early relist (H.50.4) brings a list whose
/// `last_accepted` is below it. R15's time clause resolves it, and
/// `incomplete` goes out.
#[test]
fn h50_the_attempt_resolves_by_a_covering_list_or_page_0_past_d() {
    let mut f = complete_uncertain(D);
    f.step(AuthProcess { c: 0, idx: 0 });
    assert!(f.enabled(FetchStart), "the list changed");
    let rows = f.list();
    assert!(rows.contains(&RowId::R15) && rows.contains(&RowId::R13));
    assert!(f.s.dur.uncertain[0].iter().next().is_none());
    assert_eq!(f.s.dur.floors[0].map(|x| x.state), Some(RState::Complete));
    assert!(!f.enabled(Enqueue(0)), "never below the floor");

    let mut f = complete_uncertain(D);
    let resolving = f.s.clone();
    f.step(DropDead { c: 0, idx: 0 });
    assert!(f.s.net.reqs[0].iter().next().is_none());
    assert!(f.enabled(FetchStart), "an early relist (H.50.4)");
    let rows = f.list();
    assert!(rows.contains(&RowId::R15));
    assert!(f.s.dur.uncertain[0].iter().next().is_none());
    assert!(f.s.dur.floors[0].is_none(), "nothing was committed");
    f.request(0);
    let sent = f.s.net.reqs[0].get(0).unwrap();
    assert_eq!(sent.key.obl().map(|k| k.state), Some(RState::Incomplete));
    assert_eq!(sent.retracts, 0);

    // While a copy is still in flight, the list does not resolve it: page 0
    // can't be past `D` yet. Nothing changed in the list, so no relist is
    // explored either.
    assert!(!Fake::new(resolving, D).enabled(FetchStart));
}

/// H.50 exhaustively, from the instant R0's `complete` is on the wire: one
/// network failure on its channel, and R1 dropped from the list once. With
/// the fix I1–I9 and L1 hold, including the time clause's liveness (every
/// lower state the holder needs eventually goes out). Without it, the first
/// violation is the stale claim. (From R0's applied retirement, with every
/// channel's failure and every list change, the graph exceeds 6M states.)
#[test]
fn h50_exhaustive_ablation() {
    let p = Profile {
        name: "h50-stale-claim",
        design: D,
        budget: H50_BUDGET,
        faults: faults::NET_FAIL,
        garbage: false,
        same_state: false,
        liveness: true,
        skip: 0,
        max_states: 6_000_000,
    };
    let mut f = retired(D);
    f.run(&[PurgeDelete { u: 0, fail: false }, PurgeScan(0)]);
    f.request(0);
    let candidates: Vec<Action> = Action::candidates()
        .into_iter()
        .filter(|a| !matches!(a, NetFail(1 | 2) | AuthRelist(_) | AuthDrop(_)))
        .collect();
    let cex = ablate(
        Fix::LowerWaitsForUncertain,
        Prop::I3StaleClaim,
        p,
        f.s,
        &candidates,
    );
    assert!(
        cex.prefix.iter().any(|t| t.action == NetFail(0)),
        "the higher attempt's outcome became unknown while it was in flight"
    );
}

// ------------------------------------------------------------------ H.52

fn gate_profile(name: &'static str) -> Profile {
    Profile {
        name,
        design: D,
        budget: NONE,
        faults: faults::ALL,
        garbage: false,
        same_state: false,
        liveness: true,
        skip: Prop::I7Deadlock.bit(),
        max_states: 2_000_000,
    }
}

/// The modeled holder serves its epoch-0 list; two other engine holders of
/// the id-space are not enrolled yet. One epoch change is left in the budget.
fn two_holders(bad_journal: bool) -> State {
    let mut f = Fake::new(
        State::initial(
            Budget {
                restores: 1,
                ..NONE
            },
            false,
        ),
        D,
    );
    f.s.env.probe = Probe::EpochMint;
    f.s.env.capability_skew = 30;
    f.s.env.peer_symmetry = true;
    f.boot();
    f.list();
    f.s.env.bad_journal = bad_journal;
    f.s
}

/// The false-attestation fixture of `tests/rev11_5.rs`: a false attestation
/// after a failed source check, in a raw or supported restore; the old
/// instance keeps issuing lists and minting; the other holders may fetch a
/// new-epoch list and wait for `accept-epoch`.
fn false_candidates() -> Vec<Action> {
    let mut c = vec![
        AuthBackup,
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
        RawRestore,
        SourceCheckFails,
        FalseAttestation,
        AuthRestore,
        OpRestore,
        OpRestoreJournalLost,
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
            PeerForbidden { p, received: false },
            PeerRefusalAged(p),
            PeerOldList(p),
            PeerSeesEpoch(p),
            PeerOldCapability(p),
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

/// A false attestation in a raw restore after a failed source check, the
/// exit, and peer 0 still on its epoch-0 list.
fn false_exit(design: Design, key_set_unavailable: bool) -> Fake {
    let mut f = Fake::new(two_holders(false), design);
    f.s.env.key_set_unavailable = key_set_unavailable;
    f.run(&[PeerEnroll(0), AuthBackup]);
    f.run(&[RawRestore, SourceCheckFails, FalseAttestation]);
    assert!(f.s.env.old_running, "the attestation is false");
    f.step(OpRestore);
    assert_eq!(f.s.auth.attest, Attest::Exited);
    f
}

/// Peer 0 accepts the new epoch and applies a list of it, never reports,
/// and clears by the lease timeout after the modeled holder reported.
fn cleared_by_lease(design: Design) -> Fake {
    let mut f = false_exit(design, false);
    assert!(f.rows(PeerMove(0)).contains(&RowId::H25));
    f.step(PeerSilent(0));
    f.holder_reports();
    let out = f.step(LeaseTimeout);
    assert!(observed(
        &out,
        Obs::MintGateClearedByLease {
            holder: false,
            peers: 1
        }
    ));
    assert_eq!(f.s.peers[0].lease, Some(1), "it holds a new-epoch lease");
    assert!(mintable(&f.s, 0));
    f
}

/// H.52.1–2: the key set the new epoch requires arrives with `accept-epoch`
/// (H25), in the commit that arms it, and is in effect before the first
/// new-epoch list is admitted. A member that clears by the lease timeout
/// holds a new-epoch lease under the new set and refuses the old instance's
/// capabilities.
#[test]
fn h52_the_key_set_arrives_with_accept_epoch_before_the_first_admit() {
    let f = cleared_by_lease(D);
    assert!(f.s.peers[0].cap_rotated);
    assert!(!f.enabled(PeerOldCapability(0)));
}

/// H.52, each rule necessary. Arming without the key set (no H.52.2: the
/// engine keeps its current set) and admitting a new-epoch list before the
/// set is in effect (no H.52.1: it takes effect only at the first report,
/// H.49.3 as written) both reproduce the revision-11.5 counterexample: the
/// member clears by the lease timeout and answers an old-instance capability
/// (I1). The exploration holds with the rule and finds the same failure,
/// cleared by lease and not by report, without it.
#[test]
fn h52_arming_without_the_set_or_admitting_before_it_answers_old_capabilities() {
    for fix in [Fix::AcceptEpochKeySet, Fix::KeySetBeforeAdmit] {
        let without = D.without(fix);
        let f = cleared_by_lease(without);
        assert!(!f.s.peers[0].cap_rotated, "{fix:?}");
        assert_eq!(
            first_violation(without, &f.s, &[PeerOldCapability(0)]),
            Some(Prop::I1CapabilityExposure),
            "{fix:?}"
        );
        let cex = ablate(
            fix,
            Prop::I1CapabilityExposure,
            gate_profile("h52-rules"),
            two_holders(false),
            &false_candidates(),
        );
        assert!(
            cex.prefix.iter().any(|t| t.action == LeaseTimeout),
            "{fix:?}: cleared by lease, not by report"
        );
    }
}

/// H.52.3: without the key set the engine fails closed. Neither the other
/// holder nor the modeled one arms its acceptance; the modeled holder's
/// operator gets an explicit refusal and nothing changes. No new-epoch list
/// is admitted, the earlier leases lapse at the lease timeout, so nothing is
/// served and minting resumes, and nothing is deleted.
#[test]
fn h52_without_the_key_set_the_engine_fails_closed() {
    let mut f = false_exit(D, true);
    assert!(!f.enabled(PeerMove(0)), "no arming");
    let units = f.s.dur.units;
    f.run(&[FetchStart, ListDeliver]);
    assert!(f.s.vol.alert_epoch.is_some(), "`unexpected_epoch`");
    let before = f.s.clone();
    let out = f.step(OpAcceptEpoch);
    assert!(out.rows.iter().any(|r| r == RowId::H25));
    assert!(observed(&out, Obs::AcceptEpochRefused));
    assert_eq!(f.s, before, "nothing armed or consumed");
    f.step(LeaseTimeout);
    assert_eq!(f.s.peers[0].lease, None, "its earlier lease lapsed");
    assert!(!(0..ROOMS).any(|r| servable(&f.s, r)), "nothing served");
    assert!(mintable(&f.s, 0), "minting resumes by the lease timeout");
    assert!(!f.enabled(PeerOldCapability(0)) && !f.enabled(PeerAnswer(0)));
    assert_eq!(f.s.dur.units, units, "nothing deleted");

    // Exhaustively, both restore causes: I1 and L1 hold with the set
    // unavailable.
    for lost in [false, true] {
        let mut s = two_holders(lost);
        s.env.key_set_unavailable = true;
        let r = check_from(&gate_profile("h52-fail-closed"), s, false_candidates());
        eprintln!("fail-closed lost={lost}: {}", r.render());
        assert!(r.violation.is_none(), "{}", r.render());
        assert!(r.rows_seen[RowId::H25.index()]);
    }
}

/// H.52.4: an operator who wrongly confirms `unchanged` after an exit by
/// attestation is a named residual. The engine audits the choice, the
/// restore record names the rotation, and the old instance's capabilities
/// are answered under the new-epoch lease. The model records that as the
/// residual and never counts it as a gate guarantee; with it in the
/// exploration nothing else is exposed.
#[test]
fn h52_a_wrong_unchanged_is_audited_and_recorded_as_the_residual() {
    let mut f = false_exit(D, false);
    let out = f.step(PeerUnchanged(0));
    assert!(observed(&out, Obs::UnchangedAfterAttestation));
    assert!(f.s.peers[0].unchanged && !f.s.peers[0].cap_rotated);
    f.step(PeerSilent(0));
    f.holder_reports();
    f.step(LeaseTimeout);
    assert!(mintable(&f.s, 0));
    assert!(observed(
        &f.step(PeerOldCapability(0)),
        Obs::UnchangedResidual
    ));

    let mut candidates = false_candidates();
    for p in 0..PEERS as u8 {
        candidates.push(PeerUnchanged(p));
    }
    let r = check_from(
        &gate_profile("h52-unchanged"),
        two_holders(false),
        candidates,
    );
    eprintln!("{}", r.render());
    assert!(r.violation.is_none(), "{}", r.render());
}

/// The peer-symmetry quotient keeps the verdicts and rows with the H.52.4
/// action added: with every fix, and without H.52.2.
#[test]
fn h52_peer_symmetry_preserves_verdicts() {
    let state = |symmetry| {
        let mut s = two_holders(false);
        s.env.peer_symmetry = symmetry;
        s
    };
    let mut candidates = false_candidates();
    for p in 0..PEERS as u8 {
        candidates.push(PeerUnchanged(p));
    }
    for fix in [None, Some(Fix::AcceptEpochKeySet)] {
        let p = Profile {
            design: fix.map_or(D, |x| D.without(x)),
            ..gate_profile("h52-symmetry")
        };
        let full = check_from(&p, state(false), candidates.clone());
        let reduced = check_from(&p, state(true), candidates.clone());
        eprintln!("{}{}", full.render(), reduced.render());
        assert_eq!(
            full.violation.as_ref().map(|v| v.prop),
            reduced.violation.as_ref().map(|v| v.prop)
        );
        assert_eq!(full.violation.is_none(), fix.is_none());
        if fix.is_none() {
            assert_eq!(full.rows_seen, reduced.rows_seen);
            assert!(reduced.states < full.states);
        }
    }
}
