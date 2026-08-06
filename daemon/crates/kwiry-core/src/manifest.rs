use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{Error, Result};
use crate::model::{
    CHUNKING_VERSION, FileIngestOutcome, FileOutcomeKind, ResourceKey, SourceFormatCounts,
    VaultRegistration,
};
use crate::policy::{ExtractionProfile, extraction_policy_fingerprint, extraction_profile_for};
use crate::source::SOURCE_PREPARATION_SCHEMA_VERSION;
pub use crate::source::source_key;
use crate::state::{read_json, write_json_atomic};

pub const MANIFEST_VERSION: u32 = 4;
pub const INDEX_FORMAT_VERSION: u32 = 12;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub manifest_version: u32,
    pub index_format_version: u32,
    pub chunking_version: u64,
    /// The source-preparation schema every entry below was produced under.
    pub preparation_schema_version: u32,
    /// The extraction-policy identity every entry below was produced under.
    /// This is the daemon's rebuild gate: an index is a single-profile
    /// artifact, and a build whose compiled extractor set differs cannot reuse
    /// one row of it.
    pub extraction_policy_fingerprint: String,
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
            preparation_schema_version: SOURCE_PREPARATION_SCHEMA_VERSION,
            extraction_policy_fingerprint: extraction_policy_fingerprint().to_owned(),
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
            || self.preparation_schema_version != SOURCE_PREPARATION_SCHEMA_VERSION
        {
            return Err(Error::State(format!(
                "unsupported manifest versions: found manifest={}, index={}, chunking={}, preparation={}; expected manifest={MANIFEST_VERSION}, index={INDEX_FORMAT_VERSION}, chunking={CHUNKING_VERSION}, preparation={SOURCE_PREPARATION_SCHEMA_VERSION}; run `kwiry index` to rebuild the disposable index",
                self.manifest_version,
                self.index_format_version,
                self.chunking_version,
                self.preparation_schema_version
            )));
        }
        // A policy change is a full rebuild, never a partial reuse. Selective
        // per-format invalidation is expressible — `ManifestFile` records its
        // own profile — but it is deliberately not enabled: a tier switch is a
        // rare, deliberate action, the index is disposable by contract, and an
        // optimization that can still be wrong buys nothing here.
        if self.extraction_policy_fingerprint != extraction_policy_fingerprint() {
            return Err(Error::State(format!(
                "manifest was built under extraction policy {} but this build compiles {}; run `kwiry index` to rebuild the disposable index",
                self.extraction_policy_fingerprint,
                extraction_policy_fingerprint()
            )));
        }
        if let Some(file) = self
            .files
            .values()
            .find(|file| file.extraction_profile != extraction_profile_for(file.format))
        {
            return Err(Error::State(format!(
                "manifest entry {} was produced by the {} extraction profile but this build compiles {} for {}; run `kwiry index` to rebuild the disposable index",
                file.path,
                file.extraction_profile.as_str(),
                extraction_profile_for(file.format).as_str(),
                file.format.as_str()
            )));
        }
        if self.files.values().any(|file| {
            file.coverage.is_indexed() != (file.outcome == ManifestFileOutcome::Indexed)
        }) {
            return Err(Error::State(
                "manifest coverage does not match persisted file outcomes; run `kwiry index` to rebuild the disposable index"
                    .to_owned(),
            ));
        }
        let counts = self.source_format_counts();
        if counts.indexed_documents() != self.document_count() {
            return Err(Error::State(
                "manifest per-format coverage counts do not match its document total; run `kwiry index` to rebuild the disposable index"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn insert_outcome(
        &mut self,
        outcome: &FileIngestOutcome,
        registration_fingerprint: &str,
    ) -> bool {
        self.insert_outcome_for_resource(outcome, registration_fingerprint, None)
    }

    pub(crate) fn insert_outcome_for_resource(
        &mut self,
        outcome: &FileIngestOutcome,
        registration_fingerprint: &str,
        resource: Option<&ResourceKey>,
    ) -> bool {
        let Some((source_key, file)) =
            ManifestFile::from_outcome(outcome, registration_fingerprint, resource)
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

    pub fn source_format_counts(&self) -> SourceFormatCounts {
        let mut counts = SourceFormatCounts::default();
        for file in self.files.values() {
            counts.record(file.format, file.coverage);
        }
        counts
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
    pub format: crate::format::SourceFormat,
    /// The extractor tier that produced this entry. Evidence, not authority:
    /// the top-level fingerprint is what forces the rebuild. This exists so
    /// status can be honest about what a retained row was made by, and so
    /// reconciliation can re-read a source whose tier changed.
    pub extraction_profile: ExtractionProfile,
    pub coverage: crate::extract::ExtractionCoverage,
    pub content_hash: String,
    pub registration_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<ResourceKey>,
    pub byte_length: u64,
    pub mtime_nanos: u128,
    pub chunk_count: usize,
    pub outcome: ManifestFileOutcome,
    pub warning: Option<String>,
}

impl ManifestFile {
    pub(crate) fn retained(previous: &Self) -> Self {
        previous.clone()
    }

    pub(crate) fn from_outcome(
        outcome: &FileIngestOutcome,
        registration_fingerprint: &str,
        resource: Option<&ResourceKey>,
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
                format: outcome.format,
                extraction_profile: outcome.extraction_profile,
                coverage: outcome.coverage,
                content_hash,
                registration_fingerprint: registration_fingerprint.to_owned(),
                resource: resource.cloned(),
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

    /// Pins the whole version set the extraction-profile wave moved, and the
    /// two constants it deliberately left alone. Each line is a claim, not a
    /// snapshot: changing one of these numbers should require editing the
    /// sentence that says why it moved.
    #[test]
    fn extraction_profile_wave_versions_are_explicit_and_narrow() {
        // `Manifest` gained the preparation schema and the policy fingerprint;
        // `ManifestFile` gained the per-entry profile.
        assert_eq!(MANIFEST_VERSION, 4);
        // The document inventory a daemon index may contain is now
        // policy-dependent, and this is the constant whose mismatch already
        // emits the rebuild instruction.
        assert_eq!(INDEX_FORMAT_VERSION, 12);
        // `SourcePreparation` gained a required field, so old and new builds
        // cannot read each other's preparations.
        assert_eq!(crate::source::SOURCE_PREPARATION_SCHEMA_VERSION, 9);
        // The digest shape is new, so its schema starts at 1.
        assert_eq!(crate::policy::EXTRACTION_POLICY_SCHEMA_VERSION, 1);

        // Deliberately unchanged. `split_oversized`, `MAX_CHUNK_CHARS`, and
        // `CHUNK_OVERLAP_CHARS` are untouched: the profile is an *input to*
        // chunking, not a change to the chunking algorithm. Bumping this would
        // be a false statement and would invalidate semantic state alongside
        // lexical for no reason.
        assert_eq!(CHUNKING_VERSION, 2);
        // Also deliberately unchanged: registration identity is *vault*
        // identity. Folding extraction policy into it would let a policy change
        // masquerade as a re-registration and corrupt the meaning of a field
        // that gates re-ingest for an unrelated reason.
        let vault = VaultRegistration {
            id: "vault".to_owned(),
            path: std::path::PathBuf::from("/vault"),
            room: None,
        };
        assert_eq!(
            registration_fingerprint(&vault),
            registration_fingerprint(&vault)
        );
    }

    #[test]
    fn a_manifest_from_another_extraction_policy_is_refused() {
        let manifest = Manifest {
            extraction_policy_fingerprint: "0".repeat(64),
            ..Manifest::default()
        };

        let error = manifest.validate().unwrap_err();
        assert!(error.to_string().contains("extraction policy"));
        assert!(error.to_string().contains(extraction_policy_fingerprint()));
        assert!(error.to_string().contains("kwiry index"));
    }

    #[test]
    fn a_manifest_entry_from_another_extraction_profile_is_refused() {
        let mut manifest = Manifest::default();
        manifest.files.insert(
            "foreign".to_owned(),
            ManifestFile {
                vault_id: "vault".to_owned(),
                path: "note.md".to_owned(),
                format: crate::format::SourceFormat::Markdown,
                // No build compiles an enhanced Markdown extractor, so this
                // entry cannot have been produced by any build of this crate.
                extraction_profile: ExtractionProfile::Enhanced,
                coverage: crate::extract::ExtractionCoverage::IndexedComplete,
                content_hash: "hash".to_owned(),
                registration_fingerprint: "fingerprint".to_owned(),
                resource: None,
                byte_length: 4,
                mtime_nanos: 1,
                chunk_count: 1,
                outcome: ManifestFileOutcome::Indexed,
                warning: None,
            },
        );

        let error = manifest.validate().unwrap_err();
        assert!(error.to_string().contains("enhanced"));
        assert!(error.to_string().contains("kwiry index"));
    }

    #[test]
    fn an_incompatible_preparation_schema_requires_an_explicit_rebuild() {
        let manifest = Manifest {
            preparation_schema_version: crate::source::SOURCE_PREPARATION_SCHEMA_VERSION - 1,
            ..Manifest::default()
        };

        let error = manifest.validate().unwrap_err();
        assert!(error.to_string().contains("preparation="));
        assert!(error.to_string().contains("kwiry index"));
    }

    #[test]
    fn incompatible_index_manifest_requires_an_explicit_rebuild() {
        let manifest = Manifest {
            index_format_version: 10,
            ..Manifest::default()
        };

        let error = manifest.validate().unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("found manifest={MANIFEST_VERSION}, index=10"))
        );
        assert!(error.to_string().contains(&format!(
            "expected manifest={MANIFEST_VERSION}, index={INDEX_FORMAT_VERSION}"
        )));
        assert!(error.to_string().contains("kwiry index"));
    }

    #[test]
    fn manifest_rejects_coverage_that_disagrees_with_document_outcome() {
        let mut manifest = Manifest::default();
        manifest.files.insert(
            "broken".to_owned(),
            ManifestFile {
                vault_id: "vault".to_owned(),
                path: "broken.base".to_owned(),
                format: crate::format::SourceFormat::Base,
                extraction_profile: ExtractionProfile::Portable,
                coverage: crate::extract::ExtractionCoverage::Quarantined,
                content_hash: "hash".to_owned(),
                registration_fingerprint: "fingerprint".to_owned(),
                resource: None,
                byte_length: 4,
                mtime_nanos: 1,
                chunk_count: 0,
                outcome: ManifestFileOutcome::Indexed,
                warning: Some("invalid Base YAML".to_owned()),
            },
        );

        let error = manifest.validate().unwrap_err();
        assert!(error.to_string().contains("coverage"));
    }

    #[test]
    fn retained_manifest_file_preserves_content_evidence() {
        let previous = ManifestFile {
            vault_id: "vault".to_owned(),
            path: "note.md".to_owned(),
            format: crate::format::SourceFormat::Markdown,
            extraction_profile: ExtractionProfile::Portable,
            coverage: crate::extract::ExtractionCoverage::IndexedComplete,
            content_hash: "hash".to_owned(),
            registration_fingerprint: "fingerprint".to_owned(),
            resource: Some(ResourceKey::new("tenant", "vault", "room")),
            byte_length: 42,
            mtime_nanos: 123,
            chunk_count: 7,
            outcome: ManifestFileOutcome::Indexed,
            warning: None,
        };

        assert_eq!(ManifestFile::retained(&previous), previous);
    }

    #[test]
    fn manifest_persists_and_aggregates_per_format_coverage() {
        let mut manifest = Manifest::default();
        manifest.files.insert(
            "markdown".to_owned(),
            ManifestFile {
                vault_id: "vault".to_owned(),
                path: "note.md".to_owned(),
                format: crate::format::SourceFormat::Markdown,
                extraction_profile: ExtractionProfile::Portable,
                coverage: crate::extract::ExtractionCoverage::IndexedPartial,
                content_hash: "markdown-hash".to_owned(),
                registration_fingerprint: "fingerprint".to_owned(),
                resource: None,
                byte_length: 4,
                mtime_nanos: 1,
                chunk_count: 1,
                outcome: ManifestFileOutcome::Indexed,
                warning: Some("partial metadata".to_owned()),
            },
        );
        manifest.files.insert(
            "base".to_owned(),
            ManifestFile {
                vault_id: "vault".to_owned(),
                path: "broken.base".to_owned(),
                format: crate::format::SourceFormat::Base,
                extraction_profile: ExtractionProfile::Portable,
                coverage: crate::extract::ExtractionCoverage::Quarantined,
                content_hash: "base-hash".to_owned(),
                registration_fingerprint: "fingerprint".to_owned(),
                resource: None,
                byte_length: 4,
                mtime_nanos: 1,
                chunk_count: 0,
                outcome: ManifestFileOutcome::Skipped,
                warning: Some("invalid Base YAML".to_owned()),
            },
        );

        let counts = manifest.source_format_counts();
        assert_eq!(manifest.document_count(), 1);
        assert_eq!(counts.indexed_documents(), manifest.document_count());
        assert_eq!(counts.total_sources(), 2);
        assert_eq!(counts.markdown.indexed_partial, 1);
        assert_eq!(counts.base.quarantined, 1);
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
