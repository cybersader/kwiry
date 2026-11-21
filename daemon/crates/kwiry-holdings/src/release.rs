//! Operator release instructions (v2 Part I §11.1 C, §11.2 D, §11.4 G.2,
//! G.3, G.5; H.9, H.14, H.18, H.35). Kwiry's rows: Annex K.1 L1–L11.
//!
//! A release is the only path besides a listed obligation by which a holder
//! deletes anything, and it deletes only what a human approved exactly.
//!
//! **Bind and check (L1, L2).** After the list that carries an instruction
//! is applied, the holder looks up the manifest its own report named, by
//! that report's exact digest (`bound_report_digest`, G.3), and checks it
//! against its own independent census. The release proceeds only if a
//! manifest is retained for that digest, its count equals `approved_count`,
//! and **every** unit in it is still in the current unattributed set,
//! present and not reserved. Anything else (no manifest, a different count,
//! a unit gone, attributed, reclassified or reserved) is a mismatch:
//! nothing is reserved or deleted, and a status reports `mismatch` with the
//! current manifest digest. There is no partial or intersection execution
//! (G.5).
//!
//! **Reserve, drain, delete (L1, L4, L5), with a durable write before and
//! after every step:**
//! 1. W(`reserving`, the exact units) → purger reserve → W(`draining`).
//! 2. W(claimed units) → one deletion per unit under the release claimant →
//!    W(deleted units).
//! 3. Barrier and census: when none of the units is observed any more,
//!    W(`deleted`, the manifest digest observed now). A status then reports
//!    `executed` (L6) and the instruction is `done` once it is accepted
//!    (L7).
//!
//! A crash at any point resumes from the durable step. A recorded
//! reservation the purger has not confirmed in any process holds every list
//! back until [`Holder::honor_reservations`] confirms it (startup honours a
//! reservation before any list applies). Reserved units are counted
//! unattributed, never claimed by an obligation and never re-mapped.
//!
//! **Already done (L10).** An instruction the holder executed or reported
//! as a mismatch is never evaluated or executed again, whatever re-lists
//! it; its stored result is re-reported only as `status::owed_outcome`
//! says. An instruction no longer listed while it drains still completes
//! (L11). An unknown `kind` is skipped whole when the list is verified
//! (H.35.2); a `blocked` obligation (H.9) authorizes no deletion of any
//! kind.

use crate::client::Transport;
use crate::events::HolderEvent;
use crate::holder::Holder;
use crate::obligations::manifest_digest;
use crate::purge::{Inventory, ObligationsRefused};
use crate::status::{InstructionRecord, ReleaseStep, StatusBook};
use crate::store::{
    ArtifactPurger, Census, Claimant, DeleteOutcome, ReserveOutcome, ResourceStore,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The one instruction kind this holder executes (v2 §11.2 D).
pub const KIND_QUARANTINE_RELEASE: &str = "quarantine_release";

/// A verified `quarantine_release` entry of page 0's `instructions[]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub instruction_id: String,
    pub bound_report_digest: String,
    pub approved_count: u64,
    pub issued_seq: u64,
}

impl Instruction {
    /// Reads an entry the list verification already checked (closed
    /// members, grammar, `issued_seq ≤ rev.seq`).
    pub fn parse(value: &Value) -> Option<Self> {
        if value.get("kind")?.as_str()? != KIND_QUARANTINE_RELEASE {
            return None;
        }
        Some(Self {
            instruction_id: value.get("instruction_id")?.as_str()?.to_owned(),
            bound_report_digest: value.get("bound_report_digest")?.as_str()?.to_owned(),
            approved_count: value.get("approved_count")?.as_u64()?,
            issued_seq: value.get("issued_seq")?.as_u64()?,
        })
    }
}

/// What one release pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReleaseSummary {
    /// Newly listed instructions checked against the census.
    pub evaluated: usize,
    pub reserved: usize,
    pub mismatched: usize,
    pub deleted: usize,
    pub waiting_on_handles: usize,
    pub failed: usize,
    /// Instructions whose every unit is now gone.
    pub executed: usize,
}

impl<S: ResourceStore, T: Transport> Holder<S, T> {
    /// Whether a recorded reservation still waits for the purger's
    /// confirmation (it holds every list back).
    pub fn reservation_pending(&self) -> bool {
        self.writer.state().status.reservation_pending()
    }

    /// Confirms every recorded reservation with the purger (idempotent) and
    /// marks it draining. Run at startup before the first list, and by every
    /// release pass. Returns how many were confirmed.
    pub fn honor_reservations(
        &mut self,
        purger: &mut dyn ArtifactPurger,
    ) -> Result<usize, ObligationsRefused> {
        if self.fail_stopped() {
            return Err(ObligationsRefused::HolderStateUnwritable);
        }
        let pending: Vec<(String, crate::store::ManifestUnits)> = self
            .writer
            .state()
            .status
            .instructions
            .iter()
            .filter(|(_, r)| r.step == ReleaseStep::Reserving)
            .map(|(id, r)| (id.clone(), r.units.clone()))
            .collect();
        let mut confirmed = 0;
        for (id, units) in pending {
            if self.fail_stopped() {
                return Err(ObligationsRefused::HolderStateUnwritable);
            }
            match purger.reserve(&id, &units) {
                Ok(ReserveOutcome::Reserved) => {
                    if self.update_record(&id, |r| r.step = ReleaseStep::Draining) {
                        confirmed += 1;
                        self.events.emit(HolderEvent::ReleaseReserved);
                    }
                }
                Ok(ReserveOutcome::Mismatch { unattributed }) => {
                    let digest =
                        manifest_digest(&self.writer.enrollment().holder_id, &unattributed);
                    if self.update_record(&id, |r| {
                        r.step = ReleaseStep::Mismatch;
                        r.units.clear();
                        r.observed_manifest_digest = Some(digest);
                    }) {
                        self.events.emit(HolderEvent::ReleaseMismatch);
                    }
                }
                Err(_) => self
                    .events
                    .emit(HolderEvent::ReleaseStepFailed { step: "reserve" }),
            }
            if self.fail_stopped() {
                return Err(ObligationsRefused::HolderStateUnwritable);
            }
        }
        Ok(confirmed)
    }

    /// One release pass: check every newly listed instruction against the
    /// census (L1, L2), then drain every reserved release (L4, L5).
    pub fn release_once(
        &mut self,
        purger: &mut dyn ArtifactPurger,
    ) -> Result<ReleaseSummary, ObligationsRefused> {
        self.deletion_permitted()?;
        self.honor_reservations(purger)?;
        let mut summary = ReleaseSummary::default();
        let listed: Vec<Instruction> = self
            .applied
            .as_ref()
            .map(|a| a.instructions.clone())
            .unwrap_or_default();
        for instruction in listed {
            self.deletion_permitted()?;
            if self.release_list_rejected {
                break;
            }
            if self
                .writer
                .state()
                .status
                .instructions
                .contains_key(&instruction.instruction_id)
            {
                // L10, L11: seen before; never evaluated again.
                continue;
            }
            let Ok(census) = purger.census() else {
                self.events
                    .emit(HolderEvent::ReleaseStepFailed { step: "census" });
                return Ok(summary);
            };
            let Some(record) = self.evaluate(&instruction, &census) else {
                continue;
            };
            let step = record.step;
            // A response delivered during census/manifest reads must win over
            // the first durable reservation. A later refusal may stop new
            // evaluations but cannot undo a reservation already committed.
            let write = self.with_deletion_permission(|holder| {
                if holder.release_list_rejected {
                    return None;
                }
                let mut next = holder.writer.state().status.clone();
                next.instructions
                    .insert(instruction.instruction_id.clone(), record);
                Some(holder.writer.update_lifecycle(None, Some(next), false))
            })?;
            let Some(write) = write else { break };
            if let Err(error) = write {
                crate::holder::report_write_failure(self.events.as_ref(), &error);
                return Ok(summary);
            }
            summary.evaluated += 1;
            if step == ReleaseStep::Reserving {
                self.honor_reservations(purger)?;
                match self.writer.state().status.instructions[&instruction.instruction_id].step {
                    ReleaseStep::Draining => summary.reserved += 1,
                    ReleaseStep::Mismatch => summary.mismatched += 1,
                    _ => {}
                }
            } else {
                self.events.emit(HolderEvent::ReleaseMismatch);
                summary.mismatched += 1;
            }
        }
        let draining: Vec<String> = self
            .writer
            .state()
            .status
            .instructions
            .iter()
            .filter(|(_, r)| r.step == ReleaseStep::Draining)
            .map(|(id, _)| id.clone())
            .collect();
        for id in draining {
            self.drain(purger, &id, &mut summary)?;
        }
        self.emit_reserve_uses();
        Ok(summary)
    }

    /// The manifest this holder stored for the report whose exact bytes
    /// hash to `digest`, if it still has it and it is intact.
    pub(crate) fn retained_manifest(
        &self,
        digest: &str,
    ) -> std::io::Result<Option<crate::store::ManifestUnits>> {
        let Some(reference) = self.writer.state().status.manifests.get(digest) else {
            return Ok(None);
        };
        let Some(bytes) = self
            .writer
            .dir()
            .read(&format!("manifest.{}.json", reference.key))?
        else {
            return Ok(None);
        };
        let holder_id = &self.writer.enrollment().holder_id;
        // Closed and canonical: old id-only manifests are not deletion authority.
        Ok(crate::strict_json::parse(&bytes).ok().and_then(|value| {
            let units = serde_json::from_value(value.get("units")?.clone()).ok()?;
            (crate::obligations::manifest_bytes(holder_id, &units) == bytes
                && manifest_digest(holder_id, &units) == reference.manifest_digest)
                .then_some(units)
        }))
    }

    /// Prepare L1 or L2 for one unseen instruction without authorizing it.
    /// A temporarily unreadable manifest is retried on the next pass.
    fn evaluate(&self, instruction: &Instruction, census: &Census) -> Option<InstructionRecord> {
        let status = &self.writer.state().status;
        let held = status.release_held_units();
        let inventory = Inventory::with_held(census, &self.plan(), &held);
        let observed: BTreeMap<&str, bool> = census
            .units
            .iter()
            .map(|u| (u.unit_id.as_str(), u.reserved))
            .collect();
        let manifest = match self.retained_manifest(&instruction.bound_report_digest) {
            Ok(manifest) => manifest,
            Err(_) => {
                self.events.emit(HolderEvent::ReleaseStepFailed {
                    step: "manifest_read",
                });
                return None;
            }
        };
        let exact = manifest.as_ref().filter(|units| {
            u64::try_from(units.len()).is_ok_and(|n| n == instruction.approved_count)
                && units.iter().all(|(unit, attribution)| {
                    inventory.unattributed.get(unit) == Some(attribution)
                        && observed.get(unit.as_str()) == Some(&false)
                        && !held.contains(unit)
                })
        });
        let order = status
            .instructions
            .values()
            .map(|r| r.order + 1)
            .max()
            .unwrap_or(0);
        let holder_id = self.writer.enrollment().holder_id.clone();
        let (step, units, observed_digest) = match exact {
            Some(units) => (ReleaseStep::Reserving, units.clone(), None),
            None => (
                ReleaseStep::Mismatch,
                Default::default(),
                Some(manifest_digest(&holder_id, &inventory.unattributed)),
            ),
        };
        Some(InstructionRecord {
            bound_report_digest: instruction.bound_report_digest.clone(),
            approved_count: instruction.approved_count,
            step,
            units,
            claimed: BTreeSet::new(),
            deleted: BTreeSet::new(),
            observed_manifest_digest: observed_digest,
            carried_epoch: None,
            order,
        })
    }

    fn record(&self, id: &str) -> Option<InstructionRecord> {
        self.writer.state().status.instructions.get(id).cloned()
    }

    fn update_record(&mut self, id: &str, change: impl FnOnce(&mut InstructionRecord)) -> bool {
        let mut next: StatusBook = self.writer.state().status.clone();
        let Some(record) = next.instructions.get_mut(id) else {
            return false;
        };
        change(record);
        self.commit_status(next)
    }

    /// One deletion pass for a reserved release, then its verification.
    fn drain(
        &mut self,
        purger: &mut dyn ArtifactPurger,
        id: &str,
        summary: &mut ReleaseSummary,
    ) -> Result<(), ObligationsRefused> {
        self.deletion_permitted()?;
        let Some(record) = self.record(id) else {
            return Ok(());
        };
        let Ok(census) = purger.census() else {
            self.events
                .emit(HolderEvent::ReleaseStepFailed { step: "census" });
            return Ok(());
        };
        let observed: BTreeMap<&str, u32> = census
            .units
            .iter()
            .filter(|u| record.units.contains_key(&u.unit_id))
            .map(|u| (u.unit_id.as_str(), u.open_handles))
            .collect();
        // A claim recorded before a crash: units no longer observed are gone.
        let recovered: BTreeSet<String> = record
            .claimed
            .iter()
            .filter(|unit| !observed.contains_key(unit.as_str()))
            .cloned()
            .collect();
        let busy = observed.values().filter(|h| **h > 0).count();
        if busy > 0 {
            summary.waiting_on_handles += busy;
            self.events
                .emit(HolderEvent::PurgeWaitingOnHandles { count: busy });
        }
        let claims: BTreeSet<String> = observed
            .iter()
            .filter(|(_, handles)| **handles == 0)
            .map(|(unit, _)| (*unit).to_owned())
            .collect();
        if !claims.is_empty() || !recovered.is_empty() {
            // Before: the exact claim.
            let claimed = claims.clone();
            if !self.update_record(id, |r| {
                r.deleted.extend(recovered);
                r.claimed = claimed;
            }) {
                return Ok(());
            }
            let claimant = Claimant::Release {
                instruction_id: id.to_owned(),
            };
            let mut deleted = BTreeSet::new();
            let mut failed = 0;
            for unit in &claims {
                // H.45: nothing runs after a fail-stop, deletions included.
                match self.with_deletion_permission(|_| purger.delete(unit, &claimant))? {
                    Ok(DeleteOutcome::Deleted | DeleteOutcome::Absent) => {
                        deleted.insert(unit.clone());
                    }
                    Ok(DeleteOutcome::Busy) => summary.waiting_on_handles += 1,
                    Ok(DeleteOutcome::Failed) => failed += 1,
                    Err(_) => {
                        // The claim stays durable; the next pass re-checks.
                        self.events
                            .emit(HolderEvent::ReleaseStepFailed { step: "delete" });
                        return Ok(());
                    }
                }
            }
            if failed > 0 {
                summary.failed += failed;
                self.events
                    .emit(HolderEvent::PurgeDeleteFailed { count: failed });
            }
            let count = deleted.len();
            let mut next = self.writer.state().status.clone();
            let Some(record) = next.instructions.get_mut(id) else {
                return Ok(());
            };
            record.deleted.extend(deleted);
            record.claimed.clear();
            if self
                .commit_books_then(None, Some(next), false, |holder| {
                    summary.deleted += count;
                    if count > 0 {
                        holder.writer.replenish_reserve();
                    }
                })
                .is_none()
            {
                return Ok(());
            }
        }
        // Verify: a barrier, then an independent census that observes none
        // of the reserved units (an open descriptor counts as present).
        if purger.barrier().is_err() {
            self.events
                .emit(HolderEvent::ReleaseStepFailed { step: "barrier" });
            return Ok(());
        }
        let Ok(census) = purger.census() else {
            self.events
                .emit(HolderEvent::ReleaseStepFailed { step: "census" });
            return Ok(());
        };
        if census
            .units
            .iter()
            .any(|u| record.units.contains_key(&u.unit_id))
        {
            return Ok(());
        }
        let mut held = self.writer.state().status.release_held_units();
        for unit in record.units.keys() {
            held.remove(unit);
        }
        let inventory = Inventory::with_held(&census, &self.plan(), &held);
        let holder_id = self.writer.enrollment().holder_id.clone();
        let observed_digest = manifest_digest(&holder_id, &inventory.unattributed);
        let all = record.units.into_keys();
        if self.update_record(id, |r| {
            r.step = ReleaseStep::Deleted;
            r.deleted.extend(all);
            r.claimed.clear();
            r.observed_manifest_digest = Some(observed_digest);
        }) {
            summary.executed += 1;
            self.events.emit(HolderEvent::ReleaseExecuted);
        }
        Ok(())
    }
}
