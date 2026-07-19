use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tempfile::NamedTempFile;

use crate::error::{Error, Result, io_error};

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let source = fs::read(path).map_err(|error| io_error(path, error))?;
    serde_json::from_slice(&source)
        .map_err(|error| Error::State(format!("invalid JSON at {}: {error}", path.display())))
}

pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::State(format!("state path has no parent: {}", path.display())))?;
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    let encoded = serde_json::to_vec_pretty(value)
        .map_err(|error| Error::State(format!("could not encode {}: {error}", path.display())))?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| io_error(parent, error))?;
    temporary
        .write_all(&encoded)
        .map_err(|error| io_error(temporary.path(), error))?;
    temporary
        .write_all(b"\n")
        .map_err(|error| io_error(temporary.path(), error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| io_error(temporary.path(), error))?;
    temporary
        .persist(path)
        .map_err(|error| io_error(path, error.error))?;
    sync_parent(parent)?;
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_error(path, error))
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}
