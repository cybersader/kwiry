//! Local holder alerts. Events carry codes and counts only: never a resource
//! id, pin id, nonce, digest or tag.

use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HolderEvent {
    /// Trust on first use: holder state and data were both proven lost.
    HolderStateReset,
    /// The pin-history set was initialised fresh; walk exclusions are weaker
    /// until lists refill it. Once per start.
    PinHistoryUnavailable,
    /// A list failed verification and was discarded.
    ListInvalid { reason: String },
    /// Same epoch, lower `seq` than the high-water.
    ListRegressed,
    /// A list from an epoch other than the high-water's.
    UnexpectedEpoch,
    /// A tombstoned resource was listed `active`; it stays refused.
    TombstoneConflict { count: usize },
    /// Five consecutive `410 list_expired`.
    ListLivelock,
    /// Network failure, deadline, `429` or `503 lifecycle_unavailable`.
    AuthorityUnreachable,
    /// `503 authority_epoch_mismatch`.
    AuthorityRollbackSuspected,
    /// `401 invalid_holder_proof`: no lifecycle traffic until restart.
    HolderUnauthorized,
    /// `403 holder_not_authorized`: nothing servable until restart.
    HolderNotAuthorized,
    /// Two consecutive `401 proof_expired`.
    ProofExpiredClockAlert,
    /// `400`/`413` or an answer the list route never gives.
    ListRequestRejected { status: u16 },
    /// A verified page or capability proved the holder clock lags by > 30 s.
    ClockBehindRaised { lag_lower_bound_ms: i64 },
    /// A nonce-bound page proved the lag is at most 30 s.
    ClockBehindCleared {
        measured_lag_upper_bound_ms: i64,
        rtt_ms: u64,
    },
    /// A nonce-bound page did not confirm a correction while the hold is raised.
    ClockBehindUnconfirmed {
        measured_lag_upper_bound_ms: i64,
        rtt_ms: u64,
    },
    /// Writing the clock hold failed; it stays raised in memory.
    ClockHoldWriteFailed,
    /// A key-set statement was verified and its keys pinned.
    KeysetAccepted { keyset_seq: u64 },
    /// An artefact named a key that no verifiable key-set statement adds.
    UnknownKid,
    /// The pin-history ceiling would be exceeded; it was raised, never evicted.
    PinHistoryCeilingRaised { from: u64, to: u64 },
    /// Instructions of an unknown kind were skipped.
    InstructionsSkipped { count: usize },
    /// An apply step failed; A0 refusals stay in force.
    ApplyFailed { step: &'static str },
    /// The whole-list deadline expired or the fetch was cancelled.
    ListDeadline,
}

impl HolderEvent {
    pub fn code(&self) -> &'static str {
        match self {
            Self::HolderStateReset => "holder_state_reset",
            Self::PinHistoryUnavailable => "pin_history_unavailable",
            Self::ListInvalid { .. } => "lifecycle_list_invalid",
            Self::ListRegressed => "lifecycle_regressed",
            Self::UnexpectedEpoch => "unexpected_epoch",
            Self::TombstoneConflict { .. } => "tombstone_conflict",
            Self::ListLivelock => "lifecycle_list_livelock",
            Self::AuthorityUnreachable => "lifecycle_unreachable",
            Self::AuthorityRollbackSuspected => "authority_rollback_suspected",
            Self::HolderUnauthorized => "lifecycle_unauthorized",
            Self::HolderNotAuthorized => "holder_not_authorized",
            Self::ProofExpiredClockAlert => "lifecycle_proof_expired",
            Self::ListRequestRejected { .. } => "lifecycle_list_rejected",
            Self::ClockBehindRaised { .. } => "clock_behind_authority",
            Self::ClockBehindCleared { .. } => "clock_behind_authority_cleared",
            Self::ClockBehindUnconfirmed { .. } => "clock_behind_authority_unconfirmed",
            Self::ClockHoldWriteFailed => "clock_hold_write_failed",
            Self::KeysetAccepted { .. } => "lifecycle_keyset_accepted",
            Self::UnknownKid => "unknown_kid",
            Self::PinHistoryCeilingRaised { .. } => "pin_history_ceiling_raised",
            Self::InstructionsSkipped { .. } => "lifecycle_instruction_skipped",
            Self::ApplyFailed { .. } => "lifecycle_apply_failed",
            Self::ListDeadline => "lifecycle_list_deadline",
        }
    }
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: HolderEvent);
}

/// Keeps every event in memory; useful for composition-level inspection.
#[derive(Default)]
pub struct MemorySink {
    events: Mutex<Vec<HolderEvent>>,
}

impl MemorySink {
    pub fn events(&self) -> Vec<HolderEvent> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn codes(&self) -> Vec<&'static str> {
        self.events().iter().map(HolderEvent::code).collect()
    }

    pub fn clear(&self) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

impl EventSink for MemorySink {
    fn emit(&self, event: HolderEvent) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event);
    }
}
