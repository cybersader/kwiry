//! The daemon's holder adapters: what Kwiry holds, a census of its retained
//! index data, its data-root marker and a log-only event sink.
//!
//! **No adapter here deletes or reserves anything.** [`CensusOnlyPurger`]
//! observes the data root and refuses `detach`, `reserve` and `delete` with a
//! retryable error. Production deletion is a separate, separately reviewed
//! adapter.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kwiry_core::{Config, freeze_generation_publications};
use kwiry_holdings::events::{EventSink, HolderEvent};
use kwiry_holdings::startup::{ContentState, DataRoot, DataRootEvidence, MarkerState};
use kwiry_holdings::store::{
    ArtifactPurger, Census, Claimant, DeleteOutcome, HeldResource, ManifestUnits, ObservedUnit,
    PublicationFreeze, PublishPlan, ReserveOutcome, ResourceKey, ResourceStore, StoreError,
};

/// The resource kind Kwiry holds: one room per registered vault.
pub(crate) const ROOM_KIND: &str = "room";

/// The holder marker in the data root (the startup loss/reset rules).
pub(crate) const MARKER_FILE: &str = "holder-marker.json";

/// The refusal every deleting operation returns in this build.
pub(crate) const DELETION_DISABLED: &str = "deletion is not enabled in this build";

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

/// Observes the data root; never detaches, reserves or deletes.
///
/// Every published generation and every staging build is one unit. Unit ids
/// are the generation directory names (random, never a room id), and each
/// names one immutable generation. The census cannot tell which room's bytes
/// a generation holds, so every unit is unattributed: no obligation can ever
/// claim one, and a report can never say `complete` while one remains.
pub(crate) struct CensusOnlyPurger {
    data_dir: PathBuf,
}

impl CensusOnlyPurger {
    pub(crate) fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
        }
    }

    fn refuse(step: &'static str) -> StoreError {
        tracing::warn!(step, "holder {step} refused: {DELETION_DISABLED}");
        StoreError(DELETION_DISABLED.to_owned())
    }
}

impl ArtifactPurger for CensusOnlyPurger {
    fn detach(&mut self, _resource: &ResourceKey) -> Result<(), StoreError> {
        Err(Self::refuse("detach"))
    }

    fn delete(
        &mut self,
        _unit_id: &str,
        _claimant: &Claimant,
    ) -> Result<DeleteOutcome, StoreError> {
        Err(Self::refuse("delete"))
    }

    fn reserve(
        &mut self,
        _instruction_id: &str,
        _units: &ManifestUnits,
    ) -> Result<ReserveOutcome, StoreError> {
        Err(Self::refuse("reserve"))
    }

    fn barrier(&mut self) -> Result<(), StoreError> {
        Ok(())
    }

    fn census(&self) -> Result<Census, StoreError> {
        let generations = self.data_dir.join("generations");
        let entries = match fs::read_dir(&generations) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Census {
                    locations: vec!["generations".to_owned()],
                    units: Vec::new(),
                });
            }
            Err(_) => return Err(StoreError("census: data root unreadable".to_owned())),
        };
        let mut units = BTreeMap::new();
        for entry in entries {
            let entry = entry.map_err(|_| StoreError("census: entry unreadable".to_owned()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = match name.strip_prefix(".staging-") {
                Some(_) => "staging",
                None if name.starts_with("g-") => "generation",
                // Anything else in the generations directory is still
                // retained data the census must count.
                None => "other",
            };
            units.insert(
                format!("{kind}:{name}"),
                ObservedUnit {
                    unit_id: format!("{kind}:{name}"),
                    attribution: None,
                    open_handles: 0,
                    reserved: false,
                },
            );
        }
        Ok(Census {
            locations: vec!["generations".to_owned()],
            units: units.into_values().collect(),
        })
    }

    fn freeze_publications(&self) -> Result<PublicationFreeze<'_>, StoreError> {
        Ok(PublicationFreeze::new(freeze_generation_publications()))
    }
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

/// Entries that are never index content: the marker, the holder snapshot,
/// locks, capability replay state, logs and the connection descriptor that
/// share a custom data directory.
const NON_CONTENT: &[&str] = &[
    MARKER_FILE,
    super::driver::SNAPSHOT_FILE,
    "daemon.lock",
    "replay.lock",
    "replay-epoch",
    "replay-iat-watermark",
    "logs",
    "connection.json",
    "setup.lock",
];

impl DataRoot for IndexDataRoot {
    fn inspect(&self, holder_id: &str) -> Result<DataRootEvidence, String> {
        let entries = match fs::read_dir(&self.data_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DataRootEvidence {
                    content: ContentState::Lost,
                    marker: MarkerState::Absent,
                });
            }
            Err(_) => return Err("data root unreadable".to_owned()),
        };
        let mut content = ContentState::Lost;
        for entry in entries {
            let Ok(entry) = entry else {
                content = ContentState::Intact;
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if NON_CONTENT.contains(&name.as_ref()) {
                continue;
            }
            // An empty generations directory holds nothing.
            if name == "generations"
                && fs::read_dir(entry.path()).is_ok_and(|mut inner| inner.next().is_none())
            {
                continue;
            }
            content = ContentState::Intact;
        }
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

    fn key() -> ResourceKey {
        ResourceKey {
            kind: ROOM_KIND.into(),
            id: "room-test-1".into(),
        }
    }

    #[test]
    fn the_census_only_purger_refuses_every_deleting_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut purger = CensusOnlyPurger::new(dir.path());
        assert!(purger.detach(&key()).is_err());
        assert!(purger.reserve("in_x", &ManifestUnits::new()).is_err());
        let claimant = kwiry_holdings::store::Claimant::Obligation {
            obligation_id: "ob_x".into(),
            resource: key(),
        };
        assert!(purger.delete("generation:g-1", &claimant).is_err());
        assert!(purger.barrier().is_ok());
    }

    #[test]
    fn the_census_counts_every_generation_unattributed() {
        let dir = tempfile::tempdir().unwrap();
        let purger = CensusOnlyPurger::new(dir.path());
        assert!(purger.census().unwrap().units.is_empty());
        fs::create_dir_all(dir.path().join("generations/g-1")).unwrap();
        fs::create_dir_all(dir.path().join("generations/.staging-g-2")).unwrap();
        let census = purger.census().unwrap();
        let ids: Vec<_> = census.units.iter().map(|u| u.unit_id.as_str()).collect();
        assert_eq!(ids, ["generation:g-1", "staging:.staging-g-2"]);
        assert!(census.units.iter().all(|u| u.attribution.is_none()));
        drop(purger.freeze_publications().unwrap());
    }

    #[test]
    fn the_data_root_is_lost_only_when_it_holds_no_index_content() {
        let dir = tempfile::tempdir().unwrap();
        let root = IndexDataRoot::new(&dir.path().join("data"));
        let lost = DataRootEvidence {
            content: ContentState::Lost,
            marker: MarkerState::Absent,
        };
        assert_eq!(root.inspect("hd_a").unwrap(), lost);
        fs::create_dir_all(dir.path().join("data/generations")).unwrap();
        fs::write(dir.path().join("data/daemon.lock"), b"").unwrap();
        assert_eq!(root.inspect("hd_a").unwrap(), lost);
        root.bind("hd_a").unwrap();
        assert_eq!(root.inspect("hd_a").unwrap().marker, MarkerState::Matches);
        assert_eq!(root.inspect("hd_b").unwrap().marker, MarkerState::Other);
        assert!(root.bind("hd_b").is_err());
        fs::create_dir_all(dir.path().join("data/generations/g-1")).unwrap();
        assert_eq!(root.inspect("hd_a").unwrap().content, ContentState::Intact);
    }
}
