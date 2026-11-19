//! Storage-root pins (engine profile).
//!
//! For pinned resources the authority delivers `{pin_id, pin_ref}` in the
//! list: `pin_id` is the engine's own observation of the storage root, and
//! `pin_ref` is the authority's attestation id. The engine serves a pinned
//! resource only:
//! - when the pin is non-null and its `pin_id` is not in the history under
//!   another resource;
//! - from artifacts whose durable derivation record carries the listed
//!   `pin_ref`; and
//! - while it observes the root at `pin_id`, or within a bounded stale window
//!   for artifacts that a walk validated under the same `pin_ref`.
//!
//! At admission, a pin that is null, changed since the list last applied in
//! this process, or held under another resource is refused at once. Only a
//! completed apply broadens serving again.

use super::{EngineGateState, EngineQuery, EngineSettings};
use crate::gate::{GateEntry, Refusal};
use crate::grammar;
use crate::verify::{ListedItem, PageError};
use crate::wire::Item;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RootPin {
    pub pin_id: String,
    pub pin_ref: String,
}

/// The storage-pin member of a list item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinMember {
    /// The item's kind carries no pin for this holder (member absent).
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

/// A `(pin_id, resource_id)` pair seen in an applied list. Pairs are never
/// replaced or evicted; one pin id may appear under several resources.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinPair {
    pub pin_id: String,
    pub resource_id: String,
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
    /// `pin_id` under this same `pin_ref`.
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

/// Parses the storage-pin member. `pinned` says whether this holder kind and
/// resource kind carry it; anywhere else the member is refused.
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

/// Pairs in `items` that the history does not hold yet.
pub(crate) fn new_pairs(items: &[ListedItem], history: &BTreeSet<PinPair>) -> Vec<PinPair> {
    items
        .iter()
        .filter_map(|listed| {
            listed.item.root_pin.pin().map(|pin| PinPair {
                pin_id: pin.pin_id.clone(),
                resource_id: listed.item.resource.id.clone(),
            })
        })
        .filter(|pair| !history.contains(pair))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(crate) fn history_map<'a>(
    pairs: impl Iterator<Item = &'a PinPair>,
) -> HashMap<String, BTreeSet<String>> {
    let mut map: HashMap<String, BTreeSet<String>> = HashMap::new();
    for pair in pairs {
        map.entry(pair.pin_id.clone())
            .or_default()
            .insert(pair.resource_id.clone());
    }
    map
}

/// The raised ceiling when `size` pairs would exceed `ceiling`. The ceiling
/// only ever rises; pairs are never evicted.
pub fn raised_ceiling(size: u64, ceiling: u64) -> Option<u64> {
    (size > ceiling).then(|| size.max(ceiling.saturating_mul(2)))
}

/// The pins of the list being applied, by holding id: the next process-local
/// baseline for "changed".
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

/// Admission refusal for an active, untombstoned item: a null pin, a pin
/// changed against the baseline (empty at process start, so every pin of a
/// process's first list counts as changed), or a pin the history holds under
/// another resource. The history includes the list's own pairs.
pub(crate) fn refused_at_admission(
    item: &Item,
    baseline: &HashMap<String, Option<RootPin>>,
    history: &HashMap<String, BTreeSet<String>>,
) -> bool {
    match &item.root_pin {
        PinMember::NotApplicable => false,
        PinMember::Unpinned => true,
        PinMember::Pinned(pin) => {
            let unchanged = baseline.get(&item.holding_id) == Some(&Some(pin.clone()));
            let conflict = history
                .get(&pin.pin_id)
                .is_some_and(|ids| ids.iter().any(|other| other != &item.resource.id));
            !unchanged || conflict
        }
    }
}

pub(crate) fn previously_listed(state: &EngineGateState, pin_id: &str, resource_id: &str) -> bool {
    state
        .pin_history
        .get(pin_id)
        .is_some_and(|ids| ids.iter().any(|id| id != resource_id))
}

/// The pin rules for one entry that passed every core rule.
pub(crate) fn check(
    entry: &GateEntry,
    state: &EngineGateState,
    query: &EngineQuery,
    now_mono: u64,
    settings: &EngineSettings,
) -> Result<(), Refusal> {
    let pin = match &entry.pin {
        PinMember::NotApplicable => return Ok(()),
        PinMember::Unpinned => return Err(Refusal::RootPinMissing),
        PinMember::Pinned(pin) => pin,
    };
    if previously_listed(state, &pin.pin_id, query.resource_id) {
        return Err(Refusal::RootPinPreviouslyListed);
    }
    let artifact = query
        .artifact
        .filter(|a| a.pin_ref == pin.pin_ref)
        .ok_or(Refusal::ArtifactPinRef)?;
    match query.root {
        RootObservation::Matches => Ok(()),
        RootObservation::Mismatch { detected_mono_ms }
            if artifact.validated_under_matching_root
                && now_mono.saturating_sub(detected_mono_ms) <= settings.max_stale_serve_ms =>
        {
            Ok(())
        }
        _ => Err(Refusal::RootPinMismatch),
    }
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
