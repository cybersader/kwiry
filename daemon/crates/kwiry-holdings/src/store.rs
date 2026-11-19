//! The engine-facing traits: what the holder holds, and what an applied list
//! means for it.
//!
//! [`ResourceStore`] has no deletion method. Nothing in the apply path can
//! delete: unknown data is withheld or quarantined, lease expiry only stops
//! serving. Deletion happens solely through [`ArtifactPurger`], under a
//! listed obligation (the `purge` module).

pub use crate::wire::ResourceKey;
use std::collections::{BTreeMap, BTreeSet};

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

/// The only authorities a deletion may cite (v2 §3.4 rule 5, §11.2 D). A
/// purger checks the claimant against its own records of the unit and
/// refuses any mismatch: no other reason can stand in for either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claimant {
    /// A listed obligation for exactly this resource.
    Obligation {
        obligation_id: String,
        resource: ResourceKey,
    },
    /// An operator release whose strict manifest match the holder proved
    /// (v2 §11.2 D, §11.4 G.5): exactly the units this instruction reserved
    /// through [`ArtifactPurger::reserve`], and nothing else.
    Release { instruction_id: String },
}

impl Claimant {
    /// The resource an obligation claim names; `None` for a release, which
    /// names units, never a resource.
    pub fn resource(&self) -> Option<&ResourceKey> {
        match self {
            Self::Obligation { resource, .. } => Some(resource),
            Self::Release { .. } => None,
        }
    }
}

/// One unit of retained data, as the purger's independent census observed it.
///
/// Unit ids are random and holder-defined (an object id, a staging build id,
/// a quarantine entry name). They MUST NOT encode a resource id: they appear
/// in quarantine manifests and in final-scan records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedUnit {
    pub unit_id: String,
    /// The resource the census derived this unit belongs to, from the bytes
    /// it found and not from any registry, or `None` when it cannot tell.
    /// An unattributable unit is never deleted by an obligation.
    pub attribution: Option<ResourceKey>,
    /// Open handles, including handles to an unlinked file. A unit with an
    /// open handle is never deleted, and it still counts as remaining.
    pub open_handles: u32,
    /// Reserved by an operator release ([`ArtifactPurger::reserve`]): never
    /// claimed by an obligation, never re-mapped, and counted unattributed
    /// until deleted.
    pub reserved: bool,
}

/// Everything the purger physically observed, independently of its own
/// registry: every storage location, open descriptors (unlinked files
/// included) and builder outputs behind the publication barrier.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// Names of the locations scanned (no paths, no ids).
    pub locations: Vec<String>,
    pub units: Vec<ObservedUnit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// Unlinked, and every parent directory synced.
    Deleted,
    /// Already gone: nothing to do.
    Absent,
    /// An open handle or lease still refers to it; retried on a later pass.
    Busy,
    /// The deletion failed; the unit stays attributed and is retried.
    Failed,
}

/// Anything an adapter's publication lock guard is made of.
pub trait FreezeHold {}
impl<T> FreezeHold for T {}

/// Proof that new publications are held back. While it lives, no artifact
/// may become visible to [`ArtifactPurger::census`]; dropping it lets held
/// publications proceed. See [`ArtifactPurger::freeze_publications`].
#[must_use = "publications resume as soon as the freeze is dropped"]
pub struct PublicationFreeze<'a> {
    _hold: Box<dyn FreezeHold + 'a>,
}

impl<'a> PublicationFreeze<'a> {
    /// Wraps the adapter's exclusive publication lock guard (or any value
    /// whose drop releases it).
    pub fn new(hold: impl FreezeHold + 'a) -> Self {
        Self {
            _hold: Box::new(hold),
        }
    }
}

/// Deletes retained data under a listed obligation (v2 §3.4 rules 3 to 5)
/// or an executed operator release (v2 §11.2 D, G.5).
///
/// For an obligation, the holder drives it in a fixed, crash-safe order,
/// with a durable holder-state write before and after every step:
///
/// 1. [`detach`](Self::detach): the resource can no longer be opened, leased
///    or built from. Idempotent; deletes nothing.
/// 2. [`delete`](Self::delete): one unit at a time, each under the exact
///    [`Claimant`]. The purger refuses a unit it does not attribute to the
///    claimant's resource, a unit with an open handle and a reserved unit.
/// 3. [`barrier`](Self::barrier) then [`census`](Self::census): sync every
///    location, then observe everything that remains.
///
/// For a release (the `release` module) the order is: a durable record of
/// the exact units, then [`reserve`](Self::reserve), a durable `draining`
/// mark, the exact claimed units recorded, one deletion per unit under
/// [`Claimant::Release`], the deleted units recorded, and a barrier and
/// census proving every reserved unit gone.
///
/// Nothing else ever calls `delete`. Lease expiry, unknown data, authority
/// errors, regression, disk pressure, a declined release (`blocked`) and
/// absence from a list never reach it.
pub trait ArtifactPurger {
    fn detach(&mut self, resource: &ResourceKey) -> Result<(), StoreError>;
    /// Deletes one unit under `claimant`. The purger refuses (`Err`) a unit
    /// it does not attribute to an obligation claimant's resource, a unit a
    /// release reserved when the claimant is an obligation, and a unit not
    /// reserved by exactly that release when the claimant is a release. A
    /// unit with an open handle is `Busy`, never deleted.
    fn delete(&mut self, unit_id: &str, claimant: &Claimant) -> Result<DeleteOutcome, StoreError>;
    /// Reserves `units` for the release `instruction_id` (v2 §11.2 D).
    ///
    /// **Adapter obligation.** Idempotent for the same instruction and
    /// units. Once it returns `Ok`, and across restarts until each unit is
    /// deleted: the census reports every one of them `reserved`; no
    /// obligation claim may delete them; no publication, mapping, build or
    /// list may re-map or reuse them; and only [`Claimant::Release`] with
    /// the same `instruction_id` may delete them. It deletes nothing. It
    /// returns `Err` if any unit is reserved by another instruction or the
    /// reservation cannot be made durable; the holder then retries later
    /// and deletes nothing meanwhile.
    fn reserve(&mut self, instruction_id: &str, units: &BTreeSet<String>)
    -> Result<(), StoreError>;
    /// Syncs every storage location; an error means the barrier failed.
    fn barrier(&mut self) -> Result<(), StoreError>;
    fn census(&self) -> Result<Census, StoreError>;

    /// Holds every new publication back until the returned guard drops.
    ///
    /// **Adapter obligation.** A *publication* is anything that can make an
    /// artifact observable to [`census`](Self::census): a builder renaming
    /// output into place, a registry or manifest insert, a cache fill, a
    /// restore. The adapter MUST:
    ///
    /// 1. make every publication take the **shared** side of one
    ///    publication lock for its whole visible step, and hold it until the
    ///    artifact is fully observable;
    /// 2. make this method take the **exclusive** side, waiting for every
    ///    publication already holding the shared side to finish;
    /// 3. while the guard lives, make a publication that wants the lock
    ///    **wait** (never drop it, and never apply it out of view of the
    ///    census);
    /// 4. return `Err` if it cannot give that guarantee. The holder then
    ///    cancels the report as never transmitted.
    ///
    /// The holder takes the freeze immediately before the final census of a
    /// report, keeps it across the first byte of the transmission and until
    /// the send completes, then drops it. A report's claim therefore never
    /// misses an artifact published before its first byte.
    fn freeze_publications(&self) -> Result<PublicationFreeze<'_>, StoreError>;
}
