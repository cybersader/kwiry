//! S.3: authority-held root pins (v2 Part II, H.16, H.24, H.28).
//!
//! For items of the profile's pinned resource kind the authority delivers
//! `root_pin: {pin_id, pin_ref} | null`, the class-defined member of Profile S (§C.3, H.16.4).
//! `pin_id` is the engine's own observation of the storage root; `pin_ref`
//! is the authority's attestation id. The engine serves a pinned resource
//! only (H.16.5):
//! - when the pin is non-null and its `pin_id` is not held under another
//!   resource in the pin-history set (S.6);
//! - from artifacts whose durable derivation record carries the listed
//!   `pin_ref` (H.24, H.28);
//! - while it observes the root at `pin_id`, or inside the stale-serve
//!   window (S.5);
//! - from a list received in its current process lifetime (the core's
//!   no-list-this-process rule).
//!
//! At admit, a null pin, a pin changed since the list last applied in this
//! process (H.24: empty at process start), or a pin held under another
//! resource is refused at once; only a successful apply broadens serving.

use super::{
    EngineGateState, EngineQuery, EngineRefusal, EngineSettings, pin_history, stale_serve,
};
use crate::gate::{GateEntry, Refusal};
use crate::grammar;
use crate::verify::{ListedItem, PageError};
use crate::wire::Item;
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RootPin {
    pub pin_id: String,
    pub pin_ref: String,
}

/// The class-defined pin member of a list item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinMember {
    /// The holder's profile defines no pin for this item (member absent).
    NotApplicable,
    /// The member is `null`: not attested, never served.
    Unpinned,
    Pinned(RootPin),
}

impl PinMember {
    pub fn pin(&self) -> Option<&RootPin> {
        match self {
            Self::Pinned(pin) => Some(pin),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootObservation {
    /// The engine observed the storage root at the listed `pin_id`.
    Matches,
    /// The observation stopped matching at this monotonic instant.
    Mismatch { detected_mono_ms: u64 },
    /// Not observed, or the observation failed.
    Unobserved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtifactStamp<'a> {
    /// `pin_ref` recorded in the artifact's durable derivation record.
    pub pin_ref: &'a str,
    /// Published by a walk whose root observation matched the listed
    /// `pin_id` under this same `pin_ref` (S.5 "previously validated").
    pub validated_under_matching_root: bool,
}

/// Inputs an engine walk needs to keep other resources' storage out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WalkInputs {
    /// Current pins of other active items: meeting one is a nesting defect.
    pub nesting_pins: BTreeSet<String>,
    /// Pins of terminating or retired items, and pins held in the history
    /// under another resource: skipped and counted against coverage.
    pub skip_pins: BTreeSet<String>,
    /// False after trust on first use, until lists refill the history.
    pub history_available: bool,
}

/// Parses the member under the §C.3 presence rule: present exactly when
/// `pinned`, refused anywhere else (H.16.4 must-reject shapes).
pub fn parse_member(item: &Map<String, Value>, pinned: bool) -> Result<PinMember, PageError> {
    match (pinned, item.get("root_pin")) {
        (false, None) => Ok(PinMember::NotApplicable),
        (false, Some(_)) => Err(PageError::RootPin("member on an unpinned item")),
        (true, None) => Err(PageError::RootPin("member missing")),
        (true, Some(Value::Null)) => Ok(PinMember::Unpinned),
        (true, Some(Value::Object(pin))) => {
            if pin.keys().any(|k| k != "pin_id" && k != "pin_ref") {
                return Err(PageError::RootPin("unknown member"));
            }
            let pin_id = pin.get("pin_id").and_then(Value::as_str);
            let pin_ref = pin.get("pin_ref").and_then(Value::as_str);
            let (Some(pin_id), Some(pin_ref)) = (pin_id, pin_ref) else {
                return Err(PageError::RootPin(
                    "pin_id and pin_ref must both be present",
                ));
            };
            if !grammar::pin_id(pin_id) {
                return Err(PageError::RootPin("pin_id"));
            }
            if !grammar::pin_ref(pin_ref) {
                return Err(PageError::RootPin("pin_ref"));
            }
            Ok(PinMember::Pinned(RootPin {
                pin_id: pin_id.to_owned(),
                pin_ref: pin_ref.to_owned(),
            }))
        }
        (true, Some(_)) => Err(PageError::RootPin("not an object")),
    }
}

/// The pins of the list being applied, by holding id: the next
/// process-local baseline for "changed" (H.24).
pub(crate) fn baseline(items: &[ListedItem]) -> HashMap<String, Option<RootPin>> {
    items
        .iter()
        .filter(|listed| listed.item.root_pin != PinMember::NotApplicable)
        .map(|listed| {
            (
                listed.item.holding_id.clone(),
                listed.item.root_pin.pin().cloned(),
            )
        })
        .collect()
}

/// Admit refusal for an active, untombstoned item (H.16.5 effective point,
/// H.38): a null pin; a `pin_id` the history, including this list's own
/// pairs from every phase, holds under another resource
/// (`root_pin_previously_listed`, per item, never whole-list); or a pin
/// changed against the baseline (H.24).
pub(crate) fn refused_at_admit(
    item: &Item,
    baseline: &HashMap<String, Option<RootPin>>,
    history: &HashMap<String, BTreeSet<String>>,
) -> Option<Refusal> {
    match &item.root_pin {
        PinMember::NotApplicable => None,
        PinMember::Unpinned => Some(EngineRefusal::RootPinMissing.into()),
        PinMember::Pinned(pin) => {
            if pin_history::conflicts(history, &pin.pin_id, &item.resource.id) {
                return Some(EngineRefusal::RootPinPreviouslyListed.into());
            }
            let unchanged = baseline.get(&item.holding_id) == Some(&Some(pin.clone()));
            (!unchanged).then_some(Refusal::RefusedAtAdmit)
        }
    }
}

/// A mismatch the gate should latch: the `pin_ref` and detection instant.
pub(crate) type Latch = Option<(String, u64)>;

/// The pin rules for one entry that passed every core rule. Returns the
/// verdict, and a mismatch to latch when the query reports one first.
pub(crate) fn check(
    entry: &GateEntry,
    state: &EngineGateState,
    query: &EngineQuery,
    now_mono: u64,
    settings: &EngineSettings,
) -> (Result<(), EngineRefusal>, Latch) {
    let pin = match &entry.pin {
        PinMember::NotApplicable => return (Ok(()), None),
        PinMember::Unpinned => return (Err(EngineRefusal::RootPinMissing), None),
        PinMember::Pinned(pin) => pin,
    };
    if pin_history::previously_listed(state, &pin.pin_id, query.resource_id) {
        return (Err(EngineRefusal::RootPinPreviouslyListed), None);
    }
    let Some(artifact) = query.artifact.filter(|a| a.pin_ref == pin.pin_ref) else {
        return (Err(EngineRefusal::ArtifactPinRef), None);
    };
    let latched = state
        .root_mismatch
        .get(query.resource_id)
        .filter(|(pin_ref, _)| pin_ref == &pin.pin_ref)
        .map(|(_, since)| *since);
    // S.5: once a mismatch is seen under this `pin_ref`, a later matching
    // observation does not end it; only a successful walk does.
    let (observation, latch) = match (query.root, latched) {
        (RootObservation::Unobserved, _) => (RootObservation::Unobserved, None),
        (RootObservation::Matches, Some(since)) => (
            RootObservation::Mismatch {
                detected_mono_ms: since,
            },
            None,
        ),
        (RootObservation::Mismatch { detected_mono_ms }, Some(since)) => (
            RootObservation::Mismatch {
                detected_mono_ms: detected_mono_ms.min(since),
            },
            None,
        ),
        (RootObservation::Mismatch { detected_mono_ms }, None) => {
            (query.root, Some((pin.pin_ref.clone(), detected_mono_ms)))
        }
        (RootObservation::Matches, None) => (RootObservation::Matches, None),
    };
    let verdict = if stale_serve::permits(observation, artifact, now_mono, settings) {
        Ok(())
    } else {
        Err(EngineRefusal::RootPinMismatch)
    };
    (verdict, latch)
}

pub(crate) fn walk_inputs(
    entries: &HashMap<String, GateEntry>,
    state: &EngineGateState,
    resource_id: &str,
) -> WalkInputs {
    let own_pin = entries
        .get(resource_id)
        .and_then(|e| e.pin.pin())
        .map(|p| p.pin_id.clone());
    let mut inputs = WalkInputs {
        history_available: state.history_available,
        ..WalkInputs::default()
    };
    for (id, entry) in entries {
        let (true, Some(pin)) = (id != resource_id, entry.pin.pin()) else {
            continue;
        };
        if entry.active {
            inputs.nesting_pins.insert(pin.pin_id.clone());
        } else {
            inputs.skip_pins.insert(pin.pin_id.clone());
        }
    }
    for (pin_id, ids) in &state.pin_history {
        if ids.iter().any(|id| id != resource_id) && own_pin.as_deref() != Some(pin_id) {
            inputs.skip_pins.insert(pin_id.clone());
        }
    }
    inputs
}
