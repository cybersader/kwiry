//! S.6: the holder-local pin-history set (v2 Part II, S.6).
//!
//! A durable, refusal-only set of `(pin_id, resource.id)` pairs from every
//! verified list the holder applied, persisted in the same atomic commit as
//! the high-water (H.16.5). A pin id may appear under several resources, and
//! no pair is ever replaced or evicted: if a list would exceed the ceiling,
//! the holder alerts and raises the ceiling before persisting. A list that
//! attests a resource to a `pin_id` the set holds under another resource
//! fails closed with `root_pin_previously_listed`. Losing the set never
//! authorizes serving; it only weakens this refusal.

use super::EngineGateState;
use crate::verify::ListedItem;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

pub const DEFAULT_CEILING: u64 = 100_000;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinPair {
    pub pin_id: String,
    pub resource_id: String,
}

/// Pairs in `items` that the set does not hold yet.
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

pub(crate) fn map<'a>(
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

/// The raised ceiling when `size` pairs would exceed `ceiling`.
pub fn raised_ceiling(size: u64, ceiling: u64) -> Option<u64> {
    (size > ceiling).then(|| size.max(ceiling.saturating_mul(2)))
}

/// Whether `pin_id` is held under any resource other than `resource_id`.
pub(crate) fn conflicts(
    history: &HashMap<String, BTreeSet<String>>,
    pin_id: &str,
    resource_id: &str,
) -> bool {
    history
        .get(pin_id)
        .is_some_and(|ids| ids.iter().any(|id| id != resource_id))
}

pub(crate) fn previously_listed(state: &EngineGateState, pin_id: &str, resource_id: &str) -> bool {
    conflicts(&state.pin_history, pin_id, resource_id)
}
