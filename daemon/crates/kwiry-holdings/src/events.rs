//! Local holder alerts. Events carry codes and counts only: never a resource
//! id, pin id, nonce, digest or tag.

use crate::engine::EngineEvent;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HolderEvent {
    /// Trust on first use: holder state and data were both proven lost.
    HolderStateReset,
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
    /// Treat as unreachable: network failure, deadline, `429` or
    /// `503 lifecycle_unavailable`.
    AuthorityUnreachable,
    /// `503 authority_epoch_mismatch`.
    AuthorityRollbackSuspected,
    /// Auth halt after `401 invalid_holder_proof`.
    HolderUnauthorized,
    /// Auth halt after `403 holder_not_authorized`.
    HolderNotAuthorized,
    /// Two consecutive `401 proof_expired`.
    ProofExpiredClockAlert,
    /// An answer the list route never gives (`409`).
    ListRequestRejected { status: u16 },
    /// v2 H.40: `400`/`413` on the list route. The alert stays raised until a
    /// list applies; every retry is rebuilt with a fresh nonce and `jti`.
    ListRequestInvalid { status: u16 },
    /// A list applied after `lifecycle_list_request_invalid`.
    ListRequestInvalidCleared,
    /// v2 H.37.6: a verified key-set statement would leave no pinned key.
    KeysetInvalid,
    /// A key-set statement was verified and its keys pinned.
    KeysetAccepted { keyset_seq: u64 },
    /// An artefact named a key that no verifiable key-set statement adds.
    UnknownKid,
    /// Instructions of an unknown kind were skipped.
    InstructionsSkipped { count: usize },
    /// Obligations carried a `reason` this holder class does not declare;
    /// they were treated as opaque.
    ReasonUnrecognised { count: usize },
    /// An apply step failed; admit refusals stay in force.
    ApplyFailed { step: &'static str },
    /// Storage exhaustion: a durable lifecycle write failed for lack of space.
    StorageExhausted,
    /// Fatal: durable state could not be made to agree with memory. The
    /// holder fail-stopped; the host must terminate it.
    HolderStateUnwritable,
    /// The whole-list deadline expired or the fetch was cancelled.
    ListDeadline,
    /// K2: a lifecycle write consumed reserve bytes and succeeded.
    DiskReserveUsed,
    /// K3: a new artifact write was refused by the storage budget. Nothing
    /// was evicted or deleted.
    DiskPressure,
    /// Purge: deletions failed and will be retried; the units stay
    /// attributed.
    PurgeDeleteFailed { count: usize },
    /// Purge: units still have open handles; deletion waits for them.
    PurgeWaitingOnHandles { count: usize },
    /// Purge: the purger refused a step or failed; nothing was assumed.
    PurgeStepFailed { step: &'static str },
    /// The remaining inventory moved after the final scan (a late builder
    /// output, a handle, a reclassification): the scan no longer proves
    /// anything and the pass repeats.
    PurgeScanInvalidated,
    /// A report channel bug halt (`400`, `413`, `409 idempotency_conflict`,
    /// repeated unverifiable acks). An operator resume re-derives it.
    ReportBugHalt { status: u16 },
    /// A verified ack that matches no path; nothing was stored.
    AckUnverifiable,
    /// A replay receipt for a key the authority definitively refused.
    AckContradiction,
    /// A convergence ack (`409 state_regression` or `already_complete`, or a
    /// mismatching key): the floor rose, never a receipt for the sent key.
    ReportConverged,
    /// An ack minted in another epoch: audit only, no floor change; relist.
    AckOtherEpoch,
    /// The authority definitively refused a report
    /// (`complete_refused_unattributed`, `stale_list_ref`,
    /// `obligation_not_found`); it is re-derived.
    ReportRejected { status: u16 },
    /// `403`: queued, unsent reports were cancelled (possibly sent attempts
    /// are kept as uncertain).
    ReportsCancelled { count: usize },
    /// A holder-class profile alert.
    Engine(EngineEvent),
}

impl HolderEvent {
    pub fn code(&self) -> &'static str {
        match self {
            Self::HolderStateReset => "holder_state_reset",
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
            Self::ListRequestInvalid { .. } => "lifecycle_list_request_invalid",
            Self::ListRequestInvalidCleared => "lifecycle_list_request_invalid_cleared",
            Self::KeysetInvalid => "keyset_invalid",
            Self::KeysetAccepted { .. } => "lifecycle_keyset_accepted",
            Self::UnknownKid => "unknown_kid",
            Self::InstructionsSkipped { .. } => "lifecycle_instruction_skipped",
            Self::ReasonUnrecognised { .. } => "lifecycle_reason_unrecognised",
            Self::ApplyFailed { .. } => "lifecycle_apply_failed",
            Self::StorageExhausted => "disk_reserve_exhausted",
            Self::HolderStateUnwritable => "holder_state_unwritable",
            Self::ListDeadline => "lifecycle_list_deadline",
            Self::DiskReserveUsed => "disk_reserve_used",
            Self::DiskPressure => "disk_pressure",
            Self::PurgeDeleteFailed { .. } => "lifecycle_purge_delete_failed",
            Self::PurgeWaitingOnHandles { .. } => "lifecycle_purge_waiting_on_handles",
            Self::PurgeStepFailed { .. } => "lifecycle_purge_step_failed",
            Self::PurgeScanInvalidated => "lifecycle_purge_scan_invalidated",
            Self::ReportBugHalt { .. } => "lifecycle_report_bug_halt",
            Self::AckUnverifiable => "ack_unverifiable",
            Self::AckContradiction => "ack_contradiction",
            Self::ReportConverged => "lifecycle_report_converged",
            Self::AckOtherEpoch => "lifecycle_ack_other_epoch",
            Self::ReportRejected { .. } => "lifecycle_report_rejected",
            Self::ReportsCancelled { .. } => "lifecycle_reports_cancelled",
            Self::Engine(event) => event.code(),
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
