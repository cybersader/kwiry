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
use kwiry_lifecycle::step::{Config, exec, mintable, observed_remaining, servable};
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
const D: Design = Design::REV10;

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
    fn try_step(&mut self, a: Action) -> Option<Vec<RowId>> {
        let out = exec(&self.s, a, &self.cfg).unwrap()?;
        assert_eq!(props::check_step(&self.s, &out, 0), None, "{a:?}");
        assert_eq!(props::check_state(&out.next, 0), None, "{a:?}");
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
        changes: 0,
        ..BUDGET
    };
    p.liveness = true;
    p.skip = Prop::I8AuthorityFidelity.bit();
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
