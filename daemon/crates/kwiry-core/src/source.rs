use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::frontmatter::parse_frontmatter;
use crate::lexical::technical_identifiers;
use crate::links::extract_wikilinks;
use crate::model::{
    CHUNK_OVERLAP_CHARS, CHUNKING_VERSION, Chunk, MAX_CHUNK_CHARS, MAX_FILE_BYTES, PreparedChunk,
    RetrievalMetadata,
};

pub const SOURCE_PREPARATION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceFormat {
    Markdown,
    Text,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceDescriptor {
    pub vault_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    pub path: String,
    pub format: SourceFormat,
    pub byte_length: u64,
    pub mtime: u64,
    #[serde(with = "decimal_u128")]
    pub mtime_nanos: u128,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourcePreparationKind {
    Indexed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourcePreparation {
    pub schema_version: u32,
    pub source_key: String,
    pub vault_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    pub path: String,
    pub format: SourceFormat,
    pub content_hash: Option<String>,
    pub byte_length: u64,
    pub mtime: u64,
    #[serde(with = "decimal_u128")]
    pub mtime_nanos: u128,
    pub retrieval: RetrievalMetadata,
    pub chunks: Vec<PreparedChunk>,
    pub kind: SourcePreparationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SourcePreparationError {
    pub code: String,
    pub message: String,
}

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

pub fn prepare_source_buffer(
    descriptor: &SourceDescriptor,
    bytes: &[u8],
) -> Result<SourcePreparation, SourcePreparationError> {
    validate_descriptor(descriptor)?;
    let actual_byte_length = bytes.len() as u64;
    if descriptor.byte_length != actual_byte_length {
        return Err(validation_error(&format!(
            "byte_length {} does not match source buffer length {actual_byte_length}",
            descriptor.byte_length
        )));
    }
    let source_key = source_key(&descriptor.vault_id, &descriptor.path);
    let empty_retrieval = retrieval_metadata(&descriptor.path, Vec::new());

    if actual_byte_length > MAX_FILE_BYTES {
        return Ok(SourcePreparation {
            schema_version: SOURCE_PREPARATION_SCHEMA_VERSION,
            source_key,
            vault_id: descriptor.vault_id.clone(),
            room: descriptor.room.clone(),
            path: descriptor.path.clone(),
            format: descriptor.format,
            content_hash: None,
            byte_length: descriptor.byte_length,
            mtime: descriptor.mtime,
            mtime_nanos: descriptor.mtime_nanos,
            retrieval: empty_retrieval,
            chunks: Vec::new(),
            kind: SourcePreparationKind::Skipped,
            warning: Some(format!(
                "skipped file larger than {MAX_FILE_BYTES} bytes ({})",
                descriptor.byte_length
            )),
        });
    }

    let content_hash = hex_digest(bytes);
    let source = match String::from_utf8(bytes.to_vec()) {
        Ok(source) => source,
        Err(error) => {
            return Ok(skipped_preparation(
                descriptor,
                source_key,
                empty_retrieval,
                Some(content_hash),
                format!("skipped non-UTF-8 file: {error}"),
            ));
        }
    };
    if source.contains('\0') {
        return Ok(skipped_preparation(
            descriptor,
            source_key,
            empty_retrieval,
            Some(content_hash),
            "skipped binary file containing NUL bytes".to_owned(),
        ));
    }

    let (frontmatter, aliases, body, warning) = match descriptor.format {
        SourceFormat::Markdown => parse_frontmatter(&source),
        SourceFormat::Text => (Default::default(), Vec::new(), source.as_str(), None),
    };
    let retrieval = retrieval_metadata(&descriptor.path, aliases);
    let links_out = extract_wikilinks(body);
    let sections = match descriptor.format {
        SourceFormat::Markdown => markdown_sections(body),
        SourceFormat::Text => vec![Section {
            heading_path: Vec::new(),
            content: body,
        }],
    };

    let mut chunks = Vec::new();
    let mut chunk_ix = 0_u64;
    for section in sections {
        for part in split_oversized(section.content) {
            if part.trim().is_empty() {
                continue;
            }
            let chunk = Chunk {
                chunk_id: chunk_id(
                    &descriptor.vault_id,
                    &descriptor.path,
                    &section.heading_path,
                    chunk_ix,
                ),
                vault_id: descriptor.vault_id.clone(),
                room: descriptor.room.clone(),
                path: descriptor.path.clone(),
                heading_path: section.heading_path.clone(),
                content: part.trim().to_owned(),
                frontmatter: frontmatter.clone(),
                links_out: links_out.clone(),
                mtime: descriptor.mtime,
                content_hash: content_hash.clone(),
                chunking_version: CHUNKING_VERSION,
            };
            chunks.push(PreparedChunk {
                heading_text: chunk.heading_path.join(" "),
                technical_identifiers: technical_identifiers(&chunk.content),
                chunk,
            });
            chunk_ix += 1;
        }
    }

    Ok(SourcePreparation {
        schema_version: SOURCE_PREPARATION_SCHEMA_VERSION,
        source_key,
        vault_id: descriptor.vault_id.clone(),
        room: descriptor.room.clone(),
        path: descriptor.path.clone(),
        format: descriptor.format,
        content_hash: Some(content_hash),
        byte_length: descriptor.byte_length,
        mtime: descriptor.mtime,
        mtime_nanos: descriptor.mtime_nanos,
        retrieval,
        chunks,
        kind: SourcePreparationKind::Indexed,
        warning,
    })
}

pub fn source_key(vault_id: &str, path: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"kwiry-source-v1\0");
    update_component(&mut digest, vault_id.as_bytes());
    update_component(&mut digest, path.as_bytes());
    format!("{:x}", digest.finalize())
}

fn validate_descriptor(descriptor: &SourceDescriptor) -> Result<(), SourcePreparationError> {
    if descriptor.vault_id.trim().is_empty() {
        return Err(validation_error("vault_id must not be empty"));
    }
    if descriptor
        .room
        .as_ref()
        .is_some_and(|room| room.trim().is_empty())
    {
        return Err(validation_error("room must not be empty when provided"));
    }
    if descriptor.path.is_empty()
        || descriptor.path.starts_with('/')
        || descriptor.path.ends_with('/')
        || descriptor.path.contains('\\')
        || descriptor.path.contains('\0')
        || descriptor
            .path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(validation_error(
            "path must be a normalized vault-relative forward-slash file path",
        ));
    }
    Ok(())
}

fn validation_error(message: &str) -> SourcePreparationError {
    SourcePreparationError {
        code: "invalid_source".to_owned(),
        message: message.to_owned(),
    }
}

fn skipped_preparation(
    descriptor: &SourceDescriptor,
    source_key: String,
    retrieval: RetrievalMetadata,
    content_hash: Option<String>,
    warning: String,
) -> SourcePreparation {
    SourcePreparation {
        schema_version: SOURCE_PREPARATION_SCHEMA_VERSION,
        source_key,
        vault_id: descriptor.vault_id.clone(),
        room: descriptor.room.clone(),
        path: descriptor.path.clone(),
        format: descriptor.format,
        content_hash,
        byte_length: descriptor.byte_length,
        mtime: descriptor.mtime,
        mtime_nanos: descriptor.mtime_nanos,
        retrieval,
        chunks: Vec::new(),
        kind: SourcePreparationKind::Skipped,
        warning: Some(warning),
    }
}

pub(crate) fn retrieval_metadata(path: &str, aliases: Vec<String>) -> RetrievalMetadata {
    let filename = path.rsplit('/').next().unwrap_or(path).to_owned();
    let stem = filename
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map_or_else(|| filename.clone(), |(stem, _)| stem.to_owned());
    RetrievalMetadata {
        filename,
        stem,
        aliases,
    }
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
    digest.update(b"kwiry-chunk-v1\0");
    update_component(&mut digest, vault_id.as_bytes());
    update_component(&mut digest, path.as_bytes());
    update_component(&mut digest, &heading_json);
    digest.update(chunk_ix.to_le_bytes());
    format!("{:x}", digest.finalize())
}

fn update_component(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

mod decimal_u128 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &u128, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<u128, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(path: &str, format: SourceFormat, bytes: &[u8]) -> SourceDescriptor {
        SourceDescriptor {
            vault_id: "fixture".into(),
            room: None,
            path: path.into(),
            format,
            byte_length: bytes.len() as u64,
            mtime: 42,
            mtime_nanos: 42_000_000_000,
        }
    }

    #[test]
    fn preserves_preamble_nested_and_repeated_headings() {
        let source = b"Preamble\n\n# One\nFirst\n\n## Two\nSecond\n\n## Two\nThird\n";
        let prepared = prepare_source_buffer(
            &descriptor("note.md", SourceFormat::Markdown, source),
            source,
        )
        .unwrap();

        assert!(prepared.warning.is_none());
        assert_eq!(prepared.chunks.len(), 4);
        assert_eq!(prepared.chunks[0].heading_path, Vec::<String>::new());
        assert_eq!(prepared.chunks[1].heading_path, ["One"]);
        assert_eq!(prepared.chunks[2].heading_path, ["One", "Two"]);
        assert_eq!(prepared.chunks[3].heading_path, ["One", "Two"]);
        assert_ne!(prepared.chunks[2].chunk_id, prepared.chunks[3].chunk_id);
    }

    #[test]
    fn skipped_heading_levels_do_not_add_empty_breadcrumbs() {
        let source = b"# One\nFirst\n\n### Three\nThird\n\n## Two\nSecond\n";
        let prepared = prepare_source_buffer(
            &descriptor("note.md", SourceFormat::Markdown, source),
            source,
        )
        .unwrap();

        assert_eq!(prepared.chunks[0].heading_path, ["One"]);
        assert_eq!(prepared.chunks[1].heading_path, ["One", "Three"]);
        assert_eq!(prepared.chunks[2].heading_path, ["One", "Two"]);
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
        let source = b"# Heading\nBody";
        let first =
            prepare_source_buffer(&descriptor("a.md", SourceFormat::Markdown, source), source)
                .unwrap();
        let second =
            prepare_source_buffer(&descriptor("a.md", SourceFormat::Markdown, source), source)
                .unwrap();
        let moved =
            prepare_source_buffer(&descriptor("b.md", SourceFormat::Markdown, source), source)
                .unwrap();

        assert_eq!(first.chunks[0].chunk_id, second.chunks[0].chunk_id);
        assert_ne!(first.chunks[0].chunk_id, moved.chunks[0].chunk_id);
    }

    #[test]
    fn plain_text_is_one_logical_section() {
        let source = b"Plain text without headings";
        let prepared =
            prepare_source_buffer(&descriptor("note.txt", SourceFormat::Text, source), source)
                .unwrap();
        assert_eq!(prepared.chunks.len(), 1);
        assert!(prepared.chunks[0].heading_path.is_empty());
    }

    #[test]
    fn rejects_invalid_paths_and_preserves_skipped_hashes() {
        let source = b"text\0binary";
        let invalid = descriptor("../note.md", SourceFormat::Markdown, source);
        assert_eq!(
            prepare_source_buffer(&invalid, source).unwrap_err().code,
            "invalid_source"
        );

        let prepared = prepare_source_buffer(
            &descriptor("note.md", SourceFormat::Markdown, source),
            source,
        )
        .unwrap();
        assert_eq!(prepared.kind, SourcePreparationKind::Skipped);
        assert!(prepared.content_hash.is_some());
        assert!(prepared.warning.unwrap().contains("NUL"));
    }

    #[test]
    fn rejects_mismatched_lengths_and_sizes_actual_buffers() {
        let source = b"body";
        let mut underreported = descriptor("note.md", SourceFormat::Markdown, source);
        underreported.byte_length -= 1;
        let error = prepare_source_buffer(&underreported, source).unwrap_err();
        assert_eq!(error.code, "invalid_source");
        assert!(error.message.contains("does not match"));

        let mut overreported = descriptor("note.md", SourceFormat::Markdown, source);
        overreported.byte_length += 1;
        assert!(
            prepare_source_buffer(&overreported, source)
                .unwrap_err()
                .message
                .contains("does not match")
        );

        let oversized = vec![b'x'; MAX_FILE_BYTES as usize + 1];
        let prepared = prepare_source_buffer(
            &descriptor("large.md", SourceFormat::Markdown, &oversized),
            &oversized,
        )
        .unwrap();
        assert_eq!(prepared.kind, SourcePreparationKind::Skipped);
        assert_eq!(prepared.byte_length, MAX_FILE_BYTES + 1);
        assert!(prepared.content_hash.is_none());
    }

    #[test]
    fn serializes_nanoseconds_without_javascript_precision_loss() {
        let descriptor = SourceDescriptor {
            vault_id: "fixture".into(),
            room: None,
            path: "note.md".into(),
            format: SourceFormat::Markdown,
            byte_length: 4,
            mtime: 1_700_000_000,
            mtime_nanos: 1_700_000_000_123_456_789,
        };
        let encoded = serde_json::to_string(&descriptor).unwrap();
        assert!(encoded.contains("\"mtime_nanos\":\"1700000000123456789\""));
        assert_eq!(
            serde_json::from_str::<SourceDescriptor>(&encoded).unwrap(),
            descriptor
        );
    }
}
