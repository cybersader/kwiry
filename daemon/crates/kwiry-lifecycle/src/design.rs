//! Which revision of the lifecycle the model executes.
//!
//! [`Design::REV8`] is the revision-8 table encoded as written (with the
//! interpretations listed in the crate docs). [`Design::REV9`] adds every
//! [`Fix`]. Each fix is a separate flag so the checker can show that it is
//! necessary: removing any single fix from `REV9` must reproduce the
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
}

impl Fix {
    pub const ALL: [Fix; 9] = [
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

    const fn bit(self) -> u16 {
        1 << self as u16
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
        }
    }
}

/// A set of [`Fix`]es.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Design(u16);

impl Design {
    /// Revision 8 as written.
    pub const REV8: Design = Design(0);
    /// Revision 9: every fix.
    pub const REV9: Design = {
        let mut bits = 0;
        let mut i = 0;
        while i < Fix::ALL.len() {
            bits |= Fix::ALL[i].bit();
            i += 1;
        }
        Design(bits)
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
        let missing: Vec<_> = Fix::ALL.iter().filter(|x| !self.has(**x)).collect();
        write!(f, "REV9 without {missing:?}")
    }
}
