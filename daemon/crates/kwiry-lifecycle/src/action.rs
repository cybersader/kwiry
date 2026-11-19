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
    RelDelete {
        i: u8,
        u: u8,
        fail: bool,
    },
    RelDeleted(u8),
    RelResult(u8),
    // --- M5 GC and obligation purge (holder)
    GcRevoke(u8),
    GcDelete {
        u: u8,
        fail: bool,
    },
    Quarantine(u8),
    QRetry {
        u: u8,
        fail: bool,
    },
    PurgeDelete {
        u: u8,
        fail: bool,
    },
    PurgeScan(u8),
    // --- M6 / M8 report actors and transport (holder)
    Enqueue(u8),
    Transmit(u8),
    Handoff(u8),
    Dispatch(u8),
    Deliver {
        c: u8,
        idx: u8,
    },
    Replenish,
    // --- healthy environment
    AuthProcess {
        c: u8,
        idx: u8,
    },
    DropDead {
        c: u8,
        idx: u8,
    },
    HumanRelease,
    HumanDecline,
    SpaceFreed,
    // --- faults
    AuthFault {
        c: u8,
        idx: u8,
        code: FaultCode,
    },
    NetFail(u8),
    NetDup {
        c: u8,
        idx: u8,
    },
    Detach {
        c: u8,
        idx: u8,
    },
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
    /// The focused probes' clock reaches this many seconds.
    TickExposure(u16),
    AnswerCapability {
        room: u8,
        expiry_with_skew: u8,
    },
    /// H.21 focused inputs: the capability and page are already verified.
    StartCapability {
        room: u8,
        iat: i16,
        exp: i16,
    },
    FinishCapability,
    /// A verified list page with authority `iat`. `rtt: Some(r)`: it answers
    /// a list request this process sent, with the matching `nonce` echo, and
    /// `r` seconds passed on the monotonic clock since that request was sent.
    /// `None`: not bound to this process's nonce (nonce-less, replayed, or
    /// answering an earlier process's request).
    VerifyListPage {
        iat: i16,
        rtt: Option<u8>,
    },
    /// H.23 probe: one `page_size: 1` list request, at most once per 60 s.
    ClockProbe,
    /// 60 s pass: the next probe window opens.
    ProbeWindow,
    /// A local NTP or clock-step event: the true lag becomes `lag` seconds
    /// (negative: the engine clock runs ahead).
    ClockStep {
        lag: i8,
    },
    /// An operator command asking to clear `clock_behind_authority`.
    OpClearClock,
    /// H.26: the durable write of a `clock_behind_authority` the holder
    /// raised in memory (C19's W).
    PersistClockHold,
    /// H.30 and H.31 (focused probe): an admin enrolls engine holder `p` of
    /// the id-space; it lists at once.
    PeerEnroll(u8),
    /// An admin disables peer `p`.
    PeerDisable(u8),
    /// An admin re-enables peer `p`.
    PeerEnable(u8),
    /// An admin rotates peer `p`'s key (the holder signs with the new key).
    PeerRotate(u8),
    /// Peer `p` accepts the new epoch and applies a list of it (needs an
    /// enrollment the authority recognizes).
    PeerMove(u8),
    /// Peer `p`'s status report with a current-epoch `list_ref`.
    PeerReport(u8),
    /// Peer `p` is gone or stuck: it never reports again.
    PeerSilent(u8),
    /// A capability minted now is presented to peer `p`.
    PeerAnswer(u8),
    /// H.30.2(b): the authority clock passes `T_stop + serve_seconds_ceiling
    /// + 150 s`.
    LeaseTimeout,
    /// H.30.3: an admin asks to clear the mint gate early.
    OpMintOverride,
    /// H.32.3: a disabled peer `p`'s request is answered `403
    /// holder_not_authorized`, which the authority records. `received`: the
    /// peer gets it and halts; otherwise the response is lost.
    PeerForbidden {
        p: u8,
        received: bool,
    },
    /// H.32.2: while lists are still served, peer `p` has been refused on
    /// every route since an instant more than `serve_seconds_ceiling + 150 s`
    /// ago.
    PeerRefusalAged(u8),
    /// H.33.1: peer `p` receives page 0 of a list of the current epoch;
    /// later pages are still to be fetched under its cursor.
    PeerListStart(u8),
    /// H.33.1: peer `p` presents its cursor for the remaining pages.
    PeerListPage(u8),
    /// The old authority instance, still running after `T_stop` was
    /// recorded, issues peer `p` an earlier-epoch list. Reachable under the
    /// rejected H.33.2 and H.44 readings (a restore without the confirmation,
    /// or `T_stop` taken before it) and after a false attestation (H.44.4's
    /// residual, which H.48.3 narrows).
    PeerOldList(u8),
    /// HW v2 H.48.3: peer `p` fetches a list of the new epoch after an exit
    /// by attestation. It applies the key-set statement before it verifies
    /// the list, then refuses the list with `unexpected_epoch` until its
    /// operator's `accept-epoch`, so it keeps serving its earlier-epoch list.
    PeerSeesEpoch(u8),
    /// HW v2 H.49: the old instance, still running after a false attestation,
    /// mints a search capability that is presented to engine `p` after the
    /// exit. It mints for rooms the restored lineage retired, which it does
    /// not know about, so an answer is exposure past the H.49.2 bound.
    PeerOldCapability(u8),
    /// H.33.1: an operator epoch (`cause: "operator"`). Its transaction is
    /// `T_stop`.
    OpEpoch,
    /// H.33.2, H.44.3(a): the restore tooling verifies on the source
    /// deployment that the old authority instance is stopped and its
    /// start-up disabled, then records `T_stop` (H.44.4: the confirmation
    /// instant). On a raw-restored instance it is the confirmation of its
    /// exit.
    RestoreBegin,
    /// H.44.3(b): the source is unreachable, so an admin records the audited
    /// `old_instance_stopped` attestation (deciding level, actor, how it was
    /// stopped), with an unsampled security event. `T_stop` is its instant.
    /// The attestation is true. H.48.1: accepted only after a recorded failed
    /// source check no older than 1 h, otherwise `attestation_precondition_unmet`.
    AttestOldStopped,
    /// H.44.4 residual: the same attestation, but false. The old instance is
    /// still running and keeps issuing lists and minting (H.48.3, H.49.3).
    FalseAttestation,
    /// HW v2 H.48.1: the restore tooling tries the source deployment (host,
    /// store-lock path, lifecycle URL) and fails. The audited record names
    /// each target, its error and the time.
    SourceCheckFails,
    /// HW v2 H.48.1: the recorded failed check becomes older than 1 h.
    SourceCheckAges,
    /// H.44.1–2: an instance started from a raw restore of the authority
    /// backup, bypassing the tooling. The F startup check detects it, so it
    /// starts in `authority_rollback_suspected`; the old instance may still
    /// run.
    RawRestore,
    /// H.44.3: the operator runs the supported restore procedure on a
    /// raw-restored instance (after its confirmation). A corrupt journal ends
    /// in H12j, and `OpRestoreJournalLost` follows.
    OpRestore,
    /// H.41 (gate fixtures): an ordinary holder-affecting write after the
    /// backup (an accepted report, a deadline), outside the journal. It
    /// advances every holder's `holder_rev` (coalesced at most once per 5 s,
    /// §3.7; the model takes one).
    AuthTouch,
    /// H.16 focused inputs. `dir: None` is `--pin-id none`; `stale` is a
    /// stale `--expect-current`.
    AttestRoot {
        room: u8,
        dir: Option<u8>,
        stale: bool,
    },
    RepointRoot {
        room: u8,
        dir: u8,
    },
    PinRebuild(u8),
    StaleDeadline,
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
    WrongRelease {
        i: u8,
        u: u8,
    },
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
            | AttestRoot { .. }
            | RepointRoot { .. }
            | StaleDeadline
            | ProbeWindow
            | ClockStep { .. }
            | TickExposure(_)
            | PeerEnroll(_)
            | PeerDisable(_)
            | PeerEnable(_)
            | PeerRotate(_)
            | PeerSilent(_)
            | PeerAnswer(_)
            | PeerForbidden { .. }
            | PeerRefusalAged(_)
            | PeerListStart(_)
            | PeerListPage(_)
            | PeerOldList(_)
            | PeerSeesEpoch(_)
            | PeerOldCapability(_)
            | OpEpoch
            | RestoreBegin
            | AttestOldStopped
            | FalseAttestation
            | SourceCheckFails
            | SourceCheckAges
            | RawRestore
            | AuthTouch => Kind::Scenario,
            // H.44: once the confirmation is recorded, the operator completes
            // the restore procedure; L1 requires that it does.
            BeginRecovery | CommitRecovery | OpClearClock | OpMintOverride | OpRestore => {
                Kind::Operator
            }
            // Another holder's progress and the passage of time are fair.
            PeerMove(_) | PeerReport(_) | LeaseTimeout => Kind::Env,
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
