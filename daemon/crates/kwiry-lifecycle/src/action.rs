//! Every event the checker can interleave.
//!
//! Actions belong to one of six [`Kind`]s. Holder actors, the healthy
//! authority and network, and the operator form the *healthy* system that the
//! liveness check schedules fairly. Faults, crashes and scenario changes are
//! budgeted: they can drive the system into any reachable state, but the
//! liveness check asks what happens once they stop.

/// An error the authority (or the network) injects into a report exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FaultCode {
    /// `429`, `503 lifecycle_unavailable`, a deadline or a network failure.
    Transient,
    /// `503 authority_epoch_mismatch`.
    EpochMismatch,
    /// `401 invalid_holder_proof`.
    BadProof,
    /// `403 holder_not_authorized`.
    Forbidden,
    /// `409 idempotency_conflict`, `400 invalid_request`, `413`.
    Bug,
    ProofExpired,
}

impl FaultCode {
    pub const ALL: [FaultCode; 6] = [
        FaultCode::Transient,
        FaultCode::EpochMismatch,
        FaultCode::BadProof,
        FaultCode::Forbidden,
        FaultCode::Bug,
        FaultCode::ProofExpired,
    ];
}

/// Fault families a profile may enable (bit flags).
pub mod faults {
    /// Injected error responses on report and status exchanges.
    pub const REPORT_ERRORS: u16 = 1 << 0;
    /// Injected error responses on list fetches.
    pub const LIST_ERRORS: u16 = 1 << 1;
    /// A connection fails (request or response lost; the deadline fires).
    pub const NET_FAIL: u16 = 1 << 2;
    /// The network duplicates a request (the authority answers `401 replay`).
    pub const NET_DUP: u16 = 1 << 3;
    /// A queued attempt outlives its JWS `exp` before dispatch.
    pub const AGE_OUT: u16 = 1 << 4;
    /// The volume fills up (ENOSPC on lifecycle writes).
    pub const DISK_FULL: u16 = 1 << 5;
    /// An unlink fails.
    pub const DELETE_FAIL: u16 = 1 << 6;
    /// A response outlives the holder process that sent the request and is
    /// delivered after a restart (the late receipts R18/R19 anticipate).
    pub const LATE_DELIVERY: u16 = 1 << 7;
    pub const ALL: u16 = (1 << 8) - 1;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Holder,
    /// The healthy authority, network or volume.
    Env,
    Operator,
    Fault,
    Crash,
    /// Authority-side changes that define the scenario (retire a room, change
    /// the projection, back up and restore).
    Scenario,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    // --- M1 startup (holder)
    Boot,
    Markers,
    Narrow,
    // --- M3 / M2 / M4a (holder)
    FetchStart,
    ListDeliver,
    Apply,
    // --- M7 release (holder)
    RelBind(u8),
    RelDetach(u8),
    RelDrain(u8),
    RelDelete { i: u8, u: u8, fail: bool },
    RelDeleted(u8),
    RelResult(u8),
    // --- M5 GC and obligation purge (holder)
    GcRevoke(u8),
    GcDelete { u: u8, fail: bool },
    Quarantine(u8),
    QRetry { u: u8, fail: bool },
    PurgeDelete { u: u8, fail: bool },
    PurgeScan(u8),
    // --- M6 / M8 report actors and transport (holder)
    Enqueue(u8),
    Transmit(u8),
    Handoff(u8),
    Dispatch(u8),
    Deliver { c: u8, idx: u8 },
    Replenish,
    // --- healthy environment
    AuthProcess { c: u8, idx: u8 },
    DropDead { c: u8, idx: u8 },
    HumanRelease,
    HumanDecline,
    SpaceFreed,
    // --- faults
    AuthFault { c: u8, idx: u8, code: FaultCode },
    NetFail(u8),
    NetDup { c: u8, idx: u8 },
    Detach { c: u8, idx: u8 },
    AgeOut(u8),
    DelayCommit(u8),
    ProofBackoffElapsed(u8),
    ListFault(FaultCode),
    ListDup,
    ListLost,
    DiskFull,
    Crash,
    // --- scenario
    AuthRetire(u8),
    AuthUnlist(u8),
    AuthRelist(u8),
    AuthDrop(u8),
    AuthBackup,
    AuthRestore,
    // --- operator
    OpAcceptEpoch,
    OpRestart,
    OpClearHalt(u8),
    OpReopen(u8),
    TickExposure(u8),
    AnswerCapability { room: u8, expiry_with_skew: u8 },
    OpRestoreJournalLost,
    SwapHolder,
    LoseHolderState,
    BeginRecovery,
    CommitRecovery,
    TryLegacy,
    LeaseDeadline,
    KeepOpen,
    CloseHandle,
    LatePublish,
    ReconcileLate,
    WrongRelease { i: u8, u: u8 },
}

impl Action {
    pub fn kind(self) -> Kind {
        use Action::*;
        match self {
            RelDelete { fail: true, .. }
            | GcDelete { fail: true, .. }
            | QRetry { fail: true, .. }
            | PurgeDelete { fail: true, .. }
            | AuthFault { .. }
            | NetFail(_)
            | NetDup { .. }
            | Detach { .. }
            | AgeOut(_)
            | DelayCommit(_)
            | ListFault(_)
            | ListDup
            | ListLost
            | DiskFull => Kind::Fault,
            Crash => Kind::Crash,
            SwapHolder
            | LoseHolderState
            | TryLegacy
            | LeaseDeadline
            | KeepOpen
            | LatePublish
            | WrongRelease { .. }
            | TickExposure(_) => Kind::Scenario,
            BeginRecovery | CommitRecovery => Kind::Operator,
            AuthRetire(_) | AuthUnlist(_) | AuthRelist(_) | AuthDrop(_) | AuthBackup
            | AuthRestore => Kind::Scenario,
            OpAcceptEpoch | OpRestart | OpClearHalt(_) | OpReopen(_) | OpRestoreJournalLost => {
                Kind::Operator
            }
            AuthProcess { .. } | DropDead { .. } | HumanRelease | HumanDecline | SpaceFreed => {
                Kind::Env
            }
            _ => Kind::Holder,
        }
    }

    /// Part of the healthy suffix the liveness check schedules fairly.
    pub fn healthy(self) -> bool {
        matches!(self.kind(), Kind::Holder | Kind::Env | Kind::Operator)
    }

    /// The strong-fairness class: network actions are fair per channel, not
    /// per message slot (each delivery removes a message, so no message on a
    /// channel can starve).
    pub fn fairness_key(self) -> Action {
        match self {
            Action::Deliver { c, .. } => Action::Deliver { c, idx: 0 },
            Action::AuthProcess { c, .. } => Action::AuthProcess { c, idx: 0 },
            Action::DropDead { c, .. } => Action::DropDead { c, idx: 0 },
            a => a,
        }
    }

    /// Every action, with every parameter, that might be enabled.
    pub fn candidates() -> Vec<Action> {
        use Action::*;
        let mut v = vec![
            Boot,
            Markers,
            Narrow,
            FetchStart,
            ListDeliver,
            Apply,
            Replenish,
            HumanRelease,
            HumanDecline,
            SpaceFreed,
            ListDup,
            ListLost,
            DiskFull,
            Crash,
            AuthBackup,
            AuthRestore,
            OpRestoreJournalLost,
            OpAcceptEpoch,
            OpRestart,
            ProofBackoffElapsed(3),
            SwapHolder,
            LoseHolderState,
            BeginRecovery,
            CommitRecovery,
            TryLegacy,
            LeaseDeadline,
            KeepOpen,
            CloseHandle,
            LatePublish,
            ReconcileLate,
        ];
        for code in FaultCode::ALL {
            v.push(ListFault(code));
        }
        for i in 0..2u8 {
            v.extend([
                RelBind(i),
                RelDetach(i),
                RelDrain(i),
                RelDeleted(i),
                RelResult(i),
            ]);
            for u in 0..3u8 {
                v.push(WrongRelease { i, u });
                for fail in [false, true] {
                    v.push(RelDelete { i, u, fail });
                }
            }
        }
        for u in 0..3u8 {
            v.extend([GcRevoke(u), Quarantine(u)]);
            for fail in [false, true] {
                v.extend([
                    GcDelete { u, fail },
                    QRetry { u, fail },
                    PurgeDelete { u, fail },
                ]);
            }
        }
        for r in 0..2u8 {
            v.extend([
                PurgeScan(r),
                AuthRetire(r),
                AuthUnlist(r),
                AuthRelist(r),
                AuthDrop(r),
                OpReopen(r),
            ]);
        }
        for c in 0..3u8 {
            v.extend([
                Enqueue(c),
                Transmit(c),
                Handoff(c),
                Dispatch(c),
                NetFail(c),
                AgeOut(c),
                DelayCommit(c),
                ProofBackoffElapsed(c),
                OpClearHalt(c),
            ]);
            for idx in 0..6u8 {
                v.extend([
                    Deliver { c, idx },
                    AuthProcess { c, idx },
                    DropDead { c, idx },
                    NetDup { c, idx },
                    Detach { c, idx },
                ]);
                for code in FaultCode::ALL {
                    v.push(AuthFault { c, idx, code });
                }
            }
        }
        v
    }
}
