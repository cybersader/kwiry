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

use serde::{Deserialize, Serialize};
use std::io;

/// What holder state records about one named reserve file. A reserve is
/// proven to be the holder's own only by the random nonce in its header.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReserveRecord {
    /// The nonce of the reserve this holder created and recorded.
    pub nonce: Option<String>,
    /// A nonce made durable before its file was created: a crash after the
    /// file's header was written but before `nonce` was recorded still lets
    /// the next start adopt that file.
    pub staged: Option<String>,
}

impl ReserveRecord {
    /// Every nonce a file at the reserve path may carry and be adopted.
    pub fn accepted(&self) -> Vec<&str> {
        self.nonce
            .iter()
            .chain(self.staged.iter())
            .map(String::as_str)
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReserveError {
    /// The reserve name holds something this holder cannot prove it
    /// created. Nothing was changed; an operator must clear it.
    #[error("reserve_path_foreign: the reserve name holds a {0} this holder did not create")]
    Foreign(&'static str),
    /// The reserve directory is not a private directory of this user.
    #[error("reserve_dir_unsafe: {0}")]
    UnsafeDirectory(&'static str),
    #[error("reserve: {0}")]
    Io(#[from] io::Error),
}

impl ReserveError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Foreign(_) => "reserve_path_foreign",
            Self::UnsafeDirectory(_) => "reserve_dir_unsafe",
            Self::Io(_) => "reserve_io",
        }
    }
}

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
pub use file_reserve::{FileReserve, HEADER_LEN, ReserveDir};

#[cfg(unix)]
mod file_reserve {
    use super::{DiskReserve, ReserveError, ReserveLevel};
    use rustix::fs::{FileType, Mode, OFlags};
    use std::fs::File;
    use std::io;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::FileExt;
    use std::path::Path;

    const MAGIC: &[u8] = b"holdings-reserve/1\n";
    /// Header bytes at the start of every reserve file: the magic, the
    /// nonce and zero padding. Releases never cut into it.
    pub const HEADER_LEN: u64 = 128;

    fn header(nonce: &str) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(nonce.as_bytes());
        bytes.push(b'\n');
        bytes.resize(usize::try_from(HEADER_LEN).unwrap_or(128), 0);
        bytes
    }

    /// The nonce a header carries, if it is a well-formed reserve header.
    fn header_nonce(bytes: &[u8]) -> Option<&str> {
        let rest = bytes.strip_prefix(MAGIC)?;
        let end = rest.iter().position(|b| *b == b'\n')?;
        let nonce = std::str::from_utf8(&rest[..end]).ok()?;
        let padded = rest[end + 1..].iter().all(|b| *b == 0);
        (padded && crate::grammar::is_b64url(nonce)).then_some(nonce)
    }

    /// A private directory for reserve files, held by descriptor.
    ///
    /// It is created with mode 0700 if absent, then opened with
    /// `O_DIRECTORY | O_NOFOLLOW` and checked through that descriptor: a
    /// directory, owned by this process's effective user, mode exactly 0700.
    /// Every reserve operation is relative to the descriptor, so the path is
    /// never resolved again.
    pub struct ReserveDir {
        fd: OwnedFd,
    }

    impl ReserveDir {
        pub fn open(path: &Path) -> Result<Self, ReserveError> {
            let flags = OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::RDONLY | OFlags::CLOEXEC;
            let (fd, created) =
                match rustix::fs::openat(rustix::fs::CWD, path, flags, Mode::empty()) {
                    Ok(fd) => (fd, false),
                    Err(rustix::io::Errno::NOENT) => {
                        rustix::fs::mkdirat(rustix::fs::CWD, path, Mode::RWXU)
                            .map_err(io::Error::from)?;
                        let fd = rustix::fs::openat(rustix::fs::CWD, path, flags, Mode::empty())
                            .map_err(io::Error::from)?;
                        (fd, true)
                    }
                    Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
                        return Err(ReserveError::UnsafeDirectory("not a directory"));
                    }
                    Err(error) => return Err(io::Error::from(error).into()),
                };
            if created {
                // Whatever the umask did, the directory this holder created
                // is private.
                rustix::fs::fchmod(&fd, Mode::RWXU).map_err(io::Error::from)?;
            }
            let stat = rustix::fs::fstat(&fd).map_err(io::Error::from)?;
            if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
                return Err(ReserveError::UnsafeDirectory("not a directory"));
            }
            if stat.st_uid != rustix::process::geteuid().as_raw() {
                return Err(ReserveError::UnsafeDirectory("owned by another user"));
            }
            if Mode::from_raw_mode(stat.st_mode).bits() & 0o7777 != 0o700 {
                return Err(ReserveError::UnsafeDirectory("mode is not 0700"));
            }
            Ok(Self { fd })
        }

        fn sync(&self) -> io::Result<()> {
            rustix::fs::fsync(&self.fd).map_err(io::Error::from)
        }
    }

    /// A reserve file, bound to its descriptor.
    ///
    /// The reserve code never unlinks, renames or replaces anything. A new
    /// reserve is created with `openat(O_CREAT | O_EXCL | O_NOFOLLOW)` in a
    /// [`ReserveDir`], and its header (a random nonce) is written and
    /// fsynced, with the directory, before the caller records the nonce.
    /// An existing one is opened with `O_NOFOLLOW` and no `O_CREAT`, and
    /// adopted only if it is a regular file whose header carries a nonce
    /// the caller accepts; anything else is refused and left exactly as it
    /// is. Sizes change only through the held descriptor.
    pub struct FileReserve {
        file: File,
        target: u64,
    }

    impl FileReserve {
        /// Opens the reserve `name` if one exists. `Ok(None)` when the name
        /// is free. A file is adopted only with a header nonce in
        /// `accepted`; anything else is [`ReserveError::Foreign`].
        pub fn adopt(
            dir: &ReserveDir,
            name: &str,
            target: u64,
            accepted: &[&str],
        ) -> Result<Option<(Self, String)>, ReserveError> {
            let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
            let fd = match rustix::fs::openat(&dir.fd, name, flags, Mode::empty()) {
                Ok(fd) => fd,
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                // `O_NOFOLLOW` on a link: ELOOP, or EMLINK on some BSDs.
                Err(rustix::io::Errno::LOOP | rustix::io::Errno::MLINK) => {
                    return Err(ReserveError::Foreign("symlink"));
                }
                Err(rustix::io::Errno::ISDIR) => return Err(ReserveError::Foreign("directory")),
                Err(error) => return Err(io::Error::from(error).into()),
            };
            let stat = rustix::fs::fstat(&fd).map_err(io::Error::from)?;
            if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
                return Err(ReserveError::Foreign("special file"));
            }
            let file = File::from(fd);
            let mut bytes = vec![0u8; usize::try_from(HEADER_LEN).unwrap_or(128)];
            if file.read_exact_at(&mut bytes, 0).is_err() {
                return Err(ReserveError::Foreign("file without a reserve header"));
            }
            let Some(nonce) = header_nonce(&bytes) else {
                return Err(ReserveError::Foreign("file without a reserve header"));
            };
            if !accepted.contains(&nonce) {
                return Err(ReserveError::Foreign("file with another reserve's header"));
            }
            let nonce = nonce.to_owned();
            Ok(Some((Self { file, target }, nonce)))
        }

        /// Creates the reserve `name` exclusively and makes its header with
        /// `nonce` durable (file, then directory). Space is allocated later
        /// by [`DiskReserve::replenish`], once the caller recorded the nonce.
        pub fn create(
            dir: &ReserveDir,
            name: &str,
            target: u64,
            nonce: &str,
        ) -> Result<Self, ReserveError> {
            let flags =
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let fd = match rustix::fs::openat(&dir.fd, name, flags, Mode::RUSR | Mode::WUSR) {
                Ok(fd) => fd,
                Err(rustix::io::Errno::EXIST) => {
                    return Err(ReserveError::Foreign("existing entry"));
                }
                Err(error) => return Err(io::Error::from(error).into()),
            };
            let file = File::from(fd);
            file.write_all_at(&header(nonce), 0)?;
            file.sync_all()?;
            dir.sync()?;
            Ok(Self { file, target })
        }

        /// Bytes currently held, header included, read from the descriptor.
        pub fn len(&self) -> io::Result<u64> {
            Ok(self.file.metadata()?.len())
        }

        pub fn is_empty(&self) -> io::Result<bool> {
            Ok(self.len()? <= HEADER_LEN)
        }
    }

    impl DiskReserve for FileReserve {
        fn release(&mut self, bytes: u64) -> io::Result<bool> {
            let len = self.file.metadata()?.len();
            if len.saturating_sub(HEADER_LEN) < bytes {
                return Ok(false);
            }
            self.file.set_len(len - bytes)?;
            self.file.sync_all()?;
            Ok(true)
        }

        fn replenish(&mut self) -> io::Result<ReserveLevel> {
            match fs2::FileExt::allocate(&self.file, HEADER_LEN.saturating_add(self.target)) {
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

    #[cfg(test)]
    pub(crate) fn header_for_tests(nonce: &str) -> Vec<u8> {
        header(nonce)
    }
}

#[cfg(all(test, unix))]
pub(crate) use file_reserve::header_for_tests;

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
    fn private_dir() -> (tempfile::TempDir, std::path::PathBuf, ReserveDir) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reserves");
        let dir = ReserveDir::open(&path).unwrap();
        (root, path, dir)
    }

    #[cfg(unix)]
    const NONCE: &str = "nonceAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    #[cfg(unix)]
    #[test]
    fn a_reserve_is_created_adopted_by_its_nonce_and_resized_through_its_descriptor() {
        let (_root, _path, dir) = private_dir();
        assert!(matches!(
            FileReserve::adopt(&dir, ".reserve", 4096, &[]),
            Ok(None)
        ));
        let mut reserve = FileReserve::create(&dir, ".reserve", 4096, NONCE).unwrap();
        assert_eq!(reserve.replenish().unwrap(), ReserveLevel::Full);
        assert_eq!(reserve.len().unwrap(), HEADER_LEN + 4096);
        assert!(reserve.release(1000).unwrap());
        assert_eq!(reserve.len().unwrap(), HEADER_LEN + 3096);
        assert!(
            !reserve.release(3097).unwrap(),
            "the header is never released"
        );
        assert!(reserve.release(3096).unwrap());
        assert_eq!(reserve.len().unwrap(), HEADER_LEN);
        drop(reserve);
        let (mut adopted, nonce) = FileReserve::adopt(&dir, ".reserve", 4096, &[NONCE])
            .unwrap()
            .unwrap();
        assert_eq!(nonce, NONCE);
        assert_eq!(adopted.replenish().unwrap(), ReserveLevel::Full);
        assert_eq!(adopted.len().unwrap(), HEADER_LEN + 4096);
    }

    /// Re-check item 3: the same inode with other content (a reused inode
    /// number, or a file rewritten in place) proves nothing.
    #[cfg(unix)]
    #[test]
    fn inode_reuse_is_refused_and_the_file_kept() {
        let (_root, path, dir) = private_dir();
        let reserve = FileReserve::create(&dir, ".reserve", 4096, NONCE).unwrap();
        drop(reserve);
        let file_path = path.join(".reserve");
        let inode = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&file_path).unwrap());
        // Same inode, foreign content.
        std::fs::write(&file_path, b"operator data").unwrap();
        assert_eq!(
            std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&file_path).unwrap()),
            inode
        );
        let refused = FileReserve::adopt(&dir, ".reserve", 4096, &[NONCE])
            .err()
            .unwrap();
        assert_eq!(refused.code(), "reserve_path_foreign");
        assert_eq!(std::fs::read(&file_path).unwrap(), b"operator data");
        // Another holder's reserve header is not this holder's either.
        let other = "nonceBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
        std::fs::write(&file_path, header_for_tests(other)).unwrap();
        assert!(matches!(
            FileReserve::adopt(&dir, ".reserve", 4096, &[NONCE]),
            Err(ReserveError::Foreign(_))
        ));
        assert_eq!(std::fs::read(&file_path).unwrap(), header_for_tests(other));
    }

    /// Re-check item 3: a symlink, a foreign file or a directory swapped in
    /// at the reserve name is never touched, and nothing is ever unlinked.
    #[cfg(unix)]
    #[test]
    fn a_swapped_in_symlink_file_or_directory_is_never_touched() {
        let (root, path, dir) = private_dir();
        let victim = root.path().join("victim");
        std::fs::write(&victim, b"precious operator data").unwrap();
        let name = path.join(".reserve");
        std::os::unix::fs::symlink(&victim, &name).unwrap();
        assert!(matches!(
            FileReserve::adopt(&dir, ".reserve", 4096, &[NONCE]),
            Err(ReserveError::Foreign("symlink"))
        ));
        assert!(matches!(
            FileReserve::create(&dir, ".reserve", 4096, NONCE),
            Err(ReserveError::Foreign(_))
        ));
        assert!(
            std::fs::symlink_metadata(&name)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious operator data");

        std::fs::remove_file(&name).unwrap();
        std::fs::hard_link(&victim, &name).unwrap();
        assert!(matches!(
            FileReserve::adopt(&dir, ".reserve", 4096, &[NONCE]),
            Err(ReserveError::Foreign(_))
        ));
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious operator data");
        assert!(name.exists());

        std::fs::remove_file(&name).unwrap();
        std::fs::create_dir(&name).unwrap();
        assert!(matches!(
            FileReserve::adopt(&dir, ".reserve", 4096, &[NONCE]),
            Err(ReserveError::Foreign(_))
        ));
        assert!(name.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn the_reserve_directory_must_be_private_and_real() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let open = root.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            ReserveDir::open(&open).err().unwrap().code(),
            "reserve_dir_unsafe"
        );
        let link = root.path().join("link");
        std::os::unix::fs::symlink(root.path(), &link).unwrap();
        assert_eq!(
            ReserveDir::open(&link).err().unwrap().code(),
            "reserve_dir_unsafe"
        );
        let created = root.path().join("fresh");
        ReserveDir::open(&created).unwrap();
        let mode = std::fs::metadata(&created).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o700);
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
