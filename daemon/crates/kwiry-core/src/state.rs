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
    sync_directory(parent)?;
    Ok(())
}

pub(crate) fn sync_tree(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(Error::State(format!(
            "derived-state tree contains a symbolic link: {}",
            path.display()
        )));
    }
    if metadata.is_file() {
        return File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| io_error(path, error));
    }
    if !metadata.is_dir() {
        return Err(Error::State(format!(
            "derived-state tree contains an unsupported entry: {}",
            path.display()
        )));
    }

    let mut entries = fs::read_dir(path)
        .map_err(|error| io_error(path, error))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| io_error(path, error))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        sync_tree(&entry.path())?;
    }
    sync_directory(path)
}

#[cfg(unix)]
pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_error(path, error))
}

#[cfg(not(unix))]
pub(crate) fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}
