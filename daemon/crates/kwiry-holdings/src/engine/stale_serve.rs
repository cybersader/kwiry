//! S.5: the stale-serve window (v2 Part II, S.5).
//!
//! While the listed `pin_ref` is unchanged and the item is still active and
//! attested, a resource whose root observation stopped matching may keep
//! serving artifacts that a walk validated under that same `pin_ref`, for at
//! most `max_stale_serve_seconds` from detection (default 3600, at most
//! 86,400). The window is never available after an admit refusal (the core
//! refuses first), never for a pin held under another resource (S.6), and
//! never for an artifact stamped with another `pin_ref` (H.28). Once a
//! mismatch is seen, only a successful walk under the current `pin_ref` ends
//! it: a root that matches again later does not, by itself, resume service.

use super::EngineSettings;
use super::root_pin::{ArtifactStamp, RootObservation};

pub const DEFAULT_STALE_SERVE_S: u32 = 3_600;
pub const MAX_STALE_SERVE_S: u32 = 86_400;

/// Whether `observation` lets `artifact` (already known to carry the listed
/// `pin_ref`) serve now.
pub(crate) fn permits(
    observation: RootObservation,
    artifact: ArtifactStamp,
    now_mono: u64,
    settings: &EngineSettings,
) -> bool {
    match observation {
        RootObservation::Matches => true,
        RootObservation::Mismatch { detected_mono_ms } => {
            artifact.validated_under_matching_root
                && now_mono.saturating_sub(detected_mono_ms) <= settings.max_stale_serve_ms()
        }
        RootObservation::Unobserved => false,
    }
}
