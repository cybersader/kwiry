use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{Error, Result};
use crate::model::{CHUNKING_VERSION, FileIngestOutcome, FileOutcomeKind, VaultRegistration};
use crate::state::{read_json, write_json_atomic};

pub const MANIFEST_VERSION: u32 = 1;
pub const INDEX_FORMAT_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub manifest_version: u32,
    pub index_format_version: u32,
    pub chunking_version: u64,
    pub state_revision: u64,
    pub last_sync: Option<String>,
    pub files: BTreeMap<String, ManifestFile>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            manifest_version: MANIFEST_VERSION,
            index_format_version: INDEX_FORMAT_VERSION,
            chunking_version: CHUNKING_VERSION,
            state_revision: 0,
            last_sync: None,
            files: BTreeMap::new(),
        }
    }
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let manifest: Self = read_json(path)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        write_json_atomic(path, self)
    }

    pub fn validate(&self) -> Result<()> {
        if self.manifest_version != MANIFEST_VERSION
            || self.index_format_version != INDEX_FORMAT_VERSION
            || self.chunking_version != CHUNKING_VERSION
        {
            return Err(Error::State(format!(
                "unsupported manifest versions: found manifest={}, index={}, chunking={}; expected manifest={MANIFEST_VERSION}, index={INDEX_FORMAT_VERSION}, chunking={CHUNKING_VERSION}; run `kwiry index` to rebuild the disposable index",
                self.manifest_version, self.index_format_version, self.chunking_version
            )));
        }
        Ok(())
    }

    pub(crate) fn insert_outcome(
        &mut self,
        outcome: &FileIngestOutcome,
        registration_fingerprint: &str,
    ) -> bool {
        let Some((source_key, file)) =
            ManifestFile::from_outcome(outcome, registration_fingerprint)
        else {
            return false;
        };
        self.files.insert(source_key, file);
        true
    }

    pub fn document_count(&self) -> usize {
        self.files
            .values()
            .filter(|file| file.outcome == ManifestFileOutcome::Indexed)
            .count()
    }

    pub fn chunk_count(&self) -> usize {
        self.files.values().map(|file| file.chunk_count).sum()
    }

    pub fn mark_synced(&mut self) -> Result<()> {
        self.state_revision = self.state_revision.saturating_add(1);
        self.last_sync = Some(
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|error| Error::State(format!("could not format sync time: {error}")))?,
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestFile {
    pub vault_id: String,
    pub path: String,
    pub content_hash: String,
    pub registration_fingerprint: String,
    pub byte_length: u64,
    pub mtime_nanos: u128,
    pub chunk_count: usize,
    pub outcome: ManifestFileOutcome,
    pub warning: Option<String>,
}

impl ManifestFile {
    pub(crate) fn from_outcome(
        outcome: &FileIngestOutcome,
        registration_fingerprint: &str,
    ) -> Option<(String, Self)> {
        let content_hash = outcome.content_hash.clone()?;
        let kind = match outcome.kind {
            FileOutcomeKind::Indexed => ManifestFileOutcome::Indexed,
            FileOutcomeKind::Skipped => ManifestFileOutcome::Skipped,
            FileOutcomeKind::TransientError => return None,
        };
        Some((
            source_key(&outcome.vault_id, &outcome.path),
            Self {
                vault_id: outcome.vault_id.clone(),
                path: outcome.path.clone(),
                content_hash,
                registration_fingerprint: registration_fingerprint.to_owned(),
                byte_length: outcome.byte_length,
                mtime_nanos: outcome.mtime_nanos,
                chunk_count: outcome.chunks.len(),
                outcome: kind,
                warning: outcome
                    .warning
                    .as_ref()
                    .map(|warning| warning.message.clone()),
            },
        ))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManifestFileOutcome {
    Indexed,
    Skipped,
}

pub fn source_key(vault_id: &str, path: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"kwiry-source-v1\0");
    update_component(&mut digest, vault_id.as_bytes());
    update_component(&mut digest, path.as_bytes());
    format!("{:x}", digest.finalize())
}

pub fn registration_fingerprint(vault: &VaultRegistration) -> String {
    let mut digest = Sha256::new();
    digest.update(b"kwiry-registration-v1\0");
    update_component(&mut digest, vault.id.as_bytes());
    update_component(&mut digest, vault.path.to_string_lossy().as_bytes());
    update_component(
        &mut digest,
        vault.room.as_deref().unwrap_or_default().as_bytes(),
    );
    format!("{:x}", digest.finalize())
}

fn update_component(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn source_keys_are_stable_and_vault_scoped() {
        assert_eq!(source_key("a", "note.md"), source_key("a", "note.md"));
        assert_ne!(source_key("a", "note.md"), source_key("b", "note.md"));
        assert_ne!(source_key("a", "note.md"), source_key("a", "moved.md"));
    }

    #[test]
    fn incompatible_index_manifest_requires_an_explicit_rebuild() {
        let manifest = Manifest {
            index_format_version: 2,
            ..Manifest::default()
        };

        let error = manifest.validate().unwrap_err();
        assert!(error.to_string().contains("found manifest=1, index=2"));
        assert!(error.to_string().contains("expected manifest=1, index=3"));
        assert!(error.to_string().contains("kwiry index"));
    }

    #[test]
    fn manifest_round_trips_deterministically() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("manifest.json");
        let manifest = Manifest::default();
        manifest.save(&path).unwrap();
        assert_eq!(Manifest::load(&path).unwrap(), manifest);
    }
}
