//! Disk reserve and storage budget (v2 §C.0 *storage exhaustion*; Annex K.1
//! K2, K2x, K3, K4).
//!
//! - **Reserve.** Preallocated space that a durable lifecycle write may
//!   consume when the volume is full: on `ENOSPC` the writer releases the
//!   bytes it needs from the reserve and retries once (`disk_reserve_used`).
//!   If the reserve holds less, or the retry fails too, the calling step
//!   fails with no state change (`disk_reserve_exhausted`). The reserve is
//!   refilled as soon as space allows.
//! - **Budget.** New artifact writes are admitted only while every retained
//!   byte, the estimate and the reserve targets fit. A refusal alerts
//!   (`disk_pressure`) and stops the write. It never evicts, quarantines or
//!   deletes anything: protected data stays exactly where it is.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReserveLevel {
    Full,
    /// Some of the target is missing (consumed, or not yet refilled).
    Partial,
}

/// Space set aside for lifecycle writes.
pub trait DiskReserve: Send {
    /// Frees at least `bytes` and makes that durable. `Ok(false)` when the
    /// reserve holds less than `bytes`; nothing is freed then.
    fn release(&mut self, bytes: u64) -> io::Result<bool>;
    /// Refills up to the target as far as space allows.
    fn replenish(&mut self) -> io::Result<ReserveLevel>;
}

/// A reserve kept as one preallocated file (`.reserve`).
pub struct FileReserve {
    path: PathBuf,
    target: u64,
}

impl FileReserve {
    /// Opens or creates the reserve file and preallocates what fits.
    pub fn open(path: PathBuf, target: u64) -> io::Result<(Self, ReserveLevel)> {
        let mut reserve = Self { path, target };
        let level = reserve.replenish()?;
        Ok((reserve, level))
    }

    fn file(&self) -> io::Result<File> {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.path)
    }

    pub fn len(&self) -> io::Result<u64> {
        Ok(self.file()?.metadata()?.len())
    }

    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }
}

impl DiskReserve for FileReserve {
    fn release(&mut self, bytes: u64) -> io::Result<bool> {
        let file = self.file()?;
        let len = file.metadata()?.len();
        if len < bytes {
            return Ok(false);
        }
        file.set_len(len - bytes)?;
        file.sync_all()?;
        Ok(true)
    }

    fn replenish(&mut self) -> io::Result<ReserveLevel> {
        let file = self.file()?;
        match fs2::FileExt::allocate(&file, self.target) {
            Ok(()) => {
                file.sync_all()?;
                Ok(ReserveLevel::Full)
            }
            Err(error) if error.kind() == io::ErrorKind::StorageFull => {
                file.sync_all()?;
                Ok(ReserveLevel::Partial)
            }
            Err(error) => Err(error),
        }
    }
}

/// Bytes a holder retains, by kind, when a new artifact write asks to start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StorageUsage {
    /// Servable, withheld, quarantined and staging bytes: everything retained.
    pub retained_bytes: u64,
    /// The new write's estimate.
    pub estimate_bytes: u64,
    pub free_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageBudget {
    pub max_bytes: u64,
    /// Sum of every reserve target.
    pub reserve_bytes: u64,
}

impl StorageBudget {
    /// K3: whether a new artifact write may start. The answer never names
    /// anything to delete.
    pub fn admits(&self, usage: StorageUsage) -> bool {
        let needed = usage
            .retained_bytes
            .saturating_add(usage.estimate_bytes)
            .saturating_add(self.reserve_bytes);
        // Retained bytes and preallocated reserves already occupy the volume;
        // only the new write needs free space.
        needed <= self.max_bytes && usage.estimate_bytes <= usage.free_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_reserve_releases_only_what_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let (mut reserve, level) = FileReserve::open(dir.path().join(".reserve"), 4096).unwrap();
        assert_eq!(level, ReserveLevel::Full);
        assert_eq!(reserve.len().unwrap(), 4096);
        assert!(reserve.release(1000).unwrap());
        assert_eq!(reserve.len().unwrap(), 3096);
        assert!(!reserve.release(5000).unwrap());
        assert_eq!(reserve.len().unwrap(), 3096);
        assert_eq!(reserve.replenish().unwrap(), ReserveLevel::Full);
        assert_eq!(reserve.len().unwrap(), 4096);
    }

    #[test]
    fn the_budget_refuses_without_naming_anything_to_evict() {
        let budget = StorageBudget {
            max_bytes: 1000,
            reserve_bytes: 100,
        };
        let fits = StorageUsage {
            retained_bytes: 700,
            estimate_bytes: 200,
            free_bytes: 10_000,
        };
        assert!(budget.admits(fits));
        assert!(!budget.admits(StorageUsage {
            estimate_bytes: 201,
            ..fits
        }));
        assert!(!budget.admits(StorageUsage {
            free_bytes: 199,
            ..fits
        }));
    }
}
