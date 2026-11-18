//! The properties the checker proves in every reachable state and on every
//! transition.
//!
//! | Id | Property |
//! |---|---|
//! | I1 | A resource is servable only if the newest list admitted by an H row shows it `active` and it is not tombstoned (including during apply); retirements cannot be minted or resurrected in a newer list |
//! | I2 | A unit is deleted only under a listed obligation for its resource, that exact release claimant and reservation, or GC of an unreferenced object of an active resource |
//! | I3 | `complete` is never dispatched while the unattributed inventory is non-empty, while anything of the resource remains, or while its proof is stale |
//! | I4 | Tombstones only grow |
//! | I5 | Floors are monotone within an epoch, belong to the high-water's epoch, and evidence minted in another epoch never raises them |
//! | I6 | Each armed epoch acceptance is consumed at most once |
//! | I7 | Some healthy actor (holder, environment or operator) can move whenever any work is outstanding |
//! | I8 | An operator wait (auth halt, bug halt) is entered only on a fault, never on a correct authority or network response |
//! | I9 | The unattributed inventory never counts data attributable to a listed holding (except units a release reserved) |
//! | L1 | Liveness: from every reachable state, once crashes, faults and scenario changes stop, every ghost retirement reaches `complete` or `blocked`, every consumed epoch acceptance is answered by a status accepted in the new epoch, and after an epoch change minting resumes, through member reports or the lease timeout, including for a member that never reports (H.25, H.30), under strongly fair scheduling of the holder, the healthy authority and network, the human release decision and the operator |

use crate::state::*;
use crate::step::{Obs, Outcome, delete_authorized, mintable, reserving, room_class, servable};

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
    I1Retirement,
    I8ForbiddenSend,
    I8DeclineVisible,
    I1WrongHolder,
    I1Lease,
    I1UntrustedKey,
    I8AuthorityFidelity,
    I8ListWitness,
    I8ConvergenceEnvelope,
    I8ValidationPrecedence,
    I8ProofDeadline,
    I1UnverifiedRestore,
    I1CapabilityExposure,
    I8AdvisoryListWitness,
    I8CommitInvariant,
    I8ReplayTarget,
    I1RootPin,
    I1EpochExposure,
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
                "L1: an obligation never reaches complete or blocked under fair healthy scheduling (fair cycle or dead end)"
            }
            Prop::I1Retirement => {
                "I1: a ghost retirement became mintable or servable under a post-retirement list"
            }
            Prop::I8ForbiddenSend => "I8: an unsent report reached the wire after a 403 halt",
            Prop::I8DeclineVisible => {
                "I8: a human release decline did not block the eligible obligation"
            }
            Prop::I1WrongHolder => "I1: a non-empty root was opened under another holder identity",
            Prop::I1Lease => "I1: governed data was served without a live verified lease",
            Prop::I1UntrustedKey => {
                "I1: an unpinned signing key authorized a list without a valid pinned statement"
            }
            Prop::I8AuthorityFidelity => {
                "I8: the authority missed rollback evidence or failed to advance seq on acceptance"
            }
            Prop::I8ListWitness => {
                "I8: the list request omitted a verified rollback witness from its last applied list"
            }
            Prop::I8ConvergenceEnvelope => {
                "I8: a valid state_regression current_ack extension was not decoded"
            }
            Prop::I8ValidationPrecedence => {
                "I8: a later validation rejected a real rollback witness"
            }
            Prop::I8ProofDeadline => "I8: a report committed beyond the proof deadline",
            Prop::I1UnverifiedRestore => "I1: a corrupt journal did not fence restore/minting",
            Prop::I1CapabilityExposure => {
                "I1: a pre-retirement capability answered beyond its deadline"
            }
            Prop::I8AdvisoryListWitness => "I8: an unverifiable advisory witness refused the list",
            Prop::I8CommitInvariant => "I8: an inconsistent static check did not abort the CAS",
            Prop::I1RootPin => {
                "I1: a room served under a null, changed-unapplied, conflicting, pre-journal-loss or orphaned pin, from another room's bytes or another pin_ref's artifacts, or past the stale bound"
            }
            Prop::I1EpochExposure => {
                "I1: a holder's earlier-epoch list released results more than 90 s after minting stopped"
            }
            Prop::I8ReplayTarget => {
                "I8: a replayed finalization or clear left its restored target outstanding"
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
    if s.env.legacy_started && (0..ROOMS).any(|r| servable(s, r))
        || (s.env.probe == Probe::Lease
            && s.env.clock >= s.vol.admitted_until
            && s.vol.applied == s.vol.admitted
            && (0..ROOMS).any(|r| servable(s, r)))
    {
        v.push(Prop::I1Lease);
    }
    if i1 {
        v.push(Prop::I1Serving);
    }
    if !s.env.legacy_started && (0..ROOMS).any(|r| servable(s, r) && root_pin_unsafe(s, r)) {
        v.push(Prop::I1RootPin);
    }
    if (0..ROOMS).any(|r| {
        s.ghost.retired_at[r].is_some_and(|retired| {
            mintable(s, r) || (servable(s, r) && s.vol.applied.is_some_and(|l| l.rev >= retired))
        })
    }) {
        v.push(Prop::I1Retirement);
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

/// H.16: a served room's admitted pin is non-null and already applied; under
/// a list of the epoch a `restore_journal_lost` opened (or later) it was
/// minted after that decision; it is no orphaned directory; its artifacts
/// hold only its own bytes and were built under that `pin_ref` (H.24); and a
/// root mismatch is within H10.5.
fn root_pin_unsafe(s: &State, r: usize) -> bool {
    let admitted = s.vol.admitted.and_then(|l| l.pins[r]);
    let Some(l) = s.vol.applied else {
        return true;
    };
    let Some(p) = l.pins[r] else {
        return true;
    };
    let (floor_epoch, floor) = s.ghost.pin_floor;
    let (orphan_epoch, orphans) = s.ghost.orphan_dirs;
    admitted != Some(p)
        || (l.rev.epoch >= floor_epoch && p.pin_ref < floor)
        || (l.rev.epoch >= orphan_epoch && orphans & bit(p.id as usize) != 0)
        || s.ghost.derived_from[r] != Some(r as u8)
        || s.ghost.derived_ref[r] != Some(p.pin_ref)
        || (s.env.root_dir[r] != p.id && s.env.stale_expired)
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
            Obs::Deleted { u, claimant } if !delete_authorized(prev, u as usize, claimant) => {
                Some(Prop::I2Deletion)
            }
            Obs::RootOpenedAsWrongHolder => Some(Prop::I1WrongHolder),
            Obs::UntrustedKey => Some(Prop::I1UntrustedKey),
            Obs::AuthorityUnfaithful => Some(Prop::I8AuthorityFidelity),
            Obs::ListWitnessOmitted => Some(Prop::I8ListWitness),
            Obs::WrongConvergenceEnvelope => Some(Prop::I8ConvergenceEnvelope),
            Obs::ValidationPrecedence => Some(Prop::I8ValidationPrecedence),
            Obs::ProofDeadlineMissed => Some(Prop::I8ProofDeadline),
            Obs::UnverifiedRestore => Some(Prop::I1UnverifiedRestore),
            Obs::CapabilityOverexposed => Some(Prop::I1CapabilityExposure),
            Obs::EpochOverexposed => Some(Prop::I1EpochExposure),
            Obs::AdvisoryWitnessRejected => Some(Prop::I8AdvisoryListWitness),
            Obs::CommitInvariantMissed => Some(Prop::I8CommitInvariant),
            Obs::ReplayTargetMissed => Some(Prop::I8ReplayTarget),
            Obs::SentWhileForbidden => Some(Prop::I8ForbiddenSend),
            Obs::DeclineInvisible => Some(Prop::I8DeclineVisible),
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

/// A retirement cannot disappear from the goal when the authority is restored.
/// An epoch acceptance is answered by a status accepted in the new epoch
/// (HW §15 H.19), whatever its count, and minting resumes after an epoch
/// change, through member reports or the lease timeout (H.25, H.30).
pub fn goal(s: &State) -> bool {
    !s.ghost.status_due && !s.ghost.search_due && retirements_done(s)
}

/// Every ghost retirement reached `complete` or `blocked`: the obligation
/// half of L1. H.22's ablation checks it without the operator's status
/// resume.
pub fn retirements_done(s: &State) -> bool {
    (0..ROOMS).all(|r| {
        s.ghost.retired_at[r].is_none()
            || s.auth.core.rooms[r]
                .obl
                .is_some_and(|o| o.complete() || o.blocked)
    })
}
