//! OpenClast room boundaries in the current mount namespace.
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::{Config, Error, Result};

type Failure = Option<&'static str>;

fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Resolve existing ancestors of not-yet-created state paths, but do not
/// mistake a broken symlink for a state path that has not been created.
fn resolve_state_path(path: &Path) -> io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match fs::symlink_metadata(path) {
                Err(missing) if missing.kind() == io::ErrorKind::NotFound => {}
                _ => return Err(error),
            }
            let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
                return Err(error);
            };
            Ok(resolve_state_path(parent)?.join(name))
        }
        Err(error) => Err(error),
    }
}

/// Validate the original spelling before the filesystem can normalize it.
/// Missing/offline roots have no canonical path to walk, but do not invalidate
/// unrelated rooms at runtime. Registration requires all roots to resolve.
pub(crate) fn canonical_room_root(path: &Path) -> Result<Option<PathBuf>> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
    {
        return Err(boundary_error("room root path is not canonical"));
    }
    match fs::canonicalize(path) {
        Ok(canonical) if canonical.as_os_str() == path.as_os_str() => Ok(Some(canonical)),
        Ok(_) => Err(boundary_error("room root path is not canonical")),
        Err(_) => Ok(None),
    }
}

pub(crate) fn require_canonical_room_root(path: &Path) -> Result<PathBuf> {
    canonical_room_root(path)?.ok_or_else(|| boundary_error("room root cannot be resolved"))
}

pub(crate) fn validate_ingestion_roots(config: &Config) -> Result<()> {
    for vault in &config.vaults {
        canonical_room_root(&vault.path)?;
    }
    Ok(())
}

/// Refuse invalid configurations before writing Kwiry state. The walker uses
/// the same comparison but blocks only the affected rooms.
pub(crate) fn validate(config: &Config, data_root: Option<&Path>) -> Result<()> {
    let roots = config
        .vaults
        .iter()
        .map(|vault| require_canonical_room_root(&vault.path))
        .collect::<Result<Vec<_>>>()?;
    if roots.is_empty() {
        return Ok(());
    }
    // Keep the room handles live until the same snapshot has been checked.
    #[cfg(target_os = "linux")]
    let opened = roots
        .iter()
        .map(|path| crate::room_walk::open_root_for_boundary(path))
        .collect::<Result<Vec<_>>>()?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;

        let mut identities = std::collections::BTreeSet::new();
        for root in &opened {
            let metadata = root
                .metadata()
                .map_err(|_| boundary_error("room root metadata is unavailable"))?;
            if !identities.insert((metadata.dev(), metadata.ino())) {
                return Err(boundary_error(
                    "room root is shared with another registration",
                ));
            }
        }
    }
    #[cfg(target_os = "linux")]
    let mount_ids = opened
        .iter()
        .map(crate::room_walk::descriptor_mount_id)
        .collect::<Vec<_>>();
    #[cfg(target_os = "linux")]
    let ids = Some(mount_ids.as_slice());
    #[cfg(not(target_os = "linux"))]
    let ids = None;
    match compare(&roots, &state_paths(config, data_root)?, true, ids)?
        .into_iter()
        .flatten()
        .next()
    {
        Some(message) => Err(boundary_error(message)),
        None => Ok(()),
    }
}

/// Check the writable root separately, so a broken or overlapping room does
/// not stop a build from walking unrelated rooms in a safe data directory.
pub(crate) fn validate_data_root(config: &Config, data_root: &Path) -> Result<()> {
    if config.vaults.is_empty() {
        return Ok(());
    }
    let rooms: Vec<_> = config
        .vaults
        .iter()
        .map(|vault| Ok(canonical_room_root(&vault.path)?.unwrap_or_else(|| vault.path.clone())))
        .collect::<Result<_>>()?;
    let data_root = resolve_state_path(data_root)
        .map_err(|_| boundary_error("Kwiry data root cannot be resolved"))?;
    let failures = compare(&rooms, &[data_root], false, None)?;
    if failures.iter().any(Option::is_some) {
        return Err(boundary_error(
            "Kwiry data root is not separate from the room roots",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn walk_failures(
    roots: &[PathBuf],
    mount_ids: &[Option<u64>],
    config: &Config,
    data_root: Option<&Path>,
) -> Result<Vec<Failure>> {
    if roots.is_empty() {
        return Ok(Vec::new());
    }
    compare(
        roots,
        &state_paths(config, data_root)?,
        true,
        Some(mount_ids),
    )
}

fn state_paths(config: &Config, data_root: Option<&Path>) -> Result<Vec<PathBuf>> {
    config
        .openclast_state_paths
        .iter()
        .map(PathBuf::as_path)
        .chain(data_root)
        .chain(
            config
                .auth
                .openclast
                .as_ref()
                .map(|auth| auth.jwks_file.as_path()),
        )
        .map(resolve_state_path)
        .collect::<io::Result<Vec<_>>>()
        .map_err(|_| boundary_error("Kwiry state path cannot be resolved"))
}

fn compare(
    roots: &[PathBuf],
    state: &[PathBuf],
    compare_rooms: bool,
    descriptor_mount_ids: Option<&[Option<u64>]>,
) -> Result<Vec<Failure>> {
    #[cfg(not(target_os = "linux"))]
    let _ = descriptor_mount_ids;
    let mut failures = vec![None; roots.len()];
    for (i, room) in roots.iter().enumerate() {
        for (j, other) in roots[..i].iter().enumerate() {
            if compare_rooms && overlap(room, other) {
                failures[i] = Some("room roots overlap");
                failures[j] = failures[i];
            }
        }
        if state.iter().any(|path| overlap(room, path)) {
            failures[i] = Some("room root overlaps Kwiry state");
        }
    }
    #[cfg(target_os = "linux")]
    linux::validate_mounts(
        roots,
        state,
        &mut failures,
        compare_rooms,
        descriptor_mount_ids,
    )?;
    Ok(failures)
}

fn boundary_error(message: &str) -> Error {
    Error::State(format!("openclast_room_boundary: {message}"))
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    struct Mount<'a> {
        id: &'a [u8],
        parent: &'a [u8],
        device: &'a [u8],
        root: PathBuf,
        point: PathBuf,
    }

    // Preserve non-UTF-8 bytes. mountinfo escapes whitespace and backslash.
    fn decode_path(field: &[u8]) -> Option<PathBuf> {
        let mut bytes = Vec::with_capacity(field.len());
        let mut remaining = field;
        while let Some((&byte, rest)) = remaining.split_first() {
            if byte == b'\\' {
                bytes.push(match rest.get(..3)? {
                    b"040" => b' ',
                    b"011" => b'\t',
                    b"012" => b'\n',
                    b"134" => b'\\',
                    _ => return None,
                });
                remaining = &rest[3..];
            } else {
                bytes.push(byte);
                remaining = rest;
            }
        }
        let path = PathBuf::from(OsString::from_vec(bytes));
        path.is_absolute().then_some(path)
    }

    fn parse_mounts(input: &[u8]) -> Result<Vec<Mount<'_>>> {
        input
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                let mut fields = line.split(|byte| *byte == b' ');
                (|| {
                    Some(Mount {
                        id: fields.next()?,
                        parent: fields.next()?,
                        device: fields.next()?,
                        root: decode_path(fields.next()?)?,
                        point: decode_path(fields.next()?)?,
                    })
                })()
                .ok_or_else(|| boundary_error("invalid mountinfo record"))
            })
            .collect()
    }

    fn location<'a>(mounts: &[Mount<'a>], path: &Path) -> Result<Location<'a>> {
        let mut roots = mounts.iter().filter(|mount| {
            mount.point == Path::new("/")
                && !mounts
                    .iter()
                    .any(|other| other.id == mount.parent && other.id != mount.id)
        });
        let mut mount = roots
            .next()
            .ok_or_else(|| boundary_error("mountinfo has no root mount"))?;
        if roots.next().is_some() || !path.starts_with(&mount.point) {
            return Err(boundary_error("ambiguous root mount"));
        }
        // Follow the mount tree: a visible overmount can hide a mount with a
        // longer textual prefix on the underlying filesystem.
        loop {
            let mut children: Vec<_> = mounts
                .iter()
                .filter(|child| {
                    child.parent == mount.id
                        && child.id != mount.id
                        && path.starts_with(&child.point)
                })
                .collect();
            children.sort_unstable_by_key(|child| child.point.components().count());
            let Some(next) = children.first() else { break };
            if children
                .get(1)
                .is_some_and(|other| other.point == next.point)
            {
                return Err(boundary_error("ambiguous covering mounts"));
            }
            mount = next;
        }
        let below_mount = path.strip_prefix(&mount.point).expect("covering mount");
        let mount_id = std::str::from_utf8(mount.id)
            .ok()
            .and_then(|id| id.parse().ok())
            .ok_or_else(|| boundary_error("invalid mount ID"))?;
        Ok(Location {
            mount_id,
            device: mount.device,
            path: mount.root.join(below_mount),
        })
    }

    struct Location<'a> {
        mount_id: u64,
        device: &'a [u8],
        path: PathBuf,
    }

    impl Location<'_> {
        fn overlaps(&self, other: &Self) -> bool {
            self.device == other.device && overlap(&self.path, &other.path)
        }
    }

    pub(super) fn validate_mounts(
        rooms: &[PathBuf],
        state: &[PathBuf],
        failures: &mut [Failure],
        compare_rooms: bool,
        descriptor_mount_ids: Option<&[Option<u64>]>,
    ) -> Result<()> {
        let bytes = fs::read("/proc/self/mountinfo")
            .map_err(|_| boundary_error("mountinfo is unavailable"))?;
        validate_mount_snapshot(
            &bytes,
            rooms,
            state,
            failures,
            compare_rooms,
            descriptor_mount_ids,
        )
    }

    fn validate_mount_snapshot(
        bytes: &[u8],
        rooms: &[PathBuf],
        state: &[PathBuf],
        failures: &mut [Failure],
        compare_rooms: bool,
        descriptor_mount_ids: Option<&[Option<u64>]>,
    ) -> Result<()> {
        let mounts = parse_mounts(bytes)?;
        let room_keys: Vec<_> = rooms.iter().map(|path| location(&mounts, path)).collect();
        let state = state
            .iter()
            .map(|path| location(&mounts, path))
            .collect::<Result<Vec<_>>>()?;
        for (i, key) in room_keys.iter().enumerate() {
            let Ok(room) = key else {
                failures[i] = Some("room mount could not be checked");
                continue;
            };
            for (j, other) in room_keys[..i].iter().enumerate() {
                if compare_rooms && other.as_ref().is_ok_and(|other| room.overlaps(other)) {
                    failures[i] = Some("room roots overlap through a mount");
                    failures[j] = failures[i];
                }
            }
            if state.iter().any(|path| room.overlaps(path)) {
                failures[i] = Some("room root overlaps Kwiry state through a mount");
            }
        }
        // A path-only location is not evidence about an already-open room.
        // Compare with the mount ID of that very descriptor from statx.
        if let Some(ids) = descriptor_mount_ids {
            for (i, key) in room_keys.iter().enumerate() {
                if let Ok(room) = key
                    && ids.get(i).copied().flatten() != Some(room.mount_id)
                {
                    failures[i] = Some("opened room mount does not match mountinfo");
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    #[test]
    fn room_mount_snapshot_requires_matching_descriptor_mount_id() {
        let snapshot = b"1 0 8:1 / / rw - ext4 fixture rw\n";
        let rooms = ["/rooms/A".into(), "/rooms/B".into()];
        for mount_id in [Some(1), Some(2), None] {
            let mut failures = [None; 2];
            validate_mount_snapshot(
                snapshot,
                &rooms,
                &[],
                &mut failures,
                true,
                Some(&[mount_id, Some(1)]),
            )
            .unwrap();
            assert_eq!(failures[0].is_none(), mount_id == Some(1));
            assert_eq!(failures[1], None, "an unrelated room remains available");
        }
    }

    #[cfg(test)]
    #[test]
    fn mountinfo_refuses_subtree_aliases_and_uses_component_boundaries() {
        let mountinfo = b"1 0 8:1 / / rw - ext4 fixture rw\n2 1 8:1 /rooms/B/sub\x5c040dir /aliases/A rw - ext4 fixture rw\n3 1 8:2 / /isolated rw - ext4 fixture rw\n4 1 8:1 /rooms/B /isolated/hidden rw - ext4 fixture rw\n";
        let a = PathBuf::from("/aliases/A");
        let check = |rooms: &[PathBuf], state: &[PathBuf]| {
            let mut failures = vec![None; rooms.len()];
            validate_mount_snapshot(mountinfo, rooms, state, &mut failures, true, None).unwrap();
            failures
                .into_iter()
                .map(|result| result.is_some())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            check(&[a.clone(), "/rooms/B".into(), "/rooms/C".into()], &[]),
            [true, true, false]
        );
        assert_eq!(
            check(std::slice::from_ref(&a), &["/rooms/B/sub dir".into()]),
            [true]
        );
        assert_eq!(
            check(&[a.clone(), "/rooms/B-extra".into()], &[]),
            [false, false]
        );
        assert_eq!(check(&[a, "/isolated/hidden".into()], &[]), [false, false]);
    }
}
