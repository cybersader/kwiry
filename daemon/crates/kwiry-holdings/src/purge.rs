//! Purge execution under listed obligations (v2 Part I §3.4 rules 3–6,
//! §C.0 *storage exhaustion*, *no-resurrection*; H.9, H.45). Kwiry's rows:
//! Annex K.1 O4–O7, O11, O14x and §5.6 steps 4–6 of its design.
//!
//! **What may be deleted.** A unit is deleted only when all of these hold:
//! - the list applied in this process lists an obligation for exactly the
//!   unit's resource: `terminating` (including `blocked`), `retired` with
//!   `escalated`, or `retired` with `complete` and residual data (§3.4.4);
//! - the resource is durably tombstoned;
//! - the purger's independent census attributes the unit to that resource,
//!   the unit has no open handle, and no release reserved or holds it;
//! - the claim was durably recorded first, and the holder has not
//!   fail-stopped (H.45).
//!
//! Lease expiry, unknown or unattributable data, quarantined unattributed
//! data, authority errors, regression, rollback suspicion, disk pressure
//! and absence from a list never reach the purger. A `blocked` obligation
//! only continues the purge of its own attributed data.
//!
//! **Order, with a durable write before and after every step.**
//! 1. *Detach:* W(`Detaching`) → purger detach → W(`Detached`).
//! 2. *Delete:* W(`Deleting`, exact claimed units) → one deletion per unit
//!    under the obligation's claimant → W(`Deleted`, deleted units).
//! 3. *Verify:* purger barrier → independent census → a final-scan record
//!    bound to what was observed. It proves `complete` only in the process
//!    that made it, and only while the observed inventory is unchanged: a
//!    late builder output, a reopened handle or a reclassification
//!    invalidates it.
//!
//! A crash at any point resumes from the durable step: a re-run detach is
//! idempotent, and a re-run deletion re-checks every claim against the
//! current list and census before touching anything.

use crate::canonical::{canonical, labelled_digest, sha256_tagged};
use crate::client::Transport;
use crate::clock::format_utc_seconds;
use crate::events::HolderEvent;
use crate::holder::{Holder, report_write_failure};
use crate::obligations::{Channel, ObligationBook, PurgeStep};
use crate::store::{
    ArtifactPurger, Census, Claimant, DeleteOutcome, Disposition, ObservedUnit, PublishPlan,
    ResourceStore,
};
use crate::wire::{ObligationState, Phase, ResourceKey};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Version of the final-scan record format (`evidence.kind` `…/v2`).
pub const SCAN_RECORD_VERSION: u64 = 2;

/// A final scan made in this process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedScan {
    /// Digest of the observed inventory the scan read (the resource's units
    /// with their handles, and the unattributed set).
    pub binding: String,
    pub remaining: u64,
    pub fd_refs: u64,
    pub barrier_ok: bool,
    pub scanned_at: String,
    /// The canonical final-scan record.
    pub record: Vec<u8>,
}

impl VerifiedScan {
    pub fn evidence_digest(&self) -> String {
        sha256_tagged(&self.record)
    }

    /// Nothing of the resource remains, the barrier held and no descriptor
    /// refers to a deleted unit.
    pub fn clean(&self) -> bool {
        self.remaining == 0 && self.fd_refs == 0 && self.barrier_ok
    }
}

/// The census read against the current classification.
#[derive(Clone, Debug, Default)]
pub struct Inventory {
    /// Units the census attributes to a resource the classification also
    /// attributes (listed `active`, unconfigured, obligated, retired).
    pub attributed: BTreeMap<ResourceKey, Vec<ObservedUnit>>,
    /// Units no listed holding accounts for: unknown, tombstoned-unlisted,
    /// unattributable, or reserved by a release until deleted.
    pub unattributed: crate::store::ManifestUnits,
}

impl Inventory {
    pub fn new(census: &Census, plan: &PublishPlan) -> Self {
        Self::with_held(census, plan, &BTreeSet::new())
    }

    /// As [`new`](Self::new), with `held` units a release holds: whatever
    /// the census says, they are counted unattributed and no obligation
    /// can claim them.
    pub fn with_held(census: &Census, plan: &PublishPlan, held: &BTreeSet<String>) -> Self {
        let mut inventory = Self::default();
        for unit in &census.units {
            let attributed = unit.attribution.as_ref().filter(|key| {
                !unit.reserved
                    && !held.contains(&unit.unit_id)
                    && plan
                        .dispositions
                        .get(*key)
                        .is_some_and(|d| !d.is_unattributed())
            });
            match attributed {
                Some(key) => inventory
                    .attributed
                    .entry(key.clone())
                    .or_default()
                    .push(unit.clone()),
                None => {
                    inventory
                        .unattributed
                        .insert(unit.unit_id.clone(), unit.attribution.clone());
                }
            }
        }
        inventory
    }

    pub fn units(&self, resource: &ResourceKey) -> &[ObservedUnit] {
        self.attributed.get(resource).map_or(&[], Vec::as_slice)
    }

    pub fn remaining(&self, resource: &ResourceKey) -> u64 {
        u64::try_from(self.units(resource).len()).unwrap_or(u64::MAX)
    }

    pub fn fd_refs(&self, resource: &ResourceKey) -> u64 {
        self.units(resource)
            .iter()
            .map(|u| u64::from(u.open_handles))
            .sum()
    }

    pub fn unattributed_count(&self) -> u64 {
        u64::try_from(self.unattributed.len()).unwrap_or(u64::MAX)
    }

    /// What a final scan of `resource` is bound to.
    pub fn binding(&self, resource: &ResourceKey) -> String {
        let mut units: Vec<Value> = self
            .units(resource)
            .iter()
            .map(|u| json!({ "unit": u.unit_id, "handles": u.open_handles }))
            .collect();
        units.sort_by_key(canonical);
        let value = json!({
            "units": units,
            "unattributed": self.unattributed.iter().collect::<Vec<_>>(),
        });
        crate::b64::encode(&labelled_digest("holdings-scan-binding/v1", &value))
    }
}

/// One obligation the purge works on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub obligation_id: String,
    pub holding_id: String,
    pub resource: ResourceKey,
    /// `retired` with `complete` and data still present (§3.4 rule 4).
    pub residual: bool,
}

impl Target {
    pub fn claimant(&self) -> Claimant {
        Claimant::Obligation {
            obligation_id: self.obligation_id.clone(),
            resource: self.resource.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PurgeSummary {
    pub deleted: usize,
    pub waiting_on_handles: usize,
    pub failed: usize,
    pub verified: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObligationsRefused {
    /// Authority reported custody unavailable; a successful list apply clears it.
    LifecycleUnavailable,
    /// v2 H.45: the holder fail-stopped; no purge, report or write runs.
    HolderStateUnwritable,
    /// No list was applied in this process: nothing is known to be owed.
    NoListThisProcess,
    /// `503 authority_epoch_mismatch` (H.12 steps 2 and 4): nothing is
    /// deleted until a verified list applies (§4.5).
    AuthorityRollbackSuspected,
    /// A list committed its A1 write but its apply did not complete: the
    /// earlier list is superseded and the new one is not in force, so
    /// nothing is purged or reported until an apply fully succeeds.
    ApplyIncomplete,
}

fn file_safe(text: &str) -> bool {
    crate::grammar::is_b64url(text)
}

impl<S: ResourceStore, T: Transport> Holder<S, T> {
    pub(crate) fn plan(&self) -> PublishPlan {
        self.applied
            .as_ref()
            .map(|a| a.plan.clone())
            .unwrap_or_default()
    }

    /// The census against the current classification.
    pub(crate) fn inventory(
        &self,
        purger: &dyn ArtifactPurger,
    ) -> Result<Inventory, crate::store::StoreError> {
        Ok(self.inventory_of(&purger.census()?))
    }

    /// `census` against the current classification and the units releases
    /// hold.
    pub(crate) fn inventory_of(&self, census: &Census) -> Inventory {
        Inventory::with_held(
            census,
            &self.plan(),
            &self.writer.state().status.release_held_units(),
        )
    }

    /// Every obligation of the applied list that authorizes a purge now.
    pub(crate) fn targets(&self, inventory: &Inventory) -> Vec<Target> {
        let Some(applied) = &self.applied else {
            return Vec::new();
        };
        let state = self.writer.state();
        applied
            .items
            .iter()
            .filter_map(|item| {
                let obligation = item.obligation.as_ref()?;
                if !state.is_tombstoned(&item.resource.id) || !file_safe(&obligation.obligation_id)
                {
                    return None;
                }
                let obligated = item.is_obligated();
                // §3.4 rule 4: residual data, or a residual purge whose
                // reconfirmation is still owed (durable, survives restart).
                let owed = state
                    .obligations
                    .channels
                    .get(&obligation.obligation_id)
                    .is_some_and(|c| c.reconfirm_pending);
                let residual = item.phase == Phase::Retired
                    && obligation.state == ObligationState::Complete
                    && (inventory.remaining(&item.resource) > 0 || owed);
                let disposition = applied.plan.dispositions.get(&item.resource);
                let classified = matches!(
                    disposition,
                    Some(Disposition::Obligated { .. } | Disposition::RetiredComplete { .. })
                );
                (classified && (obligated || residual)).then(|| Target {
                    obligation_id: obligation.obligation_id.clone(),
                    holding_id: item.holding_id.clone(),
                    resource: item.resource.clone(),
                    residual: !obligated,
                })
            })
            .collect()
    }

    pub(crate) fn obligations_allowed(&self) -> Result<(), ObligationsRefused> {
        if self.fail_stopped() {
            return Err(ObligationsRefused::HolderStateUnwritable);
        }
        if self.applied.is_none() {
            return Err(if self.apply_failed_since_admit {
                ObligationsRefused::ApplyIncomplete
            } else {
                ObligationsRefused::NoListThisProcess
            });
        }
        Ok(())
    }

    /// Deletion needs everything reporting needs, and no rollback suspicion.
    pub(crate) fn purge_allowed(&self) -> Result<(), ObligationsRefused> {
        self.obligations_allowed()?;
        if self.apply_failed_since_admit {
            return Err(ObligationsRefused::ApplyIncomplete);
        }
        if self.lifecycle_unavailable {
            return Err(ObligationsRefused::LifecycleUnavailable);
        }
        if self.rollback_suspected {
            return Err(ObligationsRefused::AuthorityRollbackSuspected);
        }
        Ok(())
    }

    fn book(&self) -> ObligationBook {
        self.writer.state().obligations.clone()
    }

    /// Commits `book`; on failure emits the storage alert and returns false
    /// (the step fails with no state change).
    pub(crate) fn commit_book(&mut self, book: ObligationBook) -> bool {
        match self.writer.update_obligations(book) {
            Ok(()) => true,
            Err(error) => {
                report_write_failure(self.events.as_ref(), &error);
                false
            }
        }
    }

    fn channel_mut<'a>(book: &'a mut ObligationBook, target: &Target) -> &'a mut Channel {
        book.channels
            .entry(target.obligation_id.clone())
            .or_insert_with(|| Channel::new(&target.holding_id, target.resource.clone()))
    }

    /// Step 1 for every target: detach, between two durable writes.
    pub fn purge_detach(
        &mut self,
        purger: &mut dyn ArtifactPurger,
    ) -> Result<usize, ObligationsRefused> {
        self.purge_allowed()?;
        let Ok(inventory) = self.inventory(purger) else {
            self.events
                .emit(HolderEvent::PurgeStepFailed { step: "census" });
            return Ok(0);
        };
        let mut detached = 0;
        for target in self.targets(&inventory) {
            self.purge_allowed()?;
            let step = self
                .writer
                .state()
                .obligations
                .channels
                .get(&target.obligation_id)
                .map(|c| c.purge.step)
                .unwrap_or_default();
            if matches!(step, PurgeStep::Idle | PurgeStep::Detaching) {
                let mut book = self.book();
                Self::channel_mut(&mut book, &target).purge.step = PurgeStep::Detaching;
                if !self.commit_book(book) {
                    continue;
                }
            }
            // Idempotent; run in every process so the purger's own state
            // agrees whatever a crash left behind.
            if purger.detach(&target.resource).is_err() {
                self.events
                    .emit(HolderEvent::PurgeStepFailed { step: "detach" });
                continue;
            }
            if matches!(step, PurgeStep::Idle | PurgeStep::Detaching) {
                let mut book = self.book();
                Self::channel_mut(&mut book, &target).purge.step = PurgeStep::Detached;
                if !self.commit_book(book) {
                    continue;
                }
            }
            detached += 1;
        }
        self.emit_reserve_uses();
        Ok(detached)
    }

    /// Steps 2 and 3 for every detached target: delete the claimed units,
    /// then verify with a barrier and an independent census.
    pub fn purge_delete_and_verify(
        &mut self,
        purger: &mut dyn ArtifactPurger,
    ) -> Result<PurgeSummary, ObligationsRefused> {
        self.purge_allowed()?;
        let mut summary = PurgeSummary::default();
        let Ok(inventory) = self.inventory(purger) else {
            self.events
                .emit(HolderEvent::PurgeStepFailed { step: "census" });
            return Ok(summary);
        };
        let targets = self.targets(&inventory);
        let mut passed = Vec::new();
        for target in &targets {
            if self.delete_pass(purger, target, &inventory, &mut summary)? {
                passed.push(target.clone());
            }
        }
        if passed.is_empty() {
            self.emit_reserve_uses();
            return Ok(summary);
        }
        // Verify after every deletion, so all scans read one inventory.
        let barrier_ok = purger.barrier().is_ok();
        if !barrier_ok {
            self.events
                .emit(HolderEvent::PurgeStepFailed { step: "barrier" });
        }
        let census = match purger.census() {
            Ok(census) => census,
            Err(_) => {
                self.events
                    .emit(HolderEvent::PurgeStepFailed { step: "census" });
                return Ok(summary);
            }
        };
        let inventory = self.inventory_of(&census);
        for target in passed {
            let scan = self.final_scan(&target, &census, &inventory, barrier_ok);
            self.verified.insert(target.obligation_id.clone(), scan);
            summary.verified += 1;
        }
        if summary.deleted > 0 {
            // K4: deletions freed space; refill the reserve.
            self.writer.replenish_reserve();
        }
        self.emit_reserve_uses();
        Ok(summary)
    }

    /// One deletion pass. Returns whether the pass reached `Deleted`.
    fn delete_pass(
        &mut self,
        purger: &mut dyn ArtifactPurger,
        target: &Target,
        inventory: &Inventory,
        summary: &mut PurgeSummary,
    ) -> Result<bool, ObligationsRefused> {
        let Some(channel) = self
            .writer
            .state()
            .obligations
            .channels
            .get(&target.obligation_id)
            .cloned()
        else {
            return Ok(false);
        };
        if matches!(channel.purge.step, PurgeStep::Idle | PurgeStep::Detaching) {
            return Ok(false);
        }
        let observed: BTreeSet<&str> = inventory
            .units(&target.resource)
            .iter()
            .map(|u| u.unit_id.as_str())
            .collect();
        // A claim recorded before a crash: units no longer observed are gone.
        let recovered: BTreeSet<String> = channel
            .purge
            .claimed
            .iter()
            .filter(|unit| !observed.contains(unit.as_str()))
            .cloned()
            .collect();
        let busy = inventory
            .units(&target.resource)
            .iter()
            .filter(|u| u.open_handles > 0)
            .count();
        let claims: BTreeSet<String> = inventory
            .units(&target.resource)
            .iter()
            .filter(|u| u.open_handles == 0 && !u.reserved)
            .map(|u| u.unit_id.clone())
            .collect();
        if busy > 0 {
            summary.waiting_on_handles += busy;
            self.events
                .emit(HolderEvent::PurgeWaitingOnHandles { count: busy });
        }

        if claims.is_empty() && recovered.is_empty() && channel.purge.step == PurgeStep::Deleted {
            return Ok(true);
        }
        // Before: the exact claim, and for residual data the durable marker
        // that a reconfirmation is owed once it is gone.
        let mut book = self.book();
        let channel = Self::channel_mut(&mut book, target);
        if target.residual && !claims.is_empty() {
            channel.reconfirm_pending = true;
        }
        let record = &mut channel.purge;
        record.deleted.extend(recovered);
        record.claimed = claims.clone();
        record.step = PurgeStep::Deleting;
        if !self.commit_book(book) {
            return Ok(false);
        }

        let claimant = target.claimant();
        let mut deleted = BTreeSet::new();
        let mut failed = 0;
        for unit in &claims {
            // H.45: nothing runs after a fail-stop, deletions included.
            self.purge_allowed()?;
            match purger.delete(unit, &claimant) {
                Ok(DeleteOutcome::Deleted | DeleteOutcome::Absent) => {
                    deleted.insert(unit.clone());
                }
                Ok(DeleteOutcome::Busy) => summary.waiting_on_handles += 1,
                Ok(DeleteOutcome::Failed) => failed += 1,
                Err(_) => {
                    // The purger refused or stopped: the claim stays durable
                    // and the next pass re-checks it.
                    self.events
                        .emit(HolderEvent::PurgeStepFailed { step: "delete" });
                    return Ok(false);
                }
            }
        }
        if failed > 0 {
            summary.failed += failed;
            self.events
                .emit(HolderEvent::PurgeDeleteFailed { count: failed });
        }
        summary.deleted += deleted.len();

        // After: what was deleted.
        let mut book = self.book();
        let record = &mut Self::channel_mut(&mut book, target).purge;
        record.deleted.extend(deleted);
        record.claimed.clear();
        record.step = PurgeStep::Deleted;
        record.passes += 1;
        Ok(self.commit_book(book))
    }

    fn final_scan(
        &self,
        target: &Target,
        census: &Census,
        inventory: &Inventory,
        barrier_ok: bool,
    ) -> VerifiedScan {
        let remaining = inventory.remaining(&target.resource);
        let fd_refs = inventory.fd_refs(&target.resource);
        let binding = inventory.binding(&target.resource);
        let scanned_at = format_utc_seconds(self.clock.wall_ms());
        let deleted: Vec<String> = self
            .writer
            .state()
            .obligations
            .channels
            .get(&target.obligation_id)
            .map(|c| c.purge.deleted.iter().cloned().collect())
            .unwrap_or_default();
        // No resource id: the claimant is the obligation, units are random.
        let record = json!({
            "v": SCAN_RECORD_VERSION,
            "holder_id": self.writer.enrollment().holder_id,
            "obligation_id": target.obligation_id,
            "claimant": { "kind": "obligation", "obligation_id": target.obligation_id },
            "inventory": { "source": "independent-census", "binding": binding },
            "locations": census.locations,
            "deleted": deleted,
            "found": remaining,
            "fd_refs": fd_refs,
            "unattributed": inventory.unattributed_count(),
            "barrier_ok": barrier_ok,
            "scanned_at": scanned_at,
            "binary_version": concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
        });
        VerifiedScan {
            binding,
            remaining,
            fd_refs,
            barrier_ok,
            scanned_at,
            record: canonical(&record).into_bytes(),
        }
    }
}
