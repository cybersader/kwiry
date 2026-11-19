//! S.1: the request-time access check for search capabilities (v2 H.15,
//! H.21).
//!
//! A capability minted before a retirement may still be presented for a
//! short time; the engine keeps that window at most 90 s after the
//! retirement commit (§C.2). Mechanically (H.21):
//! - refuse a lifetime `exp − iat` above 60 s, and an `iat` more than 30 s
//!   ahead of the engine clock (at admission);
//! - answer only while `now ≤ exp + clock_skew_allowance`, with the allowance
//!   at most 30 s, at admission and again immediately before release;
//! - refuse every capability-authorized request while the clock hold is
//!   raised (S.4, H.21.5);
//! - refuse every resource an admitted list showed not active (the core's
//!   admit refusal).

use super::{EngineRefusal, EngineSettings};

pub const MAX_CAPABILITY_LIFETIME_S: i64 = 60;
pub const MAX_FUTURE_IAT_S: i64 = 30;
pub const MAX_CLOCK_SKEW_ALLOWANCE_S: u32 = 30;

/// Where the check runs (v2 §C.0 "request-time access check").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    /// At admission of the request.
    Admission,
    /// Immediately before results are released.
    Release,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapabilityTimes {
    pub iat_s: i64,
    pub exp_s: i64,
}

/// The engine's capability verifier. It checks a presented capability's
/// signature and `typ` and returns its times. Only a capability that passes
/// it can raise the clock hold (v2 H.36): the holder takes clock evidence
/// from its output, never from a bare timestamp.
pub trait CapabilityVerifier {
    type Rejection;
    fn verify(&self, capability: &[u8]) -> Result<CapabilityTimes, Self::Rejection>;
}

pub(crate) fn check(
    times: CapabilityTimes,
    cut: Cut,
    clock_hold: bool,
    now_wall: i64,
    settings: &EngineSettings,
) -> Result<(), EngineRefusal> {
    if clock_hold {
        return Err(EngineRefusal::ClockHold);
    }
    if cut == Cut::Admission {
        if times.exp_s.saturating_sub(times.iat_s) > MAX_CAPABILITY_LIFETIME_S {
            return Err(EngineRefusal::CapabilityLifetime);
        }
        if times.iat_s.saturating_mul(1000) > now_wall.saturating_add(MAX_FUTURE_IAT_S * 1000) {
            return Err(EngineRefusal::CapabilityNotYetValid);
        }
    }
    let deadline = times
        .exp_s
        .saturating_mul(1000)
        .saturating_add(settings.clock_skew_allowance_ms());
    if now_wall > deadline {
        return Err(EngineRefusal::CapabilityExpired);
    }
    Ok(())
}
