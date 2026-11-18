//! HOLDINGS-WIRE §15 and Appendix A. The OpenClast byte-level reference fake
//! is not vendored here. Kwiry uses this deterministic typed fake with opaque
//! synthetic proof/ack bytes, and drives actual lifecycle transitions where
//! the core abstracts away the envelope or clock. No test uses real keys,
//! wall-clock sleeps, or a deployed authority.

use kwiry_lifecycle::action::{Action, Action::*, FaultCode, faults};
use kwiry_lifecycle::checker::{Profile, check, check_from};
use kwiry_lifecycle::design::{Design, Fix};
use kwiry_lifecycle::props::{self, Prop};
use kwiry_lifecycle::row_id::RowId;
use kwiry_lifecycle::state::*;
use kwiry_lifecycle::step::{
    CapabilityRefusal, Config, Obs, Outcome, ReapprovalCause, exec, mintable, observed_remaining,
    servable,
};
use kwiry_lifecycle::wire::{self, AckClass, Expiry, KeyPin, Keyset, Proof};

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
const REV: Rev = Rev { epoch: 0, seq: 1 };
const D: Design = Design::REV11;

fn profile(name: &'static str, design: Design) -> Profile {
    Profile {
        name,
        design,
        budget: BUDGET,
        faults: faults::ALL,
        garbage: false,
        same_state: false,
        liveness: false,
        skip: Prop::I7Deadlock.bit(),
        max_states: 30_000,
    }
}

struct Fake {
    s: State,
    cfg: Config,
}

impl Fake {
    fn new() -> Self {
        Self::with(D)
    }
    fn with(design: Design) -> Self {
        Self {
            s: State::initial(BUDGET, false),
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
    fn try_step(&mut self, a: Action) -> Option<Vec<RowId>> {
        let out = self.outcome(a)?;
        self.s = out.next;
        Some(out.rows.iter().collect())
    }
    fn step(&mut self, a: Action) -> Vec<RowId> {
        self.try_step(a).unwrap_or_else(|| {
            panic!(
                "{a:?} disabled: {}",
                kwiry_lifecycle::checker::summary(&self.s)
            )
        })
    }
    fn run(&mut self, actions: &[Action]) {
        for a in actions {
            self.step(*a);
        }
    }
    fn boot(&mut self) {
        self.run(&[Boot, Markers, Narrow]);
    }
    fn list(&mut self) -> ListMsg {
        self.step(FetchStart);
        let Some(ListResp::List(list)) = self.s.net.list else {
            panic!("expected list")
        };
        self.step(ListDeliver);
        while self.s.vol.apply.is_some() {
            self.step(Apply);
        }
        list
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
    /// H.19: after an epoch acceptance the fresh status goes first; new-epoch
    /// obligation reports wait for its current-epoch receipt.
    fn fresh_status(&mut self) {
        assert!(self.s.dur.epoch_status_pending);
        assert!(
            self.try_step(Enqueue(0)).is_none(),
            "obligation reports wait"
        );
        self.request(2);
        let epoch = self.s.dur.hw.unwrap().epoch;
        assert!(matches!(self.reply(2), RespKind::Accepted { epoch: e, .. } if e == epoch));
        assert!(!self.s.dur.epoch_status_pending && !self.s.ghost.status_due);
    }
}

/// The signed bytes are synthetic but opaque: the fake caches the first
/// accepted bytes, never regenerates them to match the transport attempt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct WireAck {
    key: u8,
    jti: u32,
    digest: u8,
    epoch: u8,
    bytes: &'static [u8],
}
const ACK1: WireAck = WireAck {
    key: 1,
    jti: 101,
    digest: 201,
    epoch: 0,
    bytes: b"ack.k1.j101-d201",
};
const TRY1: (u32, u8) = (101, 201);
const TRY2: (u32, u8) = (102, 202);

#[derive(Default)]
struct AckFake {
    original: Option<WireAck>,
    attempts: Vec<(u32, u8)>,
    uncertain: bool,
    receipt: Option<WireAck>,
    floor: Option<RState>,
    relist: bool,
}
impl AckFake {
    fn send(&mut self, attempt: (u32, u8)) -> WireAck {
        self.attempts.push(attempt);
        *self.original.get_or_insert(ACK1)
    }
    fn receive(&mut self, ack: WireAck) -> AckClass {
        let disposition = wire::classify_ack(
            D,
            1,
            ack.key,
            false,
            false,
            ack.epoch == 0,
            self.attempts.contains(&(ack.jti, ack.digest)),
        );
        match disposition {
            AckClass::Receipt => {
                self.receipt = Some(ack);
                self.floor = Some(RState::Incomplete);
                self.uncertain = false;
                self.attempts.clear();
            }
            AckClass::Unverifiable | AckClass::AuditAndRelist => self.relist = true,
            AckClass::Convergence => {}
        }
        disposition
    }
}

#[test]
fn v01_delayed_receipt_is_for_the_business_key_not_the_in_flight_attempt() {
    let mut f = AckFake::default();
    let original = f.send(TRY1);
    assert_eq!((original.jti, original.digest), TRY1);
    let replay = f.send(TRY2);
    assert_eq!(replay.bytes, original.bytes);
    assert_eq!(f.receive(replay), AckClass::Receipt);
    assert_eq!(f.receipt, Some(original));
    assert_eq!(f.floor, Some(RState::Incomplete));
    assert!(!f.uncertain && f.attempts.is_empty());
}

#[test]
fn v02_lost_sent_store_relearns_floor_without_claiming_receipt() {
    let mut a = AckFake::default();
    a.send(TRY1);
    let replay = a.send(TRY2);
    a.attempts.clear();
    assert_eq!(a.receive(replay), AckClass::Unverifiable);
    assert!(a.relist && a.receipt.is_none() && a.floor.is_none());
    let mut f = Fake::new();
    f.boot();
    f.list();
    f.step(AuthRetire(0));
    f.list();
    f.step(PurgeDelete { u: 0, fail: true });
    f.step(PurgeScan(0));
    f.request(0);
    f.step(AuthProcess { c: 0, idx: 0 });
    // Lose the sent evidence/response before applying the authority list.
    f.s.net.resps[0] = Default::default();
    f.s.dur.slots[0] = None;
    f.s.vol.slotv[0] = SlotVol::Idle;
    f.list();
    assert_eq!(f.s.dur.floors[0].map(|x| x.state), Some(RState::Incomplete));
    assert_eq!(f.s.vol.route, Route::Normal);
}

#[test]
fn v03_receipt_resolves_uncertain_key() {
    let mut a = AckFake::default();
    let ack = a.send(TRY1);
    a.uncertain = true;
    assert_eq!(a.receive(ack), AckClass::Receipt);
    assert!(!a.uncertain);
    let mut f = Fake::new();
    f.boot();
    f.list();
    f.step(AuthRetire(0));
    f.list();
    f.request(0);
    let req = f.s.net.reqs[0].get(0).unwrap();
    f.step(AuthProcess { c: 0, idx: 0 });
    let original = f.s.net.resps[0].remove_at(0).unwrap();
    let key = f.s.dur.slots[0].take().unwrap().key;
    f.s.dur.uncertain[0].insert(key, "fixture").unwrap();
    f.s.vol.slotv[0] = SlotVol::Idle;
    f.s.net.reqs[0]
        .push(req, "re-signed transport attempt")
        .unwrap();
    assert_eq!(f.reply(0), original.kind);
    assert!(!f.s.dur.uncertain[0].contains(&key));
    assert_eq!(f.s.dur.floors[0].unwrap().state, RState::Revoked);
}

#[test]
fn v04_previous_epoch_current_ack_is_audit_only() {
    assert_eq!(
        wire::classify_ack(D, 1, 2, true, false, false, true),
        AckClass::AuditAndRelist
    );
    let mut f = Fake::new();
    f.boot();
    f.list();
    f.step(AuthRetire(0));
    f.list();
    f.run(&[AuthBackup, AuthRestore]);
    f.list();
    f.step(OpAcceptEpoch);
    f.boot();
    assert_eq!(f.list().rev.epoch, 1);
    f.fresh_status();
    f.request(0);
    let key = f.s.dur.slots[0].unwrap().key;
    f.s.net.resps[0]
        .push(
            Resp {
                key,
                kind: RespKind::StateRegression(Ack {
                    state: RState::Complete,
                    epoch: 0,
                }),
                healthy: true,
                late: false,
            },
            "fixture",
        )
        .unwrap();
    ablate(
        Fix::ConvergenceEnvelope,
        Prop::I8ConvergenceEnvelope,
        f.s.clone(),
        &[Deliver { c: 0, idx: 0 }],
    );
    assert!(f.step(Deliver { c: 0, idx: 0 }).contains(&RowId::R6e));
    assert!(f.s.dur.floors[0].is_none() && f.s.vol.relist);
}

#[test]
fn v05_already_complete_for_another_key_is_convergence() {
    assert_eq!(
        wire::classify_ack(D, 2, 1, false, true, true, false),
        AckClass::Convergence
    );
    assert_eq!(
        wire::classify_ack(D, 2, 1, false, false, true, false),
        AckClass::Convergence
    );
    // Even with a matching key and known attempt, an explicit convergence
    // outcome cannot be filed as a receipt for the sent attempt.
    assert_eq!(
        wire::classify_ack(D, 1, 1, false, true, true, true),
        AckClass::Convergence
    );
    assert_eq!(
        wire::classify_ack(
            D.without(Fix::ConvergenceEnvelope),
            1,
            1,
            false,
            true,
            true,
            true
        ),
        AckClass::Receipt
    );
    assert_eq!(
        wire::classify_ack(D, 1, 1, true, false, true, true),
        AckClass::Convergence
    );
}

fn release_mismatch(change: impl FnOnce(&mut Fake)) {
    let mut f = Fake::new();
    f.boot();
    f.list();
    f.step(AuthUnlist(1));
    f.list();
    f.request(2);
    f.reply(2);
    f.step(HumanRelease);
    change(&mut f);
    f.list();
    let before = f.s.dur.units;
    assert!(f.step(RelBind(0)).contains(&RowId::L2));
    assert_eq!(f.s.dur.units, before);
    let observed = f.s.vol.inv;
    f.request(2);
    let key = f.s.dur.slots[2].unwrap().key.status().unwrap();
    assert_eq!(
        key.result,
        Some(kwiry_lifecycle::state::RelResult {
            instr: 0,
            executed: false,
            observed
        })
    );
    assert!(matches!(
        f.reply(2),
        RespKind::Accepted {
            disposition: Some(ReleaseDisposition::Applied),
            ..
        }
    ));
    assert_eq!(f.s.auth.core.superseded_mismatch, 1);
    assert!(f.s.auth.core.instr.is_none());
    assert_eq!(f.s.dur.units, before);
}
#[test]
fn v06_unretained_manifest() {
    release_mismatch(|f| f.s.dur.manifests = 0);
}
#[test]
fn v07_approved_count_mismatch() {
    release_mismatch(|f| f.s.auth.core.instr.as_mut().unwrap().approved_count += 1);
}
#[test]
fn v08_manifest_unit_no_longer_unattributed() {
    release_mismatch(|f| {
        f.s.budget.changes = 1;
        f.step(AuthRelist(1));
    });
}

#[test]
fn v09_unknown_instruction_status_is_accepted_and_replays() {
    let mut f = Fake::new();
    let key = Key::S(SKey {
        list_ref: REV,
        reset: true,
        manifest: 4,
        result: Some(kwiry_lifecycle::state::RelResult {
            instr: 9,
            executed: true,
            observed: 4,
        }),
    });
    let seq = f.s.auth.core.seq;
    let submit = |f: &mut Fake| {
        f.s.net.reqs[2]
            .push(
                Req {
                    key,
                    replay: false,
                    aged: false,
                    commit_late: false,
                    alive: true,
                },
                "fixture",
            )
            .unwrap();
        f.step(AuthProcess { c: 2, idx: 0 });
        f.s.net.resps[2].remove_at(0).unwrap()
    };
    let original = submit(&mut f);
    assert_eq!(
        original.kind,
        RespKind::Accepted {
            epoch: 0,
            accepted_holder_reset: Some(true),
            disposition: Some(ReleaseDisposition::UnknownInstruction)
        }
    );
    assert_eq!(f.s.auth.core.status, key.status());
    assert_eq!(f.s.auth.core.seq, seq + 1);
    assert_eq!(submit(&mut f), original);
    assert_eq!(f.s.auth.core.seq, seq + 1);
}

#[test]
fn v10_tag_valid_ahead_reference_latches_restore_alarm() {
    for design in [D, D.without(Fix::AuthorityFidelity)] {
        let mut f = Fake::with(design);
        f.step(AuthRetire(0));
        let key = Key::O(OKey {
            o: 0,
            list_ref: Rev { epoch: 0, seq: 3 },
            remaining: true,
            state: RState::Revoked,
            manifest: 0,
        });
        f.s.net.reqs[0]
            .push(
                Req {
                    key,
                    replay: false,
                    aged: false,
                    commit_late: false,
                    alive: true,
                },
                "fixture",
            )
            .unwrap();
        let outcome = exec(&f.s, AuthProcess { c: 0, idx: 0 }, &f.cfg)
            .unwrap()
            .unwrap();
        assert_eq!(
            props::check_step(&f.s, &outcome, 0),
            if design == D {
                None
            } else {
                Some(Prop::I8AuthorityFidelity)
            }
        );
        let kind = outcome.next.net.resps[0].get(0).unwrap().kind;
        f.s = outcome.next;
        if design == D {
            assert_eq!(kind, RespKind::EpochMismatch);
            assert!(f.s.auth.rollback_suspected && !mintable(&f.s, 1));
        } else {
            assert_eq!(kind, RespKind::StaleListRef);
            assert!(!f.s.auth.rollback_suspected);
        }
    }
    assert_eq!(
        wire::reference(D, REV, Rev { epoch: 0, seq: 3 }, false),
        wire::RefVerdict::Stale
    );
    assert_eq!(
        wire::reference(D, REV, Rev { epoch: 1, seq: 1 }, true),
        wire::RefVerdict::Rollback
    );
}

#[test]
fn accepted_report_advances_seq_and_holder_revision() {
    let mut f = Fake::new();
    f.boot();
    f.list();
    f.step(AuthRetire(0));
    f.list();
    f.request(0);
    ablate(
        Fix::AuthorityFidelity,
        Prop::I8AuthorityFidelity,
        f.s.clone(),
        &[AuthProcess { c: 0, idx: 0 }],
    );
    let before = f.s.auth.core.seq;
    f.reply(0);
    assert_eq!(f.s.auth.core.seq, before + 1);
    assert_eq!(f.s.auth.core.holder_rev, f.s.auth.core.seq);
}

#[test]
fn v11_proof_crosses_commit_deadline() {
    let a = Proof::sign(10, 1000);
    assert!(a.timely_at_receipt(1120));
    assert!(!a.timely_at_receipt(1121));
    assert!(a.timely_at_commit(1420));
    assert!(!a.timely_at_commit(1421));
    let mut f = Fake::new();
    f.s.env.probe = Probe::Proof;
    f.boot();
    f.list();
    f.step(AuthRetire(0));
    f.list();
    f.request(0);
    let before = f.s.auth.core;
    for (n, expected) in [RowId::R11p, RowId::R11b].into_iter().enumerate() {
        f.step(DelayCommit(0));
        f.step(AuthProcess { c: 0, idx: 0 });
        assert_eq!(
            f.s.net.resps[0].get(0).unwrap().kind,
            RespKind::ProofExpired
        );
        assert!(f.step(Deliver { c: 0, idx: 0 }).contains(&expected));
        assert_eq!(f.s.auth.core, before);
        assert_eq!(f.s.vol.route, Route::Normal);
        if n == 0 {
            f.run(&[Transmit(0), Handoff(0), Dispatch(0)]);
        }
    }
    let retry = Proof::sign(11, 1421);
    assert_ne!(retry.jti, a.jti);
    assert_eq!(retry.exp - retry.iat, 300);
    assert_eq!(wire::expiry(D, true), Expiry::BackoffClockAlert);
}

#[test]
fn v12_offline_retirement_survives_restore_and_epoch_acceptance() {
    let mut f = Fake::new();
    f.boot();
    f.list();
    f.run(&[AuthBackup, Crash, AuthRetire(0), AuthRestore]);
    assert!(!mintable(&f.s, 0));
    assert!(!props::goal(&f.s));
    f.boot();
    let first = f.list();
    assert_eq!(first.rev.epoch, 1);
    assert_eq!(first.items[0], Item::Terminating(None)); // stable holding/obligation index 0
    assert!(f.s.auth.core.rooms[0].obl.is_some());
    f.step(OpAcceptEpoch);
    f.boot();
    f.list();
    assert!(!servable(&f.s, 0));
    assert_eq!(f.s.dur.hw.unwrap().epoch, 1);
    assert!(!props::goal(&f.s), "the fresh-epoch status is still due");
    f.fresh_status();
    f.run(&[PurgeDelete { u: 0, fail: false }, PurgeScan(0)]);
    f.request(0);
    f.reply(0);
    assert!(props::goal(&f.s));
}

fn ready_to_decline() -> Fake {
    let mut f = Fake::new();
    f.boot();
    f.list();
    f.run(&[AuthUnlist(1), AuthRetire(0)]);
    f.list();
    f.run(&[PurgeDelete { u: 0, fail: false }, PurgeScan(0)]);
    f.request(0);
    f.reply(0);
    f.request(2);
    f.reply(2);
    f
}
#[test]
fn v13_blocked_list_does_not_release_quarantine() {
    let mut f = ready_to_decline();
    let before = f.s.dur.units;
    f.step(HumanDecline);
    let l = f.list();
    assert_eq!(l.items[0], Item::Blocked(Some(RState::Incomplete)));
    assert_eq!(f.s.vol.class[0], Class::Obligated);
    assert!(props::goal(&f.s) && !servable(&f.s, 0));
    assert_eq!(f.s.dur.units, before);
    assert!(l.instr.is_none());
    assert!(f.try_step(RelBind(0)).is_none());
    f.s.env.probe = Probe::Governance;
    f.step(OpReopen(0));
    assert_eq!(
        f.list().items[0],
        Item::Terminating(Some(RState::Incomplete))
    );
    assert_eq!(f.s.dur.units, before);
}

#[test]
fn v14_unknown_key_fails_closed_and_signed_rotation_survives_restart() {
    let mut f = Fake::new();
    f.boot();
    f.s.env.list_signing = Some((2, None));
    f.step(FetchStart);
    assert_eq!(f.step(ListDeliver), vec![RowId::F10]);
    assert_eq!(f.s.dur.pinned_keys, KeyPin::ENROLLED);
    assert!(!servable(&f.s, 0));
    let statement = Keyset {
        seq: 1,
        signed_by: 1,
        verified: true,
        keys: 3,
        retire: 1,
    };
    f.s.env.list_signing = Some((2, Some(statement)));
    f.list();
    assert_eq!(f.s.dur.pinned_keys, KeyPin { seq: 1, kids: 2 });
    f.step(Crash);
    f.boot();
    f.s.env.list_signing = Some((2, None));
    f.list();
    assert!(servable(&f.s, 0));
    assert!(
        f.s.dur
            .pinned_keys
            .authorize(D, 1, Some(statement))
            .is_err()
    );
}

fn ablate(fix: Fix, expected: Prop, s: State, actions: &[Action]) {
    let p = profile(fix.finding(), D);
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

#[test]
fn retirement_replay_is_necessary_for_safety_and_l1() {
    let mut p = profile("offline-retirement-restore", Design::REV9);
    p.budget = Budget {
        faults: 0,
        crashes: 0,
        changes: 0,
        ..BUDGET
    };
    p.liveness = true;
    p.skip = Prop::I8AuthorityFidelity.bit();
    // The L1 counterexample needs the whole graph. Crash schedules aren't
    // needed for this witness; the epoch/crash CI profile covers them.
    p.max_states = 200_000;
    for design in [Design::REV9, D.without(Fix::RetirementReplay)] {
        let r = check(&Profile { design, ..p });
        assert_eq!(r.violation.unwrap().prop, Prop::I1Retirement);
    }
    let r = check(&Profile {
        design: D.without(Fix::RetirementReplay),
        skip: Prop::I1Retirement.bit() | p.skip,
        ..p
    });
    assert_eq!(r.violation.unwrap().prop, Prop::L1Liveness);
    let r = check(&Profile { design: D, ..p });
    assert!(r.violation.is_none(), "{}", r.render());
}

#[test]
fn remaining_fix_ablations_and_lease_conformance() {
    let mut f = Fake::new();
    f.boot();
    f.s.env.list_signing = Some((2, None));
    ablate(
        Fix::SignedKeyRotation,
        Prop::I1UntrustedKey,
        f.s,
        &[FetchStart, ListDeliver],
    );

    let mut f = Fake::new();
    f.boot();
    f.step(FetchStart);
    f.step(ListFault(FaultCode::ProofExpired));
    ablate(
        Fix::ProofExpiryRetry,
        Prop::I8HealthyHalt,
        f.s,
        &[ListDeliver],
    );
    ablate(
        Fix::ReleaseDeclined,
        Prop::I8DeclineVisible,
        ready_to_decline().s,
        &[HumanDecline],
    );

    let mut f = Fake::new();
    f.s.env.probe = Probe::Root;
    ablate(
        Fix::RootHolderBinding,
        Prop::I1WrongHolder,
        f.s.clone(),
        &[SwapHolder, Boot],
    );
    f.run(&[LoseHolderState, BeginRecovery, Crash]);
    assert!(f.try_step(Boot).is_none());
    f.run(&[BeginRecovery, CommitRecovery, Boot]);
    assert_eq!(f.s.dur.root_holder, Some(0));
    let mut f = Fake::new();
    f.s.env.probe = Probe::Root;
    f.s.dur.root_holder = None;
    f.s.dur.state_holder = None;
    f.s.dur.units = [UDur::Deleted; UNITS];
    f.s.dur.last_list_ref = Some(REV);
    assert!(f.step(Boot).contains(&RowId::S4));
    assert!(f.s.dur.last_list_ref.is_none() && f.s.dur.reset_pending);

    let mut f = Fake::new();
    f.boot();
    f.list();
    assert_eq!(wire::list_echo(D, f.s.dur.last_list_ref), Some(REV));
    // Inject a raw rollback below a tag-valid reference, then restart.
    f.step(Crash);
    f.boot();
    f.s.auth.core.seq = 0;
    let s = f.s.clone();
    ablate(
        Fix::EchoListRef,
        Prop::I8ListWitness,
        s.clone(),
        &[FetchStart],
    );
    f.step(FetchStart);
    assert!(f.s.auth.rollback_suspected);
    let cfg = Config {
        design: D.without(Fix::EchoListRef),
        ..f.cfg
    };
    assert!(
        !exec(&s, FetchStart, &cfg)
            .unwrap()
            .unwrap()
            .next
            .auth
            .rollback_suspected
    );

    let mut f = Fake::new();
    f.s.env.probe = Probe::Lease;
    ablate(
        Fix::LeaseAndRollbackFence,
        Prop::I1Lease,
        f.s.clone(),
        &[TryLegacy],
    );
    f.boot();
    f.run(&[FetchStart, ListDeliver, Apply]);
    ablate(
        Fix::LeaseAndRollbackFence,
        Prop::I1Lease,
        f.s.clone(),
        &[LeaseDeadline, Apply],
    );
    let before = f.s.dur.units;
    f.run(&[LeaseDeadline, Apply]);
    assert!(!servable(&f.s, 0));
    assert_eq!(f.s.dur.units, before);
    f.list();
    assert!(servable(&f.s, 0));
    f.step(AuthUnlist(1));
    f.run(&[FetchStart, ListDeliver]);
    assert!(!servable(&f.s, 1), "A0 must hold before A1");
    f.step(LeaseDeadline);
    assert!(!servable(&f.s, 0), "deadline crosses before A4");
    assert_eq!(f.s.dur.units, before);
}

#[test]
fn first_byte_403_cancels_unsent_but_preserves_sent_uncertain() {
    let mut f = Fake::new();
    f.boot();
    f.list();
    f.run(&[AuthUnlist(1), AuthRetire(0)]);
    f.list();
    f.request(0);
    f.run(&[Enqueue(2), Transmit(2), Handoff(2)]);
    f.step(AuthFault {
        c: 0,
        idx: 0,
        code: FaultCode::Forbidden,
    });
    ablate(
        Fix::CancelOnForbidden,
        Prop::I8ForbiddenSend,
        f.s.clone(),
        &[Deliver { c: 0, idx: 0 }, Dispatch(2)],
    );
    let sent = f.s.dur.slots[0].unwrap().key;
    f.step(Deliver { c: 0, idx: 0 });
    assert!(f.s.dur.uncertain[0].contains(&sent));
    assert!(f.s.dur.uncertain[2].is_empty());
    assert!(f.try_step(Dispatch(2)).is_none());
}

#[test]
fn exact_claimant_and_independent_census() {
    let mut f = Fake::new();
    f.s.env.probe = Probe::Evidence;
    f.boot();
    f.list();
    f.step(KeepOpen);
    f.step(AuthRetire(0));
    f.list();
    ablate(
        Fix::ExactDeletionEvidence,
        Prop::I2Deletion,
        f.s.clone(),
        &[PurgeDelete { u: 0, fail: false }],
    );
    // Independent observation explicitly includes a live handle even when the
    // directory entry no longer exists.
    f.s.dur.units[0] = UDur::Deleted;
    assert!(observed_remaining(&f.s, 0));
    f.step(PurgeScan(0));
    assert_eq!(f.s.vol.scans[0].unwrap().state, RState::Incomplete);
    f.step(CloseHandle);
    f.step(PurgeScan(0));
    f.run(&[Enqueue(0), Transmit(0), Handoff(0)]);
    f.step(LatePublish);
    ablate(
        Fix::ExactDeletionEvidence,
        Prop::I3CompleteDispatch,
        f.s.clone(),
        &[Dispatch(0)],
    );
    assert!(f.step(Dispatch(0)).contains(&RowId::R2q));
    f.step(ReconcileLate);
    f.step(PurgeDelete { u: 1, fail: false });
    assert!(!observed_remaining(&f.s, 0));

    let mut f = Fake::new();
    f.s.env.probe = Probe::Evidence;
    f.boot();
    f.list();
    f.step(AuthUnlist(1));
    f.list();
    f.request(2);
    f.reply(2);
    f.step(HumanRelease);
    f.list();
    f.run(&[RelBind(0), RelDetach(0), RelDrain(0)]);
    ablate(
        Fix::ExactDeletionEvidence,
        Prop::I2Deletion,
        f.s,
        &[WrongRelease { i: 1, u: 2 }],
    );
}

#[test]
fn k9_list_budget_respects_cursor_cap() {
    assert_eq!(wire::list_budget_seconds(0), 120);
    assert_eq!(wire::list_budget_seconds(22_500), 120);
    assert_eq!(wire::list_budget_seconds(22_501), 122);
    assert_eq!(wire::list_budget_seconds(100_000), 430);
    assert_eq!(wire::list_budget_seconds(442_500), 1800);
    assert_eq!(wire::list_budget_seconds(u64::MAX), 1800);
}

// ------------------------------------------------- revision 10.1 (H.12–H.15)

#[test]
fn h12_every_route_preserves_a_verified_witness_before_later_errors() {
    use wire::{Envelope, ProofTimes, RefInput, Refusal};
    let ahead = RefInput::verified(Rev { epoch: 0, seq: 2 });
    let envelope = Envelope {
        refs: [RefInput::Malformed, ahead],
        malformed: true,
        replay: true,
        wrong_scope: true,
        rate_limited: true,
        clock: Some(ProofTimes {
            proof: Proof::sign(1, 0),
            received: 421,
            commit: 421,
        }),
        ..Envelope::default()
    };
    // Step 4 wins over an expired proof, a replayed jti, a wrong scope, the
    // rate limit and malformed claims (including the other, malformed ref).
    assert_eq!(
        wire::preflight(D, envelope, REV, false),
        Err(Refusal::Witness)
    );
    // Steps 1–3 still come first.
    let first = [
        (
            Envelope {
                too_large: true,
                ..envelope
            },
            true,
            Refusal::TooLarge,
        ),
        (
            Envelope {
                bad_signature: true,
                ..envelope
            },
            true,
            Refusal::EpochMismatch,
        ),
        (
            Envelope {
                custody_unreadable: true,
                bad_signature: true,
                ..envelope
            },
            false,
            Refusal::CustodyUnavailable,
        ),
        (
            Envelope {
                bad_signature: true,
                ..envelope
            },
            false,
            Refusal::InvalidProof,
        ),
    ];
    for (e, suspected, expected) in first {
        assert_eq!(wire::preflight(D, e, REV, suspected), Err(expected));
    }
    // A malformed ref is no evidence: steps 5–9 decide, in order.
    let no_witness = Envelope {
        refs: [RefInput::Malformed; 2],
        ..envelope
    };
    for (e, expected) in [
        (no_witness, Refusal::ProofExpired),
        (
            Envelope {
                clock: None,
                ..no_witness
            },
            Refusal::Replay,
        ),
        (
            Envelope {
                clock: None,
                replay: false,
                ..no_witness
            },
            Refusal::Scope,
        ),
        (
            Envelope {
                clock: None,
                replay: false,
                wrong_scope: false,
                ..no_witness
            },
            Refusal::Rate,
        ),
        (
            Envelope {
                refs: [RefInput::Malformed, RefInput::Absent],
                ..Envelope::default()
            },
            Refusal::Grammar,
        ),
    ] {
        assert_eq!(wire::preflight(D, e, REV, false), Err(expected));
    }
    // A tag that does not verify is skipped at step 4: it is no witness.
    let bad_tag = RefInput::WellFormed {
        rev: Rev { epoch: 0, seq: 2 },
        tag_valid: false,
    };
    assert!(bad_tag.invalid_tag());
    let skipped = Envelope {
        refs: [bad_tag, RefInput::Absent],
        ..Envelope::default()
    };
    assert_eq!(wire::preflight(D, skipped, REV, false), Ok(()));

    // The list route.
    let mut list = Fake::new();
    list.boot();
    list.s.env.envelope = Some(envelope);
    ablate(
        Fix::UnifiedValidation,
        Prop::I8ValidationPrecedence,
        list.s.clone(),
        &[FetchStart],
    );
    list.step(FetchStart);
    assert!(list.s.auth.rollback_suspected && !mintable(&list.s, 0));
    assert!(matches!(
        list.s.net.list,
        Some(ListResp::Err {
            code: ListErr::EpochMismatch,
            ..
        })
    ));
    assert!(list.step(ListDeliver).contains(&RowId::H10));
    // The report and status routes.
    for (c, key) in [
        (
            0,
            Key::O(OKey {
                o: 0,
                state: RState::Revoked,
                list_ref: REV,
                manifest: 0,
                remaining: true,
            }),
        ),
        (
            2,
            Key::S(SKey {
                list_ref: REV,
                reset: false,
                manifest: 0,
                result: None,
            }),
        ),
    ] {
        let mut f = Fake::new();
        f.s.env.envelope = Some(envelope);
        f.s.net.reqs[c]
            .push(
                Req {
                    key,
                    replay: false,
                    aged: false,
                    commit_late: false,
                    alive: true,
                },
                "fixture",
            )
            .unwrap();
        let action = AuthProcess { c: c as u8, idx: 0 };
        ablate(
            Fix::UnifiedValidation,
            Prop::I8ValidationPrecedence,
            f.s.clone(),
            &[action],
        );
        f.step(action);
        assert_eq!(
            f.s.net.resps[c].get(0).unwrap().kind,
            RespKind::EpochMismatch
        );
        assert_eq!(f.s.auth.core.seq, REV.seq, "nothing processed or committed");
        assert!(f.s.auth.rollback_suspected);
    }
}

fn report_in_flight() -> Fake {
    let mut f = Fake::new();
    f.boot();
    f.list();
    f.step(AuthRetire(0));
    f.list();
    f.request(0);
    f
}

/// The in-flight report, as the authority sees it at the given clock.
fn timed(mut s: State, proof: Proof, received: i64, commit: i64) -> State {
    use wire::{Envelope, ProofTimes, RefInput};
    let list_ref = s.net.reqs[0].get(0).unwrap().key.list_ref();
    s.env.envelope = Some(Envelope {
        refs: [RefInput::verified(list_ref), RefInput::Absent],
        clock: Some(ProofTimes {
            proof,
            received,
            commit,
        }),
        ..Envelope::default()
    });
    s
}

fn report_at(proof: Proof, received: i64, commit: i64) -> (RespKind, AuthCore, AuthCore) {
    let mut f = report_in_flight();
    let before = f.s.auth.core;
    f.s = timed(f.s, proof, received, commit);
    f.step(AuthProcess { c: 0, idx: 0 });
    (f.s.net.resps[0].get(0).unwrap().kind, before, f.s.auth.core)
}

/// V11 as H.13 restates it: one deadline `D = exp + 120 s`, checked at
/// receipt and again inside the CAS; bare `exp` is never a boundary.
#[test]
fn v11_restated_receipt_and_commit_share_the_deadline() {
    // Controls: a 60 s proof received at `exp + 1` is accepted, and its
    // commit at exactly `D` commits.
    let short = Proof {
        jti: 12,
        iat: 1000,
        exp: 1060,
    };
    assert!(short.timely_at_receipt(1061));
    let (kind, before, after) = report_at(short, 1061, 1180);
    assert!(matches!(kind, RespKind::Accepted { .. }));
    assert_eq!(after.seq, before.seq + 1);

    // Received in time, CAS at `D + 1`: 401 proof_expired, nothing
    // committed, seq unchanged, no idempotency receipt.
    let (kind, before, after) = report_at(short, 1061, 1181);
    assert_eq!(kind, RespKind::ProofExpired);
    assert_eq!(after, before, "no seq, no receipt, no obligation change");
    ablate(
        Fix::ProofDeadline,
        Prop::I8ProofDeadline,
        timed(report_in_flight().s, short, 1061, 1181),
        &[AuthProcess { c: 0, idx: 0 }],
    );

    // Kwiry's own proofs (`exp = iat + 300`): receipt needs `|now − iat| ≤
    // 120`, so `D = iat + 420` is the last commit instant.
    let own = Proof::sign(10, 1000);
    assert!(own.timely_at_receipt(1120) && !own.timely_at_receipt(1121));
    assert!(matches!(
        report_at(own, 1120, 1420).0,
        RespKind::Accepted { .. }
    ));
    assert_eq!(report_at(own, 1120, 1421).0, RespKind::ProofExpired);
    // A proof that lives longer than 300 s is refused at receipt, so it can
    // never stretch D past iat + 420.
    let long = Proof { exp: 1301, ..own };
    let (kind, before, after) = report_at(long, 1100, 1400);
    assert_eq!(kind, RespKind::ProofExpired);
    assert_eq!(after, before);
    ablate(
        Fix::ProofDeadline,
        Prop::I8ProofDeadline,
        timed(report_in_flight().s, long, 1100, 1400),
        &[AuthProcess { c: 0, idx: 0 }],
    );
}

#[test]
fn h13_only_page_zero_supports_absence_and_proof_duration_is_bounded() {
    let short = Proof {
        jti: 1,
        iat: 0,
        exp: 59,
    };
    let long = Proof { exp: 301, ..short };
    assert!(short.receipt(D.without(Fix::ProofDeadline), 0));
    assert!(!short.timely_at_receipt(0) && !long.timely_at_receipt(0));
    assert!(Proof { exp: 60, ..short }.timely_at_receipt(0));
    // Kwiry's R15 margin is exp + 420, against page 0's iat only.
    assert!(!wire::absence_proved(
        720,
        300,
        Some(RState::Revoked),
        RState::Complete
    ));
    assert!(wire::absence_proved(
        721,
        300,
        Some(RState::Revoked),
        RState::Complete
    ));
    assert!(wire::absence_proved(721, 300, None, RState::Revoked));
    assert!(!wire::absence_proved(
        721,
        300,
        Some(RState::Complete),
        RState::Complete
    ));
}

fn release_before_restore() -> Fake {
    let mut f = Fake::new();
    f.boot();
    f.list();
    f.step(AuthUnlist(1));
    f.list();
    f.request(2);
    f.reply(2);
    f.run(&[AuthBackup, HumanRelease]);
    f.list();
    f
}

#[test]
fn h14_replay_carries_bindings_in_one_transaction_and_never_reexecutes() {
    let mut f = release_before_restore();
    let issued = f.s.auth.core.instr.unwrap();
    let baseline = f.s.auth.backup.as_ref().unwrap().seq;
    // The holder holds a result for this id that the authority never
    // finalized: the re-listed id is re-reported, never executed again.
    f.s.dur.instr[0] = IDur::MismatchReported(f.s.vol.inv);
    f.step(AuthRestore);
    let replay = f.s.auth.core.instr.unwrap();
    assert_eq!(
        replay,
        Instr {
            issued_seq: baseline + 1,
            ..issued
        },
        "same id, bound digest and count from the entry; issued_seq is the replay seq"
    );
    assert_eq!(f.s.auth.core.seq, baseline + 1, "one replay transaction");
    f.list();
    f.step(OpAcceptEpoch);
    f.boot();
    let l = f.list();
    assert!(l.instr.is_some_and(|i| i.issued_seq <= l.rev.seq));
    assert!(f.try_step(RelBind(0)).is_none());
    f.request(2);
    let result = f.s.dur.slots[2].unwrap().key.status().unwrap().result;
    assert!(result.is_some_and(|r| r.instr == 0 && !r.executed));
    f.reply(2);
    assert_eq!(f.s.auth.core.superseded_mismatch, 1);
    assert!(f.s.auth.core.instr.is_none());

    // A release finalized after the backup is never listed again.
    let mut f = release_before_restore();
    f.run(&[
        RelBind(0),
        RelDetach(0),
        RelDrain(0),
        RelDelete {
            i: 0,
            u: 2,
            fail: false,
        },
        RelDeleted(0),
        Action::RelResult(0),
    ]);
    f.request(2);
    f.reply(2);
    assert!(f.s.auth.journal_finalized[0].is_some_and(|e| e.executed));
    f.step(AuthRestore);
    assert!(f.s.auth.core.instr.is_none());
    assert_eq!(f.s.auth.core.final_instr, 1);
}

#[test]
fn h14_decline_and_clear_replay_carried_data_and_a_corrupt_chain_fences_everything() {
    let mut f = ready_to_decline();
    f.run(&[AuthBackup, HumanDecline]);
    let declined = f.s.auth.journal_declined[0].unwrap();
    let mut cleared = Fake {
        s: f.s.clone(),
        cfg: f.cfg,
    };
    f.step(AuthRestore);
    let obl = f.s.auth.core.rooms[0].obl.unwrap();
    assert!(
        obl.blocked && obl.blocked_by == Some(declined),
        "digest and at from the entry"
    );
    cleared.s.env.probe = Probe::Governance;
    cleared.run(&[OpReopen(0), AuthRestore]);
    assert!(
        !cleared.s.auth.core.rooms[0].obl.unwrap().blocked,
        "a later blocked_cleared wins"
    );

    let mut f = release_before_restore();
    f.s.env.bad_journal = true;
    ablate(
        Fix::AtomicJournalReplay,
        Prop::I1UnverifiedRestore,
        f.s.clone(),
        &[AuthRestore],
    );
    let pre = f.s.auth.core;
    assert!(f.step(AuthRestore).contains(&RowId::H12j));
    assert_eq!(f.s.auth.core, pre, "no partial replay, no new epoch");
    assert!(f.s.auth.failed_restore && f.s.auth.rollback_suspected);
    assert!(!mintable(&f.s, 0));
    f.step(FetchStart);
    assert!(matches!(
        f.s.net.list,
        Some(ListResp::Err {
            code: ListErr::EpochMismatch,
            ..
        })
    ));
    assert!(f.try_step(AuthRestore).is_none(), "no automatic retry");
    assert!(f.step(OpRestoreJournalLost).contains(&RowId::H13j));
    assert!(f.s.auth.journal_lost_audited && !f.s.auth.rollback_suspected);
    assert_eq!(f.s.auth.core.epoch, 1);
    assert!(
        f.s.auth.core.instr.is_none(),
        "a lost approval must be given again"
    );
    assert!(
        f.s.auth.core.status.is_none(),
        "the next status report re-raises the count"
    );
}

#[test]
fn h15_capability_deadline_and_a0_both_bound_exposure() {
    let mut f = Fake::new();
    f.s.env.probe = Probe::Exposure;
    f.boot();
    f.list();
    f.step(AuthRetire(0));
    assert!(!mintable(&f.s, 0), "nothing new enters the window");
    let ask = AnswerCapability {
        room: 0,
        expiry_with_skew: 90,
    };
    ablate(
        Fix::CapabilityExposure,
        Prop::I1CapabilityExposure,
        f.s.clone(),
        &[TickExposure(91), ask],
    );
    f.step(TickExposure(90));
    assert!(
        f.step(ask).contains(&RowId::C17),
        "inside exp + skew, at most 90 s"
    );
    let sooner = AnswerCapability {
        room: 0,
        expiry_with_skew: 89,
    };
    assert!(f.step(sooner).contains(&RowId::C18));
    let mut expired = Fake {
        s: f.s.clone(),
        cfg: f.cfg,
    };
    expired.step(TickExposure(91));
    assert!(expired.step(ask).contains(&RowId::C18), "never past 90 s");
    f.run(&[FetchStart, ListDeliver]);
    assert!(
        f.step(ask).contains(&RowId::C18),
        "A0 refuses before any apply write"
    );
    assert!(!servable(&f.s, 0));
}

/// H.7 forbids listing a retired id `active` again, and H.1 replay keeps a
/// supported restore from doing it, so no CI profile reaches C9. C9 is the
/// holder's fail-closed answer if the authority does it anyway (a bug, or the
/// audited `restore_journal_lost` residual): tombstones win, the room is not
/// served and nothing is deleted.
#[test]
fn c9_a_resurrecting_list_never_reopens_a_tombstoned_room() {
    let mut f = Fake::new();
    f.boot();
    f.list();
    f.step(AuthRetire(0));
    f.list();
    assert!(f.s.dur.tomb & 1 != 0);
    let units = f.s.dur.units;
    // Inject the H.7 violation. I1's ghost clause flags that by design, so
    // the steps below check every other property.
    let c = &mut f.s.auth.core;
    c.rooms[0] = ARoom {
        phase: APhase::Active,
        obl: None,
    };
    c.seq += 1;
    c.holder_rev = c.seq;
    let skip = Prop::I1Retirement.bit();
    let mut rows = Vec::new();
    let mut go = |f: &mut Fake, a: Action| {
        let out = exec(&f.s, a, &f.cfg)
            .unwrap()
            .unwrap_or_else(|| panic!("{a:?} disabled"));
        assert_eq!(props::check_step(&f.s, &out, skip), None, "{a:?}");
        assert_eq!(props::check_state(&out.next, skip), None, "{a:?}");
        rows.extend(out.rows.iter());
        f.s = out.next;
    };
    go(&mut f, FetchStart);
    go(&mut f, ListDeliver);
    while f.s.vol.apply.is_some() {
        go(&mut f, Apply);
    }
    assert!(rows.contains(&RowId::C9));
    assert_eq!(f.s.vol.class[0], Class::Conflict);
    assert!(!servable(&f.s, 0));
    assert!(f.s.dur.tomb & 1 != 0, "tombstones only grow");
    assert_eq!(f.s.dur.units, units, "nothing deleted");
}

// ------------------------------------------------- revision 11 (H.17–H.21)

#[test]
fn h17_list_witness_is_advisory_but_valid_rollback_evidence_still_wins() {
    use wire::{Envelope, RefInput, WitnessReason};
    let mut f = Fake::new();
    f.boot();
    let ordinary = f.outcome(FetchStart).unwrap().next.net.list;
    for (reference, reason) in [
        (
            RefInput::WellFormed {
                rev: Rev { epoch: 0, seq: 2 },
                tag_valid: false,
            },
            WitnessReason::TagInvalid,
        ),
        (RefInput::RootRetired(REV), WitnessReason::RootRetired),
    ] {
        f.s.env.envelope = Some(Envelope {
            refs: [RefInput::Absent, reference],
            ..Envelope::default()
        });
        let out = f.outcome(FetchStart).unwrap();
        assert_eq!(out.next.net.list, ordinary);
        assert!(!out.next.auth.rollback_suspected);
        assert!(out.obs.contains(&Some(Obs::WitnessUnverifiable(reason))));
        ablate(
            Fix::AdvisoryListWitness,
            Prop::I8AdvisoryListWitness,
            f.s.clone(),
            &[FetchStart],
        );
    }
    for (reference, code) in [
        (RefInput::Malformed, ListErr::BadRequest),
        (
            RefInput::verified(Rev { epoch: 0, seq: 2 }),
            ListErr::EpochMismatch,
        ),
        (
            RefInput::verified(Rev { epoch: 1, seq: 1 }),
            ListErr::EpochMismatch,
        ),
    ] {
        f.s.env.envelope = Some(Envelope {
            refs: [RefInput::Absent, reference],
            ..Envelope::default()
        });
        let out = f.outcome(FetchStart).unwrap();
        assert!(matches!(out.next.net.list, Some(ListResp::Err { code: got, .. }) if code == got));
        assert_eq!(
            out.next.auth.rollback_suspected,
            code == ListErr::EpochMismatch
        );
    }
    f.s.auth.core.epoch = 1;
    f.s.env.envelope = Some(Envelope {
        refs: [RefInput::Absent, RefInput::verified(REV)],
        ..Envelope::default()
    });
    let ancestor = f.outcome(FetchStart).unwrap();
    assert!(matches!(ancestor.next.net.list, Some(ListResp::List(_))));
    assert_eq!(ancestor.obs, [None; 4]);
}

#[test]
fn h18_replay_finalizes_and_clears_restored_targets_and_audits_missing_ones() {
    for executed in [true, false] {
        let mut f = ready_to_decline();
        f.run(&[HumanRelease, AuthBackup]);
        let status_digest = f.s.auth.core.status.unwrap();
        // A finalized journal entry after the snapshot, with no replayed issue.
        let seq = f.s.auth.core.seq + 1;
        f.s.auth.journal_finalized[0] = Some(FinalEntry {
            seq,
            status_digest,
            executed,
        });
        let out = f.outcome(AuthRestore).unwrap();
        assert!(out.next.auth.core.instr.is_none());
        assert_eq!(out.next.auth.core.final_instr, 1);
        assert_eq!(out.next.auth.core.superseded_mismatch, u8::from(!executed));
        assert_eq!(
            out.obs.contains(&Some(Obs::ReapprovalRequested {
                cause: ReapprovalCause::SupersededMismatch,
                report: Some(status_digest),
            })),
            !executed
        );
    }

    let mut f = ready_to_decline();
    f.s.env.probe = Probe::Governance;
    f.run(&[HumanDecline, AuthBackup, OpReopen(0)]);
    let out = f.outcome(AuthRestore).unwrap();
    assert!(!out.next.auth.core.rooms[0].obl.unwrap().blocked);

    let mut f = Fake::new();
    f.step(AuthBackup);
    let status_digest = SKey {
        list_ref: REV,
        reset: false,
        manifest: bit(2),
        result: None,
    };
    f.s.auth.journal_finalized[0] = Some(FinalEntry {
        seq: 2,
        status_digest,
        executed: true,
    });
    f.s.auth.journal_declined[0] = Some(DeclineEntry {
        seq: 3,
        status_digest,
        declined_count: 1,
    });
    f.s.auth.journal_cleared[1] = 4;
    let out = f.outcome(AuthRestore).unwrap();
    assert!(out.obs.contains(&Some(Obs::ReplaySkipped {
        releases: [2, 0],
        declines: [3, 0],
        clears: [0, 4],
    })));
    assert_eq!(out.next.auth.core.final_instr, 0, "do not invent a target");
    assert!(out.next.auth.core.rooms.iter().all(|r| r.obl.is_none()));
    f.cfg.design = D.without(Fix::RestoredReplayTargets);
    let without = f.outcome(AuthRestore).unwrap();
    assert_eq!(without.next.auth.core.final_instr, 0);
    assert_eq!(without.obs, [None; 4], "missing targets were not audited");
}

/// H.18's rejected reading applies replay only to targets that replayed
/// entries created. The checker finds the restored instruction still listed
/// and the restored block still in force.
#[test]
fn h18_restored_targets_are_necessary() {
    let mut f = ready_to_decline();
    f.run(&[HumanRelease, AuthBackup]);
    let status_digest = f.s.auth.core.status.unwrap();
    f.s.auth.journal_finalized[0] = Some(FinalEntry {
        seq: f.s.auth.core.seq + 1,
        status_digest,
        executed: true,
    });
    ablate(
        Fix::RestoredReplayTargets,
        Prop::I8ReplayTarget,
        f.s.clone(),
        &[AuthRestore],
    );

    let mut f = ready_to_decline();
    f.s.env.probe = Probe::Governance;
    f.run(&[HumanDecline, AuthBackup, OpReopen(0)]);
    ablate(
        Fix::RestoredReplayTargets,
        Prop::I8ReplayTarget,
        f.s.clone(),
        &[AuthRestore],
    );
}

/// H.19's holder rule is a liveness obligation: without the fresh-epoch
/// status trigger, a zero-count holder never reports after an epoch change.
#[test]
fn h19_fresh_status_is_necessary_for_l1() {
    let p = Profile {
        budget: Budget {
            crashes: 0,
            faults: 0,
            changes: 0,
            ..BUDGET
        },
        liveness: true,
        skip: 0,
        max_states: 200_000,
        ..profile("h19-fresh-status", D)
    };
    let with = check(&p);
    assert!(with.violation.is_none(), "{}", with.render());
    let without = check(&Profile {
        design: D.without(Fix::JournalLostRecovery),
        ..p
    });
    assert_eq!(
        without.violation.as_ref().map(|v| v.prop),
        Some(Prop::L1Liveness),
        "{}",
        without.render()
    );
}

#[test]
fn h19_lost_journal_preserves_custody_and_requires_fresh_approval() {
    let mut f = ready_to_decline();
    f.run(&[HumanDecline, HumanRelease, AuthBackup]);
    let restored = f.s.auth.core;
    f.s.env.bad_journal = true;
    f.step(AuthRestore);
    let out = f.outcome(OpRestoreJournalLost).unwrap();
    assert_eq!(
        out.next.auth.journal_lost_evidence.as_deref(),
        Some(&restored)
    );
    assert!(out.obs.contains(&Some(Obs::ReapprovalRequested {
        cause: ReapprovalCause::RestoreJournalLost,
        report: None,
    })));
    let core = out.next.auth.core;
    assert_eq!(core.final_instr, 1);
    assert!(core.instr.is_none() && core.status.is_none() && core.status_receipts.is_empty());
    let obl = core.rooms[0].obl.unwrap();
    assert!(!obl.blocked && obl.blocked_by.is_none());
    assert_eq!(core.rooms[0].phase, restored.rooms[0].phase);
    assert_eq!(obl.accepted, restored.rooms[0].obl.unwrap().accepted);
    f.s = out.next;
    assert!(f.try_step(HumanRelease).is_none());
    assert_eq!(
        f.list().items[0],
        Item::Terminating(Some(RState::Incomplete))
    );
    f.step(OpAcceptEpoch);
    f.boot();
    f.list();
    f.request(2);
    assert!(matches!(f.reply(2), RespKind::Accepted { epoch: 1, .. }));
    f.step(HumanRelease);
    assert_eq!(f.s.auth.core.instr.unwrap().id, 1, "fresh human approval");
}

#[test]
fn h19_any_epoch_acceptance_triggers_zero_count_status_even_across_restart() {
    for design in [D, D.without(Fix::JournalLostRecovery)] {
        let mut f = Fake::with(design);
        f.boot();
        f.list();
        f.run(&[AuthBackup, AuthRestore]);
        f.list();
        f.step(OpAcceptEpoch);
        f.boot();
        f.list();
        f.step(Crash);
        f.boot();
        f.list();
        if design == D {
            assert!(f.s.dur.epoch_status_pending);
            f.request(2);
            assert_eq!(f.s.dur.slots[2].unwrap().key.status().unwrap().manifest, 0);
            assert!(matches!(f.reply(2), RespKind::Accepted { epoch: 1, .. }));
            assert!(!f.s.dur.epoch_status_pending);
        }
        assert!(f.try_step(Enqueue(2)).is_none());
    }
}

#[test]
fn h20_static_cas_failure_is_audited_500_with_no_commit() {
    let p = Proof::sign(1, 0);
    let mut f = report_in_flight();
    f.s = timed(f.s, p, 0, 420);
    f.s.env.envelope.as_mut().unwrap().commit_static_failed = true;
    let before = f.s.auth.clone();
    let action = AuthProcess { c: 0, idx: 0 };
    let out = f.outcome(action).unwrap();
    assert_eq!(
        out.next.net.resps[0].get(0).unwrap().kind,
        RespKind::InvariantViolation
    );
    assert!(out.obs.contains(&Some(Obs::CommitInvariantAudit)));
    assert_eq!(out.next.auth, before);
    ablate(
        Fix::CommitInvariant,
        Prop::I8CommitInvariant,
        f.s,
        &[action],
    );
}

fn exposure_fixture() -> Fake {
    let mut f = Fake::new();
    f.s.env.probe = Probe::Exposure;
    f.s.env.capability_skew = 30;
    f.boot();
    f.list();
    f
}

#[test]
fn h21_capability_deadline_and_a0_are_rechecked_at_release() {
    let mut f = exposure_fixture();
    let ask = StartCapability {
        room: 0,
        iat: 0,
        exp: 60,
    };
    f.run(&[AuthRetire(0), TickExposure(90), ask]);
    assert!(f.step(FinishCapability).contains(&RowId::C17));
    f.step(ask);
    ablate(
        Fix::EngineCapabilityFence,
        Prop::I1CapabilityExposure,
        f.s.clone(),
        &[TickExposure(91), FinishCapability],
    );
    f.step(TickExposure(91));
    let out = f.outcome(FinishCapability).unwrap();
    assert!(
        out.obs
            .contains(&Some(Obs::CapabilityRefused(CapabilityRefusal::Expired)))
    );
    assert!(!out.rows.iter().any(|r| r == RowId::C17));
    f.s = out.next;
    assert!(
        f.outcome(ask)
            .unwrap()
            .obs
            .contains(&Some(Obs::CapabilityRefused(CapabilityRefusal::Expired)))
    );

    // A0 of an unlisting (not just a retirement) also refuses an in-flight read.
    let mut f = exposure_fixture();
    f.run(&[
        StartCapability {
            room: 1,
            iat: 0,
            exp: 60,
        },
        AuthUnlist(1),
        FetchStart,
        ListDeliver,
    ]);
    let out = f.outcome(FinishCapability).unwrap();
    assert!(
        out.obs
            .contains(&Some(Obs::CapabilityRefused(CapabilityRefusal::NotActive)))
    );
}

#[test]
fn h21_clock_evidence_refuses_all_governed_reads_until_correction() {
    let mut f = exposure_fixture();
    let at_limit = StartCapability {
        room: 0,
        iat: 30,
        exp: 90,
    };
    let future = StartCapability {
        room: 0,
        iat: 31,
        exp: 91,
    };
    let long = StartCapability {
        room: 0,
        iat: 0,
        exp: 61,
    };
    for (action, why) in [
        (long, CapabilityRefusal::LifetimeTooLong),
        (future, CapabilityRefusal::FutureIat),
    ] {
        ablate(
            Fix::EngineCapabilityFence,
            Prop::I1CapabilityExposure,
            f.s.clone(),
            &[action],
        );
        let out = f.outcome(action).unwrap();
        assert!(out.obs.contains(&Some(Obs::CapabilityRefused(why))));
        assert!(out.next.vol.capability.is_none());
    }
    f.run(&[at_limit, FinishCapability, future]);
    let refused = f
        .outcome(StartCapability {
            room: 1,
            iat: 0,
            exp: 60,
        })
        .unwrap();
    assert!(refused.obs.contains(&Some(Obs::CapabilityRefused(
        CapabilityRefusal::ClockBehind
    ))));
    f.run(&[ClockSynchronized, at_limit, AuthRetire(0), FetchStart]);
    f.step(VerifyListPage { iat: 30 });
    assert!(!f.s.env.clock_behind_authority);
    f.step(VerifyListPage { iat: 31 });
    assert!(f.s.env.clock_behind_authority);
    f.step(VerifyListPage { iat: 29 });
    let out = f.outcome(FinishCapability).unwrap();
    assert!(out.obs.contains(&Some(Obs::CapabilityRefused(
        CapabilityRefusal::ClockBehind
    ))));

    let mut f = Fake::new();
    f.s.env.capability_skew = 31;
    assert!(f.try_step(Boot).is_none());
    f.cfg.design = D.without(Fix::EngineCapabilityFence);
    assert!(f.try_step(Boot).is_some());
}
