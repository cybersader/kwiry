use std::fs::{File, Metadata};
use std::io::Read;

use crate::format::SourceFormat;
use crate::model::{
    DiscoveredFile, FileIngestOutcome, FileOutcomeKind, IngestReport, IngestWarning,
    MAX_FILE_BYTES, PreparedChunk, VaultRegistration,
};
use crate::policy::extraction_profile_for;
use crate::source::{
    SourceDescriptor, SourcePreparation, SourcePreparationKind, prepare_oversized_source,
    prepare_source_buffer, retrieval_metadata,
};
use crate::walk::{EnumerationResult, discover_vault};

pub fn ingest_vault(vault: &VaultRegistration) -> IngestReport {
    let (outcomes, enumeration) = ingest_vault_files(vault);
    let mut warnings = enumeration.warnings;
    let mut report = IngestReport::default();

    for outcome in outcomes {
        if outcome.kind == FileOutcomeKind::Indexed {
            report.documents += 1;
            report
                .chunks
                .extend(outcome.chunks.into_iter().map(|prepared| prepared.chunk));
        }
        if let Some(warning) = outcome.warning {
            warnings.push(warning);
        }
    }

    report.warnings = warnings;
    report
}

pub(crate) fn ingest_vault_files(
    vault: &VaultRegistration,
) -> (Vec<FileIngestOutcome>, EnumerationResult) {
    let enumeration = discover_vault(vault);
    let outcomes = enumeration
        .files
        .iter()
        .map(|file| ingest_file(vault, file))
        .collect();
    (outcomes, enumeration)
}

pub(crate) fn ingest_file(vault: &VaultRegistration, file: &DiscoveredFile) -> FileIngestOutcome {
    let format = SourceFormat::from_extension(&file.extension)
        .expect("vault discovery only admits registered source formats");
    // Dormant since PDF admission — every registered format is extractable, and
    // `format::tests` asserts that. Kept because the registry is what discovery
    // trusts: a format admitted to the closed set ahead of its extractor must
    // skip without reading bytes rather than reach a dispatcher arm that has
    // nothing to dispatch to.
    if !format.is_extractable() {
        return file_outcome(
            vault,
            file,
            crate::extract::ExtractionCoverage::SkippedNoExtractableText,
            None,
            Vec::new(),
            FileOutcomeKind::Skipped,
            Some(format!(
                "{} extraction is not yet supported; skipped without reading source bytes",
                format.as_str()
            )),
        );
    }
    let read = match bounded_snapshot_read(file) {
        Ok(read) => read,
        Err(ReadFailure::Unavailable) => {
            return file_outcome(
                vault,
                file,
                crate::extract::ExtractionCoverage::Unreadable,
                None,
                Vec::new(),
                FileOutcomeKind::TransientError,
                Some("host_read: source is unavailable".to_owned()),
            );
        }
        Err(ReadFailure::Stale) => {
            return file_outcome(
                vault,
                file,
                crate::extract::ExtractionCoverage::Unreadable,
                None,
                Vec::new(),
                FileOutcomeKind::TransientError,
                Some("host_read: source changed during bounded read".to_owned()),
            );
        }
    };
    let byte_length = match &read {
        SnapshotRead::Bytes(bytes) => bytes.len() as u64,
        SnapshotRead::Oversized(byte_length) => *byte_length,
    };
    let descriptor = SourceDescriptor {
        vault_id: vault.id.clone(),
        room: vault.room.clone(),
        path: file.relative_path.clone(),
        format,
        byte_length,
        mtime: file.mtime,
        mtime_nanos: file.mtime_nanos,
    };
    if matches!(read, SnapshotRead::Oversized(_)) {
        return match prepare_oversized_source(&descriptor) {
            Ok(preparation) => file_outcome_from_preparation(file, preparation),
            Err(_) => file_outcome(
                vault,
                file,
                crate::extract::ExtractionCoverage::Unreadable,
                None,
                Vec::new(),
                FileOutcomeKind::Skipped,
                Some("host_read: oversized source could not be recorded".to_owned()),
            ),
        };
    }
    let SnapshotRead::Bytes(bytes) = read else {
        unreachable!("oversized reads return above")
    };
    match prepare_source_buffer(&descriptor, &bytes) {
        Ok(preparation) => file_outcome_from_preparation(file, preparation),
        Err(error) => file_outcome(
            vault,
            file,
            crate::extract::ExtractionCoverage::Unreadable,
            None,
            Vec::new(),
            FileOutcomeKind::Skipped,
            Some(error.to_string()),
        ),
    }
}

enum SnapshotRead {
    Bytes(Vec<u8>),
    Oversized(u64),
}

enum ReadFailure {
    Unavailable,
    Stale,
}

fn bounded_snapshot_read(file: &DiscoveredFile) -> Result<SnapshotRead, ReadFailure> {
    let mut handle = File::open(&file.absolute_path).map_err(|_| ReadFailure::Unavailable)?;
    let before = handle.metadata().map_err(|_| ReadFailure::Unavailable)?;
    if before.len() != file.byte_length {
        return Err(ReadFailure::Stale);
    }
    if before.len() > MAX_FILE_BYTES {
        let after = handle.metadata().map_err(|_| ReadFailure::Unavailable)?;
        if !same_snapshot(&before, &after) {
            return Err(ReadFailure::Stale);
        }
        return Ok(SnapshotRead::Oversized(before.len()));
    }

    let capacity = usize::try_from(before.len().saturating_add(1))
        .unwrap_or(MAX_FILE_BYTES as usize + 1)
        .min(MAX_FILE_BYTES as usize + 1);
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| ReadFailure::Unavailable)?;
    handle
        .by_ref()
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ReadFailure::Unavailable)?;
    let after = handle.metadata().map_err(|_| ReadFailure::Unavailable)?;
    if !same_snapshot(&before, &after) || bytes.len() as u64 != before.len() {
        return Err(ReadFailure::Stale);
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Ok(SnapshotRead::Oversized(after.len().max(bytes.len() as u64)));
    }
    Ok(SnapshotRead::Bytes(bytes))
}

fn same_snapshot(before: &Metadata, after: &Metadata) -> bool {
    before.len() == after.len()
        && before.modified().ok() == after.modified().ok()
        && before.is_file() == after.is_file()
}

fn file_outcome_from_preparation(
    file: &DiscoveredFile,
    preparation: SourcePreparation,
) -> FileIngestOutcome {
    // A preparation from another extraction profile is refused, not reused.
    // Nothing downstream would notice on its own: chunk identity is
    // path-derived, so the other tier's rows would silently claim the
    // identities this tier's rows are about to claim.
    if let Err(error) = preparation.ensure_current_policy() {
        return FileIngestOutcome {
            vault_id: preparation.vault_id,
            path: preparation.path,
            format: preparation.format,
            extraction_profile: extraction_profile_for(preparation.format),
            coverage: crate::extract::ExtractionCoverage::Quarantined,
            content_hash: preparation.content_hash,
            byte_length: preparation.byte_length,
            mtime: preparation.mtime,
            mtime_nanos: preparation.mtime_nanos,
            chunks: Vec::new(),
            retrieval: preparation.retrieval,
            kind: FileOutcomeKind::Skipped,
            warning: Some(IngestWarning {
                path: file.absolute_path.clone(),
                message: error.message,
            }),
        };
    }
    FileIngestOutcome {
        vault_id: preparation.vault_id,
        path: preparation.path,
        format: preparation.format,
        extraction_profile: preparation.extraction_profile,
        coverage: preparation.coverage,
        content_hash: preparation.content_hash,
        byte_length: preparation.byte_length,
        mtime: preparation.mtime,
        mtime_nanos: preparation.mtime_nanos,
        chunks: preparation.chunks,
        retrieval: preparation.retrieval,
        kind: match preparation.kind {
            SourcePreparationKind::Indexed => FileOutcomeKind::Indexed,
            SourcePreparationKind::Skipped => FileOutcomeKind::Skipped,
        },
        warning: preparation.warning.map(|message| IngestWarning {
            path: file.absolute_path.clone(),
            message,
        }),
    }
}

fn file_outcome(
    vault: &VaultRegistration,
    file: &DiscoveredFile,
    coverage: crate::extract::ExtractionCoverage,
    content_hash: Option<String>,
    chunks: Vec<PreparedChunk>,
    kind: FileOutcomeKind,
    warning: Option<String>,
) -> FileIngestOutcome {
    FileIngestOutcome {
        vault_id: vault.id.clone(),
        path: file.relative_path.clone(),
        format: SourceFormat::from_extension(&file.extension)
            .expect("vault discovery only admits registered source formats"),
        extraction_profile: extraction_profile_for(
            SourceFormat::from_extension(&file.extension)
                .expect("vault discovery only admits registered source formats"),
        ),
        coverage,
        content_hash,
        byte_length: file.byte_length,
        mtime: file.mtime,
        mtime_nanos: file.mtime_nanos,
        chunks,
        retrieval: retrieval_metadata(&file.relative_path, Vec::new()),
        kind,
        warning: warning.map(|message| IngestWarning {
            path: file.absolute_path.clone(),
            message,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn ingest_warns_and_skips_binary_text_files() {
        let temporary = tempdir().unwrap();
        fs::write(temporary.path().join("nul.txt"), b"text\0binary").unwrap();
        let vault = VaultRegistration {
            id: "fixture".into(),
            path: temporary.path().to_path_buf(),
            room: None,
        };

        let report = ingest_vault(&vault);
        assert_eq!(report.documents, 0);
        assert!(report.chunks.is_empty());
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].message.contains("NUL"));
    }

    /// This test used `.pdf` as the stand-in for an unextractable format. PDF is
    /// admitted, and it was the last such format, so the pre-read refusal in
    /// `ingest_file` is now dormant by construction rather than removed — and
    /// that is what is asserted here, together with the fact that a `.pdf` now
    /// reaches the extractor instead of being turned away with an
    /// "extraction is not yet supported" warning.
    #[test]
    fn native_ingest_reads_pdf_bytes_and_no_registered_format_is_refused_pre_read() {
        for spec in crate::format::format_specs() {
            assert!(
                spec.extraction_supported,
                "{} would trip the dormant pre-read refusal",
                spec.name
            );
        }

        let temporary = tempdir().unwrap();
        fs::write(temporary.path().join("paper.pdf"), b"not a pdf at all").unwrap();
        let vault = VaultRegistration {
            id: "fixture".into(),
            path: temporary.path().to_path_buf(),
            room: None,
        };
        let file = DiscoveredFile {
            absolute_path: temporary.path().join("paper.pdf"),
            relative_path: "paper.pdf".into(),
            extension: "pdf".into(),
            byte_length: b"not a pdf at all".len() as u64,
            mtime: 42,
            mtime_nanos: 42_000_000_000,
        };

        let outcome = ingest_file(&vault, &file);

        // The bytes were read and judged: a quarantine is an extraction verdict,
        // which the pre-read refusal could never have produced.
        assert_eq!(outcome.kind, FileOutcomeKind::Skipped);
        assert_eq!(
            outcome.coverage,
            crate::extract::ExtractionCoverage::Quarantined
        );
        assert!(outcome.chunks.is_empty());
        assert!(
            outcome
                .warning
                .is_none_or(|warning| !warning.message.contains("not yet supported"))
        );
    }

    #[test]
    fn native_ingest_records_oversized_html_as_quarantined_without_reading_it() {
        let temporary = tempdir().unwrap();
        fs::File::create(temporary.path().join("large.html"))
            .unwrap()
            .set_len(crate::model::MAX_FILE_BYTES + 1)
            .unwrap();
        let vault = VaultRegistration {
            id: "fixture".into(),
            path: temporary.path().to_path_buf(),
            room: None,
        };
        let enumeration = discover_vault(&vault);
        assert_eq!(enumeration.files.len(), 1);

        let outcome = ingest_file(&vault, &enumeration.files[0]);
        assert_eq!(outcome.kind, FileOutcomeKind::Skipped);
        assert_eq!(
            outcome.coverage,
            crate::extract::ExtractionCoverage::Quarantined
        );
        assert!(outcome.chunks.is_empty());
        assert_eq!(
            outcome
                .warning
                .as_ref()
                .map(|warning| warning.message.as_str()),
            Some("HTML extraction exceeded a mandatory budget")
        );
    }

    #[test]
    fn native_ingest_delegates_all_source_outcomes_to_portable_preparation() {
        let temporary = tempdir().unwrap();
        let vault = VaultRegistration {
            id: "fixture".into(),
            path: temporary.path().to_path_buf(),
            room: None,
        };
        let cases = [
            (
                "note.md",
                b"---\ntitle: Fixture\naliases: [Alias]\n---\n# Heading\nCVE-2026-1234".to_vec(),
            ),
            ("nul.md", b"text\0binary".to_vec()),
            ("invalid.md", vec![0xff, 0xfe]),
            ("empty.md", Vec::new()),
            (
                "large.md",
                vec![b'x'; crate::model::MAX_FILE_BYTES as usize + 1],
            ),
        ];

        for (relative_path, bytes) in cases {
            let path = temporary.path().join(relative_path);
            fs::write(&path, &bytes).unwrap();
            let file = DiscoveredFile {
                absolute_path: path,
                relative_path: relative_path.into(),
                extension: "md".into(),
                byte_length: bytes.len() as u64,
                mtime: 42,
                mtime_nanos: 42_000_000_000,
            };
            let descriptor = SourceDescriptor {
                vault_id: vault.id.clone(),
                room: vault.room.clone(),
                path: file.relative_path.clone(),
                format: SourceFormat::Markdown,
                byte_length: file.byte_length,
                mtime: file.mtime,
                mtime_nanos: file.mtime_nanos,
            };

            let native = ingest_file(&vault, &file);
            let portable = prepare_source_buffer(&descriptor, &bytes).unwrap();

            assert_eq!(native.vault_id, portable.vault_id);
            assert_eq!(native.path, portable.path);
            assert_eq!(native.format, portable.format);
            assert_eq!(native.coverage, portable.coverage);
            assert_eq!(native.content_hash, portable.content_hash);
            assert_eq!(native.byte_length, portable.byte_length);
            assert_eq!(native.mtime, portable.mtime);
            assert_eq!(native.mtime_nanos, portable.mtime_nanos);
            assert_eq!(native.chunks, portable.chunks);
            assert_eq!(native.retrieval, portable.retrieval);
            assert_eq!(
                native.kind,
                match portable.kind {
                    SourcePreparationKind::Indexed => FileOutcomeKind::Indexed,
                    SourcePreparationKind::Skipped => FileOutcomeKind::Skipped,
                }
            );
            assert_eq!(
                native
                    .warning
                    .as_ref()
                    .map(|warning| warning.message.as_str()),
                portable.warning.as_deref()
            );
        }

        let path = temporary.path().join("changed.md");
        fs::write(&path, b"body").unwrap();
        let changed_during_read = DiscoveredFile {
            absolute_path: path,
            relative_path: "changed.md".into(),
            extension: "md".into(),
            byte_length: 3,
            mtime: 42,
            mtime_nanos: 42_000_000_000,
        };
        let outcome = ingest_file(&vault, &changed_during_read);
        assert_eq!(outcome.kind, FileOutcomeKind::TransientError);
        assert!(outcome.content_hash.is_none());
        assert_eq!(
            outcome.warning.unwrap().message,
            "host_read: source changed during bounded read"
        );

        #[cfg(unix)]
        {
            let path = temporary.path().join("bad\\name.md");
            fs::write(&path, b"body").unwrap();
            let invalid_path = DiscoveredFile {
                absolute_path: path,
                relative_path: "bad\\name.md".into(),
                extension: "md".into(),
                byte_length: 4,
                mtime: 42,
                mtime_nanos: 42_000_000_000,
            };
            let outcome = ingest_file(&vault, &invalid_path);
            assert_eq!(outcome.kind, FileOutcomeKind::Skipped);
            assert!(outcome.content_hash.is_none());
            assert!(outcome.warning.unwrap().message.contains("normalized"));
        }
    }
}
