//! The transition function.
//!
//! [`exec`] applies one [`Action`] to a [`State`] and returns the next state,
//! the §5.8 rows the step implements, and observations for the invariants.
//! `Ok(None)` means the action is not enabled (its guard is false, or the row
//! is refused at its first durable write while the reserve is exhausted, K5).
//!
//! Conventions:
//! - Each durable write (W, W[x], G, M, Q) is a separate action, so a crash
//!   can fall between any two of them. Side files written before a W
//!   (`sent/`, `receipts/`, `manifests/`) are folded into that W: a crash
//!   between them leaves only an orphan file with no effect on any guard.
//! - Every lifecycle write goes through [`write`], which implements M9
//!   (K2, K2x, K5).
//! - A crash or an operator stop discards all volatile state, the transport
//!   queue and every response in flight; requests already on the wire may
//!   still be processed by the authority, but their answers are lost.

use crate::action::{Action, FaultCode, faults};
use crate::bag::Bound;
use crate::design::{Design, Fix};
use crate::row_id::RowId::{self, *};
use crate::state::*;
use crate::wire::{
    self, AckClass, Envelope, Expiry, Proof, ProofTimes, RefInput, RefVerdict, Refusal,
};

/// Knobs that are not part of the state.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub design: Design,
    /// Enabled fault families (`action::faults`).
    pub faults: u16,
    /// Explore optional counts-only same-state updates (HW §14 Q7). They are
    /// never needed for progress, so profiles may leave them out.
    pub same_state: bool,
}

/// What an invariant needs to see about a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Obs {
    /// A `complete` report reached the wire. `true_claim`: at that instant
    /// the unattributed inventory was empty and nothing of the resource
    /// remained. `fresh`: the versions R2 recorded were unchanged.
    DispatchedComplete {
        o: u8,
        true_claim: bool,
        fresh: bool,
    },
    /// A floor rose on evidence minted in epoch `ev` while the holder's
    /// high-water is in epoch `cur`.
    FloorRaised {
        o: u8,
        ev: u8,
        cur: u8,
    },
    /// A unit was unlinked. `authorized`: an obligation listed for its
    /// resource, a release reservation, or GC of an unreferenced object of an
    /// active resource owned the deletion at that instant.
    Deleted {
        u: u8,
        claimant: Reason,
    },
    /// The holder entered a state only an operator can leave. `healthy`: the
    /// trigger was a correct authority or network response, not a fault.
    OperatorWait {
        healthy: bool,
    },
    SentWhileForbidden,
    DeclineInvisible,
    RootOpenedAsWrongHolder,
    UntrustedKey,
    AuthorityUnfaithful,
    WrongConvergenceEnvelope,
    ValidationPrecedence,
    ProofDeadlineMissed,
    UnverifiedRestore,
    CapabilityOverexposed,
    CapabilityRefused(CapabilityRefusal),
    ClockBehindAuthority,
    /// H.23: a nonce-bound page did not confirm a correction; the alert names
    /// `measured_lag_upper_bound` (`(iat − now_recv) + rtt`) and `rtt`.
    ClockLagAlert {
        bound: i16,
        rtt: u8,
    },
    /// H.23: an operator command cannot clear the hold.
    ClockClearRefused,
    /// H.25: an earlier-epoch list released results more than 90 s after
    /// minting stopped.
    EpochOverexposed,
    /// H.30.3: unsampled `lifecycle.mint_gate.cleared_by_lease` events, each
    /// with the H.11 silence alert for its holder: whether the modeled holder
    /// cleared by lease, and how many peers did.
    MintGateClearedByLease {
        holder: bool,
        peers: u8,
    },
    /// H.33 (rejected readings): an earlier-epoch list or page was issued
    /// after `T_stop`.
    EarlierEpochListAfterStop,
    /// H.30.3: no admin override clears a member early.
    MintOverrideRefused,
    ListWitnessOmitted,
    /// Unsampled `lifecycle.witness.unverifiable`; the model has one holder.
    WitnessUnverifiable(wire::WitnessReason),
    AdvisoryWitnessRejected,
    /// Unsampled `lifecycle.invariant_violation` from a failed static CAS check.
    CommitInvariantAudit,
    CommitInvariantMissed,
    /// H.18 `lifecycle.restore.replay_skipped` events, projected as entry
    /// kinds and seqs (zero means absent); no resource IDs are exposed.
    ReplaySkipped {
        releases: [u8; INSTRS],
        declines: [u8; ROOMS],
        clears: [u8; ROOMS],
    },
    /// A replayed finalization or clear left its target outstanding (H.18).
    ReplayTargetMissed,
    /// H.16.2 `lifecycle.root_pin.attest_refused`.
    PinAttestRefused(PinRefusal),
    /// H.16.3 replay skipped a `root_pin_attested` entry (audited).
    PinReplaySkipped,
    /// `None` means the request waits for a status accepted in the new epoch.
    ReapprovalRequested {
        cause: ReapprovalCause,
        report: Option<SKey>,
    },
    /// An armed epoch acceptance was consumed. `reused`: it was consumed
    /// before.
    AcceptanceConsumed {
        reused: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityRefusal {
    LifetimeTooLong,
    FutureIat,
    /// `capability_expired`: release no results.
    Expired,
    ClockBehind,
    NotActive,
}

/// H.16.2 attestation refusal codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinRefusal {
    HoldingUnknown,
    HoldingInactive,
    Reused,
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReapprovalCause {
    SupersededMismatch,
    RestoreJournalLost,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rows {
    ids: [Option<RowId>; 8],
}

impl Rows {
    fn push(&mut self, r: RowId) {
        if self.ids.contains(&Some(r)) {
            return;
        }
        let slot = self
            .ids
            .iter_mut()
            .find(|x| x.is_none())
            .expect("a step cites at most 8 rows");
        *slot = Some(r);
    }
    pub fn iter(&self) -> impl Iterator<Item = RowId> + '_ {
        self.ids.iter().map_while(|x| *x)
    }
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub next: State,
    pub rows: Rows,
    pub obs: [Option<Obs>; 4],
}

pub type Step = Result<Option<Outcome>, Bound>;

struct Out {
    n: State,
    rows: Rows,
    obs: [Option<Obs>; 4],
}

impl Out {
    fn new(s: &State) -> Out {
        Out {
            n: s.clone(),
            rows: Rows::default(),
            obs: [None; 4],
        }
    }
    fn row(&mut self, r: RowId) {
        self.rows.push(r);
    }
    fn obs(&mut self, o: Obs) {
        let slot = self
            .obs
            .iter_mut()
            .find(|x| x.is_none())
            .expect("a step makes at most 4 observations");
        *slot = Some(o);
    }
    fn done(self) -> Step {
        Ok(Some(Outcome {
            next: self.n,
            rows: self.rows,
            obs: self.obs,
        }))
    }
}

// ------------------------------------------------------------------ queries

pub fn room_class(s: &State, u: usize) -> Class {
    s.vol.class[UNIT_ROOM[u]]
}

pub fn reserved_units(dur: &Durable) -> u8 {
    dur.instr.iter().fold(0, |a, i| a | i.reserved_units())
}

/// The instruction whose outstanding reservation holds `u`.
pub fn reserving(dur: &Durable, u: usize) -> Option<u8> {
    (0..INSTRS)
        .find(|i| dur.instr[*i].reserved_units() & bit(u) != 0)
        .map(|i| i as u8)
}

fn present_mask(dur: &Durable) -> u8 {
    (0..UNITS)
        .filter(|u| dur.units[*u] != UDur::Deleted)
        .fold(0, |a, u| a | bit(u))
}

/// Generations compare without exclusion bits of units already deleted.
fn same_generation(dur: &Durable, a: Generation, b: Generation) -> bool {
    let m = present_mask(dur);
    Generation {
        excluded: a.excluded & m,
        ..a
    } == Generation {
        excluded: b.excluded & m,
        ..b
    }
}

pub fn referenced(s: &State, u: usize) -> bool {
    let r = UNIT_ROOM[u];
    let g = s.vol.mem_gen;
    u == ROOM_CUR[r] && (g.layout | g.withheld) & bit(r) != 0 && g.excluded & bit(u) == 0
}

pub fn remaining(s: &State, o: usize) -> bool {
    units_of(o).any(|u| s.dur.units[u] != UDur::Deleted)
}

/// Whether `u` is in the unattributed inventory (§5.5).
fn counted(s: &State, u: usize) -> bool {
    if s.dur.units[u] == UDur::Deleted {
        return false;
    }
    if reserving(&s.dur, u).is_some() {
        return true;
    }
    let c = room_class(s, u);
    match s.dur.units[u] {
        UDur::Present => matches!(c, Class::Unknown | Class::TombUnlisted),
        UDur::Quarantined(q) => match q {
            QOwner::Release(_) | QOwner::Unattributed => true,
            QOwner::Obligation => false,
            QOwner::Derived => matches!(c, Class::Unknown | Class::TombUnlisted),
        },
        UDur::Deleted => false,
    }
}

pub fn inventory(s: &State) -> u8 {
    (0..UNITS)
        .filter(|u| counted(s, *u))
        .fold(0, |a, u| a | bit(u))
}

pub fn servable(s: &State, r: usize) -> bool {
    if s.env.legacy_started {
        return s.dur.generation.layout & bit(r) != 0;
    }
    s.vol.phase == Phase::P7
        && s.env.clock < s.vol.lease_until
        && !earlier_epoch_lapsed(s)
        && s.vol.route != Route::Halted403
        && s.vol.class[r] == Class::Active
        && s.dur.tomb & bit(r) == 0
        && s.vol.block & bit(r) == 0
        && pin_servable(s, r)
}

/// H.30.2(b): once the authority clock passes `T_stop +
/// serve_seconds_ceiling + 150 s`, a lease from a list of the stopped lineage
/// has lapsed (no such list was issued after `T_stop`, H.33). The model's
/// lists stop only in epoch 0, so before a restore's first transaction the
/// stopped lineage is the current epoch.
fn earlier_epoch_lapsed(s: &State) -> bool {
    s.env.lease_timeout
        && s.vol
            .applied
            .is_some_and(|l| l.rev.epoch < s.auth.core.epoch || s.auth.core.epoch == 0)
}

/// H.16.5: a non-null pin from a list applied in this process, artifacts
/// derived under its `pin_ref`, no AP24 conflict, and the root observed at
/// `pin_id`. On a mismatch only H10.5 bounded stale service of those
/// same-ref artifacts remains (H.28).
pub fn pin_servable(s: &State, r: usize) -> bool {
    let Some(p) = s.vol.applied.and_then(|l| l.pins[r]) else {
        return false;
    };
    let mismatch = s.env.root_dir[r] != p.id;
    (s.dur.derived[r] == Some(p.pin_ref)
        || s.vol.adopted & bit(r) != 0
        || (s.vol.stale_carry & bit(r) != 0 && mismatch && !s.env.stale_expired))
        && s.vol.pin_conflict & bit(r) == 0
        && (!mismatch || !s.env.stale_expired)
}

/// H.21.5, H.23, H.26, H.34: the request path reads the in-memory hold,
/// which every start loads from holder state; a stopped engine has only the
/// durable flag.
pub fn clock_held(s: &State) -> bool {
    if s.vol.phase == Phase::Down {
        s.dur.clock_behind
    } else {
        s.vol.clock_hold
    }
}

/// The H.19 status-first gate (a holder choice H.22.1 permits): after an
/// epoch acceptance, obligation reports wait for the fresh status's
/// current-epoch receipt. H.22.3: a status bug halt releases it. H.27: the
/// gate is derived from durable holder state, open once a current-epoch
/// status ack is recorded (`epoch_status_pending` cleared) or while the
/// status is durably `halted`; an N5 resume (`halted` → `invalidated`)
/// closes it again until the re-derived status is acknowledged.
pub fn status_gate_shut(s: &State, d: Design) -> bool {
    let released = if d.has(Fix::DurableStatusGate) {
        s.dur.slots[STATUS].is_some_and(|p| p.st == DurSt::Halted)
    } else {
        s.vol.gate_released
    };
    s.dur.epoch_status_pending && !(d.has(Fix::StatusGateRelease) && released)
}

/// AP24 `root_pin_previously_listed`: the holder's pin-history holds the
/// list's `pin_id` under another room.
fn history_conflict(dur: &Durable, d: Design, r: usize, pin: Option<Pin>) -> bool {
    d.has(Fix::RootPinHistory)
        && pin.is_some_and(|p| dur.pin_history[p.id as usize].is_some_and(|o| o as usize != r))
}

/// H.16.5 effective point: rooms an admitted list shows `active` with a
/// changed, null or history-conflicting pin, refused at A0 until A4.
/// H.24: "changed" is judged against the same holding's pin in the list last
/// applied in this process; at process start that baseline is empty, so every
/// pin of the first list is changed.
fn pin_refusal(s: &State, d: Design, l: &ListMsg) -> u8 {
    let baseline = s.vol.applied.map(|a| a.pins);
    (0..ROOMS)
        .filter(|&r| {
            l.items[r] == Item::Active
                && (l.pins[r].is_none()
                    || baseline.is_none_or(|b| l.pins[r] != b[r])
                    || history_conflict(&s.dur, d, r, l.pins[r]))
        })
        .fold(0, |a, r| a | bit(r))
}

pub fn publisher_idle(s: &State) -> bool {
    s.vol.apply.is_none() && s.vol.rel_txn.is_none()
}

/// The release outcome the next status report must carry (L6, L10).
pub fn release_outcome(s: &State) -> Option<RelResult> {
    let listed = |i: usize| {
        s.vol
            .applied
            .and_then(|l| l.instr)
            .is_some_and(|x| x.id as usize == i)
    };
    let reported = |r: RelResult| s.dur.status_acc.and_then(|k| k.result) == Some(r);
    (0..INSTRS).find_map(|i| {
        let result = |executed: bool, observed| RelResult {
            instr: i as u8,
            executed,
            observed,
        };
        match s.dur.instr[i] {
            IDur::Deleted(_) if s.vol.resulted & bit(i) != 0 => Some(result(true, 0)),
            IDur::Mismatch(m) => Some(result(false, m)),
            IDur::Done if listed(i) && !reported(result(true, 0)) => Some(result(true, 0)),
            IDur::MismatchReported(m) if listed(i) && !reported(result(false, m)) => {
                Some(result(false, m))
            }
            _ => None,
        }
    })
}

/// Physical census, independent of the report scan and registry reclamation.
/// An unlinked but open unit and an unregistered builder output both remain.
pub fn observed_remaining(s: &State, o: usize) -> bool {
    units_of(o).any(|u| {
        s.dur.units[u] != UDur::Deleted
            || (s.env.open_unlinked | s.env.late_published) & bit(u) != 0
    })
}

/// Check the exact claimant, not the disjunction of reasons for a unit.
pub fn delete_authorized(s: &State, u: usize, claimant: Reason) -> bool {
    let c = room_class(s, u);
    if (s.dur.units[u] == UDur::Present && referenced(s, u)) || s.env.open_unlinked & bit(u) != 0 {
        return false;
    }
    match claimant {
        Reason::Obligation(ob) => {
            ob as usize == UNIT_ROOM[u] && c.obligated() && reserving(&s.dur, u).is_none()
        }
        Reason::Gc => c == Class::Active && reserving(&s.dur, u).is_none(),
        Reason::Release(i) => {
            matches!(s.dur.instr[i as usize], IDur::Draining(m) if m & bit(u) != 0)
                && (s.vol.umem[u] == UMem::Revoked(Reason::Release(i))
                    || s.dur.units[u] == UDur::Quarantined(QOwner::Release(i)))
        }
    }
}

pub fn classify(l: &ListMsg, tomb: u8) -> [Class; ROOMS] {
    std::array::from_fn(|r| {
        let t = tomb & bit(r) != 0;
        match l.items[r] {
            Item::Active if t => Class::Conflict,
            Item::Active => Class::Active,
            Item::Terminating(_) | Item::Blocked(_) => Class::Obligated,
            Item::Retired => Class::RetiredComplete,
            Item::Absent if t => Class::TombUnlisted,
            Item::Absent => Class::Unknown,
        }
    })
}

fn generation_for(classes: &[Class; ROOMS], reserved: u8) -> Generation {
    let rooms = |want: Class| {
        (0..ROOMS)
            .filter(|r| classes[*r] == want)
            .fold(0, |a, r| a | bit(r))
    };
    Generation {
        layout: rooms(Class::Active),
        withheld: rooms(Class::Unknown),
        excluded: reserved,
    }
}

/// The M4b row a class change implements.
fn class_row(old: Class, new: Class) -> Option<RowId> {
    use Class::*;
    Some(match (old, new) {
        (Unknown, Active) => C5,
        (_, Active) => C1,
        (Active, Unknown) => C4,
        (Obligated, Obligated) => C12,
        (_, Obligated) => C6,
        (_, RetiredComplete) => C7,
        (Obligated | RetiredComplete, TombUnlisted) => C8,
        (_, Conflict) => C9,
        _ => return None,
    })
}

fn obligation_view(i: Item) -> Option<(bool, Option<RState>)> {
    match i {
        Item::Terminating(l) | Item::Blocked(l) => Some((false, l)),
        Item::Retired => Some((true, Some(RState::Complete))),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Adm {
    Admit(RowId),
    Refuse(RowId, Option<u8>),
}

/// M2: whether a verified list is admitted, and by which H row.
fn admission(dur: &Durable, l: &ListMsg) -> Adm {
    let Some(hw) = dur.hw else {
        return Adm::Admit(H1);
    };
    let same = l.rev.epoch == hw.epoch;
    match dur.accept {
        None if same && l.rev.seq >= hw.seq => Adm::Admit(H2),
        None if same => Adm::Refuse(H3, None),
        None => Adm::Refuse(H4, Some(l.rev.epoch)),
        Some(a) if l.rev.epoch == a.to && hw == a.from => Adm::Admit(H6),
        Some(_) if same && l.rev.seq >= hw.seq => Adm::Admit(H7),
        Some(_) if same => Adm::Refuse(H3, None),
        Some(_) => Adm::Refuse(H8, Some(l.rev.epoch)),
    }
}

/// Rooms a list does not show active (or that are tombstoned).
fn narrowing(l: &ListMsg, tomb: u8) -> u8 {
    (0..ROOMS)
        .filter(|r| l.items[*r] != Item::Active || tomb & bit(*r) != 0)
        .fold(0, |a, r| a | bit(r))
}

fn list_view(core: &AuthCore) -> ListMsg {
    let items = core.rooms.map(|r| match r.phase {
        APhase::Active => Item::Active,
        APhase::Unlisted => Item::Absent,
        APhase::Terminating => {
            let last = r.obl.and_then(|o| o.accepted).map(|a| a.state);
            if r.obl.is_some_and(|o| o.blocked) {
                Item::Blocked(last)
            } else {
                Item::Terminating(last)
            }
        }
        APhase::Retired => Item::Retired,
    });
    ListMsg {
        rev: Rev {
            epoch: core.epoch,
            seq: core.seq,
        },
        items,
        holder_rev: core.holder_rev,
        instr: core.instr,
        pins: std::array::from_fn(|r| {
            if core.rooms[r].phase == APhase::Unlisted {
                None
            } else {
                core.pins[r]
            }
        }),
    }
}

// ------------------------------------------------------------ state helpers

#[derive(Clone, Copy)]
enum Bump {
    Obl(usize),
    Res(usize),
    Inv,
    Release,
}

/// A version bump: every record that read the bumped version goes stale.
fn bump(n: &mut State, b: Bump) {
    let hits_obl = |o: usize| match b {
        Bump::Obl(x) | Bump::Res(x) => x == o,
        Bump::Inv => true,
        Bump::Release => false,
    };
    for o in 0..OBLS {
        if hits_obl(o)
            && let Some(sc) = n.vol.scans[o].as_mut()
        {
            sc.clean = false;
        }
    }
    for c in 0..CHANS {
        let hit = if c < OBLS {
            hits_obl(c)
        } else {
            matches!(b, Bump::Inv | Bump::Release)
        };
        if hit {
            match &mut n.vol.slotv[c] {
                SlotVol::Armed { clean } | SlotVol::Queued { clean, .. } => *clean = false,
                SlotVol::Idle | SlotVol::Sent { .. } => {}
            }
        }
    }
}

fn refresh_inv(n: &mut State) {
    let inv = inventory(n);
    if inv != n.vol.inv {
        n.vol.inv = inv;
        bump(n, Bump::Inv);
    }
}

/// M9: one lifecycle write. `None`: refused at its first W while the reserve
/// is exhausted (K5), so the row is not enabled. `Some(false)`: the write hit
/// ENOSPC and the reserve could not cover it (K2x): the calling row fails
/// with no further state change.
fn write(o: &mut Out) -> Option<bool> {
    if o.n.vol.rsv == Rsv::Exhausted {
        return None;
    }
    if !o.n.env.disk_full {
        return Some(true);
    }
    if o.n.dur.reserve {
        o.n.dur.reserve = false;
        o.n.vol.rsv = Rsv::Partial;
        o.row(K2);
        Some(true)
    } else {
        o.n.vol.rsv = Rsv::Exhausted;
        o.row(K2x);
        Some(false)
    }
}

/// Discard all volatile state (crash, shutdown, operator stop).
fn stop(n: &mut State) {
    if n.vol.clock_unpersisted {
        // H.26: a crash before C19's write loses only the flag. Nothing is
        // served until a list of the next process applies, and its pages
        // are measured again; the evidence is gone with the process.
        n.ghost.lag_proven = false;
    }
    n.vol = Volatile::down();
    n.env.recovering = false;
    n.env.legacy_started = false;
    n.env.open_unlinked = 0;
    n.net.list = None;
    for c in 0..CHANS {
        n.net.reqs[c].map_in_place(|r| r.alive = false);
        n.net.resps[c].retain(|r| r.late);
    }
}

/// Whether the slot's current or earlier attempts may have reached the
/// authority.
fn possibly_tx(p: &Pending, v: SlotVol) -> bool {
    p.earlier || (p.st == DurSt::Armed && matches!(v, SlotVol::Sent { .. } | SlotVol::Idle))
}

fn invalidate_slot(n: &mut State, c: usize) -> Result<(), Bound> {
    if let Some(p) = n.dur.slots[c]
        && p.st != DurSt::Invalidated
    {
        if possibly_tx(&p, n.vol.slotv[c]) {
            n.dur.uncertain[c].insert(p.key, "uncertain keys per channel")?;
        }
        n.dur.slots[c] = Some(Pending {
            key: p.key,
            st: DurSt::Invalidated,
            earlier: false,
        });
        n.vol.slotv[c] = SlotVol::Idle;
    }
    Ok(())
}

fn replace_slot(n: &mut State, c: usize, key: Key) -> Result<(), Bound> {
    if let Some(p) = n.dur.slots[c]
        && possibly_tx(&p, n.vol.slotv[c])
        && p.key != key
    {
        n.dur.uncertain[c].insert(p.key, "uncertain keys per channel")?;
    }
    n.dur.slots[c] = Some(Pending {
        key,
        st: DurSt::Ready,
        earlier: false,
    });
    n.vol.slotv[c] = SlotVol::Idle;
    Ok(())
}

fn clear_slot(n: &mut State, c: usize) {
    n.dur.slots[c] = None;
    n.vol.slotv[c] = SlotVol::Idle;
}

fn raise_floor(o: &mut Out, ob: usize, state: RState, key: Option<OKey>, ev: u8) {
    let cur = o.n.dur.hw.map_or(0, |h| h.epoch);
    match o.n.dur.floors[ob] {
        Some(f) if f.state > state => {}
        Some(f) if f.state == state => {
            if key.is_some() {
                o.n.dur.floors[ob] = Some(Floor { key, ..f });
            }
        }
        _ => {
            o.n.dur.floors[ob] = Some(Floor {
                epoch: cur,
                state,
                key,
            });
            o.obs(Obs::FloorRaised {
                o: ob as u8,
                ev,
                cur,
            });
        }
    }
}

/// L7 / L8 inside a status receipt's commit.
fn settle_release(o: &mut Out, rr: Option<RelResult>) {
    let Some(r) = rr else { return };
    let i = r.instr as usize;
    if i >= INSTRS {
        return;
    }
    match (o.n.dur.instr[i], r.executed) {
        (IDur::Deleted(_) | IDur::Done, true) => {
            o.n.dur.instr[i] = IDur::Done;
            o.row(L7);
        }
        (IDur::Mismatch(_) | IDur::MismatchReported(_), false) => {
            o.n.dur.instr[i] = IDur::MismatchReported(r.observed);
            o.row(L8);
        }
        _ => {}
    }
}

fn halt(o: &mut Out, d: Design, to: Route, healthy: bool) -> Result<(), Bound> {
    match (o.n.vol.route, to) {
        (Route::Normal, Route::Halted401) => {
            o.n.vol.route = Route::Halted401;
            o.row(N1);
            o.obs(Obs::OperatorWait { healthy });
        }
        (r, Route::Halted403) if r != Route::Halted403 => {
            o.n.vol.route = Route::Halted403;
            o.row(N2);
            o.row(C11);
            if d.has(Fix::CancelOnForbidden) {
                for c in 0..CHANS {
                    if matches!(
                        o.n.vol.slotv[c],
                        SlotVol::Armed { .. } | SlotVol::Queued { .. } | SlotVol::Sent { .. }
                    ) {
                        invalidate_slot(&mut o.n, c)?;
                        o.row(N2c);
                    }
                }
            }
            o.obs(Obs::OperatorWait { healthy });
        }
        _ => {}
    }
    Ok(())
}

fn proof_expiry(s: &mut State, d: Design, c: usize) -> Expiry {
    let result = wire::expiry(d, s.vol.proof_retried & bit(c) != 0);
    match result {
        Expiry::Resign => s.vol.proof_retried |= bit(c),
        Expiry::BackoffClockAlert => {
            s.vol.proof_retried &= !bit(c);
            s.vol.proof_backoff |= bit(c);
        }
        Expiry::Halt => {}
    }
    result
}

fn proof_backoff_elapsed(s: &State, c: usize) -> Step {
    if s.vol.proof_backoff & bit(c) == 0 {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.vol.proof_backoff &= !bit(c);
    o.done()
}

fn fault_ok(s: &State, cfg: &Config, family: u16) -> bool {
    cfg.faults & family != 0 && s.budget.faults > 0
}

fn quarantine_owner(d: Design, rev8: QOwner) -> QOwner {
    if d.has(Fix::DerivedQuarantineOwner) {
        QOwner::Derived
    } else {
        rev8
    }
}

const fn ch_row(c: usize, obl: RowId, status: RowId) -> RowId {
    if c < OBLS { obl } else { status }
}

// --------------------------------------------------------------------- exec

pub fn exec(s: &State, a: Action, cfg: &Config) -> Step {
    use Action::*;
    let d = cfg.design;
    match a {
        Boot => boot(s, d),
        Markers => markers(s),
        Narrow => narrow(s),
        FetchStart => fetch_start(s, d),
        ListDeliver => list_deliver(s, d),
        Apply => apply(s, d),
        RelBind(i) => rel_bind(s, i as usize),
        RelDetach(i) => rel_detach(s, d, i as usize),
        RelDrain(i) => rel_drain(s, i as usize),
        RelDelete { i, u, fail } => rel_delete(s, cfg, i as usize, u as usize, fail),
        RelDeleted(i) => rel_deleted(s, i as usize),
        RelResult(i) => rel_result(s, i as usize),
        GcRevoke(u) => gc_revoke(s, u as usize),
        GcDelete { u, fail } => gc_delete(s, cfg, u as usize, fail),
        Quarantine(u) => quarantine(s, d, u as usize),
        QRetry { u, fail } => qretry(s, cfg, u as usize, fail),
        PurgeDelete { u, fail } => purge_delete(s, cfg, u as usize, fail),
        PurgeScan(o) => purge_scan(s, d, o as usize),
        Enqueue(c) => enqueue(s, d, c as usize, cfg.same_state),
        Transmit(c) => transmit(s, d, c as usize),
        Handoff(c) => handoff(s, c as usize),
        Dispatch(c) => dispatch(s, d, c as usize),
        Deliver { c, idx } => deliver(s, d, c as usize, idx as usize),
        Replenish => replenish(s),
        AuthProcess { c, idx } => auth_process(s, d, c as usize, idx as usize),
        DropDead { c, idx } => drop_dead(s, c as usize, idx as usize),
        HumanRelease => human_release(s),
        HumanDecline => human_decline(s, d),
        OpReopen(ob) => reopen(s, ob as usize),
        SpaceFreed => space_freed(s),
        AuthFault { c, idx, code } => auth_fault(s, cfg, c as usize, idx as usize, code),
        NetFail(c) => net_fail(s, cfg, c as usize),
        NetDup { c, idx } => net_dup(s, cfg, c as usize, idx as usize),
        Detach { c, idx } => detach(s, cfg, c as usize, idx as usize),
        AgeOut(c) => age_out(s, cfg, c as usize),
        DelayCommit(c) => delay_commit(s, cfg, c as usize),
        ProofBackoffElapsed(c) => proof_backoff_elapsed(s, c as usize),
        ListFault(code) => list_fault(s, cfg, code),
        ListDup => list_dup(s, cfg),
        ListLost => list_lost(s, cfg),
        DiskFull => disk_full(s, cfg),
        Crash => crash(s),
        AuthRetire(r) => auth_retire(s, r as usize),
        AuthUnlist(r) => auth_unlist(s, r as usize),
        AuthRelist(r) => auth_relist(s, r as usize),
        AuthDrop(r) => auth_drop(s, r as usize),
        AuthBackup => auth_backup(s),
        AuthRestore => auth_restore(s, d, false),
        OpRestoreJournalLost => auth_restore(s, d, true),
        TickExposure(t) => tick_exposure(s, t),
        AnswerCapability {
            room,
            expiry_with_skew,
        } => answer_capability(s, d, room as usize, expiry_with_skew),
        StartCapability { room, iat, exp } => start_capability(s, d, room, iat, exp),
        FinishCapability => finish_capability(s, d),
        VerifyListPage { iat, rtt } => observe_list_clock(s, d, iat, rtt),
        ClockProbe => clock_probe(s),
        ProbeWindow => probe_window(s, d),
        ClockStep { lag } => clock_step(s, d, lag),
        OpClearClock => op_clear_clock(s, d),
        PersistClockHold => persist_clock_hold(s, d),
        PeerEnroll(p) => peer_enroll(s, d, p as usize),
        PeerDisable(p) => peer_set_enabled(s, d, p as usize, false),
        PeerEnable(p) => peer_set_enabled(s, d, p as usize, true),
        PeerRotate(p) => peer_rotate(s, d, p as usize),
        PeerMove(p) => peer_move(s, p as usize),
        PeerReport(p) => peer_report(s, d, p as usize),
        PeerSilent(p) => peer_silent(s, p as usize),
        PeerAnswer(p) => peer_answer(s, p as usize),
        LeaseTimeout => lease_timeout(s, d),
        OpMintOverride => op_mint_override(s, d),
        PeerForbidden { p, received } => peer_forbidden(s, d, p as usize, received),
        PeerRefusalAged(p) => peer_refusal_aged(s, d, p as usize),
        PeerListStart(p) => peer_list_start(s, p as usize),
        PeerListPage(p) => peer_list_page(s, p as usize),
        PeerOldList(p) => peer_old_list(s, p as usize),
        OpEpoch => op_epoch(s, d),
        RestoreBegin => restore_begin(s),
        AttestRoot { room, dir, stale } => attest_root(s, room, dir, stale),
        RepointRoot { room, dir } => repoint_root(s, room as usize, dir),
        PinRebuild(r) => pin_rebuild(s, d, r as usize),
        StaleDeadline => stale_deadline(s),
        OpAcceptEpoch => op_accept_epoch(s),
        OpRestart => op_restart(s),
        OpClearHalt(c) => op_clear_halt(s, d, c as usize),
        SwapHolder
        | LoseHolderState
        | BeginRecovery
        | CommitRecovery
        | TryLegacy
        | LeaseDeadline
        | KeepOpen
        | CloseHandle
        | LatePublish
        | ReconcileLate
        | WrongRelease { .. } => probe(s, d, a),
    }
}

/// The checker's state quotient. It erases history that no later step,
/// observation or property reads, so two states that differ only in it have
/// the same future (`tests/projection.rs` checks this as a bisimulation on
/// every reachable state of several profiles):
///
/// - An authority idempotency receipt (obligation or status) is read only
///   when a request with that exact body arrives. It is kept while the body is
///   live: on the wire, in a holder slot (which may be re-dispatched), or in
///   the holder's uncertain set. It is also kept while the holder could derive
///   the body again. A body names `list_ref = hw`, and within an epoch `hw`
///   only rises, so a body whose `list_ref` is below both the holder's
///   high-water and the authority's `seq` in the same epoch can never be sent
///   again. The newest obligation receipt (the H.9 "last accepted report") and
///   the current status body are always kept. Focused probes, which can reset
///   or swap holder state, are not projected.
/// - `Floor::key` is read only by the optional same-state rule (R16), and
///   only its `manifest` and `remaining`. It is erased when that rule is not
///   explored, and its `list_ref` is erased when it is.
/// - The holder's accepted status body is read for its counts and result,
///   and its `list_ref` only by equality with a body derived at `hw`. Once
///   that `list_ref` is below `hw` it can never be equal again, so it becomes
///   `seq 0`, which no list carries.
/// - With static pins (no root-pin probe) the holder's pin-history, which
///   then can never conflict.
/// - Audit-only authority data no step reads: `blocked_by`,
///   `superseded_mismatch`, the journal-loss audit flag, and the journal
///   itself once no restore remains in the budget.
/// - With `peer_symmetry` (the H.30 fixtures), the order of the peers.
/// - The H.30 seats while no gate is closed: only a closed gate reads them
///   (reports, `403`s, the lease timeout and the override all require one,
///   and the next gate assigns every seat afresh).
/// - The H.31 journaled roster entries when the journal is corrupt and H.14
///   fences it: every supported restore then fails verification (H12j), and
///   `restore_journal_lost` replays no journal.
/// - A peer's key generation while no backup or journaled copy exists and
///   its signing key matches its roster entry: only that equality is read.
pub fn canonical(s: &mut State, cfg: &Config) {
    let core = &mut s.auth.core;
    core.superseded_mismatch = 0;
    for obl in core.rooms.iter_mut().filter_map(|r| r.obl.as_mut()) {
        obl.blocked_by = None;
    }
    s.auth.journal_lost_audited = false;
    if s.budget.restores == 0 {
        let a = &mut s.auth;
        a.journal_retired = 0;
        a.journal_declined = [None; ROOMS];
        a.journal_cleared = [0; ROOMS];
        a.journal_finalized = [None; INSTRS];
        a.journal_issued = [None; INSTRS];
    }
    for f in s.dur.floors.iter_mut().flatten() {
        if !cfg.same_state {
            f.key = None;
        } else if let Some(k) = f.key.as_mut() {
            k.list_ref = Rev { epoch: 0, seq: 0 };
        }
    }
    // H.30 peers are interchangeable: every fact about one is in its record,
    // every peer action exists for each index, and no property or
    // observation names a peer's position.
    let dead_journal = s.env.bad_journal && cfg.design.has(Fix::AtomicJournalReplay);
    let no_backup = s.auth.backup.is_none();
    for p in s.peers.iter_mut() {
        if s.auth.mint_gate.is_none() {
            p.seat = Seat::Absent;
        }
        if dead_journal {
            p.journaled = None;
        }
        // Without a backup or journaled copy, a peer's key generation is
        // read only as the equality of its signing key with its roster key,
        // so a rotation is a renaming (it leaves at most a rotation
        // self-loop, which is a scenario step).
        if no_backup
            && p.backup.is_none()
            && p.journaled.is_none()
            && let Some(r) = p.roster.as_mut()
            && r.key == p.key
        {
            r.key = 0;
            p.key = 0;
        }
    }
    if s.env.peer_symmetry {
        s.peers.sort_unstable();
    }
    if s.env.probe != Probe::None {
        return;
    }
    // Outside the root-pin probe every room keeps its initial `pin_id`, and
    // the pin-history only ever maps a directory to that room, so it can
    // never conflict: it is unread. (Which pairs it holds records only
    // whether a room was listed before an unlisting.)
    s.dur.pin_history = [None; DIRS];
    let Some(hw) = s.dur.hw else {
        return;
    };
    if let Some(acc) = s.dur.status_acc.as_mut()
        && acc.list_ref.epoch == hw.epoch
        && acc.list_ref.seq < hw.seq
    {
        acc.list_ref.seq = 0;
    }
    // An invalidated slot is dormant: it is never transmitted, and an attempt
    // that may have been sent is already in `uncertain`. Only an obligation
    // key's state is read (R6, R13 and R18 clear a slot a floor overtakes).
    for slot in s.dur.slots.iter_mut() {
        if let Some(p) = slot.as_mut()
            && p.st == DurSt::Invalidated
            && !p.earlier
        {
            p.key = dormant_key(p.key);
        }
    }
    let (epoch, seq) = (s.auth.core.epoch, s.auth.core.seq);
    let dead = |r: Rev| r.epoch == hw.epoch && r.epoch == epoch && r.seq < hw.seq && r.seq < seq;
    let (net, dur) = (&s.net, &s.dur);
    let live = |k: Key| {
        (0..CHANS).any(|c| {
            net.reqs[c].iter().any(|q| q.key == k)
                || dur.slots[c].is_some_and(|p| p.key == k)
                || dur.uncertain[c].contains(&k)
        })
    };
    // A zero-count, result-free status is read only by the status floor
    // (`k.list_ref.seq < last.list_ref.seq`); release and decline need a
    // non-zero manifest. Once no live or derivable body can fall below its
    // `list_ref`, it is equivalent to no current status.
    let floor_moot = |last: SKey| {
        last.manifest == 0
            && last.result.is_none()
            && (last.list_ref.epoch != epoch
                || ((hw.epoch != epoch || hw.seq >= last.list_ref.seq)
                    && (0..CHANS).all(|c| {
                        let low = |k: Key| {
                            k.status().is_some_and(|k| {
                                k.list_ref.epoch == epoch && k.list_ref.seq < last.list_ref.seq
                            })
                        };
                        !net.reqs[c].iter().any(|q| low(q.key))
                            && !dur.slots[c]
                                .is_some_and(|p| p.st != DurSt::Invalidated && low(p.key))
                            && !dur.uncertain[c].iter().any(low)
                    })))
    };
    let status_moot = s.auth.core.status.is_some_and(floor_moot);
    let core = &mut s.auth.core;
    for obl in core.rooms.iter_mut().filter_map(|r| r.obl.as_mut()) {
        let newest = obl.receipts.iter().map(|(_, _, at)| at).max();
        obl.receipts
            .retain(|&(k, _, at)| Some(at) == newest || live(Key::O(k)) || !dead(k.list_ref));
        // A receipt's `at` is read only to find the newest receipt of its
        // obligation, so keep its rank and forget the absolute seq (which
        // records how other channels' commits interleaved).
        let mut ats: Vec<u8> = obl.receipts.iter().map(|(_, _, at)| at).collect();
        ats.sort_unstable();
        ats.dedup();
        obl.receipts.map_in_place(|r| {
            r.2 = ats.iter().position(|&at| at == r.2).expect("own value") as u8 + 1;
        });
    }
    if status_moot {
        core.status = None;
    }
    let current = core.status;
    core.status_receipts
        .retain(|&(k, _, _)| Some(k) == current || live(Key::S(k)) || !dead(k.list_ref));
    // The holder reads its accepted status only for the counts, the result
    // and the equality with a newly derived body. A zero-count, result-free
    // body is equivalent to none unless a reset or fresh-epoch trigger could
    // re-derive it exactly.
    let d = &mut s.dur;
    if !d.reset_pending
        && !d.epoch_status_pending
        && d.status_acc
            .is_some_and(|k| k.manifest == 0 && k.result.is_none())
    {
        d.status_acc = None;
    }
}

/// The placeholder key of an invalidated slot (see [`canonical`]).
fn dormant_key(k: Key) -> Key {
    let list_ref = Rev { epoch: 0, seq: 0 };
    match k {
        Key::O(k) => Key::O(OKey {
            list_ref,
            manifest: 0,
            remaining: false,
            ..k
        }),
        Key::S(_) => Key::S(SKey {
            list_ref,
            reset: false,
            manifest: 0,
            result: None,
        }),
    }
}

// ------------------------------------------------------------- M1 startup

fn boot(s: &State, d: Design) -> Step {
    if s.vol.phase != Phase::Down
        || s.env.legacy_started
        || (d.has(Fix::EngineCapabilityFence) && s.env.capability_skew > 30)
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.vol = Volatile::down();
    if s.dur.state_holder.is_none() && present_mask(&s.dur) == 0 && s.dur.root_holder.is_none() {
        o.row(S4);
        o.n.dur.state_holder = Some(s.env.holder);
        o.n.dur.root_holder = Some(s.env.holder);
        o.n.dur.hw = None;
        o.n.dur.last_list_ref = None;
        o.n.dur.reset_pending = true;
        // H.26: a fresh holder state starts with the clock flag clear; the
        // first nonce-bound page measures the clock again.
        o.n.dur.clock_behind = false;
        o.n.ghost.lag_proven = false;
    } else if s.dur.state_holder == Some(s.env.holder)
        && present_mask(&s.dur) == 0
        && s.dur.root_holder.is_none()
    {
        o.n.dur.root_holder = Some(s.env.holder);
    } else if s.dur.state_holder.is_none() {
        return Ok(None); // S5: non-empty or claimed root, no holder state
    } else if s.dur.root_holder != Some(s.env.holder) || s.dur.state_holder != Some(s.env.holder) {
        if d.has(Fix::RootHolderBinding) {
            return Ok(None);
        }
        o.obs(Obs::RootOpenedAsWrongHolder);
    }
    o.n.vol.phase = Phase::P3;
    // H.26: the in-memory hold is loaded from holder state.
    o.n.vol.clock_hold = o.n.dur.clock_behind;
    o.row(S6);
    o.row(S7);
    if o.n.dur.reserve {
        o.n.vol.rsv = Rsv::Full;
        o.row(K1);
    } else if !o.n.env.disk_full {
        o.n.dur.reserve = true;
        o.n.vol.rsv = Rsv::Full;
        o.row(K1);
    } else {
        o.n.vol.rsv = Rsv::Insufficient;
        o.row(K1i);
    }
    o.done()
}

fn markers(s: &State) -> Step {
    if s.vol.phase != Phase::P3 {
        return Ok(None);
    }
    let mut o = Out::new(s);
    if s.dur.tomb != 0 && !s.dur.rollback_closed {
        o.row(S8);
        match write(&mut o) {
            None => return Ok(None),
            Some(false) => return o.done(),
            Some(true) => o.n.dur.rollback_closed = true,
        }
    } else {
        o.row(S8n);
    }
    o.n.vol.phase = Phase::P3a;
    o.done()
}

fn narrow(s: &State) -> Step {
    if s.vol.phase != Phase::P3a {
        return Ok(None);
    }
    let res = reserved_units(&s.dur);
    let g = s.dur.generation;
    let g2 = Generation {
        layout: g.layout & !s.dur.tomb,
        withheld: g.withheld & !s.dur.tomb,
        excluded: g.excluded | res,
    };
    let mut o = Out::new(s);
    if !same_generation(&s.dur, g, g2) {
        o.row(S10);
        match write(&mut o) {
            None => return Ok(None),
            Some(false) => return o.done(),
            Some(true) => o.n.dur.generation = g2,
        }
    } else {
        o.row(S10n);
    }
    // L3 from durable reasons; repair a reserved ledger mark lost to a crash
    // between L1's W and its ledger write.
    for (u, &room) in UNIT_ROOM.iter().enumerate() {
        if let Some(i) = reserving(&o.n.dur, u) {
            match o.n.dur.units[u] {
                UDur::Present => o.n.vol.umem[u] = UMem::Revoked(Reason::Release(i)),
                UDur::Quarantined(q) if q != QOwner::Release(i) => {
                    o.n.dur.units[u] = UDur::Quarantined(QOwner::Release(i));
                    o.row(O13);
                }
                _ => {}
            }
        } else if o.n.dur.tomb & bit(room) != 0 && o.n.dur.units[u] == UDur::Present {
            o.n.vol.umem[u] = UMem::Revoked(Reason::Obligation(room as u8));
        }
    }
    o.n.vol.detached = (0..INSTRS)
        .filter(|i| o.n.dur.instr[*i].reserved_units() != 0)
        .fold(0, |a, i| a | bit(i));
    o.n.vol.mem_gen = o.n.dur.generation;
    o.n.vol.phase = Phase::P5;
    o.row(S11);
    o.done()
}

// ------------------------------------------------------- M3 / M2 fetch

/// State-space reduction: a relist whose result would equal the applied
/// list is a no-op (same high-water, tombstones, floors, classes and
/// queues), so it is explored only when something could change.
fn fetch_useful(s: &State) -> bool {
    let Some(applied) = s.vol.applied else {
        return true;
    };
    let next = list_view(&s.auth.core);
    // A seq-only transaction does not change this holder's list projection.
    s.env.list_signing.is_some_and(|(kid, stmt)| {
        s.dur.pinned_keys.kids & kid == 0 || stmt.is_some_and(|k| k.seq > s.dur.pinned_keys.seq)
    }) || (s.env.probe == Probe::Lease && s.env.clock >= s.vol.lease_until)
        || s.auth.rollback_suspected
        || s.vol.relist
        || next.rev.epoch != applied.rev.epoch
        || next.items != applied.items
        || next.instr != applied.instr
        || next.pins != applied.pins
        || s.vol.admitted != Some(applied)
}

fn preflight(o: &mut Out, d: Design, envelope: Envelope) -> Result<(), Refusal> {
    let store = Rev {
        epoch: o.n.auth.core.epoch,
        seq: o.n.auth.core.seq,
    };
    let result = wire::preflight(d, envelope, store, o.n.auth.rollback_suspected);
    if result == Err(Refusal::Witness) {
        o.n.auth.rollback_suspected = true;
    }
    if o.n.env.envelope.is_some()
        && d.has(Fix::AuthorityFidelity)
        && wire::preflight(Design::REV10_1, envelope, store, false) == Err(Refusal::Witness)
        && !o.n.auth.rollback_suspected
    {
        o.obs(Obs::ValidationPrecedence);
    }
    result
}

fn list_refusal(r: Refusal) -> ListErr {
    match r {
        Refusal::Witness | Refusal::EpochMismatch => ListErr::EpochMismatch,
        Refusal::CustodyUnavailable | Refusal::Rate => ListErr::Transient,
        Refusal::InvalidProof => ListErr::Unauthorized { replay: false },
        Refusal::ProofExpired => ListErr::ProofExpired,
        Refusal::Replay => ListErr::Unauthorized { replay: true },
        Refusal::Scope => ListErr::Forbidden,
        Refusal::TooLarge | Refusal::Grammar => ListErr::BadRequest,
    }
}

fn fetch_start(s: &State, d: Design) -> Step {
    if !matches!(s.vol.phase, Phase::P5 | Phase::P7)
        || s.vol.fetching
        || s.vol.proof_backoff & bit(CHANS) != 0
        || s.vol.apply.is_some()
        || !fetch_useful(s)
    {
        return Ok(None);
    }
    if s.vol.route != Route::Normal {
        return Ok(None); // N3
    }
    if s.auth.restoring {
        return Ok(None); // the old authority is stopped; nothing is listed
    }
    let mut o = Out::new(s);
    let view = list_view(&s.auth.core);
    let echo = wire::list_echo(d, s.dur.last_list_ref);
    if s.dur
        .last_list_ref
        .is_some_and(|r| wire::reference(Design::REV10, view.rev, r, true) == RefVerdict::Rollback)
        && echo.is_none()
    {
        o.obs(Obs::ListWitnessOmitted);
    }
    let envelope = s.env.envelope.unwrap_or_else(|| Envelope {
        refs: [
            RefInput::Absent,
            echo.map_or(RefInput::Absent, RefInput::verified),
        ],
        ..Envelope::default()
    });
    o.n.net.list = Some(match preflight(&mut o, d, envelope) {
        Ok(()) => match envelope.refs[1].unverifiable() {
            Some(reason) if d.has(Fix::AdvisoryListWitness) => {
                o.obs(Obs::WitnessUnverifiable(reason));
                ListResp::List(view)
            }
            Some(_) => {
                o.obs(Obs::AdvisoryWitnessRejected);
                ListResp::Err {
                    code: ListErr::LegacyStaleListRef,
                    healthy: false,
                }
            }
            None => ListResp::List(view),
        },
        Err(r) => ListResp::Err {
            code: list_refusal(r),
            healthy: matches!(
                r,
                Refusal::Witness | Refusal::EpochMismatch | Refusal::ProofExpired | Refusal::Replay
            ),
        },
    });
    o.n.vol.fetching = true;
    o.row(F1);
    o.done()
}

fn list_deliver(s: &State, d: Design) -> Step {
    let (true, Some(resp)) = (s.vol.fetching, s.net.list) else {
        return Ok(None);
    };
    let mut o = Out::new(s);
    o.n.net.list = None;
    o.n.vol.fetching = false;
    match resp {
        ListResp::List(l) => {
            if let Some((kid, statement)) = s.env.list_signing {
                let Ok(pin) = s.dur.pinned_keys.authorize(d, kid, statement) else {
                    o.row(F10);
                    return o.done();
                };
                if s.dur
                    .pinned_keys
                    .authorize(Design::REV10, kid, statement)
                    .is_err()
                {
                    o.obs(Obs::UntrustedKey);
                }
                if pin != s.dur.pinned_keys {
                    o.row(F11);
                    match write(&mut o) {
                        None => return Ok(None),
                        Some(false) => return o.done(),
                        Some(true) => o.n.dur.pinned_keys = pin,
                    }
                }
            }
            o.n.vol.proof_retried &= !bit(CHANS);
            o.row(F3);
            match admission(&s.dur, &l) {
                Adm::Admit(h) => {
                    o.row(h);
                    o.n.vol.admitted = Some(l);
                    if s.env.probe == Probe::Lease {
                        o.n.vol.admitted_until = s.env.clock + 1;
                    }
                    o.n.vol.apply = Some(ApplyStep::A1);
                    o.n.vol.alert_epoch = None;
                    if d.has(Fix::NarrowFirst) {
                        o.n.vol.block |= narrowing(&l, s.dur.tomb);
                        o.row(A0);
                    }
                    let refused = pin_refusal(s, d, &l);
                    if d.has(Fix::RootPinFence) && refused != 0 {
                        o.n.vol.block |= refused;
                        o.row(P1);
                    }
                }
                Adm::Refuse(h, alert) => {
                    o.row(h);
                    if alert.is_some() {
                        o.n.vol.alert_epoch = alert;
                    }
                }
            }
        }
        ListResp::Err { code, healthy } => match code {
            ListErr::BadRequest => {
                o.row(F8);
                halt(&mut o, d, Route::Halted401, false)?;
            }
            ListErr::Transient | ListErr::LegacyStaleListRef => {
                o.n.vol.proof_retried &= !bit(CHANS);
                o.row(F7);
            }
            ListErr::ProofExpired => match proof_expiry(&mut o.n, d, CHANS) {
                Expiry::Halt => halt(&mut o, d, Route::Halted401, true)?,
                Expiry::Resign => o.row(F8p),
                Expiry::BackoffClockAlert => o.row(F8b),
            },
            ListErr::EpochMismatch => {
                o.row(F7);
                o.row(H10);
            }
            ListErr::Unauthorized { replay: true } if d.has(Fix::ReplayRetries) => o.row(F8r),
            ListErr::Unauthorized { .. } => {
                o.row(F8);
                halt(&mut o, d, Route::Halted401, healthy)?;
            }
            ListErr::Forbidden => {
                o.row(F8);
                halt(&mut o, d, Route::Halted403, healthy)?;
            }
        },
    }
    o.done()
}

// ------------------------------------------------------------- M4a apply

fn apply(s: &State, d: Design) -> Step {
    let Some(step) = s.vol.apply else {
        return Ok(None);
    };
    if s.vol.rel_txn.is_some() {
        return Ok(None); // the publisher actor runs one transaction at a time
    }
    let l = s
        .vol
        .admitted
        .expect("an apply always has its admitted list");
    match step {
        ApplyStep::A1 => apply_a1(s, d, &l),
        ApplyStep::A2 => {
            let mut o = Out::new(s);
            o.row(A2);
            match write(&mut o) {
                None => return Ok(None),
                Some(false) => {
                    o.n.vol.apply = None;
                    return o.done();
                }
                Some(true) => o.n.dur.rollback_closed = true,
            }
            o.n.vol.apply = Some(next_apply_step(&mut o, ApplyStep::A3, &l));
            o.done()
        }
        ApplyStep::A3 => {
            let classes = classify(&l, s.dur.tomb);
            let g2 = generation_for(&classes, reserved_units(&s.dur));
            let mut o = Out::new(s);
            o.row(A3);
            match write(&mut o) {
                None => return Ok(None),
                Some(false) => {
                    o.n.vol.apply = None;
                    return o.done();
                }
                Some(true) => o.n.dur.generation = g2,
            }
            o.n.vol.apply = Some(ApplyStep::A4);
            o.done()
        }
        ApplyStep::A4 => apply_a4(s, d, &l),
    }
}

fn apply_a1(s: &State, d: Design, l: &ListMsg) -> Step {
    let Adm::Admit(h) = admission(&s.dur, l) else {
        unreachable!("hw and acceptance change only inside an apply or offline");
    };
    let mut o = Out::new(s);
    o.row(A1);
    match write(&mut o) {
        None => return Ok(None),
        Some(false) => {
            o.n.vol.apply = None;
            return o.done();
        }
        Some(true) => {}
    }
    o.n.dur.tomb |= (0..ROOMS)
        .filter(|r| l.items[*r].obligated())
        .fold(0, |a, r| a | bit(r));
    o.n.dur.hw = Some(l.rev);
    // H.16: the list's pairs join the grow-only pin-history in this W.
    for r in 0..ROOMS {
        if let Some(p) = l.pins[r]
            && o.n.dur.pin_history[p.id as usize].is_none()
        {
            o.n.dur.pin_history[p.id as usize] = Some(r as u8);
        }
    }
    match h {
        H6 => {
            let acc =
                o.n.dur
                    .accept
                    .take()
                    .expect("H6 consumes an armed acceptance");
            let reused = o.n.ghost.consumed.contains(&acc);
            o.n.ghost.consumed.insert(acc, "consumed acceptances")?;
            o.obs(Obs::AcceptanceConsumed { reused });
            o.n.ghost.status_due = true;
            o.n.dur.floors = [None; OBLS];
            o.n.dur.status_acc = None;
            o.n.dur.epoch_status_pending = d.has(Fix::JournalLostRecovery);
            for c in 0..CHANS {
                invalidate_slot(&mut o.n, c)?;
            }
            o.row(R14);
        }
        H7 => o.n.dur.accept = None,
        _ => {}
    }
    for ob in 0..OBLS {
        let item = l.items[ob];
        if item.obligated() {
            let last = item.last_accepted();
            if let Some(a) = last
                && o.n.dur.floors[ob].is_none_or(|f| a > f.state)
            {
                // R13 (same epoch: the list's epoch is now the high-water's).
                o.n.dur.floors[ob] = Some(Floor {
                    epoch: l.rev.epoch,
                    state: a,
                    key: None,
                });
                o.obs(Obs::FloorRaised {
                    o: ob as u8,
                    ev: l.rev.epoch,
                    cur: l.rev.epoch,
                });
                o.row(R13);
                if let Some(p) = o.n.dur.slots[ob]
                    && p.key.obl().is_some_and(|k| a > k.state)
                {
                    clear_slot(&mut o.n, ob);
                }
            }
            // R15, first clause. The time clause (`iat` beyond every attempt's
            // `exp + 420 s`) is not modeled: such keys stay as evidence.
            let before = o.n.dur.uncertain[ob].len();
            o.n.dur.uncertain[ob].retain(|k| {
                let st = k.obl().map_or(RState::Complete, |k| k.state);
                last.is_none_or(|a| a < st)
            });
            if o.n.dur.uncertain[ob].len() != before {
                o.row(R15);
            }
        } else {
            if o.n.dur.floors[ob].is_some() || o.n.dur.slots[ob].is_some() {
                o.n.dur.floors[ob] = None;
                clear_slot(&mut o.n, ob);
                o.row(R12);
            }
        }
    }
    for i in 0..INSTRS {
        let listed = l.instr.is_some_and(|x| x.id as usize == i);
        match o.n.dur.instr[i] {
            IDur::Reserved(_) | IDur::Draining(_) | IDur::Deleted(_) if !listed => o.row(L11),
            _ => {}
        }
    }
    o.n.vol.apply = Some(next_apply_step(&mut o, ApplyStep::A2, l));
    o.done()
}

/// Skip apply steps that write nothing (A2n, A3n): they are stutter steps
/// with no crash point, so they run inside the preceding step.
fn next_apply_step(o: &mut Out, from: ApplyStep, l: &ListMsg) -> ApplyStep {
    let n = &o.n;
    if from == ApplyStep::A2 {
        if n.dur.tomb != 0 && !n.dur.rollback_closed {
            return ApplyStep::A2;
        }
        o.row(A2n);
    }
    let classes = classify(l, o.n.dur.tomb);
    let g2 = generation_for(&classes, reserved_units(&o.n.dur));
    if !same_generation(&o.n.dur, o.n.dur.generation, g2) {
        return ApplyStep::A3;
    }
    o.row(A3n);
    ApplyStep::A4
}

fn apply_a4(s: &State, d: Design, l: &ListMsg) -> Step {
    let mut o = Out::new(s);
    let old = s.vol.class;
    let new = classify(l, s.dur.tomb);
    if d.has(Fix::EchoListRef) && s.dur.last_list_ref != Some(l.rev) {
        match write(&mut o) {
            None => return Ok(None),
            Some(false) => {
                o.n.vol.apply = None;
                return o.done();
            }
            Some(true) => o.n.dur.last_list_ref = Some(l.rev),
        }
    }
    o.n.vol.class = new;
    if s.env.probe == Probe::Lease {
        o.n.vol.lease_until = if d.has(Fix::LeaseAndRollbackFence) {
            s.vol.admitted_until
        } else {
            s.env.clock + 1
        };
    }
    o.n.vol.block = 0;
    o.n.vol.pin_conflict = (0..ROOMS)
        .filter(|&r| history_conflict(&s.dur, d, r, l.pins[r]))
        .fold(0, |a, r| a | bit(r));
    if !d.has(Fix::SameRefStaleService) {
        // The reading H.28 rejects: H10.5 stale service covers every
        // previously validated artifact, so artifacts of an earlier
        // `pin_ref` keep serving within the bound while the root does not
        // match the new pin (even after the A0 refusal of the change).
        o.n.vol.stale_carry = (0..ROOMS)
            .filter(|&r| {
                l.pins[r].is_some_and(|p| {
                    s.dur.derived[r].is_some_and(|x| x != p.pin_ref) && s.env.root_dir[r] != p.id
                })
            })
            .fold(s.vol.stale_carry, |a, r| a | bit(r));
    }
    if !d.has(Fix::DurablePinDerivation) {
        // The reading H.24 rejects: "derived under `pin_ref`" judged against
        // the in-process baseline. An unchanged pin keeps its artifacts; with
        // the empty start-up baseline, the artifacts on disk are taken as
        // derived under the first list's pins.
        o.n.vol.adopted = (0..ROOMS)
            .filter(|&r| {
                l.pins[r].is_some()
                    && match s.vol.applied {
                        None => s.dur.derived[r].is_some(),
                        Some(a) => {
                            a.pins[r] == l.pins[r]
                                && (s.vol.adopted & bit(r) != 0
                                    || a.pins[r]
                                        .is_some_and(|p| s.dur.derived[r] == Some(p.pin_ref)))
                        }
                    }
            })
            .fold(0, |a, r| a | bit(r));
    }
    o.n.vol.mem_gen = s.dur.generation;
    for ob in 0..OBLS {
        if s.vol.applied.map(|p| obligation_view(p.items[ob])) != Some(obligation_view(l.items[ob]))
        {
            bump(&mut o.n, Bump::Obl(ob));
        }
    }
    for r in 0..ROOMS {
        if old[r] != new[r] {
            bump(&mut o.n, Bump::Res(r));
        }
        if let Some(row) = class_row(old[r], new[r]) {
            o.row(row);
        }
    }
    // A5: revoke the objects of obligated, retired and tombstoned resources.
    for (u, &r) in UNIT_ROOM.iter().enumerate() {
        if o.n.dur.units[u] != UDur::Present || reserving(&o.n.dur, u).is_some() {
            continue;
        }
        if new[r].obligated() || s.dur.tomb & bit(r) != 0 {
            match o.n.vol.umem[u] {
                UMem::Live => o.n.vol.umem[u] = UMem::Revoked(Reason::Obligation(r as u8)),
                UMem::Revoked(Reason::Gc) if d.has(Fix::GcClaimRevalidates) => {
                    o.n.vol.umem[u] = UMem::Revoked(Reason::Obligation(r as u8));
                }
                UMem::Revoked(_) => {}
            }
        }
    }
    // O14 (revision 8 stores owners): an owner that left the list.
    for u in 0..UNITS {
        if o.n.dur.units[u] == UDur::Quarantined(QOwner::Obligation)
            && !new[UNIT_ROOM[u]].obligated()
        {
            o.n.dur.units[u] = UDur::Quarantined(QOwner::Unattributed);
            o.row(O14);
        }
    }
    refresh_inv(&mut o.n);
    o.n.vol.applied = Some(*l);
    o.n.vol.apply = None;
    o.n.vol.relist = false;
    if o.n.vol.phase == Phase::P5 {
        o.n.vol.phase = Phase::P7;
        o.row(S12);
        o.row(S13);
    }
    o.row(A4);
    o.row(A5);
    o.row(A6);
    o.done()
}

// ------------------------------------------------------------- M7 release

fn rel_bind(s: &State, i: usize) -> Step {
    if s.vol.phase != Phase::P7 || !publisher_idle(s) || s.dur.instr[i] != IDur::Unseen {
        return Ok(None);
    }
    let Some(list) = s.vol.applied else {
        return Ok(None);
    };
    let Some(ins) = list.instr else {
        return Ok(None);
    };
    if ins.id as usize != i {
        return Ok(None);
    }
    let ok = ins.issued_seq <= list.rev.seq
        && wire::release_matches(
            (s.dur.manifests & bit(ins.manifest as usize) != 0).then_some(ins.manifest),
            ins.approved_count,
            s.vol.inv & !reserved_units(&s.dur),
        );
    let mut o = Out::new(s);
    o.row(if ok { L1 } else { L2 });
    match write(&mut o) {
        None => return Ok(None),
        Some(false) => return o.done(),
        Some(true) => {}
    }
    if ok {
        o.n.dur.instr[i] = IDur::Reserved(ins.manifest);
        o.n.vol.rel_txn = Some(i as u8);
    } else {
        o.n.dur.instr[i] = IDur::Mismatch(s.vol.inv);
        bump(&mut o.n, Bump::Release);
    }
    o.done()
}

fn rel_detach(s: &State, d: Design, i: usize) -> Step {
    let IDur::Reserved(units) = s.dur.instr[i] else {
        return Ok(None);
    };
    let in_txn = s.vol.rel_txn == Some(i as u8);
    let retry = d.has(Fix::ReleaseDetachRetries)
        && s.vol.detached & bit(i) == 0
        && publisher_idle(s)
        && s.vol.phase == Phase::P7;
    if !in_txn && !retry {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.row(if in_txn { L1 } else { L1d });
    match write(&mut o) {
        None => return Ok(None),
        Some(false) => {
            o.n.vol.rel_txn = None;
            return o.done();
        }
        Some(true) => {}
    }
    o.n.dur.generation.excluded |= units;
    o.n.vol.mem_gen.excluded |= units;
    for u in 0..UNITS {
        if units & bit(u) == 0 {
            continue;
        }
        match o.n.dur.units[u] {
            UDur::Quarantined(_) => {
                o.n.dur.units[u] = UDur::Quarantined(QOwner::Release(i as u8));
                o.row(O13);
            }
            UDur::Present => {
                o.n.vol.umem[u] = UMem::Revoked(Reason::Release(i as u8));
                o.row(O4);
            }
            UDur::Deleted => {}
        }
    }
    o.n.vol.detached |= bit(i);
    o.n.vol.rel_txn = None;
    bump(&mut o.n, Bump::Inv);
    o.done()
}

fn rel_drain(s: &State, i: usize) -> Step {
    let IDur::Reserved(units) = s.dur.instr[i] else {
        return Ok(None);
    };
    if s.vol.phase != Phase::P7 || s.vol.detached & bit(i) == 0 {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.row(L4);
    match write(&mut o) {
        None => return Ok(None),
        Some(false) => return o.done(),
        Some(true) => o.n.dur.instr[i] = IDur::Draining(units),
    }
    o.done()
}

fn rel_delete(s: &State, cfg: &Config, i: usize, u: usize, fail: bool) -> Step {
    let IDur::Draining(units) = s.dur.instr[i] else {
        return Ok(None);
    };
    if s.vol.phase != Phase::P7
        || units & bit(u) == 0
        || (cfg.design.has(Fix::ExactDeletionEvidence) && s.env.open_unlinked & bit(u) != 0)
    {
        return Ok(None);
    }
    let quarantined = match s.dur.units[u] {
        UDur::Present if s.vol.umem[u] == UMem::Revoked(Reason::Release(i as u8)) => false,
        UDur::Quarantined(QOwner::Release(x)) if x as usize == i => true,
        _ => return Ok(None),
    };
    if fail && !fault_ok(s, cfg, faults::DELETE_FAIL) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    if fail {
        // A failed unlink leaves everything as it was: a stutter. It spends no
        // fault budget, so it may repeat (a superset of one budgeted failure)
        // and its successor is its own source state (crate docs, reductions).
        o.row(L5f);
        return o.done();
    }
    o.row(L5);
    if quarantined {
        o.row(O12);
    }
    o.obs(Obs::Deleted {
        u: u as u8,
        claimant: Reason::Release(i as u8),
    });
    o.n.dur.units[u] = UDur::Deleted;
    o.n.vol.umem[u] = UMem::Live;
    bump(&mut o.n, Bump::Res(UNIT_ROOM[u]));
    o.done()
}

fn rel_deleted(s: &State, i: usize) -> Step {
    let IDur::Draining(units) = s.dur.instr[i] else {
        return Ok(None);
    };
    if (0..UNITS).any(|u| units & bit(u) != 0 && s.dur.units[u] != UDur::Deleted) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.row(L5);
    match write(&mut o) {
        None => return Ok(None),
        Some(false) => return o.done(),
        Some(true) => o.n.dur.instr[i] = IDur::Deleted(units),
    }
    o.done()
}

fn rel_result(s: &State, i: usize) -> Step {
    if !matches!(s.dur.instr[i], IDur::Deleted(_))
        || s.vol.resulted & bit(i) != 0
        || !publisher_idle(s)
        || s.vol.phase != Phase::P7
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.row(L6);
    o.n.vol.resulted |= bit(i);
    refresh_inv(&mut o.n);
    bump(&mut o.n, Bump::Release);
    o.done()
}

// ------------------------------------------------------- M5 GC and purge

fn gc_revoke(s: &State, u: usize) -> Step {
    if s.vol.phase != Phase::P7
        || s.dur.units[u] != UDur::Present
        || s.vol.umem[u] != UMem::Live
        || referenced(s, u)
        || room_class(s, u) != Class::Active
        || reserving(&s.dur, u).is_some()
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.vol.umem[u] = UMem::Revoked(Reason::Gc);
    o.row(O4);
    o.done()
}

fn gc_delete(s: &State, cfg: &Config, u: usize, fail: bool) -> Step {
    let d = cfg.design;
    if s.vol.phase != Phase::P7
        || s.dur.units[u] != UDur::Present
        || s.vol.umem[u] != UMem::Revoked(Reason::Gc)
        || (d.has(Fix::ExactDeletionEvidence) && s.env.open_unlinked & bit(u) != 0)
    {
        return Ok(None);
    }
    if d.has(Fix::GcClaimRevalidates) && room_class(s, u) != Class::Active {
        return Ok(None);
    }
    if fail && !fault_ok(s, cfg, faults::DELETE_FAIL) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.row(O5);
    if fail {
        o.n.budget.faults -= 1;
        o.row(O7);
        match write(&mut o) {
            None => return Ok(None),
            Some(false) => return o.done(),
            Some(true) => {}
        }
        // Revision 8: GC is not a quarantine owner, so the entry is unowned.
        o.n.dur.units[u] = UDur::Quarantined(quarantine_owner(d, QOwner::Unattributed));
    } else {
        o.row(O6);
        o.obs(Obs::Deleted {
            u: u as u8,
            claimant: Reason::Gc,
        });
        o.n.dur.units[u] = UDur::Deleted;
    }
    o.n.vol.umem[u] = UMem::Live;
    bump(&mut o.n, Bump::Res(UNIT_ROOM[u]));
    refresh_inv(&mut o.n);
    o.done()
}

fn quarantine(s: &State, d: Design, u: usize) -> Step {
    let c = room_class(s, u);
    if s.vol.phase != Phase::P7
        || s.dur.units[u] != UDur::Present
        || referenced(s, u)
        || !matches!(c, Class::Unknown | Class::TombUnlisted)
        || reserving(&s.dur, u).is_some()
    {
        return Ok(None);
    }
    match s.vol.umem[u] {
        UMem::Live => {}
        UMem::Revoked(Reason::Gc) if d.has(Fix::GcClaimRevalidates) => {}
        UMem::Revoked(Reason::Obligation(_)) if c == Class::TombUnlisted => {}
        UMem::Revoked(_) => return Ok(None),
    }
    let mut o = Out::new(s);
    o.row(if c == Class::TombUnlisted { C8 } else { O8 });
    match write(&mut o) {
        None => return Ok(None),
        Some(false) => return o.done(),
        Some(true) => {}
    }
    o.n.dur.units[u] = UDur::Quarantined(quarantine_owner(d, QOwner::Unattributed));
    o.n.vol.umem[u] = UMem::Live;
    bump(&mut o.n, Bump::Res(UNIT_ROOM[u]));
    refresh_inv(&mut o.n);
    o.done()
}

fn qretry(s: &State, cfg: &Config, u: usize, fail: bool) -> Step {
    let UDur::Quarantined(q) = s.dur.units[u] else {
        return Ok(None);
    };
    if s.vol.phase != Phase::P7
        || reserving(&s.dur, u).is_some()
        || (cfg.design.has(Fix::ExactDeletionEvidence) && s.env.open_unlinked & bit(u) != 0)
    {
        return Ok(None);
    }
    let c = room_class(s, u);
    let row = match q {
        QOwner::Obligation | QOwner::Derived if c.obligated() => O11,
        QOwner::Derived if c == Class::Active => O11g,
        _ => return Ok(None),
    };
    if fail && !fault_ok(s, cfg, faults::DELETE_FAIL) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.row(row);
    if fail {
        // A stutter, as in `rel_delete`: no budget, no state change.
        return o.done();
    }
    o.obs(Obs::Deleted {
        u: u as u8,
        claimant: if c.obligated() {
            Reason::Obligation(UNIT_ROOM[u] as u8)
        } else {
            Reason::Gc
        },
    });
    o.n.dur.units[u] = UDur::Deleted;
    bump(&mut o.n, Bump::Res(UNIT_ROOM[u]));
    refresh_inv(&mut o.n);
    o.done()
}

fn purge_delete(s: &State, cfg: &Config, u: usize, fail: bool) -> Step {
    if s.vol.phase != Phase::P7
        || s.dur.units[u] != UDur::Present
        || !room_class(s, u).obligated()
        || s.vol.umem[u] != UMem::Revoked(Reason::Obligation(UNIT_ROOM[u] as u8))
        || (cfg.design.has(Fix::ExactDeletionEvidence) && s.env.open_unlinked & bit(u) != 0)
    {
        return Ok(None);
    }
    if fail && !fault_ok(s, cfg, faults::DELETE_FAIL) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.row(O5);
    if fail {
        o.n.budget.faults -= 1;
        o.row(O7);
        match write(&mut o) {
            None => return Ok(None),
            Some(false) => return o.done(),
            Some(true) => {}
        }
        o.n.dur.units[u] = UDur::Quarantined(quarantine_owner(cfg.design, QOwner::Obligation));
    } else {
        o.row(O6);
        o.obs(Obs::Deleted {
            u: u as u8,
            claimant: Reason::Obligation(UNIT_ROOM[u] as u8),
        });
        o.n.dur.units[u] = UDur::Deleted;
    }
    o.n.vol.umem[u] = UMem::Live;
    bump(&mut o.n, Bump::Res(UNIT_ROOM[u]));
    refresh_inv(&mut o.n);
    o.done()
}

/// §5.6 steps 4–6: barrier and final scan, in this process.
fn purge_scan(s: &State, d: Design, ob: usize) -> Step {
    if s.vol.phase != Phase::P7 || !s.vol.class[ob].obligated() {
        return Ok(None);
    }
    // Steps 4–5 first: a pass scans once every revoked unit of `r` has had a
    // deletion attempt (failures are quarantined and retried later, O11).
    let unattempted = units_of(ob).any(|u| {
        s.dur.units[u] == UDur::Present
            && s.vol.umem[u] == UMem::Revoked(Reason::Obligation(UNIT_ROOM[u] as u8))
    });
    if unattempted {
        return Ok(None);
    }
    let rem = if d.has(Fix::ExactDeletionEvidence) {
        observed_remaining(s, ob)
    } else {
        remaining(s, ob)
    };
    let state = if !rem && s.vol.inv == 0 {
        RState::Complete
    } else {
        RState::Incomplete
    };
    let sc = Scan {
        state,
        remaining: rem,
        inv: s.vol.inv,
        clean: true,
    };
    if s.vol.scans[ob] == Some(sc) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.vol.scans[ob] = Some(sc);
    o.done()
}

// ------------------------------------------------- M6 / M8 report actors

fn enqueue(s: &State, d: Design, c: usize, same_state: bool) -> Step {
    if s.vol.phase != Phase::P7 || !publisher_idle(s) || s.vol.relist {
        return Ok(None);
    }
    let Some(hw) = s.dur.hw else {
        return Ok(None);
    };
    let p = s.dur.slots[c];
    let key = if c < OBLS {
        // H.19 "first opportunity": after an epoch acceptance the fresh
        // status goes first; new-epoch obligation reports wait for its
        // current-epoch receipt (H.22.1), unless it bug-halted (H.22.3).
        if !s.vol.class[c].obligated() || status_gate_shut(s, d) {
            return Ok(None);
        }
        let (state, rem, inv) = match s.vol.scans[c] {
            Some(sc) if sc.clean => (sc.state, sc.remaining, sc.inv),
            Some(_) => return Ok(None), // the pass repeats steps 6–7
            None => (RState::Revoked, remaining(s, c), s.vol.inv),
        };
        if (state == RState::Revoked && !rem) || (state == RState::Complete && inv != 0) {
            return Ok(None);
        }
        let k = OKey {
            o: c as u8,
            state,
            list_ref: hw,
            manifest: inv,
            remaining: rem,
        };
        let floor_ok = s.dur.floors[c].is_none_or(|f| {
            state > f.state
                || (same_state
                    && state == f.state
                    && state != RState::Complete
                    && f.key.is_none_or(|prev| {
                        prev.manifest != k.manifest || prev.remaining != k.remaining
                    }))
        });
        let pend_ok = p.is_none_or(|p| {
            let pst = p.key.obl().map_or(RState::Complete, |k| k.state);
            match p.st {
                DurSt::Invalidated | DurSt::Rejected => true,
                DurSt::Ready => state > pst,
                DurSt::Armed => {
                    matches!(
                        s.vol.slotv[c],
                        SlotVol::Queued { .. } | SlotVol::Sent { .. }
                    ) && state > pst
                }
                DurSt::Halted => false,
            }
        });
        if !floor_ok || !pend_ok {
            return Ok(None);
        }
        Key::O(k)
    } else {
        let rr = release_outcome(s);
        let last = s.dur.status_acc.map_or(0, |k| k.manifest);
        if s.vol.inv == last && rr.is_none() && !s.dur.reset_pending && !s.dur.epoch_status_pending
        {
            return Ok(None);
        }
        if p.is_some_and(|p| !matches!(p.st, DurSt::Invalidated | DurSt::Rejected)) {
            return Ok(None);
        }
        let k = SKey {
            list_ref: hw,
            reset: s.dur.reset_pending,
            manifest: s.vol.inv,
            result: rr,
        };
        if s.dur.status_acc == Some(k) {
            return Ok(None);
        }
        Key::S(k)
    };
    let mut o = Out::new(s);
    o.row(ch_row(c, R1, T1));
    match write(&mut o) {
        None => return Ok(None),
        Some(false) => return o.done(),
        Some(true) => {}
    }
    replace_slot(&mut o.n, c, key)?;
    o.done()
}

/// The transmission guard (§5.6): re-derive the claim from the current
/// snapshot.
fn transmission_guard(s: &State, d: Design, key: Key) -> bool {
    let Some(hw) = s.dur.hw else { return false };
    match key {
        Key::O(k) => {
            let ob = k.o as usize;
            let rem = if d.has(Fix::ExactDeletionEvidence) {
                observed_remaining(s, ob)
            } else {
                remaining(s, ob)
            };
            s.vol.class[ob].obligated()
                && k.list_ref.epoch == hw.epoch
                && k.manifest == s.vol.inv
                && k.remaining == rem
                && match k.state {
                    RState::Revoked => rem,
                    RState::Incomplete => rem || s.vol.inv != 0,
                    RState::Complete => {
                        !rem && s.vol.inv == 0
                            && s.vol.scans[ob].is_some_and(|sc| sc.state == RState::Complete)
                    }
                }
        }
        Key::S(k) => k.manifest == s.vol.inv && k.result == release_outcome(s),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum From {
    Ready,
    Retry,
    Restored,
}

fn transmit(s: &State, d: Design, c: usize) -> Step {
    if s.vol.phase != Phase::P7 || s.vol.route != Route::Normal || s.vol.proof_backoff & bit(c) != 0
    {
        return Ok(None); // N3 refuses R2 and T2 while halted
    }
    let Some(p) = s.dur.slots[c] else {
        return Ok(None);
    };
    let from = match (p.st, s.vol.slotv[c]) {
        (DurSt::Ready, SlotVol::Idle) => From::Ready,
        (DurSt::Armed, SlotVol::Sent { awaiting: false }) => From::Retry,
        (DurSt::Armed, SlotVol::Idle) if d.has(Fix::RestartArmedResumes) => From::Restored,
        _ => return Ok(None),
    };
    // H.22.1, H.27: a shut gate holds back transmission of an obligation
    // report never sent; reports already sent while it was open stand.
    if c < OBLS && from == From::Ready && !p.earlier && status_gate_shut(s, d) {
        return Ok(None);
    }
    let ok = transmission_guard(s, d, p.key);
    let tx = p.earlier || from != From::Ready;
    let mut o = Out::new(s);
    if from == From::Restored {
        o.row(ch_row(c, R4a, T2r));
    }
    o.row(if ok {
        ch_row(c, R2, T2)
    } else {
        ch_row(c, R3, T3)
    });
    match write(&mut o) {
        None => return Ok(None),
        Some(false) => return o.done(),
        Some(true) => {}
    }
    if ok {
        o.n.dur.slots[c] = Some(Pending {
            key: p.key,
            st: DurSt::Armed,
            earlier: tx,
        });
        o.n.vol.slotv[c] = SlotVol::Armed { clean: true };
        let manifest = match p.key {
            Key::O(k) => k.manifest,
            Key::S(k) => k.manifest,
        };
        o.n.dur.manifests |= bit(manifest as usize);
    } else {
        if tx {
            o.n.dur.uncertain[c].insert(p.key, "uncertain keys per channel")?;
        }
        o.n.dur.slots[c] = Some(Pending {
            key: p.key,
            st: DurSt::Invalidated,
            earlier: false,
        });
        o.n.vol.slotv[c] = SlotVol::Idle;
    }
    o.done()
}

fn handoff(s: &State, c: usize) -> Step {
    if !publisher_idle(s) || s.vol.route != Route::Normal {
        return Ok(None); // a publisher transaction; N3 refuses R2h and T2h
    }
    let (Some(p), SlotVol::Armed { clean }) = (s.dur.slots[c], s.vol.slotv[c]) else {
        return Ok(None);
    };
    if p.st != DurSt::Armed {
        return Ok(None);
    }
    let mut o = Out::new(s);
    if clean {
        o.row(ch_row(c, R2h, T2h));
        o.n.vol.slotv[c] = SlotVol::Queued {
            clean: true,
            aged: false,
        };
        return o.done();
    }
    o.row(ch_row(c, R2x, T2x));
    match write(&mut o) {
        None => return Ok(None),
        Some(false) => return o.done(),
        Some(true) => {}
    }
    if p.earlier {
        o.n.dur.uncertain[c].insert(p.key, "uncertain keys per channel")?;
    }
    o.n.dur.slots[c] = Some(Pending {
        key: p.key,
        st: DurSt::Invalidated,
        earlier: false,
    });
    o.n.vol.slotv[c] = SlotVol::Idle;
    o.done()
}

/// The transport puts queued bytes on the wire.
fn dispatch(s: &State, d: Design, c: usize) -> Step {
    let SlotVol::Queued { clean, aged } = s.vol.slotv[c] else {
        return Ok(None);
    };
    let p = s.dur.slots[c].expect("a queued attempt has its pending report");
    let mut o = Out::new(s);
    if d.has(Fix::CancelOnForbidden) && s.vol.route == Route::Halted403 {
        invalidate_slot(&mut o.n, c)?;
        o.row(N2c);
        return o.done();
    }
    if d.has(Fix::DispatchLinearized) {
        let observed_conflict = d.has(Fix::ExactDeletionEvidence)
            && p.key.obl().is_some_and(|k| {
                k.state == RState::Complete
                    && (observed_remaining(s, k.o as usize) || inventory(s) != 0)
            });
        if !clean || aged || observed_conflict {
            o.row(ch_row(c, R2q, T2q));
            o.n.vol.slotv[c] = SlotVol::Armed { clean: false };
            return o.done();
        }
        o.row(ch_row(c, R2d, T2d));
    }
    if let Key::O(k) = p.key
        && k.state == RState::Complete
    {
        o.obs(Obs::DispatchedComplete {
            o: k.o,
            true_claim: inventory(s) == 0 && !observed_remaining(s, k.o as usize),
            fresh: clean,
        });
    }
    if s.vol.route == Route::Halted403 {
        o.obs(Obs::SentWhileForbidden);
    }
    o.n.net.reqs[c].push(
        Req {
            key: p.key,
            replay: false,
            aged,
            commit_late: false,
            alive: true,
        },
        "requests in flight per channel",
    )?;
    o.n.vol.slotv[c] = SlotVol::Sent { awaiting: true };
    o.done()
}

fn deliver(s: &State, d: Design, c: usize, idx: usize) -> Step {
    let Some(resp) = s.net.resps[c].get(idx) else {
        return Ok(None);
    };
    let mut o = Out::new(s);
    o.n.net.resps[c].remove_at(idx);
    let p = s.dur.slots[c];
    let awaiting = matches!(s.vol.slotv[c], SlotVol::Sent { .. })
        && p.is_some_and(|p| p.st == DurSt::Armed && p.key == resp.key);
    let cur_epoch = s.dur.hw.map_or(0, |h| h.epoch);
    // A row that must commit: on K2x the response is consumed unrecorded and
    // the attempt is retried (the authority answers the retry the same way).
    macro_rules! commit {
        () => {
            match write(&mut o) {
                None => return Ok(None),
                Some(false) => {
                    if awaiting {
                        o.n.vol.slotv[c] = SlotVol::Sent { awaiting: false };
                    }
                    return o.done();
                }
                Some(true) => {}
            }
        };
    }
    if awaiting && resp.kind != RespKind::ProofExpired {
        o.n.vol.proof_retried &= !bit(c);
    }
    match resp.kind {
        RespKind::ProofExpired if awaiting => match proof_expiry(&mut o.n, d, c) {
            Expiry::Halt => halt(&mut o, d, Route::Halted401, true)?,
            retry => {
                o.row(if retry == Expiry::Resign {
                    ch_row(c, R11p, T6p)
                } else {
                    ch_row(c, R11b, T6e)
                });
                o.n.vol.slotv[c] = SlotVol::Sent { awaiting: false };
            }
        },
        RespKind::ProofExpired => {}
        RespKind::Accepted {
            epoch,
            accepted_holder_reset,
            ..
        } => {
            if awaiting && epoch != cur_epoch && d.has(Fix::AuthorityFidelity) {
                o.row(ch_row(c, R19, T7));
                o.n.vol.slotv[c] = SlotVol::Sent { awaiting: false };
                o.n.vol.relist = true;
            } else if awaiting {
                o.row(ch_row(c, R5, T4));
                commit!();
                match resp.key {
                    Key::O(k) => {
                        raise_floor(&mut o, c, k.state, Some(k), epoch);
                        clear_slot(&mut o.n, c);
                    }
                    Key::S(k) => {
                        o.n.dur.status_acc = Some(k);
                        if epoch == cur_epoch && k.list_ref.epoch == cur_epoch {
                            o.n.dur.epoch_status_pending = false;
                        }
                        if accepted_holder_reset == Some(true) && k.reset && epoch == cur_epoch {
                            o.n.dur.reset_pending = false;
                        }
                        clear_slot(&mut o.n, c);
                        settle_release(&mut o, k.result);
                    }
                }
                o.n.dur.uncertain[c].remove(&resp.key);
            } else if s.dur.uncertain[c].contains(&resp.key) {
                match resp.key {
                    Key::O(k) if k.list_ref.epoch == cur_epoch => {
                        o.row(R18);
                        commit!();
                        raise_floor(&mut o, c, k.state, Some(k), epoch);
                        let f = o.n.dur.floors[c].map(|f| f.state);
                        if let Some(p) = o.n.dur.slots[c]
                            && p.key
                                .obl()
                                .is_some_and(|pk| f.is_some_and(|f| f > pk.state))
                        {
                            clear_slot(&mut o.n, c);
                        }
                    }
                    Key::O(_) => {
                        o.row(R19);
                        commit!();
                    }
                    Key::S(k) => {
                        o.row(T7);
                        commit!();
                        if epoch == cur_epoch && k.list_ref.epoch == cur_epoch {
                            o.n.dur.epoch_status_pending = false;
                        }
                        if accepted_holder_reset == Some(true) && k.reset && epoch == cur_epoch {
                            o.n.dur.reset_pending = false;
                        }
                        settle_release(&mut o, k.result);
                    }
                }
                o.n.dur.uncertain[c].remove(&resp.key);
            }
        }
        RespKind::AlreadyComplete(ack) | RespKind::StateRegression(ack) => {
            let applies = c < OBLS && (awaiting || p.is_some_and(|p| p.st == DurSt::Invalidated));
            if applies {
                // Rev 9 assumed the entire 409 body was the compact ack.
                // Under H.5 the signed ack is in the problem extension.
                if d.has(Fix::AuthorityFidelity)
                    && !d.has(Fix::ConvergenceEnvelope)
                    && matches!(resp.kind, RespKind::StateRegression(_))
                {
                    o.n.vol.relist = true;
                    o.obs(Obs::WrongConvergenceEnvelope);
                    return o.done();
                }
                let envelope = wire::classify_ack(
                    d,
                    0,
                    1,
                    matches!(resp.kind, RespKind::StateRegression(_)),
                    matches!(resp.kind, RespKind::AlreadyComplete(_)),
                    ack.epoch == cur_epoch,
                    false,
                );
                if d.has(Fix::ConvergenceEpochBound) && envelope == AckClass::AuditAndRelist {
                    o.row(R6e);
                    commit!();
                    if awaiting && let Some(p) = p {
                        o.n.dur.slots[c] = Some(Pending {
                            st: DurSt::Rejected,
                            ..p
                        });
                        o.n.vol.slotv[c] = SlotVol::Idle;
                    }
                    o.n.vol.relist = true;
                } else {
                    o.row(R6);
                    commit!();
                    raise_floor(&mut o, c, ack.state, None, ack.epoch);
                    let f = o.n.dur.floors[c].map_or(RState::Revoked, |f| f.state);
                    if let Some(p) = o.n.dur.slots[c]
                        && p.key.obl().is_some_and(|k| k.state <= f)
                    {
                        clear_slot(&mut o.n, c);
                    }
                    o.n.dur.uncertain[c].retain(|k| k.obl().is_none_or(|k| k.state > f));
                }
            }
        }
        RespKind::CompleteRefused | RespKind::StaleListRef | RespKind::NotFound => {
            if awaiting && let Some(p) = p {
                o.row(match resp.kind {
                    RespKind::NotFound if c < OBLS => R8,
                    _ if c < OBLS => R7,
                    _ => T6d,
                });
                commit!();
                o.n.dur.slots[c] = Some(Pending {
                    st: DurSt::Rejected,
                    ..p
                });
                o.n.vol.slotv[c] = SlotVol::Idle;
                if resp.kind != RespKind::CompleteRefused {
                    o.n.vol.relist = true;
                }
            }
        }
        RespKind::Bug => {
            if awaiting && let Some(p) = p {
                o.row(ch_row(c, R9, T6c));
                commit!();
                o.n.dur.slots[c] = Some(Pending {
                    st: DurSt::Halted,
                    ..p
                });
                o.n.vol.slotv[c] = SlotVol::Idle;
                if c == STATUS && !d.has(Fix::DurableStatusGate) {
                    // The reading H.27 rejects: the release is an event of
                    // this process, not derived from the durable halt.
                    o.n.vol.gate_released = true;
                }
                o.obs(Obs::OperatorWait {
                    healthy: resp.healthy,
                });
            }
        }
        RespKind::Transient | RespKind::EpochMismatch | RespKind::InvariantViolation => {
            if awaiting {
                o.row(ch_row(c, R10, T5));
                if resp.kind == RespKind::EpochMismatch {
                    o.row(H10);
                }
                o.n.vol.slotv[c] = SlotVol::Sent { awaiting: false };
            }
        }
        RespKind::Unauthorized { replay } => {
            if awaiting {
                if replay && d.has(Fix::ReplayRetries) {
                    o.row(ch_row(c, R11r, T6r));
                    o.n.vol.slotv[c] = SlotVol::Sent { awaiting: false };
                } else {
                    o.row(ch_row(c, R11, T6b));
                    halt(&mut o, d, Route::Halted401, resp.healthy)?;
                }
            }
        }
        RespKind::Forbidden => {
            if awaiting {
                o.row(ch_row(c, R11, T6a));
                halt(&mut o, d, Route::Halted403, resp.healthy)?;
            }
        }
    }
    o.done()
}

fn replenish(s: &State) -> Step {
    if s.vol.phase == Phase::Down || s.env.disk_full || (s.dur.reserve && s.vol.rsv == Rsv::Full) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.dur.reserve = true;
    o.n.vol.rsv = Rsv::Full;
    o.row(K4);
    o.done()
}

// ------------------------------------------------- healthy environment

/// OpenClast may mint a search capability naming `room`: no rollback
/// suspicion or restore in progress (H.15.1), no H.25 epoch-change gate, and
/// the holding is `active`.
pub fn mintable(s: &State, room: usize) -> bool {
    !s.auth.rollback_suspected
        && !s.auth.restoring
        && s.auth.mint_gate.is_none()
        && s.auth.core.rooms[room].phase == APhase::Active
}

/// H.32.2: whether a roster entry makes its holder a gate member: every
/// engine holder in the roster, enabled, disabled or decommissioned, except
/// one refused on every route since long before `T_stop`. The reading H.32
/// rejects (H.30.1 as first written) counts only enabled holders.
fn gate_member(r: Option<Roster>, d: Design) -> bool {
    match r {
        None => false,
        Some(r) if d.has(Fix::RosterMintMembership) => !r.refused_aged,
        Some(r) => r.enabled,
    }
}

/// H.30.1: whether a peer still holds the gate shut. Frozen membership reads
/// its seat; the reading H.30.1 rejects reads the live roster, so a member
/// that stops counting reopens minting and a later enrollee starts counting.
fn peer_pending(p: &Peer, d: Design, timed_out: bool) -> bool {
    if d.has(Fix::FrozenMintMembership) {
        p.seat == Seat::Pending
    } else {
        !timed_out && gate_member(p.roster, d) && p.seat != Seat::Cleared
    }
}

/// H14: the new epoch's first transaction closes minting for the id-space
/// and freezes the members (H.25, H.30.1, H.32.2). After
/// `restore_journal_lost` minting also waits for the id-space-wide timeout
/// (H.30.4). L1 requires that minting resumes for a holder that held a
/// high-water when the epoch changed. A first transaction after the lease
/// timeout clears every member by lease in it (H.33.3).
fn start_gate(o: &mut Out, d: Design, idspace_wait: bool, had_hw: bool) {
    o.n.auth.mint_gate = Some(MintGate {
        holder: Seat::Pending,
        idspace_wait,
    });
    for p in o.n.peers.iter_mut() {
        p.seat = if gate_member(p.roster, d) {
            Seat::Pending
        } else {
            Seat::Absent
        };
    }
    o.n.ghost.search_due = had_hw;
    o.row(H14);
    if o.n.env.lease_timeout {
        clear_by_lease(o, d);
    }
}

/// H.25, H.30.3: minting for the id-space resumes once no member is pending
/// and, after `restore_journal_lost`, the id-space-wide timeout has passed.
fn settle_gate(o: &mut Out, d: Design) {
    let Some(g) = o.n.auth.mint_gate else {
        return;
    };
    let timed_out = o.n.env.lease_timeout && d.has(Fix::MintLeaseTimeout);
    if g.holder == Seat::Pending
        || g.idspace_wait
        || o.n.peers.iter().any(|p| peer_pending(p, d, timed_out))
    {
        return;
    }
    o.n.auth.mint_gate = None;
    o.n.ghost.search_due = false;
    o.row(H15);
}

pub fn witness(s: &mut State, d: Design, echo: Rev, tag_valid: bool) -> RefVerdict {
    let store = list_view(&s.auth.core).rev;
    let result = wire::reference(d, store, echo, tag_valid);
    if result == RefVerdict::Rollback {
        s.auth.rollback_suspected = true;
    }
    result
}

fn accepted(epoch: u8) -> RespKind {
    RespKind::Accepted {
        epoch,
        disposition: None,
        accepted_holder_reset: None,
    }
}

fn process_report(
    core: &mut AuthCore,
    d: Design,
    k: OKey,
    bad_tag: bool,
) -> Result<RespKind, Bound> {
    let (epoch, seq) = (core.epoch, core.seq);
    let room = &mut core.rooms[k.o as usize];
    let Some(obl) = room.obl.as_mut() else {
        return Ok(RespKind::NotFound);
    };
    if let Some((_, ep, _)) = obl.receipts.iter().find(|(rk, _, _)| *rk == k) {
        return Ok(accepted(ep));
    }
    if bad_tag
        || k.list_ref.epoch != epoch
        || k.list_ref.seq < obl.created_seq
        || k.list_ref.seq > seq
    {
        return Ok(RespKind::StaleListRef);
    }
    if let Some(a) = obl.accepted {
        if k.state < a.state {
            return Ok(RespKind::StateRegression(a));
        }
        if k.state == RState::Complete && a.state == RState::Complete {
            return Ok(RespKind::AlreadyComplete(a));
        }
    }
    if k.state == RState::Complete && (k.manifest != 0 || k.remaining) {
        return Ok(RespKind::CompleteRefused);
    }
    obl.accepted = Some(Ack {
        state: k.state,
        epoch,
    });
    obl.receipts.insert(
        (k, epoch, core.seq + 1),
        "authority receipts per obligation",
    )?;
    if k.manifest == 0 {
        obl.blocked = false;
        obl.blocked_by = None;
    }
    if k.state == RState::Complete {
        room.phase = APhase::Retired;
    }
    if k.state == RState::Complete || d.has(Fix::AuthorityFidelity) {
        core.seq += 1;
        core.holder_rev = core.seq;
    }
    Ok(accepted(epoch))
}

fn process_status(
    core: &mut AuthCore,
    d: Design,
    k: SKey,
    bad_tag: bool,
) -> Result<RespKind, Bound> {
    let fidelity = d.has(Fix::AuthorityFidelity);
    if let Some((_, epoch, disposition)) = core.status_receipts.iter().find(|(rk, _, _)| *rk == k) {
        return Ok(RespKind::Accepted {
            epoch,
            disposition,
            accepted_holder_reset: Some(k.reset),
        });
    }
    if bad_tag
        || k.list_ref.epoch != core.epoch
        || k.list_ref.seq > core.seq
        || (fidelity
            && core.status.is_some_and(|last| {
                last.list_ref.epoch == core.epoch && k.list_ref.seq < last.list_ref.seq
            }))
    {
        return Ok(RespKind::StaleListRef);
    }
    let mut changed = false;
    let disposition = if let Some(rr) = k.result {
        if core.instr.is_some_and(|x| x.id == rr.instr) {
            core.instr = None;
            changed = true;
            Some(ReleaseDisposition::Applied)
        } else if (rr.instr as usize) < INSTRS && core.final_instr & bit(rr.instr as usize) != 0 {
            Some(ReleaseDisposition::AlreadyFinal)
        } else {
            Some(ReleaseDisposition::UnknownInstruction)
        }
    } else {
        None
    };
    if disposition == Some(ReleaseDisposition::Applied)
        && let Some(rr) = k.result
    {
        core.final_instr |= bit(rr.instr as usize);
        if !rr.executed {
            core.superseded_mismatch |= bit(rr.instr as usize);
        }
    }
    if k.manifest == 0
        || (disposition == Some(ReleaseDisposition::Applied)
            && k.result.is_some_and(|r| r.executed))
    {
        core.release_declined = false;
        for obl in core.rooms.iter_mut().filter_map(|r| r.obl.as_mut()) {
            changed |= obl.blocked;
            obl.blocked = false;
            obl.blocked_by = None;
        }
    }
    core.status = Some(k);
    let disposition = if fidelity { disposition } else { None };
    core.status_receipts
        .insert((k, core.epoch, disposition), "authority status receipts")?;
    if changed || fidelity {
        core.seq += 1;
    }
    if changed {
        core.holder_rev = core.seq;
    }
    Ok(RespKind::Accepted {
        epoch: core.epoch,
        disposition,
        accepted_holder_reset: Some(k.reset),
    })
}

fn auth_process(s: &State, d: Design, c: usize, idx: usize) -> Step {
    let Some(req) = s.net.reqs[c].get(idx) else {
        return Ok(None);
    };
    if s.auth.restoring {
        return Ok(None); // the old authority is stopped (H19)
    }
    let mut o = Out::new(s);
    o.n.net.reqs[c].remove_at(idx);
    let clock = if req.commit_late {
        Some(ProofTimes {
            proof: Proof::sign(1, 0),
            received: 0,
            commit: 421,
        })
    } else if req.aged {
        Some(ProofTimes {
            proof: Proof::sign(1, 0),
            received: 421,
            commit: 421,
        })
    } else {
        None
    };
    let envelope = s.env.envelope.unwrap_or_else(|| Envelope {
        refs: [RefInput::verified(req.key.list_ref()), RefInput::Absent],
        replay: req.replay,
        bad_signature: req.aged && !d.has(Fix::AuthorityFidelity),
        clock,
        ..Envelope::default()
    });
    let mut kind = match preflight(&mut o, d, envelope) {
        Ok(()) => {
            let bad_tag = envelope.refs[0].invalid_tag();
            match req.key {
                Key::O(k) => process_report(&mut o.n.auth.core, d, k, bad_tag)?,
                Key::S(k) => process_status(&mut o.n.auth.core, d, k, bad_tag)?,
            }
        }
        Err(Refusal::Witness | Refusal::EpochMismatch) => RespKind::EpochMismatch,
        Err(Refusal::Replay) => RespKind::Unauthorized { replay: true },
        Err(Refusal::InvalidProof) => RespKind::Unauthorized { replay: false },
        Err(Refusal::ProofExpired) => RespKind::ProofExpired,
        Err(Refusal::Scope) => RespKind::Forbidden,
        Err(Refusal::TooLarge | Refusal::Grammar) => RespKind::Bug,
        Err(Refusal::CustodyUnavailable | Refusal::Rate) => RespKind::Transient,
    };
    let pre = &s.auth.core;
    if o.n.auth.core.seq > pre.seq
        && let Some(t) = envelope.clock
    {
        if !t.proof.cas(d, t.commit)
            || (req.commit_late && s.env.envelope.is_none() && !t.proof.timely_at_commit(t.commit))
        {
            o.n.auth.core = *pre;
            kind = RespKind::ProofExpired;
        } else if !(t.proof.receipt(Design::REV10_1, t.received)
            && t.proof.timely_at_commit(t.commit))
        {
            // H.13: a commit needs the full receipt check and `now ≤ D` at
            // the CAS, so nothing commits after `D ≤ iat + 420 s`.
            o.obs(Obs::ProofDeadlineMissed);
        }
    }
    if envelope.commit_static_failed && o.n.auth.core != *pre {
        if d.has(Fix::CommitInvariant) {
            o.n.auth.core = *pre;
            kind = RespKind::InvariantViolation;
            o.obs(Obs::CommitInvariantAudit);
        } else {
            o.obs(Obs::CommitInvariantMissed);
        }
    }
    if o.n.auth.core.seq > pre.seq && d.has(Fix::AtomicJournalReplay) {
        record_report_journal(&mut o.n.auth, pre, req.key, kind);
    }
    // H.25, H.30.2(a): an accepted report (status or obligation) whose
    // `list_ref` is a tag-valid list of the current epoch proves the holder
    // applied a new-epoch list and stopped serving the old one, so it clears
    // the modeled holder's seat (a member, being enrolled and enabled).
    if let Some(g) = o.n.auth.mint_gate.as_mut()
        && g.holder == Seat::Pending
        && matches!(kind, RespKind::Accepted { .. })
        && req.key.list_ref().epoch == o.n.auth.core.epoch
    {
        g.holder = Seat::Cleared;
        o.row(H15);
        settle_gate(&mut o, d);
    }
    if let (Key::S(k), RespKind::Accepted { epoch, .. }) = (req.key, kind)
        && epoch == o.n.auth.core.epoch
        && k.list_ref.epoch == epoch
    {
        o.n.ghost.status_due = false;
    }
    let post = &o.n.auth.core;
    let replay_key = match req.key {
        Key::O(k) => pre.rooms[k.o as usize]
            .obl
            .is_some_and(|o| o.receipts.iter().any(|(key, _, _)| key == k)),
        Key::S(k) => pre.status_receipts.iter().any(|(key, _, _)| key == k),
    };
    let must_detect = s.env.envelope.is_none()
        && !req.replay
        && !req.aged
        && wire::reference(Design::REV10, list_view(pre).rev, req.key.list_ref(), true)
            == RefVerdict::Rollback;
    if (must_detect && (kind != RespKind::EpochMismatch || !o.n.auth.rollback_suspected))
        || (matches!(kind, RespKind::Accepted { .. }) && !replay_key && post.seq != pre.seq + 1)
    {
        o.obs(Obs::AuthorityUnfaithful);
    }
    if req.alive {
        o.n.net.resps[c].push(
            Resp {
                key: req.key,
                kind,
                healthy: true,
                late: false,
            },
            "responses in flight per channel",
        )?;
    }
    o.done()
}

fn record_report_journal(auth: &mut Auth, prev: &AuthCore, key: Key, kind: RespKind) {
    let core = &auth.core;
    if matches!(
        kind,
        RespKind::Accepted {
            disposition: Some(ReleaseDisposition::Applied),
            ..
        }
    ) && let Key::S(k) = key
        && let Some(rr) = k.result
    {
        auth.journal_finalized[rr.instr as usize] = Some(FinalEntry {
            seq: core.seq,
            status_digest: k,
            executed: rr.executed,
        });
    }
    for ob in 0..ROOMS {
        if prev.rooms[ob].obl.is_some_and(|x| x.blocked)
            && core.rooms[ob].obl.is_some_and(|x| !x.blocked)
        {
            auth.journal_cleared[ob] = core.seq;
        }
    }
}

fn drop_dead(s: &State, c: usize, idx: usize) -> Step {
    match s.net.reqs[c].get(idx) {
        Some(r) if !r.alive => {
            let mut o = Out::new(s);
            o.n.net.reqs[c].remove_at(idx);
            o.done()
        }
        _ => Ok(None),
    }
}

/// A human approves a release of the last accepted status report's
/// unattributed set (HW §13 C, §14 D). Instruction ids are never reused.
fn human_release(s: &State) -> Step {
    let core = &s.auth.core;
    let Some(st) = core.status else {
        return Ok(None);
    };
    if core.instr.is_some() || st.manifest == 0 || core.next_instr as usize >= INSTRS {
        return Ok(None);
    }
    let mut o = Out::new(s);
    let c = &mut o.n.auth.core;
    c.instr = Some(Instr {
        id: c.next_instr,
        manifest: st.manifest,
        approved_count: st.manifest.count_ones() as u8,
        issued_seq: c.seq + 1,
    });
    let ins = c.instr.expect("just issued");
    c.next_instr += 1;
    c.seq += 1;
    c.holder_rev = c.seq;
    o.n.auth.journal_issued[ins.id as usize] = Some((c.seq, ins));
    o.done()
}

fn human_decline(s: &State, d: Design) -> Step {
    if !d.has(Fix::AuthorityFidelity) {
        return Ok(None);
    }
    let core = &s.auth.core;
    if core.instr.is_some() || core.release_declined || core.status.is_none_or(|k| k.manifest == 0)
    {
        return Ok(None);
    }
    let eligible = |r: usize| {
        core.rooms[r].obl.is_some_and(|obl| {
            obl.accepted.is_some_and(|a| a.state == RState::Incomplete)
                && obl
                    .receipts
                    .iter()
                    .max_by_key(|(_, _, seq)| *seq)
                    .is_some_and(|(k, _, _)| {
                        k.state == RState::Incomplete && !k.remaining && k.manifest != 0
                    })
        })
    };
    // Project away declines that block no modeled obligation. The model does
    // not explore their audit-only transaction or thread a no-op decision
    // through unrelated pending reports, keys and revisions.
    if !(0..ROOMS).any(eligible) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    let c = &mut o.n.auth.core;
    c.release_declined = true;
    c.seq += 1;
    if d.has(Fix::ReleaseDeclined) {
        c.holder_rev = c.seq;
        for r in (0..ROOMS).filter(|r| eligible(*r)) {
            let status_digest = c.status.expect("release request bound to status");
            let entry = DeclineEntry {
                seq: c.seq,
                status_digest,
                declined_count: status_digest.manifest.count_ones() as u8,
            };
            let obl = c.rooms[r].obl.as_mut().expect("eligible obligation");
            obl.blocked = true;
            obl.blocked_by = Some(entry);
            o.n.auth.journal_declined[r] = Some(entry);
        }
        o.row(C16);
    } else {
        o.obs(Obs::DeclineInvisible);
    }
    o.done()
}

fn reopen(s: &State, ob: usize) -> Step {
    if s.env.probe != Probe::Governance || !s.auth.core.rooms[ob].obl.is_some_and(|o| o.blocked) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    let core = &mut o.n.auth.core;
    let entry = core.rooms[ob].obl.as_mut().expect("guard checked");
    entry.blocked = false;
    entry.blocked_by = None;
    core.release_declined = false;
    core.seq += 1;
    core.holder_rev = core.seq;
    o.n.auth.journal_cleared[ob] = core.seq;
    o.done()
}

fn space_freed(s: &State) -> Step {
    if !s.env.disk_full {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.env.disk_full = false;
    o.done()
}

// ------------------------------------------------------------------ faults

fn resp_kind(code: FaultCode) -> RespKind {
    match code {
        FaultCode::Transient => RespKind::Transient,
        FaultCode::EpochMismatch => RespKind::EpochMismatch,
        FaultCode::BadProof => RespKind::Unauthorized { replay: false },
        FaultCode::ProofExpired => RespKind::ProofExpired,
        FaultCode::Forbidden => RespKind::Forbidden,
        FaultCode::Bug => RespKind::Bug,
    }
}

fn auth_fault(s: &State, cfg: &Config, c: usize, idx: usize, code: FaultCode) -> Step {
    let Some(req) = s.net.reqs[c].get(idx) else {
        return Ok(None);
    };
    if !fault_ok(s, cfg, faults::REPORT_ERRORS)
        || !req.alive
        || (code == FaultCode::ProofExpired && !cfg.design.has(Fix::AuthorityFidelity))
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.faults -= 1;
    o.n.net.reqs[c].remove_at(idx);
    o.n.net.resps[c].push(
        Resp {
            key: req.key,
            kind: resp_kind(code),
            healthy: code == FaultCode::ProofExpired,
            late: false,
        },
        "responses in flight per channel",
    )?;
    o.done()
}

fn net_fail(s: &State, cfg: &Config, c: usize) -> Step {
    if !fault_ok(s, cfg, faults::NET_FAIL) || s.vol.slotv[c] != (SlotVol::Sent { awaiting: true }) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.faults -= 1;
    o.n.vol.slotv[c] = SlotVol::Sent { awaiting: false };
    o.n.net.reqs[c].map_in_place(|r| r.alive = false);
    o.n.net.resps[c].retain(|r| r.late);
    o.done()
}

fn net_dup(s: &State, cfg: &Config, c: usize, idx: usize) -> Step {
    let Some(req) = s.net.reqs[c].get(idx) else {
        return Ok(None);
    };
    // One duplicate in flight per channel is enough to exercise `401 replay`.
    if !fault_ok(s, cfg, faults::NET_DUP)
        || !req.alive
        || req.replay
        || s.net.reqs[c].iter().any(|r| r.replay)
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.faults -= 1;
    o.n.net.reqs[c].push(
        Req {
            replay: true,
            ..req
        },
        "requests in flight per channel",
    )?;
    o.done()
}

/// A response detaches from its connection and survives a holder restart.
fn detach(s: &State, cfg: &Config, c: usize, idx: usize) -> Step {
    let Some(r) = s.net.resps[c].get(idx) else {
        return Ok(None);
    };
    if !fault_ok(s, cfg, faults::LATE_DELIVERY) || r.late {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.faults -= 1;
    o.n.net.resps[c].remove_at(idx);
    o.n.net.resps[c].push(Resp { late: true, ..r }, "responses in flight per channel")?;
    o.done()
}

fn age_out(s: &State, cfg: &Config, c: usize) -> Step {
    let SlotVol::Queued { clean, aged: false } = s.vol.slotv[c] else {
        return Ok(None);
    };
    if !fault_ok(s, cfg, faults::AGE_OUT) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.faults -= 1;
    o.n.vol.slotv[c] = SlotVol::Queued { clean, aged: true };
    o.done()
}

fn delay_commit(s: &State, cfg: &Config, c: usize) -> Step {
    let Some(req) = s.net.reqs[c].get(0) else {
        return Ok(None);
    };
    if s.env.probe != Probe::Proof
        || req.aged
        || req.commit_late
        || req.replay
        || !req.alive
        || !cfg.design.has(Fix::AuthorityFidelity)
        || !fault_ok(s, cfg, faults::AGE_OUT)
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.faults -= 1;
    o.n.net.reqs[c].remove_at(0);
    o.n.net.reqs[c].push(
        Req {
            commit_late: true,
            ..req
        },
        "requests in flight per channel",
    )?;
    o.done()
}

fn list_fault(s: &State, cfg: &Config, code: FaultCode) -> Step {
    if code == FaultCode::ProofExpired && !cfg.design.has(Fix::AuthorityFidelity) {
        return Ok(None);
    }
    let code = match code {
        FaultCode::Transient => ListErr::Transient,
        FaultCode::EpochMismatch => ListErr::EpochMismatch,
        FaultCode::BadProof => ListErr::Unauthorized { replay: false },
        FaultCode::ProofExpired => ListErr::ProofExpired,
        FaultCode::Forbidden => ListErr::Forbidden,
        FaultCode::Bug => return Ok(None),
    };
    if !fault_ok(s, cfg, faults::LIST_ERRORS) || !matches!(s.net.list, Some(ListResp::List(_))) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.faults -= 1;
    o.n.net.list = Some(ListResp::Err {
        code,
        healthy: code == ListErr::ProofExpired,
    });
    o.done()
}

/// A duplicated list request: the copy's `401 replay` (a correct answer)
/// reaches the holder first.
fn list_dup(s: &State, cfg: &Config) -> Step {
    if !fault_ok(s, cfg, faults::NET_DUP) || !matches!(s.net.list, Some(ListResp::List(_))) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.faults -= 1;
    o.n.net.list = Some(ListResp::Err {
        code: ListErr::Unauthorized { replay: true },
        healthy: true,
    });
    o.done()
}

fn list_lost(s: &State, cfg: &Config) -> Step {
    if !fault_ok(s, cfg, faults::NET_FAIL) || !s.vol.fetching {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.faults -= 1;
    o.n.net.list = None;
    o.n.vol.fetching = false;
    o.row(F6);
    o.done()
}

fn disk_full(s: &State, cfg: &Config) -> Step {
    if !fault_ok(s, cfg, faults::DISK_FULL) || s.env.disk_full {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.faults -= 1;
    o.n.env.disk_full = true;
    o.done()
}

fn crash(s: &State) -> Step {
    if s.budget.crashes == 0 || (s.vol.phase == Phase::Down && !s.env.recovering) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.crashes -= 1;
    stop(&mut o.n);
    o.done()
}

// ---------------------------------------------------------------- scenario

fn auth_retire(s: &State, r: usize) -> Step {
    let room = s.auth.core.rooms[r];
    if s.budget.retires == 0
        || s.budget.retire_rooms & bit(r) == 0
        || room.phase != APhase::Active
        || room.obl.is_some()
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.retires -= 1;
    let c = &mut o.n.auth.core;
    c.seq += 1;
    let rev = Rev {
        epoch: c.epoch,
        seq: c.seq,
    };
    o.n.ghost.retired_at[r] = Some(rev);
    o.n.auth.journal_retired |= bit(r);
    c.holder_rev = c.seq;
    c.rooms[r] = ARoom {
        phase: APhase::Terminating,
        obl: Some(AObl {
            created_seq: c.seq,
            blocked: false,
            blocked_by: None,
            accepted: None,
            receipts: Default::default(),
        }),
    };
    o.done()
}

fn auth_unlist(s: &State, r: usize) -> Step {
    let room = s.auth.core.rooms[r];
    if s.budget.changes == 0
        || s.budget.change_rooms & bit(r) == 0
        || room.phase != APhase::Active
        || room.obl.is_some()
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.changes -= 1;
    o.n.auth.core.rooms[r].phase = APhase::Unlisted;
    o.n.auth.core.seq += 1;
    o.n.auth.core.holder_rev = o.n.auth.core.seq;
    o.done()
}

fn auth_relist(s: &State, r: usize) -> Step {
    let room = s.auth.core.rooms[r];
    if s.budget.changes == 0
        || s.budget.change_rooms & bit(r) == 0
        || room.phase != APhase::Unlisted
        || room.obl.is_some()
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.changes -= 1;
    o.n.auth.core.rooms[r].phase = APhase::Active;
    o.n.auth.core.seq += 1;
    o.n.auth.core.holder_rev = o.n.auth.core.seq;
    o.done()
}

/// A retired item drops off the list after its retention period.
fn auth_drop(s: &State, r: usize) -> Step {
    if s.budget.drops == 0 || s.auth.core.rooms[r].phase != APhase::Retired {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.drops -= 1;
    o.n.auth.core.rooms[r].phase = APhase::Unlisted;
    o.n.auth.core.seq += 1;
    o.n.auth.core.holder_rev = o.n.auth.core.seq;
    o.done()
}

fn auth_backup(s: &State) -> Step {
    if s.budget.restores == 0 || s.auth.backup.is_some() || s.auth.core.epoch != 0 {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.auth.backup = Some(Box::new(s.auth.core));
    for p in o.n.peers.iter_mut() {
        p.backup = p.roster;
    }
    o.done()
}

/// A supported restore (HW §14 Q4, G.4): the backed-up state under a newly
/// minted epoch. Instruction ids are random in the real system and never
/// reused, so the id counter is not rolled back.
fn auth_restore(s: &State, d: Design, override_lost: bool) -> Step {
    let Some(b) = s.auth.backup.as_deref() else {
        return Ok(None);
    };
    if s.budget.restores == 0 || override_lost != s.auth.failed_restore {
        return Ok(None);
    }
    let gate_probe = s.env.probe == Probe::EpochMint;
    // H.33.2: a restore proceeds only after H19 confirmed the old authority
    // stopped. The reading H.33.2 rejects proceeds without it, and the old
    // instance keeps running.
    if gate_probe && !s.auth.restoring && d.has(Fix::StoppedOldAuthority) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    if gate_probe && !s.auth.restoring {
        o.n.env.old_running = true;
    }
    if gate_probe && s.ghost.mint_stop.is_none() {
        // H.15.1: minting stops when a restore begins.
        o.n.ghost.mint_stop = Some(s.env.exposure_elapsed);
    }
    if gate_probe && s.auth.lists_stopped_at.is_none() {
        // H.30.2: `T_stop` for `restore_journal_lost` and a raw restore is the
        // restored authority's first start (this step); H19 recorded it for
        // a supported restore. Suspicion refuses lists from now on.
        o.n.auth.lists_stopped_at = Some(s.env.exposure_elapsed);
    }
    if s.env.bad_journal && !override_lost {
        if d.has(Fix::AtomicJournalReplay) {
            o.n.auth.failed_restore = true;
            o.n.auth.rollback_suspected = true;
            o.row(H12j);
            return o.done();
        }
        o.obs(Obs::UnverifiedRestore);
    }
    o.n.budget.restores -= 1;
    let next_instr = s.auth.core.next_instr;
    o.n.auth.core = AuthCore {
        epoch: s.auth.core.epoch + 1,
        next_instr,
        ..*b
    };
    if override_lost {
        o.n.auth.journal_lost_audited = true;
        let superseded = o.n.auth.core.instr.take();
        if d.has(Fix::JournalLostRecovery) {
            // Keep restored evidence in custody; only the current projection
            // and its replay/floor indexes are cleared in the new epoch.
            o.n.auth.journal_lost_evidence = o.n.auth.backup.take();
            if let Some(ins) = superseded {
                o.n.auth.core.final_instr |= bit(ins.id as usize);
                o.obs(Obs::ReapprovalRequested {
                    cause: ReapprovalCause::RestoreJournalLost,
                    report: None,
                });
            }
        }
        // H.19 item 5 (H.16.3): pins minted before this decision belong to
        // the lost timeline; no artifact derived under them may be served.
        o.n.ghost.pin_floor = (o.n.auth.core.epoch, s.auth.next_pin_ref);
        o.n.auth.journal_pins = [None; PIN_ENTRIES];
        if d.has(Fix::JournalLostPinReset) {
            let c = &mut o.n.auth.core;
            let mut nulled = false;
            for r in 0..ROOMS {
                if c.rooms[r].phase == APhase::Active
                    && let Some(p) = c.pins[r]
                {
                    c.pins[r] = None;
                    nulled = true;
                    o.n.auth.journal_pins[r] = Some(PinEntry {
                        seq: c.seq + 1,
                        room: r as u8,
                        pin: None,
                        prev: Some(p.pin_ref),
                    });
                }
            }
            if nulled {
                c.seq += 1;
                c.holder_rev = c.seq;
                o.row(P8);
            }
        }
        o.n.auth.core.status = None;
        o.n.auth.core.release_declined = false;
        for obl in
            o.n.auth
                .core
                .rooms
                .iter_mut()
                .filter_map(|r| r.obl.as_mut())
        {
            obl.blocked = false;
            obl.blocked_by = None;
        }
        o.row(H13j);
    } else if d.has(Fix::RetirementReplay) {
        replay_journal(&mut o, b, d);
    }
    if d.has(Fix::AuthorityFidelity) {
        let c = &mut o.n.auth.core;
        c.status_receipts = Default::default();
        for o in c.rooms.iter_mut().filter_map(|r| r.obl.as_mut()) {
            o.receipts = Default::default();
        }
    }
    // H.31: the roster as of the backup, then (supported restore only) the
    // journaled roster entries after it, in journal order. A
    // `restore_journal_lost` decision has no journal to replay.
    let mut roster_replayed = false;
    for p in o.n.peers.iter_mut() {
        let journaled = p.journaled.filter(|_| !override_lost);
        roster_replayed |= journaled.is_some();
        p.roster = journaled.or(p.backup);
        p.backup = None;
        p.journaled = None;
    }
    if roster_replayed {
        o.row(H17);
    }
    if d.has(Fix::EpochMintGate) {
        start_gate(
            &mut o,
            d,
            override_lost && d.has(Fix::JournalLostMintWait),
            s.dur.hw.is_some(),
        );
    }
    o.n.auth.rollback_suspected = false;
    o.n.auth.failed_restore = false;
    o.n.auth.restoring = false;
    o.n.auth.backup = None;
    o.done()
}

/// H.14 replay: one transaction; later final/clear entries veto earlier
/// issue/decline entries. This bounded journal has unique retirements, at
/// most two instruction IDs, and only the latest decision/clear per room.
fn replay_journal(out: &mut Out, backup: &AuthCore, d: Design) {
    let atomic = d.has(Fix::AtomicJournalReplay);
    let restored_targets = d.has(Fix::RestoredReplayTargets);
    let a = &mut out.n.auth;
    let c = &mut a.core;
    let after_backup = |seq| seq > backup.seq;
    let mut replayed = false;
    let replay_seq = backup.seq + 1;
    let (mut releases, mut declines, mut clears) = ([0; INSTRS], [0; ROOMS], [0; ROOMS]);
    let mut reapprovals = [None; INSTRS];
    for r in 0..ROOMS {
        if a.journal_retired & bit(r) != 0 && c.rooms[r].obl.is_none() {
            replayed = true;
            c.rooms[r] = ARoom {
                phase: APhase::Terminating,
                obl: Some(AObl {
                    created_seq: replay_seq,
                    blocked: false,
                    blocked_by: None,
                    accepted: None,
                    receipts: Default::default(),
                }),
            };
        }
        let declined = a.journal_declined[r].filter(|e| after_backup(e.seq));
        let cleared = atomic && after_backup(a.journal_cleared[r]);
        let Some(obl) = c.rooms[r].obl.as_mut() else {
            declines[r] = declined.map_or(0, |e| e.seq);
            if cleared {
                clears[r] = a.journal_cleared[r];
            }
            continue;
        };
        let clear_target =
            restored_targets || declined.is_some_and(|e| e.seq < a.journal_cleared[r]);
        if cleared && clear_target {
            replayed = true;
            obl.blocked = false;
            obl.blocked_by = None;
        }
        if let Some(e) = declined
            && !obl.complete()
            && (!cleared || a.journal_cleared[r] < e.seq)
        {
            replayed = true;
            obl.blocked = true;
            obl.blocked_by = Some(e);
        }
    }
    c.release_declined = c.rooms.iter().any(|r| r.obl.is_some_and(|o| o.blocked));
    for i in 0..INSTRS {
        let issued = a.journal_issued[i].filter(|(seq, _)| after_backup(*seq));
        if atomic && let Some(finalized) = a.journal_finalized[i].filter(|e| after_backup(e.seq)) {
            let restored = c.instr.is_some_and(|ins| ins.id as usize == i);
            let replayed_issue = issued.is_some_and(|(seq, _)| seq < finalized.seq);
            // Without H.18: the rejected reading, where an entry applies only
            // to a target a replayed entry created, and a missing target is
            // silently ignored.
            let target = replayed_issue || (restored_targets && restored);
            if !target {
                if restored_targets && c.final_instr & bit(i) == 0 {
                    releases[i] = finalized.seq;
                }
                continue;
            }
            replayed = true;
            if restored {
                c.instr = None;
            }
            c.final_instr |= bit(i);
            if !finalized.executed {
                c.superseded_mismatch |= bit(i);
                reapprovals[i] = Some(finalized.status_digest);
            }
            continue;
        }
        // H.14 re-lists unless a later `release_finalized` names the id (the
        // `continue` above). Revision 10 asked the restored state instead,
        // which predates the entry and cannot answer.
        if let Some((_, ins)) = issued
            && (atomic
                || !backup
                    .status_receipts
                    .iter()
                    .any(|(k, _, _)| k.result.is_some_and(|r| r.instr == ins.id)))
        {
            replayed = true;
            c.instr = Some(Instr {
                issued_seq: replay_seq,
                ..ins
            });
        }
    }
    // H.16.3: attestations after the restore point, in journal order.
    let (mut pin_skipped, mut orphaned) = (false, 0u8);
    for e in a
        .journal_pins
        .iter()
        .flatten()
        .filter(|e| after_backup(e.seq))
    {
        let r = e.room as usize;
        let cur = c.pins[r].map(|p| p.pin_ref);
        if c.rooms[r].phase == APhase::Unlisted {
            // The holding did not survive: reserve the directory as "*".
            if let Some(p) = e.pin {
                orphaned |= bit(p.id as usize);
                if d.has(Fix::OrphanPinReserve) {
                    c.pin_history[p.id as usize] = Some(PinOwner::Orphan);
                }
            }
            pin_skipped = true;
        } else if cur == e.pin.map(|p| p.pin_ref) {
            // Already committed: a no-op.
        } else if cur != e.prev
            || e.pin.is_some_and(|p| {
                c.pin_history[p.id as usize].is_some_and(|o| o != PinOwner::Room(e.room))
            })
        {
            pin_skipped = true;
        } else {
            replayed = true;
            c.pins[r] = e.pin;
            if let Some(p) = e.pin {
                c.pin_history[p.id as usize] = Some(PinOwner::Room(e.room));
            }
        }
    }
    if replayed {
        c.seq = replay_seq;
        c.holder_rev = replay_seq;
    }
    // H.18 as stated: a replayed `release_finalized` leaves its instruction
    // outstanding nowhere, and a replayed `blocked_cleared` with no later
    // decline leaves its obligation blocked nowhere.
    let finalized_listed = (0..INSTRS).any(|i| {
        a.journal_finalized[i].is_some_and(|e| after_backup(e.seq))
            && c.instr.is_some_and(|ins| ins.id as usize == i)
    });
    let cleared_blocked = (0..ROOMS).any(|r| {
        let cleared = a.journal_cleared[r];
        after_backup(cleared)
            && a.journal_declined[r].is_none_or(|e| e.seq < cleared)
            && c.rooms[r].obl.is_some_and(|o| o.blocked)
    });
    if atomic && (finalized_listed || cleared_blocked) {
        out.obs(Obs::ReplayTargetMissed);
    }
    if orphaned != 0 {
        let (_, dirs) = out.n.ghost.orphan_dirs;
        out.n.ghost.orphan_dirs = (out.n.auth.core.epoch, dirs | orphaned);
    }
    if pin_skipped {
        out.obs(Obs::PinReplaySkipped);
    }
    if restored_targets
        && releases
            .into_iter()
            .chain(declines)
            .chain(clears)
            .any(|seq| seq != 0)
    {
        out.obs(Obs::ReplaySkipped {
            releases,
            declines,
            clears,
        });
    }
    for report in reapprovals.into_iter().flatten() {
        out.obs(Obs::ReapprovalRequested {
            cause: ReapprovalCause::SupersededMismatch,
            report: Some(report),
        });
    }
}

fn tick_exposure(s: &State, elapsed: u16) -> Step {
    if !matches!(s.env.probe, Probe::Exposure | Probe::EpochMint)
        || elapsed <= s.env.exposure_elapsed
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.env.exposure_elapsed = elapsed;
    o.done()
}

fn answer_capability(s: &State, d: Design, room: usize, expiry_with_skew: u8) -> Step {
    if s.env.probe != Probe::Exposure || s.ghost.retired_at[room].is_none() {
        return Ok(None);
    }
    // H.15 fixture shortcut: admission and release at the same instant,
    // with an already bounded and verified capability envelope.
    let a0 = s
        .vol
        .admitted
        .is_some_and(|l| l.items[room] != Item::Active);
    let answered = servable(s, room)
        && !clock_held(s)
        && wire::pre_retirement_answer(d, s.env.exposure_elapsed, expiry_with_skew, a0);
    let mut o = Out::new(s);
    o.row(if answered { C17 } else { C18 });
    if answered
        && !wire::pre_retirement_answer(
            Design::REV10_1,
            s.env.exposure_elapsed,
            expiry_with_skew,
            a0,
        )
    {
        o.obs(Obs::CapabilityOverexposed);
    }
    o.done()
}

// H.21 uses the holder's receive/release clock, not knowledge of when an
// authority retirement committed. The focused clock is monotone, in seconds.
fn start_capability(s: &State, d: Design, room: u8, iat: i16, exp: i16) -> Step {
    if !matches!(s.env.probe, Probe::Exposure | Probe::Pin | Probe::EpochMint) {
        return Ok(None);
    }
    let now = i32::from(s.env.exposure_elapsed);
    let (iat, exp) = (i32::from(iat), i32::from(exp));
    // The authority stamps `iat` on its own clock, so no capability is dated
    // after the authority instant it arrives at (`now + lag`).
    if iat - now > i32::from(s.env.lag)
        || (s.env.probe == Probe::EpochMint && !minted(s, room, iat, exp, now))
    {
        return Ok(None);
    }
    let deadline = exp + i32::from(s.env.capability_skew);
    let future = iat > now + 30;
    let long = exp - iat > 60;
    let fenced = d.has(Fix::EngineCapabilityFence);
    let mut o = Out::new(s);
    if future {
        clock_evidence(&mut o, d);
    } else {
        unconfirmed_clear(&mut o, d);
    }
    let refusal = if fenced && future {
        Some(CapabilityRefusal::FutureIat)
    } else if fenced && long {
        Some(CapabilityRefusal::LifetimeTooLong)
    } else if fenced && clock_held(&o.n) {
        Some(CapabilityRefusal::ClockBehind)
    } else if d.has(Fix::CapabilityExposure) && now > deadline {
        Some(CapabilityRefusal::Expired)
    } else if !capability_room_active(s, room) {
        Some(CapabilityRefusal::NotActive)
    } else {
        None
    };
    if let Some(reason) = refusal {
        refuse_capability(&mut o, reason);
    } else {
        if s.vol.capability.is_some() {
            return Ok(None); // the probe bounds admitted work, not clock evidence
        }
        o.n.vol.capability = Some(InFlightCapability { room, deadline });
        // H.34: holder state recording the hold also refuses: only a durable
        // step broadens serving.
        if future || long || o.n.ghost.lag_proven || now > deadline || o.n.dur.clock_behind {
            o.obs(Obs::CapabilityOverexposed);
        }
    }
    o.done()
}

/// H.25 probe: whether the authority could have minted this capability.
/// Minting was open before it stopped; after that, a capability is minted
/// (and presented) only at an instant where `mintable` holds. Mint limits
/// cap the lifetime at 60 s.
fn minted(s: &State, room: u8, iat: i32, exp: i32, now: i32) -> bool {
    exp - iat <= 60
        && match s.ghost.mint_stop {
            None => iat <= now,
            Some(t) => iat <= i32::from(t) || (iat == now && mintable(s, usize::from(room))),
        }
}

fn finish_capability(s: &State, d: Design) -> Step {
    let Some(req) = s.vol.capability else {
        return Ok(None);
    };
    let now = i32::from(s.env.exposure_elapsed);
    let expired = now > req.deadline;
    let fenced = d.has(Fix::EngineCapabilityFence);
    let mut o = Out::new(s);
    o.n.vol.capability = None;
    if fenced && expired {
        refuse_capability(&mut o, CapabilityRefusal::Expired);
    } else if fenced && clock_held(s) {
        refuse_capability(&mut o, CapabilityRefusal::ClockBehind);
    } else if !capability_room_active(s, req.room) {
        refuse_capability(&mut o, CapabilityRefusal::NotActive);
    } else {
        o.row(C17);
        if expired || s.ghost.lag_proven || s.dur.clock_behind {
            o.obs(Obs::CapabilityOverexposed);
        }
        // H.25: results released from an earlier-epoch list, beyond the
        // 90 s that capabilities minted before the stop can reach.
        if s.env.probe == Probe::EpochMint
            && s.vol
                .applied
                .is_some_and(|l| l.rev.epoch < s.auth.core.epoch)
            && s.ghost.mint_stop.is_some_and(|t| now > i32::from(t) + 90)
        {
            o.obs(Obs::EpochOverexposed);
        }
    }
    o.done()
}

fn capability_room_active(s: &State, room: u8) -> bool {
    let r = usize::from(room);
    s.vol.admitted.is_some_and(|l| l.items[r] == Item::Active) && servable(s, r)
}

fn refuse_capability(o: &mut Out, reason: CapabilityRefusal) {
    o.row(C18);
    o.obs(Obs::CapabilityRefused(reason));
}

/// A verified page or capability with `iat − now_recv > 30 s`: proof that the
/// engine clock lags by more than 30 s (transit only makes it look ahead).
/// H.21.5 raises `clock_behind_authority` (C19). H.26: the refusal takes
/// effect in memory at once; the durable write is a step of its own
/// ([`persist_clock_hold`]), so a crash can fall between them.
fn clock_evidence(o: &mut Out, d: Design) {
    o.n.ghost.lag_proven = true;
    if d.has(Fix::EngineCapabilityFence) {
        if d.has(Fix::ClockHoldMemoryFirst) {
            o.n.vol.clock_hold = true;
        }
        if !o.n.dur.clock_behind {
            o.n.vol.clock_unpersisted = true;
        }
        o.row(C19);
        o.obs(Obs::ClockBehindAuthority);
    }
}

/// Lower the hold, in memory and in holder state (C20's W).
fn clear_clock_hold(o: &mut Out) {
    o.n.vol.clock_hold = false;
    o.n.vol.clock_unpersisted = false;
    o.n.dur.clock_behind = false;
}

/// The reading H.23 rejects ("it resumes when the condition clears"): any
/// input that no longer shows the lag clears the hold. With H.23 only C20's
/// confirmed correction clears it, so this does nothing.
fn unconfirmed_clear(o: &mut Out, d: Design) {
    if !d.has(Fix::ConfirmedClockCorrection) && clock_held(&o.n) {
        clear_clock_hold(o);
        o.row(C20);
    }
}

/// C19's durable write (H.26). The reading that the flag is volatile writes
/// holder state without it, so a restart forgets it; the reading that the
/// refusal waits for the write starts refusing only now.
fn persist_clock_hold(s: &State, d: Design) -> Step {
    if !s.vol.clock_unpersisted || s.vol.phase == Phase::Down {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.row(C19);
    match write(&mut o) {
        None => return Ok(None),
        Some(false) => return o.done(),
        Some(true) => {}
    }
    o.n.vol.clock_unpersisted = false;
    o.n.vol.clock_hold = true;
    if d.has(Fix::DurableClockHold) {
        o.n.dur.clock_behind = true;
    }
    o.done()
}

/// H.21.5 and H.23 on one verified list page (any page, not only page 0).
fn observe_list_clock(s: &State, d: Design, iat: i16, rtt: Option<u8>) -> Step {
    if s.env.probe != Probe::Exposure || !(s.vol.fetching || s.vol.clock_probe) {
        return Ok(None);
    }
    let ahead = i32::from(iat) - i32::from(s.env.exposure_elapsed); // iat − now_recv
    let lag = i32::from(s.env.lag);
    // The authority signs between this process sending the request and
    // receiving the page: lag − rtt ≤ iat − now_recv ≤ lag. A page that is
    // not bound to this process's nonce may be arbitrarily old.
    if ahead > lag || rtt.is_some_and(|r| ahead < lag - i32::from(r)) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    if rtt.is_some() && s.vol.clock_probe {
        o.n.vol.clock_probe = false; // the probe's page: its result need not be applied
    }
    if ahead > 30 {
        clock_evidence(&mut o, d);
    } else if !d.has(Fix::ConfirmedClockCorrection) {
        unconfirmed_clear(&mut o, d);
    } else if let (true, Some(r)) = (clock_held(s), rtt) {
        // C20: lag ≤ (iat − now_recv) + transit, and transit ≤ rtt. H.29:
        // the inequality alone decides, whatever `rtt` is.
        let bound = ahead + i32::from(r);
        o.row(C20);
        if bound <= 30 && (d.has(Fix::InequalityDecidesClear) || r <= 30) {
            // H.34: write first, then drop the in-memory hold. The reading
            // H.34 rejects drops it first, so a failed write leaves the
            // engine serving while holder state still records the hold.
            if !d.has(Fix::ClockClearWriteFirst) {
                o.n.vol.clock_hold = false;
            }
            match write(&mut o) {
                None => return Ok(None),
                // K2x: the hold stays raised in memory and durably; the next
                // confirmed correction tries again.
                Some(false) => return o.done(),
                Some(true) => {}
            }
            clear_clock_hold(&mut o);
        } else {
            o.obs(Obs::ClockLagAlert {
                bound: bound as i16,
                rtt: r,
            });
        }
    }
    o.done()
}

/// C21: while the hold is raised, at most one `page_size: 1` list request per
/// 60 s window, counted under H.8 (the model does not schedule rates).
fn clock_probe(s: &State) -> Step {
    if s.env.probe != Probe::Exposure
        || !clock_held(s)
        || s.vol.clock_probe
        || s.env.probe_window_used
        || !matches!(s.vol.phase, Phase::P5 | Phase::P7)
        || s.vol.route != Route::Normal
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.vol.clock_probe = true;
    o.n.env.probe_window_used = true;
    o.row(C21);
    o.done()
}

/// 60 s pass. Elapsed time never clears the hold under H.23.
fn probe_window(s: &State, d: Design) -> Step {
    if s.env.probe != Probe::Exposure || !(s.env.probe_window_used || clock_held(s)) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.env.probe_window_used = false;
    unconfirmed_clear(&mut o, d);
    o.done()
}

/// A local NTP or clock-step event. It may or may not fix the lag; it is no
/// evidence either way, so under H.23 it never clears the hold.
fn clock_step(s: &State, d: Design, lag: i8) -> Step {
    if s.env.probe != Probe::Exposure || lag == s.env.lag {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.env.lag = lag;
    if lag <= 30 {
        o.n.ghost.lag_proven = false;
    }
    unconfirmed_clear(&mut o, d);
    o.done()
}

/// An operator command cannot clear the hold under H.23 (fail closed).
fn op_clear_clock(s: &State, d: Design) -> Step {
    if s.env.probe != Probe::Exposure || !clock_held(s) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    if d.has(Fix::ConfirmedClockCorrection) {
        o.obs(Obs::ClockClearRefused);
    } else {
        unconfirmed_clear(&mut o, d);
    }
    o.done()
}

// ------------------------------------------- H.30 / H.31 other holders

/// The old lineage issues lists: no epoch change or restore has begun, so
/// `T_stop` is still ahead.
fn lists_served(s: &State) -> bool {
    s.auth.lists_stopped_at.is_none()
        && !s.auth.restoring
        && !s.auth.rollback_suspected
        && !s.auth.failed_restore
}

/// The authority answers lifecycle routes (and admin commands): no restore
/// is in progress and no rollback is suspected.
fn authority_up(s: &State) -> bool {
    !s.auth.restoring && !s.auth.rollback_suspected && !s.auth.failed_restore
}

/// H.31: a roster change after the backup is a journal entry, appended
/// before its governance transaction (H.1.2), so the restore replays it.
/// The reading H.31 rejects journals nothing.
fn journal_roster(p: &mut Peer, has_backup: bool, d: Design) {
    if d.has(Fix::RosterJournal) && has_backup {
        p.journaled = p.roster;
    }
}

/// The authority recognizes the peer's requests: enrolled, enabled, and the
/// key it signs with is the enrolled one (otherwise `401` or `403`).
fn peer_authorized(p: &Peer) -> bool {
    p.roster.is_some_and(|r| r.enabled && r.key == p.key)
}

/// The authority may serve the peer a list: its key is enrolled and it is
/// not refused on every route. A disabled holder with open obligations is
/// still listed (HW §4.5), so disabled peers may list too.
fn peer_listable(p: &Peer) -> bool {
    p.roster.is_some_and(|r| r.key == p.key && !r.refused_aged)
}

/// `enroll-holder` (HW §2.1): a new engine holder of the id-space. It lists
/// at once if lists are served. Enrolled after the new epoch's first
/// transaction, it is not a mint-gate member (H.30.1).
fn peer_enroll(s: &State, d: Design, i: usize) -> Step {
    if s.env.probe != Probe::EpochMint || s.peers[i] != Peer::default() || !authority_up(s) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    let has_backup = s.auth.backup.is_some();
    let p = &mut o.n.peers[i];
    p.roster = Some(Roster {
        enabled: true,
        key: 0,
        refused_aged: false,
    });
    p.lease = Some(s.auth.core.epoch);
    journal_roster(p, has_backup, d);
    o.done()
}

/// An admin disables (or decommissions) or re-enables a peer. Disabling
/// changes nothing the peer serves: it stops only once it receives a `403`
/// ([`peer_forbidden`]), which a holder with open obligations never does
/// (H.32.1), or once its lease lapses. A re-enabled peer restarts and lists:
/// in the old lineage it serves again; during the gate it applies only
/// current-epoch lists, so what it serves does not change.
fn peer_set_enabled(s: &State, d: Design, i: usize, enabled: bool) -> Step {
    let p = s.peers[i];
    let Some(r) = p.roster else {
        return Ok(None);
    };
    if s.env.probe != Probe::EpochMint || r.enabled == enabled || !authority_up(s) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    let has_backup = s.auth.backup.is_some();
    let old_lineage = lists_served(s);
    let n = &mut o.n.peers[i];
    n.roster = Some(Roster {
        enabled,
        refused_aged: false,
        ..r
    });
    journal_roster(n, has_backup, d);
    if old_lineage && enabled {
        n.lease = Some(s.auth.core.epoch);
    }
    // The reading H.30.1 rejects can open the gate when a member stops
    // counting.
    settle_gate(&mut o, d);
    o.done()
}

/// An admin rotates a peer's key in the old lineage (HW §2.1): the holder
/// signs with the new key, which the roster records.
fn peer_rotate(s: &State, d: Design, i: usize) -> Step {
    let p = s.peers[i];
    if s.env.probe != Probe::EpochMint || !lists_served(s) || p.key != 0 || !peer_authorized(&p) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    let has_backup = s.auth.backup.is_some();
    let n = &mut o.n.peers[i];
    n.key = 1;
    n.roster = p.roster.map(|r| Roster { key: 1, ..r });
    journal_roster(n, has_backup, d);
    o.done()
}

/// The peer's operator accepts the new epoch and the peer applies a list of
/// it, so it stops serving its earlier-epoch list.
fn peer_move(s: &State, i: usize) -> Step {
    let p = s.peers[i];
    let cur = s.auth.core.epoch;
    if s.env.probe != Probe::EpochMint
        || cur == 0
        || s.auth.rollback_suspected
        || p.silent
        || p.lease == Some(cur)
        || !peer_authorized(&p)
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    let n = &mut o.n.peers[i];
    n.lease = Some(cur);
    n.late = false;
    n.cursor = None;
    o.done()
}

/// The peer's status report with a tag-valid current-epoch `list_ref`
/// (H.30.2(a)). Accepted only for a recognized enrollment. Only a report that
/// clears a seat is explored: a later enrollee's startup status is harmless.
fn peer_report(s: &State, d: Design, i: usize) -> Step {
    let p = s.peers[i];
    let Some(_) = s.auth.mint_gate else {
        return Ok(None);
    };
    let counts =
        p.seat == Seat::Pending || (!d.has(Fix::FrozenMintMembership) && p.seat == Seat::Absent);
    if p.silent || p.lease != Some(s.auth.core.epoch) || !peer_authorized(&p) || !counts {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.peers[i].seat = Seat::Cleared;
    o.row(H15);
    settle_gate(&mut o, d);
    o.done()
}

fn peer_silent(s: &State, i: usize) -> Step {
    let p = s.peers[i];
    if s.env.probe != Probe::EpochMint || p.silent || p.roster.is_none() {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.peers[i].silent = true;
    o.done()
}

/// A capability minted now is presented to a peer that still serves an
/// earlier-epoch list. Capabilities minted before the stop answer at most
/// until 90 s after it (H.15, H.21); only a later answer is exposure.
fn peer_answer(s: &State, i: usize) -> Step {
    let now = s.env.exposure_elapsed;
    let stale = s.peers[i].lease.is_some_and(|e| e < s.auth.core.epoch);
    if s.env.probe != Probe::EpochMint
        || !stale
        || !(0..ROOMS).any(|r| mintable(s, r))
        || s.ghost.mint_stop.is_none_or(|t| now <= t + 90)
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.obs(Obs::EpochOverexposed);
    o.done()
}

/// H.30.2(b): the authority clock passes `T_stop + serve_seconds_ceiling +
/// 150 s` (1050 s with the default ceiling). Every lease from the old
/// lineage has lapsed (`quarantine_after` and the monotonic deadline, the
/// 30 s response deadline and the 120 s skew), and every member still pending
/// clears with `lifecycle.mint_gate.cleared_by_lease` and the H.11 alert.
/// The id-space-wide timeout of H.30.4 passes with it.
fn lease_timeout(s: &State, d: Design) -> Step {
    let Some(t) = s.auth.lists_stopped_at else {
        return Ok(None);
    };
    if s.env.probe != Probe::EpochMint || s.env.lease_timeout {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.env.lease_timeout = true;
    o.n.env.exposure_elapsed = s.env.exposure_elapsed.max(t + 1051);
    // Every lease from a list issued before `T_stop` has lapsed. A lease a
    // rejected H.33 reading renewed after `T_stop` has not.
    for p in o.n.peers.iter_mut() {
        if p.lease.is_some_and(|e| e < s.auth.core.epoch || e == 0) && !p.late {
            p.lease = None;
        }
    }
    clear_by_lease(&mut o, d);
    o.done()
}

/// H.30.2(b) on the open gate once `now > T_stop + serve_seconds_ceiling +
/// 150 s` (also at a gate created after that instant, when a restore took
/// longer): every pending member clears by lease, and the H.30.4 wait ends.
fn clear_by_lease(o: &mut Out, d: Design) {
    let Some(g) = o.n.auth.mint_gate.as_mut() else {
        return;
    };
    g.idspace_wait = false;
    if d.has(Fix::MintLeaseTimeout) {
        let holder = g.holder == Seat::Pending;
        if holder {
            g.holder = Seat::Cleared;
        }
        let mut peers = 0;
        for p in o.n.peers.iter_mut() {
            if p.seat == Seat::Pending {
                p.seat = Seat::Cleared;
                peers += 1;
            }
        }
        if holder || peers != 0 {
            o.obs(Obs::MintGateClearedByLease { holder, peers });
        }
    }
    o.row(H16);
    settle_gate(o, d);
}

/// H.30.3: an admin asks to clear the gate early. Refused: an early override
/// would reopen the exposure H.25 closes. The reading H.30.3 rejects clears
/// every pending member and the id-space wait.
fn op_mint_override(s: &State, d: Design) -> Step {
    if s.env.probe != Probe::EpochMint || s.auth.mint_gate.is_none() {
        return Ok(None);
    }
    let mut o = Out::new(s);
    if d.has(Fix::NoMintOverride) {
        o.obs(Obs::MintOverrideRefused);
        return o.done();
    }
    if let Some(g) = o.n.auth.mint_gate.as_mut() {
        g.holder = Seat::Cleared;
        g.idspace_wait = false;
    }
    for p in o.n.peers.iter_mut() {
        if p.seat == Seat::Pending {
            p.seat = Seat::Cleared;
        }
    }
    o.n.auth.mint_gate = None;
    o.n.ghost.search_due = false;
    o.done()
}

/// H.32.3: a disabled peer's request is answered `403
/// holder_not_authorized` and the authority records it. HW §4.5 refuses only
/// a disabled holder with no open obligations, so this may never happen
/// (decommissioned holders keep open obligations). `received`: the peer gets
/// the `403` and halts, so it stops serving. A lost response changes nothing
/// the peer serves, so under H.32.3 it changes nothing at all; the reading
/// H.32.3 rejects clears the peer's seat on the recorded `403`.
fn peer_forbidden(s: &State, d: Design, i: usize, received: bool) -> Step {
    let p = s.peers[i];
    if s.env.probe != Probe::EpochMint
        || p.roster.is_none_or(|r| r.enabled)
        || p.lease.is_none()
        || !authority_up(s)
    {
        return Ok(None);
    }
    let clears = !d.has(Fix::NoForbiddenClear) && p.seat == Seat::Pending;
    if !received && !clears {
        return Ok(None);
    }
    let mut o = Out::new(s);
    let n = &mut o.n.peers[i];
    if received {
        n.lease = None;
        n.late = false;
        n.cursor = None;
    }
    if clears {
        n.seat = Seat::Cleared;
        settle_gate(&mut o, d);
    }
    o.done()
}

/// H.32.2: while lists are still served, a disabled peer has been refused on
/// every route since an instant `t_r` and `serve_seconds_ceiling + 150 s`
/// have passed since. It can't have received a list after `t_r`, so its lease
/// has lapsed. The authority's record (derived from the journaled
/// `holder_disabled` and the obligations) travels with the roster entry.
fn peer_refusal_aged(s: &State, d: Design, i: usize) -> Step {
    let p = s.peers[i];
    let Some(r) = p.roster else {
        return Ok(None);
    };
    if s.env.probe != Probe::EpochMint || r.enabled || r.refused_aged || !lists_served(s) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    let has_backup = s.auth.backup.is_some();
    let n = &mut o.n.peers[i];
    n.roster = Some(Roster {
        refused_aged: true,
        ..r
    });
    n.lease = None;
    n.cursor = None;
    journal_roster(n, has_backup, d);
    o.done()
}

/// H.33.1: in the old lineage, a peer serving the current epoch receives
/// page 0 of its next list. The cursor seals the list's epoch and the
/// holder's `holder_rev` (§3.7); the list applies only once every page has
/// arrived, so until then the peer keeps its current lease.
fn peer_list_start(s: &State, i: usize) -> Step {
    let p = s.peers[i];
    let cur = s.auth.core.epoch;
    if s.env.probe != Probe::EpochMint
        || !lists_served(s)
        || p.cursor.is_some()
        || p.lease != Some(cur)
        || !peer_listable(&p)
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.peers[i].cursor = Some((cur, s.auth.core.holder_rev));
    o.done()
}

/// H.33.1: the peer presents its cursor. The authority honours it only while
/// the holder's `holder_rev` is unchanged; otherwise `410 list_expired` and
/// the peer discards the list (its earlier lease keeps running). A page of
/// an earlier epoch served after `T_stop` renews that epoch's lease beyond
/// the lease timeout.
fn peer_list_page(s: &State, i: usize) -> Step {
    let p = s.peers[i];
    let Some((epoch, rev)) = p.cursor else {
        return Ok(None);
    };
    if s.env.probe != Probe::EpochMint || !authority_up(s) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    let n = &mut o.n.peers[i];
    n.cursor = None;
    if rev == s.auth.core.holder_rev && peer_listable(&p) {
        n.lease = Some(epoch);
        if epoch < s.auth.core.epoch {
            n.late = true;
            o.obs(Obs::EarlierEpochListAfterStop);
        }
    } else if rev != s.auth.core.holder_rev {
        // `410 list_expired`: H18 advanced `holder_rev`.
        o.row(H18);
    }
    o.done()
}

/// The reading H.33.2 rejects: a restore proceeded while the old authority
/// instance still runs, and it renews a peer's earlier-epoch list after
/// `T_stop`, beyond the lease timeout.
fn peer_old_list(s: &State, i: usize) -> Step {
    let p = s.peers[i];
    if s.env.probe != Probe::EpochMint
        || !s.env.old_running
        || p.late
        || p.lease.is_none_or(|e| e >= s.auth.core.epoch)
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.peers[i].late = true;
    o.obs(Obs::EarlierEpochListAfterStop);
    o.done()
}

/// H.33.1: an operator epoch. One governance transaction, holding the store
/// lock that list projection takes (H.13): the new epoch, every holder's
/// `holder_rev` advanced, `lists_stopped_at` = its commit instant, and the
/// H.25 gate. Lists are issued in the new epoch from now on, and holders
/// refuse them with `unexpected_epoch` until `accept-epoch`. The reading
/// H.33.1 rejects leaves `holder_rev` alone, because nothing projected into a
/// holder's list changed (§3.7 lists no epoch change).
fn op_epoch(s: &State, d: Design) -> Step {
    if s.env.probe != Probe::EpochMint
        || s.budget.restores == 0
        || s.auth.core.epoch != 0
        || s.auth.backup.is_some()
        || !lists_served(s)
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.restores -= 1;
    let now = s.env.exposure_elapsed;
    o.n.ghost.mint_stop = Some(now);
    o.n.auth.lists_stopped_at = Some(now);
    let c = &mut o.n.auth.core;
    c.epoch += 1;
    c.seq += 1;
    if d.has(Fix::OperatorEpochRevBump) {
        c.holder_rev = c.seq;
    }
    o.row(H18);
    if d.has(Fix::EpochMintGate) {
        start_gate(&mut o, d, false, s.dur.hw.is_some());
    }
    o.done()
}

/// H.33.2 (H19): the restore tooling confirms that no process holds the
/// governance store lock or serves the lifecycle routes on the source
/// deployment, and that the old instance's start-up is disabled. Minting
/// stops (H.15.1) and nothing is listed until the snapshot. For a supported
/// restore `T_stop` is now, the start of the restore procedure; for
/// `restore_journal_lost` it is the restored authority's first start.
fn restore_begin(s: &State) -> Step {
    if s.env.probe != Probe::EpochMint
        || s.auth.backup.is_none()
        || s.budget.restores == 0
        || s.auth.restoring
        || !lists_served(s)
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    let now = s.env.exposure_elapsed;
    o.n.auth.restoring = true;
    o.n.ghost.mint_stop = Some(now);
    if !s.env.bad_journal {
        o.n.auth.lists_stopped_at = Some(now);
    }
    // The old instance's cursors die with it.
    for p in o.n.peers.iter_mut() {
        p.cursor = None;
    }
    o.row(H19);
    o.done()
}

// ---------------------------------------------------------------- root pins

/// H.16.2 `openclast lifecycle attest-root` (admin, local). The checks, the
/// journal append and the governance transaction run under one lock hold, so
/// the model takes them as one step; a refusal changes nothing.
fn attest_root(s: &State, room: u8, dir: Option<u8>, stale: bool) -> Step {
    // A committed attestation is a list change: it spends that budget.
    if s.env.probe != Probe::Pin
        || s.budget.changes == 0
        || s.auth.rollback_suspected
        || s.auth.failed_restore
    {
        return Ok(None);
    }
    let r = room as usize;
    let c = &s.auth.core;
    let mut o = Out::new(s);
    let refusal = if c.rooms[r].phase == APhase::Unlisted {
        Some(PinRefusal::HoldingUnknown)
    } else if c.rooms[r].phase != APhase::Active {
        Some(PinRefusal::HoldingInactive)
    } else if stale {
        Some(PinRefusal::Conflict)
    } else if dir
        .is_some_and(|id| c.pin_history[id as usize].is_some_and(|o| o != PinOwner::Room(room)))
    {
        Some(PinRefusal::Reused)
    } else {
        None
    };
    if let Some(code) = refusal {
        o.obs(Obs::PinAttestRefused(code));
        return o.done();
    }
    let pin = dir.map(|id| Pin {
        id,
        pin_ref: s.auth.next_pin_ref,
    });
    if pin.is_some() {
        o.n.auth.next_pin_ref += 1;
    }
    o.n.budget.changes -= 1;
    let c = &mut o.n.auth.core;
    let prev = c.pins[r].map(|p| p.pin_ref);
    c.pins[r] = pin;
    if let Some(p) = pin {
        c.pin_history[p.id as usize] = Some(PinOwner::Room(room));
    }
    c.seq += 1;
    c.holder_rev = c.seq;
    let entry = PinEntry {
        seq: c.seq,
        room,
        pin,
        prev,
    };
    let Some(slot) = o.n.auth.journal_pins.iter_mut().find(|e| e.is_none()) else {
        return Err(Bound("root pin journal entries"));
    };
    *slot = Some(entry);
    o.row(P6);
    o.done()
}

/// The room's configured root now resolves to `dir` (a storage move, a
/// directory swap, or a configuration edit). An empty directory receives the
/// room's data; one that already holds a room's bytes keeps them.
fn repoint_root(s: &State, r: usize, dir: u8) -> Step {
    if s.env.probe != Probe::Pin || s.env.root_dir[r] == dir {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.env.root_dir[r] = dir;
    o.n.ghost.dir_room[dir as usize].get_or_insert(r as u8);
    o.done()
}

/// Walk and publish the room under the applied `pin_ref` (P4). The walk
/// fails closed on a root mismatch: nothing from another tree is indexed.
fn pin_rebuild(s: &State, d: Design, r: usize) -> Step {
    if s.env.probe != Probe::Pin || s.vol.phase != Phase::P7 || !publisher_idle(s) {
        return Ok(None);
    }
    let Some(p) = s.vol.applied.and_then(|l| l.pins[r]) else {
        return Ok(None); // a null pin: nothing is built or served
    };
    if s.vol.class[r] != Class::Active
        || s.dur.derived[r] == Some(p.pin_ref)
        || (s.vol.block | s.vol.pin_conflict) & bit(r) != 0
        || (d.has(Fix::RootPinServing) && s.env.root_dir[r] != p.id)
    {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.row(P4);
    match write(&mut o) {
        None => return Ok(None),
        Some(false) => return o.done(),
        Some(true) => {}
    }
    o.n.dur.derived[r] = Some(p.pin_ref);
    o.n.vol.adopted &= !bit(r);
    o.n.vol.stale_carry &= !bit(r);
    o.n.ghost.derived_ref[r] = Some(p.pin_ref);
    o.n.ghost.derived_from[r] = s.ghost.dir_room[s.env.root_dir[r] as usize];
    o.done()
}

/// The H10.5 stale bound passes while some room's root mismatches its pin.
fn stale_deadline(s: &State) -> Step {
    let mismatch = (0..ROOMS).any(|r| {
        s.vol
            .applied
            .and_then(|l| l.pins[r])
            .is_some_and(|p| s.env.root_dir[r] != p.id)
    });
    if s.env.probe != Probe::Pin || s.env.stale_expired || !mismatch {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.env.stale_expired = true;
    o.row(P5);
    o.done()
}

// ---------------------------------------------------------------- operator

fn op_accept_epoch(s: &State) -> Step {
    let (Some(e), Some(hw)) = (s.vol.alert_epoch, s.dur.hw) else {
        return Ok(None);
    };
    let acc = Acceptance { from: hw, to: e };
    if e == hw.epoch || s.dur.accept == Some(acc) {
        return Ok(None);
    }
    let mut o = Out::new(s);
    stop(&mut o.n);
    o.n.dur.accept = Some(acc);
    o.row(H5);
    o.done()
}

fn op_restart(s: &State) -> Step {
    if s.vol.route == Route::Normal || s.vol.phase == Phase::Down {
        return Ok(None);
    }
    let mut o = Out::new(s);
    stop(&mut o.n);
    o.row(N4);
    o.done()
}

fn op_clear_halt(s: &State, d: Design, c: usize) -> Step {
    let Some(p) = s.dur.slots[c] else {
        return Ok(None);
    };
    if !d.has(Fix::HaltOperatorResume) || p.st != DurSt::Halted {
        return Ok(None);
    }
    let mut o = Out::new(s);
    stop(&mut o.n);
    if p.earlier {
        o.n.dur.uncertain[c].insert(p.key, "uncertain keys per channel")?;
    }
    o.n.dur.slots[c] = Some(Pending {
        key: p.key,
        st: DurSt::Invalidated,
        earlier: false,
    });
    o.row(N5);
    o.done()
}

// Focused adversary probes share the main transition and invariant functions.
fn probe(s: &State, d: Design, action: Action) -> Step {
    use Action::*;
    let mut o = Out::new(s);
    match (s.env.probe, action) {
        (Probe::Root, SwapHolder) if s.vol.phase == Phase::Down && s.env.holder == 0 => {
            o.n.env.holder = 1;
            // A valid state belonging to the new holder, not a corrupt file.
            o.n.dur.state_holder = Some(1);
        }
        (Probe::Root, LoseHolderState)
            if s.vol.phase == Phase::Down && s.dur.state_holder.is_some() =>
        {
            o.n.dur.state_holder = None;
            // H.26: holder-state loss starts with the clock flag clear.
            o.n.dur.clock_behind = false;
            o.n.ghost.lag_proven = false;
        }
        (Probe::Root, BeginRecovery)
            if s.vol.phase == Phase::Down && s.dur.state_holder.is_none() && !s.env.recovering =>
        {
            // Backup authenticity/age are held fixed here; validate owner before restoring.
            if d.has(Fix::RootHolderBinding) && Some(s.env.holder) != s.dur.root_holder {
                return Ok(None);
            }
            o.n.env.recovering = true;
            o.row(S15);
        }
        (Probe::Root, CommitRecovery) if s.vol.phase == Phase::Down && s.env.recovering => {
            if d.has(Fix::RootHolderBinding) && Some(s.env.holder) != s.dur.root_holder {
                return Ok(None);
            }
            o.n.dur.state_holder = Some(s.env.holder);
            o.n.env.recovering = false;
            o.row(S15);
        }
        (Probe::Lease, TryLegacy) if s.vol.phase == Phase::Down && !s.env.legacy_started => {
            if d.has(Fix::LeaseAndRollbackFence) || s.dur.rollback_closed {
                return Ok(None);
            }
            o.n.env.legacy_started = true;
        }
        (Probe::Lease, LeaseDeadline) if s.env.clock < 2 && s.vol.admitted.is_some() => {
            o.n.env.clock += 1;
            o.row(C2);
        }
        // A handle to the old current object can be installed before A0. Its
        // directory entry may disappear; the independent census still sees it.
        (Probe::Evidence, KeepOpen) if servable(s, 0) && s.env.open_unlinked == 0 => {
            o.n.env.open_unlinked = bit(0);
        }
        (Probe::Evidence, CloseHandle) if s.env.open_unlinked != 0 => {
            o.n.env.open_unlinked = 0;
            bump(&mut o.n, Bump::Res(0));
        }
        // Model an output published by a builder after the final scan, but
        // before the first wire byte; the holder registry has not observed it.
        (Probe::Evidence, LatePublish)
            if !s.env.late_once
                && s.vol.scans[0].is_some_and(|sc| sc.state == RState::Complete) =>
        {
            o.n.env.late_published = bit(1);
            o.n.env.late_once = true;
            o.row(O2);
        }
        (Probe::Evidence, ReconcileLate)
            if s.env.late_published != 0 && s.vol.phase == Phase::P7 =>
        {
            o.n.env.late_published = 0;
            o.n.dur.units[1] = UDur::Present;
            o.n.vol.umem[1] = UMem::Revoked(Reason::Obligation(0));
            bump(&mut o.n, Bump::Res(0));
            o.row(O14x);
        }
        (Probe::Evidence, WrongRelease { i, u })
            if !d.has(Fix::ExactDeletionEvidence)
                && matches!(s.dur.instr[1 - i as usize], IDur::Draining(m) if m & bit(u as usize) != 0)
                && s.dur.units[u as usize] != UDur::Deleted =>
        {
            o.n.dur.units[u as usize] = UDur::Deleted;
            o.obs(Obs::Deleted {
                u,
                claimant: Reason::Release(i),
            });
        }
        _ => return Ok(None),
    }
    o.done()
}
