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

use std::io;

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

#[cfg(unix)]
pub use file_reserve::FileReserve;

#[cfg(unix)]
mod file_reserve {
    use super::{DiskReserve, ReserveLevel};
    use std::fs::{self, File, OpenOptions};
    use std::io;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::Path;

    /// A reserve kept as one preallocated file, bound to the descriptor it
    /// was created with.
    ///
    /// The path is used once, at [`FileReserve::open`]. Whatever was at it
    /// (an earlier run's reserve, a link, anything else) is unlinked, which
    /// never follows a link and never changes another file's data. The
    /// reserve is then created exclusively with `O_NOFOLLOW`, mode 0600, and
    /// its identity checked: a regular file with one link, and the path
    /// still names that same inode. Every later release and refill acts on
    /// that descriptor (`ftruncate`, `fallocate`), never on the path, so a
    /// path swapped for a link to other data afterwards cannot be truncated.
    pub struct FileReserve {
        file: File,
        target: u64,
    }

    impl FileReserve {
        pub fn open(path: &Path, target: u64) -> io::Result<(Self, ReserveLevel)> {
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.is_dir() => {
                    return Err(io::Error::other("the reserve path is a directory"));
                }
                Ok(_) => fs::remove_file(path)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)?;
            let bound = file.metadata()?;
            let named = fs::symlink_metadata(path)?;
            if !bound.file_type().is_file()
                || bound.nlink() != 1
                || bound.dev() != named.dev()
                || bound.ino() != named.ino()
            {
                return Err(io::Error::other("the reserve file is not the one created"));
            }
            let mut reserve = Self { file, target };
            let level = reserve.replenish()?;
            Ok((reserve, level))
        }

        /// Bytes currently held, read from the bound descriptor.
        pub fn len(&self) -> io::Result<u64> {
            Ok(self.file.metadata()?.len())
        }

        pub fn is_empty(&self) -> io::Result<bool> {
            Ok(self.len()? == 0)
        }
    }

    impl DiskReserve for FileReserve {
        fn release(&mut self, bytes: u64) -> io::Result<bool> {
            let len = self.file.metadata()?.len();
            if len < bytes {
                return Ok(false);
            }
            self.file.set_len(len - bytes)?;
            self.file.sync_all()?;
            Ok(true)
        }

        fn replenish(&mut self) -> io::Result<ReserveLevel> {
            match fs2::FileExt::allocate(&self.file, self.target) {
                Ok(()) => {
                    self.file.sync_all()?;
                    Ok(ReserveLevel::Full)
                }
                Err(error) if error.kind() == io::ErrorKind::StorageFull => {
                    self.file.sync_all()?;
                    Ok(ReserveLevel::Partial)
                }
                Err(error) => Err(error),
            }
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

    #[cfg(unix)]
    #[test]
    fn file_reserve_releases_only_what_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let (mut reserve, level) = FileReserve::open(&dir.path().join(".reserve"), 4096).unwrap();
        assert_eq!(level, ReserveLevel::Full);
        assert_eq!(reserve.len().unwrap(), 4096);
        assert!(reserve.release(1000).unwrap());
        assert_eq!(reserve.len().unwrap(), 3096);
        assert!(!reserve.release(5000).unwrap());
        assert_eq!(reserve.len().unwrap(), 3096);
        assert_eq!(reserve.replenish().unwrap(), ReserveLevel::Full);
        assert_eq!(reserve.len().unwrap(), 4096);
    }

    /// Review finding 2: a reserve path that is, or later becomes, a link to
    /// other data never lets a release or refill touch that data.
    #[cfg(unix)]
    #[test]
    fn a_reserve_never_follows_a_link_and_releases_only_its_own_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"precious operator data").unwrap();
        let path = dir.path().join(".reserve");
        std::os::unix::fs::symlink(&victim, &path).unwrap();

        let (mut reserve, _) = FileReserve::open(&path, 4096).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious operator data");
        assert!(
            !std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );

        // The path is swapped for a link to the victim after binding.
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        assert!(reserve.release(1000).unwrap());
        assert_eq!(reserve.len().unwrap(), 3096);
        reserve.replenish().unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious operator data");

        // A hard link at the path is unlinked, never truncated through.
        let other = dir.path().join(".reserve-2");
        std::fs::hard_link(&victim, &other).unwrap();
        let (mut second, _) = FileReserve::open(&other, 2048).unwrap();
        assert!(second.release(2048).unwrap());
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious operator data");
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
