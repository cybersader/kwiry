//! The properties the checker proves in every reachable state and on every
//! transition.
//!
//! | Id | Property |
//! |---|---|
//! | I1 | A resource is servable only if the newest list admitted by an H row shows it `active` and it is not tombstoned (checked whenever no apply transaction is mid-flight; an apply linearizes at A4) |
//! | I2 | A unit is deleted only under a listed obligation for its resource, a strict release reservation, or GC of an unreferenced object of an active resource |
//! | I3 | `complete` is never dispatched while the unattributed inventory is non-empty, while anything of the resource remains, or while its proof is stale |
//! | I4 | Tombstones only grow |
//! | I5 | Floors are monotone within an epoch, belong to the high-water's epoch, and evidence minted in another epoch never raises them |
//! | I6 | Each armed epoch acceptance is consumed at most once |
//! | I7 | Some healthy actor (holder, environment or operator) can move whenever any work is outstanding |
//! | I8 | An operator wait (auth halt, bug halt) is entered only on a fault, never on a correct authority or network response |
//! | I9 | The unattributed inventory never counts data attributable to a listed holding (except units a release reserved) |
//! | L1 | Liveness: from every reachable state, once crashes, faults and scenario changes stop, every obligation reaches `complete` under strongly fair scheduling of the holder, the healthy authority and network, the human release decision and the operator |

use crate::state::*;
use crate::step::{Obs, Outcome, reserving, room_class, servable};

/// Which property a counterexample violates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Prop {
    I1Serving,
    I2Deletion,
    I3CompleteDispatch,
    I3StaleProof,
    I4Tombstones,
    I5Floors,
    I6Acceptance,
    I7Deadlock,
    I8HealthyHalt,
    I9Attribution,
    L1Liveness,
    /// A model bound was hit; the result would not be exhaustive.
    Bound,
}

impl Prop {
    pub const fn bit(self) -> u32 {
        1 << self as u32
    }

    pub fn describe(self) -> &'static str {
        match self {
            Prop::I1Serving => {
                "I1: a resource the newest admitted list does not show active is servable"
            }
            Prop::I2Deletion => {
                "I2: a unit was deleted without an owning obligation, release or GC reason"
            }
            Prop::I3CompleteDispatch => {
                "I3: `complete` dispatched while unattributed > 0 or data of the resource remains"
            }
            Prop::I3StaleProof => {
                "I3: `complete` dispatched while its proof is stale (versions moved since R2)"
            }
            Prop::I4Tombstones => "I4: a tombstone disappeared",
            Prop::I5Floors => {
                "I5: a floor decreased within an epoch or was raised by evidence from another epoch"
            }
            Prop::I6Acceptance => "I6: an epoch acceptance was consumed twice",
            Prop::I7Deadlock => "I7: work is outstanding but no healthy actor can move",
            Prop::I8HealthyHalt => {
                "I8: an operator wait was entered on a correct (non-fault) response"
            }
            Prop::I9Attribution => {
                "I9: the unattributed inventory counts data attributable to a listed holding"
            }
            Prop::L1Liveness => {
                "L1: an obligation never completes under fair healthy scheduling (fair cycle or dead end)"
            }
            Prop::Bound => "a model bound was exceeded",
        }
    }
}

/// Invariants of a single state (the first violated one not in `skip`).
pub fn check_state(s: &State, skip: u32) -> Option<Prop> {
    state_violations(s)
        .into_iter()
        .find(|p| skip & p.bit() == 0)
}

/// Invariants of a transition (the first violated one not in `skip`).
pub fn check_step(prev: &State, out: &Outcome, skip: u32) -> Option<Prop> {
    step_violations(prev, out)
        .into_iter()
        .find(|p| skip & p.bit() == 0)
}

fn state_violations(s: &State) -> Vec<Prop> {
    let mut v = Vec::new();
    let quiescent = s.vol.apply.is_none();
    let i1 = (0..ROOMS).any(|r| {
        servable(s, r)
            && (s.vol.admitted.is_none_or(|l| l.items[r] != Item::Active)
                || s.dur.tomb & bit(r) != 0)
    });
    if quiescent && i1 {
        v.push(Prop::I1Serving);
    }
    let i9 = (0..UNITS).any(|u| {
        s.vol.inv & bit(u) != 0
            && reserving(&s.dur, u).is_none()
            && matches!(
                room_class(s, u),
                Class::Active | Class::Obligated | Class::RetiredComplete
            )
    });
    if quiescent && s.vol.phase == Phase::P7 && i9 {
        v.push(Prop::I9Attribution);
    }
    if let Some(hw) = s.dur.hw
        && s.dur.floors.iter().flatten().any(|f| f.epoch != hw.epoch)
    {
        v.push(Prop::I5Floors);
    }
    v
}

fn step_violations(prev: &State, out: &Outcome) -> Vec<Prop> {
    let mut v = Vec::new();
    let next = &out.next;
    for o in out.obs.iter().flatten() {
        let p = match *o {
            Obs::DispatchedComplete {
                true_claim: false, ..
            } => Some(Prop::I3CompleteDispatch),
            Obs::DispatchedComplete { fresh: false, .. } => Some(Prop::I3StaleProof),
            Obs::FloorRaised { ev, cur, .. } if ev != cur => Some(Prop::I5Floors),
            Obs::Deleted {
                authorized: false, ..
            } => Some(Prop::I2Deletion),
            Obs::OperatorWait { healthy: true } => Some(Prop::I8HealthyHalt),
            Obs::AcceptanceConsumed { reused: true } => Some(Prop::I6Acceptance),
            _ => None,
        };
        v.extend(p);
    }
    if prev.dur.tomb & !next.dur.tomb != 0 {
        v.push(Prop::I4Tombstones);
    }
    for o in 0..OBLS {
        if let (Some(a), Some(b)) = (prev.dur.floors[o], next.dur.floors[o])
            && a.epoch == b.epoch
            && b.state < a.state
        {
            v.push(Prop::I5Floors);
        }
    }
    v
}

/// Nothing is outstanding: the only thing the holder would do is a relist
/// that changes nothing (which the checker does not explore). Invalidated and
/// rejected reports are dormant (re-derivation is R1's job; if it is needed
/// and blocked, L1 reports it). A state with no healthy move that is *not*
/// settled is a deadlock (I7).
pub fn settled(s: &State) -> bool {
    s.vol.phase == Phase::P7
        && s.vol.apply.is_none()
        && s.vol.rel_txn.is_none()
        && s.vol.route == Route::Normal
        && s.dur
            .slots
            .iter()
            .all(|p| p.is_none_or(|p| matches!(p.st, DurSt::Invalidated | DurSt::Rejected)))
        && s.dur.instr.iter().all(|i| i.reserved_units() == 0)
        && s.net.list.is_none()
        && s.net.reqs.iter().all(|b| b.is_empty())
        && s.net.resps.iter().all(|b| b.is_empty())
}

/// L1's goal: every obligation the authority holds is complete.
pub fn goal(s: &State) -> bool {
    s.auth
        .core
        .rooms
        .iter()
        .all(|r| r.obl.is_none_or(|o| o.complete()))
}
