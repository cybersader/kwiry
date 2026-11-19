//! Admission and apply: from a verified list to a narrowed-then-broadened
//! serving gate.
//!
//! - **Admission (H rows).** First contact accepts; the same epoch accepts a
//!   `seq` at or above the high-water; a lower `seq` is `lifecycle_regressed`
//!   and any other epoch is `unexpected_epoch`. Refused lists change nothing
//!   and leases keep running down.
//! - **A0.** Before any durable write, refuse every resource the list does
//!   not show `active`, every tombstoned resource, and every pinned resource
//!   whose pin is null, changed since the list last applied in this process,
//!   or held in the pin history under another resource. These refusals
//!   survive any later failure.
//! - **A1.** One atomic commit: high-water, tombstones ∪ newly terminating
//!   or retired resources, pin-history ∪ every pin in the list.
//! - **A2–A3.** The store publishes the classification. Nothing is deleted:
//!   unknown data is withheld, tombstoned-unlisted data quarantined.
//! - **A4.** Record the applied list, then swap the gate snapshot: the only
//!   step that broadens serving.
//! - **A5.** In-memory revocation in the store.

use crate::client::{self, ClockEvidence, FetchEnv, FetchFailure, FetchParams, Transport};
use crate::clock::Clock;
use crate::events::{EventSink, HolderEvent};
use crate::gate::{GateEntry, GateSettings, GateSnapshot, ServingGate};
use crate::keys::{HolderKey, PinnedKeys};
use crate::profile::WireProfile;
use crate::schedule::{EarlyRelist, RelistScheduler};
use crate::startup::Started;
use crate::state::{ApplyMutation, HolderState, PinPair, StateWriter, Tombstone, raised_ceiling};
use crate::store::{Disposition, HeldResource, PublishPlan, ResourceStore};
use crate::verify::{VerifiedList, VerifyContext};
use crate::wire::{Item, PinMember, ResourceKey, Rev, RootPin};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

pub const DEFAULT_PAGE_SIZE: u16 = 500;

#[derive(Clone, Debug)]
pub struct HolderConfig {
    pub profile: WireProfile,
    pub page_size: u16,
    pub gate: GateSettings,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("wire profile: {0}")]
    Profile(&'static str),
    #[error("page_size must be 1–1000")]
    PageSize,
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
    config: HolderConfig,
    writer: StateWriter,
    key: HolderKey,
    store: S,
    transport: T,
    clock: Arc<dyn Clock>,
    events: Arc<dyn EventSink>,
    gate: ServingGate,
    route: RouteHealth,
    /// Pins of the list last applied in this process, by holding id. Empty at
    /// process start, so every pin of the first list counts as changed.
    baseline: HashMap<String, Option<RootPin>>,
    scheduler: RelistScheduler,
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
        let hold = started.writer.state().clock_behind_authority;
        let gate = ServingGate::new(clock.clone(), config.gate, hold);
        Ok(Self {
            config,
            writer: started.writer,
            key: started.key,
            store,
            transport,
            clock,
            events,
            gate,
            route: RouteHealth::Normal,
            baseline: HashMap::new(),
            scheduler: RelistScheduler::new(300),
        })
    }

    /// The serving gate, for the engine's request path.
    pub fn gate(&self) -> ServingGate {
        self.gate.clone()
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

    /// Fetches, verifies, admits and applies one list (row F1 onward).
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

    /// One clock probe while the hold is raised (row C21): at most once per
    /// 60 s, counted as a list start, never applied. `None` when no probe is
    /// needed or allowed now.
    pub fn probe_clock(&mut self) -> Option<Result<(), FetchFailure>> {
        let now = self.clock.mono_ms();
        if self.route != RouteHealth::Normal
            || !self.gate.clock_hold()
            || !self.scheduler.probe_due(now)
        {
            return None;
        }
        self.scheduler.on_probe(now);
        Some(self.fetch(true).map(|_| ()).inspect_err(|failure| {
            self.on_failure(failure);
        }))
    }

    /// Clock evidence from a verified capability (row C19).
    pub fn observe_capability_iat(&mut self, iat_s: i64) {
        let lag = iat_s.saturating_mul(1000) - self.clock.wall_ms();
        if lag > client::CLOCK_BEHIND_MS {
            self.raise_clock_hold(lag);
        }
    }

    /// Amendment B: a capability named a resource that is neither listed
    /// nor tombstoned. The request itself is refused by the gate meanwhile.
    pub fn request_early_relist(&mut self, resource_id: &str) -> Option<EarlyRelist> {
        let listed = self.gate.entries_snapshot().contains_key(resource_id);
        if listed || self.writer.state().is_tombstoned(resource_id) {
            return None;
        }
        Some(self.scheduler.request_early(self.clock.mono_ms()))
    }

    fn fetch(&mut self, probe: bool) -> Result<Option<VerifiedList>, FetchFailure> {
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
            if matches!(observed, ClockEvidence::Behind { .. }) {
                gate_view.raise_clock_hold();
            }
            evidence.push(observed);
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

    fn raise_clock_hold(&mut self, lag_lower_bound_ms: i64) {
        self.gate.raise_clock_hold();
        self.events
            .emit(HolderEvent::ClockBehindRaised { lag_lower_bound_ms });
        if self.writer.raise_clock_hold().is_err() {
            self.events.emit(HolderEvent::ClockHoldWriteFailed);
        }
    }

    /// Raising takes effect in memory first, then is written. Clearing is
    /// written first, then the in-memory hold drops; a failed clearing write
    /// leaves the hold raised in memory and durably.
    fn on_clock_evidence(&mut self, observed: ClockEvidence) {
        match observed {
            ClockEvidence::Behind { lag_lower_bound_ms } => {
                self.raise_clock_hold(lag_lower_bound_ms)
            }
            ClockEvidence::Measured {
                upper_bound_ms,
                rtt_ms,
            } if self.gate.clock_hold() => {
                if upper_bound_ms > client::CLOCK_BEHIND_MS {
                    self.events.emit(HolderEvent::ClockBehindUnconfirmed {
                        measured_lag_upper_bound_ms: upper_bound_ms,
                        rtt_ms,
                    });
                } else if self.writer.clear_clock_hold().is_ok() {
                    self.gate.drop_clock_hold();
                    self.events.emit(HolderEvent::ClockBehindCleared {
                        measured_lag_upper_bound_ms: upper_bound_ms,
                        rtt_ms,
                    });
                } else {
                    self.events.emit(HolderEvent::ClockHoldWriteFailed);
                }
            }
            ClockEvidence::Measured { .. } => {}
        }
    }

    fn on_failure(&mut self, failure: &FetchFailure) {
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
        let new_pairs: Vec<PinPair> = list
            .items
            .iter()
            .filter_map(|listed| {
                listed.item.root_pin.pin().map(|pin| PinPair {
                    pin_id: pin.pin_id.clone(),
                    resource_id: listed.item.resource.id.clone(),
                })
            })
            .filter(|pair| !state.pin_history.contains(pair))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let history = pin_history_map(state.pin_history.iter().chain(&new_pairs));

        // A0: narrow at once, before any durable write.
        let (refuse, conflicts) = self.admission_refusals(&list, &tombstoned, &history);
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
        let size = self.writer.state().pin_history.len() + new_pairs.len();
        let ceiling = self.writer.state().pin_history_ceiling;
        if let Some(raised) = raised_ceiling(u64::try_from(size).unwrap_or(u64::MAX), ceiling) {
            self.events.emit(HolderEvent::PinHistoryCeilingRaised {
                from: ceiling,
                to: raised,
            });
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
        self.baseline = list
            .items
            .iter()
            .filter(|listed| listed.item.root_pin != PinMember::NotApplicable)
            .map(|listed| {
                (
                    listed.item.holding_id.clone(),
                    listed.item.root_pin.pin().cloned(),
                )
            })
            .collect();
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
        history: &HashMap<String, BTreeSet<String>>,
    ) -> (HashSet<String>, usize) {
        let listed: HashMap<&str, &Item> = list
            .items
            .iter()
            .map(|listed| (listed.item.resource.id.as_str(), &listed.item))
            .collect();
        let mut refuse: HashSet<String> = self
            .gate
            .entries_snapshot()
            .into_keys()
            .filter(|id| {
                !listed
                    .get(id.as_str())
                    .is_some_and(|item| item.is_active() && !tombstoned.contains(id))
            })
            .collect();
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
                match &item.root_pin {
                    PinMember::NotApplicable => false,
                    PinMember::Unpinned => true,
                    PinMember::Pinned(pin) => {
                        let unchanged =
                            self.baseline.get(&item.holding_id) == Some(&Some(pin.clone()));
                        let conflict = history
                            .get(&pin.pin_id)
                            .is_some_and(|ids| ids.iter().any(|other| other != id));
                        !unchanged || conflict
                    }
                }
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
        let holder_kind = &self.writer.enrollment().holder_kind;
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
                    pinned: self
                        .config
                        .profile
                        .is_pinned(holder_kind, &item.resource.kind),
                    pin: item.root_pin.pin().cloned(),
                };
                (item.resource.id.clone(), entry)
            })
            .collect();
        GateSnapshot {
            entries,
            tombstoned: tombstoned.clone(),
            pin_history: pin_history_map(state.pin_history.iter()),
            history_available: !state.reset_pending,
        }
    }
}

fn pin_history_map<'a>(
    pairs: impl Iterator<Item = &'a PinPair>,
) -> HashMap<String, BTreeSet<String>> {
    let mut map: HashMap<String, BTreeSet<String>> = HashMap::new();
    for pair in pairs {
        map.entry(pair.pin_id.clone())
            .or_default()
            .insert(pair.resource_id.clone());
    }
    map
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
