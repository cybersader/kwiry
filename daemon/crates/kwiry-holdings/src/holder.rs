//! Admission and apply: from a verified list to a narrowed-then-broadened
//! serving gate.
//!
//! - **Admission.** First contact accepts; the same epoch accepts a `seq` at
//!   or above the high-water; a lower `seq` is `lifecycle_regressed` and any
//!   other epoch is `unexpected_epoch`. Refused lists change nothing and
//!   leases keep running down.
//! - **A0.** Before any durable write, refuse every resource the list does
//!   not show `active` and every tombstoned resource; with the engine
//!   profile, also every pin that is null, changed or held under another
//!   resource. These refusals survive any later failure.
//! - **A1.** One atomic commit: high-water, tombstones ∪ newly terminating
//!   or retired resources, and (engine profile) pin-history ∪ the list's pins.
//! - **A2–A3.** The store publishes the classification. Nothing is deleted:
//!   unknown data is withheld, tombstoned-unlisted data quarantined.
//! - **A4.** Record the applied list, then swap the gate snapshot: the only
//!   step that broadens serving.
//! - **A5.** In-memory revocation in the store.

use crate::client::{self, ClockEvidence, FetchEnv, FetchFailure, FetchParams, Transport};
use crate::clock::Clock;
use crate::engine::{self, EngineGate, EngineSettings, RootPin};
use crate::events::{EventSink, HolderEvent};
use crate::gate::{GateEntry, GateSnapshot, ServingGate};
use crate::keys::{HolderKey, PinnedKeys};
use crate::profile::WireProfile;
use crate::schedule::{EarlyRelist, RelistScheduler};
use crate::startup::Started;
use crate::state::{ApplyMutation, HolderState, StateWriter, Tombstone};
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteHealth {
    Normal,
    /// `401 invalid_holder_proof`: no lifecycle traffic until restart;
    /// serving continues until leases lapse.
    Unauthorized,
    /// `403 holder_not_authorized`: nothing servable until restart.
    NotAuthorized,
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
    /// An apply step failed; A0 refusals stay in force.
    ApplyFailed {
        step: &'static str,
    },
    /// Route health forbids lifecycle traffic.
    Halted,
    /// The next counted list start is not allowed before this instant.
    RateLimited {
        until_mono_ms: u64,
    },
}

pub struct Holder<S: ResourceStore, T: Transport> {
    pub(crate) config: HolderConfig,
    pub(crate) writer: StateWriter,
    key: HolderKey,
    store: S,
    transport: T,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) events: Arc<dyn EventSink>,
    pub(crate) gate: ServingGate,
    pub(crate) engine: Option<EngineGate>,
    pub(crate) route: RouteHealth,
    /// Engine profile: pins of the list last applied in this process, by
    /// holding id. Empty at process start.
    baseline: HashMap<String, Option<RootPin>>,
    pub(crate) scheduler: RelistScheduler,
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
        let hold = started.writer.state().engine.clock_behind_authority;
        let gate = ServingGate::new(clock.clone(), hold);
        let engine = config
            .engine
            .map(|settings| EngineGate::new(gate.clone(), settings));
        Ok(Self {
            config,
            writer: started.writer,
            key: started.key,
            store,
            transport,
            clock,
            events,
            gate,
            engine,
            route: RouteHealth::Normal,
            baseline: HashMap::new(),
            scheduler: RelistScheduler::new(300),
        })
    }

    /// The core serving gate.
    pub fn gate(&self) -> ServingGate {
        self.gate.clone()
    }

    /// The engine profile's gate, when the profile has one.
    pub fn engine_gate(&self) -> Option<EngineGate> {
        self.engine.clone()
    }

    pub fn state(&self) -> &HolderState {
        self.writer.state()
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
        if self.route != RouteHealth::Normal {
            return ListOutcome::Halted;
        }
        let now = self.clock.mono_ms();
        if !self.scheduler.may_start(now) {
            return ListOutcome::RateLimited {
                until_mono_ms: self.scheduler.earliest_start(),
            };
        }
        self.scheduler.on_start(now);
        let outcome = match self.fetch(false) {
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
            }
            ListOutcome::Failed(FetchFailure::Unreachable { retry_after_s }) => {
                let retry_ms = retry_after_s.map(|s| s.saturating_mul(1000));
                self.scheduler.on_failure(finished, retry_ms);
            }
            _ => self.scheduler.on_failure(finished, None),
        }
        outcome
    }

    /// Amendment B: a request named a resource that is neither listed nor
    /// tombstoned. The request itself is refused by the gate meanwhile.
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
        let mut evidence = Vec::new();
        let gate_view: &ServingGate = gate;
        let sink: &dyn EventSink = events.as_ref();
        let mut accept = |pins: &PinnedKeys| -> bool {
            let accepted = writer.pin_keys(pins.clone()).is_ok();
            if accepted && let Some(keyset_seq) = pins.keyset_seq {
                sink.emit(HolderEvent::KeysetAccepted { keyset_seq });
            }
            accepted
        };
        let mut on_clock = |observed: ClockEvidence| {
            if engine_on {
                engine::capability::raise_on_signed_page(gate_view, &observed);
                evidence.push(observed);
            }
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
            accept_pins: &mut accept,
            on_clock: &mut on_clock,
        };
        let params = FetchParams {
            high_water: high_water.as_ref(),
            last_list_ref: last_list_ref.as_ref(),
            page_size: config.page_size,
        };
        let result = if probe {
            client::probe_clock(&mut env, &params).map(|()| None)
        } else {
            client::fetch_list(&mut env, &params).map(Some)
        };
        for observed in evidence {
            self.on_clock_evidence(observed);
        }
        result
    }

    pub(crate) fn on_failure(&mut self, failure: &FetchFailure) {
        let event = match failure {
            FetchFailure::Unreachable { .. } => HolderEvent::AuthorityUnreachable,
            FetchFailure::Deadline => HolderEvent::ListDeadline,
            FetchFailure::Invalid(error) => HolderEvent::ListInvalid {
                reason: error.to_string(),
            },
            FetchFailure::Regressed => HolderEvent::ListRegressed,
            FetchFailure::UnexpectedEpoch => HolderEvent::UnexpectedEpoch,
            FetchFailure::UnknownKid => HolderEvent::UnknownKid,
            FetchFailure::Livelock => HolderEvent::ListLivelock,
            FetchFailure::RollbackSuspected => HolderEvent::AuthorityRollbackSuspected,
            FetchFailure::ProofExpiredTwice => HolderEvent::ProofExpiredClockAlert,
            FetchFailure::Unauthorized => {
                self.route = RouteHealth::Unauthorized;
                HolderEvent::HolderUnauthorized
            }
            FetchFailure::NotAuthorized => {
                self.route = RouteHealth::NotAuthorized;
                self.gate.set_not_authorized();
                HolderEvent::HolderNotAuthorized
            }
            FetchFailure::Rejected { status } => {
                HolderEvent::ListRequestRejected { status: *status }
            }
            FetchFailure::Local(reason) => HolderEvent::ListInvalid {
                reason: (*reason).to_owned(),
            },
        };
        self.events.emit(event);
    }

    fn apply_failed(&self, step: &'static str) -> ListOutcome {
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
            engine::pin::new_pairs(&list.items, &state.engine.pin_history)
        } else {
            Vec::new()
        };

        // A0: narrow at once, before any durable write.
        let (refuse, conflicts) = self.admission_refusals(&list, &tombstoned, &new_pairs);
        self.gate.refuse_at_admission(refuse);
        if conflicts > 0 {
            self.events
                .emit(HolderEvent::TombstoneConflict { count: conflicts });
        }

        let Ok(held) = self.store.held() else {
            return self.apply_failed("held");
        };
        let plan = classify(&list, &held, &tombstoned);

        // A1.
        if engine_on {
            let engine_state = &self.writer.state().engine;
            let size = engine_state.pin_history.len() + new_pairs.len();
            let ceiling = engine_state.pin_history_ceiling;
            let size = u64::try_from(size).unwrap_or(u64::MAX);
            if let Some(raised) = engine::pin::raised_ceiling(size, ceiling) {
                self.events.emit(HolderEvent::PinHistoryCeilingRaised {
                    from: ceiling,
                    to: raised,
                });
            }
        }
        let mutation = ApplyMutation {
            rev: rev.clone(),
            tombstones: new_tombstones,
            pin_pairs: new_pairs,
        };
        if self.writer.apply_list(mutation).is_err() {
            return self.apply_failed("A1");
        }
        // A2–A3.
        if self.store.publish(&plan).is_err() {
            return self.apply_failed("A3");
        }
        // A4.
        if self.writer.record_applied(list.list_ref()).is_err() {
            return self.apply_failed("A4");
        }
        let snapshot = self.snapshot(&list, &plan, &tombstoned);
        self.gate.swap(snapshot);
        if engine_on {
            self.baseline = engine::pin::baseline(&list.items);
        }
        self.scheduler
            .set_relist_seconds(list.head.lease.relist_seconds);
        // A5.
        self.store.revoke(&plan);
        ListOutcome::Applied(ApplyReport {
            rev,
            plan,
            tombstone_conflicts: conflicts,
            instructions: list.instructions.len(),
        })
    }

    fn admission_refusals(
        &self,
        list: &VerifiedList,
        tombstoned: &HashSet<String>,
        new_pairs: &[engine::PinPair],
    ) -> (HashSet<String>, usize) {
        let listed: HashMap<&str, &Item> = list
            .items
            .iter()
            .map(|listed| (listed.item.resource.id.as_str(), &listed.item))
            .collect();
        let mut refuse: HashSet<String> = self
            .gate
            .listed_ids()
            .into_iter()
            .filter(|id| {
                !listed
                    .get(id.as_str())
                    .is_some_and(|item| item.is_active() && !tombstoned.contains(id))
            })
            .collect();
        let history = self.engine.as_ref().map(|_| {
            engine::pin::history_map(
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
            let refused = if !item.is_active() {
                true
            } else if tombstoned.contains(id) {
                conflicts += 1;
                true
            } else {
                history.as_ref().is_some_and(|history| {
                    engine::pin::refused_at_admission(item, &self.baseline, history)
                })
            };
            if refused {
                refuse.insert(id.clone());
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
                let remaining = item
                    .quarantine_after_ms
                    .saturating_sub(listed.page_iat_ms)
                    .clamp(0, serve_ms);
                let entry = GateEntry {
                    active: item.is_active() && !tombstoned.contains(&item.resource.id),
                    mapped: matches!(
                        plan.dispositions.get(&item.resource),
                        Some(Disposition::Mapped { .. })
                    ),
                    lease_wall_ms: item.quarantine_after_ms,
                    lease_mono_ms: listed
                        .received_mono_ms
                        .saturating_add(u64::try_from(remaining).unwrap_or(0)),
                    pin: item.root_pin.clone(),
                };
                (item.resource.id.clone(), entry)
            })
            .collect();
        GateSnapshot {
            entries,
            tombstoned: tombstoned.clone(),
            pin_history: engine::pin::history_map(state.engine.pin_history.iter()),
            history_available: !state.reset_pending,
        }
    }
}

/// Classifies everything held or listed. A listed item matches a held
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
