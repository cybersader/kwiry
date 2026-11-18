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
    FloorRaised { o: u8, ev: u8, cur: u8 },
    /// A unit was unlinked. `authorized`: an obligation listed for its
    /// resource, a release reservation, or GC of an unreferenced object of an
    /// active resource owned the deletion at that instant.
    Deleted { u: u8, authorized: bool },
    /// The holder entered a state only an operator can leave. `healthy`: the
    /// trigger was a correct authority or network response, not a fault.
    OperatorWait { healthy: bool },
    /// An armed epoch acceptance was consumed. `reused`: it was consumed
    /// before.
    AcceptanceConsumed { reused: bool },
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
    s.vol.phase == Phase::P7
        && s.vol.route != Route::Halted403
        && s.vol.class[r] == Class::Active
        && s.dur.tomb & bit(r) == 0
        && s.vol.block & bit(r) == 0
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
        let result = |executed: bool| RelResult {
            instr: i as u8,
            executed,
        };
        match s.dur.instr[i] {
            IDur::Deleted(_) if s.vol.resulted & bit(i) != 0 => Some(result(true)),
            IDur::Mismatch(_) => Some(result(false)),
            IDur::Done if listed(i) && !reported(result(true)) => Some(result(true)),
            IDur::MismatchReported if listed(i) && !reported(result(false)) => Some(result(false)),
            _ => None,
        }
    })
}

/// Ghost check for I2: who may delete `u` at this instant.
pub fn delete_authorized(s: &State, u: usize) -> bool {
    let c = room_class(s, u);
    let reserved = (0..INSTRS).any(
        |i| matches!(s.dur.instr[i], IDur::Reserved(m) | IDur::Draining(m) if m & bit(u) != 0),
    );
    c.obligated() || reserved || (c == Class::Active && !referenced(s, u))
}

pub fn classify(l: &ListMsg, tomb: u8) -> [Class; ROOMS] {
    std::array::from_fn(|r| {
        let t = tomb & bit(r) != 0;
        match l.items[r] {
            Item::Active if t => Class::Conflict,
            Item::Active => Class::Active,
            Item::Terminating(_) => Class::Obligated,
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
        Item::Terminating(l) => Some((false, l)),
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
        APhase::Terminating => Item::Terminating(r.obl.and_then(|o| o.accepted).map(|a| a.state)),
        APhase::Retired => Item::Retired,
    });
    ListMsg {
        rev: Rev {
            epoch: core.epoch,
            seq: core.seq,
        },
        items,
        instr: core.instr,
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
    n.vol = Volatile::down();
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
    match (o.n.dur.instr[i], r.executed) {
        (IDur::Deleted(_) | IDur::Done, true) => {
            o.n.dur.instr[i] = IDur::Done;
            o.row(L7);
        }
        (IDur::Mismatch(_) | IDur::MismatchReported, false) => {
            o.n.dur.instr[i] = IDur::MismatchReported;
            o.row(L8);
        }
        _ => {}
    }
}

fn halt(o: &mut Out, to: Route, healthy: bool) {
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
            o.obs(Obs::OperatorWait { healthy });
        }
        _ => {}
    }
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
        Boot => boot(s),
        Markers => markers(s),
        Narrow => narrow(s),
        FetchStart => fetch_start(s),
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
        PurgeScan(o) => purge_scan(s, o as usize),
        Enqueue(c) => enqueue(s, c as usize, cfg.same_state),
        Transmit(c) => transmit(s, d, c as usize),
        Handoff(c) => handoff(s, c as usize),
        Dispatch(c) => dispatch(s, d, c as usize),
        Deliver { c, idx } => deliver(s, d, c as usize, idx as usize),
        Replenish => replenish(s),
        AuthProcess { c, idx } => auth_process(s, c as usize, idx as usize),
        DropDead { c, idx } => drop_dead(s, c as usize, idx as usize),
        HumanRelease => human_release(s),
        SpaceFreed => space_freed(s),
        AuthFault { c, idx, code } => auth_fault(s, cfg, c as usize, idx as usize, code),
        NetFail(c) => net_fail(s, cfg, c as usize),
        NetDup { c, idx } => net_dup(s, cfg, c as usize, idx as usize),
        Detach { c, idx } => detach(s, cfg, c as usize, idx as usize),
        AgeOut(c) => age_out(s, cfg, c as usize),
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
        AuthRestore => auth_restore(s),
        OpAcceptEpoch => op_accept_epoch(s),
        OpRestart => op_restart(s),
        OpClearHalt(c) => op_clear_halt(s, d, c as usize),
    }
}

// ------------------------------------------------------------- M1 startup

fn boot(s: &State) -> Step {
    if s.vol.phase != Phase::Down {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.vol = Volatile::down();
    o.n.vol.phase = Phase::P3;
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
            o.n.vol.umem[u] = UMem::Revoked(Reason::Obligation);
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
    s.vol.relist || list_view(&s.auth.core) != applied || s.vol.admitted != Some(applied)
}

fn fetch_start(s: &State) -> Step {
    if !matches!(s.vol.phase, Phase::P5 | Phase::P7)
        || s.vol.fetching
        || s.vol.apply.is_some()
        || !fetch_useful(s)
    {
        return Ok(None);
    }
    if s.vol.route != Route::Normal {
        return Ok(None); // N3
    }
    let mut o = Out::new(s);
    o.n.net.list = Some(ListResp::List(list_view(&s.auth.core)));
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
            o.row(F3);
            match admission(&s.dur, &l) {
                Adm::Admit(h) => {
                    o.row(h);
                    o.n.vol.admitted = Some(l);
                    o.n.vol.apply = Some(ApplyStep::A1);
                    o.n.vol.alert_epoch = None;
                    if d.has(Fix::NarrowFirst) {
                        o.n.vol.block |= narrowing(&l, s.dur.tomb);
                        o.row(A0);
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
            ListErr::Transient => o.row(F7),
            ListErr::EpochMismatch => {
                o.row(F7);
                o.row(H10);
            }
            ListErr::Unauthorized { replay: true } if d.has(Fix::ReplayRetries) => o.row(F8r),
            ListErr::Unauthorized { .. } => {
                o.row(F8);
                halt(&mut o, Route::Halted401, healthy);
            }
            ListErr::Forbidden => {
                o.row(F8);
                halt(&mut o, Route::Halted403, healthy);
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
        ApplyStep::A1 => apply_a1(s, &l),
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

fn apply_a1(s: &State, l: &ListMsg) -> Step {
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
            o.n.dur.floors = [None; OBLS];
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
            IDur::Done | IDur::MismatchReported | IDur::Mismatch(_) if !listed => {
                o.n.dur.instr[i] = IDur::Unseen;
                o.n.vol.resulted &= !bit(i);
                o.n.vol.detached &= !bit(i);
            }
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
    o.n.vol.class = new;
    o.n.vol.block = 0;
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
                UMem::Live => o.n.vol.umem[u] = UMem::Revoked(Reason::Obligation),
                UMem::Revoked(Reason::Gc) if d.has(Fix::GcClaimRevalidates) => {
                    o.n.vol.umem[u] = UMem::Revoked(Reason::Obligation);
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
    let Some(ins) = s.vol.applied.and_then(|l| l.instr) else {
        return Ok(None);
    };
    if ins.id as usize != i {
        return Ok(None);
    }
    let ok = ins.manifest != 0
        && ins.manifest & !s.vol.inv == 0
        && ins.manifest & reserved_units(&s.dur) == 0;
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
    if s.vol.phase != Phase::P7 || units & bit(u) == 0 {
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
        o.n.budget.faults -= 1;
        o.row(L5f);
        return o.done();
    }
    o.row(L5);
    if quarantined {
        o.row(O12);
    }
    o.obs(Obs::Deleted {
        u: u as u8,
        authorized: delete_authorized(s, u),
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
            authorized: delete_authorized(s, u),
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
        UMem::Revoked(Reason::Obligation) if c == Class::TombUnlisted => {}
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
    if s.vol.phase != Phase::P7 || reserving(&s.dur, u).is_some() {
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
        o.n.budget.faults -= 1;
        return o.done();
    }
    o.obs(Obs::Deleted {
        u: u as u8,
        authorized: delete_authorized(s, u),
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
        || s.vol.umem[u] != UMem::Revoked(Reason::Obligation)
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
            authorized: delete_authorized(s, u),
        });
        o.n.dur.units[u] = UDur::Deleted;
    }
    o.n.vol.umem[u] = UMem::Live;
    bump(&mut o.n, Bump::Res(UNIT_ROOM[u]));
    refresh_inv(&mut o.n);
    o.done()
}

/// §5.6 steps 4–6: barrier and final scan, in this process.
fn purge_scan(s: &State, ob: usize) -> Step {
    if s.vol.phase != Phase::P7 || !s.vol.class[ob].obligated() {
        return Ok(None);
    }
    // Steps 4–5 first: a pass scans once every revoked unit of `r` has had a
    // deletion attempt (failures are quarantined and retried later, O11).
    let unattempted = units_of(ob).any(|u| {
        s.dur.units[u] == UDur::Present && s.vol.umem[u] == UMem::Revoked(Reason::Obligation)
    });
    if unattempted {
        return Ok(None);
    }
    let rem = remaining(s, ob);
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

fn enqueue(s: &State, c: usize, same_state: bool) -> Step {
    if s.vol.phase != Phase::P7 || !publisher_idle(s) || s.vol.relist {
        return Ok(None);
    }
    let Some(hw) = s.dur.hw else {
        return Ok(None);
    };
    let p = s.dur.slots[c];
    let key = if c < OBLS {
        if !s.vol.class[c].obligated() {
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
                || (same_state && state == f.state && state != RState::Complete && f.key != Some(k))
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
        if s.vol.inv == last && rr.is_none() {
            return Ok(None);
        }
        if p.is_some_and(|p| !matches!(p.st, DurSt::Invalidated | DurSt::Rejected)) {
            return Ok(None);
        }
        let k = SKey {
            list_ref: hw,
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
fn transmission_guard(s: &State, key: Key) -> bool {
    let Some(hw) = s.dur.hw else { return false };
    match key {
        Key::O(k) => {
            let ob = k.o as usize;
            let rem = remaining(s, ob);
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
    if s.vol.phase != Phase::P7 || s.vol.route != Route::Normal {
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
    let ok = transmission_guard(s, p.key);
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
    if d.has(Fix::DispatchLinearized) {
        if !clean || aged {
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
            true_claim: s.vol.inv == 0 && !remaining(s, k.o as usize),
            fresh: clean,
        });
    }
    o.n.net.reqs[c].push(
        Req {
            key: p.key,
            replay: false,
            aged,
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
    match resp.kind {
        RespKind::Accepted { epoch } => {
            if awaiting {
                o.row(ch_row(c, R5, T4));
                commit!();
                match resp.key {
                    Key::O(k) => {
                        raise_floor(&mut o, c, k.state, Some(k), epoch);
                        clear_slot(&mut o.n, c);
                    }
                    Key::S(k) => {
                        o.n.dur.status_acc = Some(k);
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
                        settle_release(&mut o, k.result);
                    }
                }
                o.n.dur.uncertain[c].remove(&resp.key);
            }
        }
        RespKind::AlreadyComplete(ack) | RespKind::StateRegression(ack) => {
            let applies = c < OBLS && (awaiting || p.is_some_and(|p| p.st == DurSt::Invalidated));
            if applies {
                if d.has(Fix::ConvergenceEpochBound) && ack.epoch != cur_epoch {
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
                o.obs(Obs::OperatorWait {
                    healthy: resp.healthy,
                });
            }
        }
        RespKind::Transient | RespKind::EpochMismatch => {
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
                    halt(&mut o, Route::Halted401, resp.healthy);
                }
            }
        }
        RespKind::Forbidden => {
            if awaiting {
                o.row(ch_row(c, R11, T6a));
                halt(&mut o, Route::Halted403, resp.healthy);
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

fn process_report(core: &mut AuthCore, k: OKey) -> Result<RespKind, Bound> {
    let (epoch, seq) = (core.epoch, core.seq);
    let room = &mut core.rooms[k.o as usize];
    let Some(obl) = room.obl.as_mut() else {
        return Ok(RespKind::NotFound);
    };
    if let Some((_, ep)) = obl.receipts.iter().find(|(rk, _)| *rk == k) {
        return Ok(RespKind::Accepted { epoch: ep });
    }
    if k.list_ref.epoch != epoch || k.list_ref.seq < obl.created_seq || k.list_ref.seq > seq {
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
    obl.receipts
        .insert((k, epoch), "authority receipts per obligation")?;
    if k.state == RState::Complete {
        room.phase = APhase::Retired;
        core.seq += 1;
    }
    Ok(RespKind::Accepted { epoch })
}

fn process_status(core: &mut AuthCore, k: SKey) -> Result<RespKind, Bound> {
    if let Some((_, ep)) = core.status_receipts.iter().find(|(rk, _)| *rk == k) {
        return Ok(RespKind::Accepted { epoch: ep });
    }
    if k.list_ref.epoch != core.epoch || k.list_ref.seq > core.seq {
        return Ok(RespKind::StaleListRef);
    }
    core.status = Some(k);
    core.status_receipts
        .insert((k, core.epoch), "authority status receipts")?;
    if let Some(rr) = k.result
        && core.instr.is_some_and(|x| x.id == rr.instr)
    {
        core.instr = None;
        core.seq += 1;
    }
    Ok(RespKind::Accepted { epoch: core.epoch })
}

fn auth_process(s: &State, c: usize, idx: usize) -> Step {
    let Some(req) = s.net.reqs[c].get(idx) else {
        return Ok(None);
    };
    let mut o = Out::new(s);
    o.n.net.reqs[c].remove_at(idx);
    let kind = if req.replay {
        RespKind::Unauthorized { replay: true }
    } else if req.aged {
        RespKind::Unauthorized { replay: false }
    } else {
        match req.key {
            Key::O(k) => process_report(&mut o.n.auth.core, k)?,
            Key::S(k) => process_status(&mut o.n.auth.core, k)?,
        }
    };
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
    });
    c.next_instr += 1;
    c.seq += 1;
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
        FaultCode::Forbidden => RespKind::Forbidden,
        FaultCode::Bug => RespKind::Bug,
    }
}

fn auth_fault(s: &State, cfg: &Config, c: usize, idx: usize, code: FaultCode) -> Step {
    let Some(req) = s.net.reqs[c].get(idx) else {
        return Ok(None);
    };
    if !fault_ok(s, cfg, faults::REPORT_ERRORS) || !req.alive {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.faults -= 1;
    o.n.net.reqs[c].remove_at(idx);
    o.n.net.resps[c].push(
        Resp {
            key: req.key,
            kind: resp_kind(code),
            healthy: false,
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

fn list_fault(s: &State, cfg: &Config, code: FaultCode) -> Step {
    let code = match code {
        FaultCode::Transient => ListErr::Transient,
        FaultCode::EpochMismatch => ListErr::EpochMismatch,
        FaultCode::BadProof => ListErr::Unauthorized { replay: false },
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
        healthy: false,
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
    if s.budget.crashes == 0 || s.vol.phase == Phase::Down {
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
    c.rooms[r] = ARoom {
        phase: APhase::Terminating,
        obl: Some(AObl {
            created_seq: c.seq,
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
    o.done()
}

fn auth_backup(s: &State) -> Step {
    if s.budget.restores == 0 || s.auth.backup.is_some() || s.auth.core.epoch != 0 {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.auth.backup = Some(Box::new(s.auth.core));
    o.done()
}

/// A supported restore (HW §14 Q4, G.4): the backed-up state under a newly
/// minted epoch. Instruction ids are random in the real system and never
/// reused, so the id counter is not rolled back.
fn auth_restore(s: &State) -> Step {
    let Some(b) = s.auth.backup.as_deref() else {
        return Ok(None);
    };
    if s.budget.restores == 0 {
        return Ok(None);
    }
    let mut o = Out::new(s);
    o.n.budget.restores -= 1;
    let next_instr = s.auth.core.next_instr;
    o.n.auth.core = AuthCore {
        epoch: s.auth.core.epoch + 1,
        next_instr,
        ..*b
    };
    o.n.auth.backup = None;
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
