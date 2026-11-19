//! S.4: the clock hold (v2 Part II, H.21.5, H.23, H.26, H.29, H.34).
//!
//! The S.1 bounds hold in real time only while the engine clock is at most
//! 30 s behind the authority clock. Any signed page or capability with
//! `iat − now_recv > 30 s` proves a larger lag and raises the hold, which
//! fails the request-time access check for every capability-authorized
//! request. Lists, applies, reports and status continue.
//!
//! - **Raise (H.26, H.34):** in memory at once, then written through the
//!   single holder-state writer. A crash before the write loses only the
//!   flag; nothing serves until a list of the next process applies.
//! - **Clear (H.23, H.29):** only a page that answers this process's own
//!   nonce-bound request with `(iat − now_recv) + rtt ≤ 30 s`, whatever the
//!   round trip. Written first, then the in-memory hold drops (H.34); a
//!   failed clearing write keeps it raised in memory and durably, and the
//!   next confirmed correction retries. Nothing else clears it: no
//!   capability, elapsed time, restart or operator command.
//! - **Probe (H.23):** while raised, at most one extra `page_size: 1` list
//!   request per 60 s, counted under H.8 and never applied.

use super::EngineEvent;
use crate::client::{ClockEvidence, FetchFailure, Transport};
use crate::events::HolderEvent;
use crate::gate::ServingGate;
use crate::holder::{Holder, RouteHealth};
use crate::state::{StateWriter, WriteError};
use crate::store::ResourceStore;

/// A page or capability proving a lag above this raises the hold.
pub const MAX_LAG_MS: i64 = 30_000;

/// Called for each page while a list is still being fetched, so a proven
/// lag refuses in memory at once.
pub(crate) fn raise_on_signed_page(gate: &ServingGate, evidence: &ClockEvidence) {
    if let ClockEvidence::Signed { lag_ms } = evidence
        && *lag_ms > MAX_LAG_MS
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
    /// Clock evidence from a verified capability. No-op without Profile S.
    pub fn observe_capability_iat(&mut self, iat_s: i64) {
        if self.engine.is_none() {
            return;
        }
        let lag = iat_s.saturating_mul(1000) - self.clock.wall_ms();
        if lag > MAX_LAG_MS {
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
        self.emit_engine(EngineEvent::ClockHoldRaised { lag_lower_bound_ms });
        if let Err(error) = self.writer.raise_clock_hold() {
            self.write_failed(&error);
            self.emit_engine(EngineEvent::ClockHoldWriteFailed);
        }
    }

    fn emit_engine(&self, event: EngineEvent) {
        self.events.emit(HolderEvent::Engine(event));
    }

    pub(crate) fn on_clock_evidence(&mut self, observed: ClockEvidence) {
        match observed {
            ClockEvidence::Signed { lag_ms } if lag_ms > MAX_LAG_MS => {
                self.raise_clock_hold(lag_ms);
            }
            ClockEvidence::Signed { .. } => {}
            ClockEvidence::Answered { lag_ms, rtt_ms } if self.gate.clock_hold_raised() => {
                let upper = lag_ms.saturating_add(i64::try_from(rtt_ms).unwrap_or(i64::MAX));
                if upper > MAX_LAG_MS {
                    self.emit_engine(EngineEvent::ClockHoldUnconfirmed {
                        measured_lag_upper_bound_ms: upper,
                        rtt_ms,
                    });
                    return;
                }
                match self.writer.clear_clock_hold() {
                    Ok(()) => {
                        self.gate.drop_clock_hold();
                        self.emit_engine(EngineEvent::ClockHoldCleared {
                            measured_lag_upper_bound_ms: upper,
                            rtt_ms,
                        });
                    }
                    Err(error) => {
                        self.write_failed(&error);
                        self.emit_engine(EngineEvent::ClockHoldWriteFailed);
                    }
                }
            }
            ClockEvidence::Answered { .. } => {}
        }
    }
}
