use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::model::{HostProfile, IngestWarning, ResourceKey, SnapshotId};
use crate::state::{read_json, write_json_atomic};

use crate::generation::{CandidateGeneration, GENERATED_ID_BYTES, GenerationPaths, LAYOUT_VERSION};

pub(crate) const LABEL_SIDECAR: &str = "snapshot-labels.json";
const LABEL_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotLabels {
    version: u32,
    generation: String,
    labels: Vec<LabelBinding>,
}

// The sidecar is disposable. Never allocate more than this bound for its bytes.
pub(crate) const MAX_LABEL_BYTES: u64 = 1024 * 1024;
const MAX_LABEL_RESOURCE_JSON_BYTES: usize = 260;
// The encoded resource tuple uses 10 bytes for quotes, commas and brackets,
// leaving at most 250 escaped value bytes. In the pretty sidecar each binding
// needs at most 250 + 64 (partition hash) + 128 (label) + 163 (JSON and separator)
// bytes. Reserve 151 bytes for the envelope (94-byte generation ID and newline
// included); subtract one more byte to stay strictly below the reader cap.
pub(crate) const MAX_LABELED_ROOMS: usize = (MAX_LABEL_BYTES as usize - 151 - 1) / 605;

pub(crate) fn label_resource_fits(tenant_id: &str, vault_id: &str, room_id: &str) -> bool {
    serde_json::to_vec(&[tenant_id, vault_id, room_id])
        .is_ok_and(|bytes| bytes.len() <= MAX_LABEL_RESOURCE_JSON_BYTES)
}

#[cfg(test)]
thread_local! { pub(crate) static FAIL_LABEL_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

impl SnapshotLabels {
    fn fit(&self) -> Result<bool> {
        if self.labels.is_empty() {
            return Ok(true);
        }
        let encoded = serde_json::to_vec_pretty(self)
            .map_err(|error| Error::State(format!("could not encode snapshot labels: {error}")))?;
        // write_json_atomic appends one newline.
        Ok((encoded.len() as u64) < MAX_LABEL_BYTES)
    }

    fn read(path: &Path) -> Option<Self> {
        use std::io::Read;
        #[cfg(test)]
        if FAIL_LABEL_READ.with(|cell| cell.replace(cell.get().saturating_sub(1))) == 1 {
            return None;
        }
        let meta = path.symlink_metadata().ok()?;
        if !meta.is_file() || meta.len() > MAX_LABEL_BYTES {
            return None;
        }
        let file = std::fs::File::open(path).ok()?;
        let mut bytes = Vec::new();
        file.take(MAX_LABEL_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_LABEL_BYTES {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "ResourceKey", deny_unknown_fields)]
struct StrictResource {
    tenant_id: String,
    vault_id: String,
    room_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LabelBinding {
    #[serde(with = "StrictResource")]
    resource: ResourceKey,
    partition_id: String,
    snapshot_id: SnapshotId,
}

pub(crate) enum PartitionEvidence<'a> {
    Walk {
        configured: Option<&'a SnapshotId>,
        clean_and_exact: bool,
    },
    UnchangedLiveRows(Option<&'a SnapshotId>),
    NoWalk,
}

impl PartitionEvidence<'_> {
    /// Certify a newly built or changed partition iff the producing pass walked the
    /// entire room, read and indexed every eligible source cleanly, and its exact
    /// per-source chunk counts and whole-file hashes match the live partition. Without this evidence the
    /// label is absent. Maintenance may carry the previous proof only when it
    /// preserves exactly the same live rows (e.g. compacting deleted documents).
    pub(crate) fn publication_label(&self) -> Option<SnapshotId> {
        match self {
            Self::Walk {
                configured,
                clean_and_exact: true,
            }
            | Self::UnchangedLiveRows(configured) => configured.cloned(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GenerationLayout {
    pub layout_version: u32,
    pub profile: HostProfile,
    pub partitions: Vec<PartitionLayout>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PartitionLayout {
    pub partition_id: String,
    pub resource: ResourceKey,
    #[serde(skip)]
    pub snapshot_id: Option<SnapshotId>,
}

impl GenerationLayout {
    #[cfg(test)]
    pub(crate) fn openclast(resources: impl IntoIterator<Item = ResourceKey>) -> Result<Self> {
        Self::with_snapshots(resources.into_iter().map(|r| (r, None)).collect())
    }

    pub(crate) fn with_snapshots(
        resources: BTreeMap<ResourceKey, Option<SnapshotId>>,
    ) -> Result<Self> {
        let layout = Self {
            layout_version: LAYOUT_VERSION,
            profile: HostProfile::OpenClast,
            partitions: resources
                .into_iter()
                .map(|(resource, snapshot_id)| PartitionLayout {
                    snapshot_id,
                    partition_id: partition_id(&resource),
                    resource,
                })
                .collect(),
        };
        layout.validate()?;
        Ok(layout)
    }

    pub(crate) fn load(path: &Path) -> Result<Self> {
        let layout: Self = read_json(path)?;
        layout.validate()?;
        Ok(layout)
    }

    pub(crate) fn load_generation(paths: &GenerationPaths) -> Result<Self> {
        Ok(Self::load(&paths.layout_path)?.with_generation_labels(paths))
    }

    pub(crate) fn with_generation_labels(mut self, paths: &GenerationPaths) -> Self {
        // An absent, unreadable or unrecognized sidecar never invalidates the
        // lexical generation. Validate ALL bindings before trusting any one.
        let path = paths.root.join(LABEL_SIDECAR);
        if let Some(sidecar) = SnapshotLabels::read(&path)
            && sidecar.version == LABEL_VERSION
            && sidecar.generation == paths.id
        {
            let partitions: BTreeMap<_, _> = self
                .partitions
                .iter()
                .map(|p| (&p.resource, &p.partition_id))
                .collect();
            let mut labels = BTreeMap::new();
            for binding in sidecar.labels {
                if partitions.get(&binding.resource).copied() != Some(&binding.partition_id)
                    || labels
                        .insert(binding.resource, binding.snapshot_id)
                        .is_some()
                {
                    return self;
                }
            }
            for p in &mut self.partitions {
                p.snapshot_id = labels.remove(&p.resource);
            }
        }
        self
    }

    fn snapshot_labels(&self, generation: String) -> SnapshotLabels {
        SnapshotLabels {
            version: LABEL_VERSION,
            generation,
            labels: self
                .partitions
                .iter()
                .filter_map(|p| {
                    Some(LabelBinding {
                        partition_id: p.partition_id.clone(),
                        resource: p.resource.clone(),
                        snapshot_id: p.snapshot_id.clone()?,
                    })
                })
                .collect(),
        }
    }

    pub(crate) fn labels_fit(&self) -> Result<bool> {
        // Newly generated IDs have a fixed width and require no JSON escaping.
        self.snapshot_labels("0".repeat(GENERATED_ID_BYTES)).fit()
    }

    pub(crate) fn save_candidate(
        &self,
        candidate: &CandidateGeneration,
        warnings: &mut Vec<IngestWarning>,
    ) -> Result<()> {
        self.save(&candidate.layout_path())?;
        let sidecar = self.snapshot_labels(candidate.id.clone());
        // Fresh staging never inherits the predecessor's sidecar.
        if !sidecar.labels.is_empty() {
            let path = candidate.staging_dir.join(LABEL_SIDECAR);
            if sidecar.fit()? {
                write_json_atomic(&path, &sidecar)?;
            } else {
                warnings.push(IngestWarning {
                    path,
                    message:
                        "snapshot label sidecar exceeds the storage limit; generation is unlabeled"
                            .into(),
                });
            }
        }
        Ok(())
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        write_json_atomic(path, self)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.layout_version != LAYOUT_VERSION {
            return Err(Error::State(format!(
                "unsupported generation layout version {}; expected {LAYOUT_VERSION}",
                self.layout_version
            )));
        }
        if self.profile != HostProfile::OpenClast {
            return Err(Error::State(
                "partition layout must use the openclast profile".to_owned(),
            ));
        }

        let mut ids = BTreeSet::new();
        let mut resources = BTreeSet::new();
        for partition in &self.partitions {
            validate_resource(&partition.resource)?;
            if partition.partition_id != partition_id(&partition.resource) {
                return Err(Error::State(format!(
                    "partition ID does not match its resource: {}",
                    partition.partition_id
                )));
            }
            if !ids.insert(partition.partition_id.as_str()) {
                return Err(Error::State(format!(
                    "duplicate partition ID: {}",
                    partition.partition_id
                )));
            }
            if !resources.insert(&partition.resource) {
                return Err(Error::State(format!(
                    "duplicate resource partition: {}/{}/{}",
                    partition.resource.tenant_id,
                    partition.resource.vault_id,
                    partition.resource.room_id
                )));
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_resource(resource: &ResourceKey) -> Result<()> {
    for (name, value) in [
        ("tenant_id", resource.tenant_id.as_str()),
        ("vault_id", resource.vault_id.as_str()),
        ("room_id", resource.room_id.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(Error::State(format!("resource {name} must not be empty")));
        }
    }
    Ok(())
}

pub(crate) fn partition_id(resource: &ResourceKey) -> String {
    let mut digest = Sha256::new();
    digest.update(b"kwiry-resource-v1\0");
    update_component(&mut digest, resource.tenant_id.as_bytes());
    update_component(&mut digest, resource.vault_id.as_bytes());
    update_component(&mut digest, resource.room_id.as_bytes());
    format!("{:x}", digest.finalize())
}

pub(crate) fn partition_index_dir(partitions_dir: &Path, resource: &ResourceKey) -> PathBuf {
    partitions_dir.join(partition_id(resource)).join("index")
}

fn update_component(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_ids_are_stable_and_tuple_scoped() {
        let first = ResourceKey::new("tenant", "vault", "room");
        assert_eq!(partition_id(&first), partition_id(&first));
        assert_ne!(
            partition_id(&first),
            partition_id(&ResourceKey::new("tenant", "vault", "other"))
        );
        assert_ne!(
            partition_id(&first),
            partition_id(&ResourceKey::new("other", "vault", "room"))
        );
    }

    #[test]
    fn maximum_size_snapshot_label_bindings_round_trip_at_room_bound() {
        let dir = tempfile::tempdir().unwrap();
        let candidate = crate::DataRoot::new(dir.path()).create_candidate().unwrap();
        let tenant = format!("tenant_{}", "t".repeat(20));
        let snapshot = "s".repeat(128).parse::<SnapshotId>().unwrap();
        let resources = (0..MAX_LABELED_ROOMS)
            .map(|i| {
                // Each control byte uses six escaped bytes; 174 + 27 + 27 + 22 = 250.
                let vault = format!("{}{:027}", "\u{1}".repeat(29), i);
                let room = format!("{i:022}");
                assert!(label_resource_fits(&tenant, &vault, &room));
                assert!(!label_resource_fits(&tenant, &vault, &format!("{room}x")));
                (
                    ResourceKey::new(&tenant, vault, room),
                    Some(snapshot.clone()),
                )
            })
            .collect();
        let layout = GenerationLayout::with_snapshots(resources).unwrap();
        let mut warnings = Vec::new();
        layout.save_candidate(&candidate, &mut warnings).unwrap();
        assert!(warnings.is_empty());
        let path = candidate.staging_dir.join(LABEL_SIDECAR);
        let bytes = path.metadata().unwrap().len();
        assert!(bytes < MAX_LABEL_BYTES);
        assert_eq!(bytes as usize, 151 + MAX_LABELED_ROOMS * 605);
        let paths = GenerationPaths {
            manifest_path: candidate.manifest_path(),
            layout_path: candidate.layout_path(),
            id: candidate.id,
            root: candidate.staging_dir,
            index_dir: candidate.index_dir,
            partitions_dir: candidate.partitions_dir,
        };
        assert_eq!(GenerationLayout::load_generation(&paths).unwrap(), layout);
    }

    #[test]
    fn layout_sorts_deduplicates_and_validates_resources() {
        let a = ResourceKey::new("tenant", "a", "room-a");
        let b = ResourceKey::new("tenant", "b", "room-b");
        let layout = GenerationLayout::openclast([b.clone(), a.clone(), b]).unwrap();
        assert_eq!(
            layout
                .partitions
                .iter()
                .map(|partition| partition.resource.clone())
                .collect::<Vec<_>>(),
            vec![a, ResourceKey::new("tenant", "b", "room-b")]
        );

        assert!(GenerationLayout::openclast([ResourceKey::new("", "a", "b")]).is_err());
    }
}
