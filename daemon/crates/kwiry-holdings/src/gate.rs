//! The serving gate: the one question an engine asks before it serves,
//! reports ready, or releases results for a resource.
//!
//! A resource is servable only when all of these hold:
//! - a verified list was applied in this process;
//! - the holder is not refused by the authority (`403`);
//! - the resource is listed `active`, mapped (configured), not tombstoned and
//!   not refused at admission (A0) of a newer list;
//! - its lease holds on both the wall and the monotonic clock;
//! - for pinned kinds: the pin is non-null and not listed under another
//!   resource, the artifact was built under the listed `pin_ref`, and the
//!   engine observes the storage at the listed `pin_id` (or is inside the
//!   bounded stale window for previously validated same-ref artifacts);
//! - for capability-authorized requests: the clock hold is down, and the
//!   capability's lifetime, `iat` and expiry pass at both cuts.
//!
//! Refusals narrow immediately (A0); only a completed apply (A4) broadens.

use crate::clock::Clock;
use crate::wire::RootPin;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, PoisonError, RwLock};

pub const MAX_CAPABILITY_LIFETIME_S: i64 = 60;
pub const MAX_FUTURE_IAT_S: i64 = 30;
pub const MAX_CLOCK_SKEW_ALLOWANCE_S: u32 = 30;
pub const MAX_STALE_SERVE_S: u32 = 86_400;
pub const DEFAULT_STALE_SERVE_S: u32 = 3_600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateSettings {
    pub clock_skew_allowance_ms: i64,
    pub max_stale_serve_ms: u64,
}

impl GateSettings {
    /// Refuses a skew allowance above 30 s (the engine must refuse to start)
    /// and a stale window above 86,400 s.
    pub fn new(clock_skew_allowance_s: u32, max_stale_serve_s: u32) -> Result<Self, &'static str> {
        if clock_skew_allowance_s > MAX_CLOCK_SKEW_ALLOWANCE_S {
            return Err("clock_skew_allowance above 30 s");
        }
        if max_stale_serve_s > MAX_STALE_SERVE_S {
            return Err("max_stale_serve_seconds above 86400");
        }
        Ok(Self {
            clock_skew_allowance_ms: i64::from(clock_skew_allowance_s) * 1000,
            max_stale_serve_ms: u64::from(max_stale_serve_s) * 1000,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    /// Before work starts.
    Admission,
    /// Immediately before results leave the engine.
    Release,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapabilityTimes {
    pub iat_s: i64,
    pub exp_s: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootObservation {
    /// The engine observed the storage at the listed `pin_id`.
    Matches,
    /// The observation stopped matching at this monotonic instant.
    Mismatch { detected_mono_ms: u64 },
    /// Not observed (or the observation failed).
    Unobserved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtifactStamp<'a> {
    /// `pin_ref` recorded in the artifact's durable derivation record.
    pub pin_ref: &'a str,
    /// Published by a walk whose root observation matched the listed
    /// `pin_id` under this same `pin_ref`.
    pub validated_under_matching_root: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ServeQuery<'a> {
    pub resource_id: &'a str,
    pub capability: Option<CapabilityTimes>,
    pub artifact: Option<ArtifactStamp<'a>>,
    pub root: RootObservation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    NotAuthorized,
    ClockBehindAuthority,
    CapabilityLifetime,
    CapabilityNotYetValid,
    CapabilityExpired,
    NoListThisProcess,
    Tombstoned,
    RefusedAtAdmission,
    NotListedActive,
    NotMapped,
    LeaseLapsed,
    RootPinMissing,
    RootPinPreviouslyListed,
    ArtifactPinRef,
    RootPinMismatch,
}

impl Refusal {
    /// The engine-facing error class.
    pub fn code(self) -> &'static str {
        match self {
            Self::CapabilityExpired => "capability_expired",
            Self::CapabilityLifetime | Self::CapabilityNotYetValid => "capability_invalid",
            Self::ClockBehindAuthority => "clock_behind_authority",
            _ => "resource_unavailable",
        }
    }

    /// A local reason code: carries no path, pin id or resource id.
    pub fn reason(self) -> &'static str {
        match self {
            Self::NotAuthorized => "holder_not_authorized",
            Self::ClockBehindAuthority => "clock_behind_authority",
            Self::CapabilityLifetime => "capability_lifetime",
            Self::CapabilityNotYetValid => "capability_iat_in_future",
            Self::CapabilityExpired => "capability_expired",
            Self::NoListThisProcess => "no_list_this_process",
            Self::Tombstoned => "tombstoned",
            Self::RefusedAtAdmission => "refused_at_admission",
            Self::NotListedActive => "not_listed_active",
            Self::NotMapped => "not_mapped",
            Self::LeaseLapsed => "lease_lapsed",
            Self::RootPinMissing => "root_pin_missing",
            Self::RootPinPreviouslyListed => "root_pin_previously_listed",
            Self::ArtifactPinRef => "artifact_pin_ref_not_current",
            Self::RootPinMismatch => "root_pin_mismatch",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GateEntry {
    pub active: bool,
    pub mapped: bool,
    pub lease_wall_ms: i64,
    pub lease_mono_ms: u64,
    pub pinned: bool,
    pub pin: Option<RootPin>,
}

/// Everything A4 swaps in at once.
pub(crate) struct GateSnapshot {
    pub entries: HashMap<String, GateEntry>,
    pub tombstoned: HashSet<String>,
    pub pin_history: HashMap<String, BTreeSet<String>>,
    pub history_available: bool,
}

#[derive(Default)]
struct GateState {
    list_applied_in_process: bool,
    not_authorized: bool,
    clock_hold: bool,
    entries: HashMap<String, GateEntry>,
    refused_at_admission: HashSet<String>,
    tombstoned: HashSet<String>,
    pin_history: HashMap<String, BTreeSet<String>>,
    history_available: bool,
}

impl GateState {
    fn previously_listed(&self, pin_id: &str, resource_id: &str) -> bool {
        self.pin_history
            .get(pin_id)
            .is_some_and(|ids| ids.iter().any(|id| id != resource_id))
    }
}

/// Inputs an engine walk needs to exclude other resources' storage.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WalkInputs {
    /// Current pins of other active items: meeting one is a nesting defect.
    pub nesting_pins: BTreeSet<String>,
    /// Pins of terminating or retired items and pins held in the history under
    /// another resource: skipped and counted against coverage.
    pub skip_pins: BTreeSet<String>,
    /// False after trust on first use until lists refill the history.
    pub history_available: bool,
}

#[derive(Clone)]
pub struct ServingGate {
    inner: Arc<RwLock<GateState>>,
    clock: Arc<dyn Clock>,
    settings: GateSettings,
}

impl ServingGate {
    pub(crate) fn new(clock: Arc<dyn Clock>, settings: GateSettings, clock_hold: bool) -> Self {
        let state = GateState {
            clock_hold,
            ..GateState::default()
        };
        Self {
            inner: Arc::new(RwLock::new(state)),
            clock,
            settings,
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, GateState> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, GateState> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn settings(&self) -> GateSettings {
        self.settings
    }

    pub fn check(&self, query: &ServeQuery, cut: Cut) -> Result<(), Refusal> {
        let state = self.read();
        let now_wall = self.clock.wall_ms();
        let now_mono = self.clock.mono_ms();
        if state.not_authorized {
            return Err(Refusal::NotAuthorized);
        }
        if let Some(capability) = query.capability {
            if state.clock_hold {
                return Err(Refusal::ClockBehindAuthority);
            }
            if cut == Cut::Admission {
                if capability.exp_s.saturating_sub(capability.iat_s) > MAX_CAPABILITY_LIFETIME_S {
                    return Err(Refusal::CapabilityLifetime);
                }
                if capability.iat_s.saturating_mul(1000)
                    > now_wall.saturating_add(MAX_FUTURE_IAT_S * 1000)
                {
                    return Err(Refusal::CapabilityNotYetValid);
                }
            }
            if now_wall
                > capability
                    .exp_s
                    .saturating_mul(1000)
                    .saturating_add(self.settings.clock_skew_allowance_ms)
            {
                return Err(Refusal::CapabilityExpired);
            }
        }
        if !state.list_applied_in_process {
            return Err(Refusal::NoListThisProcess);
        }
        let id = query.resource_id;
        if state.tombstoned.contains(id) {
            return Err(Refusal::Tombstoned);
        }
        if state.refused_at_admission.contains(id) {
            return Err(Refusal::RefusedAtAdmission);
        }
        let entry = state
            .entries
            .get(id)
            .filter(|e| e.active)
            .ok_or(Refusal::NotListedActive)?;
        if !entry.mapped {
            return Err(Refusal::NotMapped);
        }
        if now_wall >= entry.lease_wall_ms || now_mono >= entry.lease_mono_ms {
            return Err(Refusal::LeaseLapsed);
        }
        if !entry.pinned {
            return Ok(());
        }
        let pin = entry.pin.as_ref().ok_or(Refusal::RootPinMissing)?;
        if state.previously_listed(&pin.pin_id, id) {
            return Err(Refusal::RootPinPreviouslyListed);
        }
        let artifact = query
            .artifact
            .filter(|a| a.pin_ref == pin.pin_ref)
            .ok_or(Refusal::ArtifactPinRef)?;
        match query.root {
            RootObservation::Matches => Ok(()),
            RootObservation::Mismatch { detected_mono_ms }
                if artifact.validated_under_matching_root
                    && now_mono.saturating_sub(detected_mono_ms)
                        <= self.settings.max_stale_serve_ms =>
            {
                Ok(())
            }
            _ => Err(Refusal::RootPinMismatch),
        }
    }

    /// Whether a holder clock hold is in force.
    pub fn clock_hold(&self) -> bool {
        self.read().clock_hold
    }

    pub fn not_authorized(&self) -> bool {
        self.read().not_authorized
    }

    pub fn list_applied_in_process(&self) -> bool {
        self.read().list_applied_in_process
    }

    /// Whether `pin_id` is in the pin history under a resource other than
    /// `resource_id` (an observation tool reports `pin_id_previously_listed`).
    pub fn pin_previously_listed(&self, pin_id: &str, resource_id: &str) -> bool {
        self.read().previously_listed(pin_id, resource_id)
    }

    pub fn walk_inputs(&self, resource_id: &str) -> WalkInputs {
        let state = self.read();
        let own_pin = state
            .entries
            .get(resource_id)
            .and_then(|e| e.pin.as_ref())
            .map(|p| p.pin_id.clone());
        let mut inputs = WalkInputs {
            history_available: state.history_available,
            ..WalkInputs::default()
        };
        for (id, entry) in &state.entries {
            let (true, Some(pin)) = (id != resource_id, entry.pin.as_ref()) else {
                continue;
            };
            if entry.active {
                inputs.nesting_pins.insert(pin.pin_id.clone());
            } else {
                inputs.skip_pins.insert(pin.pin_id.clone());
            }
        }
        for (pin_id, ids) in &state.pin_history {
            if ids.iter().any(|id| id != resource_id) && own_pin.as_deref() != Some(pin_id) {
                inputs.skip_pins.insert(pin_id.clone());
            }
        }
        inputs
    }

    /// A0: refuse at once, before any durable write.
    pub(crate) fn refuse_at_admission(&self, ids: impl IntoIterator<Item = String>) {
        self.write().refused_at_admission.extend(ids);
    }

    /// A4: swap the snapshot in; the only step that broadens serving.
    pub(crate) fn swap(&self, snapshot: GateSnapshot) {
        let mut state = self.write();
        state.entries = snapshot.entries;
        state.tombstoned = snapshot.tombstoned;
        state.pin_history = snapshot.pin_history;
        state.history_available = snapshot.history_available;
        state.refused_at_admission.clear();
        state.list_applied_in_process = true;
    }

    pub(crate) fn entries_snapshot(&self) -> HashMap<String, GateEntry> {
        self.read().entries.clone()
    }

    /// `403`: nothing is servable until restart.
    pub(crate) fn set_not_authorized(&self) {
        self.write().not_authorized = true;
    }

    /// Raises the clock hold in memory. Returns false if it was already up.
    pub(crate) fn raise_clock_hold(&self) -> bool {
        !std::mem::replace(&mut self.write().clock_hold, true)
    }

    /// Drops the in-memory hold; called only after the durable clear.
    pub(crate) fn drop_clock_hold(&self) {
        self.write().clock_hold = false;
    }
}
