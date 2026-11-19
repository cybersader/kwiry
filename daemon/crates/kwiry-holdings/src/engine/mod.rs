//! The search-engine holder profile.
//!
//! An engine answers capability-authorized requests from artifacts it built
//! over storage roots. On top of the generic core it needs:
//!
//! - [`pin`]: storage-root pins delivered in the list, the grow-only pin
//!   history, admission refusals for changed, null or conflicting pins, the
//!   artifact and root checks at serving time, and walk exclusions;
//! - [`capability`]: the capability-exposure bounds (lifetime, future `iat`,
//!   expiry plus skew at admission and at release) and the clock hold that
//!   keeps them true in real time.
//!
//! The generic core never depends on these rules. A holder whose wire
//! profile has no engine section (a device, a sync client, a backup target)
//! never reaches this module; for it, every pin member is refused and every
//! engine-owned durable field stays empty.

pub mod capability;
pub mod pin;

pub use capability::{CapabilityTimes, Cut};
pub use pin::{ArtifactStamp, PinMember, PinPair, RootObservation, RootPin, WalkInputs};

use crate::gate::{Refusal, ServingGate};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

pub const MAX_STALE_SERVE_S: u32 = 86_400;
pub const DEFAULT_STALE_SERVE_S: u32 = 3_600;
pub const DEFAULT_PIN_HISTORY_CEILING: u64 = 100_000;

/// The engine section of a wire profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineProfile {
    /// The holder kind this section applies to.
    pub holder_kind: String,
    /// The resource kind whose items carry the storage-pin member.
    pub pinned_resource_kind: String,
}

impl EngineProfile {
    pub fn pins(&self, holder_kind: &str, resource_kind: &str) -> bool {
        self.holder_kind == holder_kind && self.pinned_resource_kind == resource_kind
    }
}

/// Operator settings of the engine profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineSettings {
    pub clock_skew_allowance_ms: i64,
    pub max_stale_serve_ms: u64,
}

impl EngineSettings {
    /// Refuses a skew allowance above 30 s (the engine must refuse to start)
    /// and a stale window above 86,400 s.
    pub fn new(clock_skew_allowance_s: u32, max_stale_serve_s: u32) -> Result<Self, &'static str> {
        if clock_skew_allowance_s > capability::MAX_CLOCK_SKEW_ALLOWANCE_S {
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

/// Engine-owned durable fields. They live in the holder-state file and
/// change only through its single writer; every field only ever refuses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineState {
    /// `(pin_id, resource_id)` pairs from every applied list; never evicted.
    pub pin_history: BTreeSet<PinPair>,
    pub pin_history_ceiling: u64,
    /// The holder clock was proven to lag the authority by more than 30 s.
    pub clock_behind_authority: bool,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            pin_history: BTreeSet::new(),
            pin_history_ceiling: DEFAULT_PIN_HISTORY_CEILING,
            clock_behind_authority: false,
        }
    }
}

/// Engine fields of the in-memory gate state.
#[derive(Default)]
pub(crate) struct EngineGateState {
    pub clock_hold: bool,
    /// `pin_id → resource ids`, from the durable history.
    pub pin_history: HashMap<String, BTreeSet<String>>,
    pub history_available: bool,
}

/// What an engine asks before it serves, reports ready or releases results.
#[derive(Clone, Copy, Debug)]
pub struct EngineQuery<'a> {
    pub resource_id: &'a str,
    pub capability: Option<CapabilityTimes>,
    pub artifact: Option<ArtifactStamp<'a>>,
    pub root: RootObservation,
}

/// The serving gate with the engine profile's checks on top of the core's.
#[derive(Clone)]
pub struct EngineGate {
    gate: ServingGate,
    settings: EngineSettings,
}

impl EngineGate {
    pub(crate) fn new(gate: ServingGate, settings: EngineSettings) -> Self {
        Self { gate, settings }
    }

    pub fn core(&self) -> &ServingGate {
        &self.gate
    }

    pub fn settings(&self) -> EngineSettings {
        self.settings
    }

    /// Capability bounds and clock hold, then every core rule, then the
    /// storage-pin rules for pinned resources.
    pub fn check(&self, query: &EngineQuery, cut: Cut) -> Result<(), Refusal> {
        let state = self.gate.read();
        let now_wall = self.gate.clock.wall_ms();
        let now_mono = self.gate.clock.mono_ms();
        if let Some(times) = query.capability {
            capability::check(
                times,
                cut,
                state.engine.clock_hold,
                now_wall,
                &self.settings,
            )?;
        }
        let entry = state.listed_entry(query.resource_id, now_wall, now_mono)?;
        pin::check(entry, &state.engine, query, now_mono, &self.settings)
    }

    /// Whether the clock hold is in force.
    pub fn clock_hold(&self) -> bool {
        self.gate.read().engine.clock_hold
    }

    /// Whether `pin_id` is in the pin history under a resource other than
    /// `resource_id` (an observation tool reports it as previously listed).
    pub fn pin_previously_listed(&self, pin_id: &str, resource_id: &str) -> bool {
        pin::previously_listed(&self.gate.read().engine, pin_id, resource_id)
    }

    pub fn walk_inputs(&self, resource_id: &str) -> WalkInputs {
        let state = self.gate.read();
        pin::walk_inputs(&state.entries, &state.engine, resource_id)
    }
}
