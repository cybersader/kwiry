use std::fs;
use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use sha2::{Digest, Sha256};

use crate::frontmatter::parse_frontmatter;
use crate::links::extract_wikilinks;
use crate::model::{
    CHUNK_OVERLAP_CHARS, CHUNKING_VERSION, Chunk, DiscoveredFile, FileIngestOutcome,
    FileOutcomeKind, IngestReport, IngestWarning, MAX_CHUNK_CHARS, VaultRegistration,
};
use crate::walk::discover_vault;

#[derive(Debug)]
struct Section<'a> {
    heading_path: Vec<String>,
    content: &'a str,
}

#[derive(Debug)]
struct HeadingMarker {
    start: usize,
    path: Vec<String>,
}

pub fn ingest_vault(vault: &VaultRegistration) -> IngestReport {
    let (outcomes, mut warnings) = ingest_vault_files(vault);
    let mut report = IngestReport::default();

    for mut outcome in outcomes {
        if outcome.kind == FileOutcomeKind::Indexed {
            report.documents += 1;
            report.chunks.append(&mut outcome.chunks);
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
) -> (Vec<FileIngestOutcome>, Vec<IngestWarning>) {
    let (files, warnings) = discover_vault(vault);
    let outcomes = files.iter().map(|file| ingest_file(vault, file)).collect();
    (outcomes, warnings)
}

pub(crate) fn ingest_file(vault: &VaultRegistration, file: &DiscoveredFile) -> FileIngestOutcome {
    let bytes = match fs::read(&file.absolute_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return file_outcome(
                vault,
                file,
                None,
                Vec::new(),
                FileOutcomeKind::TransientError,
                Some(error.to_string()),
            );
        }
    };
    let content_hash = hex_digest(&bytes);
    let source = match String::from_utf8(bytes) {
        Ok(source) => source,
        Err(error) => {
            return file_outcome(
                vault,
                file,
                Some(content_hash),
                Vec::new(),
                FileOutcomeKind::Skipped,
                Some(format!("skipped non-UTF-8 file: {error}")),
            );
        }
    };
    if source.contains('\0') {
        return file_outcome(
            vault,
            file,
            Some(content_hash),
            Vec::new(),
            FileOutcomeKind::Skipped,
            Some("skipped binary file containing NUL bytes".to_owned()),
        );
    }

    let (chunks, warning) = chunk_source_hashed(vault, file, &source, content_hash.clone());
    file_outcome(
        vault,
        file,
        Some(content_hash),
        chunks,
        FileOutcomeKind::Indexed,
        warning,
    )
}

fn file_outcome(
    vault: &VaultRegistration,
    file: &DiscoveredFile,
    content_hash: Option<String>,
    chunks: Vec<Chunk>,
    kind: FileOutcomeKind,
    warning: Option<String>,
) -> FileIngestOutcome {
    FileIngestOutcome {
        vault_id: vault.id.clone(),
        path: file.relative_path.clone(),
        content_hash,
        byte_length: file.byte_length,
        mtime: file.mtime,
        mtime_nanos: file.mtime_nanos,
        chunks,
        kind,
        warning: warning.map(|message| IngestWarning {
            path: file.absolute_path.clone(),
            message,
        }),
    }
}

#[cfg(test)]
fn chunk_source(
    vault: &VaultRegistration,
    file: &DiscoveredFile,
    source: &str,
) -> (Vec<Chunk>, Option<String>) {
    chunk_source_hashed(vault, file, source, hex_digest(source.as_bytes()))
}

fn chunk_source_hashed(
    vault: &VaultRegistration,
    file: &DiscoveredFile,
    source: &str,
    content_hash: String,
) -> (Vec<Chunk>, Option<String>) {
    let (frontmatter, body, frontmatter_warning) = if file.extension == "txt" {
        (Default::default(), source, None)
    } else {
        parse_frontmatter(source)
    };
    let links_out = extract_wikilinks(body);
    let sections = if file.extension == "txt" {
        vec![Section {
            heading_path: Vec::new(),
            content: body,
        }]
    } else {
        markdown_sections(body)
    };

    let mut chunks = Vec::new();
    let mut chunk_ix = 0_u64;
    for section in sections {
        for part in split_oversized(section.content) {
            if part.trim().is_empty() {
                continue;
            }
            let chunk_id = chunk_id(
                &vault.id,
                &file.relative_path,
                &section.heading_path,
                chunk_ix,
            );
            chunks.push(Chunk {
                chunk_id,
                vault_id: vault.id.clone(),
                room: vault.room.clone(),
                path: file.relative_path.clone(),
                heading_path: section.heading_path.clone(),
                content: part.trim().to_owned(),
                frontmatter: frontmatter.clone(),
                links_out: links_out.clone(),
                mtime: file.mtime,
                content_hash: content_hash.clone(),
                chunking_version: CHUNKING_VERSION,
            });
            chunk_ix += 1;
        }
    }

    (chunks, frontmatter_warning)
}

fn markdown_sections(source: &str) -> Vec<Section<'_>> {
    let mut markers = Vec::new();
    let mut heading_stack: Vec<(usize, String)> = Vec::new();
    let mut active_heading: Option<(usize, usize, String)> = None;

    for (event, range) in Parser::new_ext(source, Options::all()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                active_heading = Some((heading_level(level), range.start, String::new()));
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, _, heading)) = active_heading.as_mut() {
                    heading.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, _, heading)) = active_heading.as_mut() {
                    heading.push(' ');
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, start, heading)) = active_heading.take() {
                    while heading_stack
                        .last()
                        .is_some_and(|(parent_level, _)| *parent_level >= level)
                    {
                        heading_stack.pop();
                    }
                    heading_stack.push((level, heading.trim().to_owned()));
                    markers.push(HeadingMarker {
                        start,
                        path: heading_stack
                            .iter()
                            .map(|(_, heading)| heading.clone())
                            .collect(),
                    });
                }
            }
            _ => {}
        }
    }

    sections_from_markers(source, &markers)
}

fn heading_level(level: pulldown_cmark::HeadingLevel) -> usize {
    match level {
        pulldown_cmark::HeadingLevel::H1 => 1,
        pulldown_cmark::HeadingLevel::H2 => 2,
        pulldown_cmark::HeadingLevel::H3 => 3,
        pulldown_cmark::HeadingLevel::H4 => 4,
        pulldown_cmark::HeadingLevel::H5 => 5,
        pulldown_cmark::HeadingLevel::H6 => 6,
    }
}

fn sections_from_markers<'a>(source: &'a str, markers: &[HeadingMarker]) -> Vec<Section<'a>> {
    if markers.is_empty() {
        return vec![Section {
            heading_path: Vec::new(),
            content: source,
        }];
    }

    let mut sections = Vec::new();
    if markers[0].start > 0 {
        sections.push(Section {
            heading_path: Vec::new(),
            content: &source[..markers[0].start],
        });
    }

    for (index, marker) in markers.iter().enumerate() {
        let end = markers
            .get(index + 1)
            .map_or(source.len(), |next| next.start);
        sections.push(Section {
            heading_path: marker.path.clone(),
            content: &source[marker.start..end],
        });
    }
    sections
}

fn split_oversized(source: &str) -> Vec<&str> {
    let boundaries = char_boundaries(source);
    let char_count = boundaries.len().saturating_sub(1);
    if char_count <= MAX_CHUNK_CHARS {
        return vec![source];
    }

    let mut parts = Vec::new();
    let mut start = 0;
    while start < char_count {
        let target = (start + MAX_CHUNK_CHARS).min(char_count);
        let end = if target == char_count {
            target
        } else {
            find_split(source, &boundaries, start, target)
        };
        parts.push(&source[boundaries[start]..boundaries[end]]);
        if end == char_count {
            break;
        }
        let next = end.saturating_sub(CHUNK_OVERLAP_CHARS);
        start = if next > start { next } else { end };
    }
    parts
}

fn char_boundaries(source: &str) -> Vec<usize> {
    source
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(source.len()))
        .collect()
}

fn find_split(source: &str, boundaries: &[usize], start: usize, target: usize) -> usize {
    let floor = start + (MAX_CHUNK_CHARS * 3 / 4);
    let search: Range<usize> = floor.min(target)..target;

    for index in search.clone().rev() {
        let byte = boundaries[index];
        if source[..byte].ends_with("\n\n") {
            return index;
        }
    }
    for index in search.rev() {
        let byte = boundaries[index];
        if source[byte..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            return index;
        }
    }
    target
}

fn chunk_id(vault_id: &str, path: &str, heading_path: &[String], chunk_ix: u64) -> String {
    let heading_json = serde_json::to_vec(heading_path).expect("heading paths serialize");
    let mut digest = Sha256::new();
    digest.update(b"kwir-chunk-v1\0");
    update_component(&mut digest, vault_id.as_bytes());
    update_component(&mut digest, path.as_bytes());
    update_component(&mut digest, &heading_json);
    digest.update(chunk_ix.to_le_bytes());
    let digest = digest.finalize();
    format!("{digest:x}")
}

fn update_component(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use tempfile::tempdir;

    use super::*;

    fn file(path: &str, extension: &str) -> DiscoveredFile {
        DiscoveredFile {
            absolute_path: PathBuf::from(path),
            relative_path: path.into(),
            extension: extension.into(),
            byte_length: 0,
            mtime: 42,
            mtime_nanos: 42_000_000_000,
        }
    }

    fn vault() -> VaultRegistration {
        VaultRegistration {
            id: "fixture".into(),
            path: PathBuf::from("/fixture"),
            room: None,
        }
    }

    #[test]
    fn preserves_preamble_nested_and_repeated_headings() {
        let source = "Preamble\n\n# One\nFirst\n\n## Two\nSecond\n\n## Two\nThird\n";
        let (chunks, warning) = chunk_source(&vault(), &file("note.md", "md"), source);

        assert!(warning.is_none());
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].heading_path, Vec::<String>::new());
        assert_eq!(chunks[1].heading_path, ["One"]);
        assert_eq!(chunks[2].heading_path, ["One", "Two"]);
        assert_eq!(chunks[3].heading_path, ["One", "Two"]);
        assert_ne!(chunks[2].chunk_id, chunks[3].chunk_id);
    }

    #[test]
    fn skipped_heading_levels_do_not_add_empty_breadcrumbs() {
        let source = "# One\nFirst\n\n### Three\nThird\n\n## Two\nSecond\n";
        let (chunks, _) = chunk_source(&vault(), &file("note.md", "md"), source);

        assert_eq!(chunks[0].heading_path, ["One"]);
        assert_eq!(chunks[1].heading_path, ["One", "Three"]);
        assert_eq!(chunks[2].heading_path, ["One", "Two"]);
    }

    #[test]
    fn splits_oversized_sections_with_overlap() {
        let source = "word ".repeat(1_000);
        let parts = split_oversized(&source);

        assert!(parts.len() >= 2);
        assert!(
            parts
                .iter()
                .all(|part| part.chars().count() <= MAX_CHUNK_CHARS)
        );
        let first_tail: String = parts[0]
            .chars()
            .rev()
            .take(CHUNK_OVERLAP_CHARS)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        assert!(parts[1].starts_with(&first_tail));
    }

    #[test]
    fn chunk_ids_are_stable_but_change_with_path() {
        let source = "# Heading\nBody";
        let (first, _) = chunk_source(&vault(), &file("a.md", "md"), source);
        let (second, _) = chunk_source(&vault(), &file("a.md", "md"), source);
        let (moved, _) = chunk_source(&vault(), &file("b.md", "md"), source);

        assert_eq!(first[0].chunk_id, second[0].chunk_id);
        assert_ne!(first[0].chunk_id, moved[0].chunk_id);
    }

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
    fn plain_text_is_one_logical_section() {
        let (chunks, _) = chunk_source(
            &vault(),
            &file("note.txt", "txt"),
            "Plain text without headings",
        );
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].heading_path.is_empty());
    }
}
