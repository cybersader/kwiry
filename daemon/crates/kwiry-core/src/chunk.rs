use std::fs;

use crate::format::SourceFormat;
use crate::model::{
    DiscoveredFile, FileIngestOutcome, FileOutcomeKind, IngestReport, IngestWarning, PreparedChunk,
    VaultRegistration,
};
use crate::source::{
    SourceDescriptor, SourcePreparation, SourcePreparationKind, prepare_source_buffer,
    retrieval_metadata,
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
    let bytes = match fs::read(&file.absolute_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return file_outcome(
                vault,
                file,
                crate::extract::ExtractionCoverage::Unreadable,
                None,
                Vec::new(),
                FileOutcomeKind::TransientError,
                Some(error.to_string()),
            );
        }
    };
    let descriptor = SourceDescriptor {
        vault_id: vault.id.clone(),
        room: vault.room.clone(),
        path: file.relative_path.clone(),
        format,
        byte_length: file.byte_length,
        mtime: file.mtime,
        mtime_nanos: file.mtime_nanos,
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

fn file_outcome_from_preparation(
    file: &DiscoveredFile,
    preparation: SourcePreparation,
) -> FileIngestOutcome {
    FileIngestOutcome {
        vault_id: preparation.vault_id,
        path: preparation.path,
        format: preparation.format,
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

    #[test]
    fn native_ingest_refuses_unextractable_formats_before_reading_bytes() {
        let temporary = tempdir().unwrap();
        let vault = VaultRegistration {
            id: "fixture".into(),
            path: temporary.path().to_path_buf(),
            room: None,
        };
        let file = DiscoveredFile {
            absolute_path: temporary.path().join("missing.pdf"),
            relative_path: "missing.pdf".into(),
            extension: "pdf".into(),
            byte_length: 123,
            mtime: 42,
            mtime_nanos: 42_000_000_000,
        };

        let outcome = ingest_file(&vault, &file);

        assert_eq!(outcome.kind, FileOutcomeKind::Skipped);
        assert_eq!(
            outcome.coverage,
            crate::extract::ExtractionCoverage::SkippedNoExtractableText
        );
        assert!(outcome.content_hash.is_none());
        assert!(outcome.chunks.is_empty());
        assert!(
            outcome
                .warning
                .unwrap()
                .message
                .contains("without reading source bytes")
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
        assert_eq!(outcome.kind, FileOutcomeKind::Skipped);
        assert!(outcome.content_hash.is_none());
        assert!(outcome.warning.unwrap().message.contains("does not match"));

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
