//! Fake clock, store, data root and I/O faults.

use crate::clock::Clock;
use crate::reserve::{DiskReserve, ReserveLevel};
use crate::startup::{ContentState, DataRoot, DataRootEvidence, MarkerState};
use crate::state::{DurableIo, FsIo, WriteFailure};
use crate::store::{
    ArtifactPurger, Census, Claimant, DeleteOutcome, HeldResource, ObservedUnit, PublicationFreeze,
    PublishPlan, ReserveOutcome, ResourceKey, ResourceStore, StoreError,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// A clock the test moves. Wall and monotonic advance together unless the
/// wall clock is stepped on its own.
pub struct FakeClock {
    wall: AtomicI64,
    mono: AtomicU64,
}

impl FakeClock {
    pub fn new(wall_ms: i64, mono_ms: u64) -> Self {
        Self {
            wall: AtomicI64::new(wall_ms),
            mono: AtomicU64::new(mono_ms),
        }
    }

    pub fn advance(&self, ms: u64) {
        self.mono.fetch_add(ms, Ordering::SeqCst);
        self.wall
            .fetch_add(i64::try_from(ms).unwrap_or(i64::MAX), Ordering::SeqCst);
    }

    /// Steps only the wall clock (an NTP step, a wrong clock).
    pub fn step_wall(&self, delta_ms: i64) {
        self.wall.fetch_add(delta_ms, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn wall_ms(&self) -> i64 {
        self.wall.load(Ordering::SeqCst)
    }

    fn mono_ms(&self) -> u64 {
        self.mono.load(Ordering::SeqCst)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldEntry {
    pub configured: bool,
    pub units: u64,
    /// Whether the resource's bytes still exist. Nothing in the holder can
    /// clear this: the store exposes no deletion.
    pub bytes_present: bool,
}

#[derive(Default)]
pub struct StoreView {
    pub held: BTreeMap<ResourceKey, HeldEntry>,
    pub published: Vec<PublishPlan>,
    pub revoked: Vec<PublishPlan>,
    pub fail_publish: bool,
    pub fail_held: bool,
}

/// An in-memory engine. It records what the holder told it and never
/// deletes anything.
#[derive(Clone, Default)]
pub struct MemoryStore(pub Arc<Mutex<StoreView>>);

impl MemoryStore {
    pub fn view(&self) -> MutexGuard<'_, StoreView> {
        lock(&self.0)
    }

    pub fn hold(&self, kind: &str, id: &str, configured: bool, units: u64) {
        self.view().held.insert(
            ResourceKey {
                kind: kind.into(),
                id: id.into(),
            },
            HeldEntry {
                configured,
                units,
                bytes_present: true,
            },
        );
    }

    pub fn bytes_present(&self, kind: &str, id: &str) -> bool {
        self.view()
            .held
            .get(&ResourceKey {
                kind: kind.into(),
                id: id.into(),
            })
            .is_some_and(|e| e.bytes_present)
    }

    pub fn last_plan(&self) -> Option<PublishPlan> {
        self.view().published.last().cloned()
    }
}

impl ResourceStore for MemoryStore {
    fn held(&self) -> Result<Vec<HeldResource>, StoreError> {
        let view = self.view();
        if view.fail_held {
            return Err(StoreError("injected".into()));
        }
        Ok(view
            .held
            .iter()
            .map(|(key, entry)| HeldResource {
                key: key.clone(),
                configured: entry.configured,
                units: entry.units,
            })
            .collect())
    }

    fn publish(&mut self, plan: &PublishPlan) -> Result<(), StoreError> {
        let mut view = self.view();
        if view.fail_publish {
            return Err(StoreError("injected disk full".into()));
        }
        view.published.push(plan.clone());
        Ok(())
    }

    fn revoke(&mut self, plan: &PublishPlan) {
        self.view().revoked.push(plan.clone());
    }
}

#[derive(Clone, Debug)]
pub struct DataRootView {
    pub content: ContentState,
    /// The holder id in the root marker, if any.
    pub marker: Option<String>,
    pub marker_unreadable: bool,
    pub binds: u32,
}

impl Default for DataRootView {
    fn default() -> Self {
        Self {
            content: ContentState::Lost,
            marker: None,
            marker_unreadable: false,
            binds: 0,
        }
    }
}

#[derive(Clone, Default)]
pub struct FakeDataRoot(pub Arc<Mutex<DataRootView>>);

impl FakeDataRoot {
    pub fn view(&self) -> MutexGuard<'_, DataRootView> {
        lock(&self.0)
    }
}

impl DataRoot for FakeDataRoot {
    fn inspect(&self, holder_id: &str) -> Result<DataRootEvidence, String> {
        let view = self.view();
        let marker = match (&view.marker, view.marker_unreadable) {
            (_, true) => MarkerState::Unreadable,
            (None, false) => MarkerState::Absent,
            (Some(id), false) if id == holder_id => MarkerState::Matches,
            (Some(_), false) => MarkerState::Other,
        };
        Ok(DataRootEvidence {
            content: view.content,
            marker,
        })
    }

    fn bind(&self, holder_id: &str) -> Result<(), String> {
        let mut view = self.view();
        match &view.marker {
            Some(existing) if existing != holder_id => Err("bound to another holder".into()),
            _ => {
                view.marker = Some(holder_id.to_owned());
                view.binds += 1;
                Ok(())
            }
        }
    }
}

/// One planned outcome of an upcoming holder-file write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The write succeeds.
    Pass,
    /// The write fails before its rename (`ENOSPC`); nothing changes on disk.
    BeforeRename,
    /// The rename happens, then the directory fsync fails: the file holds the
    /// new bytes but the write reports failure.
    AfterRename,
}

/// Shared plan of faults for upcoming holder-file writes, in order.
#[derive(Clone, Default)]
pub struct IoFaults {
    plan: Arc<Mutex<std::collections::VecDeque<Fault>>>,
    fail_removes: Arc<std::sync::atomic::AtomicBool>,
}

impl IoFaults {
    /// The next `count` writes fail before their rename.
    pub fn fail_writes(&self, count: u32) {
        self.plan(vec![Fault::BeforeRename; count as usize]);
    }

    /// Appends outcomes for the upcoming writes.
    pub fn plan(&self, faults: Vec<Fault>) {
        lock(&self.plan).extend(faults);
    }

    pub fn clear(&self) {
        lock(&self.plan).clear();
        self.fail_removes.store(false, Ordering::SeqCst);
    }

    /// Makes every file removal fail until cleared.
    pub fn fail_removes(&self) {
        self.fail_removes.store(true, Ordering::SeqCst);
    }

    pub fn io(&self) -> FaultyIo {
        FaultyIo {
            faults: self.clone(),
        }
    }
}

pub struct FaultyIo {
    faults: IoFaults,
}

impl DurableIo for FaultyIo {
    fn write_atomic(&self, dir: &Path, name: &str, bytes: &[u8]) -> Result<(), WriteFailure> {
        let fault = lock(&self.faults.plan).pop_front().unwrap_or(Fault::Pass);
        match fault {
            Fault::Pass => FsIo.write_atomic(dir, name, bytes),
            Fault::BeforeRename => Err(WriteFailure::before_rename(io::Error::new(
                io::ErrorKind::StorageFull,
                "injected ENOSPC",
            ))),
            Fault::AfterRename => {
                FsIo.write_atomic(dir, name, bytes)?;
                Err(WriteFailure {
                    replaced: true,
                    error: io::Error::other("injected directory fsync failure"),
                })
            }
        }
    }

    fn remove(&self, dir: &Path, name: &str) -> io::Result<()> {
        if self.faults.fail_removes.load(Ordering::SeqCst) {
            return Err(io::Error::other("injected unlink failure"));
        }
        FsIo.remove(dir, name)
    }
}

/// One unit of synthetic retained data in [`MemoryPurger`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PurgerUnit {
    pub attribution: Option<ResourceKey>,
    pub open_handles: u32,
    pub reserved: bool,
    /// Linked in its storage location. A unit that is unlinked but still
    /// open stays visible to the census through its descriptor.
    pub present: bool,
}

#[derive(Default)]
pub struct PurgerView {
    pub units: BTreeMap<String, PurgerUnit>,
    pub detached: BTreeSet<ResourceKey>,
    /// Every unit deleted, with the obligation that claimed it.
    pub deletions: Vec<(String, String)>,
    /// Every deletion the purger refused, with its reason.
    pub refused: Vec<(String, &'static str)>,
    pub fail_delete: BTreeSet<String>,
    pub fail_detach: bool,
    pub fail_barrier: bool,
    pub fail_census: bool,
    /// Builder outputs published once the census has run this many times:
    /// `(after_census_calls, unit_id, unit)`.
    pub late: Vec<(usize, String, PurgerUnit)>,
    pub census_calls: usize,
    /// Simulate time spent scanning under the first-byte publication freeze.
    pub frozen_census_delay: Option<(Arc<FakeClock>, u64)>,
    pub barriers: usize,
    /// Stops (as a crashing process would) once this many units were
    /// deleted: every later delete fails until cleared.
    pub crash_after_deletes: Option<usize>,
    /// Freezes currently held; publications wait while it is non-zero.
    pub frozen: usize,
    /// Publications that arrived during a freeze, applied when it ends.
    pub waiting: Vec<(String, PurgerUnit)>,
    /// Every publication, with whether a freeze held it back.
    pub publications: Vec<(String, bool)>,
    /// `freeze_publications` refuses.
    pub fail_freeze: bool,
    /// Every purger call in order: `detach`, `delete`, `barrier`, `census`,
    /// `reserve`, `freeze`.
    pub calls: Vec<&'static str>,
    /// Units reserved for a release, with its instruction id. Durable in a
    /// real adapter: it survives a holder restart here too.
    pub reservations: BTreeMap<String, String>,
    /// `reserve` refuses.
    pub fail_reserve: bool,
}

/// TEST-ONLY purger over synthetic units. It checks every claim itself, as
/// a real adapter must: a unit is deleted only for a claimant whose resource
/// it belongs to, only after that resource was detached, never while a
/// handle is open and never when a release reserved it.
#[derive(Clone, Default)]
pub struct MemoryPurger(Arc<Mutex<PurgerView>>, MemoryStore);

impl MemoryPurger {
    pub fn for_store(store: &MemoryStore) -> Self {
        Self(Default::default(), store.clone())
    }

    pub fn view(&self) -> MutexGuard<'_, PurgerView> {
        lock(&self.0)
    }

    /// Adds `count` units attributed to `(kind, id)`; returns their ids.
    pub fn add(&self, kind: &str, id: &str, count: usize) -> Vec<String> {
        let mut view = self.view();
        (0..count)
            .map(|_| {
                let unit_id = format!("u{:04}", view.units.len());
                view.units.insert(
                    unit_id.clone(),
                    PurgerUnit {
                        attribution: Some(ResourceKey {
                            kind: kind.into(),
                            id: id.into(),
                        }),
                        open_handles: 0,
                        reserved: false,
                        present: true,
                    },
                );
                unit_id
            })
            .collect()
    }

    /// Adds a unit the census cannot attribute to any resource.
    pub fn add_unattributable(&self) -> String {
        let mut view = self.view();
        let unit_id = format!("u{:04}", view.units.len());
        view.units.insert(
            unit_id.clone(),
            PurgerUnit {
                attribution: None,
                open_handles: 0,
                reserved: false,
                present: true,
            },
        );
        unit_id
    }

    pub fn present(&self, unit_id: &str) -> bool {
        self.view().units.get(unit_id).is_some_and(|u| u.present)
    }

    pub fn present_of(&self, kind: &str, id: &str) -> usize {
        let key = ResourceKey {
            kind: kind.into(),
            id: id.into(),
        };
        self.view()
            .units
            .values()
            .filter(|u| u.present && u.attribution.as_ref() == Some(&key))
            .count()
    }

    /// A builder publishes `unit` now. While a freeze is held it waits
    /// (the shared side of the publication lock) and becomes visible only
    /// when the freeze ends.
    pub fn publish(&self, unit_id: &str, unit: PurgerUnit) {
        let mut view = self.view();
        let held = view.frozen > 0;
        view.publications.push((unit_id.to_owned(), held));
        if held {
            view.waiting.push((unit_id.to_owned(), unit));
        } else {
            view.units.insert(unit_id.to_owned(), unit);
        }
    }

    pub fn deleted(&self) -> Vec<String> {
        self.view()
            .deletions
            .iter()
            .map(|(u, _)| u.clone())
            .collect()
    }
}

impl ArtifactPurger for MemoryPurger {
    fn detach(&mut self, resource: &ResourceKey) -> Result<(), StoreError> {
        let mut view = self.view();
        view.calls.push("detach");
        if view.fail_detach {
            return Err(StoreError("injected detach failure".into()));
        }
        view.detached.insert(resource.clone());
        Ok(())
    }

    fn delete(&mut self, unit_id: &str, claimant: &Claimant) -> Result<DeleteOutcome, StoreError> {
        let mut view = self.view();
        view.calls.push("delete");
        let Some(unit) = view.units.get(unit_id).cloned() else {
            return Ok(DeleteOutcome::Absent);
        };
        if !unit.present {
            return Ok(DeleteOutcome::Absent);
        }
        let refusal = match claimant {
            Claimant::Obligation { resource, .. } => {
                if unit.attribution.as_ref() != Some(resource) {
                    Some("claimant does not own the unit")
                } else if unit.reserved {
                    Some("reserved by a release")
                } else if !view.detached.contains(resource) {
                    Some("resource not detached")
                } else {
                    None
                }
            }
            Claimant::Release { instruction_id } => {
                if !unit.reserved || view.reservations.get(unit_id) != Some(instruction_id) {
                    Some("not reserved by this release")
                } else {
                    None
                }
            }
        };
        if let Some(reason) = refusal {
            view.refused.push((unit_id.to_owned(), reason));
            return Err(StoreError(reason.into()));
        }
        if view
            .crash_after_deletes
            .is_some_and(|limit| view.deletions.len() >= limit)
        {
            return Err(StoreError("injected crash".into()));
        }
        if unit.open_handles > 0 {
            return Ok(DeleteOutcome::Busy);
        }
        if view.fail_delete.contains(unit_id) {
            return Ok(DeleteOutcome::Failed);
        }
        let by = match claimant {
            Claimant::Obligation { obligation_id, .. } => obligation_id.clone(),
            Claimant::Release { instruction_id } => instruction_id.clone(),
        };
        view.deletions.push((unit_id.to_owned(), by));
        if let Some(stored) = view.units.get_mut(unit_id) {
            stored.present = false;
        }
        Ok(DeleteOutcome::Deleted)
    }

    fn reserve(
        &mut self,
        instruction_id: &str,
        units: &BTreeSet<String>,
    ) -> Result<ReserveOutcome, StoreError> {
        // Hold both the publication/classification state and the physical
        // inventory until the entire reservation is visible.
        let store = self.1.view();
        let mut view = self.view();
        view.calls.push("reserve");
        if view.fail_reserve || view.fail_census {
            return Err(StoreError("injected reserve failure".into()));
        }
        let plan = store
            .published
            .last()
            .ok_or_else(|| StoreError("classification unavailable".into()))?;
        let unattributed = crate::purge::Inventory::new(&view.snapshot(), plan).unattributed;
        let owned: BTreeSet<_> = view
            .reservations
            .iter()
            .filter(|(_, id)| *id == instruction_id)
            .map(|(unit, _)| unit.clone())
            .collect();
        if !owned.is_empty() && owned == *units {
            return Ok(ReserveOutcome::Reserved);
        }
        if !owned.is_empty()
            || !units.is_subset(&unattributed)
            || units
                .iter()
                .any(|unit| view.units[unit].reserved || view.reservations.contains_key(unit))
        {
            return Ok(ReserveOutcome::Mismatch { unattributed });
        }
        for unit in units {
            view.reservations
                .insert(unit.clone(), instruction_id.to_owned());
            view.units
                .get_mut(unit)
                .expect("validated under the lock")
                .reserved = true;
        }
        Ok(ReserveOutcome::Reserved)
    }

    fn barrier(&mut self) -> Result<(), StoreError> {
        let mut view = self.view();
        view.calls.push("barrier");
        view.barriers += 1;
        if view.fail_barrier {
            return Err(StoreError("injected barrier failure".into()));
        }
        Ok(())
    }

    fn freeze_publications(&self) -> Result<PublicationFreeze<'_>, StoreError> {
        let mut view = self.view();
        view.calls.push("freeze");
        if view.fail_freeze {
            return Err(StoreError("injected freeze refusal".into()));
        }
        view.frozen += 1;
        drop(view);
        Ok(PublicationFreeze::new(Thaw(self.clone())))
    }

    fn census(&self) -> Result<Census, StoreError> {
        let mut view = self.view();
        view.calls.push("census");
        view.census_calls += 1;
        if view.frozen > 0
            && let Some((clock, delay)) = &view.frozen_census_delay
        {
            clock.advance(*delay);
        }
        if view.fail_census {
            return Err(StoreError("injected census failure".into()));
        }
        let calls = view.census_calls;
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut view.late)
            .into_iter()
            .partition(|(n, _, _)| *n < calls);
        view.late = later;
        for (_, unit_id, unit) in due {
            view.units.insert(unit_id, unit);
        }
        Ok(view.snapshot())
    }
}

impl PurgerView {
    fn snapshot(&self) -> Census {
        Census {
            locations: vec!["objects".into(), "staging".into(), "quarantine".into()],
            units: self
                .units
                .iter()
                .filter(|(_, u)| u.present || u.open_handles > 0)
                .map(|(unit_id, u)| ObservedUnit {
                    unit_id: unit_id.clone(),
                    attribution: u.attribution.clone(),
                    open_handles: u.open_handles,
                    reserved: u.reserved,
                })
                .collect(),
        }
    }
}

/// The fake's exclusive publication lock: dropping it lets waiting
/// publications through.
struct Thaw(MemoryPurger);

impl Drop for Thaw {
    fn drop(&mut self) {
        let mut view = self.0.view();
        view.frozen = view.frozen.saturating_sub(1);
        if view.frozen == 0 {
            for (unit_id, unit) in std::mem::take(&mut view.waiting) {
                view.units.insert(unit_id, unit);
            }
        }
    }
}

/// TEST-ONLY disk reserve holding a byte count.
#[derive(Clone, Default)]
pub struct FakeReserve(pub Arc<Mutex<u64>>);

impl FakeReserve {
    pub fn with(bytes: u64) -> Self {
        Self(Arc::new(Mutex::new(bytes)))
    }

    pub fn bytes(&self) -> u64 {
        *lock(&self.0)
    }
}

impl DiskReserve for FakeReserve {
    fn release(&mut self, bytes: u64) -> io::Result<bool> {
        let mut held = lock(&self.0);
        if *held < bytes {
            return Ok(false);
        }
        *held -= bytes;
        Ok(true)
    }

    fn replenish(&mut self) -> io::Result<ReserveLevel> {
        Ok(ReserveLevel::Full)
    }
}
