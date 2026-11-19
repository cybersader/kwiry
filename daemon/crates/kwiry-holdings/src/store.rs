//! The engine-facing traits: what the holder holds, and what an applied list
//! means for it.
//!
//! [`ResourceStore`] has no deletion method. Nothing in the apply path can
//! delete: unknown data is withheld or quarantined, lease expiry only stops
//! serving. Deletion happens solely through [`ArtifactPurger`], under a
//! listed obligation, in a later slice.

pub use crate::wire::ResourceKey;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldResource {
    pub key: ResourceKey,
    /// Named in the engine's configuration.
    pub configured: bool,
    /// Holder-defined units of retained data (partitions, objects, files).
    pub units: u64,
}

/// What the holder decided for one resource at one applied list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// Listed `active`, configured, not tombstoned: mapped for serving while
    /// its lease holds.
    Mapped { holding_id: String },
    /// Listed `active` but not configured: kept, withheld, attributed.
    WithheldUnconfigured { holding_id: String },
    /// Not listed and never tombstoned: kept, withheld, unattributed.
    WithheldUnknown,
    /// Listed `terminating`, or `retired` with an escalated obligation:
    /// revoked, attributed to the obligation, purged only under it.
    Obligated {
        holding_id: String,
        obligation_id: String,
    },
    /// Listed `retired` with `complete`: revoked; residual data is purged under
    /// the completed obligation.
    RetiredComplete {
        holding_id: String,
        obligation_id: String,
    },
    /// Tombstoned and no longer listed: quarantined, unattributed, never
    /// served again.
    QuarantinedUnattributed,
    /// Tombstoned but listed `active` (an authority invariant violation):
    /// refused, withheld, unattributed.
    TombstoneConflict { holding_id: String },
}

impl Disposition {
    /// Counted in `quarantined_unattributed`.
    pub fn is_unattributed(&self) -> bool {
        matches!(
            self,
            Self::WithheldUnknown | Self::QuarantinedUnattributed | Self::TombstoneConflict { .. }
        )
    }
}

/// The classification an applied list produces for everything held or listed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublishPlan {
    pub dispositions: BTreeMap<ResourceKey, Disposition>,
    pub unattributed_units: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("resource store: {0}")]
pub struct StoreError(pub String);

pub trait ResourceStore {
    /// Every resource the engine holds data or configuration for.
    fn held(&self) -> Result<Vec<HeldResource>, StoreError>;
    /// Durably publishes the classification (rows A2–A3): which resources
    /// are mapped, withheld, revoked or quarantined. Must not delete.
    fn publish(&mut self, plan: &PublishPlan) -> Result<(), StoreError>;
    /// In-memory revocation and cache eviction after the swap (row A5).
    /// Must not delete.
    fn revoke(&mut self, plan: &PublishPlan);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PurgeProgress {
    pub units_purged: u64,
    pub units_remaining: u64,
}

/// Deletes a resource's artifacts under a listed obligation. Defined here to
/// fix the boundary; the apply path of this slice never calls it.
pub trait ArtifactPurger {
    fn purge(
        &mut self,
        resource: &ResourceKey,
        obligation_id: &str,
    ) -> Result<PurgeProgress, StoreError>;
}
