//! Profile S: the search-engine holder (`kind: engine`).
//!
//! Normative source: Part II of the authority workspace file
//! `.workspace/kwiry-integration/HOLDINGS-WIRE-v2.md` (sha256
//! `6b6d2e27d48b6d110a11477aebf17120fa5b190b1b1dea51016865d6db49f12c`).
//! One module per section that binds a holder:
//!
//! | Module | v2 section | What it adds to the core |
//! |---|---|---|
//! | [`access`] | S.1 (H.15, H.21) | the request-time access check: capability lifetime, future `iat`, expiry plus skew at both cuts |
//! | [`root_pin`] | S.3 (H.16, H.24, H.28) | the `root_pin` member, admit refusals for changed or null pins, artifact and root checks, walk inputs |
//! | [`clock_hold`] | S.4 (H.21.5, H.23, H.26, H.29, H.34) | the durable clock hold |
//! | [`stale_serve`] | S.5 | the bounded stale-serve window |
//! | [`pin_history`] | S.6 | the refusal-only `(pin_id, resource.id)` set |
//!
//! The S.2 mint gate is authority-side and needs no holder code.
//!
//! The generic core (Part I) never depends on these rules. A holder whose
//! wire profile has no engine section never reaches this module: every
//! class-defined member is refused (§C.3), and the engine's durable section
//! of the holder state stays empty.

pub mod access;
pub mod clock_hold;
pub mod pin_history;
pub mod root_pin;
pub mod stale_serve;

pub use access::{CapabilityTimes, Cut};
pub use pin_history::PinPair;
pub use root_pin::{ArtifactStamp, PinMember, RootObservation, RootPin, WalkInputs};

use crate::gate::{Refusal, ServingGate};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

/// Profile S class parameters a holder needs (v2 §C.1, S.0).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineProfile {
    /// The holder kind the profile binds (`holder.kind`).
    pub holder_kind: String,
    /// The resource kind whose items carry the class-defined pin member.
    pub pinned_resource_kind: String,
}

impl EngineProfile {
    pub fn pins(&self, holder_kind: &str, resource_kind: &str) -> bool {
        self.holder_kind == holder_kind && self.pinned_resource_kind == resource_kind
    }
}

/// Operator settings of Profile S.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineSettings {
    pub clock_skew_allowance_ms: i64,
    pub max_stale_serve_ms: u64,
}

impl EngineSettings {
    /// Refuses a skew allowance above 30 s (H.21.3: the engine refuses to
    /// start) and a stale-serve window above 86,400 s (S.5).
    pub fn new(clock_skew_allowance_s: u32, max_stale_serve_s: u32) -> Result<Self, &'static str> {
        if clock_skew_allowance_s > access::MAX_CLOCK_SKEW_ALLOWANCE_S {
            return Err("clock_skew_allowance above 30 s");
        }
        if max_stale_serve_s > stale_serve::MAX_STALE_SERVE_S {
            return Err("max_stale_serve_seconds above 86400");
        }
        Ok(Self {
            clock_skew_allowance_ms: i64::from(clock_skew_allowance_s) * 1000,
            max_stale_serve_ms: u64::from(max_stale_serve_s) * 1000,
        })
    }
}

/// Profile S durable fields. They live in the holder-state file and change
/// only through its single writer; every field only ever refuses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineState {
    /// S.6: pairs from every applied list; never replaced or evicted.
    pub pin_history: BTreeSet<PinPair>,
    pub pin_history_ceiling: u64,
    /// S.4: the clock hold.
    pub clock_behind_authority: bool,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            pin_history: BTreeSet::new(),
            pin_history_ceiling: pin_history::DEFAULT_CEILING,
            clock_behind_authority: false,
        }
    }
}

/// Profile S fields of the in-memory gate state.
#[derive(Default)]
pub(crate) struct EngineGateState {
    pub clock_hold: bool,
    /// `pin_id → resource ids`, from the durable pin-history set.
    pub pin_history: HashMap<String, BTreeSet<String>>,
    pub history_available: bool,
}

/// Profile S refusals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineRefusal {
    ClockHold,
    CapabilityLifetime,
    CapabilityNotYetValid,
    CapabilityExpired,
    RootPinMissing,
    RootPinPreviouslyListed,
    ArtifactPinRef,
    RootPinMismatch,
}

impl EngineRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::CapabilityExpired => "capability_expired",
            Self::CapabilityLifetime | Self::CapabilityNotYetValid => "capability_invalid",
            Self::ClockHold => "clock_behind_authority",
            _ => "resource_unavailable",
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Self::ClockHold => "clock_behind_authority",
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

impl From<EngineRefusal> for Refusal {
    fn from(refusal: EngineRefusal) -> Self {
        Refusal::Engine(refusal)
    }
}

/// Profile S alerts. Codes and counts only; never an id, pin or path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineEvent {
    /// S.6: the set was initialised fresh; walk exclusions are weaker until
    /// lists refill it. Once per start.
    PinHistoryUnavailable,
    /// S.6: the ceiling would be exceeded; it was raised, never evicted.
    PinHistoryCeilingRaised { from: u64, to: u64 },
    /// S.4: a signed page or capability proved a lag of more than 30 s.
    ClockHoldRaised { lag_lower_bound_ms: i64 },
    /// S.4: a nonce-bound page proved the lag is at most 30 s.
    ClockHoldCleared {
        measured_lag_upper_bound_ms: i64,
        rtt_ms: u64,
    },
    /// S.4: a nonce-bound page did not confirm a correction.
    ClockHoldUnconfirmed {
        measured_lag_upper_bound_ms: i64,
        rtt_ms: u64,
    },
    /// S.4: writing the hold failed; it stays raised in memory.
    ClockHoldWriteFailed,
}

impl EngineEvent {
    pub fn code(&self) -> &'static str {
        match self {
            Self::PinHistoryUnavailable => "pin_history_unavailable",
            Self::PinHistoryCeilingRaised { .. } => "pin_history_ceiling_raised",
            Self::ClockHoldRaised { .. } => "clock_behind_authority",
            Self::ClockHoldCleared { .. } => "clock_behind_authority_cleared",
            Self::ClockHoldUnconfirmed { .. } => "clock_behind_authority_unconfirmed",
            Self::ClockHoldWriteFailed => "clock_hold_write_failed",
        }
    }
}

/// What an engine asks before it serves, reports ready or releases results.
#[derive(Clone, Copy, Debug)]
pub struct EngineQuery<'a> {
    pub resource_id: &'a str,
    /// The capability's times, for capability-authorized requests.
    pub capability: Option<CapabilityTimes>,
    /// The artifact about to be used, for pinned resources.
    pub artifact: Option<ArtifactStamp<'a>>,
    pub root: RootObservation,
}

/// The core serving gate with Profile S on top.
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

    /// The request-time access check (v2 §C.0; S.1), every core rule
    /// (§3.4), then the root-pin rules (S.3, S.5, S.6) for pinned resources.
    pub fn check(&self, query: &EngineQuery, cut: Cut) -> Result<(), Refusal> {
        let state = self.gate.read();
        let now_wall = self.gate.clock.wall_ms();
        let now_mono = self.gate.clock.mono_ms();
        if let Some(times) = query.capability {
            access::check(
                times,
                cut,
                state.engine.clock_hold,
                now_wall,
                &self.settings,
            )?;
        }
        let entry = state.listed_entry(query.resource_id, now_wall, now_mono)?;
        root_pin::check(entry, &state.engine, query, now_mono, &self.settings)?;
        Ok(())
    }

    /// Whether the clock hold is in force (S.4).
    pub fn clock_hold(&self) -> bool {
        self.gate.read().engine.clock_hold
    }

    /// Whether `pin_id` is in the pin-history set under a resource other
    /// than `resource_id` (S.6).
    pub fn pin_previously_listed(&self, pin_id: &str, resource_id: &str) -> bool {
        pin_history::previously_listed(&self.gate.read().engine, pin_id, resource_id)
    }

    /// Exclusions a walk of `resource_id` must honour (S.3, S.6).
    pub fn walk_inputs(&self, resource_id: &str) -> WalkInputs {
        let state = self.gate.read();
        root_pin::walk_inputs(&state.entries, &state.engine, resource_id)
    }
}
