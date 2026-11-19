//! HOLDINGS-WIRE-v2 H.46–H.49 (design revision 11.5).
//!
//! - H.46 confirms the lease pair the model already uses; a test pins it.
//! - H.47 keeps both H.41 cursor controls. The tests exercise each one on
//!   its own: ablating either alone holds, ablating both splices a page, the
//!   cross-epoch check alone covers a restored `holder_rev` that coincides
//!   with a cursor's, and within one epoch `holder_rev` is the only control.
//! - H.48 and H.49 are model fixes: removing one reproduces the
//!   counterexample it closes, and the same exploration holds with it. The
//!   attestation fixtures include a false attestation, so the old instance
//!   may keep issuing lists and minting after the exit; what remains with
//!   every fix is the named residual, which the tests show is reachable.

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
const D: Design = Design::REV11_5;

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

fn observed(out: &Outcome, o: Obs) -> bool {
    out.obs.contains(&Some(o))
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

/// What every gate fixture shares (as in `tests/rev11_4.rs`).
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

/// H.41, H.47: a supported restore or `restore_journal_lost` with the other
/// holders' list cursors in flight across it. `touch`: the old lineage may
/// advance `holder_rev` once after the backup, so the restored value can
/// coincide with a cursor's.
fn cursor_candidates(touch: bool) -> Vec<Action> {
    let mut c = shared_candidates();
    c.extend([RestoreBegin, AuthRestore, OpRestoreJournalLost]);
    if touch {
        c.push(AuthTouch);
    }
    for p in 0..PEERS as u8 {
        c.extend([PeerListStart(p), PeerListPage(p)]);
    }
    c
}

/// H.33.1, H.47: an operator epoch with the other holders' cursors in flight.
fn operator_candidates() -> Vec<Action> {
    let mut c = cursor_candidates(false);
    c.retain(|a| {
        !matches!(
            a,
            AuthBackup | RestoreBegin | AuthRestore | OpRestoreJournalLost
        )
    });
    c.push(OpEpoch);
    c
}

/// H.48.1: a raw restore whose confirmation is by tooling or by a true
/// attestation, with failed source checks that may age past 1 h.
fn precondition_candidates() -> Vec<Action> {
    let mut c = shared_candidates();
    c.extend([
        RawRestore,
        RestoreBegin,
        SourceCheckFails,
        SourceCheckAges,
        AttestOldStopped,
        OpRestore,
        OpRestoreJournalLost,
    ]);
    for p in 0..PEERS as u8 {
        c.push(PeerOldList(p));
    }
    c
}

/// H.48.3, H.49.3: a false attestation, after a failed source check, in a
/// raw restore or a supported one. The old instance keeps issuing lists and
/// minting after the exit; the other holders may fetch a new-epoch list and
/// wait for `accept-epoch` while they still serve their earlier list.
fn false_candidates() -> Vec<Action> {
    let mut c = shared_candidates();
    c.extend([
        RawRestore,
        SourceCheckFails,
        FalseAttestation,
        AuthRestore,
        OpRestore,
        OpRestoreJournalLost,
    ]);
    for p in 0..PEERS as u8 {
        c.extend([PeerOldList(p), PeerSeesEpoch(p), PeerOldCapability(p)]);
    }
    c
}

/// Peer 0 is enrolled; the authority is backed up.
fn backed_up(design: Design, bad_journal: bool) -> Fake {
    let mut f = Fake::new(two_holders(bad_journal), design);
    f.run(&[PeerEnroll(0), AuthBackup]);
    f
}

/// The every-cause `holder_rev` advance of H.41: the restore's and the
/// operator epoch's (H.33.1).
fn without_advance(d: Design) -> Design {
    d.without(Fix::EpochRevBumpEveryCause)
        .without(Fix::OperatorEpochRevBump)
}

// ------------------------------------------------------------------ H.46

/// H.46: the monotonic deadline of every item of a list is `t_recv(page 0)
/// + (quarantine_after − iat(page 0))`. The model delivers a list in one
/// step, so that step is page 0's receipt, and the deadline is fixed there
/// (`admitted_until`). A4 carries it unchanged, however late the apply
/// runs: a later anchor is what H.46 forbids, and it lets the holder serve
/// past its lease (the Astra lease fence, `tests/rev10.rs`). For the other
/// holders, a page received before `T_stop` renews nothing the lease timeout
/// does not bound, because that page's list began (page 0) before `T_stop`.
#[test]
fn h46_the_lease_deadline_pairs_page_0s_receipt_and_iat() {
    let mut f = Fake::new(State::initial(NONE, false), D);
    f.s.env.probe = Probe::Lease;
    f.boot();
    f.run(&[FetchStart, ListDeliver]);
    let received = f.s.env.clock;
    assert_eq!(
        f.s.vol.admitted_until,
        received + 1,
        "from page 0's receipt"
    );
    f.step(Apply);
    let at_a4 = f.s.clone();
    f.run(&[LeaseDeadline, Apply]);
    assert!(f.s.env.clock > received);
    assert_eq!(f.s.vol.admitted_until, received + 1, "never re-anchored");
    assert_eq!(f.s.vol.lease_until, f.s.vol.admitted_until);
    assert!(!servable(&f.s, 0), "the lease lapsed at page 0's deadline");
    assert_eq!(
        first_violation(
            D.without(Fix::LeaseAndRollbackFence),
            &at_a4,
            &[LeaseDeadline, Apply]
        ),
        Some(Prop::I1Lease),
        "a later anchor serves past the lease"
    );

    // Another holder: page 0 before `T_stop`, the last page before it too.
    let mut f = Fake::new(two_holders(false), D);
    f.run(&[PeerEnroll(0), PeerListStart(0), PeerListPage(0)]);
    assert_eq!(f.s.peers[0].lease, Some(0));
    assert!(!f.s.peers[0].late);
    f.run(&[OpEpoch, LeaseTimeout]);
    assert_eq!(f.s.peers[0].lease, None, "bounded by the lease timeout");
}

// ------------------------------------------------------------------ H.47

/// Peer 0's cursor crosses a supported restore and is presented to the
/// restored authority. `touch`: the old lineage advanced `holder_rev` once
/// after the backup, before the cursor was sealed.
fn page_across_restore(design: Design, touch: bool) -> Outcome {
    let mut f = backed_up(design, false);
    if touch {
        f.step(AuthTouch);
    }
    f.run(&[PeerListStart(0), RestoreBegin, AuthRestore]);
    exec(&f.s, PeerListPage(0), &f.cfg).unwrap().unwrap()
}

/// H.47: each control refuses an old-lineage cursor on its own, and the
/// observation names which did. Without an old-lineage advance after the
/// backup, either control alone suffices; ablating both splices the page.
/// With one, the restored `holder_rev` coincides with the cursor's, so only
/// the cross-epoch check refuses: that is why it is the normative guarantee.
#[test]
fn h47_each_cursor_control_refuses_on_its_own() {
    let expired = |epoch, holder_rev| Obs::ListExpired { epoch, holder_rev };
    let both = page_across_restore(D, false);
    assert!(observed(&both, expired(true, true)));
    let check_only = page_across_restore(without_advance(D), false);
    assert!(observed(&check_only, expired(true, false)));
    let advance_only = page_across_restore(D.without(Fix::CrossEpochCursor), false);
    assert!(observed(&advance_only, expired(false, true)));
    let neither = page_across_restore(without_advance(D).without(Fix::CrossEpochCursor), false);
    assert!(observed(&neither, Obs::EarlierEpochListAfterStop));

    let coincide = page_across_restore(D, true);
    assert!(
        observed(&coincide, expired(true, false)),
        "only the epoch check"
    );
    let advance_only = page_across_restore(D.without(Fix::CrossEpochCursor), true);
    assert!(observed(&advance_only, Obs::EarlierEpochListAfterStop));
}

/// H.47 exhaustively: for each epoch cause (a supported restore,
/// `restore_journal_lost`, an operator epoch) with no old-lineage advance,
/// every fix holds, ablating the cross-epoch check alone holds, ablating the
/// every-cause advance alone holds, and ablating both splices an
/// earlier-epoch page after `T_stop` (I1). `restore_journal_lost` is the
/// exception to the last: its H.19.5 pin reset nulls the active pins in a
/// transaction of its own, which advances `holder_rev` too, so it masks the
/// splice; removing that reset as well reproduces it. Neither H.41 control
/// is what refuses the cursor there. With an old-lineage advance the
/// advance alone does not hold (`tests/rev11_4.rs`) and the check alone does.
#[test]
fn h47_ablating_either_control_alone_holds_and_both_splices() {
    let p = gate_profile("h47-controls");
    let neither = without_advance(D).without(Fix::CrossEpochCursor);
    let fixtures = [
        (
            "supported",
            two_holders(false),
            cursor_candidates(false),
            false,
        ),
        (
            "journal-lost",
            two_holders(true),
            cursor_candidates(false),
            true,
        ),
        ("operator", two_holders(false), operator_candidates(), false),
    ];
    for (name, s, candidates, masked) in fixtures {
        let mut designs = vec![
            (D, true),
            (D.without(Fix::CrossEpochCursor), true),
            (without_advance(D), true),
            (neither, masked),
        ];
        if masked {
            designs.push((neither.without(Fix::JournalLostPinReset), false));
        }
        for (design, holds) in designs {
            let r = check_from(&Profile { design, ..p }, s.clone(), candidates.clone());
            eprintln!("{name} {design:?}: {}", r.render());
            if holds {
                assert!(r.violation.is_none(), "{name}: {}", r.render());
            } else {
                assert_eq!(
                    r.violation.as_ref().map(|v| v.prop),
                    Some(Prop::I1EpochExposure),
                    "{name}: {}",
                    r.render()
                );
            }
        }
    }
    let r = check_from(
        &Profile {
            design: without_advance(D),
            ..p
        },
        two_holders(false),
        cursor_candidates(true),
    );
    assert!(r.violation.is_none(), "{}", r.render());
}

/// H.47: within one epoch, `holder_rev` is the only control. A cursor sealed
/// before an ordinary holder-affecting write answers `410` by `holder_rev`
/// alone (§3.7); the cross-epoch check cannot refuse it, and neither the
/// check nor the epoch advance plays a part.
#[test]
fn h47_holder_rev_alone_refuses_within_an_epoch() {
    for design in [D, D.without(Fix::CrossEpochCursor), without_advance(D)] {
        let mut f = backed_up(design, false);
        f.run(&[PeerListStart(0), AuthTouch]);
        assert_eq!(f.s.auth.core.epoch, 0);
        let out = f.step(PeerListPage(0));
        assert!(observed(
            &out,
            Obs::ListExpired {
                epoch: false,
                holder_rev: true
            }
        ));
        assert!(f.s.peers[0].cursor.is_none(), "the list is discarded");
    }
}

// ------------------------------------------------------------------ H.48

/// H.48.1: the authority accepts `old_instance_stopped` only in a restore
/// session whose tooling recorded a failed source check no older than 1 h
/// (H22). Otherwise `attestation_precondition_unmet`, and nothing changes.
/// The tooling's own verification needs no such record.
#[test]
fn h48_an_attestation_needs_a_fresh_failed_source_check() {
    let mut f = backed_up(D, false);
    f.step(RawRestore);
    let before = f.s.clone();
    let out = f.step(AttestOldStopped);
    assert!(out.rows.iter().any(|r| r == RowId::H22));
    assert!(observed(&out, Obs::AttestationPreconditionUnmet));
    assert_eq!(f.s, before, "refused: nothing changes");
    assert!(observed(
        &f.step(FalseAttestation),
        Obs::AttestationPreconditionUnmet
    ));

    f.run(&[SourceCheckFails, SourceCheckAges]);
    assert!(observed(
        &f.step(AttestOldStopped),
        Obs::AttestationPreconditionUnmet
    ));
    assert_eq!(f.s.auth.raw, Raw::Unconfirmed, "older than 1 h");

    f.step(SourceCheckFails);
    let out = f.step(AttestOldStopped);
    assert!(observed(&out, Obs::OldInstanceStoppedAttested));
    assert_eq!(f.s.auth.raw, Raw::Confirmed);
    assert!(f.rows(OpRestore).contains(&RowId::H23));
    assert_eq!(
        f.s.auth.source_check,
        SourceCheck::None,
        "the session ended"
    );

    // The tooling verifies without any record.
    let mut f = backed_up(D, false);
    f.run(&[RawRestore, RestoreBegin]);
    assert_eq!(f.s.auth.raw, Raw::Confirmed);
    assert!(
        !f.rows(OpRestore).contains(&RowId::H23),
        "no attestation exit"
    );
}

/// H.48.1 ablation: without the precondition an attestation is accepted
/// while no failed source check is recorded, so the tooling could still have
/// verified, and the instance exits suspicion on it (I8).
#[test]
fn h48_without_the_precondition_an_attestation_exits_while_the_tooling_could_verify() {
    let without = D.without(Fix::AttestationPrecondition);
    let mut f = backed_up(without, false);
    f.step(RawRestore);
    assert_eq!(
        first_violation(without, &f.s, &[AttestOldStopped, OpRestore]),
        Some(Prop::I8AttestationPrecondition)
    );
    let out = exec(&f.s, AttestOldStopped, &f.cfg).unwrap().unwrap();
    assert!(observed(&out, Obs::AttestedWithoutFailedCheck));
    f.s = out.next;
    f.step(OpRestore);
    assert_eq!(f.s.auth.core.epoch, 1, "it exits suspicion");

    let cex = ablate(
        Fix::AttestationPrecondition,
        Prop::I8AttestationPrecondition,
        gate_profile("h48-precondition"),
        two_holders(false),
        &precondition_candidates(),
    );
    assert!(
        !cex.prefix.iter().any(|t| t.action == SourceCheckFails),
        "no failed check was recorded"
    );
}

/// A false attestation in a raw restore after a failed source check, the
/// exit, and peer 0 still on its epoch-0 list.
fn false_exit(design: Design) -> Fake {
    let mut f = backed_up(design, false);
    f.run(&[RawRestore, SourceCheckFails, FalseAttestation]);
    assert!(f.s.env.old_running, "the attestation is false");
    f.step(OpRestore);
    assert_eq!(f.s.auth.attest, Attest::Exited);
    f
}

/// H.48.3: the first transaction after the exit issues the key-set statement
/// (H23); a holder applies it before it verifies any new-epoch list (H24)
/// and then refuses the old instance's lists. Without the rotation, a list
/// the old instance signed verifies at a holder that already applied the
/// new epoch's statement: its earlier-epoch lease outlives the lease timeout,
/// and once it clears by lease it answers at 1051 s (I1).
#[test]
fn h48_the_lc_rotation_refuses_old_instance_lists_after_the_holder_applies_it() {
    let mut f = false_exit(D);
    assert!(f.rows(PeerSeesEpoch(0)).contains(&RowId::H24));
    assert!(!f.enabled(PeerOldList(0)), "a retired kid");
    f.run(&[PeerSilent(0)]);
    f.holder_reports();
    f.step(LeaseTimeout);
    assert_eq!(f.s.peers[0].lease, None);
    assert!(mintable(&f.s, 0) && !f.enabled(PeerAnswer(0)));

    let without = D.without(Fix::LifecycleKeyRotation);
    let mut f = false_exit(without);
    f.step(PeerSeesEpoch(0));
    let out = f.step(PeerOldList(0));
    assert!(
        observed(&out, Obs::EarlierEpochListAfterStop),
        "it verifies"
    );
    f.run(&[PeerSilent(0)]);
    f.holder_reports();
    f.step(LeaseTimeout);
    assert_eq!(f.s.peers[0].lease, Some(0));
    assert_eq!(
        first_violation(without, &f.s, &[PeerAnswer(0)]),
        Some(Prop::I1EpochExposure)
    );

    let cex = ablate(
        Fix::LifecycleKeyRotation,
        Prop::I1EpochExposure,
        gate_profile("h48-lc-rotation"),
        two_holders(false),
        &false_candidates(),
    );
    let seen = cex
        .prefix
        .iter()
        .position(|t| matches!(t.action, PeerSeesEpoch(_)))
        .expect("a holder applied the statement");
    assert!(
        cex.prefix[seen..]
            .iter()
            .any(|t| matches!(t.action, PeerOldList(_))),
        "an old-instance list verifies after that"
    );
}

/// H.44.4, H.48.3, H.49.4: with every fix, a false attestation still leaves
/// a residual, and it is exactly the named one. A holder that has not
/// applied the statement can't tell the old instance's lists from earlier
/// ones: its lease outlives the lease timeout, and it answers both
/// instances' capabilities. Neither is a violation; it is recorded.
#[test]
fn h48_a_false_attestation_leaves_only_the_named_residual() {
    let mut f = false_exit(D);
    let out = f.step(PeerOldList(0));
    assert!(observed(&out, Obs::AttestationResidual));
    assert!(f.s.peers[0].residual && !f.s.peers[0].late);
    f.run(&[PeerSilent(0)]);
    f.holder_reports();
    f.step(LeaseTimeout);
    assert_eq!(f.s.peers[0].lease, Some(0), "not bounded by the timeout");
    assert!(mintable(&f.s, 0));
    assert!(observed(&f.step(PeerAnswer(0)), Obs::AttestationResidual));
    assert!(observed(
        &f.step(PeerOldCapability(0)),
        Obs::AttestationResidual
    ));
}

// ------------------------------------------------------------------ H.49

/// H.49.3: an exit by attestation rotates the search-capability key (H23),
/// and an engine applies the rotation before it reports in the new epoch
/// (H24). A member that cleared by report refuses the old instance's
/// capabilities. Without the rotation it answers them after the exit; that
/// instance mints for rooms the restored lineage retired, so the exposure
/// outlasts the H.49.2 bound (I1).
#[test]
fn h49_the_capability_rotation_refuses_old_instance_capabilities() {
    let mut f = false_exit(D);
    f.run(&[PeerMove(0), PeerReport(0)]);
    assert!(f.s.peers[0].cap_rotated);
    assert!(!f.enabled(PeerOldCapability(0)));
    f.holder_reports();
    assert!(mintable(&f.s, 0) && !f.enabled(PeerOldCapability(0)));

    let without = D.without(Fix::CapabilityKeyRotation);
    let mut f = false_exit(without);
    f.run(&[PeerMove(0), PeerReport(0)]);
    assert_eq!(
        first_violation(without, &f.s, &[PeerOldCapability(0)]),
        Some(Prop::I1CapabilityExposure)
    );
    ablate(
        Fix::CapabilityKeyRotation,
        Prop::I1CapabilityExposure,
        gate_profile("h49-capability-rotation"),
        two_holders(false),
        &false_candidates(),
    );
}

/// H.49.3 as written requires the rotation only before the engine's first
/// new-epoch report. A member that applied a new-epoch list and never
/// reported clears by the lease timeout while it still holds that list's
/// lease, so "clearing by lease lapse serves nothing" does not hold for it.
/// It has not applied the rotation and answers the old instance's
/// capabilities after minting resumes (I1). Kwiry's reading applies the
/// rotation before the first new-epoch list.
#[test]
fn h49_the_rotation_precedes_the_first_new_epoch_list() {
    let mut f = false_exit(D);
    f.run(&[PeerMove(0), PeerSilent(0)]);
    assert!(f.s.peers[0].cap_rotated, "applied with the list");

    let text = D.without(Fix::CapabilityRotationBeforeList);
    let mut f = false_exit(text);
    f.run(&[PeerMove(0), PeerSilent(0)]);
    assert!(!f.s.peers[0].cap_rotated);
    f.holder_reports();
    let out = f.step(LeaseTimeout);
    assert!(observed(
        &out,
        Obs::MintGateClearedByLease {
            holder: false,
            peers: 1
        }
    ));
    assert_eq!(f.s.peers[0].lease, Some(1), "it still serves");
    assert_eq!(
        first_violation(text, &f.s, &[PeerOldCapability(0)]),
        Some(Prop::I1CapabilityExposure)
    );
    let cex = ablate(
        Fix::CapabilityRotationBeforeList,
        Prop::I1CapabilityExposure,
        gate_profile("h49-before-list"),
        two_holders(false),
        &false_candidates(),
    );
    assert!(
        cex.prefix.iter().any(|t| t.action == LeaseTimeout),
        "cleared by lease, not by report"
    );
}

/// H.49.2, H.49.5: with a true confirmation the old instance has stopped, so
/// none of its capabilities exist after the exit, the 90 s bound counts from
/// the later of the retirement and `T_stop`, and a tooling exit rotates
/// nothing.
#[test]
fn h49_a_true_confirmation_needs_no_rotation() {
    for attest in [true, false] {
        let mut f = backed_up(D, false);
        f.step(RawRestore);
        if attest {
            f.run(&[SourceCheckFails, AttestOldStopped]);
        } else {
            f.step(RestoreBegin);
        }
        assert!(!f.s.env.old_running);
        f.step(OpRestore);
        f.step(PeerMove(0));
        assert!(!f.s.peers[0].lc_rotated && !f.s.peers[0].cap_rotated);
        assert!(!f.enabled(PeerOldCapability(0)) && !f.enabled(PeerOldList(0)));
    }
}

// --------------------------------------------------- exhaustive fixtures

/// H.48 and H.49 exhaustively, both causes: the precondition fixture (true
/// attestations, aging checks) and the false-attestation fixture hold I1,
/// I8 and L1, and exercise the new rows.
#[test]
fn h48_h49_attestation_fixtures_hold_and_always_resume() {
    for lost in [false, true] {
        for (name, candidates, rows) in [
            (
                "precondition",
                precondition_candidates(),
                &[RowId::H19, RowId::H22, RowId::H23][..],
            ),
            (
                "false-attestation",
                false_candidates(),
                &[RowId::H15, RowId::H16, RowId::H22, RowId::H23, RowId::H24][..],
            ),
        ] {
            let r = check_from(&gate_profile(name), two_holders(lost), candidates);
            eprintln!("{name} lost={lost}: {}", r.render());
            assert!(r.violation.is_none(), "{}", r.render());
            for row in rows {
                assert!(r.rows_seen[row.index()], "{}", row.name());
            }
        }
    }
}

/// The peer-symmetry quotient keeps the verdicts and the rows seen in the
/// false-attestation fixture: with every fix and for each H.48 and H.49
/// ablation.
#[test]
fn h48_h49_peer_symmetry_preserves_verdicts() {
    let state = |symmetry| {
        let mut s = two_holders(false);
        s.env.peer_symmetry = symmetry;
        s
    };
    for fix in [
        None,
        Some(Fix::LifecycleKeyRotation),
        Some(Fix::CapabilityKeyRotation),
        Some(Fix::CapabilityRotationBeforeList),
    ] {
        let p = Profile {
            design: fix.map_or(D, |x| D.without(x)),
            ..gate_profile("h48-h49-symmetry")
        };
        let full = check_from(&p, state(false), false_candidates());
        let reduced = check_from(&p, state(true), false_candidates());
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
