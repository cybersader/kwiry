//! OpenClast ingestion opens and reads sources beneath directory handles captured for the current walk.
use std::collections::BTreeSet;
use std::fs::{File, Metadata};
use std::io;
use std::path::Path;

use crate::model::{Config, IngestWarning, VaultRegistration};
use crate::reconcile::EnumerationCompleteness;
use crate::walk::EnumerationResult;

#[cfg(target_os = "linux")]
mod platform {
    use std::cell::Cell;
    use std::collections::{BTreeMap, VecDeque};
    use std::ffi::OsStr;
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    use rustix::fs::{CWD, Dir, Mode, OFlags, ResolveFlags, openat2};

    use super::*;
    use crate::format::SourceFormat;
    use crate::model::DiscoveredFile;

    const NO_LINKS: ResolveFlags = ResolveFlags::NO_SYMLINKS.union(ResolveFlags::NO_MAGICLINKS);
    const ROOM_ONLY: ResolveFlags = NO_LINKS
        .union(ResolveFlags::BENEATH)
        .union(ResolveFlags::NO_XDEV);
    const OPEN: OFlags = OFlags::RDONLY
        .union(OFlags::CLOEXEC)
        .union(OFlags::NONBLOCK);
    type Inode = (u64, u64);

    fn open_root(path: &Path) -> rustix::io::Result<File> {
        openat2(CWD, path, OPEN | OFlags::DIRECTORY, Mode::empty(), NO_LINKS).map(File::from)
    }

    fn probe_refusal(error: rustix::io::Errno) -> crate::Error {
        crate::Error::State(format!(
            "openclast_openat2_probe_failed: errno {} ({error})",
            error.raw_os_error()
        ))
    }

    pub fn probe_openclast_ingestion(
        data_root: &Path,
        vaults: &[VaultRegistration],
    ) -> crate::Result<()> {
        std::fs::create_dir_all(data_root)
            .map_err(|error| crate::error::io_error(data_root, error))?;
        let data_root = std::fs::canonicalize(data_root)
            .map_err(|error| crate::error::io_error(data_root, error))?;
        probe_roots(&data_root, vaults, |directory| {
            openat2(
                directory,
                ".",
                OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
                ROOM_ONLY,
            )
            .map(drop)
        })
    }

    fn probe_roots(
        data_root: &Path,
        vaults: &[VaultRegistration],
        mut probe: impl FnMut(&File) -> rustix::io::Result<()>,
    ) -> crate::Result<()> {
        let data = open_root(data_root).map_err(probe_refusal)?;
        probe(&data).map_err(probe_refusal)?;
        for vault in vaults {
            let root = open_root(&vault.path).map_err(probe_refusal)?;
            probe(&root).map_err(probe_refusal)?;
        }
        Ok(())
    }

    pub(crate) struct RoomRoots {
        roots: BTreeMap<PathBuf, (File, Inode)>,
        failures: BTreeMap<PathBuf, &'static str>,
        identities: BTreeSet<Inode>,
    }

    impl RoomRoots {
        pub(crate) fn capture(config: &Config, data_root: Option<&Path>) -> Self {
            let paths: Vec<_> = config
                .vaults
                .iter()
                .map(|vault| vault.path.clone())
                .collect();
            let mut roots = BTreeMap::new();
            let mut failures = BTreeMap::new();
            let mut by_identity = BTreeMap::new();
            for path in &paths {
                let opened = open_root(path).ok().and_then(|file| {
                    let inode = identity(&file.metadata().ok()?);
                    Some((file, inode))
                });
                if let Some((file, inode)) = opened {
                    if let Some(other) = by_identity.insert(inode, path.clone()) {
                        failures.insert(other, "room root is shared with another registration");
                        failures.insert(
                            path.clone(),
                            "room root is shared with another registration",
                        );
                    }
                    roots.insert(path.clone(), (file, inode));
                } else {
                    failures.insert(path.clone(), "room root could not be opened safely");
                }
            }
            match crate::room_boundary::failures(&paths, config, data_root) {
                Ok(boundary) => {
                    for (path, failure) in paths.into_iter().zip(boundary) {
                        if let Some(message) = failure {
                            failures.insert(path, message);
                        }
                    }
                }
                Err(_) => {
                    for path in paths {
                        failures.insert(path, "room boundary could not be checked");
                    }
                }
            }
            Self {
                roots,
                failures,
                identities: by_identity.into_keys().collect(),
            }
        }

        pub(crate) fn walk(
            &self,
            vault: &VaultRegistration,
        ) -> (EnumerationResult, Option<RoomRead<'_>>) {
            if let Some(message) = self.failures.get(&vault.path) {
                return (failed_walk(vault, message), None);
            }
            let Some((root, inode)) = self.roots.get(&vault.path) else {
                return (
                    failed_walk(vault, "room root could not be opened safely"),
                    None,
                );
            };
            let mut enumeration = empty_enumeration();
            let mut files = BTreeMap::new();
            let mut pending = VecDeque::from([(String::new(), *inode)]);
            while let Some((relative, expected_inode)) = pending.pop_front() {
                let dir = descend(
                    root,
                    relative.split('/').filter(|part| !part.is_empty()),
                    &self.identities,
                    OPEN,
                );
                if matches!(dir, Ok(None)) {
                    return (nesting_failure(vault), None);
                }
                let dir = dir.and_then(|file| {
                    let file = file.expect("boundary miss handled above");
                    if identity(&file.metadata()?) != expected_inode {
                        return Err(io::Error::other("directory identity changed"));
                    }
                    Ok(Dir::new(std::os::fd::OwnedFd::from(file))?)
                });
                let Ok(mut dir) = dir else {
                    skip(
                        &mut enumeration,
                        vault,
                        &relative,
                        "directory could not be opened within the room",
                    );
                    continue;
                };
                while let Some(entry) = dir.next() {
                    let Ok(entry) = entry else {
                        skip(
                            &mut enumeration,
                            vault,
                            &relative,
                            "directory could not be enumerated safely",
                        );
                        break;
                    };
                    let name = entry.file_name().to_bytes();
                    if name.starts_with(b".") {
                        continue;
                    }
                    let Ok(name) = std::str::from_utf8(name) else {
                        skip(
                            &mut enumeration,
                            vault,
                            &relative,
                            "directory entry is not valid UTF-8",
                        );
                        continue;
                    };
                    let path = if relative.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{relative}/{name}")
                    };
                    // O_PATH avoids opening special files for I/O. Both the
                    // recorded identity and subsequent read use the captured root.
                    let Ok(fd) = dir.fd().and_then(|dirfd| {
                        openat2(
                            dirfd,
                            name,
                            OFlags::PATH | OFlags::CLOEXEC,
                            Mode::empty(),
                            ROOM_ONLY,
                        )
                    }) else {
                        skip(
                            &mut enumeration,
                            vault,
                            &path,
                            "entry excluded by room boundary",
                        );
                        continue;
                    };
                    let Ok(metadata) = File::from(fd).metadata() else {
                        skip(
                            &mut enumeration,
                            vault,
                            &path,
                            "entry metadata could not be read safely",
                        );
                        continue;
                    };
                    if metadata.is_dir() {
                        let found = identity(&metadata);
                        if found != *inode && self.identities.contains(&found) {
                            return (nesting_failure(vault), None);
                        }
                        pending.push_back((path, found));
                        continue;
                    }
                    if !metadata.is_file() {
                        skip(
                            &mut enumeration,
                            vault,
                            &path,
                            "entry is not a regular file or directory",
                        );
                        continue;
                    }
                    if metadata.nlink() > 1 {
                        skip(&mut enumeration, vault, &path, "hard-linked file excluded");
                        continue;
                    }
                    let absolute_path = vault.path.join(&path);
                    let Some(format) = SourceFormat::from_extractable_path(&absolute_path) else {
                        continue;
                    };
                    if metadata.len() > crate::model::MAX_FILE_BYTES && format != SourceFormat::Html
                    {
                        skip(
                            &mut enumeration,
                            vault,
                            &path,
                            "file exceeds source size budget",
                        );
                        continue;
                    }
                    let modified = metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(UNIX_EPOCH).ok());
                    files.insert(path.clone(), identity(&metadata));
                    enumeration.files.push(DiscoveredFile {
                        extension: absolute_path
                            .extension()
                            .and_then(OsStr::to_str)
                            .expect("extractable extension")
                            .to_ascii_lowercase(),
                        absolute_path,
                        relative_path: path,
                        byte_length: metadata.len(),
                        mtime: modified.map_or(0, |time| time.as_secs()),
                        mtime_nanos: modified.map_or(0, |time| time.as_nanos()),
                    });
                }
            }
            enumeration
                .files
                .sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
            (
                enumeration,
                Some(RoomRead {
                    root,
                    files,
                    identities: &self.identities,
                    nesting_detected: Cell::new(false),
                }),
            )
        }
    }

    // Check every reopened directory component, not only the final inode.
    // A concurrent move could put a captured root above that final directory.
    fn descend<'a>(
        root: &File,
        parts: impl Iterator<Item = &'a str>,
        identities: &BTreeSet<Inode>,
        final_flags: OFlags,
    ) -> io::Result<Option<File>> {
        let mut parts = parts.peekable();
        if parts.peek().is_none() {
            return Ok(Some(File::from(openat2(
                root,
                ".",
                final_flags | OFlags::DIRECTORY,
                Mode::empty(),
                ROOM_ONLY,
            )?)));
        }
        let mut directory = None;
        while let Some(part) = parts.next() {
            let flags = if parts.peek().is_some() {
                OFlags::PATH | OFlags::CLOEXEC
            } else {
                final_flags
            };
            let child = File::from(openat2(
                directory.as_ref().unwrap_or(root),
                part,
                flags | OFlags::DIRECTORY,
                Mode::empty(),
                ROOM_ONLY,
            )?);
            if identities.contains(&identity(&child.metadata()?)) {
                return Ok(None);
            }
            directory = Some(child);
        }
        Ok(directory)
    }

    pub(crate) struct RoomRead<'a> {
        root: &'a File,
        files: BTreeMap<String, Inode>,
        identities: &'a BTreeSet<Inode>,
        nesting_detected: Cell<bool>,
    }

    impl RoomRead<'_> {
        pub(crate) fn nesting_detected(&self) -> bool {
            self.nesting_detected.get()
        }

        pub(crate) fn open(&self, relative: &str) -> io::Result<File> {
            let (directory, name) = relative
                .rsplit_once('/')
                .map_or((None, relative), |(directory, name)| {
                    (Some(directory), name)
                });
            let parent = if let Some(directory) = directory {
                Some(
                    descend(
                        self.root,
                        directory.split('/'),
                        self.identities,
                        OFlags::PATH | OFlags::CLOEXEC,
                    )?
                    .ok_or_else(|| {
                        self.nesting_detected.set(true);
                        io::Error::other("source is no longer within the room boundary")
                    })?,
                )
            } else {
                None
            };
            let file = File::from(openat2(
                parent.as_ref().unwrap_or(self.root),
                name,
                OPEN,
                Mode::empty(),
                ROOM_ONLY,
            )?);
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.nlink() != 1
                || self.files.get(relative) != Some(&identity(&metadata))
            {
                return Err(io::Error::other("entry identity or link count changed"));
            }
            Ok(file)
        }
    }

    fn identity(metadata: &Metadata) -> Inode {
        (metadata.dev(), metadata.ino())
    }

    pub(crate) fn single_link(metadata: &Metadata) -> bool {
        metadata.is_file() && metadata.nlink() == 1
    }
    #[cfg(test)]
    #[test]
    fn n34_registered_root_error_after_successful_data_probe() {
        let dir = tempfile::tempdir().unwrap();
        let room = dir.path().join("room");
        std::fs::create_dir(&room).unwrap();
        let vaults = [VaultRegistration {
            id: "vault".into(),
            path: room,
            room: Some("room".into()),
        }];
        let mut calls = 0;
        let error = probe_roots(dir.path(), &vaults, |_| {
            calls += 1;
            if calls == 1 {
                Ok(())
            } else {
                Err(rustix::io::Errno::ACCESS)
            }
        })
        .unwrap_err();
        assert_eq!(calls, 2);
        assert!(
            error
                .to_string()
                .contains("openclast_openat2_probe_failed: errno 13")
        );
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;

    pub fn probe_openclast_ingestion(_: &Path, _: &[VaultRegistration]) -> crate::Result<()> {
        Err(crate::Error::State(
            "openclast_ingestion_platform_unsupported: secure room ingestion is Linux-only".into(),
        ))
    }

    pub(crate) struct RoomRoots;
    pub(crate) struct RoomRead<'a>(std::marker::PhantomData<&'a ()>);

    impl RoomRoots {
        pub(crate) fn capture(_: &Config, _: Option<&Path>) -> Self {
            Self
        }
        pub(crate) fn walk(
            &self,
            vault: &VaultRegistration,
        ) -> (EnumerationResult, Option<RoomRead<'_>>) {
            let mut enumeration = empty_enumeration();
            skip(
                &mut enumeration,
                vault,
                "",
                "secure room ingestion is unavailable on this platform",
            );
            (enumeration, None)
        }
    }

    impl RoomRead<'_> {
        pub(crate) fn nesting_detected(&self) -> bool {
            false
        }

        pub(crate) fn open(&self, _: &str) -> io::Result<File> {
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    pub(crate) fn single_link(_: &Metadata) -> bool {
        false
    }
}

pub use platform::probe_openclast_ingestion;
pub(crate) use platform::{RoomRead, RoomRoots, single_link};

fn failed_walk(vault: &VaultRegistration, message: &str) -> EnumerationResult {
    let mut enumeration = empty_enumeration();
    skip(&mut enumeration, vault, "", message);
    // A wholly failed walk is not evidence of deletion. The runtime may
    // retain the last committed artifacts; no candidates from this pass survive.
    enumeration.excluded.clear();
    enumeration
}

pub(crate) fn nesting_failure(vault: &VaultRegistration) -> EnumerationResult {
    failed_walk(vault, "walk encountered another registered room root")
}

fn empty_enumeration() -> EnumerationResult {
    EnumerationResult {
        files: Vec::new(),
        warnings: Vec::new(),
        completeness: EnumerationCompleteness::Complete,
        excluded: BTreeSet::new(),
    }
}

fn skip(
    enumeration: &mut EnumerationResult,
    vault: &VaultRegistration,
    relative: &str,
    message: &str,
) {
    enumeration.excluded.insert(relative.to_owned());
    enumeration.completeness = EnumerationCompleteness::Defective;
    enumeration.warnings.push(IngestWarning {
        path: vault.path.join(Path::new(relative)),
        message: message.to_owned(),
    });
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn fr5_n40_scope_and_move_after_capture() {
        use std::fs;
        let fixture = tempfile::tempdir().unwrap();
        let ra = fixture.path().join("a");
        let rd = fixture.path().join("d");
        let nested = ra.join("moved");
        fs::create_dir(&ra).unwrap();
        fs::create_dir(&rd).unwrap();
        fs::write(ra.join("index.md"), "synthetic alpha").unwrap();
        fs::write(rd.join("index.md"), "synthetic delta").unwrap();
        let vault = |path, id: &str| VaultRegistration {
            id: id.into(),
            path,
            room: Some(id.into()),
        };
        let config = Config {
            vaults: vec![vault(ra.clone(), "a"), vault(rd.clone(), "d")],
            ..Config::default()
        };
        let [a, d] = &config.vaults[..] else {
            unreachable!()
        };

        // (i) An unrelated unavailable room does not block A.
        let offline = fixture.path().join("offline");
        fs::rename(&rd, &offline).unwrap();
        let roots = RoomRoots::capture(&config, None);
        assert_eq!(roots.walk(a).0.files.len(), 1);
        assert!(roots.walk(d).1.is_none());
        fs::rename(&offline, &rd).unwrap();

        // (ii) A directory encountered after capture is still D's root.
        let roots = RoomRoots::capture(&config, None);
        fs::rename(&rd, &nested).unwrap();
        std::os::unix::fs::symlink(&nested, &rd).unwrap();
        let (enumeration, reader) = roots.walk(a);
        assert!(!enumeration.completeness.may_infer_deletions());
        assert!(reader.is_none());
        assert!(enumeration.files.is_empty());
        assert_eq!(enumeration.warnings[0].path, ra);
        assert_eq!(
            enumeration.warnings[0].message,
            "walk encountered another registered room root"
        );

        // (iii) With D gone at the next capture there is no identity history.
        fs::remove_file(&rd).unwrap();
        let roots = RoomRoots::capture(&config, None);
        assert!(roots.walk(d).1.is_none());
        let (enumeration, reader) = roots.walk(a);
        assert!(enumeration.completeness.may_infer_deletions());
        assert_eq!(enumeration.files.len(), 2);
        assert!(reader.unwrap().open("moved/index.md").is_ok());
    }

    #[test]
    fn h7_visible_special_files_make_coverage_incomplete() {
        use std::os::unix::fs::FileTypeExt;
        use std::os::unix::net::UnixListener;
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("synthetic.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        assert!(
            std::fs::symlink_metadata(&socket)
                .unwrap()
                .file_type()
                .is_socket()
        );
        let vault = VaultRegistration {
            id: "vault".into(),
            path: directory.path().into(),
            room: Some("room".into()),
        };
        let roots = RoomRoots::capture(
            &Config {
                vaults: vec![vault.clone()],
                ..Config::default()
            },
            None,
        );
        let (enumeration, _) = roots.walk(&vault);
        assert!(!enumeration.completeness.may_infer_deletions());
        assert_eq!(enumeration.warnings[0].path, socket);
    }

    #[test]
    fn room_walk_n23_refuses_file_and_parent_swaps_after_walk() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir_all(a.join("d")).unwrap();
        std::fs::create_dir(&b).unwrap();
        for path in [a.join("x.md"), a.join("d/x.md"), b.join("x.md")] {
            std::fs::write(path, "synthetic").unwrap();
        }
        let room = |path, room| VaultRegistration {
            id: "vault".into(),
            path,
            room: Some(room),
        };
        let vaults = [room(a.clone(), "a".into()), room(b.clone(), "b".into())];
        let roots = RoomRoots::capture(
            &Config {
                vaults: vaults.to_vec(),
                ..Config::default()
            },
            None,
        );
        let (enumeration, reader) = roots.walk(&vaults[0]);
        assert_eq!(enumeration.files.len(), 2);
        let reader = reader.unwrap();
        std::fs::rename(a.join("x.md"), a.join("old.md")).unwrap();
        std::fs::rename(a.join("d"), a.join("old-d")).unwrap();
        std::os::unix::fs::symlink(b.join("x.md"), a.join("x.md")).unwrap();
        std::os::unix::fs::symlink(&b, a.join("d")).unwrap();
        assert!(reader.open("x.md").is_err());
        assert!(reader.open("d/x.md").is_err());
        assert!(!reader.nesting_detected());
        std::fs::remove_file(a.join("d")).unwrap();
        std::fs::rename(&b, a.join("d")).unwrap();
        assert!(reader.open("d/x.md").is_err());
        assert!(reader.nesting_detected());
    }

    #[test]
    #[ignore = "run by the dedicated Linux CI job inside unshare -rm"]
    fn room_walk_n24_does_not_cross_a_bind_mount() {
        use std::process::Command;
        let own_ns = std::fs::read_link("/proc/self/ns/mnt").unwrap();
        let init_ns =
            std::env::var_os("KWIRY_N24_PARENT_NS").expect("CI passes the parent mount namespace");
        assert_ne!(
            own_ns.as_os_str(),
            init_ns,
            "run only within a private mount namespace"
        );
        let fixture = tempfile::tempdir().unwrap();
        let a = fixture.path().join("a");
        let b = fixture.path().join("b");
        let sub = a.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir(&b).unwrap();
        std::fs::write(b.join("index.md"), "beta").unwrap();
        assert!(
            Command::new("mount")
                .arg("--bind")
                .arg(&b)
                .arg(&sub)
                .status()
                .unwrap()
                .success()
        );
        let rooms = [
            VaultRegistration {
                id: "vault".into(),
                path: a.clone(),
                room: Some("a".into()),
            },
            VaultRegistration {
                id: "vault".into(),
                path: b.clone(),
                room: Some("b".into()),
            },
        ];
        let roots = RoomRoots::capture(
            &Config {
                vaults: rooms.to_vec(),
                ..Config::default()
            },
            None,
        );
        let (enumeration, _) = roots.walk(&rooms[0]);
        let unmounted = Command::new("umount").arg(&sub).status().unwrap();
        assert!(unmounted.success());
        assert!(enumeration.files.is_empty());
        assert!(!enumeration.completeness.may_infer_deletions());
        assert_eq!(enumeration.warnings[0].path, sub);

        let c = fixture.path().join("c");
        std::fs::create_dir(&c).unwrap();
        std::fs::write(c.join("index.md"), "synthetic gamma").unwrap();
        let config = Config {
            vaults: rooms
                .into_iter()
                .chain([VaultRegistration {
                    id: "healthy".into(),
                    path: c,
                    room: Some("c".into()),
                }])
                .collect(),
            ..Config::default()
        };
        for alias in ["sub", ".trash"] {
            let source = b.join(alias);
            std::fs::create_dir(&source).unwrap();
            std::fs::write(source.join("index.md"), "synthetic beta").unwrap();
            assert!(
                Command::new("mount")
                    .arg("--bind")
                    .arg(&source)
                    .arg(&a)
                    .status()
                    .unwrap()
                    .success()
            );
            for vault in &config.vaults[..2] {
                let roots = RoomRoots::capture(&config, None);
                let (enumeration, reader) = roots.walk(vault);
                assert!(reader.is_none());
                assert!(enumeration.files.is_empty());
                assert!(!enumeration.completeness.may_infer_deletions());
                assert_eq!(enumeration.warnings[0].path, vault.path);
                assert_eq!(
                    enumeration.warnings[0].message,
                    "room roots overlap through a mount"
                );
            }
            {
                let roots = RoomRoots::capture(&config, None);
                assert_eq!(roots.walk(&config.vaults[2]).0.files.len(), 1);
                let only_a = Config {
                    vaults: vec![config.vaults[0].clone()],
                    ..Config::default()
                };
                let roots = RoomRoots::capture(&only_a, Some(&source));
                let (enumeration, reader) = roots.walk(&only_a.vaults[0]);
                assert!(reader.is_none());
                assert_eq!(
                    enumeration.warnings[0].message,
                    "room root overlaps Kwiry state through a mount"
                );
            }
            // The captures own open handles to the alias; drop them first.
            assert!(Command::new("umount").arg(&a).status().unwrap().success());
        }
    }
}
