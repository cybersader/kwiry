//! The serving gate: the generic question a holder asks before it serves,
//! reports ready, or releases anything for a resource.
//!
//! A resource is servable only when all of these hold:
//! - a verified list was applied in this process;
//! - the authority has not refused this holder (`403`);
//! - the resource is listed `active`, mapped (configured), not tombstoned
//!   and not refused at admission (A0) of a newer list;
//! - its lease holds on both the wall and the monotonic clock.
//!
//! Refusals narrow at once (A0); only a completed apply (A4) broadens.
//! Resources that carry a storage pin are never servable through
//! [`ServingGate::check`]: they need the engine profile's
//! [`crate::engine::EngineGate`], which adds the pin and capability rules.

use crate::clock::Clock;
use crate::engine::{EngineGateState, PinMember};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    NotAuthorized,
    NoListThisProcess,
    Tombstoned,
    RefusedAtAdmission,
    NotListedActive,
    NotMapped,
    LeaseLapsed,
    /// A pinned resource asked through the core check.
    EngineCheckRequired,
    ClockBehindAuthority,
    CapabilityLifetime,
    CapabilityNotYetValid,
    CapabilityExpired,
    RootPinMissing,
    RootPinPreviouslyListed,
    ArtifactPinRef,
    RootPinMismatch,
}

impl Refusal {
    /// The error class a host maps to its own response.
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
            Self::NoListThisProcess => "no_list_this_process",
            Self::Tombstoned => "tombstoned",
            Self::RefusedAtAdmission => "refused_at_admission",
            Self::NotListedActive => "not_listed_active",
            Self::NotMapped => "not_mapped",
            Self::LeaseLapsed => "lease_lapsed",
            Self::EngineCheckRequired => "engine_check_required",
            Self::ClockBehindAuthority => "clock_behind_authority",
            Self::CapabilityLifetime => "capability_lifetime",
            Self::CapabilityNotYetValid => "capability_iat_in_future",
            Self::CapabilityExpired => "capability_expired",
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
    /// `NotApplicable` unless the engine profile pins this resource's kind.
    pub pin: PinMember,
}

/// Everything A4 swaps in at once.
pub(crate) struct GateSnapshot {
    pub entries: HashMap<String, GateEntry>,
    pub tombstoned: HashSet<String>,
    pub pin_history: HashMap<String, BTreeSet<String>>,
    pub history_available: bool,
}

#[derive(Default)]
pub(crate) struct GateState {
    list_applied_in_process: bool,
    not_authorized: bool,
    pub entries: HashMap<String, GateEntry>,
    refused_at_admission: HashSet<String>,
    tombstoned: HashSet<String>,
    pub engine: EngineGateState,
}

impl GateState {
    /// Every core rule; returns the listed entry that passed them.
    pub(crate) fn listed_entry(
        &self,
        id: &str,
        now_wall: i64,
        now_mono: u64,
    ) -> Result<&GateEntry, Refusal> {
        if self.not_authorized {
            return Err(Refusal::NotAuthorized);
        }
        if !self.list_applied_in_process {
            return Err(Refusal::NoListThisProcess);
        }
        if self.tombstoned.contains(id) {
            return Err(Refusal::Tombstoned);
        }
        if self.refused_at_admission.contains(id) {
            return Err(Refusal::RefusedAtAdmission);
        }
        let entry = self
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
        Ok(entry)
    }
}

#[derive(Clone)]
pub struct ServingGate {
    inner: Arc<RwLock<GateState>>,
    pub(crate) clock: Arc<dyn Clock>,
}

impl ServingGate {
    pub(crate) fn new(clock: Arc<dyn Clock>, clock_hold: bool) -> Self {
        let mut state = GateState::default();
        state.engine.clock_hold = clock_hold;
        Self {
            inner: Arc::new(RwLock::new(state)),
            clock,
        }
    }

    pub(crate) fn read(&self) -> RwLockReadGuard<'_, GateState> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, GateState> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// The core rules for one resource. A pinned resource is refused here
    /// and must be asked through the engine gate.
    pub fn check(&self, resource_id: &str) -> Result<(), Refusal> {
        let state = self.read();
        let entry = state.listed_entry(resource_id, self.clock.wall_ms(), self.clock.mono_ms())?;
        if entry.pin != PinMember::NotApplicable {
            return Err(Refusal::EngineCheckRequired);
        }
        Ok(())
    }

    pub fn not_authorized(&self) -> bool {
        self.read().not_authorized
    }

    pub fn list_applied_in_process(&self) -> bool {
        self.read().list_applied_in_process
    }

    pub(crate) fn clock_hold_raised(&self) -> bool {
        self.read().engine.clock_hold
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
        state.engine.pin_history = snapshot.pin_history;
        state.engine.history_available = snapshot.history_available;
        state.refused_at_admission.clear();
        state.list_applied_in_process = true;
    }

    pub(crate) fn listed_ids(&self) -> HashSet<String> {
        self.read().entries.keys().cloned().collect()
    }

    /// `403`: nothing is servable until restart.
    pub(crate) fn set_not_authorized(&self) {
        self.write().not_authorized = true;
    }
}
