//! OpenClast ingestion opens and reads sources beneath pinned directory handles.
use std::collections::BTreeSet;
use std::fs::{File, Metadata};
use std::io;
use std::path::Path;

use crate::model::{Config, IngestWarning, VaultRegistration};
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

    pub(crate) fn pin_room_root(path: &Path) -> crate::Result<String> {
        let root = open_root(path).map_err(probe_refusal)?;
        let metadata = root
            .metadata()
            .map_err(|error| crate::error::io_error(path, error))?;
        Ok(identity_key(identity(&metadata)))
    }

    fn identity_key((device, inode): Inode) -> String {
        format!("{device:x}:{inode:x}")
    }

    pub(crate) struct RoomRoots {
        roots: BTreeMap<PathBuf, (File, Inode)>,
        // Keep every pinned identity even if its original root no longer opens.
        // The corresponding inode may have moved inside a different room.
        pinned: BTreeSet<String>,
    }

    impl RoomRoots {
        pub(crate) fn capture(config: &Config) -> Self {
            let mut pinned: BTreeSet<_> = config.room_root_identities.values().cloned().collect();
            let opened = (|| {
                let mut paths = BTreeSet::<PathBuf>::new();
                let mut identities = BTreeSet::new();
                let mut roots = BTreeMap::new();
                for vault in &config.vaults {
                    let path = std::fs::canonicalize(&vault.path).ok()?;
                    let root = open_root(&path).ok()?;
                    let inode = identity(&root.metadata().ok()?);
                    let key = identity_key(inode);
                    if vault
                        .room
                        .as_ref()
                        .and_then(|room| config.room_root_identities.get(room))
                        .is_some_and(|pin| pin != &key)
                        || !identities.insert(inode)
                        || paths
                            .iter()
                            .any(|prior| prior.starts_with(&path) || path.starts_with(prior))
                    {
                        return None;
                    }
                    pinned.insert(key);
                    paths.insert(path);
                    roots.insert(vault.path.clone(), (root, inode));
                }
                Some(roots)
            })();
            // No room can index while another registered root is defective.
            Self {
                roots: opened.unwrap_or_default(),
                pinned,
            }
        }

        pub(crate) fn walk(
            &self,
            vault: &VaultRegistration,
        ) -> (EnumerationResult, Option<RoomRead<'_>>) {
            let mut enumeration = empty_enumeration();
            let Some((root, inode)) = self.roots.get(&vault.path) else {
                skip(
                    &mut enumeration,
                    vault,
                    "",
                    "registered room roots could not be verified safely",
                );
                return (enumeration, None);
            };
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
                    if inode != expected_inode
                        || (!relative.is_empty() && self.pinned.contains(&identity_key(inode)))
                    {
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
                        if self.pinned.contains(&identity_key(identity(&metadata))) {
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
        pub(crate) fn capture(_: &Config) -> Self {
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

#[cfg(target_os = "linux")]
pub(crate) use platform::pin_room_root;
pub use platform::probe_openclast_ingestion;
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
    fn n40_missing_replaced_or_nested_pin_fails_every_room_walk() {
        use std::fs;
        for case in ["missing", "replacement", "resolved-nested"] {
            let fixture = tempfile::tempdir().unwrap();
            let ra = fixture.path().join("a");
            let rd = fixture.path().join("d");
            let nested = ra.join("unexpected");
            fs::create_dir(&ra).unwrap();
            fs::create_dir(&rd).unwrap();
            fs::write(ra.join("index.md"), "synthetic alpha").unwrap();
            fs::write(rd.join("index.md"), "synthetic delta").unwrap();
            let mut config = Config::default();
            config.server.profile = crate::HostProfile::OpenClast;
            config.vaults = vec![
                VaultRegistration {
                    id: "vault-a".into(),
                    path: ra.clone(),
                    room: Some("room-a".into()),
                },
                VaultRegistration {
                    id: "vault-d".into(),
                    path: rd.clone(),
                    room: Some("room-d".into()),
                },
            ];
            config.auth.openclast = Some(crate::OpenClastAuthConfig {
                tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
                issuer: "synthetic-issuer".into(),
                audience: "synthetic-audience".into(),
                jwks_file: fixture.path().join("synthetic-jwks.json"),
                max_token_ttl_seconds: 60,
                workload_subject_allowlist: vec![],
                legacy_identity_accept_until: Some(0),
            });
            let config_path = fixture.path().join("kwiry.toml");
            // The first config admission writes pins; untouched reloads
            // must not silently pick up a replacement root.
            fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();
            let config = crate::load_config(&config_path).unwrap();
            assert_eq!(config.room_root_identities.len(), 2);
            let pins_path = config_path.with_extension("room-roots.json");
            let persisted_pins = fs::read(&pins_path).unwrap();
            let pins = config.room_root_identities.clone();
            fs::rename(&rd, &nested).unwrap();
            match case {
                "missing" => {}
                "replacement" => fs::create_dir(&rd).unwrap(),
                "resolved-nested" => std::os::unix::fs::symlink(&nested, &rd).unwrap(),
                _ => unreachable!(),
            }
            if case == "replacement" {
                fs::write(&config_path, toml::to_string_pretty(&config).unwrap()).unwrap();
                let reloaded = crate::load_config(&config_path).unwrap();
                assert_eq!(reloaded.room_root_identities, pins);
            }
            assert_eq!(fs::read(&pins_path).unwrap(), persisted_pins);
            let roots = RoomRoots::capture(&config);
            for vault in &config.vaults {
                let (enumeration, reader) = roots.walk(vault);
                assert!(reader.is_none(), "{case}");
                assert!(enumeration.files.is_empty());
                assert!(!enumeration.completeness.may_infer_deletions());
                let [warning] = &enumeration.warnings[..] else {
                    panic!("expected a single warning")
                };
                assert_eq!(warning.path, vault.path);
                assert_eq!(
                    warning.message,
                    "registered room roots could not be verified safely"
                );
            }
            assert_eq!(config.room_root_identities, pins);
        }
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
        let roots = RoomRoots::capture(&Config {
            vaults: vec![vault.clone()],
            ..Config::default()
        });
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
        let roots = RoomRoots::capture(&Config {
            vaults: vaults.to_vec(),
            ..Config::default()
        });
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
        let roots = RoomRoots::capture(&Config {
            vaults: rooms.to_vec(),
            ..Config::default()
        });
        let (enumeration, _) = roots.walk(&rooms[0]);
        let unmounted = Command::new("umount").arg(&sub).status().unwrap();
        assert!(unmounted.success());
        assert!(enumeration.files.is_empty());
        assert!(!enumeration.completeness.may_infer_deletions());
        assert_eq!(enumeration.warnings[0].path, sub);
    }
}
