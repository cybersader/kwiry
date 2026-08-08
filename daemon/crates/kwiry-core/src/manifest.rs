use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{Error, Result};
use crate::format::SourceFormat;
use crate::model::{
    CHUNKING_VERSION, FileIngestOutcome, FileOutcomeKind, ResourceKey, SourceFormatCounts,
    VaultRegistration,
};
use crate::policy::{
    ExtractionProfile, FORMAT_IDENTITY_SCHEMA_VERSION, extraction_policy_fingerprint,
    extraction_profile_for, format_identity_fingerprint,
};
use crate::source::SOURCE_PREPARATION_SCHEMA_VERSION;
pub use crate::source::source_key;
use crate::state::{read_json, write_json_atomic};

pub const MANIFEST_VERSION: u32 = 5;
pub const INDEX_FORMAT_VERSION: u32 = 12;

/// The record a generation's `manifest.json` holds.
///
/// The top-level fields are **core identity**: facts about the container and
/// the shared, format-blind pipeline. A core mismatch means no row of any
/// format is usable, so it is refused whole. Per-format identity does not live
/// here — it lives on every [`ManifestFile`], and it is decided per row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub manifest_version: u32,
    pub index_format_version: u32,
    pub chunking_version: u64,
    /// The source-preparation schema every entry below was produced under.
    pub preparation_schema_version: u32,
    /// The *shape* of the per-format identity every entry below carries. Core,
    /// deliberately: a new identity component must invalidate everything, which
    /// is the escape hatch that lets the per-format digest stay small.
    ///
    /// Defaulted on read so a record predating the field reaches the version
    /// gate instead of the JSON parser — the gate emits `run kwiry index`,
    /// the parser emits a corruption message naming an absolute path. The
    /// default is `0`, which is not a schema version any build ever wrote, so
    /// it can only ever produce a refusal. This is the exact opposite of
    /// defaulting `ManifestFile::format_identity` to a concrete string, which
    /// would fabricate a *match*; defaulting toward refusal is safe, and
    /// defaulting toward reuse is the failure this wave exists to prevent.
    #[serde(default)]
    pub format_identity_schema_version: u32,
    /// Written, never read. **Not authority, and not even evidence.**
    ///
    /// Every build before this wave declares this field as required with no
    /// serde default, so a v5 record that omits it makes a rollback fail in
    /// `read_json` — a corruption message naming an absolute path, with no
    /// remedy — instead of reaching that build's `MANIFEST_VERSION` gate,
    /// which says `run kwiry index`. Keeping the key on disk costs one line
    /// and preserves the honest downgrade message.
    ///
    /// `skip_deserializing` is what keeps it from becoming a fact: whatever a
    /// record carries is discarded on read and the running build's value is
    /// re-derived, so it can never go stale on disk and there is no code path
    /// that could compare it against anything. Reuse is decided by
    /// [`ManifestFile::format_identity`], per row.
    #[serde(default = "running_extraction_policy_fingerprint", skip_deserializing)]
    extraction_policy_fingerprint: String,
    pub state_revision: u64,
    pub last_sync: Option<String>,
    pub files: BTreeMap<String, ManifestFile>,
}

fn running_extraction_policy_fingerprint() -> String {
    extraction_policy_fingerprint().to_owned()
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            manifest_version: MANIFEST_VERSION,
            index_format_version: INDEX_FORMAT_VERSION,
            chunking_version: CHUNKING_VERSION,
            preparation_schema_version: SOURCE_PREPARATION_SCHEMA_VERSION,
            format_identity_schema_version: FORMAT_IDENTITY_SCHEMA_VERSION,
            extraction_policy_fingerprint: running_extraction_policy_fingerprint(),
            state_revision: 0,
            last_sync: None,
            files: BTreeMap::new(),
        }
    }
}

impl Manifest {
    /// Reads the on-disk record and refuses it whole on a *core* mismatch.
    ///
    /// Deliberately does not return a `Manifest`: what is on disk may still
    /// contain rows this build cannot reuse, and the only way to a usable
    /// manifest is [`ManifestOnDisk::adopt`], which physically removes them.
    /// The type is the enforcement; omitting the eviction is not expressible.
    pub fn load(path: &Path) -> Result<ManifestOnDisk> {
        // `read_json` runs before any gate, so a record this build cannot even
        // parse never reaches one. That is not a hypothetical: the format
        // vocabulary is a serde enum, so a format *removed* from the compiled
        // set — the whole-artifact refusal the design calls for — surfaces as
        // `unknown variant`, with no remedy attached. Every gate below answers
        // the same way, because the index is disposable, so the remedy is
        // stated here too rather than only for the failures that got as far as
        // being classified.
        let manifest: Self = read_json(path).map_err(|error| match error {
            Error::State(message) => Error::State(format!(
                "{message}; run `kwiry index` to rebuild the disposable index"
            )),
            other => other,
        })?;
        manifest.validate_readable()?;
        Ok(ManifestOnDisk(manifest))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        write_json_atomic(path, self)
    }

    /// The publication invariant: everything `save` writes is core-compatible
    /// *and* single-identity-per-format, and that identity is this build's.
    pub fn validate(&self) -> Result<()> {
        self.validate_readable()?;
        if let Some(file) = self.files.values().find(|file| !file.identity_matches()) {
            // Two distinct refusals, named distinctly. A row whose recorded
            // tier contradicts the compiled one is evicted even when its
            // identity string matches, and reporting that as "identity X but
            // this build compiles X" is a message an operator cannot act on.
            let compiled = extraction_profile_for(file.format);
            if file.extraction_profile != compiled {
                return Err(Error::State(format!(
                    "manifest entry {} records the {} extraction profile for {} but this build compiles {}; run `kwiry index` to rebuild the disposable index",
                    file.path,
                    file.extraction_profile.as_str(),
                    file.format.as_str(),
                    compiled.as_str()
                )));
            }
            return Err(Error::State(format!(
                "manifest entry {} records {} format identity {} but this build compiles {}; run `kwiry index` to rebuild the disposable index",
                file.path,
                file.format.as_str(),
                file.format_identity.as_deref().unwrap_or("<absent>"),
                format_identity_fingerprint(file.format)
            )));
        }
        Ok(())
    }

    /// Core identity plus the aggregate invariants, and nothing per-format.
    ///
    /// This is what `load` gates on, and the distinction is the whole wave: a
    /// row whose format identity moved must leave the manifest *readable* so
    /// eviction can remove exactly that row, instead of condemning the artifact.
    pub fn validate_readable(&self) -> Result<()> {
        if self.manifest_version != MANIFEST_VERSION
            || self.index_format_version != INDEX_FORMAT_VERSION
            || self.chunking_version != CHUNKING_VERSION
            || self.preparation_schema_version != SOURCE_PREPARATION_SCHEMA_VERSION
            || self.format_identity_schema_version != FORMAT_IDENTITY_SCHEMA_VERSION
        {
            return Err(Error::State(format!(
                "unsupported manifest versions: found manifest={}, index={}, chunking={}, preparation={}, format_identity_schema={}; expected manifest={MANIFEST_VERSION}, index={INDEX_FORMAT_VERSION}, chunking={CHUNKING_VERSION}, preparation={SOURCE_PREPARATION_SCHEMA_VERSION}, format_identity_schema={FORMAT_IDENTITY_SCHEMA_VERSION}; run `kwiry index` to rebuild the disposable index",
                self.manifest_version,
                self.index_format_version,
                self.chunking_version,
                self.preparation_schema_version,
                self.format_identity_schema_version
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

/// A manifest exactly as it was read from disk: core-compatible, but possibly
/// still holding rows this build cannot reuse.
///
/// The newtype is the ordering rule made unrepresentable-otherwise. No artifact
/// may be observed, planned over, retained, published, or served until every
/// non-reusable row has been physically removed, so the only way out of this
/// type is [`adopt`](Self::adopt).
#[derive(Debug)]
pub struct ManifestOnDisk(Manifest);

impl ManifestOnDisk {
    /// The recorded rows, for *structural* inspection only (profile/layout
    /// agreement checks that run before adoption). Handing back a `&Manifest`
    /// would let a caller clone its way past the eviction, so it does not exist.
    pub fn recorded_files(&self) -> &BTreeMap<String, ManifestFile> {
        &self.0.files
    }

    /// Consumes the on-disk record, removes every row this build cannot reuse,
    /// and returns a manifest for which [`Manifest::validate`] holds.
    ///
    /// `document_count` and `source_format_counts` are derived from `files`
    /// rather than stored, so the aggregate invariants survive eviction by
    /// construction; there is no ledger arithmetic that can be wrong.
    pub fn adopt(self) -> (Manifest, EvictionReport) {
        let Self(mut manifest) = self;
        let mut report = EvictionReport::default();
        manifest.files.retain(|key, file| {
            if file.identity_matches() {
                return true;
            }
            report.record(key.clone(), file);
            false
        });
        debug_assert!(
            manifest.validate().is_ok(),
            "adopt must return a publishable manifest"
        );
        (manifest, report)
    }

    /// Refuses the record when it still holds a row this build cannot reuse.
    ///
    /// The read-only counterpart of [`adopt`](Self::adopt), and the *only*
    /// other way a caller may proceed. Eviction publishes a new generation and
    /// therefore needs the writer lock, which a reader does not hold, so a
    /// reader has exactly two options: refuse the artifact, or serve rows under
    /// an identity they were not built under. The second is the failure this
    /// wave exists to prevent, and dropping the stale rows from the result set
    /// instead would be the read-time filter the design rules out — the
    /// postings would still be there for the next reader.
    ///
    /// Delegates to [`Manifest::validate`] so a reader and the publication gate
    /// state the refusal identically, remedy included.
    pub fn require_every_row_reusable(&self) -> Result<()> {
        self.0.validate()
    }
}

/// What [`ManifestOnDisk::adopt`] removed, and what the caller must therefore
/// delete from the index before the artifact becomes searchable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvictionReport {
    /// Per-format counts, so the ingest summary can say which formats were
    /// invalidated rather than reporting a silent narrowing.
    pub by_format: BTreeMap<SourceFormat, usize>,
    /// The source keys whose index rows the caller MUST delete, with the scope
    /// needed to address them.
    pub evicted: BTreeMap<String, EvictedSource>,
}

impl EvictionReport {
    pub fn is_empty(&self) -> bool {
        self.evicted.is_empty()
    }

    pub fn total(&self) -> usize {
        self.evicted.len()
    }

    fn record(&mut self, key: String, file: &ManifestFile) {
        *self.by_format.entry(file.format).or_default() += 1;
        self.evicted.insert(
            key,
            EvictedSource {
                format: file.format,
                path: file.path.clone(),
                resource: file.resource.clone(),
                recorded_identity: file.format_identity.clone(),
            },
        );
    }
}

/// One evicted row's evidence. `recorded_identity` is `None` for a row written
/// before the field existed — refused, never adopted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvictedSource {
    pub format: SourceFormat,
    pub path: String,
    pub resource: Option<ResourceKey>,
    pub recorded_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestFile {
    pub vault_id: String,
    pub path: String,
    pub format: crate::format::SourceFormat,
    /// The extractor tier that produced this entry. Evidence for the refusal
    /// message and for honest status reporting, not authority: the tier is one
    /// of the three components of `format_identity`, which is what decides
    /// reuse.
    pub extraction_profile: ExtractionProfile,
    /// The per-format identity this row was produced under. **Authority.**
    ///
    /// `None` means "written before per-format identity existed" and is a
    /// refusal, never a match. It is `Option` rather than a required field
    /// because `read_json` parses before any gate runs: a required field would
    /// turn every legacy manifest into a corruption message naming an absolute
    /// path, instead of the `MANIFEST_VERSION` mismatch that emits the correct
    /// `run kwiry index` remedy. The `None`-refusal keeps the field honest at
    /// the *next* version bump, when a manifest written by a build lacking the
    /// field can no longer be distinguished by number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_identity: Option<String>,
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
    /// The row predicate, and the entire risk surface of the split identity.
    ///
    /// Written this way on purpose. `self.format_identity == other.format_identity`
    /// would make two `None`s compare *equal*; `unwrap_or(expected)` and
    /// `unwrap_or_default()` fabricate a match; `#[serde(default)]` on a
    /// concrete `String` would let every legacy row silently claim the running
    /// identity. Absence is a refusal, and it is spelled out here so it cannot
    /// be refactored into a default.
    pub fn identity_matches(&self) -> bool {
        // The recorded tier is evidence for the identity, so a row whose
        // evidence contradicts its authority is not trustworthy either way:
        // fail closed and evict it.
        if self.extraction_profile != extraction_profile_for(self.format) {
            return false;
        }
        let expected = format_identity_fingerprint(self.format);
        match self.format_identity.as_deref() {
            Some(actual) => actual == expected,
            None => false,
        }
    }

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
                // Stamped from the running build, which is the build that just
                // produced this row. Never copied from a previous record.
                format_identity: Some(format_identity_fingerprint(outcome.format).to_owned()),
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
    use crate::format::SourceFormat;

    /// A row this build could have produced: correct profile, correct identity.
    fn file_of(format: SourceFormat, path: &str) -> ManifestFile {
        ManifestFile {
            vault_id: "vault".to_owned(),
            path: path.to_owned(),
            format,
            extraction_profile: extraction_profile_for(format),
            format_identity: Some(format_identity_fingerprint(format).to_owned()),
            coverage: crate::extract::ExtractionCoverage::IndexedComplete,
            content_hash: format!("{path}-hash"),
            registration_fingerprint: "fingerprint".to_owned(),
            resource: None,
            byte_length: 4,
            mtime_nanos: 1,
            chunk_count: 1,
            outcome: ManifestFileOutcome::Indexed,
            warning: None,
        }
    }

    fn manifest_of(files: impl IntoIterator<Item = ManifestFile>) -> Manifest {
        let mut manifest = Manifest::default();
        for file in files {
            manifest
                .files
                .insert(source_key(&file.vault_id, &file.path), file);
        }
        manifest
    }

    /// The only way to build a `ManifestOnDisk` outside `load` — tests need to
    /// hand `adopt` a record that no build would ever have written.
    fn on_disk(manifest: Manifest) -> ManifestOnDisk {
        ManifestOnDisk(manifest)
    }

    #[test]
    fn source_keys_are_stable_and_vault_scoped() {
        assert_eq!(source_key("a", "note.md"), source_key("a", "note.md"));
        assert_ne!(source_key("a", "note.md"), source_key("b", "note.md"));
        assert_ne!(source_key("a", "note.md"), source_key("a", "moved.md"));
    }

    /// Pins the whole version set the split-identity wave moved, and the
    /// constants it deliberately left alone. Each line is a claim, not a
    /// snapshot: changing one of these numbers should require editing the
    /// sentence that says why it moved.
    #[test]
    fn split_identity_wave_versions_are_explicit_and_narrow() {
        // `Manifest` traded the extraction-policy fingerprint for the
        // format-identity *schema* version, and `ManifestFile` gained
        // `format_identity`. Pre-wave rows carry no extractor version and none
        // is recoverable, so the number moves and the refusal is honest rather
        // than back-filled from evidence that does not exist.
        assert_eq!(MANIFEST_VERSION, 5);
        // Deliberately unchanged. The Tantivy schema is untouched: it stores
        // `source_format` as a plain field, not as per-format columns, so no
        // format's rows changed shape.
        assert_eq!(INDEX_FORMAT_VERSION, 12);
        // Deliberately unchanged. `SourcePreparation` did not change shape;
        // only the manifest record around it did.
        assert_eq!(crate::source::SOURCE_PREPARATION_SCHEMA_VERSION, 9);
        // The identity shape is new, so its schema starts at 1. It is core: a
        // fourth component would bump this and invalidate everything, which is
        // what lets the per-format digest stay at exactly three facts.
        assert_eq!(crate::policy::FORMAT_IDENTITY_SCHEMA_VERSION, 1);
        // Report-only from this wave on, but still derived and still served.
        assert_eq!(crate::policy::EXTRACTION_POLICY_SCHEMA_VERSION, 1);

        // Deliberately unchanged. `split_oversized`, `MAX_CHUNK_CHARS`, and
        // `CHUNK_OVERLAP_CHARS` are untouched, and the chunker never reads a
        // format: bumping this would be a false statement and would invalidate
        // semantic state alongside lexical for no reason.
        assert_eq!(CHUNKING_VERSION, 2);
        // Also deliberately unchanged: registration identity is *vault*
        // identity. Folding extraction identity into it would let an identity
        // change masquerade as a re-registration and corrupt the meaning of a
        // field that gates re-ingest for an unrelated reason.
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

    // ---------------------------------------------------------------- core

    /// A core change invalidates everything, and it is refused whole rather
    /// than narrowed: `load` never gets as far as offering a row to `adopt`.
    #[test]
    fn a_core_version_mismatch_refuses_the_whole_manifest() {
        for (label, manifest) in [
            (
                "manifest=",
                Manifest {
                    manifest_version: MANIFEST_VERSION - 1,
                    ..manifest_of([file_of(SourceFormat::Markdown, "note.md")])
                },
            ),
            (
                "index=",
                Manifest {
                    index_format_version: INDEX_FORMAT_VERSION - 1,
                    ..manifest_of([file_of(SourceFormat::Markdown, "note.md")])
                },
            ),
            (
                "chunking=",
                Manifest {
                    chunking_version: CHUNKING_VERSION + 1,
                    ..manifest_of([file_of(SourceFormat::Markdown, "note.md")])
                },
            ),
            (
                "preparation=",
                Manifest {
                    preparation_schema_version: SOURCE_PREPARATION_SCHEMA_VERSION - 1,
                    ..manifest_of([file_of(SourceFormat::Markdown, "note.md")])
                },
            ),
            (
                "format_identity_schema=",
                Manifest {
                    format_identity_schema_version: FORMAT_IDENTITY_SCHEMA_VERSION + 1,
                    ..manifest_of([file_of(SourceFormat::Markdown, "note.md")])
                },
            ),
        ] {
            let error = manifest.validate_readable().unwrap_err().to_string();
            assert!(error.contains(label), "{label} missing from: {error}");
            assert!(error.contains("kwiry index"), "{label} lacks the remedy");
            // And it is the *readable* gate that refuses, so no eviction path
            // can quietly adopt a core-incompatible record.
            assert!(manifest.validate().is_err());
        }
    }

    /// A manifest predating the field parses (so the version gate can emit the
    /// correct remedy rather than a corruption message), and its rows are
    /// refused on their own merits once the version no longer distinguishes
    /// them.
    #[test]
    fn a_manifest_predating_the_field_parses_and_then_refuses() {
        let legacy = serde_json::json!({
            "manifest_version": 4,
            "index_format_version": INDEX_FORMAT_VERSION,
            "chunking_version": CHUNKING_VERSION,
            "preparation_schema_version": SOURCE_PREPARATION_SCHEMA_VERSION,
            "extraction_policy_fingerprint": "0".repeat(64),
            "state_revision": 3,
            "last_sync": null,
            "files": {},
        });
        let parsed: Manifest = serde_json::from_value(legacy).expect(
            "a legacy record must parse, so the version gate emits the rebuild instruction",
        );
        // The retired field is ignored, and the field that did not exist yet
        // defaults to a value no build ever wrote.
        assert_eq!(parsed.format_identity_schema_version, 0);
        let error = parsed.validate_readable().unwrap_err().to_string();
        assert!(error.contains("found manifest=4"));
        assert!(error.contains("format_identity_schema=0"));
        assert!(error.contains(&format!("expected manifest={MANIFEST_VERSION}")));
        assert!(error.contains("kwiry index"));
    }

    // ----------------------------------------------------------- per format

    /// A single format's identity change invalidates only that format's rows.
    #[test]
    fn one_format_identity_change_evicts_only_that_format() {
        let survivors = [
            file_of(SourceFormat::Markdown, "note.md"),
            file_of(SourceFormat::Text, "log.txt"),
            file_of(SourceFormat::Canvas, "board.canvas"),
            file_of(SourceFormat::Excalidraw, "sketch.excalidraw"),
        ];
        let mut stale = file_of(SourceFormat::Docx, "report.docx");
        stale.format_identity = Some("f".repeat(64));
        let manifest = manifest_of(survivors.iter().cloned().chain([stale.clone()]));
        assert_eq!(manifest.files.len(), 5);

        let (adopted, report) = on_disk(manifest).adopt();

        assert_eq!(report.total(), 1);
        assert_eq!(report.by_format, BTreeMap::from([(SourceFormat::Docx, 1)]));
        let evicted = &report.evicted[&source_key("vault", "report.docx")];
        assert_eq!(evicted.format, SourceFormat::Docx);
        assert_eq!(evicted.path, "report.docx");
        assert_eq!(evicted.recorded_identity, stale.format_identity);
        // Every other format's row survives byte-identically.
        assert_eq!(adopted.files.len(), 4);
        for file in &survivors {
            assert_eq!(adopted.files[&source_key("vault", &file.path)], *file);
        }
        // And the adopted manifest is publishable.
        adopted.validate().unwrap();
    }

    /// A row carrying no identity at all is refused, not adopted. Asserted
    /// directly with the version forced current, so the core gate cannot mask
    /// it — this is the case that becomes reachable at the *next* version bump.
    #[test]
    fn a_row_without_an_identity_is_refused() {
        let mut absent = file_of(SourceFormat::Markdown, "legacy.md");
        absent.format_identity = None;
        assert!(!absent.identity_matches());

        let manifest = manifest_of([absent.clone(), file_of(SourceFormat::Base, "view.base")]);
        assert_eq!(manifest.manifest_version, MANIFEST_VERSION);
        // Readable: the record's shape is fine, only one row is unusable.
        manifest.validate_readable().unwrap();
        // Publishable: no. An absent identity must never be written out.
        let error = manifest.validate().unwrap_err().to_string();
        assert!(error.contains("legacy.md"));
        assert!(error.contains("<absent>"));
        assert!(error.contains("kwiry index"));

        let (adopted, report) = on_disk(manifest).adopt();
        assert_eq!(report.total(), 1);
        assert_eq!(
            report.evicted[&source_key("vault", "legacy.md")].recorded_identity,
            None
        );
        assert_eq!(adopted.files.len(), 1);
        adopted.validate().unwrap();
    }

    /// Two rows that both lack an identity must not match each other. This is
    /// the `None == None` trap the predicate is written to avoid.
    #[test]
    fn two_absent_identities_do_not_match_each_other() {
        let mut left = file_of(SourceFormat::Markdown, "a.md");
        let mut right = file_of(SourceFormat::Markdown, "b.md");
        left.format_identity = None;
        right.format_identity = None;
        assert_eq!(left.format_identity, right.format_identity);
        assert!(!left.identity_matches());
        assert!(!right.identity_matches());
    }

    /// A row whose recorded tier contradicts the running one is evicted even
    /// though a hand-written identity string could be made to match: the
    /// evidence and the authority must agree, and disagreement fails closed.
    #[test]
    fn a_row_whose_profile_contradicts_this_build_is_evicted() {
        let mut file = file_of(SourceFormat::Markdown, "note.md");
        // No build compiles an enhanced Markdown extractor.
        file.extraction_profile = ExtractionProfile::Enhanced;
        assert!(!file.identity_matches());

        let (adopted, report) = on_disk(manifest_of([file])).adopt();
        assert_eq!(report.total(), 1);
        assert!(adopted.files.is_empty());
    }

    /// A newly admitted format leaves existing rows intact: nothing about the
    /// other formats' identities depends on which formats exist.
    #[test]
    fn admitting_a_format_leaves_other_formats_identities_untouched() {
        // Every shipped format's identity is derived from that format alone,
        // so the whole map is stable against the arrival of another entry.
        for spec in crate::format::format_specs() {
            let file = file_of(spec.format, "source");
            assert!(
                file.identity_matches(),
                "{} row must be reusable by the build that wrote it",
                spec.name
            );
        }
        let manifest = manifest_of(
            crate::format::format_specs()
                .iter()
                .map(|spec| file_of(spec.format, spec.name)),
        );
        let (adopted, report) = on_disk(manifest.clone()).adopt();
        assert!(report.is_empty());
        assert_eq!(adopted, manifest);
    }

    /// After eviction the aggregate invariants hold without any ledger
    /// arithmetic: the counts are derived from the surviving rows.
    #[test]
    fn eviction_preserves_the_aggregate_invariants() {
        let mut skipped = file_of(SourceFormat::Base, "broken.base");
        skipped.coverage = crate::extract::ExtractionCoverage::Quarantined;
        skipped.outcome = ManifestFileOutcome::Skipped;
        skipped.chunk_count = 0;
        skipped.warning = Some("invalid Base YAML".to_owned());
        let mut stale = file_of(SourceFormat::Pdf, "paper.pdf");
        stale.format_identity = Some("0".repeat(64));

        let manifest = manifest_of([
            file_of(SourceFormat::Markdown, "note.md"),
            skipped,
            stale,
            file_of(SourceFormat::Canvas, "board.canvas"),
        ]);
        let (adopted, report) = on_disk(manifest).adopt();

        assert_eq!(report.total(), 1);
        assert_eq!(adopted.document_count(), 2);
        let counts = adopted.source_format_counts();
        assert_eq!(counts.indexed_documents(), adopted.document_count());
        assert_eq!(counts.total_sources(), 3);
        assert_eq!(counts.pdf.indexed_complete, 0);
        assert_eq!(counts.base.quarantined, 1);
        adopted.validate().unwrap();
    }

    #[test]
    fn manifest_rejects_coverage_that_disagrees_with_document_outcome() {
        let mut file = file_of(SourceFormat::Base, "broken.base");
        file.coverage = crate::extract::ExtractionCoverage::Quarantined;
        file.outcome = ManifestFileOutcome::Indexed;
        file.chunk_count = 0;

        let error = manifest_of([file]).validate_readable().unwrap_err();
        assert!(error.to_string().contains("coverage"));
    }

    #[test]
    fn retained_manifest_file_preserves_content_evidence() {
        let mut previous = file_of(SourceFormat::Markdown, "note.md");
        previous.resource = Some(ResourceKey::new("tenant", "vault", "room"));
        previous.byte_length = 42;
        previous.mtime_nanos = 123;
        previous.chunk_count = 7;

        // Retention clones verbatim, identity included. That is safe only
        // because eviction runs before any plan can observe a stale row.
        assert_eq!(ManifestFile::retained(&previous), previous);
    }

    #[test]
    fn manifest_persists_and_aggregates_per_format_coverage() {
        let mut markdown = file_of(SourceFormat::Markdown, "note.md");
        markdown.coverage = crate::extract::ExtractionCoverage::IndexedPartial;
        markdown.warning = Some("partial metadata".to_owned());
        let mut base = file_of(SourceFormat::Base, "broken.base");
        base.coverage = crate::extract::ExtractionCoverage::Quarantined;
        base.outcome = ManifestFileOutcome::Skipped;
        base.chunk_count = 0;
        base.warning = Some("invalid Base YAML".to_owned());
        let manifest = manifest_of([markdown, base]);

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
        let manifest = manifest_of([
            file_of(SourceFormat::Markdown, "note.md"),
            file_of(SourceFormat::Excalidraw, "sketch.excalidraw"),
        ]);
        manifest.save(&path).unwrap();

        let (adopted, report) = Manifest::load(&path).unwrap().adopt();
        assert!(report.is_empty());
        assert_eq!(adopted, manifest);
    }

    /// `save` is the publication invariant: nothing that reaches disk may carry
    /// a foreign or absent identity.
    #[test]
    fn saving_a_stale_row_is_refused() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("manifest.json");
        let mut stale = file_of(SourceFormat::Docx, "report.docx");
        stale.format_identity = Some("a".repeat(64));

        let error = manifest_of([stale]).save(&path).unwrap_err().to_string();
        assert!(error.contains("report.docx"));
        assert!(error.contains("docx"));
        assert!(error.contains(format_identity_fingerprint(SourceFormat::Docx)));
        assert!(!path.exists(), "a refused manifest must not be written");
    }

    // ------------------------------------------------- read-only refusal

    /// The read-only counterpart of `adopt`, asserted on a record whose stale
    /// identity is a **literal** rather than something derived from the
    /// constant under test. A fixture built from `format_identity_fingerprint`
    /// moves with the predicate and would keep passing while the predicate
    /// widened; `"f" * 64` cannot.
    #[test]
    fn a_reader_refuses_a_record_holding_a_row_it_cannot_reuse() {
        let mut stale = file_of(SourceFormat::Pdf, "paper.pdf");
        stale.format_identity = Some("f".repeat(64));
        let manifest = manifest_of([file_of(SourceFormat::Markdown, "note.md"), stale]);
        // Readable — that is exactly why the reader needs its own gate.
        manifest.validate_readable().unwrap();

        let error = on_disk(manifest)
            .require_every_row_reusable()
            .unwrap_err()
            .to_string();
        assert!(error.contains("paper.pdf"), "{error}");
        assert!(error.contains(&"f".repeat(64)), "{error}");
        assert!(error.contains("kwiry index"), "{error}");
    }

    /// A row with no identity at all must refuse the read too. Spelled with a
    /// literal `None` so a `None => true` widening of the predicate cannot hide
    /// behind a fixture that always carries the running identity.
    #[test]
    fn a_reader_refuses_a_record_holding_a_row_with_no_identity() {
        let mut absent = file_of(SourceFormat::Canvas, "board.canvas");
        absent.format_identity = None;
        let error = on_disk(manifest_of([absent]))
            .require_every_row_reusable()
            .unwrap_err()
            .to_string();
        assert!(error.contains("board.canvas"), "{error}");
        assert!(error.contains("<absent>"), "{error}");
        assert!(error.contains("kwiry index"), "{error}");
    }

    /// A record every row of which this build could have produced is readable
    /// *and* servable, so the reader gate costs the ordinary path nothing.
    #[test]
    fn a_reader_admits_a_record_this_build_could_have_written() {
        let manifest = manifest_of(
            crate::format::format_specs()
                .iter()
                .map(|spec| file_of(spec.format, spec.name)),
        );
        on_disk(manifest).require_every_row_reusable().unwrap();
    }

    /// A tier disagreement is a different statement from a digest
    /// disagreement, and must not be reported as "identity X but this build
    /// compiles X" — a message naming the same value twice is one an operator
    /// cannot act on.
    #[test]
    fn a_profile_disagreement_names_the_profile_rather_than_a_self_identical_digest() {
        let mut file = file_of(SourceFormat::Markdown, "note.md");
        // No build compiles an enhanced Markdown extractor, and the identity
        // string is left exactly as this build would have stamped it.
        file.extraction_profile = ExtractionProfile::Enhanced;
        assert_eq!(
            file.format_identity.as_deref(),
            Some(format_identity_fingerprint(SourceFormat::Markdown))
        );

        let error = manifest_of([file]).validate().unwrap_err().to_string();
        assert!(error.contains("note.md"), "{error}");
        assert!(error.contains("enhanced"), "{error}");
        assert!(error.contains("portable"), "{error}");
        assert!(error.contains("kwiry index"), "{error}");
    }

    // ------------------------------------------------- migration honesty

    /// A format removed from the compiled vocabulary is a whole-artifact
    /// refusal by design — but serde refuses it inside `read_json`, before any
    /// gate runs, so the remedy has to be attached there or the operator gets
    /// a bare parser message and no instruction.
    #[test]
    fn a_record_this_build_cannot_parse_still_carries_the_rebuild_remedy() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("manifest.json");
        manifest_of([file_of(SourceFormat::Markdown, "note.md")])
            .save(&path)
            .unwrap();
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        document["files"][source_key("vault", "note.md")]["format"] =
            serde_json::json!("tiff-that-no-build-compiles");
        std::fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

        let error = Manifest::load(&path).unwrap_err().to_string();
        assert!(error.contains("unknown variant"), "{error}");
        assert!(error.contains("kwiry index"), "{error}");
        // One refusal, stated once: the remedy is folded into the existing
        // message rather than wrapped around a second copy of its prefix.
        assert_eq!(error.matches("state error:").count(), 1, "{error}");
    }

    /// Every build shipped before this wave declares
    /// `extraction_policy_fingerprint` as a required `String` with no serde
    /// default. Dropping it from what v5 writes turns a rollback into a
    /// deserialization failure; keeping it lets the older build reach its own
    /// `MANIFEST_VERSION` gate, which emits `run kwiry index`.
    ///
    /// The key set is asserted literally rather than derived from the struct,
    /// because the whole claim is about a *different* struct's requirements.
    #[test]
    fn a_written_record_keeps_every_key_a_pre_wave_build_requires() {
        const REQUIRED_BY_MANIFEST_VERSION_4: &[&str] = &[
            "manifest_version",
            "index_format_version",
            "chunking_version",
            "preparation_schema_version",
            "extraction_policy_fingerprint",
            "state_revision",
            "last_sync",
            "files",
        ];
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("manifest.json");
        manifest_of([file_of(SourceFormat::Markdown, "note.md")])
            .save(&path)
            .unwrap();

        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let written = document.as_object().unwrap();
        for key in REQUIRED_BY_MANIFEST_VERSION_4 {
            assert!(written.contains_key(*key), "v4 requires {key}");
        }
        assert_eq!(
            written["extraction_policy_fingerprint"],
            serde_json::json!(extraction_policy_fingerprint())
        );
        // A pre-wave build also requires every `ManifestFile` key it declared,
        // and `format_identity` is additive rather than a replacement.
        let row = written["files"][source_key("vault", "note.md")]
            .as_object()
            .unwrap();
        assert!(row.contains_key("extraction_profile"));
        assert!(row.contains_key("format_identity"));
    }

    /// Written, never read. A record claiming a foreign policy fingerprint is
    /// ignored rather than believed, so the field cannot become a second
    /// authority alongside the per-row identity — and cannot go stale on disk.
    #[test]
    fn the_retained_policy_fingerprint_is_never_read_back() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("manifest.json");
        manifest_of([file_of(SourceFormat::Text, "log.txt")])
            .save(&path)
            .unwrap();
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        document["extraction_policy_fingerprint"] = serde_json::json!("0".repeat(64));
        std::fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

        let (adopted, report) = Manifest::load(&path).unwrap().adopt();
        assert!(report.is_empty(), "a foreign policy value must not evict");
        adopted.save(&path).unwrap();

        let rewritten: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            rewritten["extraction_policy_fingerprint"],
            serde_json::json!(extraction_policy_fingerprint()),
            "the running build's value is re-derived, never carried over"
        );
    }
}
