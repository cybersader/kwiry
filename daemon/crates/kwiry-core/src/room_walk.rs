//! OpenClast ingestion opens and reads sources beneath pinned directory handles.
use std::collections::BTreeSet;
use std::fs::{File, Metadata};
use std::io;
use std::path::Path;

use crate::model::{IngestWarning, VaultRegistration};
use crate::reconcile::EnumerationCompleteness;
use crate::walk::EnumerationResult;

#[cfg(target_os = "linux")]
mod platform {
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

    pub(crate) struct RoomRoots(BTreeMap<PathBuf, (File, Inode)>);

    impl RoomRoots {
        pub(crate) fn capture(vaults: &[VaultRegistration]) -> Self {
            Self(
                vaults
                    .iter()
                    .filter_map(|vault| {
                        let root = File::from(
                            openat2(
                                CWD,
                                &vault.path,
                                OPEN | OFlags::DIRECTORY,
                                Mode::empty(),
                                NO_LINKS,
                            )
                            .ok()?,
                        );
                        let inode = identity(&root.metadata().ok()?);
                        Some((vault.path.clone(), (root, inode)))
                    })
                    .collect(),
            )
        }

        pub(crate) fn walk(
            &self,
            vault: &VaultRegistration,
        ) -> (EnumerationResult, Option<RoomRead<'_>>) {
            let mut enumeration = empty_enumeration();
            let Some((root, inode)) = self.0.get(&vault.path) else {
                skip(
                    &mut enumeration,
                    vault,
                    "",
                    "room root could not be opened safely",
                );
                return (enumeration, None);
            };
            let other_roots: BTreeSet<_> = self
                .0
                .iter()
                .filter(|(path, _)| **path != vault.path)
                .map(|(_, (_, inode))| *inode)
                .collect();
            if other_roots.contains(inode) {
                skip(
                    &mut enumeration,
                    vault,
                    "",
                    "room root overlaps another registered root",
                );
                return (enumeration, None);
            }
            let mut files = BTreeMap::new();
            let mut pending = VecDeque::from([(String::new(), *inode)]);
            while let Some((relative, expected_inode)) = pending.pop_front() {
                let path = if relative.is_empty() { "." } else { &relative };
                let dir = (|| -> io::Result<Dir> {
                    let file = File::from(openat2(
                        root,
                        path,
                        OPEN | OFlags::DIRECTORY,
                        Mode::empty(),
                        ROOM_ONLY,
                    )?);
                    let inode = identity(&file.metadata()?);
                    if inode != expected_inode || other_roots.contains(&inode) {
                        return Err(io::Error::other("directory identity changed"));
                    }
                    Ok(Dir::new(std::os::fd::OwnedFd::from(file))?)
                })();
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
                    if crate::walk::SKIPPED_DIRECTORIES.contains(&name)
                        && entry.file_type() == rustix::fs::FileType::Directory
                    {
                        continue;
                    }
                    let path = if relative.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{relative}/{name}")
                    };
                    // O_PATH avoids opening special files for I/O. Both the
                    // recorded identity and subsequent read use the pinned root.
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
                        if other_roots.contains(&identity(&metadata)) {
                            skip(
                                &mut enumeration,
                                vault,
                                &path,
                                "directory is another registered room root",
                            );
                        } else {
                            pending.push_back((path, identity(&metadata)));
                        }
                        continue;
                    }
                    if !metadata.is_file() {
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
            (enumeration, Some(RoomRead { root, files }))
        }
    }

    pub(crate) struct RoomRead<'a> {
        root: &'a File,
        files: BTreeMap<String, Inode>,
    }

    impl RoomRead<'_> {
        pub(crate) fn open(&self, relative: &str) -> io::Result<File> {
            let file = File::from(openat2(
                self.root,
                relative,
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
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;

    pub(crate) struct RoomRoots;
    pub(crate) struct RoomRead<'a>(std::marker::PhantomData<&'a ()>);

    impl RoomRoots {
        pub(crate) fn capture(_: &[VaultRegistration]) -> Self {
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
        pub(crate) fn open(&self, _: &str) -> io::Result<File> {
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    pub(crate) fn single_link(_: &Metadata) -> bool {
        false
    }
}

pub(crate) use platform::{RoomRead, RoomRoots, single_link};

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
        let roots = RoomRoots::capture(&vaults);
        let (enumeration, reader) = roots.walk(&vaults[0]);
        assert_eq!(enumeration.files.len(), 2);
        let reader = reader.unwrap();
        std::fs::rename(a.join("x.md"), a.join("old.md")).unwrap();
        std::fs::rename(a.join("d"), a.join("old-d")).unwrap();
        std::os::unix::fs::symlink(b.join("x.md"), a.join("x.md")).unwrap();
        std::os::unix::fs::symlink(&b, a.join("d")).unwrap();
        assert!(reader.open("x.md").is_err());
        assert!(reader.open("d/x.md").is_err());
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
                path: b,
                room: Some("b".into()),
            },
        ];
        let roots = RoomRoots::capture(&rooms);
        let (enumeration, _) = roots.walk(&rooms[0]);
        let unmounted = Command::new("umount").arg(&sub).status().unwrap();
        assert!(unmounted.success());
        assert!(enumeration.files.is_empty());
        assert!(!enumeration.completeness.may_infer_deletions());
        assert_eq!(enumeration.warnings[0].path, sub);
    }
}
