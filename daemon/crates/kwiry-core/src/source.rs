// SPDX-License-Identifier: MIT OR Apache-2.0

use std::ops::Range;
use std::sync::Arc;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::frontmatter::parse_frontmatter;
use crate::lexical::{normalize_raw, technical_identifiers};
use crate::links::extract_wikilinks;
use crate::model::{
    CHUNK_OVERLAP_CHARS, CHUNKING_VERSION, Chunk, Frontmatter, MAX_CHUNK_CHARS, MAX_FILE_BYTES,
    PreparedChunk, PropertyBag, RetrievalMetadata,
};

pub const SOURCE_PREPARATION_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceExactMetadata {
    pub filename: Option<String>,
    pub stem: Option<String>,
    pub aliases: Vec<String>,
    pub title: Option<String>,
}

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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
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
    pub normalized_exact: SourceExactMetadata,
    /// Canonical source-level properties. The Obsidian ABI serializes this bag
    /// with explicit numeric variants so JavaScript cannot round unsafe integers
    /// or reclassify integral floats before the durable projection is built.
    #[serde(with = "frontmatter_abi")]
    pub frontmatter: PropertyBag,
    pub chunks: Vec<PreparedChunk>,
    pub kind: SourcePreparationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Deserialize)]
struct SourcePreparationWire {
    schema_version: u32,
    source_key: String,
    vault_id: String,
    #[serde(default)]
    room: Option<String>,
    path: String,
    format: SourceFormat,
    content_hash: Option<String>,
    byte_length: u64,
    mtime: u64,
    #[serde(with = "decimal_u128")]
    mtime_nanos: u128,
    retrieval: RetrievalMetadata,
    normalized_exact: SourceExactMetadata,
    #[serde(with = "frontmatter_abi")]
    frontmatter: PropertyBag,
    chunks: Vec<PreparedChunk>,
    kind: SourcePreparationKind,
    #[serde(default)]
    warning: Option<String>,
}

impl<'de> Deserialize<'de> for SourcePreparation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut wire = SourcePreparationWire::deserialize(deserializer)?;
        let source_frontmatter = Arc::new(Frontmatter::from_properties(&wire.frontmatter));
        for chunk in &mut wire.chunks {
            chunk.source_properties = wire.frontmatter.clone();
            chunk.source_frontmatter = Arc::clone(&source_frontmatter);
        }
        Ok(Self {
            schema_version: wire.schema_version,
            source_key: wire.source_key,
            vault_id: wire.vault_id,
            room: wire.room,
            path: wire.path,
            format: wire.format,
            content_hash: wire.content_hash,
            byte_length: wire.byte_length,
            mtime: wire.mtime,
            mtime_nanos: wire.mtime_nanos,
            retrieval: wire.retrieval,
            normalized_exact: wire.normalized_exact,
            frontmatter: wire.frontmatter,
            chunks: wire.chunks,
            kind: wire.kind,
            warning: wire.warning,
        })
    }
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

/// Records an oversized source from trusted host metadata without reading or
/// transferring its contents. The same descriptor validation and Rust-authored
/// identity path as ordinary preparation are used; only the content-dependent
/// work is deliberately absent.
pub fn prepare_oversized_source(
    descriptor: &SourceDescriptor,
) -> Result<SourcePreparation, SourcePreparationError> {
    validate_descriptor(descriptor)?;
    if descriptor.byte_length <= MAX_FILE_BYTES {
        return Err(validation_error(
            "oversized source does not exceed the file limit",
        ));
    }
    let retrieval = retrieval_metadata(&descriptor.path, Vec::new());
    Ok(SourcePreparation {
        schema_version: SOURCE_PREPARATION_SCHEMA_VERSION,
        source_key: source_key(&descriptor.vault_id, &descriptor.path),
        vault_id: descriptor.vault_id.clone(),
        room: descriptor.room.clone(),
        path: descriptor.path.clone(),
        format: descriptor.format,
        content_hash: None,
        byte_length: descriptor.byte_length,
        mtime: descriptor.mtime,
        mtime_nanos: descriptor.mtime_nanos,
        normalized_exact: source_exact_metadata(&retrieval, &Frontmatter::default()),
        retrieval,
        frontmatter: Default::default(),
        chunks: Vec::new(),
        kind: SourcePreparationKind::Skipped,
        warning: Some(format!(
            "skipped file larger than {MAX_FILE_BYTES} bytes ({})",
            descriptor.byte_length
        )),
    })
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
    if actual_byte_length > MAX_FILE_BYTES {
        return prepare_oversized_source(descriptor);
    }
    let source_key = source_key(&descriptor.vault_id, &descriptor.path);
    let empty_retrieval = retrieval_metadata(&descriptor.path, Vec::new());

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

    let (properties, frontmatter, aliases, body, warning) = match descriptor.format {
        SourceFormat::Markdown => parse_frontmatter(&source),
        SourceFormat::Text => (
            PropertyBag::default(),
            Frontmatter::default(),
            Vec::new(),
            source.as_str(),
            None,
        ),
    };
    let retrieval = retrieval_metadata(&descriptor.path, aliases);
    let normalized_exact = source_exact_metadata(&retrieval, &frontmatter);
    let source_frontmatter = Arc::new(frontmatter);
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
                frontmatter: Frontmatter::default(),
                links_out: links_out.clone(),
                mtime: descriptor.mtime,
                content_hash: content_hash.clone(),
                chunking_version: CHUNKING_VERSION,
            };
            let heading_text = chunk.heading_path.join(" ");
            chunks.push(PreparedChunk {
                normalized_heading: normalize_raw(&heading_text),
                heading_text,
                technical_identifiers: technical_identifiers(&chunk.content),
                source_properties: properties.clone(),
                source_frontmatter: Arc::clone(&source_frontmatter),
                chunk,
            });
            chunk_ix += 1;
        }
    }

    // A frontmatter-only note still needs a deterministic searchable result. Its empty portable
    // chunk carries no source-level projection; native indexing shares both source-owned views.
    if chunks.is_empty() && !properties.is_empty() {
        let chunk = Chunk {
            chunk_id: chunk_id(&descriptor.vault_id, &descriptor.path, &[], 0),
            vault_id: descriptor.vault_id.clone(),
            room: descriptor.room.clone(),
            path: descriptor.path.clone(),
            heading_path: Vec::new(),
            content: String::new(),
            frontmatter: Frontmatter::default(),
            links_out,
            mtime: descriptor.mtime,
            content_hash: content_hash.clone(),
            chunking_version: CHUNKING_VERSION,
        };
        chunks.push(PreparedChunk {
            heading_text: String::new(),
            normalized_heading: None,
            technical_identifiers: Vec::new(),
            source_properties: properties.clone(),
            source_frontmatter: Arc::clone(&source_frontmatter),
            chunk,
        });
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
        normalized_exact,
        frontmatter: properties,
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
        normalized_exact: source_exact_metadata(&retrieval, &Frontmatter::default()),
        retrieval,
        frontmatter: Default::default(),
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

fn source_exact_metadata(
    retrieval: &RetrievalMetadata,
    frontmatter: &Frontmatter,
) -> SourceExactMetadata {
    SourceExactMetadata {
        filename: normalize_raw(&retrieval.filename),
        stem: normalize_raw(&retrieval.stem),
        aliases: retrieval
            .aliases
            .iter()
            .filter_map(|alias| normalize_raw(alias))
            .collect(),
        title: frontmatter.title().and_then(normalize_raw),
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

mod frontmatter_abi {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

    use crate::model::{PropertyBag, PropertyValue};

    #[derive(Serialize, Deserialize)]
    #[serde(
        tag = "type",
        content = "value",
        rename_all = "snake_case",
        deny_unknown_fields
    )]
    enum AbiPropertyValue {
        Null,
        Boolean(bool),
        I64(String),
        U64(String),
        F64(String),
        String(String),
        Sequence(Vec<Self>),
        Map(BTreeMap<String, Self>),
    }

    impl From<&PropertyValue> for AbiPropertyValue {
        fn from(value: &PropertyValue) -> Self {
            match value {
                PropertyValue::Null => Self::Null,
                PropertyValue::Bool(value) => Self::Boolean(*value),
                PropertyValue::I64(value) => Self::I64(value.to_string()),
                PropertyValue::U64(value) => Self::U64(value.to_string()),
                PropertyValue::F64(value) => Self::F64(format!("{:016x}", value.to_bits())),
                PropertyValue::String(value) => Self::String(value.clone()),
                PropertyValue::Sequence(values) => {
                    Self::Sequence(values.iter().map(Self::from).collect())
                }
                PropertyValue::Map(values) => Self::Map(
                    values
                        .iter()
                        .map(|(name, value)| (name.clone(), Self::from(value)))
                        .collect(),
                ),
            }
        }
    }

    impl TryFrom<AbiPropertyValue> for PropertyValue {
        type Error = String;

        fn try_from(value: AbiPropertyValue) -> Result<Self, Self::Error> {
            match value {
                AbiPropertyValue::Null => Ok(Self::Null),
                AbiPropertyValue::Boolean(value) => Ok(Self::Bool(value)),
                AbiPropertyValue::I64(value) => value
                    .parse()
                    .map(Self::I64)
                    .map_err(|_| "invalid i64 property value".to_owned()),
                AbiPropertyValue::U64(value) => value
                    .parse()
                    .map(Self::U64)
                    .map_err(|_| "invalid u64 property value".to_owned()),
                AbiPropertyValue::F64(value) => {
                    if value.len() != 16 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return Err("invalid f64 property value".to_owned());
                    }
                    u64::from_str_radix(&value, 16)
                        .map(f64::from_bits)
                        .map(Self::F64)
                        .map_err(|_| "invalid f64 property value".to_owned())
                }
                AbiPropertyValue::String(value) => Ok(Self::String(value)),
                AbiPropertyValue::Sequence(values) => values
                    .into_iter()
                    .map(Self::try_from)
                    .collect::<Result<Vec<_>, _>>()
                    .map(Self::Sequence),
                AbiPropertyValue::Map(values) => values
                    .into_iter()
                    .map(|(name, value)| Self::try_from(value).map(|value| (name, value)))
                    .collect::<Result<BTreeMap<_, _>, _>>()
                    .map(Self::Map),
            }
        }
    }

    pub fn serialize<S>(frontmatter: &PropertyBag, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        frontmatter
            .iter()
            .map(|(name, value)| (name, AbiPropertyValue::from(value)))
            .collect::<BTreeMap<_, _>>()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<PropertyBag, D::Error>
    where
        D: Deserializer<'de>,
    {
        let values = BTreeMap::<String, AbiPropertyValue>::deserialize(deserializer)?;
        let properties = values
            .into_iter()
            .map(|(name, value)| {
                PropertyValue::try_from(value)
                    .map(|value| (name, value))
                    .map_err(de::Error::custom)
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(PropertyBag::from_properties(properties))
    }
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
    use crate::model::PropertyValue;

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
    fn projects_rust_normalized_exact_metadata_with_unicode_scalar_bounds() {
        let rockets = "🚀".repeat(260);
        let source = format!(
            "---\ntitle: 'RÉSUMÉ   Cache'\naliases:\n  - '  Mixed   Alias  '\n  - '{rockets}'\n---\n# API   Surface\nBody\n"
        );
        let prepared = prepare_source_buffer(
            &descriptor(
                "Folder/RÉSUMÉ   Cache.md",
                SourceFormat::Markdown,
                source.as_bytes(),
            ),
            source.as_bytes(),
        )
        .unwrap();

        assert_eq!(prepared.schema_version, SOURCE_PREPARATION_SCHEMA_VERSION);
        assert_eq!(
            prepared.normalized_exact,
            SourceExactMetadata {
                filename: Some("résumé cache.md".to_owned()),
                stem: Some("résumé cache".to_owned()),
                aliases: vec!["mixed alias".to_owned(), "🚀".repeat(256)],
                title: Some("résumé cache".to_owned()),
            }
        );
        assert_eq!(
            prepared.chunks[0].normalized_heading.as_deref(),
            Some("api surface")
        );

        let bounded = normalize_raw(&rockets).unwrap();
        assert_eq!(bounded.chars().count(), 256);
        assert!(bounded.chars().all(|character| character == '🚀'));
    }

    #[test]
    fn carries_a_thousand_open_properties_without_truncation() {
        let mut source = String::from("---\n");
        for index in 0..1_000 {
            source.push_str(&format!("property_{index}: value_{index}\n"));
        }
        source.push_str("---\nBody\n");

        let prepared = prepare_source_buffer(
            &descriptor(
                "many-properties.md",
                SourceFormat::Markdown,
                source.as_bytes(),
            ),
            source.as_bytes(),
        )
        .unwrap();
        let frontmatter = &prepared.frontmatter;

        assert_eq!(frontmatter.len(), 1_000);
        assert_eq!(
            frontmatter.get("property_999"),
            Some(&PropertyValue::String("value_999".to_owned()))
        );
    }

    #[test]
    fn carries_deep_property_maps_within_the_corruption_boundary() {
        // Thirty-two levels is adversarially deep while remaining below the explicit 64-level
        // corruption/call-stack boundary shared by property construction and alias replay.
        const DEPTH: usize = 32;
        let mut source = String::from("---\nnested:\n");
        for depth in 0..DEPTH {
            source.push_str(&"  ".repeat(depth + 1));
            source.push_str(&format!("level_{depth}:\n"));
        }
        source.push_str(&"  ".repeat(DEPTH + 1));
        source.push_str("leaf: value\n---\nBody\n");

        let prepared = prepare_source_buffer(
            &descriptor(
                "deep-properties.md",
                SourceFormat::Markdown,
                source.as_bytes(),
            ),
            source.as_bytes(),
        )
        .unwrap();
        let mut value = prepared.frontmatter.get("nested").expect("nested property");
        for depth in 0..DEPTH {
            let PropertyValue::Map(map) = value else {
                panic!("level {depth} must remain a map");
            };
            value = map.get(&format!("level_{depth}")).expect("nested level");
        }
        let PropertyValue::Map(leaf) = value else {
            panic!("deepest value must remain a map");
        };
        assert_eq!(
            leaf.get("leaf"),
            Some(&PropertyValue::String("value".to_owned()))
        );
    }

    #[test]
    fn carries_a_twelve_hundred_element_property_array_once_per_source() {
        let values = (0..1_200)
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!("---\nitems: [{values}]\n---\nBody\n");

        let prepared = prepare_source_buffer(
            &descriptor("large-array.md", SourceFormat::Markdown, source.as_bytes()),
            source.as_bytes(),
        )
        .unwrap();
        let Some(PropertyValue::Sequence(items)) = prepared.frontmatter.get("items") else {
            panic!("items must remain a sequence");
        };

        assert_eq!(items.len(), 1_200);
        assert_eq!(items.first(), Some(&PropertyValue::I64(0)));
        assert_eq!(items.last(), Some(&PropertyValue::I64(1_199)));
    }

    #[test]
    fn permits_the_same_property_key_to_have_different_types_across_notes() {
        let cases = [
            ("integer.md", "signal: 7", PropertyValue::I64(7)),
            (
                "string.md",
                "signal: '7'",
                PropertyValue::String("7".to_owned()),
            ),
            ("boolean.md", "signal: true", PropertyValue::Bool(true)),
        ];

        for (path, yaml, expected) in cases {
            let source = format!("---\n{yaml}\n---\nBody\n");
            let prepared = prepare_source_buffer(
                &descriptor(path, SourceFormat::Markdown, source.as_bytes()),
                source.as_bytes(),
            )
            .unwrap();
            assert_eq!(prepared.frontmatter.get("signal"), Some(&expected));
        }
    }

    #[test]
    fn frontmatter_only_sources_emit_one_compact_search_chunk() {
        let source = b"---\npriority: 7\naliases: [Only Alias]\n---\n";
        let prepared = prepare_source_buffer(
            &descriptor("frontmatter-only.md", SourceFormat::Markdown, source),
            source,
        )
        .unwrap();

        assert_eq!(prepared.chunks.len(), 1);
        assert_eq!(prepared.chunks[0].chunking_version, 2);
        assert!(prepared.chunks[0].content.is_empty());
        assert_eq!(prepared.chunks[0].frontmatter, Frontmatter::default());
        assert_eq!(prepared.chunks[0].source_properties, prepared.frontmatter);
        assert_eq!(
            prepared.frontmatter.get("priority"),
            Some(&PropertyValue::I64(7))
        );
    }

    #[test]
    fn source_property_abi_preserves_numeric_variants_exactly() {
        let source = b"---\ni64_value: -9007199254740993\nu64_value: 18446744073709551615\nf64_value: 125.0\n---\nBody\n";
        let prepared = prepare_source_buffer(
            &descriptor("numeric.md", SourceFormat::Markdown, source),
            source,
        )
        .unwrap();
        let encoded = serde_json::to_value(&prepared).unwrap();

        assert_eq!(encoded["frontmatter"]["i64_value"]["type"], "i64");
        assert_eq!(
            encoded["frontmatter"]["i64_value"]["value"],
            "-9007199254740993"
        );
        assert_eq!(encoded["frontmatter"]["u64_value"]["type"], "u64");
        assert_eq!(
            encoded["frontmatter"]["u64_value"]["value"],
            "18446744073709551615"
        );
        assert_eq!(encoded["frontmatter"]["f64_value"]["type"], "f64");
        assert_eq!(
            encoded["frontmatter"]["f64_value"]["value"],
            "405f400000000000"
        );
        assert_eq!(
            encoded["chunks"][0]["chunk"]["frontmatter"],
            serde_json::json!({})
        );
        assert!(encoded["chunks"][0]["chunk"]["frontmatter"]["i64_value"].is_null());
        assert!(encoded["chunks"][0]["chunk"]["frontmatter"]["u64_value"].is_null());
        assert!(encoded["chunks"][0]["chunk"]["frontmatter"]["f64_value"].is_null());

        let restored: SourcePreparation = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored, prepared);
        assert!(
            restored.chunks[0]
                .source_properties
                .shares_storage_with(&restored.frontmatter)
        );
    }

    #[test]
    fn serializes_one_source_bag_without_chunk_count_amplification() {
        let payload = "property-payload-marker-".repeat(12_000);
        let body = "bodyword ".repeat(120_000);
        let source = format!("---\npayload: {payload}\n---\n{body}");
        let prepared = prepare_source_buffer(
            &descriptor(
                "amplification.md",
                SourceFormat::Markdown,
                source.as_bytes(),
            ),
            source.as_bytes(),
        )
        .unwrap();

        assert!(prepared.chunks.len() > 250);
        assert!(prepared.chunks.iter().all(|chunk| {
            chunk
                .source_properties
                .shares_storage_with(&prepared.frontmatter)
        }));
        let encoded = serde_json::to_string(&prepared).unwrap();
        assert_eq!(encoded.matches(&payload).count(), 1);
        assert!(
            encoded.len() < source.len() * 2,
            "serialized preparation unexpectedly amplified from {} to {} bytes",
            source.len(),
            encoded.len()
        );
    }

    #[test]
    fn serializes_legacy_fields_once_without_chunk_count_amplification() {
        let title = "legacy-title-marker-".repeat(7_000);
        let first_tag = "legacy-tag-marker-".repeat(5_000);
        let body = "bodyword ".repeat(120_000);
        let source = format!("---\ntitle: {title}\ntags: [{first_tag}, second]\n---\n{body}");
        let prepared = prepare_source_buffer(
            &descriptor(
                "legacy-amplification.md",
                SourceFormat::Markdown,
                source.as_bytes(),
            ),
            source.as_bytes(),
        )
        .unwrap();

        assert!(prepared.chunks.len() > 250);
        assert!(prepared.chunks.iter().all(|chunk| {
            Arc::ptr_eq(
                &chunk.source_frontmatter,
                &prepared.chunks[0].source_frontmatter,
            )
        }));
        let encoded = serde_json::to_string(&prepared).unwrap();
        assert_eq!(encoded.matches(&title).count(), 1);
        assert_eq!(encoded.matches(&first_tag).count(), 1);
        assert!(
            encoded.len() < source.len() * 2,
            "serialized legacy projection unexpectedly amplified from {} to {} bytes",
            source.len(),
            encoded.len()
        );
    }

    #[test]
    fn carries_a_megabyte_scale_property_value_without_truncation() {
        let payload = "x".repeat(2 * 1024 * 1024);
        let source = format!("---\npayload: {payload}\n---\nBody\n");

        let prepared = prepare_source_buffer(
            &descriptor(
                "large-property.md",
                SourceFormat::Markdown,
                source.as_bytes(),
            ),
            source.as_bytes(),
        )
        .unwrap();
        let Some(PropertyValue::String(actual)) = prepared.frontmatter.get("payload") else {
            panic!("payload must remain a string");
        };

        assert_eq!(actual.len(), payload.len());
        assert_eq!(actual, &payload);
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
