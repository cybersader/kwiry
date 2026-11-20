//! Admit and apply (v2 Part I §C.0, §3.4, §3.7): from a verified list to a
//! narrowed-then-broadened serving gate.
//!
//! - **High-water (§3.7, §5).** First contact accepts; the same epoch
//!   accepts a `seq` at or above the high-water; a lower `seq` is
//!   `lifecycle_regressed` and any other epoch is `unexpected_epoch`. A
//!   refused list changes nothing and leases keep running down.
//! - **Admit.** Before any durable write, refuse every resource the list does
//!   not show `active` and every tombstoned resource, plus each holder-class
//!   profile's narrowing (Profile S: changed, null or conflicting pins,
//!   H.16.5). Admit refusals survive any later failure.
//! - **Apply, durable part.** One atomic commit of the high-water, the
//!   tombstones ∪ newly terminating or retired resources (§3.4 rule 6) and
//!   (Profile S) the pin-history pairs (S.6); then the store publishes the
//!   classification. Nothing is deleted: unknown data is withheld,
//!   tombstoned-unlisted data quarantined (§3.4 rules 1 and 5).
//! - **Apply, broadening part.** Record the applied list, then swap the gate
//!   snapshot: the only step that broadens serving.
//!
//! Kwiry's row ids for these steps are A0–A5 (v2 Annex K.1).

use crate::client::{
    self, ClockEvidence, FetchEnv, FetchFailure, FetchHooks, FetchParams, Transport,
};
use crate::clock::Clock;
use crate::engine::{self, EngineEvent, EngineGate, EngineSettings, RootPin};
use crate::events::{EventSink, HolderEvent};
use crate::gate::{GateEntry, GateSnapshot, Refusal, ServingGate};
use crate::keys::{HolderKey, PinnedKeys};
use crate::profile::WireProfile;
use crate::schedule::{EarlyRelist, RelistScheduler};
use crate::startup::Started;
use crate::state::{ApplyMutation, HolderState, StateWriter, Tombstone, WriteError};
use crate::store::{Disposition, HeldResource, PublishPlan, ResourceStore};
use crate::verify::{VerifiedList, VerifyContext};
use crate::wire::{Item, ResourceKey, Rev};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

pub const DEFAULT_PAGE_SIZE: u16 = 500;

#[derive(Clone, Debug)]
pub struct HolderConfig {
    pub profile: WireProfile,
    pub page_size: u16,
    /// Required exactly when the profile has an engine section.
    pub engine: Option<EngineSettings>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("wire profile: {0}")]
    Profile(&'static str),
    #[error("page_size must be 1–1000")]
    PageSize,
    #[error("engine settings must accompany an engine profile, and only one")]
    EngineSettings,
    #[error("the enrolled holder kind is not the kind the engine profile binds")]
    HolderKind,
    #[error("could not recover durable report cancellation")]
    CancellationRecovery,
}

/// The cause of an auth halt (v2 §C.0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthHalt {
    /// `401 invalid_holder_proof`: no lifecycle traffic; serving runs down
    /// its leases.
    InvalidProof,
    /// `403 holder_not_authorized`: no traffic and nothing servable.
    NotAuthorized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteHealth {
    Normal,
    /// Lasts until an operator restart (operator resume).
    AuthHalt(AuthHalt),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyReport {
    pub rev: Rev,
    pub plan: PublishPlan,
    pub tombstone_conflicts: usize,
    pub instructions: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListOutcome {
    Applied(ApplyReport),
    Failed(FetchFailure),
    /// An apply step failed; admit refusals stay in force.
    ApplyFailed {
        step: &'static str,
    },
    /// An auth halt forbids lifecycle traffic.
    AuthHalted,
    /// The next counted list start is not allowed before this instant.
    RateLimited {
        until_mono_ms: u64,
    },
    /// A recorded release reservation is not yet confirmed by the purger:
    /// no list applies until `honor_reservations` confirms it.
    ReservationPending,
}

pub struct Holder<S: ResourceStore, T: Transport> {
    pub(crate) config: HolderConfig,
    pub(crate) writer: StateWriter,
    pub(crate) key: HolderKey,
    pub(crate) store: S,
    pub(crate) transport: T,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) events: Arc<dyn EventSink>,
    pub(crate) gate: ServingGate,
    pub(crate) engine: Option<EngineGate>,
    pub(crate) route: RouteHealth,
    /// v2 H.40: raised on a list-route `400`/`413`, until a list applies.
    pub(crate) list_request_invalid: bool,
    pub(crate) list_failed: bool,
    /// Profile S (H.24): pins of the list last applied in this process, by
    /// holding id. Empty at process start.
    baseline: HashMap<String, Option<RootPin>>,
    pub(crate) scheduler: RelistScheduler,
    /// The list last applied in this process: what obligations exist now.
    pub(crate) applied: Option<Applied>,
    /// Final scans verified in this process, by obligation id.
    pub(crate) verified: HashMap<String, crate::purge::VerifiedScan>,
    /// Report sends in the last second (≤ 10/s per holder, H.8).
    pub(crate) report_sends: std::collections::VecDeque<u64>,
    /// Unverifiable acks in a row, by obligation id (R17).
    pub(crate) unverifiable: HashMap<String, u8>,
    /// A report answer said the list is stale (`stale_list_ref`, `404`, an
    /// other-epoch ack): nothing is re-derived until a list applies.
    pub(crate) relist_before_report: bool,
    /// An admitted list failed to apply: its floors are not durable, so no
    /// report is derived or sent until a list applies (K2x).
    pub(crate) apply_failed_since_admit: bool,
    pub(crate) lifecycle_unavailable: bool,
    /// `503 authority_epoch_mismatch` on any route (H.12 steps 2 and 4):
    /// treated as unreachable, and no purge runs until a verified list
    /// applies (§4.5).
    pub(crate) rollback_suspected: bool,
    pub(crate) report_backoff_ms: u64,
    pub(crate) report_backoff_until: u64,
    pub(crate) status_backoff_ms: u64,
    pub(crate) status_backoff_until: u64,
    /// Unverifiable status acks in a row (T6c after three).
    pub(crate) status_unverifiable: u8,
    pub(crate) status_spacing_until: u64,
    pub(crate) dispatched_key: Option<String>,
    pub(crate) status_refusal: Option<crate::status::StatusRefusal>,
}

/// The parts of the list last applied in this process that obligations and
/// reports read.
#[derive(Clone, Debug)]
pub struct Applied {
    pub list_ref: crate::wire::ListRef,
    pub page0_iat_ms: i64,
    pub(crate) expires_wall_ms: i64,
    pub(crate) expires_mono_ms: u64,
    /// Every item that is not `active`.
    pub items: Vec<Item>,
    pub plan: PublishPlan,
    /// Page 0's `quarantine_release` instructions (unknown kinds were
    /// skipped whole at verification).
    pub instructions: Vec<crate::release::Instruction>,
}

impl<S: ResourceStore, T: Transport> Holder<S, T> {
    pub fn new(
        started: Started,
        config: HolderConfig,
        store: S,
        transport: T,
        clock: Arc<dyn Clock>,
        events: Arc<dyn EventSink>,
    ) -> Result<Self, ConfigError> {
        config.profile.validate().map_err(ConfigError::Profile)?;
        if !(1..=1000).contains(&config.page_size) {
            return Err(ConfigError::PageSize);
        }
        if config.profile.engine.is_some() != config.engine.is_some() {
            return Err(ConfigError::EngineSettings);
        }
        // A kind mismatch would make every pinned item look unpinned
        // (`NotApplicable`) and skip the pin rules: refuse to start instead.
        if let Some(engine) = &config.profile.engine
            && engine.holder_kind != started.writer.enrollment().holder_kind
        {
            return Err(ConfigError::HolderKind);
        }
        let state = started.writer.state();
        // Anything but a sealed clear on disk counts as raised (H.34).
        let hold = config.engine.is_some() && engine::clock_hold::durable_hold(&started.writer);
        let gate = ServingGate::new(clock.clone(), hold);
        let engine = config
            .engine
            .map(|settings| EngineGate::new(gate.clone(), settings));
        let list_request_invalid = state.list_request_invalid;
        // Monotonic clocks do not span boots. Without trusted elapsed time, wait
        // a full interval for a different key, even if the wall clock jumped.
        let status_spacing_until = if state.status.last_new_key_ms.is_some() {
            clock
                .mono_ms()
                .saturating_add(crate::status::STATUS_NEW_KEY_SPACING_MS as u64)
        } else {
            0
        };
        if engine.is_some() && state.reset_pending {
            events.emit(HolderEvent::Engine(EngineEvent::PinHistoryUnavailable));
        }
        let mut writer = started.writer;
        {
            // v2 H.45: the instant durable state becomes untrustworthy, every
            // gate clone is revoked (the B1 path) and the alert is raised,
            // inside the failing write, without waiting for a later poll.
            let gate = gate.clone();
            let events = events.clone();
            writer.set_fail_stop_hook(Box::new(move || {
                gate.revoke();
                events.emit(HolderEvent::HolderStateUnwritable);
            }));
        }
        let mut holder = Self {
            config,
            writer,
            key: started.key,
            store,
            transport,
            clock,
            events,
            gate,
            engine,
            route: RouteHealth::Normal,
            list_request_invalid,
            list_failed: false,
            baseline: HashMap::new(),
            scheduler: RelistScheduler::new(300),
            applied: None,
            verified: HashMap::new(),
            report_sends: std::collections::VecDeque::new(),
            unverifiable: HashMap::new(),
            relist_before_report: false,
            apply_failed_since_admit: false,
            rollback_suspected: false,
            lifecycle_unavailable: false,
            report_backoff_ms: 0,
            report_backoff_until: 0,
            status_backoff_ms: 0,
            status_backoff_until: 0,
            status_unverifiable: 0,
            status_refusal: None,
            status_spacing_until,
            dispatched_key: None,
        };
        holder.recover_cancellation()?;
        Ok(holder)
    }

    /// The core serving gate.
    pub fn gate(&self) -> ServingGate {
        self.gate.clone()
    }

    /// The Profile S gate, when the profile has an engine section.
    pub fn engine_gate(&self) -> Option<EngineGate> {
        self.engine.clone()
    }

    pub fn state(&self) -> &HolderState {
        self.writer.state()
    }

    /// Whether the writer refuses commits until the committed state is
    /// rewritten (a failure after a rename whose rollback also failed).
    pub fn writer_poisoned(&self) -> bool {
        self.writer.poisoned()
    }

    /// Fail-stop (`holder_state_unwritable`): durable state could not be
    /// made to agree with memory. Every gate is revoked and every entry point
    /// refuses with [`FetchFailure::HolderStateUnwritable`]; the host must
    /// terminate the holder. Returns whether the holder has stopped.
    pub fn fail_stopped(&self) -> bool {
        if self.writer.unwritable() {
            self.gate.revoke();
            return true;
        }
        false
    }

    pub fn pinned_keys(&self) -> &PinnedKeys {
        &self.writer.enrollment().pinned
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    pub fn route_health(&self) -> RouteHealth {
        self.route
    }

    pub fn scheduler(&mut self) -> &mut RelistScheduler {
        &mut self.scheduler
    }

    /// Fetches, verifies, admits and applies one list.
    pub fn list_once(&mut self) -> ListOutcome {
        if self.fail_stopped() {
            return ListOutcome::Failed(FetchFailure::HolderStateUnwritable);
        }
        if self.traffic_halted() {
            return ListOutcome::AuthHalted;
        }
        // Startup honours a recorded release reservation before any list
        // applies, so no list can re-map a unit a human approved deleting.
        if self.writer.state().status.reservation_pending() {
            self.events.emit(HolderEvent::ReleaseReservationPending);
            return ListOutcome::ReservationPending;
        }
        let now = self.clock.mono_ms();
        let start_at = self.scheduler.next_start_at();
        if now < start_at {
            return ListOutcome::RateLimited {
                until_mono_ms: start_at,
            };
        }
        self.scheduler.on_start(now);
        let fetched = self.fetch(false);
        if self.fail_stopped() {
            return ListOutcome::Failed(FetchFailure::HolderStateUnwritable);
        }
        let outcome = match fetched {
            Ok(Some(list)) => self.admit_and_apply(list),
            Ok(None) => ListOutcome::Failed(FetchFailure::Local("probe result")),
            Err(failure) => {
                self.on_failure(&failure);
                ListOutcome::Failed(failure)
            }
        };
        let finished = self.clock.mono_ms();
        match &outcome {
            ListOutcome::Applied(_) => {
                let jitter = crate::b64::random_token(8).ok().map_or(0, |t| {
                    t.bytes().fold(0u64, |a, b| a.rotate_left(5) ^ u64::from(b))
                });
                self.scheduler.on_success(finished, jitter);
                if std::mem::take(&mut self.list_request_invalid) {
                    self.events.emit(HolderEvent::ListRequestInvalidCleared);
                }
            }
            ListOutcome::Failed(failure) => {
                self.list_failed = true;
                self.schedule_after_failure(failure, finished);
            }
            _ => self.scheduler.on_failure(finished, None),
        }
        outcome
    }

    /// The backoff after a failed list attempt or probe (H.8, H.40).
    pub(crate) fn schedule_after_failure(&mut self, failure: &FetchFailure, finished: u64) {
        match failure {
            FetchFailure::RequestInvalid { .. } => self.scheduler.on_request_invalid(finished),
            FetchFailure::Unreachable { retry_after_s }
            | FetchFailure::LifecycleUnavailable { retry_after_s } => {
                let retry_ms = retry_after_s.map(|s| s.saturating_mul(1000));
                self.scheduler.on_failure(finished, retry_ms);
            }
            _ => self.scheduler.on_failure(finished, None),
        }
    }

    /// v2 §11.1 B (early relist): a request named a resource that is neither
    /// listed nor tombstoned. The request itself is refused meanwhile.
    pub fn request_early_relist(&mut self, resource_id: &str) -> Option<EarlyRelist> {
        if self.gate.listed_ids().contains(resource_id)
            || self.writer.state().is_tombstoned(resource_id)
        {
            return None;
        }
        Some(self.scheduler.request_early(self.clock.mono_ms()))
    }

    pub(crate) fn fetch(&mut self, probe: bool) -> Result<Option<VerifiedList>, FetchFailure> {
        let engine_on = self.engine.is_some();
        let Self {
            config,
            writer,
            key,
            transport,
            clock,
            events,
            gate,
            ..
        } = self;
        let enrollment = writer.enrollment().clone();
        let high_water = writer.state().high_water.clone();
        let last_list_ref = writer.state().last_list_ref.clone();
        let mut observer = Observer {
            writer,
            gate,
            events: events.as_ref(),
            engine_on,
        };
        let mut env = FetchEnv {
            transport: &*transport,
            clock: clock.as_ref(),
            key,
            ctx: VerifyContext {
                profile: &config.profile,
                issuer: &enrollment.issuer,
                holder_id: &enrollment.holder_id,
                holder_kind: &enrollment.holder_kind,
            },
            pins: &enrollment.pinned,
            hooks: &mut observer,
        };
        let params = FetchParams {
            high_water: high_water.as_ref(),
            last_list_ref: last_list_ref.as_ref(),
            page_size: config.page_size,
        };
        if probe {
            client::probe_clock(&mut env, &params).map(|()| None)
        } else {
            client::fetch_list(&mut env, &params).map(Some)
        }
    }

    /// Alerts that stay raised until their cause clears.
    pub fn raised_alerts(&self) -> Vec<&'static str> {
        let mut alerts = Vec::new();
        if self.list_request_invalid {
            alerts.push("lifecycle_list_request_invalid");
        }
        if self.gate.clock_hold_raised() {
            alerts.push("clock_behind_authority");
        }
        alerts
    }

    pub(crate) fn on_failure(&mut self, failure: &FetchFailure) {
        self.list_failed = true;
        let event = match failure {
            FetchFailure::LifecycleUnavailable { .. } => {
                self.lifecycle_unavailable = true;
                HolderEvent::AuthorityUnreachable
            }
            FetchFailure::Unreachable { .. } => HolderEvent::AuthorityUnreachable,
            FetchFailure::Deadline => HolderEvent::ListDeadline,
            FetchFailure::Invalid(error) => HolderEvent::ListInvalid {
                reason: error.to_string(),
            },
            FetchFailure::Regressed => HolderEvent::ListRegressed,
            FetchFailure::UnexpectedEpoch => HolderEvent::UnexpectedEpoch,
            FetchFailure::UnknownKid => HolderEvent::UnknownKid,
            FetchFailure::Livelock => HolderEvent::ListLivelock,
            FetchFailure::RollbackSuspected => {
                self.rollback_suspected = true;
                HolderEvent::AuthorityRollbackSuspected
            }
            FetchFailure::ProofExpiredTwice => HolderEvent::ProofExpiredClockAlert,
            FetchFailure::Unauthorized => {
                self.route = RouteHealth::AuthHalt(AuthHalt::InvalidProof);
                HolderEvent::HolderUnauthorized
            }
            FetchFailure::NotAuthorized => {
                // M10: also cancels every queued, unsent report.
                self.on_not_authorized();
                return;
            }
            FetchFailure::Rejected { status } => {
                HolderEvent::ListRequestRejected { status: *status }
            }
            FetchFailure::RequestInvalid { status } => {
                self.list_request_invalid = true;
                if let Err(error) = self.writer.raise_list_request_invalid() {
                    report_write_failure(self.events.as_ref(), &error);
                }
                HolderEvent::ListRequestInvalid { status: *status }
            }
            FetchFailure::KeysetInvalid => HolderEvent::KeysetInvalid,
            FetchFailure::Local(reason) => HolderEvent::ListInvalid {
                reason: (*reason).to_owned(),
            },
            FetchFailure::HolderStateUnwritable => HolderEvent::HolderStateUnwritable,
        };
        self.events.emit(event);
    }

    /// Emits what the disk reserve did since the last call (K2). Exhaustion
    /// is reported where the failed write surfaces.
    pub(crate) fn emit_reserve_uses(&mut self) {
        for use_ in self.writer.take_reserve_uses() {
            if use_ == crate::state::ReserveUse::Used {
                self.events.emit(HolderEvent::DiskReserveUsed);
            }
        }
    }

    /// Lets durable lifecycle writes consume `reserve` on `ENOSPC` (K2).
    pub fn set_disk_reserve(&mut self, reserve: Box<dyn crate::reserve::DiskReserve>) {
        self.writer.set_reserve(reserve);
    }

    /// K1: installs the reserve file `name` in the private directory at
    /// `dir` (created 0700 if absent, verified by descriptor), and lets
    /// lifecycle writes consume it.
    ///
    /// Nothing is ever unlinked. An existing file is adopted only if its
    /// header carries the nonce recorded (or staged) for `name` in holder
    /// state; anything else is refused (`reserve_path_foreign`) and kept. A
    /// new one follows: stage a fresh nonce (W), create the file exclusively
    /// and fsync its header and the directory, record the nonce (W), then
    /// allocate through the descriptor. A crash at any step leaves either a
    /// file the next start adopts or one it refuses and keeps.
    #[cfg(unix)]
    pub fn install_file_reserve(
        &mut self,
        dir: &std::path::Path,
        name: &str,
        target: u64,
    ) -> Result<crate::reserve::ReserveLevel, crate::reserve::ReserveError> {
        use crate::reserve::{DiskReserve, FileReserve, ReserveDir, ReserveError};
        let write_error = |e: WriteError| ReserveError::Io(std::io::Error::other(e.to_string()));
        let dir = ReserveDir::open(dir)?;
        let record = self
            .writer
            .state()
            .reserves
            .get(name)
            .cloned()
            .unwrap_or_default();
        let mut reserve = match FileReserve::adopt(&dir, name, target, &record.accepted())? {
            Some((reserve, nonce)) => {
                self.writer
                    .record_reserve(name, &nonce)
                    .map_err(write_error)?;
                reserve
            }
            None => {
                let nonce = crate::b64::random_token(32)
                    .map_err(|e| ReserveError::Io(std::io::Error::other(e.to_string())))?;
                self.writer
                    .stage_reserve(name, &nonce)
                    .map_err(write_error)?;
                let reserve = FileReserve::create(&dir, name, target, &nonce)?;
                self.writer
                    .record_reserve(name, &nonce)
                    .map_err(write_error)?;
                reserve
            }
        };
        let level = reserve.replenish()?;
        self.writer.set_reserve(Box::new(reserve));
        Ok(level)
    }

    /// K3: whether a new artifact write may start. A refusal alerts
    /// (`disk_pressure`) and stops the write; it never evicts or deletes.
    pub fn admit_artifact_write(
        &self,
        budget: &crate::reserve::StorageBudget,
        usage: crate::reserve::StorageUsage,
    ) -> bool {
        let admitted = budget.admits(usage);
        if !admitted {
            self.events.emit(HolderEvent::DiskPressure);
        }
        admitted
    }

    fn apply_failed(&mut self, step: &'static str, error: Option<&WriteError>) -> ListOutcome {
        // The admitted list may carry floors that are not durable: no report
        // runs until a list applies (K2x).
        self.apply_failed_since_admit = true;
        self.emit_reserve_uses();
        if let Some(error) = error {
            report_write_failure(self.events.as_ref(), error);
        }
        self.events.emit(HolderEvent::ApplyFailed { step });
        ListOutcome::ApplyFailed { step }
    }

    fn admit_and_apply(&mut self, list: VerifiedList) -> ListOutcome {
        let rev = list.head.rev.clone();
        if let Some(high_water) = &self.writer.state().high_water {
            let failure = if rev.epoch != high_water.epoch {
                Some(FetchFailure::UnexpectedEpoch)
            } else if rev.seq < high_water.seq {
                Some(FetchFailure::Regressed)
            } else {
                None
            };
            if let Some(failure) = failure {
                self.on_failure(&failure);
                return ListOutcome::Failed(failure);
            }
        }
        if list.skipped_instructions > 0 {
            self.events.emit(HolderEvent::InstructionsSkipped {
                count: list.skipped_instructions,
            });
        }
        let declared = &self.config.profile.obligation_reasons;
        let unrecognised = list
            .items
            .iter()
            .filter_map(|listed| listed.item.obligation.as_ref())
            .filter(|obligation| !declared.contains(&obligation.reason))
            .count();
        if unrecognised > 0 {
            self.events.emit(HolderEvent::ReasonUnrecognised {
                count: unrecognised,
            });
        }

        let engine_on = self.engine.is_some();
        let state = self.writer.state();
        let new_tombstones: Vec<Tombstone> = list
            .items
            .iter()
            .filter(|listed| !listed.item.is_active())
            .map(|listed| Tombstone {
                resource_id: listed.item.resource.id.clone(),
                resource_kind: listed.item.resource.kind.clone(),
                holding_id: listed.item.holding_id.clone(),
            })
            .filter(|t| !state.tombstones.contains(t))
            .collect();
        let tombstoned: HashSet<String> = state
            .tombstones
            .iter()
            .chain(&new_tombstones)
            .map(|t| t.resource_id.clone())
            .collect();
        let new_pairs = if engine_on {
            engine::pin_history::new_pairs(&list.items, &state.engine.pin_history)
        } else {
            Vec::new()
        };

        // Admit: narrow at once, before any durable write.
        let (refuse, conflicts) = self.admit_refusals(&list, &tombstoned, &new_pairs);
        self.gate.refuse_at_admit(refuse);
        if conflicts > 0 {
            self.events
                .emit(HolderEvent::TombstoneConflict { count: conflicts });
        }

        let Ok(held) = self.store.held() else {
            return self.apply_failed("held", None);
        };
        let plan = classify(&list, &held, &tombstoned);

        // Apply, durable part: one atomic commit.
        if engine_on {
            let engine_state = &self.writer.state().engine;
            let size = engine_state.pin_history.len() + new_pairs.len();
            let size = u64::try_from(size).unwrap_or(u64::MAX);
            let ceiling = engine_state.pin_history_ceiling;
            if let Some(raised) = engine::pin_history::raised_ceiling(size, ceiling) {
                self.events
                    .emit(HolderEvent::Engine(EngineEvent::PinHistoryCeilingRaised {
                        from: ceiling,
                        to: raised,
                    }));
            }
        }
        // Floors, ended channels and resolved attempts ride in A1 (R12, R13,
        // R15): if they cannot be made durable, the apply fails whole.
        let (book, prune) = self.adopted_book(&list);
        let mutation = ApplyMutation {
            rev: rev.clone(),
            tombstones: new_tombstones,
            pin_pairs: new_pairs,
            obligations: Some(book),
        };
        if let Err(error) = self.writer.apply_list(mutation) {
            return self.apply_failed("A1", Some(&error));
        }
        // A1 superseded the list applied before: nothing may be purged or
        // reported on its strength any more. Until this apply reaches A4,
        // obligations refuse (`ApplyIncomplete`). Final scans stay: each is
        // bound to the classification it read and lapses when that moves.
        self.applied = None;
        if self.store.publish(&plan).is_err() {
            return self.apply_failed("A3", None);
        }
        // Apply, broadening part.
        if let Err(error) = self.writer.record_applied(list.list_ref()) {
            return self.apply_failed("A4", Some(&error));
        }
        let snapshot = self.snapshot(&list, &plan, &tombstoned);
        self.gate.swap(snapshot);
        if engine_on {
            self.baseline = engine::root_pin::baseline(&list.items);
        }
        self.scheduler
            .set_relist_seconds(list.head.lease.relist_seconds);
        self.store.revoke(&plan);
        let lease_ms = list.head.lease.serve_seconds.saturating_mul(1000);
        self.applied = Some(Applied {
            expires_wall_ms: list
                .page0_iat_ms
                .saturating_add(i64::try_from(lease_ms).unwrap_or(i64::MAX)),
            expires_mono_ms: list.page0_received_mono_ms.saturating_add(lease_ms),
            list_ref: list.list_ref(),
            page0_iat_ms: list.page0_iat_ms,
            items: list
                .items
                .iter()
                .filter(|listed| !listed.item.is_active())
                .map(|listed| listed.item.clone())
                .collect(),
            plan: plan.clone(),
            instructions: list
                .instructions
                .iter()
                .filter_map(crate::release::Instruction::parse)
                .collect(),
        });
        self.after_apply(&prune);
        self.emit_reserve_uses();
        ListOutcome::Applied(ApplyReport {
            rev,
            plan,
            tombstone_conflicts: conflicts,
            instructions: list.instructions.len(),
        })
    }

    fn admit_refusals(
        &self,
        list: &VerifiedList,
        tombstoned: &HashSet<String>,
        new_pairs: &[engine::PinPair],
    ) -> (HashMap<String, Refusal>, usize) {
        let listed: HashMap<&str, &Item> = list
            .items
            .iter()
            .map(|listed| (listed.item.resource.id.as_str(), &listed.item))
            .collect();
        let mut refuse: HashMap<String, Refusal> = self
            .gate
            .listed_ids()
            .into_iter()
            .filter(|id| {
                !listed
                    .get(id.as_str())
                    .is_some_and(|item| item.is_active() && !tombstoned.contains(id))
            })
            .map(|id| (id, Refusal::RefusedAtAdmit))
            .collect();
        let history = self.engine.as_ref().map(|_| {
            engine::pin_history::map(
                self.writer
                    .state()
                    .engine
                    .pin_history
                    .iter()
                    .chain(new_pairs),
            )
        });
        let mut conflicts = 0;
        for listed in &list.items {
            let item = &listed.item;
            let id = &item.resource.id;
            let refusal = if !item.is_active() {
                Some(Refusal::RefusedAtAdmit)
            } else if tombstoned.contains(id) {
                conflicts += 1;
                Some(Refusal::Tombstoned)
            } else {
                history.as_ref().and_then(|history| {
                    engine::root_pin::refused_at_admit(item, &self.baseline, history)
                })
            };
            if let Some(refusal) = refusal {
                refuse.insert(id.clone(), refusal);
            }
        }
        (refuse, conflicts)
    }

    fn snapshot(
        &self,
        list: &VerifiedList,
        plan: &PublishPlan,
        tombstoned: &HashSet<String>,
    ) -> GateSnapshot {
        let state = self.writer.state();
        let serve_ms = i64::try_from(list.head.lease.serve_seconds)
            .unwrap_or(i64::MAX)
            .saturating_mul(1000);
        let entries = list
            .items
            .iter()
            .map(|listed| {
                let item = &listed.item;
                // H.39, H.46: the monotonic deadline is
                // `t_recv(page 0) + (quarantine_after − iat(page 0))`. A later
                // page's own receipt time is never paired with its own `iat`:
                // its longer transit would push the deadline later.
                let remaining = item
                    .quarantine_after_ms
                    .saturating_sub(list.page0_iat_ms)
                    .clamp(0, serve_ms);
                let entry = GateEntry {
                    active: item.is_active() && !tombstoned.contains(&item.resource.id),
                    mapped: matches!(
                        plan.dispositions.get(&item.resource),
                        Some(Disposition::Mapped { .. })
                    ),
                    lease_wall_ms: item.quarantine_after_ms,
                    lease_mono_ms: list
                        .page0_received_mono_ms
                        .saturating_add(u64::try_from(remaining).unwrap_or(0)),
                    pin: item.root_pin.clone(),
                };
                (item.resource.id.clone(), entry)
            })
            .collect();
        GateSnapshot {
            entries,
            tombstoned: tombstoned.clone(),
            pin_history: engine::pin_history::map(state.engine.pin_history.iter()),
            history_available: !state.reset_pending,
        }
    }
}

/// Classifies everything held or listed (§3.4). A listed item matches a held
/// resource only on the full `(kind, id)` key; tombstones match on id alone.
pub fn classify(
    list: &VerifiedList,
    held: &[HeldResource],
    tombstoned: &HashSet<String>,
) -> PublishPlan {
    let listed: HashMap<&ResourceKey, &Item> = list
        .items
        .iter()
        .map(|listed| (&listed.item.resource, &listed.item))
        .collect();
    let held_by_key: HashMap<&ResourceKey, &HeldResource> =
        held.iter().map(|h| (&h.key, h)).collect();
    let keys: BTreeSet<&ResourceKey> = listed.keys().chain(held_by_key.keys()).copied().collect();
    let mut plan = PublishPlan::default();
    for key in keys {
        let resource = held_by_key.get(key);
        let configured = resource.is_some_and(|r| r.configured);
        let is_tombstoned = tombstoned.contains(&key.id);
        let disposition = match listed.get(key) {
            Some(item) if item.is_active() && is_tombstoned => Disposition::TombstoneConflict {
                holding_id: item.holding_id.clone(),
            },
            Some(item) if item.is_active() && configured => Disposition::Mapped {
                holding_id: item.holding_id.clone(),
            },
            Some(item) if item.is_active() => Disposition::WithheldUnconfigured {
                holding_id: item.holding_id.clone(),
            },
            Some(item) => {
                let holding_id = item.holding_id.clone();
                let obligation_id = item
                    .obligation
                    .as_ref()
                    .map(|o| o.obligation_id.clone())
                    .unwrap_or_default();
                if item.is_obligated() {
                    Disposition::Obligated {
                        holding_id,
                        obligation_id,
                    }
                } else {
                    Disposition::RetiredComplete {
                        holding_id,
                        obligation_id,
                    }
                }
            }
            None if is_tombstoned => Disposition::QuarantinedUnattributed,
            None => Disposition::WithheldUnknown,
        };
        if disposition.is_unattributed() {
            plan.unattributed_units = plan
                .unattributed_units
                .saturating_add(resource.map_or(0, |r| r.units));
        }
        plan.dispositions.insert(key.clone(), disposition);
    }
    plan
}

impl<S: ResourceStore, T: Transport> Drop for Holder<S, T> {
    /// Revokes every outstanding clone of the gate before the writer, and
    /// with it the holder lock, is released. A replacement holder can then
    /// never race a gate that still shows the old snapshot.
    fn drop(&mut self) {
        self.gate.revoke();
    }
}

/// Storage exhaustion (v2 §C.0): a lifecycle write failed for lack of space.
/// The calling step fails with no state change; admit refusals stay in
/// force; nothing is deleted.
pub(crate) fn report_write_failure(events: &dyn EventSink, error: &WriteError) {
    if let WriteError::Io(io) = error
        && io.kind() == std::io::ErrorKind::StorageFull
    {
        events.emit(HolderEvent::StorageExhausted);
    }
}

/// Everything a running fetch hands back, applied at once and in order with
/// the single holder-state writer.
struct Observer<'a> {
    writer: &'a mut StateWriter,
    gate: &'a ServingGate,
    events: &'a dyn EventSink,
    engine_on: bool,
}

impl FetchHooks for Observer<'_> {
    fn accept_pins(&mut self, pins: &PinnedKeys) -> bool {
        let accepted = self.writer.pin_keys(pins.clone()).is_ok();
        if accepted && let Some(keyset_seq) = pins.keyset_seq {
            self.events.emit(HolderEvent::KeysetAccepted { keyset_seq });
        }
        accepted
    }

    fn on_clock(&mut self, evidence: ClockEvidence) {
        if self.engine_on {
            engine::clock_hold::observe(self.writer, self.gate, self.events, evidence);
        }
    }

    fn on_verified(&mut self, evidence: &[ClockEvidence]) {
        if self.engine_on {
            engine::clock_hold::observe_verified(self.writer, self.gate, self.events, evidence);
        }
    }
}
