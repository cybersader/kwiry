use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::model::{HostProfile, ResourceKey, SnapshotId};
use crate::state::{read_json, write_json_atomic};

use crate::generation::{CandidateGeneration, GenerationPaths, LAYOUT_VERSION};

pub(crate) const LABEL_SIDECAR: &str = "snapshot-labels.json";
const LABEL_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotLabels {
    version: u32,
    generation: String,
    labels: Vec<LabelBinding>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LabelBinding {
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
    /// per-source chunk counts match the live partition. Without this evidence the
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
        let mut layout = Self::load(&paths.layout_path)?;
        // An absent, unreadable or unrecognized sidecar never invalidates the
        // lexical generation. Validate ALL bindings before trusting any one.
        let path = paths.root.join(LABEL_SIDECAR);
        if !path.symlink_metadata().is_ok_and(|m| m.is_file()) {
            return Ok(layout);
        }
        if let Ok(sidecar) = read_json::<SnapshotLabels>(&path)
            && sidecar.version == LABEL_VERSION
            && sidecar.generation == paths.id
        {
            let partitions: BTreeMap<_, _> = layout
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
                    return Ok(layout);
                }
            }
            for p in &mut layout.partitions {
                p.snapshot_id = labels.remove(&p.resource);
            }
        }
        Ok(layout)
    }

    pub(crate) fn save_candidate(&self, candidate: &CandidateGeneration) -> Result<()> {
        self.save(&candidate.layout_path())?;
        let labels: Vec<_> = self
            .partitions
            .iter()
            .filter_map(|p| {
                Some(LabelBinding {
                    partition_id: p.partition_id.clone(),
                    resource: p.resource.clone(),
                    snapshot_id: p.snapshot_id.clone()?,
                })
            })
            .collect();
        // Fresh staging never inherits the predecessor's sidecar.
        if !labels.is_empty() {
            write_json_atomic(
                &candidate.staging_dir.join(LABEL_SIDECAR),
                &SnapshotLabels {
                    version: LABEL_VERSION,
                    generation: candidate.id.clone(),
                    labels,
                },
            )?;
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
