//! Which revision of the lifecycle the model executes.
//!
//! [`Design::REV8`] is the revision-8 table encoded as written (with the
//! interpretations listed in the crate docs). [`Design::REV10`] adds every
//! [`Fix`]. Each fix is a separate flag so the checker can show that it is
//! necessary: removing any single fix from the corresponding revision must reproduce the
//! counterexample it closes (`tests/model_check.rs`).

use std::fmt;

/// One repair to the revision-8 table. The discriminant is the bit in
/// [`Design`]; `finding()` names the review finding it closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fix {
    /// Round-8 finding 1. A durably `armed` report or status left by a restart
    /// is "sent, outcome unknown": R2/R3 (T2/T3) may follow it (rows R4a, T4a).
    RestartArmedResumes,
    /// Round-8 finding 2. The transport's dispatch is the linearization point:
    /// it re-checks the versions R2 recorded and the attempt's age immediately
    /// before the bytes leave, and cancels a stale or aged queued attempt
    /// instead of sending it (rows R2d, R2q, T2d, T2q).
    DispatchLinearized,
    /// Round-8 finding 3. A verified list's narrowing is applied to the serving
    /// snapshot in memory at admission, before any durable write, so a failed
    /// A1–A3 (K2x) can never leave a narrowed resource servable (row A0).
    NarrowFirst,
    /// Round-8 finding 4. A convergence receipt whose ack was minted in another
    /// epoch is audit-only: it never raises a floor; the report is rejected and
    /// a relist re-learns the floor from the current epoch's list (row R6e).
    ConvergenceEpochBound,
    /// Round-8 finding 5. A bug halt (R9, T6c, R17) has an audited operator
    /// recovery that re-derives under a new key (row N5).
    HaltOperatorResume,
    /// Round-8 finding 6. `401 replay` means a fresh `jti` and a retry, never an
    /// auth halt (rows F8r, R11r, T6r).
    ReplayRetries,
    /// Round-8 finding 7. A quarantine entry's owner is derived from its
    /// descriptor's current class (active → GC, obligated → obligation,
    /// unknown or tombstoned-unlisted → unattributed); only a release
    /// reservation is stored. GC-owned entries are retried (rows O7, O11g).
    DerivedQuarantineOwner,
    /// Found by this model (M-1). GC re-validates its reason when it claims: a
    /// GC-revoked object whose resource is no longer active is handed to the
    /// obligation or to quarantine, never deleted by GC (rows O5, O8, A5).
    GcClaimRevalidates,
    /// Found by this model (M-2). A release reservation whose detach (L1's G)
    /// failed is re-detached in process, not only at the next start (row L1d).
    ReleaseDetachRetries,
    /// H.1: replay committed retirements before opening a new epoch.
    RetirementReplay,
    /// H.2–H.4: witness, status, and transaction revision fidelity.
    AuthorityFidelity,
    /// H.2: send the last applied list reference.
    EchoListRef,
    /// H.6: only a currently pinned key may authorize rotation.
    SignedKeyRotation,
    /// H.3: time-only failures retry once and back off.
    ProofExpiryRetry,
    /// H.9: the human may decline without authorizing deletion.
    ReleaseDeclined,
    /// H.5: classify signed convergence evidence, not exact receipts.
    ConvergenceEnvelope,
    /// Astra: reject a sequential holder swap on a populated root.
    RootHolderBinding,
    /// Astra: prove the exact claimant against independent inventory.
    ExactDeletionEvidence,
    /// Astra: a 403 revokes the unsent transport queue.
    CancelOnForbidden,
    /// Astra: fence pre-lifecycle binaries and enforce lease/A0 during apply.
    LeaseAndRollbackFence,
    /// H.12: the shared validation pipeline preserves authenticated witnesses.
    UnifiedValidation,
    /// H.13: receipt and CAS enforce the same proof deadline.
    ProofDeadline,
    /// H.14: verify the entire journal, then replay it atomically.
    AtomicJournalReplay,
    /// H.15: bound the pre-retirement capability at the engine boundary.
    CapabilityExposure,
    /// H.17: serve the list despite an unverifiable witness, with audit.
    AdvisoryListWitness,
    /// H.18: replay finalizations and clears against restored targets too.
    RestoredReplayTargets,
    /// H.19: archive restored evidence and raise a fresh-epoch status.
    JournalLostRecovery,
    /// H.20: fail the CAS closed on an internal static-check inconsistency.
    CommitInvariant,
    /// H.21: enforce the capability and clock bounds at both request cuts.
    EngineCapabilityFence,
    /// H.16.5: index and serve a room only from a root observed at its
    /// `pin_id`, stamped with its `pin_ref`.
    RootPinServing,
    /// H.16.5 effective point: A0 refuses changed, null and conflicting pins
    /// at once; only A4 broadens serving.
    RootPinFence,
    /// H.16 / AP24: the holder's pin-history refuses a `pin_id` it holds under
    /// another room.
    RootPinHistory,
    /// H.19 item 5: `restore_journal_lost` nulls every active pin.
    JournalLostPinReset,
    /// H.16.3 (r16): replay reserves an orphaned attestation's `pin_id` as `"*"`.
    OrphanPinReserve,
}

impl Fix {
    pub const REV9: [Fix; 9] = [
        Fix::RestartArmedResumes,
        Fix::DispatchLinearized,
        Fix::NarrowFirst,
        Fix::ConvergenceEpochBound,
        Fix::HaltOperatorResume,
        Fix::ReplayRetries,
        Fix::DerivedQuarantineOwner,
        Fix::GcClaimRevalidates,
        Fix::ReleaseDetachRetries,
    ];

    pub const REV10: [Fix; 11] = [
        Fix::RetirementReplay,
        Fix::AuthorityFidelity,
        Fix::EchoListRef,
        Fix::SignedKeyRotation,
        Fix::ProofExpiryRetry,
        Fix::ReleaseDeclined,
        Fix::ConvergenceEnvelope,
        Fix::RootHolderBinding,
        Fix::ExactDeletionEvidence,
        Fix::CancelOnForbidden,
        Fix::LeaseAndRollbackFence,
    ];

    pub const REV10_1: [Fix; 4] = [
        Fix::UnifiedValidation,
        Fix::ProofDeadline,
        Fix::AtomicJournalReplay,
        Fix::CapabilityExposure,
    ];

    pub const REV11: [Fix; 10] = [
        Fix::AdvisoryListWitness,
        Fix::RestoredReplayTargets,
        Fix::JournalLostRecovery,
        Fix::CommitInvariant,
        Fix::EngineCapabilityFence,
        Fix::RootPinServing,
        Fix::RootPinFence,
        Fix::RootPinHistory,
        Fix::JournalLostPinReset,
        Fix::OrphanPinReserve,
    ];

    const fn bit(self) -> u64 {
        1 << self as u64
    }

    /// The review finding this fix closes.
    pub fn finding(self) -> &'static str {
        match self {
            Fix::RestartArmedResumes => "round-8 #1 (armed stranded after restart)",
            Fix::DispatchLinearized => "round-8 #2 (enqueue is not transmission)",
            Fix::NarrowFirst => "round-8 #3 (reserve exhaustion leaves narrowing unapplied)",
            Fix::ConvergenceEpochBound => "round-8 #4 (earlier-epoch convergence raises floor)",
            Fix::HaltOperatorResume => "round-8 #5 (bug halt has no recovery)",
            Fix::ReplayRetries => "round-8 #6 (401 replay halts instead of retrying)",
            Fix::DerivedQuarantineOwner => "round-8 #7 (GC deletion failure has no owner)",
            Fix::GcClaimRevalidates => "model M-1 (GC deletes after its reason lapsed)",
            Fix::ReleaseDetachRetries => "model M-2 (failed release detach is never retried)",
            Fix::RetirementReplay => "H.1: replay committed retirements before opening a new epoch",
            Fix::AuthorityFidelity => "H.2–H.4: witness, status, and transaction revision fidelity",
            Fix::EchoListRef => "H.2: send the last applied list reference",
            Fix::SignedKeyRotation => "H.6: only a currently pinned key may authorize rotation",
            Fix::ProofExpiryRetry => "H.3: time-only failures retry once and back off",
            Fix::ReleaseDeclined => "H.9: the human may decline without authorizing deletion",
            Fix::ConvergenceEnvelope => {
                "H.5: classify signed convergence evidence, not exact receipts"
            }
            Fix::RootHolderBinding => "Astra: reject a sequential holder swap on a populated root",
            Fix::ExactDeletionEvidence => {
                "Astra: prove the exact claimant against independent inventory"
            }
            Fix::CancelOnForbidden => "Astra: a 403 revokes the unsent transport queue",
            Fix::UnifiedValidation => {
                "H.12: authenticated witness precedes time/replay/scope/rate/grammar"
            }
            Fix::ProofDeadline => "H.13: proof duration, CAS deadline and page-zero absence",
            Fix::AtomicJournalReplay => "H.14: verified, atomic journal replay",
            Fix::CapabilityExposure => {
                "H.15: engine answers stop by the original capability deadline"
            }
            Fix::AdvisoryListWitness => "H.17: invalid list witness is advisory",
            Fix::RestoredReplayTargets => "H.18: replay also applies to restored targets",
            Fix::JournalLostRecovery => "H.19: preserve evidence and require fresh status",
            Fix::CommitInvariant => "H.20: static CAS failure aborts with an invariant event",
            Fix::EngineCapabilityFence => "H.21: engine clock and capability release checks",
            Fix::RootPinServing => "H.16.5: serve only a root observed at its pin, under its ref",
            Fix::RootPinFence => "H.16.5: A0 refuses changed, null and conflicting pins at once",
            Fix::RootPinHistory => "H.16 AP24: the pin-history refuses another room's pin_id",
            Fix::JournalLostPinReset => "H.19.5: restore_journal_lost nulls every active pin",
            Fix::OrphanPinReserve => "H.16.3: replay reserves an orphaned pin_id",
            Fix::LeaseAndRollbackFence => {
                "Astra: fence pre-lifecycle binaries and enforce lease/A0 during apply"
            }
        }
    }
}

/// A set of [`Fix`]es.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Design(u64);

impl Design {
    /// Revision 8 as written.
    pub const REV8: Design = Design(0);
    /// Revision 9: every fix.
    pub const REV9: Design = {
        let mut bits = 0;
        let mut i = 0;
        while i < Fix::REV9.len() {
            bits |= Fix::REV9[i].bit();
            i += 1;
        }
        Design(bits)
    };

    /// Revision 10: H.1–H.11 and the independent holder-side findings.
    pub const REV10: Design = {
        let mut d = Self::REV9;
        let mut i = 0;
        while i < Fix::REV10.len() {
            d = d.with(Fix::REV10[i]);
            i += 1;
        }
        d
    };

    /// The revision-10.1 fix set. The flags H.17–H.21 confirmed or added are
    /// off, so for those rules it executes the rejected readings that the
    /// ablations reproduce (for H.18: replay only to replayed targets).
    pub const REV10_1: Design = Self::REV10
        .with(Fix::UnifiedValidation)
        .with(Fix::ProofDeadline)
        .with(Fix::AtomicJournalReplay)
        .with(Fix::CapabilityExposure);

    pub const REV11: Design = {
        let mut d = Self::REV10_1;
        let mut i = 0;
        while i < Fix::REV11.len() {
            d = d.with(Fix::REV11[i]);
            i += 1;
        }
        d
    };

    pub const fn has(self, f: Fix) -> bool {
        self.0 & f.bit() != 0
    }

    pub const fn with(self, f: Fix) -> Design {
        Design(self.0 | f.bit())
    }

    pub const fn without(self, f: Fix) -> Design {
        Design(self.0 & !f.bit())
    }
}

impl fmt::Debug for Design {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Design::REV8 {
            return write!(f, "REV8");
        }
        if *self == Design::REV9 {
            return write!(f, "REV9");
        }
        if *self == Design::REV10 {
            return write!(f, "REV10");
        }
        if *self == Design::REV10_1 {
            return write!(f, "REV10.1");
        }
        if *self == Design::REV11 {
            return write!(f, "REV11");
        }
        let missing: Vec<_> = Fix::REV9
            .iter()
            .chain(&Fix::REV10)
            .chain(&Fix::REV10_1)
            .chain(&Fix::REV11)
            .filter(|x| !self.has(**x))
            .collect();
        write!(f, "REV11 without {missing:?}")
    }
}
