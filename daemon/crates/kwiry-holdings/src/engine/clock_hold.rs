//! S.4: the clock hold (v2 Part II, H.21.5, H.23, H.26, H.29, H.34, H.36).
//!
//! The S.1 bounds hold in real time only while the engine clock is at most
//! 30 s behind the authority clock. Any authority-signed page whose signature
//! and `iss` verified, or any verified capability, with `iat − now_recv > 30 s`
//! proves a larger lag and raises the hold, even if that page then fails its
//! own later checks (H.36). The hold fails the request-time access check for
//! every capability-authorized request; lists, applies, reports and status
//! continue.
//!
//! - **One pass, in order.** Each piece of evidence is applied exactly once,
//!   when it is observed, in observation order. Earlier evidence can never be
//!   replayed after a later raise.
//! - **Raise (H.26, H.34):** in memory at once, then written through the
//!   single holder-state writer.
//! - **Clear (H.23, H.29, H.34):** only a page that answers this process's
//!   own nonce-bound request with `(iat − now_recv) + rtt ≤ 30 s`. The clear
//!   counts only once fully durable: it is written first with an
//!   "unconfirmed" marker that a later load reads as raised, then confirmed;
//!   the in-memory hold drops only after both. Any failure keeps the hold.
//! - **Probe (H.23):** while raised, at most one extra `page_size: 1` list
//!   request per 60 s, counted under H.8 and never applied.

use super::EngineEvent;
use crate::client::{ClockEvidence, FetchFailure, Transport};
use crate::events::{EventSink, HolderEvent};
use crate::gate::ServingGate;
use crate::holder::{Holder, RouteHealth, report_write_failure};
use crate::state::{StateWriter, WriteError};
use crate::store::ResourceStore;

/// A page or capability proving a lag above this raises the hold.
pub const MAX_LAG_MS: i64 = 30_000;

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
        let engine = &self.state().engine;
        if engine.clock_behind_authority && !engine.clock_clear_unconfirmed {
            return Ok(());
        }
        let mut next = self.state().clone();
        next.engine.clock_behind_authority = true;
        next.engine.clock_clear_unconfirmed = false;
        self.commit(next)
    }

    /// Persists a cleared hold in two durable steps: first the clear with
    /// the "unconfirmed" marker (a load still reads it as raised), then the
    /// confirmation. Returns `Ok` only when both are durable; the caller drops
    /// the in-memory hold only then.
    pub fn clear_clock_hold(&mut self) -> Result<(), WriteError> {
        if !self.state().engine.clock_hold() {
            return Ok(());
        }
        let mut unconfirmed = self.state().clone();
        unconfirmed.engine.clock_behind_authority = false;
        unconfirmed.engine.clock_clear_unconfirmed = true;
        self.commit(unconfirmed)?;
        let mut confirmed = self.state().clone();
        confirmed.engine.clock_clear_unconfirmed = false;
        self.commit(confirmed)
    }
}

fn emit(events: &dyn EventSink, event: EngineEvent) {
    events.emit(HolderEvent::Engine(event));
}

/// Raises the hold: in memory first, then durably.
pub(crate) fn raise(
    writer: &mut StateWriter,
    gate: &ServingGate,
    events: &dyn EventSink,
    lag_lower_bound_ms: i64,
) {
    gate.raise_clock_hold();
    emit(events, EngineEvent::ClockHoldRaised { lag_lower_bound_ms });
    if let Err(error) = writer.raise_clock_hold() {
        report_write_failure(events, &error);
        emit(events, EngineEvent::ClockHoldWriteFailed);
    }
}

/// Applies one piece of clock evidence, at the moment it is observed.
pub(crate) fn observe(
    writer: &mut StateWriter,
    gate: &ServingGate,
    events: &dyn EventSink,
    evidence: ClockEvidence,
) {
    match evidence {
        ClockEvidence::Signed { lag_ms } if lag_ms > MAX_LAG_MS => {
            raise(writer, gate, events, lag_ms);
        }
        ClockEvidence::Signed { .. } => {}
        ClockEvidence::Answered { lag_ms, rtt_ms } if gate.clock_hold_raised() => {
            let upper = lag_ms.saturating_add(i64::try_from(rtt_ms).unwrap_or(i64::MAX));
            if upper > MAX_LAG_MS {
                emit(
                    events,
                    EngineEvent::ClockHoldUnconfirmed {
                        measured_lag_upper_bound_ms: upper,
                        rtt_ms,
                    },
                );
                return;
            }
            match writer.clear_clock_hold() {
                Ok(()) => {
                    gate.drop_clock_hold();
                    emit(
                        events,
                        EngineEvent::ClockHoldCleared {
                            measured_lag_upper_bound_ms: upper,
                            rtt_ms,
                        },
                    );
                }
                Err(error) => {
                    report_write_failure(events, &error);
                    emit(events, EngineEvent::ClockHoldWriteFailed);
                }
            }
        }
        ClockEvidence::Answered { .. } => {}
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
            raise(&mut self.writer, &self.gate, self.events.as_ref(), lag);
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
}
