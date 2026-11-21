//! The daemon's list-only holder adapters: configured rooms, the index
//! data-root marker and a log-only event sink.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kwiry_core::Config;
use kwiry_holdings::events::{EventSink, HolderEvent};
use kwiry_holdings::startup::{ContentState, DataRoot, DataRootEvidence, MarkerState};
use kwiry_holdings::store::{HeldResource, PublishPlan, ResourceKey, ResourceStore, StoreError};

/// The resource kind Kwiry holds: one room per registered vault.
pub(crate) const ROOM_KIND: &str = "room";

/// The holder marker in the data root (the startup loss/reset rules).
pub(crate) const MARKER_FILE: &str = "holder-marker.json";

/// The rooms the configuration names. The classification an applied list
/// produces is held by the holder's gate; in this build the serving path does
/// not consult it per resource yet, so publishing records nothing, and
/// nothing is ever deleted.
pub(crate) struct ConfiguredRooms {
    rooms: Vec<String>,
}

impl ConfiguredRooms {
    pub(crate) fn from_config(config: &Config) -> Self {
        let mut rooms: Vec<String> = config
            .vaults
            .iter()
            .filter_map(|vault| vault.room.clone())
            .collect();
        rooms.sort();
        rooms.dedup();
        Self { rooms }
    }
}

impl ResourceStore for ConfiguredRooms {
    fn held(&self) -> Result<Vec<HeldResource>, StoreError> {
        Ok(self
            .rooms
            .iter()
            .map(|room| HeldResource {
                key: ResourceKey {
                    kind: ROOM_KIND.to_owned(),
                    id: room.clone(),
                },
                configured: true,
                units: 1,
            })
            .collect())
    }

    fn publish(&mut self, _plan: &PublishPlan) -> Result<(), StoreError> {
        Ok(())
    }

    fn revoke(&mut self, _plan: &PublishPlan) {}
}

/// The index data root as the holder's startup rules see it.
pub(crate) struct IndexDataRoot {
    data_dir: PathBuf,
}

impl IndexDataRoot {
    pub(crate) fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
        }
    }
}

impl DataRoot for IndexDataRoot {
    fn inspect(&self, holder_id: &str) -> Result<DataRootEvidence, String> {
        let content = if kwiry_core::DataRoot::new(&self.data_dir)
            .has_index_entries()
            .map_err(|_| "data root unreadable".to_owned())?
        {
            ContentState::Intact
        } else {
            ContentState::Lost
        };
        let marker = match fs::read(self.data_dir.join(MARKER_FILE)) {
            Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(value) if value["holder_id"].as_str() == Some(holder_id) => MarkerState::Matches,
                Ok(value) if value["holder_id"].is_string() => MarkerState::Other,
                _ => MarkerState::Unreadable,
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => MarkerState::Absent,
            Err(_) => MarkerState::Unreadable,
        };
        Ok(DataRootEvidence { content, marker })
    }

    fn bind(&self, holder_id: &str) -> Result<(), String> {
        fs::create_dir_all(&self.data_dir).map_err(|_| "data root not creatable".to_owned())?;
        let path = self.data_dir.join(MARKER_FILE);
        let bytes = serde_json::to_vec(&serde_json::json!({ "holder_id": holder_id }))
            .map_err(|_| "marker encoding".to_owned())?;
        // Never overwrite another holder's marker.
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return match self.inspect(holder_id)?.marker {
                    MarkerState::Matches => Ok(()),
                    _ => Err("data root bound to another holder".to_owned()),
                };
            }
            Err(_) => return Err("marker not writable".to_owned()),
        };
        use std::io::Write;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| "marker not writable".to_owned())?;
        if let Ok(dir) = fs::File::open(&self.data_dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

/// Logs holder events by code. Events carry codes and counts only, never a
/// resource id, pin, nonce, digest or key. `holder_state_unwritable` also
/// trips the process fail-stop.
pub(crate) struct LogSink {
    fail_stop: Arc<dyn Fn() + Send + Sync>,
}

impl LogSink {
    pub(crate) fn new(fail_stop: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { fail_stop }
    }
}

impl EventSink for LogSink {
    fn emit(&self, event: HolderEvent) {
        let code = event.code();
        match event {
            HolderEvent::HolderStateUnwritable => {
                tracing::error!(code, "holder state could not be made durable");
                (self.fail_stop)();
            }
            HolderEvent::KeysetAccepted { .. }
            | HolderEvent::ListRequestInvalidCleared
            | HolderEvent::ReportResumed
            | HolderEvent::StatusResumed
            | HolderEvent::DiskReserveUsed => tracing::info!(code, "holder event"),
            _ => tracing::warn!(code, "holder alert"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_root_is_lost_only_when_it_holds_no_index_content() {
        let dir = tempfile::tempdir().unwrap();
        let root = IndexDataRoot::new(&dir.path().join("data"));
        let lost = DataRootEvidence {
            content: ContentState::Lost,
            marker: MarkerState::Absent,
        };
        assert_eq!(root.inspect("hd_a").unwrap(), lost);
        fs::create_dir_all(root.data_dir.join("generations")).unwrap();
        for name in [
            "daemon.lock",
            "replay-epoch",
            "replay-iat-watermark",
            "other",
        ] {
            fs::write(root.data_dir.join(name), b"").unwrap();
        }
        assert_eq!(root.inspect("hd_a").unwrap(), lost);
        root.bind("hd_a").unwrap();
        assert_eq!(root.inspect("hd_a").unwrap().marker, MarkerState::Matches);
        assert_eq!(root.inspect("hd_b").unwrap().marker, MarkerState::Other);
        assert!(root.bind("hd_b").is_err());
        fs::create_dir_all(dir.path().join("data/generations/g-1")).unwrap();
        assert_eq!(root.inspect("hd_a").unwrap().content, ContentState::Intact);
    }
}
