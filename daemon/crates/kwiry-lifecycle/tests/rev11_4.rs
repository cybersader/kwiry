//! HOLDINGS-WIRE-v2 H.41–H.44 (design revision 11.4). H.41 and H.44 are
//! model fixes: removing one reproduces the counterexample it closes, and
//! the same exploration holds with it. H.42 and H.43 confirm readings the
//! model already took; their tests pin those readings. The tests run the
//! latest design: from revision 11.5 an attestation needs a recorded failed
//! source check first (HW v2 H.48.1), so the fixtures include that step.

use kwiry_lifecycle::action::{Action, Action::*, faults};
use kwiry_lifecycle::checker::{Counterexample, Profile, Report, check_from, check_from_with_goal};
use kwiry_lifecycle::design::{Design, Fix};
use kwiry_lifecycle::props::{self, Prop};
use kwiry_lifecycle::row_id::RowId;
use kwiry_lifecycle::state::*;
use kwiry_lifecycle::step::{Config, Obs, Outcome, exec, mintable};

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

/// What every fixture shares: the modeled holder's epoch acceptance and
/// fresh status, the other holders' roster changes (enrollment, disable,
/// enable, key rotation), lost `403`s, aged refusals, moves, reports,
/// silence and answers, the clock, the lease timeout and the refused
/// override. A `403` the peer receives is left out by the simulation
/// argument of `tests/rev11_2.rs` (it only drops an earlier-epoch lease).
fn shared_candidates() -> Vec<Action> {
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

/// H.41: a supported restore or `restore_journal_lost` with the other
/// holders' list cursors in flight across it, and one ordinary old-lineage
/// `holder_rev` advance after the backup.
fn cursor_candidates() -> Vec<Action> {
    let mut c = shared_candidates();
    c.extend([AuthTouch, RestoreBegin, AuthRestore, OpRestoreJournalLost]);
    for p in 0..PEERS as u8 {
        c.extend([PeerListStart(p), PeerListPage(p)]);
    }
    c
}

/// H.44: a raw restore, the old instance still issuing lists, the
/// confirmation by tooling (a) or attestation (b) after a failed source
/// check (H.48.1), and the operator's supported procedure or
/// `restore_journal_lost`.
fn raw_candidates() -> Vec<Action> {
    let mut c = shared_candidates();
    c.extend([
        RawRestore,
        RestoreBegin,
        SourceCheckFails,
        AttestOldStopped,
        OpRestore,
        OpRestoreJournalLost,
    ]);
    for p in 0..PEERS as u8 {
        c.push(PeerOldList(p));
    }
    c
}

/// Peer 0 is enrolled and will never report; the authority is backed up.
fn backed_up(design: Design, bad_journal: bool) -> Fake {
    let mut f = Fake::new(two_holders(bad_journal), design);
    f.run(&[PeerEnroll(0), PeerSilent(0), AuthBackup]);
    f
}

// ------------------------------------------------------------------ H.41

/// The H.41 counterexample shape: after the backup the old lineage advances
/// `holder_rev` once, and peer 0 receives page 0 of a list under it. The
/// supported restore rolls the store back to the backup and its own H.41
/// advance brings `holder_rev` to the very value the cursor carries.
fn cursor_across_restore(design: Design) -> Fake {
    let mut f = backed_up(design, false);
    f.run(&[AuthTouch, PeerListStart(0)]);
    let sealed = f.s.peers[0].cursor.expect("a cursor in flight");
    assert_eq!(sealed.0, 0, "sealed in epoch 0");
    f.run(&[RestoreBegin, AuthRestore]);
    assert_eq!(f.s.auth.core.epoch, 1);
    assert_eq!(
        f.s.auth.core.holder_rev, sealed.1,
        "the restored holder_rev equals the cursor's"
    );
    f
}

/// H.41: a cursor whose sealed `rev.epoch` differs from the store's epoch
/// answers `410 list_expired`, whatever `holder_rev` says. Here `holder_rev`
/// matches, so only the epoch check refuses it; the peer keeps the lease it
/// had before `T_stop`, which lapses at the lease timeout. Without the check
/// an epoch-0 page is spliced after `T_stop`: the lease it renews outlives
/// the lease timeout, and once the member clears by lease the holder
/// answers at 1051 s (I1).
#[test]
fn h41_a_cursor_from_another_epoch_answers_410_whatever_holder_rev_says() {
    let mut f = cursor_across_restore(D);
    let out = f.step(PeerListPage(0));
    assert!(out.rows.iter().any(|r| r == RowId::H21));
    assert!(!out.obs.contains(&Some(Obs::EarlierEpochListAfterStop)));
    assert!(!f.s.peers[0].late && f.s.peers[0].cursor.is_none());
    f.holder_reports();
    f.step(LeaseTimeout);
    assert_eq!(f.s.peers[0].lease, None);
    assert!(mintable(&f.s, 0));
    assert!(!f.enabled(PeerAnswer(0)));

    let without = D.without(Fix::CrossEpochCursor);
    let mut f = cursor_across_restore(without);
    let out = f.step(PeerListPage(0));
    assert!(
        out.obs.contains(&Some(Obs::EarlierEpochListAfterStop)),
        "an epoch-0 page is spliced after T_stop"
    );
    assert!(f.s.peers[0].late);
    f.holder_reports();
    f.step(LeaseTimeout);
    assert_eq!(f.s.peers[0].lease, Some(0), "renewed after T_stop");
    assert_eq!(
        first_violation(without, &f.s, &[PeerAnswer(0)]),
        Some(Prop::I1EpochExposure)
    );

    let cex = ablate(
        Fix::CrossEpochCursor,
        Prop::I1EpochExposure,
        gate_profile("h41-cross-epoch-cursor"),
        two_holders(false),
        &cursor_candidates(),
    );
    let restored = cex
        .prefix
        .iter()
        .position(|t| t.action == AuthRestore)
        .expect("a restore");
    assert!(
        cex.prefix[restored..]
            .iter()
            .any(|t| matches!(t.action, PeerListPage(_))),
        "an earlier-epoch page is spliced after T_stop"
    );
}

/// H.41: the epoch-minting transaction of a restore advances every
/// `holder_rev` in that transaction, as an operator epoch's does (H.33.1).
/// The two H.41 rules are independent: the epoch check alone closes every
/// case the model explores, so removing only the advance holds; the advance
/// alone misses a cursor sealed after an old-lineage advance (above);
/// removing both splices a page even without that advance. The same holds
/// for the operator epoch: with the epoch check, H.33.1's advance is no
/// longer load-bearing on its own.
#[test]
fn h41_every_epoch_minting_transaction_advances_holder_rev() {
    let mut f = backed_up(D, false);
    let backup_rev = f.s.auth.core.holder_rev;
    f.run(&[RestoreBegin, AuthRestore]);
    assert!(
        f.s.auth.core.holder_rev > backup_rev,
        "advanced in the epoch"
    );

    let neither = D
        .without(Fix::CrossEpochCursor)
        .without(Fix::EpochRevBumpEveryCause);
    let mut f = backed_up(neither, false);
    f.run(&[PeerListStart(0), RestoreBegin, AuthRestore]);
    let out = f.step(PeerListPage(0));
    assert!(out.obs.contains(&Some(Obs::EarlierEpochListAfterStop)));
    f.holder_reports();
    f.step(LeaseTimeout);
    assert_eq!(
        first_violation(neither, &f.s, &[PeerAnswer(0)]),
        Some(Prop::I1EpochExposure)
    );

    let p = gate_profile("h41-advance");
    let only_check = check_from(
        &Profile {
            design: D.without(Fix::EpochRevBumpEveryCause),
            ..p
        },
        two_holders(false),
        cursor_candidates(),
    );
    eprintln!("{}", only_check.render());
    assert!(only_check.violation.is_none(), "{}", only_check.render());
    let no_rule: Vec<Action> = cursor_candidates()
        .into_iter()
        .filter(|a| *a != AuthTouch)
        .collect();
    let both = check_from(
        &Profile {
            design: neither,
            ..p
        },
        two_holders(false),
        no_rule,
    );
    eprintln!("{}", both.render());
    assert_eq!(
        both.violation.as_ref().map(|v| v.prop),
        Some(Prop::I1EpochExposure),
        "{}",
        both.render()
    );

    // The operator epoch (H.33.1): the epoch check alone suffices too.
    let mut op = cursor_candidates();
    op.retain(|a| {
        !matches!(
            a,
            AuthBackup | AuthTouch | RestoreBegin | AuthRestore | OpRestoreJournalLost
        )
    });
    op.push(OpEpoch);
    let check_only = check_from(
        &Profile {
            design: D.without(Fix::OperatorEpochRevBump),
            ..p
        },
        two_holders(false),
        op.clone(),
    );
    eprintln!("{}", check_only.render());
    assert!(check_only.violation.is_none(), "{}", check_only.render());
    let neither_op = check_from(
        &Profile {
            design: D
                .without(Fix::OperatorEpochRevBump)
                .without(Fix::CrossEpochCursor),
            ..p
        },
        two_holders(false),
        op,
    );
    assert_eq!(
        neither_op.violation.as_ref().map(|v| v.prop),
        Some(Prop::I1EpochExposure),
        "{}",
        neither_op.render()
    );
}

/// H.41 exhaustively over the restore-cursor fixture, both causes: I1 (90 s)
/// and L1 hold, and the cursor row is exercised.
#[test]
fn h41_restore_cursors_hold_exposure_and_always_resume() {
    for lost in [false, true] {
        let r = check_from(
            &gate_profile(if lost {
                "h41-cursor-journal-lost"
            } else {
                "h41-cursor-supported"
            }),
            two_holders(lost),
            cursor_candidates(),
        );
        eprintln!("{}", r.render());
        assert!(r.violation.is_none(), "{}", r.render());
        for row in [RowId::H14, RowId::H15, RowId::H16, RowId::H19, RowId::H21] {
            assert!(r.rows_seen[row.index()], "{}", row.name());
        }
    }
}

// ------------------------------------------------------------------ H.43

/// H.43: `t_r` that cannot be established from the current lineage's
/// durable state leaves the holder a member. Peer 0 is disabled before the
/// backup and its refusal ages after it. A supported restore replays the
/// journaled `refused_since` and excludes it; `restore_journal_lost` has no
/// journal, so the backup's entry stands without `t_r` and the holder is a
/// member, which can only lengthen the gate up to its cap.
#[test]
fn h43_t_r_lost_in_a_restore_leaves_the_holder_a_member() {
    for lost in [false, true] {
        let mut f = Fake::new(two_holders(lost), D);
        f.run(&[
            PeerEnroll(0),
            PeerDisable(0),
            PeerSilent(0),
            AuthBackup,
            PeerRefusalAged(0),
        ]);
        assert!(f.s.peers[0].roster.is_some_and(|r| r.refused_aged));
        assert!(f.s.peers[0].backup.is_some_and(|r| !r.refused_aged));
        f.run(&[RestoreBegin, AuthRestore]);
        if lost {
            f.step(OpRestoreJournalLost);
            assert_eq!(f.s.peers[0].seat, Seat::Pending, "no t_r: a member");
        } else {
            assert_eq!(f.s.peers[0].seat, Seat::Absent, "t_r replayed");
        }
    }
}

// ------------------------------------------------------------------ H.44

/// H.44.1–3: a raw restore's first start is suspected (H20). It issues no
/// list and no access grant, records no `T_stop`, and cannot leave
/// suspicion by itself. The audited attestation (b) is the confirmation:
/// an unsampled security event, `T_stop` its instant, and the old instance
/// stopped. The operator's supported procedure then mints the epoch, and
/// minting resumes once the members clear.
#[test]
fn h44_a_raw_restore_starts_suspected_and_leaves_only_through_a_confirmation() {
    let mut f = backed_up(D, false);
    assert!(f.rows(RawRestore).contains(&RowId::H20));
    assert!(f.s.auth.rollback_suspected && f.s.env.old_running);
    assert_eq!(
        f.s.auth.lists_stopped_at, None,
        "no T_stop at the first start"
    );
    assert!(!mintable(&f.s, 0));
    assert!(!f.enabled(OpRestore), "no self-confirmation");
    assert!(!f.enabled(LeaseTimeout));
    assert!(!f.enabled(PeerOldList(0)), "nothing is issued after T_stop");
    assert!(props::expected_non_progress(&f.s));
    assert!(f.s.ghost.search_due && props::goal(&f.s));

    f.run(&[TickExposure(91), SourceCheckFails]);
    let out = f.step(AttestOldStopped);
    assert!(out.rows.iter().any(|r| r == RowId::H19));
    assert!(out.obs.contains(&Some(Obs::OldInstanceStoppedAttested)));
    assert_eq!(
        f.s.auth.lists_stopped_at,
        Some(91),
        "T_stop: the confirmation"
    );
    assert!(!f.s.env.old_running && f.s.auth.rollback_suspected);
    assert!(!props::expected_non_progress(&f.s) && !props::goal(&f.s));
    assert!(!f.enabled(PeerOldList(0)));
    let rows = f.rows(OpRestore);
    assert!(rows.contains(&RowId::H14));
    assert!(!f.s.auth.rollback_suspected && f.s.auth.raw == Raw::No);
    f.holder_reports();
    assert!(!mintable(&f.s, 0), "the silent peer is pending");
    f.step(LeaseTimeout);
    assert!(f.s.env.exposure_elapsed >= 91 + 1051);
    assert!(mintable(&f.s, 0) && props::goal(&f.s));

    // (a) the tooling verifies the source; a corrupt journal ends in H12j
    // and the audited `restore_journal_lost` decision.
    let mut f = backed_up(D, true);
    f.run(&[RawRestore, TickExposure(60)]);
    assert!(f.rows(RestoreBegin).contains(&RowId::H19));
    assert_eq!(f.s.auth.lists_stopped_at, Some(60));
    assert!(f.rows(OpRestore).contains(&RowId::H12j));
    assert!(f.rows(OpRestoreJournalLost).contains(&RowId::H13j));
    assert_eq!(f.s.auth.core.epoch, 1);
}

/// H.44.3 ablation: a raw-restored instance that leaves suspicion without
/// the confirmation, with the old instance still running. The old instance
/// renews an earlier-epoch list after `T_stop`, the lease timeout clears the
/// member while that lease is live, and the holder answers at 1051 s, far
/// past 90 s (I1).
#[test]
fn h44_leaving_suspicion_without_a_confirmation_exposes_past_90_s() {
    let without = D.without(Fix::NoSelfConfirmation);
    let mut f = backed_up(without, false);
    f.run(&[RawRestore, OpRestore]);
    assert!(f.s.env.old_running, "the old instance still serves");
    let out = f.step(PeerOldList(0));
    assert!(out.obs.contains(&Some(Obs::EarlierEpochListAfterStop)));
    f.holder_reports();
    f.step(LeaseTimeout);
    assert!(f.s.env.exposure_elapsed > 90);
    assert_eq!(
        first_violation(without, &f.s, &[PeerAnswer(0)]),
        Some(Prop::I1EpochExposure)
    );
    let cex = ablate(
        Fix::NoSelfConfirmation,
        Prop::I1EpochExposure,
        gate_profile("h44-confirmation"),
        two_holders(false),
        &raw_candidates(),
    );
    assert!(
        !cex.prefix
            .iter()
            .any(|t| matches!(t.action, RestoreBegin | AttestOldStopped)),
        "no confirmation"
    );
}

/// H.44.4 ablation: `T_stop` taken as the raw-restored instance's first
/// start. The old instance keeps issuing lists until the confirmation at
/// 91 s, so the lease timeout counted from 0 s comes before a lease from such
/// a list has lapsed: the gate times out early, and the holder answers from
/// its earlier-epoch list (I1). Counted from the confirmation, it does not.
#[test]
fn h44_t_stop_is_the_confirmation_not_the_first_start() {
    let without = D.without(Fix::TStopAtConfirmation);
    let mut f = backed_up(without, false);
    f.step(RawRestore);
    assert_eq!(f.s.auth.lists_stopped_at, Some(0), "the first start");
    f.run(&[TickExposure(60), PeerOldList(0), TickExposure(91)]);
    assert!(f.s.peers[0].late, "issued after that T_stop");
    f.run(&[SourceCheckFails, AttestOldStopped, OpRestore]);
    f.holder_reports();
    let out = f.step(LeaseTimeout);
    assert!(out.obs.contains(&Some(Obs::MintGateClearedByLease {
        holder: false,
        peers: 1
    })));
    assert_eq!(
        f.s.env.exposure_elapsed, 1051,
        "timed out from the first start"
    );
    assert_eq!(
        first_violation(without, &f.s, &[PeerAnswer(0)]),
        Some(Prop::I1EpochExposure)
    );
    ablate(
        Fix::TStopAtConfirmation,
        Prop::I1EpochExposure,
        gate_profile("h44-t-stop"),
        two_holders(false),
        &raw_candidates(),
    );

    // The tooling path of `restore_journal_lost` records `T_stop` at the
    // confirmation too, not at the first start (revision 11.3's reading).
    let mut f = backed_up(D, true);
    f.step(RestoreBegin);
    assert_eq!(f.s.auth.lists_stopped_at, Some(0));
    f.step(TickExposure(60));
    assert!(f.rows(AuthRestore).contains(&RowId::H12j));
    assert_eq!(
        f.s.auth.lists_stopped_at,
        Some(0),
        "unchanged by the first start"
    );
}

/// H.44 exhaustively over the raw-restore fixture, both causes: I1 (90 s)
/// holds, and L1 holds: once a confirmation exists minting resumes, and
/// without one staying suspected is expected non-progress.
#[test]
fn h44_raw_restore_holds_exposure_and_resumes_once_confirmed() {
    for lost in [false, true] {
        let r = check_from(
            &gate_profile(if lost {
                "h44-raw-journal-lost"
            } else {
                "h44-raw-supported"
            }),
            two_holders(lost),
            raw_candidates(),
        );
        eprintln!("{}", r.render());
        assert!(r.violation.is_none(), "{}", r.render());
        for row in [RowId::H14, RowId::H15, RowId::H16, RowId::H19, RowId::H20] {
            assert!(r.rows_seen[row.index()], "{}", row.name());
        }
        if lost {
            assert!(r.rows_seen[RowId::H12j.index()] && r.rows_seen[RowId::H13j.index()]);
        }
    }
}

/// The peer-symmetry quotient keeps the verdicts and the rows seen in the
/// new fixtures: with every fix and for each H.41 and H.44 ablation.
#[test]
fn h41_h44_peer_symmetry_preserves_verdicts() {
    let raw = |symmetry| {
        let mut s = two_holders(false);
        s.env.peer_symmetry = symmetry;
        s
    };
    for (candidates, fix) in [
        (raw_candidates(), None),
        (raw_candidates(), Some(Fix::NoSelfConfirmation)),
        (raw_candidates(), Some(Fix::TStopAtConfirmation)),
        (cursor_candidates(), Some(Fix::CrossEpochCursor)),
        (cursor_candidates(), None),
    ] {
        let p = Profile {
            design: fix.map_or(D, |x| D.without(x)),
            ..gate_profile("h41-h44-symmetry")
        };
        let full = check_from(&p, raw(false), candidates.clone());
        let reduced = check_from(&p, raw(true), candidates);
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

/// H.44.3: without a confirmation the authority stays suspected
/// indefinitely, and that is the correct outcome. With no confirmation step
/// in the fixture, I1 and L1 hold because the goal counts the suspected wait
/// as expected non-progress; scored against the plain goal, the same graph
/// is an L1 failure, so the wait is real and the encoding is what accepts it.
#[test]
fn h44_without_a_confirmation_staying_suspected_is_expected_non_progress() {
    let unconfirmed: Vec<Action> = raw_candidates()
        .into_iter()
        .filter(|a| !matches!(a, RestoreBegin | AttestOldStopped))
        .collect();
    let p = gate_profile("h44-never-confirmed");
    let r = check_from(&p, two_holders(false), unconfirmed.clone());
    eprintln!("{}", r.render());
    assert!(r.violation.is_none(), "{}", r.render());
    assert!(r.rows_seen[RowId::H20.index()] && !r.rows_seen[RowId::H14.index()]);
    fn plain(s: &State) -> bool {
        !s.ghost.status_due && !s.ghost.search_due && props::retirements_done(s)
    }
    let strict = check_from_with_goal(&p, two_holders(false), unconfirmed, plain);
    assert_eq!(
        strict.violation.as_ref().map(|v| v.prop),
        Some(Prop::L1Liveness),
        "{}",
        strict.render()
    );
}
