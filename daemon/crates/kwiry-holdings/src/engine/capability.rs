//! Capability-exposure bounds and the clock hold (engine profile).
//!
//! A capability that names a resource retired after it was minted may still
//! be presented for a short time. The engine keeps that window bounded:
//! - it refuses a lifetime above 60 s and an `iat` more than 30 s ahead;
//! - it answers only while `now ≤ exp + skew`, at admission and again just
//!   before results are released, with the skew allowance at most 30 s;
//! - none of that holds in real time if the engine clock lags the authority
//!   by more than 30 s, so any signed page or capability proving such a lag
//!   raises a hold that refuses every capability-authorized request.
//!
//! The hold is raised in memory first and then written; it is cleared by
//! writing first and then dropping the in-memory flag. Only a page that
//! answers this process's own nonce-bound request and satisfies
//! `(iat − now_recv) + rtt ≤ 30 s` clears it; a failed clearing write keeps
//! it raised, and the next confirmed correction tries again.

use super::EngineSettings;
use crate::client::{ClockEvidence, FetchFailure, Transport};
use crate::events::HolderEvent;
use crate::gate::{Refusal, ServingGate};
use crate::holder::{Holder, RouteHealth};
use crate::state::{StateWriter, WriteError};
use crate::store::ResourceStore;

pub const MAX_CAPABILITY_LIFETIME_S: i64 = 60;
pub const MAX_FUTURE_IAT_S: i64 = 30;
pub const MAX_CLOCK_SKEW_ALLOWANCE_S: u32 = 30;
/// Lag beyond which the holder clock is held to be behind the authority.
pub const CLOCK_BEHIND_MS: i64 = 30_000;

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

pub(crate) fn check(
    times: CapabilityTimes,
    cut: Cut,
    clock_hold: bool,
    now_wall: i64,
    settings: &EngineSettings,
) -> Result<(), Refusal> {
    if clock_hold {
        return Err(Refusal::ClockBehindAuthority);
    }
    if cut == Cut::Admission {
        if times.exp_s.saturating_sub(times.iat_s) > MAX_CAPABILITY_LIFETIME_S {
            return Err(Refusal::CapabilityLifetime);
        }
        if times.iat_s.saturating_mul(1000) > now_wall.saturating_add(MAX_FUTURE_IAT_S * 1000) {
            return Err(Refusal::CapabilityNotYetValid);
        }
    }
    let deadline = times
        .exp_s
        .saturating_mul(1000)
        .saturating_add(settings.clock_skew_allowance_ms);
    if now_wall > deadline {
        return Err(Refusal::CapabilityExpired);
    }
    Ok(())
}

/// Called for each page while a list is still being fetched, so that a
/// proven lag refuses in memory at once.
pub(crate) fn raise_on_signed_page(gate: &ServingGate, evidence: &ClockEvidence) {
    if let ClockEvidence::Signed { lag_ms } = evidence
        && *lag_ms > CLOCK_BEHIND_MS
    {
        gate.raise_clock_hold();
    }
}

impl ServingGate {
    /// Raises the hold in memory. Returns false if it was already up.
    pub(crate) fn raise_clock_hold(&self) -> bool {
        !std::mem::replace(&mut self.write().engine.clock_hold, true)
    }

    /// Drops the in-memory hold; called only after the durable clear.
    pub(crate) fn drop_clock_hold(&self) {
        self.write().engine.clock_hold = false;
    }
}

impl StateWriter {
    /// Persists a raised hold. The caller has already refused in memory.
    pub fn raise_clock_hold(&mut self) -> Result<(), WriteError> {
        if self.state().engine.clock_behind_authority {
            return Ok(());
        }
        let mut next = self.state().clone();
        next.engine.clock_behind_authority = true;
        self.commit(next)
    }

    /// Persists a cleared hold. The caller drops the in-memory hold only
    /// after this returns `Ok`.
    pub fn clear_clock_hold(&mut self) -> Result<(), WriteError> {
        if !self.state().engine.clock_behind_authority {
            return Ok(());
        }
        let mut next = self.state().clone();
        next.engine.clock_behind_authority = false;
        self.commit(next)
    }
}

impl<S: ResourceStore, T: Transport> Holder<S, T> {
    /// Clock evidence from a verified capability. No-op without the engine
    /// profile.
    pub fn observe_capability_iat(&mut self, iat_s: i64) {
        if self.engine.is_none() {
            return;
        }
        let lag = iat_s.saturating_mul(1000) - self.clock.wall_ms();
        if lag > CLOCK_BEHIND_MS {
            self.raise_clock_hold(lag);
        }
    }

    /// One clock probe while the hold is raised: at most once per 60 s,
    /// counted as a list start, never applied. `None` when no probe is
    /// needed or allowed now.
    pub fn probe_clock(&mut self) -> Option<Result<(), FetchFailure>> {
        let now = self.clock.mono_ms();
        if self.engine.is_none()
            || self.route != RouteHealth::Normal
            || !self.gate.clock_hold_raised()
            || !self.scheduler.probe_due(now)
        {
            return None;
        }
        self.scheduler.on_probe(now);
        Some(self.fetch(true).map(|_| ()).inspect_err(|failure| {
            self.on_failure(failure);
        }))
    }

    fn raise_clock_hold(&mut self, lag_lower_bound_ms: i64) {
        self.gate.raise_clock_hold();
        self.events
            .emit(HolderEvent::ClockBehindRaised { lag_lower_bound_ms });
        if self.writer.raise_clock_hold().is_err() {
            self.events.emit(HolderEvent::ClockHoldWriteFailed);
        }
    }

    pub(crate) fn on_clock_evidence(&mut self, observed: ClockEvidence) {
        match observed {
            ClockEvidence::Signed { lag_ms } if lag_ms > CLOCK_BEHIND_MS => {
                self.raise_clock_hold(lag_ms);
            }
            ClockEvidence::Signed { .. } => {}
            ClockEvidence::Answered { lag_ms, rtt_ms } if self.gate.clock_hold_raised() => {
                let upper = lag_ms.saturating_add(i64::try_from(rtt_ms).unwrap_or(i64::MAX));
                if upper > CLOCK_BEHIND_MS {
                    self.events.emit(HolderEvent::ClockBehindUnconfirmed {
                        measured_lag_upper_bound_ms: upper,
                        rtt_ms,
                    });
                } else if self.writer.clear_clock_hold().is_ok() {
                    self.gate.drop_clock_hold();
                    self.events.emit(HolderEvent::ClockBehindCleared {
                        measured_lag_upper_bound_ms: upper,
                        rtt_ms,
                    });
                } else {
                    self.events.emit(HolderEvent::ClockHoldWriteFailed);
                }
            }
            ClockEvidence::Answered { .. } => {}
        }
    }
}
