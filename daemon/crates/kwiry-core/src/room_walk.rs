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

    use rustix::fs::{AtFlags, CWD, Dir, Mode, OFlags, ResolveFlags, StatxFlags, openat2, statx};

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

    pub(crate) fn open_root_for_boundary(path: &Path) -> crate::Result<File> {
        open_root(path).map_err(probe_refusal)
    }

    pub(crate) fn descriptor_mount_id(root: &File) -> Option<u64> {
        let info = statx(root, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID).ok()?;
        let mount_id = (info.stx_mask & StatxFlags::MNT_ID.bits() != 0).then_some(info.stx_mnt_id);
        #[cfg(test)]
        if let Some(forced) = MOUNT_ID_OVERRIDE.with(|override_| {
            let (target, forced) = override_.get()?;
            (identity(&root.metadata().ok()?) == target).then_some(forced)
        }) {
            return forced;
        }
        mount_id
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
        for vault in vaults {
            crate::room_boundary::require_canonical_room_root(&vault.path)?;
        }
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
        failures: BTreeMap<std::ffi::OsString, &'static str>,
        identities: BTreeSet<Inode>,
    }

    impl RoomRoots {
        pub(crate) fn capture(config: &Config, data_root: Option<&Path>) -> Self {
            let mut paths = Vec::with_capacity(config.vaults.len());
            let mut roots = BTreeMap::new();
            let mut failures = BTreeMap::new();
            let mut by_identity = BTreeMap::new();
            let mut mount_ids = Vec::with_capacity(config.vaults.len());
            for vault in &config.vaults {
                let path = match crate::room_boundary::canonical_room_root(&vault.path) {
                    Ok(Some(path)) => path,
                    Ok(None) => {
                        failures.insert(
                            vault.path.as_os_str().to_owned(),
                            "room root could not be opened safely",
                        );
                        continue;
                    }
                    Err(_) => {
                        failures.insert(
                            vault.path.as_os_str().to_owned(),
                            "room root path is not canonical",
                        );
                        continue;
                    }
                };
                let opened = open_root(&path).ok().and_then(|file| {
                    let inode = identity(&file.metadata().ok()?);
                    Some((file, inode))
                });
                if let Some((file, inode)) = opened {
                    if let Some(other) = by_identity.insert(inode, path.as_os_str().to_owned()) {
                        failures.insert(other, "room root is shared with another registration");
                        failures.insert(
                            path.as_os_str().to_owned(),
                            "room root is shared with another registration",
                        );
                    }
                    mount_ids.push(descriptor_mount_id(&file));
                    roots.insert(path.clone(), (file, inode));
                } else {
                    mount_ids.push(None);
                    failures.insert(
                        path.as_os_str().to_owned(),
                        "room root could not be opened safely",
                    );
                }
                // A canonical path still participates in overlap detection
                // when its own handle cannot be opened. It cannot be walked.
                paths.push(path);
            }
            match crate::room_boundary::walk_failures(&paths, &mount_ids, config, data_root) {
                Ok(boundary) => {
                    for (path, failure) in paths.into_iter().zip(boundary) {
                        if let Some(message) = failure
                            && roots.contains_key(&path)
                        {
                            failures.insert(path.into_os_string(), message);
                        }
                    }
                }
                Err(_) => {
                    for path in paths {
                        failures
                            .insert(path.into_os_string(), "room boundary could not be checked");
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
            if let Some(message) = self.failures.get(vault.path.as_os_str()) {
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
                    defect(
                        &mut enumeration,
                        vault,
                        &relative,
                        "directory could not be opened within the room",
                    );
                    continue;
                };
                #[cfg(test)]
                let mut observed_entry = false;
                loop {
                    #[cfg(test)]
                    if observed_entry && should_interrupt_listing(&vault.path.join(&relative)) {
                        defect(
                            &mut enumeration,
                            vault,
                            &relative,
                            "directory could not be enumerated safely",
                        );
                        break;
                    }
                    let entry = match dir.next() {
                        None => {
                            enumeration.directories.complete.insert(relative.clone());
                            break;
                        }
                        Some(Err(_)) => {
                            defect(
                                &mut enumeration,
                                vault,
                                &relative,
                                "directory could not be enumerated safely",
                            );
                            break;
                        }
                        Some(Ok(entry)) => entry,
                    };
                    let name = entry.file_name().to_bytes();
                    if name.starts_with(b".") {
                        continue;
                    }
                    #[cfg(test)]
                    {
                        observed_entry = true;
                    }
                    let Ok(name) = std::str::from_utf8(name) else {
                        defect(
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
                    let fd = match dir.fd().and_then(|dirfd| {
                        openat2(
                            dirfd,
                            name,
                            OFlags::PATH | OFlags::CLOEXEC,
                            Mode::empty(),
                            ROOM_ONLY,
                        )
                    }) {
                        Ok(fd) => fd,
                        Err(rustix::io::Errno::LOOP | rustix::io::Errno::XDEV) => {
                            skip(
                                &mut enumeration,
                                vault,
                                &path,
                                "entry excluded by room boundary",
                            );
                            continue;
                        }
                        Err(_) => {
                            defect(
                                &mut enumeration,
                                vault,
                                &path,
                                "entry could not be opened safely",
                            );
                            enumeration.directories.unverified_entries.insert(path);
                            continue;
                        }
                    };
                    let Ok(metadata) = File::from(fd).metadata() else {
                        defect(
                            &mut enumeration,
                            vault,
                            &path,
                            "entry metadata could not be read safely",
                        );
                        enumeration.directories.unverified_entries.insert(path);
                        continue;
                    };
                    if metadata.is_dir() {
                        let found = identity(&metadata);
                        if found != *inode && self.identities.contains(&found) {
                            return (nesting_failure(vault), None);
                        }
                        enumeration
                            .directories
                            .seen_directories
                            .insert(path.clone());
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

    #[cfg(test)]
    thread_local! {
        static INTERRUPT_LISTING: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
        static UNREADABLE_SOURCE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
        static MOUNT_ID_OVERRIDE: Cell<Option<(Inode, Option<u64>)>> = const { Cell::new(None) };
    }

    #[cfg(test)]
    pub(super) fn with_mount_id_override<T>(
        path: &Path,
        forced: Option<u64>,
        run: impl FnOnce() -> T,
    ) -> T {
        let id = identity(&std::fs::metadata(path).unwrap());
        let previous = MOUNT_ID_OVERRIDE.replace(Some((id, forced)));
        let result = run();
        MOUNT_ID_OVERRIDE.set(previous);
        result
    }

    #[cfg(test)]
    pub(crate) fn with_unreadable_source<T>(path: &str, run: impl FnOnce() -> T) -> T {
        let previous = UNREADABLE_SOURCE.replace(Some(path.to_owned()));
        let result = run();
        UNREADABLE_SOURCE.replace(previous);
        result
    }

    #[cfg(test)]
    fn should_interrupt_listing(path: &Path) -> bool {
        INTERRUPT_LISTING.with(|fault| fault.borrow().as_deref() == Some(path))
    }

    #[cfg(test)]
    pub(crate) fn with_interrupted_listing<T>(path: PathBuf, run: impl FnOnce() -> T) -> T {
        let previous = INTERRUPT_LISTING.replace(Some(path));
        let result = run();
        INTERRUPT_LISTING.set(previous);
        result
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
            #[cfg(test)]
            if UNREADABLE_SOURCE.with(|p| p.borrow().as_deref() == Some(relative)) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
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
            snapshot_id: None,
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
#[cfg(target_os = "linux")]
pub(crate) use platform::{descriptor_mount_id, open_root_for_boundary};
#[cfg(all(test, target_os = "linux"))]
pub(crate) use platform::{with_interrupted_listing, with_unreadable_source};

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
        directories: Default::default(),
    }
}

fn skip(
    enumeration: &mut EnumerationResult,
    vault: &VaultRegistration,
    relative: &str,
    message: &str,
) {
    enumeration.excluded.insert(relative.to_owned());
    defect(enumeration, vault, relative, message);
}

fn defect(
    enumeration: &mut EnumerationResult,
    vault: &VaultRegistration,
    relative: &str,
    message: &str,
) {
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
    fn fr10_mount_ids_bind_config_and_walk_to_room_descriptors() {
        use std::fs;
        let fixture = tempfile::tempdir().unwrap();
        let a = fixture.path().join("a");
        let b = fixture.path().join("b");
        let settings = fixture.path().join("settings/config.toml");
        let data = fixture.path().join("data");
        let register = |id: &str, path: &Path| {
            fs::create_dir(path).unwrap();
            fs::write(path.join("index.md"), "synthetic evidence").unwrap();
            VaultRegistration {
                snapshot_id: None,
                id: id.into(),
                path: path.into(),
                room: Some(id.into()),
            }
        };
        let mut config = Config::default();
        config.server.profile = crate::HostProfile::OpenClast;
        config.auth.openclast = Some(crate::OpenClastAuthConfig {
            tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
            issuer: "synthetic-issuer".into(),
            audience: "synthetic-audience".into(),
            jwks_file: fixture.path().join("keys.json"),
            max_token_ttl_seconds: 60,
            workload_subject_allowlist: vec![],
            legacy_identity_accept_until: None,
        });
        config.vaults = vec![register("a", &a), register("b", &b)];
        crate::save_config(&settings, &config).unwrap();
        let paths = crate::Paths::resolve(Some(settings), Some(data.clone())).unwrap();
        let loaded = crate::load_config_for_paths(&paths).unwrap();
        for id in [None, Some(u64::MAX)] {
            platform::with_mount_id_override(&a, id, || {
                let error = crate::load_config_for_paths(&paths).unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("opened room mount does not match mountinfo")
                );
                assert!(!data.exists());
                let roots = RoomRoots::capture(&loaded, Some(&data));
                let (failed, reader) = roots.walk(&loaded.vaults[0]);
                assert!(reader.is_none());
                assert!(failed.files.is_empty());
                assert!(!failed.completeness.may_infer_deletions());
                assert_eq!(failed.warnings[0].path, a);
                assert_eq!(
                    failed.warnings[0].message,
                    "opened room mount does not match mountinfo"
                );
                assert_eq!(roots.walk(&loaded.vaults[1]).0.files.len(), 1);
            });
        }
    }

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
            snapshot_id: None,
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
            snapshot_id: None,
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
            snapshot_id: None,
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
                snapshot_id: None,
                id: "vault".into(),
                path: a.clone(),
                room: Some("a".into()),
            },
            VaultRegistration {
                snapshot_id: None,
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
                    snapshot_id: None,
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
