use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, io_error};
use crate::manifest::INDEX_FORMAT_VERSION;
use crate::state::{read_json, write_json_atomic};

const LAYOUT_VERSION: u32 = 1;

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
        Ok(DataRootLock { file })
    }

    pub fn create_candidate(&self) -> Result<CandidateGeneration> {
        let id = generation_id()?;
        let generations = self.root.join("generations");
        fs::create_dir_all(&generations).map_err(|error| io_error(&generations, error))?;
        let staging_dir = generations.join(format!(".staging-{id}"));
        let index_dir = staging_dir.join("index");
        fs::create_dir_all(&index_dir).map_err(|error| io_error(&index_dir, error))?;
        Ok(CandidateGeneration {
            id,
            staging_dir,
            index_dir,
        })
    }

    pub fn publish(&self, candidate: CandidateGeneration) -> Result<GenerationPaths> {
        let final_dir = self.root.join("generations").join(&candidate.id);
        fs::rename(&candidate.staging_dir, &final_dir)
            .map_err(|error| io_error(&final_dir, error))?;
        let current = CurrentGeneration {
            layout_version: LAYOUT_VERSION,
            index_format_version: INDEX_FORMAT_VERSION,
            generation: candidate.id.clone(),
        };
        write_json_atomic(&self.root.join("current.json"), &current)?;
        Ok(GenerationPaths::new(candidate.id, final_dir))
    }

    pub fn active(&self) -> Result<Option<GenerationPaths>> {
        let current_path = self.root.join("current.json");
        if !current_path.is_file() {
            return Ok(None);
        }
        let current: CurrentGeneration = read_json(&current_path)?;
        current.validate()?;
        let generation_dir = self.root.join("generations").join(&current.generation);
        let paths = GenerationPaths::new(current.generation, generation_dir);
        if !paths.index_dir.join("meta.json").is_file() || !paths.manifest_path.is_file() {
            return Err(Error::State(format!(
                "active generation is incomplete: {}",
                paths.root.display()
            )));
        }
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
}

impl CandidateGeneration {
    pub fn manifest_path(&self) -> PathBuf {
        self.staging_dir.join("manifest.json")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationPaths {
    pub id: String,
    pub root: PathBuf,
    pub index_dir: PathBuf,
    pub manifest_path: PathBuf,
}

impl GenerationPaths {
    fn new(id: String, root: PathBuf) -> Self {
        Self {
            id,
            index_dir: root.join("index"),
            manifest_path: root.join("manifest.json"),
            root,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CurrentGeneration {
    layout_version: u32,
    index_format_version: u32,
    generation: String,
}

impl CurrentGeneration {
    fn validate(&self) -> Result<()> {
        if self.layout_version != LAYOUT_VERSION
            || self.index_format_version != INDEX_FORMAT_VERSION
        {
            return Err(Error::State(format!(
                "unsupported data layout: found layout={}, index={}; expected layout={LAYOUT_VERSION}, index={INDEX_FORMAT_VERSION}; run `kwiry index` to rebuild the disposable index",
                self.layout_version, self.index_format_version
            )));
        }
        Ok(())
    }
}

fn generation_id() -> Result<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::State(format!("system clock before Unix epoch: {error}")))?
        .as_nanos();
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random)
        .map_err(|error| Error::State(format!("could not generate generation ID: {error}")))?;
    Ok(format!(
        "g-{nanos}-{}-{}",
        std::process::id(),
        u64::from_le_bytes(random)
    ))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn writer_lock_is_exclusive() {
        let temporary = tempdir().unwrap();
        let root = DataRoot::new(temporary.path());
        let _lock = root.acquire_writer_lock().unwrap();
        assert!(matches!(
            root.acquire_writer_lock(),
            Err(Error::LockHeld(_))
        ));
    }

    #[test]
    fn incompatible_index_pointer_is_rejected_without_mutation() {
        let temporary = tempdir().unwrap();
        let current_path = temporary.path().join("current.json");
        let source = r#"{"layout_version":1,"index_format_version":2,"generation":"old"}"#;
        fs::write(&current_path, source).unwrap();
        let root = DataRoot::new(temporary.path());

        let error = root.active().unwrap_err();
        assert!(error.to_string().contains("found layout=1, index=2"));
        assert!(error.to_string().contains("expected layout=1, index=3"));
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
        assert!(error.to_string().contains("expected index format 3"));
        assert!(error.to_string().contains("kwiry index"));
        assert_eq!(fs::read_to_string(meta_path).unwrap(), "{}");
    }
}
