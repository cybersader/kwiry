use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::model::{HostProfile, ResourceKey, SnapshotId};
use crate::state::{read_json, write_json_atomic};

// Before snapshot labels, partitions and CURRENT used the same version number.
const PRE_LABEL_LAYOUT_VERSION: u32 = crate::generation::LAYOUT_VERSION;
const GENERATION_LAYOUT_VERSION: u32 = 4;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
            layout_version: GENERATION_LAYOUT_VERSION,
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
        let mut layout: Self = read_json(path)?;
        if layout.layout_version == PRE_LABEL_LAYOUT_VERSION {
            // Upgrade in memory. No rebuild, inferred labels or forced publication.
            layout.layout_version = GENERATION_LAYOUT_VERSION;
            for partition in &mut layout.partitions {
                partition.snapshot_id = None;
            }
        }
        layout.validate()?;
        Ok(layout)
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        write_json_atomic(path, self)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.layout_version != GENERATION_LAYOUT_VERSION {
            return Err(Error::State(format!(
                "unsupported generation layout version {}; expected {GENERATION_LAYOUT_VERSION}",
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
