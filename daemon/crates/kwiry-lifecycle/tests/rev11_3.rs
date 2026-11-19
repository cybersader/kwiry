//! HOLDINGS-WIRE §15 H.32–H.34 (design revision 11.3). Each load-bearing
//! rule is a model fix; removing it reproduces the counterexample it closes,
//! and the same exploration holds with it.

use kwiry_lifecycle::action::{Action, Action::*, faults};
use kwiry_lifecycle::checker::{Counterexample, Profile, Report, check_from};
use kwiry_lifecycle::design::{Design, Fix};
use kwiry_lifecycle::props::{self, Prop};
use kwiry_lifecycle::row_id::RowId;
use kwiry_lifecycle::state::*;
use kwiry_lifecycle::step::{
    CapabilityRefusal, Config, Obs, Outcome, clock_held, exec, mintable, servable,
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
        budget: NONE,
        faults: faults::ALL,
        garbage: false,
        same_state: false,
        liveness: false,
        skip: Prop::I7Deadlock.bit(),
        max_states: 2_000_000,
    }
}

fn gate_profile(name: &'static str) -> Profile {
    Profile {
        liveness: true,
        ..profile(name)
    }
}

struct Fake {
    s: State,
    cfg: Config,
}

impl Fake {
    fn new(s: State) -> Self {
        Fake {
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

// --------------------------------------------------------------- fixtures

/// The modeled holder serves its epoch-0 list; two other engine holders of
/// the id-space are not enrolled yet. Minting is open; one epoch change is
/// left in the budget.
fn two_holders(bad_journal: bool) -> State {
    let mut f = Fake::new(State::initial(
        Budget {
            restores: 1,
            ..NONE
        },
        false,
    ));
    f.s.env.probe = Probe::EpochMint;
    f.s.env.capability_skew = 30;
    f.s.env.peer_symmetry = true;
    f.boot();
    f.list();
    f.s.env.bad_journal = bad_journal;
    f.s
}

/// The modeled holder's epoch acceptance and fresh status, the other holders'
/// actions, the clock, the lease timeout and the refused override: what both
/// epoch causes share.
fn shared_candidates() -> Vec<Action> {
    let mut c = vec![
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
            PeerMove(p),
            PeerReport(p),
            PeerSilent(p),
            PeerAnswer(p),
            PeerForbidden { p, received: true },
            PeerForbidden { p, received: false },
            PeerRefusalAged(p),
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

/// A restore: backup, the confirmed stop of the old authority (H19), the
/// snapshot, the journal-lost decision, and what a still-running old
/// instance would issue under the reading H.33.2 rejects.
fn restore_candidates() -> Vec<Action> {
    let mut c = shared_candidates();
    c.extend([AuthBackup, RestoreBegin, AuthRestore, OpRestoreJournalLost]);
    for p in 0..PEERS as u8 {
        c.push(PeerOldList(p));
    }
    c
}

/// An operator epoch with lists in flight under the other holders' cursors.
fn operator_candidates() -> Vec<Action> {
    let mut c = shared_candidates();
    c.push(OpEpoch);
    for p in 0..PEERS as u8 {
        c.extend([PeerListStart(p), PeerListPage(p)]);
    }
    c
}

/// Peer 0 is enrolled and then disabled before the backup; it serves its
/// epoch-0 list because its `403` has not arrived (disabled within a relist
/// interval of `T_stop`), or never will (open obligations). Then the
/// restore, and the modeled holder's fresh status clears its own seat.
fn disabled_before_restore(design: Design) -> Fake {
    let mut f = Fake::new(two_holders(false));
    f.cfg.design = design;
    f.run(&[PeerEnroll(0), PeerDisable(0)]);
    assert_eq!(f.s.peers[0].lease, Some(0), "disabling stops nothing");
    f.run(&[AuthBackup, RestoreBegin, AuthRestore]);
    f.holder_reports();
    f.step(TickExposure(91));
    f
}

// ------------------------------------------------------------------ H.32

/// H.32.1–2, the rev 11.2 shape: a holder disabled before the snapshot still
/// serves its earlier-epoch list. It is a member now, so minting stays
/// closed after the modeled holder reports, and no capability reaches it at
/// 91 s. It clears by lease at `T_stop + serve_seconds_ceiling + 150 s`
/// (1051 s), when its lease has lapsed (H.32.4: the cap is unchanged). With
/// H.30.1's membership (enabled holders only) it is no member, minting
/// reopens, and it answers at 91 s; the exposure ends only when its lease
/// lapses.
#[test]
fn h32_a_disabled_holder_is_a_member_and_exposure_stays_within_90_s() {
    let mut f = disabled_before_restore(D);
    assert_eq!(f.s.peers[0].seat, Seat::Pending);
    assert!(!mintable(&f.s, 0), "the disabled member is pending");
    assert!(
        !f.enabled(PeerAnswer(0)),
        "nothing is minted for it at 91 s"
    );
    let out = f.step(LeaseTimeout);
    assert!(out.rows.iter().any(|r| r == RowId::H16));
    assert!(out.obs.contains(&Some(Obs::MintGateClearedByLease {
        holder: false,
        peers: 1
    })));
    assert!(f.s.env.exposure_elapsed <= 1051);
    assert_eq!(f.s.peers[0].lease, None);
    assert!(mintable(&f.s, 0));
    assert!(!f.enabled(PeerAnswer(0)));

    // H.30.1 as first written: minting reopens at the holder's report.
    let without = D.without(Fix::RosterMintMembership);
    let f = disabled_before_restore(without);
    assert_eq!(f.s.peers[0].seat, Seat::Absent);
    assert!(mintable(&f.s, 0));
    assert_eq!(
        first_violation(without, &f.s, &[PeerAnswer(0)]),
        Some(Prop::I1EpochExposure),
        "91 s > 90 s"
    );
    // Up to the remaining lease: the exposure ends when it lapses.
    let s = exec(&f.s, LeaseTimeout, &f.cfg).unwrap().unwrap().next;
    assert_eq!(s.peers[0].lease, None);
    assert!(exec(&s, PeerAnswer(0), &f.cfg).unwrap().is_none());
}

/// H.32.2 exhaustively: every roster holder is a member. With H.30.1's
/// membership, some disabled holder still serving its earlier-epoch list is
/// left out, and it answers after the members clear (I1).
#[test]
fn h32_roster_membership_is_necessary() {
    let cex = ablate(
        Fix::RosterMintMembership,
        Prop::I1EpochExposure,
        gate_profile("h32-membership"),
        two_holders(false),
        &restore_candidates(),
    );
    assert!(
        cex.prefix
            .iter()
            .any(|t| matches!(t.action, PeerDisable(_))),
        "a disabled holder"
    );
}

/// H.32.3: a recorded `403` clears no member. A disabled member's request is
/// refused and the response lost: under H.32.3 nothing changes. Under the
/// reading H.32.3 rejects, the recorded `403` clears its seat, minting
/// reopens, and the holder that never received the refusal answers at 91 s.
/// A `403` it does receive halts it, and it still clears only by H15 or H16.
#[test]
fn h32_a_recorded_403_clears_no_member() {
    let f = disabled_before_restore(D);
    let lost = PeerForbidden {
        p: 0,
        received: false,
    };
    assert!(!f.enabled(lost), "a lost 403 changes nothing");
    let mut g = Fake::new(f.s.clone());
    g.step(PeerForbidden {
        p: 0,
        received: true,
    });
    assert_eq!(g.s.peers[0].lease, None, "it halts");
    assert_eq!(g.s.peers[0].seat, Seat::Pending, "and stays a member");
    assert!(!mintable(&g.s, 0));

    let without = D.without(Fix::NoForbiddenClear);
    let f = disabled_before_restore(without);
    assert_eq!(
        first_violation(without, &f.s, &[lost, PeerAnswer(0)]),
        Some(Prop::I1EpochExposure)
    );
    ablate(
        Fix::NoForbiddenClear,
        Prop::I1EpochExposure,
        gate_profile("h32-forbidden"),
        two_holders(false),
        &restore_candidates(),
    );
}

/// H.32.2's only exclusion: a holder refused on every route since `t_r`
/// with `t_r + serve_seconds_ceiling + 150 s < T_stop` can't have received
/// a list since `t_r`; its lease lapsed before the gate began. It is no
/// member, so the modeled holder's report resumes minting at once. A holder
/// disabled more recently is a member.
#[test]
fn h32_a_holder_refused_long_before_t_stop_is_no_member() {
    let mut f = Fake::new(two_holders(false));
    f.run(&[
        PeerEnroll(0),
        PeerEnroll(1),
        PeerDisable(0),
        PeerDisable(1),
        PeerSilent(0),
        PeerSilent(1),
        PeerRefusalAged(0),
    ]);
    assert_eq!(f.s.peers[0].lease, None);
    f.run(&[AuthBackup, RestoreBegin, AuthRestore]);
    assert_eq!(f.s.peers[0].seat, Seat::Absent);
    assert_eq!(f.s.peers[1].seat, Seat::Pending);
    f.holder_reports();
    assert!(
        !mintable(&f.s, 0),
        "the recently disabled member is pending"
    );
    f.step(LeaseTimeout);
    assert!(mintable(&f.s, 0));

    let mut f = Fake::new(two_holders(false));
    f.run(&[
        PeerEnroll(0),
        PeerDisable(0),
        PeerRefusalAged(0),
        AuthBackup,
        RestoreBegin,
        AuthRestore,
    ]);
    assert_eq!(f.s.peers[0].seat, Seat::Absent);
    f.holder_reports();
    assert!(mintable(&f.s, 0), "no member is pending");
}

// The exhaustive restore exploration with every fix, both causes, is
// `h30_gate_holds_exposure_and_always_resumes` in `tests/rev11_2.rs`, which
// runs revision 11.3 over the same actions.

// ------------------------------------------------------------------ H.33

/// H.33.1: an operator epoch's transaction is `T_stop`. It advances every
/// holder's `holder_rev`, so a peer's earlier-epoch cursor answers `410`
/// and the peer keeps only the lease it had before `T_stop`, which lapses
/// at the lease timeout. The modeled holder moves through `accept-epoch`,
/// and minting resumes once the peer clears by lease.
#[test]
fn h33_an_operator_epoch_advances_holder_rev() {
    let mut f = Fake::new(two_holders(false));
    f.run(&[PeerEnroll(0), PeerSilent(0), PeerListStart(0)]);
    let rev = f.s.auth.core.holder_rev;
    let rows = f.rows(OpEpoch);
    assert!(rows.contains(&RowId::H18) && rows.contains(&RowId::H14));
    assert_eq!(f.s.auth.lists_stopped_at, Some(0), "T_stop is the commit");
    assert!(f.s.auth.core.holder_rev > rev);
    assert_eq!(f.s.peers[0].seat, Seat::Pending);
    let out = f.step(PeerListPage(0));
    assert!(out.rows.iter().any(|r| r == RowId::H18), "410 list_expired");
    assert!(!out.obs.contains(&Some(Obs::EarlierEpochListAfterStop)));
    assert!(!f.s.peers[0].late);
    f.holder_reports();
    assert!(!mintable(&f.s, 0));
    f.step(LeaseTimeout);
    assert_eq!(f.s.peers[0].lease, None);
    assert!(mintable(&f.s, 0));
    assert!(!f.enabled(PeerAnswer(0)));
}

/// H.33.1 ablation: without the `holder_rev` advance, an earlier-epoch page
/// is issued after `T_stop` under a cursor from before it. The lease it
/// renews outlives the lease timeout, so once the member clears by lease the
/// holder answers from its earlier-epoch list (I1).
#[test]
fn h33_without_the_bump_an_earlier_epoch_page_follows_t_stop() {
    let without = D.without(Fix::OperatorEpochRevBump);
    let mut f = Fake::new(two_holders(false));
    f.cfg.design = without;
    f.run(&[PeerEnroll(0), PeerSilent(0), PeerListStart(0), OpEpoch]);
    let out = f.step(PeerListPage(0));
    assert!(out.obs.contains(&Some(Obs::EarlierEpochListAfterStop)));
    assert!(f.s.peers[0].late);
    f.holder_reports();
    f.step(LeaseTimeout);
    assert_eq!(f.s.peers[0].lease, Some(0), "renewed after T_stop");
    assert_eq!(
        first_violation(without, &f.s, &[PeerAnswer(0)]),
        Some(Prop::I1EpochExposure)
    );
    let cex = ablate(
        Fix::OperatorEpochRevBump,
        Prop::I1EpochExposure,
        gate_profile("h33-operator-bump"),
        two_holders(false),
        &operator_candidates(),
    );
    let epoch = cex
        .prefix
        .iter()
        .position(|t| t.action == OpEpoch)
        .expect("an operator epoch");
    assert!(
        cex.prefix[epoch..]
            .iter()
            .any(|t| matches!(t.action, PeerListPage(_))),
        "an earlier-epoch page is issued after T_stop"
    );
}

/// H.33.1 exhaustively: with the operator epoch, its cursors in flight, and
/// everything else of the gate, I1 (90 s) and L1 hold.
#[test]
fn h33_operator_epoch_holds_exposure_and_always_resumes() {
    let r = check_from(
        &gate_profile("h33-operator"),
        two_holders(false),
        operator_candidates(),
    );
    eprintln!("{}", r.render());
    assert!(r.violation.is_none(), "{}", r.render());
    for row in [RowId::H14, RowId::H15, RowId::H16, RowId::H18] {
        assert!(r.rows_seen[row.index()], "{}", row.name());
    }
}

/// H.33.2: a restore proceeds only after the tooling confirmed the old
/// authority stopped with its start-up disabled (H19). Under the reading
/// H.33.2 rejects it proceeds while the old instance runs; that instance
/// keeps issuing earlier-epoch lists after `T_stop`, the renewed lease
/// outlives the lease timeout, and the holder answers at 1051 s, far past
/// 90 s (I1).
#[test]
fn h33_a_restore_refuses_while_the_old_authority_runs() {
    let mut f = Fake::new(two_holders(false));
    f.run(&[PeerEnroll(0), PeerSilent(0), AuthBackup]);
    assert!(!f.enabled(AuthRestore), "not confirmed stopped: refused");
    assert!(f.rows(RestoreBegin).contains(&RowId::H19));
    assert!(!mintable(&f.s, 0) && !f.enabled(FetchStart));
    f.step(AuthRestore);
    assert!(!f.enabled(PeerOldList(0)));

    let without = D.without(Fix::StoppedOldAuthority);
    let mut f = Fake::new(two_holders(false));
    f.cfg.design = without;
    f.run(&[PeerEnroll(0), PeerSilent(0), AuthBackup, AuthRestore]);
    let out = f.step(PeerOldList(0));
    assert!(out.obs.contains(&Some(Obs::EarlierEpochListAfterStop)));
    f.holder_reports();
    f.step(LeaseTimeout);
    assert!(f.s.env.exposure_elapsed > 90);
    assert_eq!(
        first_violation(without, &f.s, &[PeerAnswer(0)]),
        Some(Prop::I1EpochExposure)
    );
    ablate(
        Fix::StoppedOldAuthority,
        Prop::I1EpochExposure,
        gate_profile("h33-old-authority"),
        two_holders(false),
        &restore_candidates(),
    );
}

/// H.33.3: a restore whose snapshot comes after `T_stop +
/// serve_seconds_ceiling + 150 s` clears every member by lease in its first
/// transaction, and minting may resume at once: no earlier-epoch list was
/// issued after `T_stop`. For a supported restore `T_stop` is the start of
/// the procedure (H19); for `restore_journal_lost`, the restored authority's
/// first start.
#[test]
fn h33_a_restore_that_outlasts_the_timeout_resumes_minting_at_once() {
    let mut f = Fake::new(two_holders(false));
    f.run(&[PeerEnroll(0), PeerSilent(0), AuthBackup, RestoreBegin]);
    assert_eq!(f.s.auth.lists_stopped_at, Some(0));
    f.step(LeaseTimeout);
    assert_eq!(f.s.peers[0].lease, None);
    let out = f.step(AuthRestore);
    assert!(out.rows.iter().any(|r| r == RowId::H16));
    assert!(out.obs.contains(&Some(Obs::MintGateClearedByLease {
        holder: true,
        peers: 1
    })));
    assert!(mintable(&f.s, 0));

    let mut f = Fake::new(two_holders(true));
    f.run(&[PeerEnroll(0), PeerSilent(0), AuthBackup, RestoreBegin]);
    assert_eq!(
        f.s.auth.lists_stopped_at, None,
        "not before the first start"
    );
    assert!(!f.enabled(LeaseTimeout));
    f.step(TickExposure(60));
    assert!(f.rows(AuthRestore).contains(&RowId::H12j));
    assert_eq!(f.s.auth.lists_stopped_at, Some(60), "the first start");
    f.step(LeaseTimeout);
    assert!(f.s.env.exposure_elapsed > 60 + 1050);
    let out = f.step(OpRestoreJournalLost);
    assert!(out.obs.contains(&Some(Obs::MintGateClearedByLease {
        holder: true,
        peers: 1
    })));
    assert!(mintable(&f.s, 0), "and the id-space-wide wait has passed");
}

// ------------------------------------------------------------------ H.34

const ASK: Action = StartCapability {
    room: 0,
    iat: 0,
    exp: 60,
};
const AHEAD: Action = StartCapability {
    room: 1,
    iat: 40,
    exp: 100,
};
const CONFIRM: Action = VerifyListPage {
    iat: 0,
    rtt: Some(5),
};

/// The hold is raised and written; the clock has been corrected since; the
/// volume is full and the reserve spent, so the next write fails (K2x).
fn hold_on_a_full_disk() -> Fake {
    let mut f = Fake::new(State::initial(Budget { crashes: 1, ..NONE }, false));
    f.s.env.probe = Probe::Exposure;
    f.s.env.capability_skew = 30;
    f.s.env.lag = 40;
    f.boot();
    f.list();
    f.run(&[AHEAD, PersistClockHold, ClockStep { lag: 0 }]);
    assert!(clock_held(&f.s) && f.s.dur.clock_behind);
    f.s.env.disk_full = true;
    f.s.dur.reserve = false;
    f.s.vol.rsv = Rsv::Partial;
    f
}

/// H.34: the clear writes holder state first. When that write fails, the
/// hold stays raised in memory and durably; a crash after the failed write
/// keeps it across the restart, and the next confirmed correction clears it.
#[test]
fn h34_a_failed_clearing_write_keeps_the_hold_across_a_crash() {
    let mut f = hold_on_a_full_disk();
    f.step(ClockProbe);
    let rows = f.rows(CONFIRM);
    assert!(rows.contains(&RowId::C20) && rows.contains(&RowId::K2x));
    assert!(clock_held(&f.s) && f.s.dur.clock_behind, "raised in both");
    let refused = f.outcome(ASK).unwrap();
    assert!(refused.obs.contains(&Some(Obs::CapabilityRefused(
        CapabilityRefusal::ClockBehind
    ))));
    f.run(&[Crash, SpaceFreed]);
    assert!(clock_held(&f.s), "holder state keeps it");
    f.boot();
    assert!(clock_held(&f.s), "loaded from holder state");
    f.step(FetchStart);
    let rows = f.rows(CONFIRM);
    assert!(rows.contains(&RowId::C20) && !rows.contains(&RowId::K2x));
    assert!(!clock_held(&f.s) && !f.s.dur.clock_behind);
    f.step(ListDeliver);
    while f.s.vol.apply.is_some() {
        f.step(Apply);
    }
    assert!(servable(&f.s, 0));
}

/// H.34 ablation and crash coverage: every interleaving of the failed
/// clearing write with one crash, the restart, freed space and later
/// confirmations keeps I1. Under the reading H.34 rejects (drop the
/// in-memory hold, then write), the failed write leaves the engine serving
/// while holder state still records the hold, and a capability is answered
/// (I1).
#[test]
fn h34_the_clear_writes_first() {
    let f = hold_on_a_full_disk();
    let candidates = vec![
        ClockProbe,
        ProbeWindow,
        CONFIRM,
        ASK,
        FinishCapability,
        Crash,
        Boot,
        Markers,
        Narrow,
        FetchStart,
        ListDeliver,
        Apply,
        SpaceFreed,
        Replenish,
        PersistClockHold,
    ];
    let p = profile("h34-clear-order");
    let with = check_from(&p, f.s.clone(), candidates.clone());
    eprintln!("{}", with.render());
    assert!(with.violation.is_none(), "{}", with.render());
    for row in [RowId::C20, RowId::K2x, RowId::S6] {
        assert!(with.rows_seen[row.index()], "{}", row.name());
    }
    let cex = ablate(
        Fix::ClockClearWriteFirst,
        Prop::I1CapabilityExposure,
        p,
        f.s.clone(),
        &candidates,
    );
    eprintln!("{}", cex.render());
    let without = D.without(Fix::ClockClearWriteFirst);
    assert_eq!(
        first_violation(without, &f.s, &[ClockProbe, CONFIRM, ASK]),
        Some(Prop::I1CapabilityExposure)
    );
}
