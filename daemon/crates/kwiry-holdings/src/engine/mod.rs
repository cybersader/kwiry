//! Profile S: the search-engine holder (`kind: engine`).
//!
//! Normative source: Part II of the authority workspace file
//! `.workspace/kwiry-integration/HOLDINGS-WIRE-v2.md` (sha256
//! `066a21ae48af6a8712bb0dddcc3da1a7c460bc6b2b6c31959d5bf3f69178d042`).
//! One module per section that binds a holder:
//!
//! | Module | v2 section | What it adds to the core |
//! |---|---|---|
//! | [`access`] | S.1 (H.15, H.21) | the request-time access check: capability lifetime, future `iat`, expiry plus skew at both cuts |
//! | [`root_pin`] | S.3 (H.16, H.24, H.28, H.38) | the `root_pin` member, admit refusals for changed, null or conflicting pins, artifact and root checks, walk inputs |
//! | [`clock_hold`] | S.4 (H.21.5, H.23, H.26, H.29, H.34, H.36) | the durable clock hold |
//! | [`stale_serve`] | S.5 | the bounded stale-serve window and its walk latch |
//! | [`pin_history`] | S.6 | the refusal-only `(pin_id, resource.id)` set |
//!
//! The S.2 mint gate is authority-side and needs no holder code.
//!
//! The generic core (Part I) never depends on these rules running. A holder
//! whose wire profile has no engine section never reaches this module:
//! every class-defined member is refused (§C.3), and the engine's durable
//! section of the holder state stays empty.

pub mod access;
pub mod clock_hold;
pub mod pin_history;
pub mod root_pin;
pub mod stale_serve;

pub use access::{CapabilityTimes, CapabilityVerifier, Cut};
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

/// Operator settings of Profile S. The fields are private: the only way to
/// build one is [`EngineSettings::new`], which enforces every bound.
///
/// ```compile_fail
/// let settings = kwiry_holdings::engine::EngineSettings {
///     clock_skew_allowance_ms: 120_000,
///     max_stale_serve_ms: 0,
/// };
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineSettings {
    clock_skew_allowance_ms: i64,
    max_stale_serve_ms: u64,
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

    pub fn clock_skew_allowance_ms(&self) -> i64 {
        self.clock_skew_allowance_ms
    }

    pub fn max_stale_serve_ms(&self) -> u64 {
        self.max_stale_serve_ms
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
    /// S.4, H.34: the token of the last clear. A clear counts on load only
    /// when the seal file holds the same token; the seal is written only
    /// after this record's write was fully fsynced.
    pub clock_clear_token: Option<String>,
}

impl EngineState {
    /// Whether the hold is in force, given the seal file's content. Anything
    /// but a cleared record with a matching seal reads as raised.
    pub fn clock_hold(&self, seal: Option<&str>) -> bool {
        self.clock_behind_authority
            || self
                .clock_clear_token
                .as_deref()
                .is_some_and(|token| seal != Some(token))
    }
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            pin_history: BTreeSet::new(),
            pin_history_ceiling: pin_history::DEFAULT_CEILING,
            clock_behind_authority: false,
            clock_clear_token: None,
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
    /// S.5: per `(resource, pin_ref)`, the monotonic instant at which the
    /// root observation stopped matching. Cleared only by a successful walk
    /// under that same, current `pin_ref`.
    pub root_mismatch: HashMap<(String, String), u64>,
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
    /// The engine's latest root observation. A mismatch is also latched in
    /// the gate (see [`EngineGate::report_root_mismatch`]).
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
        let (verdict, latch) = {
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
            root_pin::check(entry, &state.engine, query, now_mono, &self.settings)
        };
        if let Some((pin_ref, detected)) = latch {
            self.latch_mismatch(query.resource_id, pin_ref, detected);
        }
        verdict.map_err(Refusal::from)
    }

    /// Latches a mismatch for `(resource_id, pin_ref)` only if `pin_ref` is
    /// the resource's current listed pin; a report about any other pin, for
    /// example a delayed report from an earlier walk, is ignored. The
    /// earliest detection stands.
    fn latch_mismatch(&self, resource_id: &str, pin_ref: String, detected_mono_ms: u64) {
        let mut state = self.gate.write();
        if !root_pin::is_current_pin(&state.entries, resource_id, &pin_ref) {
            return;
        }
        let since = state
            .engine
            .root_mismatch
            .entry((resource_id.to_owned(), pin_ref))
            .or_insert(detected_mono_ms);
        *since = (*since).min(detected_mono_ms);
    }

    /// S.5: the engine's walk found the root of `resource_id` no longer at
    /// the listed `pin_id` under `pin_ref`. Ignored unless `pin_ref` is current.
    pub fn report_root_mismatch(&self, resource_id: &str, pin_ref: &str, detected_mono_ms: u64) {
        self.latch_mismatch(resource_id, pin_ref.to_owned(), detected_mono_ms);
    }

    /// S.5: a walk under `pin_ref` observed the root at the listed `pin_id`
    /// and published. Only this ends a latched mismatch, and only for the
    /// current pin.
    pub fn record_successful_walk(&self, resource_id: &str, pin_ref: &str) {
        let mut state = self.gate.write();
        if root_pin::is_current_pin(&state.entries, resource_id, pin_ref) {
            state
                .engine
                .root_mismatch
                .remove(&(resource_id.to_owned(), pin_ref.to_owned()));
        }
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
