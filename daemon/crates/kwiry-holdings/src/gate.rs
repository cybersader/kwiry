//! The core serving gate (v2 Part I, §3.4; §C.0 admit, apply, auth halt).
//!
//! A resource is servable only when all of these hold:
//! - a verified list was applied in this process;
//! - the holder is not in an auth halt caused by `403`;
//! - the resource is listed `active`, mapped (configured), not tombstoned
//!   (the no-resurrection invariant) and not refused at the admit of a newer
//!   list;
//! - its lease holds on both the wall and the monotonic clock.
//!
//! Admit narrows at once; only a successful apply broadens. A resource that
//! carries a class-defined member (§C.3) is never servable through
//! [`ServingGate::check`]: its holder-class profile's gate must add that
//! profile's rules (for Profile S, [`crate::engine::EngineGate`]).

use crate::clock::Clock;
use crate::engine::{EngineGateState, EngineRefusal, PinMember};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The holder that issued this gate was replaced or dropped; a gate never
    /// outlives its holder.
    Revoked,
    /// Auth halt after `403 holder_not_authorized`.
    AuthHalt,
    NoListThisProcess,
    /// The no-resurrection invariant: a tombstoned resource never serves.
    Tombstoned,
    /// Refused at the admit of a newer list that has not yet applied.
    RefusedAtAdmit,
    NotListedActive,
    NotMapped,
    LeaseLapsed,
    /// A resource with a class-defined member asked through the core check.
    ProfileCheckRequired,
    /// A Profile S rule.
    Engine(EngineRefusal),
}

impl Refusal {
    /// The error class a host maps to its own response.
    pub fn code(self) -> &'static str {
        match self {
            Self::Engine(refusal) => refusal.code(),
            _ => "resource_unavailable",
        }
    }

    /// A local reason code: carries no path, pin id or resource id.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Revoked => "holder_replaced",
            Self::AuthHalt => "auth_halt",
            Self::NoListThisProcess => "no_list_this_process",
            Self::Tombstoned => "tombstoned",
            Self::RefusedAtAdmit => "refused_at_admit",
            Self::NotListedActive => "not_listed_active",
            Self::NotMapped => "not_mapped",
            Self::LeaseLapsed => "lease_lapsed",
            Self::ProfileCheckRequired => "profile_check_required",
            Self::Engine(refusal) => refusal.reason(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GateEntry {
    pub active: bool,
    pub mapped: bool,
    pub lease_wall_ms: i64,
    pub lease_mono_ms: u64,
    /// `NotApplicable` unless a holder-class profile defines the member.
    pub pin: PinMember,
}

/// Everything a successful apply swaps in at once.
pub(crate) struct GateSnapshot {
    pub entries: HashMap<String, GateEntry>,
    pub tombstoned: HashSet<String>,
    pub pin_history: HashMap<String, BTreeSet<String>>,
    pub history_available: bool,
}

#[derive(Default)]
pub(crate) struct GateState {
    revoked: bool,
    list_applied_in_process: bool,
    auth_halted: bool,
    pub entries: HashMap<String, GateEntry>,
    /// Admit refusals of a list not yet applied, with their reasons.
    refused_at_admit: HashMap<String, Refusal>,
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
        if self.revoked {
            return Err(Refusal::Revoked);
        }
        if self.auth_halted {
            return Err(Refusal::AuthHalt);
        }
        if !self.list_applied_in_process {
            return Err(Refusal::NoListThisProcess);
        }
        if self.tombstoned.contains(id) {
            return Err(Refusal::Tombstoned);
        }
        if let Some(refusal) = self.refused_at_admit.get(id) {
            return Err(*refusal);
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

    /// The core rules for one resource. A resource with a class-defined
    /// member is refused here and must be asked through its profile's gate.
    pub fn check(&self, resource_id: &str) -> Result<(), Refusal> {
        let state = self.read();
        let entry = state.listed_entry(resource_id, self.clock.wall_ms(), self.clock.mono_ms())?;
        if entry.pin != PinMember::NotApplicable {
            return Err(Refusal::ProfileCheckRequired);
        }
        Ok(())
    }

    pub fn auth_halted(&self) -> bool {
        self.read().auth_halted
    }

    pub fn list_applied_in_process(&self) -> bool {
        self.read().list_applied_in_process
    }

    pub(crate) fn clock_hold_raised(&self) -> bool {
        self.read().engine.clock_hold
    }

    /// Admit: refuse at once, before any durable write. A resource already
    /// refused keeps its first reason.
    pub(crate) fn refuse_at_admit(&self, refusals: impl IntoIterator<Item = (String, Refusal)>) {
        let mut state = self.write();
        for (id, refusal) in refusals {
            state.refused_at_admit.entry(id).or_insert(refusal);
        }
    }

    /// Invalidates this gate and every clone of it, for good. Called when its
    /// holder is dropped, before the holder's writer lock is released.
    pub(crate) fn revoke(&self) {
        self.write().revoked = true;
    }

    /// Apply: swap the snapshot in; the only step that broadens serving.
    pub(crate) fn swap(&self, snapshot: GateSnapshot) {
        let mut state = self.write();
        state.entries = snapshot.entries;
        state.tombstoned = snapshot.tombstoned;
        state.engine.pin_history = snapshot.pin_history;
        state.engine.history_available = snapshot.history_available;
        state.refused_at_admit.clear();
        state.list_applied_in_process = true;
    }

    pub(crate) fn listed_ids(&self) -> HashSet<String> {
        self.read().entries.keys().cloned().collect()
    }

    /// Auth halt on `403`: nothing is servable until an operator restart.
    pub(crate) fn set_auth_halt(&self) {
        self.write().auth_halted = true;
    }
}
