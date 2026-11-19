//! Fake clock, store, data root and I/O faults.

use crate::clock::Clock;
use crate::startup::{ContentState, DataRoot, DataRootEvidence, MarkerState};
use crate::state::{DurableIo, FsIo};
use crate::store::{HeldResource, PublishPlan, ResourceKey, ResourceStore, StoreError};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
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

/// Shared switches that make holder-file writes fail.
#[derive(Clone, Default)]
pub struct IoFaults {
    /// Number of upcoming writes to fail.
    pub fail_next: Arc<AtomicU32>,
}

impl IoFaults {
    pub fn fail_writes(&self, count: u32) {
        self.fail_next.store(count, Ordering::SeqCst);
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
    fn write_atomic(&self, dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
        let failing = self
            .faults
            .fail_next
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if failing {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "injected ENOSPC",
            ));
        }
        FsIo.write_atomic(dir, name, bytes)
    }
}
