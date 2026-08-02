use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tantivy::Index;

use crate::error::{Error, Result, io_error};
use crate::manifest::{INDEX_FORMAT_VERSION, Manifest};
use crate::partition::GenerationLayout;
use crate::state::{read_json, sync_directory, sync_tree, write_json_atomic};

const LAYOUT_VERSION: u32 = 2;
const RETAINED_GENERATIONS: usize = 3;
static GENERATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    static STORAGE_PROBE_TEST_FAULT: std::cell::Cell<bool> = const {
        std::cell::Cell::new(false)
    };
}

#[cfg(test)]
fn inject_storage_probe_test_fault() -> Result<()> {
    STORAGE_PROBE_TEST_FAULT.with(|fault| {
        if fault.replace(false) {
            return Err(Error::State(
                "injected storage-probe checkpoint failure".to_owned(),
            ));
        }
        Ok(())
    })
}

#[derive(Debug, Clone)]
pub struct DataRoot {
    root: PathBuf,
}

impl DataRoot {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn acquire_writer_lock(&self) -> Result<DataRootLock> {
        fs::create_dir_all(&self.root).map_err(|error| io_error(&self.root, error))?;
        reject_known_network_filesystem(&self.root)?;

        let lock_path = self.root.join("daemon.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| io_error(&lock_path, error))?;
        file.try_lock_exclusive()
            .map_err(|_| Error::LockHeld(lock_path.clone()))?;

        self.remove_abandoned_probes()?;
        probe_storage_semantics(&self.root)?;
        self.prepare_locked()?;
        Ok(DataRootLock { file })
    }

    pub fn prepare(&self) -> Result<()> {
        drop(self.acquire_writer_lock()?);
        Ok(())
    }

    pub fn create_candidate(&self) -> Result<CandidateGeneration> {
        let id = generation_id()?;
        let generations = self.root.join("generations");
        fs::create_dir_all(&generations).map_err(|error| io_error(&generations, error))?;
        let staging_dir = generations.join(format!(".staging-{id}"));
        let index_dir = staging_dir.join("index");
        let partitions_dir = staging_dir.join("partitions");
        fs::create_dir_all(&index_dir).map_err(|error| io_error(&index_dir, error))?;
        fs::create_dir_all(&partitions_dir).map_err(|error| io_error(&partitions_dir, error))?;
        Ok(CandidateGeneration {
            id,
            staging_dir,
            index_dir,
            partitions_dir,
        })
    }

    pub fn create_candidate_from(&self, source: &GenerationPaths) -> Result<CandidateGeneration> {
        source.validate()?;
        let candidate = self.create_candidate()?;
        if let Err(error) = copy_tree_contents(&source.root, &candidate.staging_dir) {
            let _ = fs::remove_dir_all(&candidate.staging_dir);
            return Err(error);
        }
        Ok(candidate)
    }

    pub fn publish(&self, candidate: CandidateGeneration) -> Result<GenerationPaths> {
        self.publish_inner(candidate, None)
    }

    pub fn active(&self) -> Result<Option<GenerationPaths>> {
        let current_path = self.root.join("current.json");
        if !current_path.is_file() {
            return Ok(None);
        }
        let current: CurrentGeneration = read_json(&current_path)?;
        current.validate()?;
        let paths = GenerationPaths::new(
            current.generation.clone(),
            self.root.join("generations").join(&current.generation),
        );
        paths.validate()?;
        Ok(Some(paths))
    }

    pub fn active_or_legacy_index(&self) -> Result<PathBuf> {
        if let Some(active) = self.active()? {
            return Ok(active.index_dir);
        }
        if self.root.join("meta.json").is_file() {
            return Err(Error::State(format!(
                "unsupported legacy index layout at {}; expected index format {INDEX_FORMAT_VERSION}; run `kwiry index` to rebuild the disposable index",
                self.root.display()
            )));
        }
        Err(Error::Index(format!(
            "no index found at {}; run `kwiry index` first",
            self.root.display()
        )))
    }

    fn publish_inner(
        &self,
        candidate: CandidateGeneration,
        fault: Option<PublishFault>,
    ) -> Result<GenerationPaths> {
        let candidate_paths =
            GenerationPaths::new(candidate.id.clone(), candidate.staging_dir.clone());
        candidate_paths.validate()?;
        sync_tree(&candidate.staging_dir)?;
        inject_publish_fault(fault, PublishFault::CandidateSynced)?;

        let generations = self.root.join("generations");
        let final_dir = generations.join(&candidate.id);
        fs::rename(&candidate.staging_dir, &final_dir)
            .map_err(|error| io_error(&final_dir, error))?;
        sync_directory(&generations)?;
        inject_publish_fault(fault, PublishFault::GenerationRenamed)?;

        let current = CurrentGeneration {
            layout_version: LAYOUT_VERSION,
            index_format_version: INDEX_FORMAT_VERSION,
            generation: candidate.id.clone(),
        };
        write_json_atomic(&self.root.join("current.json"), &current)?;
        inject_publish_fault(fault, PublishFault::PointerWritten)?;

        let paths = GenerationPaths::new(candidate.id, final_dir);
        self.prune_generations(&paths.id);
        Ok(paths)
    }

    fn prepare_locked(&self) -> Result<()> {
        let generations = self.root.join("generations");
        fs::create_dir_all(&generations).map_err(|error| io_error(&generations, error))?;
        self.remove_abandoned_staging(&generations)?;

        match self.read_current() {
            CurrentState::Valid(active) => self.prune_generations(&active.id),
            CurrentState::Incompatible => {}
            CurrentState::MissingOrInvalid => {
                let valid = self.valid_generations()?;
                if let Some(active) = valid.first() {
                    let current = CurrentGeneration {
                        layout_version: LAYOUT_VERSION,
                        index_format_version: INDEX_FORMAT_VERSION,
                        generation: active.id.clone(),
                    };
                    write_json_atomic(&self.root.join("current.json"), &current)?;
                    self.prune_generations(&active.id);
                } else {
                    let current_path = self.root.join("current.json");
                    if current_path.exists() {
                        fs::remove_file(&current_path)
                            .map_err(|error| io_error(&current_path, error))?;
                        sync_directory(&self.root)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn read_current(&self) -> CurrentState {
        let current_path = self.root.join("current.json");
        if !current_path.is_file() {
            return CurrentState::MissingOrInvalid;
        }
        let Ok(current) = read_json::<CurrentGeneration>(&current_path) else {
            return CurrentState::MissingOrInvalid;
        };
        if !current.has_supported_versions() {
            return CurrentState::Incompatible;
        }
        if current.validate().is_err() {
            return CurrentState::MissingOrInvalid;
        }
        let paths = GenerationPaths::new(
            current.generation.clone(),
            self.root.join("generations").join(&current.generation),
        );
        if paths.validate().is_err() {
            return CurrentState::MissingOrInvalid;
        }
        CurrentState::Valid(paths)
    }

    fn valid_generations(&self) -> Result<Vec<GenerationPaths>> {
        let generations = self.root.join("generations");
        if !generations.is_dir() {
            return Ok(Vec::new());
        }
        let mut valid = Vec::new();
        let entries = fs::read_dir(&generations).map_err(|error| io_error(&generations, error))?;
        for entry in entries {
            let entry = entry.map_err(|error| io_error(&generations, error))?;
            let file_type = entry
                .file_type()
                .map_err(|error| io_error(entry.path(), error))?;
            if !file_type.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            if !is_generation_id(&id) {
                continue;
            }
            let paths = GenerationPaths::new(id, entry.path());
            if paths.validate().is_ok() {
                valid.push(paths);
            }
        }
        valid.sort_by(|left, right| generation_order(&right.id).cmp(&generation_order(&left.id)));
        Ok(valid)
    }

    fn remove_abandoned_probes(&self) -> Result<()> {
        let mut removed = false;
        let entries = fs::read_dir(&self.root).map_err(|error| io_error(&self.root, error))?;
        for entry in entries {
            let entry = entry.map_err(|error| io_error(&self.root, error))?;
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".storage-probe-")
            {
                continue;
            }
            let path = entry.path();
            if entry
                .file_type()
                .map_err(|error| io_error(&path, error))?
                .is_dir()
            {
                fs::remove_dir_all(&path).map_err(|error| io_error(&path, error))?;
            } else {
                fs::remove_file(&path).map_err(|error| io_error(&path, error))?;
            }
            removed = true;
        }
        if removed {
            sync_directory(&self.root)?;
        }
        Ok(())
    }

    fn remove_abandoned_staging(&self, generations: &Path) -> Result<()> {
        let mut removed = false;
        let entries = fs::read_dir(generations).map_err(|error| io_error(generations, error))?;
        for entry in entries {
            let entry = entry.map_err(|error| io_error(generations, error))?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(".staging-") {
                let path = entry.path();
                if entry
                    .file_type()
                    .map_err(|error| io_error(&path, error))?
                    .is_dir()
                {
                    fs::remove_dir_all(&path).map_err(|error| io_error(&path, error))?;
                } else {
                    fs::remove_file(&path).map_err(|error| io_error(&path, error))?;
                }
                removed = true;
            }
        }
        if removed {
            sync_directory(generations)?;
        }
        Ok(())
    }

    fn prune_generations(&self, active_id: &str) {
        let Ok(valid) = self.valid_generations() else {
            return;
        };
        let mut retained = BTreeSet::new();
        retained.insert(active_id.to_owned());
        for generation in valid
            .iter()
            .filter(|generation| generation.id != active_id)
            .filter(|generation| generation_order(&generation.id) < generation_order(active_id))
            .take(RETAINED_GENERATIONS.saturating_sub(1))
        {
            retained.insert(generation.id.clone());
        }

        let generations = self.root.join("generations");
        let Ok(entries) = fs::read_dir(&generations) else {
            return;
        };
        let mut removed = false;
        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            if !is_generation_id(&id) || retained.contains(&id) {
                continue;
            }
            let path = entry.path();
            let result = match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => fs::remove_dir_all(&path),
                Ok(_) => fs::remove_file(&path),
                Err(_) => continue,
            };
            if result.is_ok() {
                removed = true;
            }
        }
        if removed {
            let _ = sync_directory(&generations);
        }
    }

    #[cfg(test)]
    fn publish_with_fault(
        &self,
        candidate: CandidateGeneration,
        fault: PublishFault,
    ) -> Result<GenerationPaths> {
        self.publish_inner(candidate, Some(fault))
    }
}

#[derive(Debug)]
pub struct DataRootLock {
    file: File,
}

impl Drop for DataRootLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[derive(Debug)]
pub struct CandidateGeneration {
    pub id: String,
    pub staging_dir: PathBuf,
    pub index_dir: PathBuf,
    pub partitions_dir: PathBuf,
}

impl CandidateGeneration {
    pub fn manifest_path(&self) -> PathBuf {
        self.staging_dir.join("manifest.json")
    }

    pub fn layout_path(&self) -> PathBuf {
        self.staging_dir.join("layout.json")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationPaths {
    pub id: String,
    pub root: PathBuf,
    pub index_dir: PathBuf,
    pub partitions_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub layout_path: PathBuf,
}

impl GenerationPaths {
    fn new(id: String, root: PathBuf) -> Self {
        Self {
            id,
            index_dir: root.join("index"),
            partitions_dir: root.join("partitions"),
            manifest_path: root.join("manifest.json"),
            layout_path: root.join("layout.json"),
            root,
        }
    }

    fn validate(&self) -> Result<()> {
        if !is_generation_id(&self.id) || !self.root.is_dir() {
            return Err(Error::State(format!(
                "generation is missing or invalid: {}",
                self.root.display()
            )));
        }
        let manifest = Manifest::load(&self.manifest_path)?;
        let has_desktop_index = self.index_dir.join("meta.json").is_file();
        let has_partition_layout = self.layout_path.is_file();
        match (has_desktop_index, has_partition_layout) {
            (true, false) => {
                validate_index_dir(&self.index_dir)?;
                if manifest.files.values().any(|file| file.resource.is_some()) {
                    return Err(Error::State(format!(
                        "desktop generation contains resource-scoped manifest entries: {}",
                        self.root.display()
                    )));
                }
            }
            (false, true) => {
                let layout = GenerationLayout::load(&self.layout_path)?;
                for partition in &layout.partitions {
                    let index_dir = self
                        .partitions_dir
                        .join(&partition.partition_id)
                        .join("index");
                    let meta = index_dir.join("meta.json");
                    if !meta.is_file() {
                        return Err(Error::State(format!(
                            "generation partition is incomplete: {}",
                            meta.display()
                        )));
                    }
                    validate_index_dir(&index_dir)?;
                }
                for file in manifest.files.values() {
                    let Some(resource) = &file.resource else {
                        return Err(Error::State(format!(
                            "OpenClast manifest entry is missing its resource: {}",
                            file.path
                        )));
                    };
                    if resource.vault_id != file.vault_id {
                        return Err(Error::State(format!(
                            "OpenClast manifest entry does not match its vault: {}",
                            file.path
                        )));
                    }
                    // An entry whose resource is absent from the layout is retained
                    // source state whose content is deliberately withheld until the
                    // registration can be verified and reindexed.
                }
            }
            _ => {
                return Err(Error::State(format!(
                    "generation is incomplete or has ambiguous profile state: {}",
                    self.root.display()
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CurrentGeneration {
    layout_version: u32,
    index_format_version: u32,
    generation: String,
}

impl CurrentGeneration {
    fn has_supported_versions(&self) -> bool {
        self.layout_version == LAYOUT_VERSION && self.index_format_version == INDEX_FORMAT_VERSION
    }

    fn validate(&self) -> Result<()> {
        if !self.has_supported_versions() {
            return Err(Error::State(format!(
                "unsupported data layout: found layout={}, index={}; expected layout={LAYOUT_VERSION}, index={INDEX_FORMAT_VERSION}; run `kwiry index` to rebuild the disposable index",
                self.layout_version, self.index_format_version
            )));
        }
        if !is_generation_id(&self.generation) {
            return Err(Error::State(
                "invalid generation ID in current pointer".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
enum CurrentState {
    Valid(GenerationPaths),
    Incompatible,
    MissingOrInvalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishFault {
    CandidateSynced,
    GenerationRenamed,
    PointerWritten,
}

fn inject_publish_fault(fault: Option<PublishFault>, step: PublishFault) -> Result<()> {
    if fault == Some(step) {
        return Err(Error::State(format!(
            "injected publication interruption after {step:?}"
        )));
    }
    Ok(())
}

fn generation_id() -> Result<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::State(format!("system clock before Unix epoch: {error}")))?
        .as_nanos();
    let sequence = GENERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random)
        .map_err(|error| Error::State(format!("could not generate generation ID: {error}")))?;
    Ok(format!(
        "g-{nanos:039}-{:010}-{sequence:020}-{:020}",
        std::process::id(),
        u64::from_le_bytes(random)
    ))
}

fn is_generation_id(id: &str) -> bool {
    id.starts_with("g-") && !id.contains(['/', '\\']) && id != "g-" && id.len() <= 128
}

fn generation_order(id: &str) -> (u128, u64, &str) {
    let mut components = id.strip_prefix("g-").unwrap_or_default().split('-');
    let nanos = components
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or_default();
    let sequence = components
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or_default();
    (nanos, sequence, id)
}

fn validate_index_dir(index_dir: &Path) -> Result<()> {
    Index::open_in_dir(index_dir).map_err(|error| {
        Error::State(format!(
            "generation index is invalid at {}: {error}",
            index_dir.display()
        ))
    })?;
    Ok(())
}

fn copy_tree_contents(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source).map_err(|error| io_error(source, error))?;
    if metadata.file_type().is_symlink() {
        return Err(Error::State(format!(
            "cannot clone symbolic link from derived state: {}",
            source.display()
        )));
    }
    if metadata.is_file() {
        fs::copy(source, destination).map_err(|error| io_error(destination, error))?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(Error::State(format!(
            "cannot clone unsupported derived-state entry: {}",
            source.display()
        )));
    }

    fs::create_dir_all(destination).map_err(|error| io_error(destination, error))?;
    let mut entries = fs::read_dir(source)
        .map_err(|error| io_error(source, error))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| io_error(source, error))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        copy_tree_contents(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn probe_storage_semantics(root: &Path) -> Result<()> {
    let probe_id = generation_id()?;
    let probe_dir = root.join(format!(".storage-probe-{probe_id}"));
    let result = (|| -> Result<()> {
        fs::create_dir(&probe_dir).map_err(|error| io_error(&probe_dir, error))?;
        #[cfg(test)]
        inject_storage_probe_test_fault()?;

        let lock_path = probe_dir.join("lock");
        let first = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| io_error(&lock_path, error))?;
        first
            .try_lock_exclusive()
            .map_err(|error| io_error(&lock_path, error))?;
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| io_error(&lock_path, error))?;
        if second.try_lock_exclusive().is_ok() {
            return Err(Error::State(
                "exclusive locks are not enforced by the data-root filesystem".to_owned(),
            ));
        }

        let staging = probe_dir.join("staging");
        fs::create_dir(&staging).map_err(|error| io_error(&staging, error))?;
        let payload = staging.join("payload");
        fs::write(&payload, b"kwiry-storage-probe").map_err(|error| io_error(&payload, error))?;
        sync_tree(&staging)?;
        let complete = probe_dir.join("complete");
        fs::rename(&staging, &complete).map_err(|error| io_error(&complete, error))?;
        sync_directory(&probe_dir)?;

        let pointer = probe_dir.join("current.json");
        write_json_atomic(&pointer, &1_u8)?;
        write_json_atomic(&pointer, &2_u8)?;
        let value: u8 = read_json(&pointer)?;
        if value != 2 {
            return Err(Error::State(
                "atomic pointer replacement did not preserve the newest value".to_owned(),
            ));
        }
        Ok(())
    })();

    let _ = fs::remove_dir_all(&probe_dir);
    let _ = sync_directory(root);
    result.map_err(|error| Error::UnsuitableDataRoot {
        path: root.to_path_buf(),
        reason: error.to_string(),
    })
}

#[cfg(target_os = "linux")]
fn reject_known_network_filesystem(root: &Path) -> Result<()> {
    let Ok(canonical) = fs::canonicalize(root) else {
        return Ok(());
    };
    let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") else {
        return Ok(());
    };
    let Some(filesystem) = filesystem_for_path(&canonical, &mountinfo) else {
        return Ok(());
    };
    if is_known_network_filesystem(&filesystem) {
        return Err(Error::UnsuitableDataRoot {
            path: root.to_path_buf(),
            reason: format!(
                "filesystem type {filesystem} is not supported for derived state; use machine-local or local-block storage"
            ),
        });
    }
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
fn filesystem_for_path(path: &Path, mountinfo: &str) -> Option<String> {
    mountinfo
        .lines()
        .filter_map(|line| {
            let (mount, filesystem) = line.split_once(" - ")?;
            let mount_point = mount.split_whitespace().nth(4)?;
            let mount_point = PathBuf::from(decode_mount_path(mount_point));
            let filesystem = filesystem.split_whitespace().next()?.to_owned();
            path.starts_with(&mount_point)
                .then_some((mount_point.components().count(), filesystem))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, filesystem)| filesystem)
}

#[cfg(any(target_os = "linux", test))]
fn decode_mount_path(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[cfg(any(target_os = "linux", test))]
fn is_known_network_filesystem(filesystem: &str) -> bool {
    matches!(
        filesystem,
        "9p" | "cifs" | "drvfs" | "fuse.sshfs" | "nfs" | "nfs4" | "smb3" | "sshfs"
    )
}

#[cfg(windows)]
fn reject_known_network_filesystem(root: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Prefix};
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    use windows_sys::Win32::System::WindowsProgramming::DRIVE_REMOTE;

    let canonical = fs::canonicalize(root).map_err(|error| io_error(root, error))?;
    let drive_root = match canonical.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                Some(PathBuf::from(format!("{}:\\", char::from(letter))))
            }
            Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) => None,
            _ => Some(canonical.clone()),
        },
        _ => Some(canonical.clone()),
    };
    let remote = match drive_root {
        None => true,
        Some(drive_root) => {
            let mut wide: Vec<u16> = drive_root.as_os_str().encode_wide().collect();
            wide.push(0);
            unsafe { GetDriveTypeW(wide.as_ptr()) == DRIVE_REMOTE }
        }
    };
    if remote {
        return Err(Error::UnsuitableDataRoot {
            path: root.to_path_buf(),
            reason: "network drives are not supported for derived state; use machine-local or local-block storage".to_owned(),
        });
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn reject_known_network_filesystem(root: &Path) -> Result<()> {
    use std::ffi::{CStr, CString};
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    let canonical = fs::canonicalize(root).map_err(|error| io_error(root, error))?;
    let encoded =
        CString::new(canonical.as_os_str().as_bytes()).map_err(|_| Error::UnsuitableDataRoot {
            path: root.to_path_buf(),
            reason: "data-root path contains an embedded NUL byte".to_owned(),
        })?;
    let mut statistics = MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::statfs(encoded.as_ptr(), statistics.as_mut_ptr()) } != 0 {
        return Err(io_error(root, std::io::Error::last_os_error()));
    }
    let statistics = unsafe { statistics.assume_init() };
    if statistics.f_flags & libc::MNT_LOCAL as u32 == 0 {
        let filesystem =
            unsafe { CStr::from_ptr(statistics.f_fstypename.as_ptr()) }.to_string_lossy();
        return Err(Error::UnsuitableDataRoot {
            path: root.to_path_buf(),
            reason: format!(
                "filesystem type {filesystem} is not local; use machine-local or local-block storage"
            ),
        });
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn reject_known_network_filesystem(_root: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn complete_desktop_candidate(root: &DataRoot) -> CandidateGeneration {
        let candidate = root.create_candidate().unwrap();
        let schema = tantivy::schema::Schema::builder().build();
        Index::create_in_dir(&candidate.index_dir, schema).unwrap();
        Manifest::default()
            .save(&candidate.manifest_path())
            .unwrap();
        candidate
    }

    fn publish_complete_desktop(root: &DataRoot) -> GenerationPaths {
        root.publish(complete_desktop_candidate(root)).unwrap()
    }

    #[test]
    fn writer_lock_is_exclusive_and_storage_probe_is_cleaned_up() {
        let temporary = tempdir().unwrap();
        fs::create_dir(temporary.path().join(".storage-probe-abandoned")).unwrap();
        let root = DataRoot::new(temporary.path());
        let _lock = root.acquire_writer_lock().unwrap();
        assert!(matches!(
            root.acquire_writer_lock(),
            Err(Error::LockHeld(_))
        ));
        assert!(fs::read_dir(temporary.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".storage-probe-")
        }));
    }

    #[test]
    fn native_local_data_root_passes_platform_suitability_and_storage_probes() {
        let temporary = tempdir().unwrap();
        let root = DataRoot::new(temporary.path());

        root.prepare().unwrap();

        assert!(temporary.path().join("generations").is_dir());
        assert!(temporary.path().join("daemon.lock").is_file());
        assert!(fs::read_dir(temporary.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".storage-probe-")
        }));
    }

    #[test]
    fn storage_probe_checkpoint_failure_is_observed_and_cleaned_up() {
        let temporary = tempdir().unwrap();
        let root = DataRoot::new(temporary.path());
        STORAGE_PROBE_TEST_FAULT.with(|fault| fault.set(true));

        let error = root.prepare().unwrap_err();

        assert!(matches!(
            error,
            Error::UnsuitableDataRoot { ref reason, .. }
                if reason.contains("injected storage-probe checkpoint failure")
        ));
        assert!(fs::read_dir(temporary.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".storage-probe-")
        }));
    }

    #[test]
    fn native_writer_lock_is_exclusive_across_processes() {
        const CHILD_ROOT: &str = "KWIRY_TEST_WRITER_LOCK_CHILD_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            assert!(matches!(
                DataRoot::new(root).acquire_writer_lock(),
                Err(Error::LockHeld(_))
            ));
            return;
        }

        let temporary = tempdir().unwrap();
        let root = DataRoot::new(temporary.path());
        let _lock = root.acquire_writer_lock().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("generation::tests::native_writer_lock_is_exclusive_across_processes")
            .arg("--nocapture")
            .env(CHILD_ROOT, temporary.path())
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "child lock assertion failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn incompatible_index_pointer_is_rejected_without_mutation() {
        let temporary = tempdir().unwrap();
        let current_path = temporary.path().join("current.json");
        let source = r#"{"layout_version":2,"index_format_version":5,"generation":"old"}"#;
        fs::write(&current_path, source).unwrap();
        let root = DataRoot::new(temporary.path());

        let error = root.active().unwrap_err();
        assert!(error.to_string().contains("found layout=2, index=5"));
        assert!(error.to_string().contains("expected layout=2, index=9"));
        assert!(error.to_string().contains("kwiry index"));
        assert_eq!(fs::read_to_string(current_path).unwrap(), source);
    }

    #[test]
    fn incompatible_layout_pointer_is_rejected_without_mutation() {
        let temporary = tempdir().unwrap();
        let current_path = temporary.path().join("current.json");
        let source = r#"{"layout_version":1,"index_format_version":9,"generation":"old"}"#;
        fs::write(&current_path, source).unwrap();
        let root = DataRoot::new(temporary.path());

        let error = root.active().unwrap_err();
        assert!(error.to_string().contains("found layout=1, index=9"));
        assert!(error.to_string().contains("expected layout=2, index=9"));
        assert!(error.to_string().contains("kwiry index"));
        assert_eq!(fs::read_to_string(current_path).unwrap(), source);
    }

    #[test]
    fn legacy_index_requires_an_explicit_rebuild() {
        let temporary = tempdir().unwrap();
        let meta_path = temporary.path().join("meta.json");
        fs::write(&meta_path, "{}").unwrap();
        let root = DataRoot::new(temporary.path());

        let error = root.active_or_legacy_index().unwrap_err();
        assert!(error.to_string().contains("legacy index layout"));
        assert!(error.to_string().contains("expected index format 9"));
        assert!(error.to_string().contains("kwiry index"));
        assert_eq!(fs::read_to_string(meta_path).unwrap(), "{}");
    }

    #[test]
    fn publication_fault_before_rename_leaves_previous_generation_active() {
        let temporary = tempdir().unwrap();
        let root = DataRoot::new(temporary.path());
        let _lock = root.acquire_writer_lock().unwrap();
        let previous = publish_complete_desktop(&root);
        let candidate = complete_desktop_candidate(&root);

        assert!(
            root.publish_with_fault(candidate, PublishFault::CandidateSynced)
                .is_err()
        );
        root.prepare_locked().unwrap();

        assert_eq!(root.active().unwrap().unwrap().id, previous.id);
        assert!(
            fs::read_dir(temporary.path().join("generations"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".staging-"))
        );
    }

    #[test]
    fn publication_fault_after_rename_keeps_valid_previous_pointer() {
        let temporary = tempdir().unwrap();
        let root = DataRoot::new(temporary.path());
        let _lock = root.acquire_writer_lock().unwrap();
        let previous = publish_complete_desktop(&root);
        let candidate = complete_desktop_candidate(&root);

        assert!(
            root.publish_with_fault(candidate, PublishFault::GenerationRenamed)
                .is_err()
        );
        root.prepare_locked().unwrap();

        assert_eq!(root.active().unwrap().unwrap().id, previous.id);
    }

    #[test]
    fn publication_fault_after_pointer_write_selects_new_generation() {
        let temporary = tempdir().unwrap();
        let root = DataRoot::new(temporary.path());
        let _lock = root.acquire_writer_lock().unwrap();
        let previous = publish_complete_desktop(&root);
        let candidate = complete_desktop_candidate(&root);
        let candidate_id = candidate.id.clone();

        assert!(
            root.publish_with_fault(candidate, PublishFault::PointerWritten)
                .is_err()
        );
        root.prepare_locked().unwrap();

        let active = root.active().unwrap().unwrap();
        assert_eq!(active.id, candidate_id);
        assert_ne!(active.id, previous.id);
    }

    #[test]
    fn native_publication_fault_instrument_proves_each_recovery_boundary() {
        for (fault, candidate_becomes_active) in [
            (PublishFault::CandidateSynced, false),
            (PublishFault::GenerationRenamed, false),
            (PublishFault::PointerWritten, true),
        ] {
            let temporary = tempdir().unwrap();
            let root = DataRoot::new(temporary.path());
            let _lock = root.acquire_writer_lock().unwrap();
            let previous = publish_complete_desktop(&root);
            let candidate = complete_desktop_candidate(&root);
            let candidate_id = candidate.id.clone();

            let error = root.publish_with_fault(candidate, fault).unwrap_err();
            assert!(error.to_string().contains(&format!(
                "injected publication interruption after {fault:?}"
            )));
            root.prepare_locked().unwrap();

            let active = root.active().unwrap().unwrap();
            let expected = if candidate_becomes_active {
                &candidate_id
            } else {
                &previous.id
            };
            assert_eq!(&active.id, expected, "recovery mismatch after {fault:?}");
        }
    }

    #[test]
    fn missing_pointer_recovers_newest_complete_generation() {
        let temporary = tempdir().unwrap();
        let root = DataRoot::new(temporary.path());
        let _lock = root.acquire_writer_lock().unwrap();
        let first = publish_complete_desktop(&root);
        let second = publish_complete_desktop(&root);
        fs::remove_file(temporary.path().join("current.json")).unwrap();

        root.prepare_locked().unwrap();

        let active = root.active().unwrap().unwrap();
        assert_eq!(active.id, second.id);
        assert_ne!(active.id, first.id);
    }

    #[test]
    fn corrupt_pointer_recovers_valid_predecessor() {
        let temporary = tempdir().unwrap();
        let root = DataRoot::new(temporary.path());
        let _lock = root.acquire_writer_lock().unwrap();
        let valid = publish_complete_desktop(&root);
        fs::write(temporary.path().join("current.json"), "not-json").unwrap();
        let incomplete = temporary
            .path()
            .join("generations/g-999999999999999999999999999999");
        fs::create_dir_all(incomplete.join("index")).unwrap();

        root.prepare_locked().unwrap();

        assert_eq!(root.active().unwrap().unwrap().id, valid.id);
    }

    #[test]
    fn invalid_current_generation_recovers_a_valid_predecessor() {
        let temporary = tempdir().unwrap();
        let root = DataRoot::new(temporary.path());
        let _lock = root.acquire_writer_lock().unwrap();
        let valid = publish_complete_desktop(&root);
        let invalid = complete_desktop_candidate(&root);
        fs::write(invalid.index_dir.join("meta.json"), "not-json").unwrap();
        let invalid_id = invalid.id.clone();
        let invalid_root = temporary.path().join("generations").join(&invalid_id);
        fs::rename(invalid.staging_dir, invalid_root).unwrap();
        write_json_atomic(
            &temporary.path().join("current.json"),
            &CurrentGeneration {
                layout_version: LAYOUT_VERSION,
                index_format_version: INDEX_FORMAT_VERSION,
                generation: invalid_id,
            },
        )
        .unwrap();

        root.prepare_locked().unwrap();

        assert_eq!(root.active().unwrap().unwrap().id, valid.id);
    }

    #[test]
    fn retains_active_generation_and_two_predecessors() {
        let temporary = tempdir().unwrap();
        let root = DataRoot::new(temporary.path());
        let _lock = root.acquire_writer_lock().unwrap();
        let mut published = Vec::new();
        for _ in 0..5 {
            published.push(publish_complete_desktop(&root).id);
        }

        let retained: BTreeSet<_> = fs::read_dir(temporary.path().join("generations"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| is_generation_id(name))
            .collect();
        let expected: BTreeSet<_> = published.into_iter().rev().take(3).collect();
        assert_eq!(retained, expected);
    }

    #[test]
    fn candidate_clone_is_independent_of_the_active_generation() {
        let temporary = tempdir().unwrap();
        let root = DataRoot::new(temporary.path());
        let _lock = root.acquire_writer_lock().unwrap();
        let candidate = complete_desktop_candidate(&root);
        fs::write(candidate.index_dir.join("segment"), "original").unwrap();
        let active = root.publish(candidate).unwrap();

        let clone = root.create_candidate_from(&active).unwrap();
        fs::write(clone.index_dir.join("segment"), "changed").unwrap();

        assert_eq!(
            fs::read_to_string(active.index_dir.join("segment")).unwrap(),
            "original"
        );
        assert_eq!(
            fs::read_to_string(clone.index_dir.join("segment")).unwrap(),
            "changed"
        );
    }

    #[test]
    fn incomplete_partition_generation_is_rejected() {
        let temporary = tempdir().unwrap();
        let root = DataRoot::new(temporary.path());
        let candidate = root.create_candidate().unwrap();
        let resource = crate::model::ResourceKey::new("tenant", "vault", "room");
        GenerationLayout::openclast([resource])
            .unwrap()
            .save(&candidate.layout_path())
            .unwrap();
        Manifest::default()
            .save(&candidate.manifest_path())
            .unwrap();

        let error = root.publish(candidate).unwrap_err();
        assert!(error.to_string().contains("partition is incomplete"));
        assert!(root.active().unwrap().is_none());
    }

    #[test]
    fn network_filesystem_type_decision_logic_is_platform_independent() {
        for filesystem in [
            "9p",
            "cifs",
            "drvfs",
            "fuse.sshfs",
            "nfs",
            "nfs4",
            "smb3",
            "sshfs",
        ] {
            assert!(is_known_network_filesystem(filesystem));
        }
        for filesystem in ["btrfs", "ext4", "overlay", "tmpfs", "xfs"] {
            assert!(!is_known_network_filesystem(filesystem));
        }
    }

    #[test]
    fn mountinfo_decision_logic_uses_deepest_mount_and_decodes_paths() {
        let mountinfo = concat!(
            "1 0 0:1 / / rw - ext4 /dev/root rw\n",
            "2 1 0:2 / /mnt/shared\\040notes rw - cifs //server/share rw\n",
        );
        assert_eq!(
            filesystem_for_path(Path::new("/mnt/shared notes/vault"), mountinfo),
            Some("cifs".to_owned())
        );
        assert_eq!(
            filesystem_for_path(Path::new("/tmp/kwiry"), mountinfo),
            Some("ext4".to_owned())
        );
    }

    #[cfg(windows)]
    fn assert_windows_network_root_is_rejected(path: &Path) {
        let error = reject_known_network_filesystem(path).unwrap_err();
        assert!(matches!(
            error,
            Error::UnsuitableDataRoot { ref reason, .. }
                if reason.contains("network drives are not supported")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_local_temp_directory_exercises_drive_type_classification() {
        let temporary = tempdir().unwrap();
        reject_known_network_filesystem(temporary.path()).unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires KWIRY_TEST_WINDOWS_UNC_ROOT on an accessible UNC share"]
    fn windows_unc_prefix_is_rejected_on_a_real_share() {
        use std::path::{Component, Prefix};

        let base = PathBuf::from(
            std::env::var_os("KWIRY_TEST_WINDOWS_UNC_ROOT").expect(
                "KWIRY_TEST_WINDOWS_UNC_ROOT is required when this ignored native validation test is explicitly run",
            ),
        );
        assert!(
            matches!(
                base.components().next(),
                Some(Component::Prefix(prefix))
                    if matches!(prefix.kind(), Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _))
            ),
            "KWIRY_TEST_WINDOWS_UNC_ROOT must be a UNC path, got {}",
            base.display()
        );
        let temporary = tempfile::Builder::new()
            .prefix("kwiry-platform-validation-")
            .tempdir_in(&base)
            .unwrap_or_else(|error| {
                panic!(
                    "could not create a validation directory in {}: {error}",
                    base.display()
                )
            });

        assert_windows_network_root_is_rejected(temporary.path());
    }

    #[cfg(windows)]
    fn windows_drive_type(path: &Path) -> (PathBuf, u32) {
        use std::os::windows::ffi::OsStrExt;
        use std::path::{Component, Prefix};
        use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;

        let canonical = fs::canonicalize(path).unwrap();
        let drive_root = match canonical.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                    PathBuf::from(format!("{}:\\", char::from(letter)))
                }
                kind => panic!(
                    "mapped-drive validation canonicalized to non-drive prefix {kind:?}: {}",
                    canonical.display()
                ),
            },
            _ => panic!(
                "mapped-drive validation canonicalized without a Windows prefix: {}",
                canonical.display()
            ),
        };
        let mut wide: Vec<u16> = drive_root.as_os_str().encode_wide().collect();
        wide.push(0);
        let drive_type = unsafe { GetDriveTypeW(wide.as_ptr()) };
        (canonical, drive_type)
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires KWIRY_TEST_WINDOWS_REMOTE_DRIVE_ROOT on a mapped network drive"]
    fn windows_drive_remote_is_rejected_on_a_mapped_drive() {
        use std::path::{Component, Prefix};
        use windows_sys::Win32::System::WindowsProgramming::DRIVE_REMOTE;

        let base = PathBuf::from(
            std::env::var_os("KWIRY_TEST_WINDOWS_REMOTE_DRIVE_ROOT").expect(
                "KWIRY_TEST_WINDOWS_REMOTE_DRIVE_ROOT is required when this ignored native validation test is explicitly run",
            ),
        );
        assert!(
            matches!(
                base.components().next(),
                Some(Component::Prefix(prefix))
                    if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
            ),
            "KWIRY_TEST_WINDOWS_REMOTE_DRIVE_ROOT must be a drive-letter path, got {}",
            base.display()
        );
        let temporary = tempfile::Builder::new()
            .prefix("kwiry-platform-validation-")
            .tempdir_in(&base)
            .unwrap_or_else(|error| {
                panic!(
                    "could not create a validation directory in {}: {error}",
                    base.display()
                )
            });
        let (canonical, drive_type) = windows_drive_type(temporary.path());
        assert!(
            matches!(
                canonical.components().next(),
                Some(Component::Prefix(prefix))
                    if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
            ),
            "mapped-drive temporary directory canonicalized away from a drive prefix: {}",
            canonical.display()
        );
        assert_eq!(
            drive_type,
            DRIVE_REMOTE,
            "GetDriveTypeW did not classify {} as DRIVE_REMOTE",
            canonical.display()
        );

        assert_windows_network_root_is_rejected(temporary.path());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_local_temp_directory_exercises_statfs_mnt_local() {
        let temporary = tempdir().unwrap();
        reject_known_network_filesystem(temporary.path()).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires KWIRY_TEST_MACOS_NETWORK_ROOT on a mount without MNT_LOCAL"]
    fn macos_nonlocal_mount_is_rejected() {
        let base = PathBuf::from(
            std::env::var_os("KWIRY_TEST_MACOS_NETWORK_ROOT").expect(
                "KWIRY_TEST_MACOS_NETWORK_ROOT is required when this ignored native validation test is explicitly run",
            ),
        );
        let temporary = tempfile::Builder::new()
            .prefix("kwiry-platform-validation-")
            .tempdir_in(&base)
            .unwrap_or_else(|error| {
                panic!(
                    "could not create a validation directory in {}: {error}",
                    base.display()
                )
            });
        let error = reject_known_network_filesystem(temporary.path()).unwrap_err();
        assert!(matches!(
            error,
            Error::UnsuitableDataRoot { ref reason, .. }
                if reason.contains("is not local")
        ));
    }
}
