use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::error::{Error, Result, io_error};

pub const CONNECTION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConnectionDescriptor {
    pub schema_version: u32,
    pub url: String,
    pub token_file: PathBuf,
    pub daemon_version: String,
}

impl ConnectionDescriptor {
    pub fn new(
        address: SocketAddr,
        token_file: PathBuf,
        daemon_version: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: CONNECTION_SCHEMA_VERSION,
            url: format!("http://{address}"),
            token_file,
            daemon_version: daemon_version.into(),
        }
    }
}

pub fn write_connection_descriptor(path: &Path, descriptor: &ConnectionDescriptor) -> Result<()> {
    let parent = path.parent().ok_or_else(|| Error::InvalidConfig {
        path: path.to_path_buf(),
        message: "connection descriptor path has no parent".to_owned(),
    })?;
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;

    let mut encoded =
        serde_json::to_vec_pretty(descriptor).map_err(|error| Error::InvalidConfig {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    encoded.push(b'\n');
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| io_error(parent, error))?;
    temporary
        .write_all(&encoded)
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

pub fn load_connection_descriptor(path: &Path) -> Result<ConnectionDescriptor> {
    let source = fs::read(path).map_err(|error| io_error(path, error))?;
    serde_json::from_slice(&source).map_err(|error| Error::InvalidConfig {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
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

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn descriptor_round_trips_without_a_token_value() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("state/connection.json");
        let token_path = temporary.path().join("state/config.token");
        let sentinel_token = "sentinel-token-must-not-be-serialized";
        let descriptor = ConnectionDescriptor::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 32189),
            token_path.clone(),
            "1.2.3",
        );

        write_connection_descriptor(&path, &descriptor).unwrap();
        let encoded = fs::read_to_string(&path).unwrap();

        assert!(!encoded.contains(sentinel_token));
        assert!(encoded.ends_with('\n'));
        assert_eq!(load_connection_descriptor(&path).unwrap(), descriptor);
        assert_eq!(descriptor.schema_version, CONNECTION_SCHEMA_VERSION);
        assert_eq!(descriptor.url, "http://127.0.0.1:32189");
        assert_eq!(descriptor.token_file, token_path);
    }
}
