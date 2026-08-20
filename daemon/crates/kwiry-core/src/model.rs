// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeMap;
#[cfg(feature = "native")]
use std::path::PathBuf;
use std::sync::Arc;

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};

use crate::extract::{ContentRole, ExtractionCoverage, SourceLocator};
use crate::format::SourceFormat;

pub const CHUNKING_VERSION: u64 = 2;
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_CHUNK_CHARS: usize = 4_000;
pub const CHUNK_OVERLAP_CHARS: usize = 400;
#[cfg(feature = "native")]
pub const DEFAULT_BIND: &str = "127.0.0.1:32189";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndexFreshnessBasis {
    #[default]
    StrictHash,
    MetadataAudit,
    ProducerManifest,
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default = "default_config_version")]
    pub version: u32,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub semantic: SemanticConfig,
    #[serde(default)]
    pub indexing: IndexingConfig,
    #[serde(default)]
    pub vaults: Vec<VaultRegistration>,
}

#[cfg(feature = "native")]
impl Default for Config {
    fn default() -> Self {
        Self {
            version: default_config_version(),
            server: ServerConfig::default(),
            auth: AuthConfig::default(),
            semantic: SemanticConfig::default(),
            indexing: IndexingConfig::default(),
            vaults: Vec::new(),
        }
    }
}

#[cfg(feature = "native")]
impl Config {
    pub fn resource_key(&self, vault: &VaultRegistration) -> Option<ResourceKey> {
        let auth = self.auth.openclast.as_ref()?;
        let room_id = vault.room.as_ref()?;
        Some(ResourceKey::new(
            auth.tenant_id.clone(),
            vault.id.clone(),
            room_id.clone(),
        ))
    }

    pub fn requires_restart_for(&self, next: &Self) -> bool {
        self.version != next.version
            || self.server != next.server
            || self.auth != next.auth
            || self.semantic != next.semantic
    }
}

#[cfg(feature = "native")]
const fn default_config_version() -> u32 {
    1
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HostProfile {
    #[default]
    Desktop,
    #[serde(rename = "openclast", alias = "open-clast")]
    OpenClast,
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    #[serde(default)]
    pub profile: HostProfile,
    #[serde(default = "default_bind")]
    pub bind: String,
}

#[cfg(feature = "native")]
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            profile: HostProfile::Desktop,
            bind: default_bind(),
        }
    }
}

#[cfg(feature = "native")]
fn default_bind() -> String {
    DEFAULT_BIND.to_owned()
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openclast: Option<OpenClastAuthConfig>,
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenClastAuthConfig {
    pub tenant_id: String,
    pub issuer: String,
    pub audience: String,
    pub jwks_file: PathBuf,
    #[serde(default = "default_capability_ttl_seconds")]
    pub max_token_ttl_seconds: u64,
}

#[cfg(feature = "native")]
const fn default_capability_ttl_seconds() -> u64 {
    60
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceKey {
    pub tenant_id: String,
    pub vault_id: String,
    pub room_id: String,
}

#[cfg(feature = "native")]
impl ResourceKey {
    pub fn new(
        tenant_id: impl Into<String>,
        vault_id: impl Into<String>,
        room_id: impl Into<String>,
    ) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            vault_id: vault_id.into(),
            room_id: room_id.into(),
        }
    }
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticConfig {
    #[serde(default)]
    pub enabled: bool,
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexingConfig {
    #[serde(default)]
    pub basis: IndexFreshnessBasis,
    #[serde(default = "default_audit_sources_per_pass")]
    pub audit_sources_per_pass: usize,
    #[serde(default = "default_audit_bytes_per_pass")]
    pub audit_bytes_per_pass: u64,
    #[serde(default = "default_racy_window_millis")]
    pub racy_window_millis: u64,
}

#[cfg(feature = "native")]
impl Default for IndexingConfig {
    fn default() -> Self {
        Self {
            basis: IndexFreshnessBasis::StrictHash,
            audit_sources_per_pass: default_audit_sources_per_pass(),
            audit_bytes_per_pass: default_audit_bytes_per_pass(),
            racy_window_millis: default_racy_window_millis(),
        }
    }
}

#[cfg(feature = "native")]
const fn default_audit_sources_per_pass() -> usize {
    16
}

#[cfg(feature = "native")]
const fn default_audit_bytes_per_pass() -> u64 {
    64 * 1024 * 1024
}

#[cfg(feature = "native")]
const fn default_racy_window_millis() -> u64 {
    2_000
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultRegistration {
    #[serde(rename = "vault_id")]
    pub id: String,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
}

// This is a call-stack safety boundary for maliciously recursive YAML, not a content-policy
// limit. Cardinality and value sizes remain governed only by the accepted source-file bytes, so
// legitimate large properties cannot repeat the links_out incident by tripping an invented cap.
pub(crate) const MAX_PROPERTY_NESTING_DEPTH: usize = 64;

#[derive(Debug, Clone)]
pub enum PropertyValue {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    String(String),
    Sequence(Vec<Self>),
    Map(BTreeMap<String, Self>),
}

impl PartialEq for PropertyValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(left), Self::Bool(right)) => left == right,
            (Self::I64(left), Self::I64(right)) => left == right,
            (Self::U64(left), Self::U64(right)) => left == right,
            (Self::F64(left), Self::F64(right)) => left.to_bits() == right.to_bits(),
            (Self::String(left), Self::String(right)) => left == right,
            (Self::Sequence(left), Self::Sequence(right)) => left == right,
            (Self::Map(left), Self::Map(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for PropertyValue {}

// PropertyValue deliberately has no general-purpose Serialize implementation. The complete open
// bag crosses persistence and JavaScript boundaries only through the tagged, precision-safe source
// ABI in source.rs; chunk/result metadata is a separate compact string projection.
impl<'de> Deserialize<'de> for PropertyValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        PropertyValueSeed { depth: 0 }.deserialize(deserializer)
    }
}

struct PropertyValueSeed {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for PropertyValueSeed {
    type Value = PropertyValue;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(PropertyValueVisitor { depth: self.depth })
    }
}

struct PropertyValueVisitor {
    depth: usize,
}

impl PropertyValueVisitor {
    fn child_seed<E>(&self) -> Result<PropertyValueSeed, E>
    where
        E: de::Error,
    {
        if self.depth >= MAX_PROPERTY_NESTING_DEPTH {
            return Err(E::custom(format!(
                "property nesting exceeds {MAX_PROPERTY_NESTING_DEPTH} levels"
            )));
        }
        Ok(PropertyValueSeed {
            depth: self.depth + 1,
        })
    }
}

impl<'de> Visitor<'de> for PropertyValueVisitor {
    type Value = PropertyValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a YAML property value")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(PropertyValue::Null)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(PropertyValue::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(PropertyValue::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(PropertyValue::I64(value))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(i64::try_from(value).map_or(PropertyValue::U64(value), PropertyValue::I64))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(PropertyValue::F64(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(PropertyValue::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(PropertyValue::String(value))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(self.child_seed()?)? {
            values.push(value);
        }
        Ok(PropertyValue::Sequence(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            values.insert(key, map.next_value_seed(self.child_seed()?)?);
        }
        Ok(PropertyValue::Map(values))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PropertyBag {
    // One immutable allocation is shared by the source preparation and its native prepared chunks.
    // PreparedChunk skips this pointer during serialization, so the portable ABI emits the bag once.
    properties: Arc<BTreeMap<String, PropertyValue>>,
}

impl PropertyBag {
    pub fn from_properties(properties: BTreeMap<String, PropertyValue>) -> Self {
        Self {
            properties: Arc::new(properties),
        }
    }

    pub fn get(&self, name: &str) -> Option<&PropertyValue> {
        self.properties.get(name)
    }

    pub fn len(&self) -> usize {
        self.properties.len()
    }

    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &PropertyValue)> {
        self.properties.iter()
    }

    #[cfg(test)]
    pub(crate) fn shares_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.properties, &other.properties)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Frontmatter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

impl Frontmatter {
    pub fn from_properties(properties: &PropertyBag) -> Self {
        let title = properties.get("title").and_then(property_string);
        let description = properties.get("description").and_then(property_string);
        let tags = properties
            .get("tags")
            .map_or_else(Vec::new, property_strings);
        let status = properties.get("status").and_then(property_string);
        // serde-saphyr currently supplies YAML dates as strings. The compact projection clones
        // that string while the original typed value remains in the source-owned property bag.
        let date = properties.get("date").and_then(property_string);

        Self {
            title,
            description,
            tags,
            status,
            date,
        }
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn date(&self) -> Option<&str> {
        self.date.as_deref()
    }
}

fn property_strings(value: &PropertyValue) -> Vec<String> {
    match value {
        PropertyValue::Sequence(values) => values.iter().filter_map(property_string).collect(),
        _ => property_string(value).into_iter().collect(),
    }
}

fn property_string(value: &PropertyValue) -> Option<String> {
    match value {
        PropertyValue::Bool(value) => Some(value.to_string()),
        PropertyValue::I64(value) => Some(value.to_string()),
        PropertyValue::U64(value) => Some(value.to_string()),
        PropertyValue::F64(value) => Some(value.to_string()),
        PropertyValue::String(value) => Some(value.clone()),
        PropertyValue::Null | PropertyValue::Sequence(_) | PropertyValue::Map(_) => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Chunk {
    pub chunk_id: String,
    pub vault_id: String,
    pub room: Option<String>,
    pub path: String,
    pub heading_path: Vec<String>,
    pub content: String,
    pub frontmatter: Frontmatter,
    pub links_out: Vec<String>,
    pub mtime: u64,
    pub content_hash: String,
    pub chunking_version: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetrievalMetadata {
    pub filename: String,
    pub stem: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedChunk {
    pub chunk: Chunk,
    pub heading_text: String,
    pub normalized_heading: Option<String>,
    pub technical_identifiers: Vec<String>,
    /// Format-local lexical class. Primary is omitted so every pre-Excel
    /// preparation remains byte-for-byte identical under schema 9.
    #[serde(default, skip_serializing_if = "ContentRole::is_primary")]
    pub content_role: ContentRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_locator: Option<SourceLocator>,
    // Native indexing needs source-level projections and non-ranking extraction metadata while
    // reconciliation still transports chunks independently. These shared views stay outside the
    // portable chunk ABI; the serialized SourcePreparation owns format and coverage at top level.
    #[serde(skip, default)]
    pub(crate) source_format: Option<crate::format::SourceFormat>,
    #[serde(skip, default)]
    pub(crate) extraction_coverage: Option<crate::extract::ExtractionCoverage>,
    #[serde(skip, default)]
    pub(crate) source_properties: PropertyBag,
    #[serde(skip, default = "default_shared_frontmatter")]
    pub(crate) source_frontmatter: Arc<Frontmatter>,
}

fn default_shared_frontmatter() -> Arc<Frontmatter> {
    Arc::new(Frontmatter::default())
}

impl std::ops::Deref for PreparedChunk {
    type Target = Chunk;

    fn deref(&self) -> &Self::Target {
        &self.chunk
    }
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestWarning {
    pub path: PathBuf,
    pub message: String,
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestReport {
    pub documents: usize,
    pub chunks: Vec<Chunk>,
    pub warnings: Vec<IngestWarning>,
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LexicalSearchRequest {
    pub query: String,
    pub limit: usize,
    pub vault_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    pub chunk_id: String,
    pub vault_id: String,
    pub path: String,
    pub heading_path: Vec<String>,
    pub format: SourceFormat,
    pub coverage: ExtractionCoverage,
    pub locator: Option<SourceLocator>,
    pub score: f32,
    pub excerpt: String,
    pub frontmatter: Frontmatter,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct ExtractionCoverageCounts {
    pub indexed_complete: usize,
    pub indexed_partial: usize,
    pub skipped_no_extractable_text: usize,
    pub unreadable: usize,
    pub quarantined: usize,
}

impl ExtractionCoverageCounts {
    fn record(&mut self, coverage: ExtractionCoverage) {
        match coverage {
            ExtractionCoverage::IndexedComplete => self.indexed_complete += 1,
            ExtractionCoverage::IndexedPartial => self.indexed_partial += 1,
            ExtractionCoverage::SkippedNoExtractableText => {
                self.skipped_no_extractable_text += 1;
            }
            ExtractionCoverage::Unreadable => self.unreadable += 1,
            ExtractionCoverage::Quarantined => self.quarantined += 1,
        }
    }

    pub const fn indexed_documents(&self) -> usize {
        self.indexed_complete + self.indexed_partial
    }

    pub const fn total_sources(&self) -> usize {
        self.indexed_documents()
            + self.skipped_no_extractable_text
            + self.unreadable
            + self.quarantined
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceFormatCounts {
    pub markdown: ExtractionCoverageCounts,
    pub text: ExtractionCoverageCounts,
    pub base: ExtractionCoverageCounts,
    pub canvas: ExtractionCoverageCounts,
    pub docx: ExtractionCoverageCounts,
    pub pdf: ExtractionCoverageCounts,
    pub excalidraw: ExtractionCoverageCounts,
    pub excel: ExtractionCoverageCounts,
    pub html: ExtractionCoverageCounts,
}

impl SourceFormatCounts {
    pub fn record(&mut self, format: SourceFormat, coverage: ExtractionCoverage) {
        let counts = match format {
            SourceFormat::Markdown => &mut self.markdown,
            SourceFormat::Text => &mut self.text,
            SourceFormat::Base => &mut self.base,
            SourceFormat::Canvas => &mut self.canvas,
            SourceFormat::Docx => &mut self.docx,
            SourceFormat::Pdf => &mut self.pdf,
            SourceFormat::Excalidraw => &mut self.excalidraw,
            SourceFormat::Excel => &mut self.excel,
            SourceFormat::Html => &mut self.html,
        };
        counts.record(coverage);
    }

    pub const fn indexed_documents(&self) -> usize {
        self.markdown.indexed_documents()
            + self.text.indexed_documents()
            + self.base.indexed_documents()
            + self.canvas.indexed_documents()
            + self.docx.indexed_documents()
            + self.excalidraw.indexed_documents()
            + self.pdf.indexed_documents()
            + self.excel.indexed_documents()
            + self.html.indexed_documents()
    }

    pub const fn total_sources(&self) -> usize {
        self.markdown.total_sources()
            + self.text.total_sources()
            + self.base.total_sources()
            + self.canvas.total_sources()
            + self.docx.total_sources()
            + self.excalidraw.total_sources()
            + self.pdf.total_sources()
            + self.excel.total_sources()
            + self.html.total_sources()
    }
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexStats {
    pub documents: usize,
    pub chunks: usize,
    pub warnings: Vec<IngestWarning>,
    pub source_format_counts: SourceFormatCounts,
}

#[cfg(feature = "native")]
impl IndexStats {
    pub(crate) fn record_outcome(&mut self, outcome: &FileIngestOutcome) {
        if outcome.content_hash.is_none() {
            return;
        }
        self.source_format_counts
            .record(outcome.format, outcome.coverage);
        if outcome.kind == FileOutcomeKind::Indexed {
            self.documents += 1;
        }
    }
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveredFile {
    pub absolute_path: PathBuf,
    pub relative_path: String,
    pub extension: String,
    pub byte_length: u64,
    pub mtime: u64,
    pub mtime_nanos: u128,
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileOutcomeKind {
    Indexed,
    Skipped,
    TransientError,
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileIngestOutcome {
    pub vault_id: String,
    pub path: String,
    pub format: SourceFormat,
    /// The extractor tier that produced this outcome. Recorded on the manifest
    /// entry so a later run can see that its own tier differs.
    pub extraction_profile: crate::policy::ExtractionProfile,
    pub coverage: ExtractionCoverage,
    pub content_hash: Option<String>,
    pub byte_length: u64,
    pub mtime: u64,
    pub mtime_nanos: u128,
    pub chunks: Vec<PreparedChunk>,
    pub retrieval: RetrievalMetadata,
    pub kind: FileOutcomeKind,
    pub warning: Option<IngestWarning>,
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;

    #[test]
    fn source_format_counts_are_closed_complete_and_match_indexed_documents() {
        let mut counts = SourceFormatCounts::default();
        counts.record(SourceFormat::Markdown, ExtractionCoverage::IndexedComplete);
        counts.record(SourceFormat::Markdown, ExtractionCoverage::IndexedPartial);
        counts.record(
            SourceFormat::Text,
            ExtractionCoverage::SkippedNoExtractableText,
        );
        counts.record(SourceFormat::Base, ExtractionCoverage::Quarantined);
        counts.record(SourceFormat::Canvas, ExtractionCoverage::Unreadable);
        counts.record(SourceFormat::Html, ExtractionCoverage::Quarantined);

        assert_eq!(counts.indexed_documents(), 2);
        assert_eq!(counts.total_sources(), 6);
        assert_eq!(
            serde_json::to_value(counts).unwrap(),
            serde_json::json!({
                "markdown": {
                    "indexed-complete": 1,
                    "indexed-partial": 1,
                    "skipped-no-extractable-text": 0,
                    "unreadable": 0,
                    "quarantined": 0
                },
                "text": {
                    "indexed-complete": 0,
                    "indexed-partial": 0,
                    "skipped-no-extractable-text": 1,
                    "unreadable": 0,
                    "quarantined": 0
                },
                "base": {
                    "indexed-complete": 0,
                    "indexed-partial": 0,
                    "skipped-no-extractable-text": 0,
                    "unreadable": 0,
                    "quarantined": 1
                },
                "canvas": {
                    "indexed-complete": 0,
                    "indexed-partial": 0,
                    "skipped-no-extractable-text": 0,
                    "unreadable": 1,
                    "quarantined": 0
                },
                "docx": {
                    "indexed-complete": 0,
                    "indexed-partial": 0,
                    "skipped-no-extractable-text": 0,
                    "unreadable": 0,
                    "quarantined": 0
                },
                "pdf": {
                    "indexed-complete": 0,
                    "indexed-partial": 0,
                    "skipped-no-extractable-text": 0,
                    "unreadable": 0,
                    "quarantined": 0
                },
                "excalidraw": {
                    "indexed-complete": 0,
                    "indexed-partial": 0,
                    "skipped-no-extractable-text": 0,
                    "unreadable": 0,
                    "quarantined": 0
                },
                "excel": {
                    "indexed-complete": 0,
                    "indexed-partial": 0,
                    "skipped-no-extractable-text": 0,
                    "unreadable": 0,
                    "quarantined": 0
                },
                "html": {
                    "indexed-complete": 0,
                    "indexed-partial": 0,
                    "skipped-no-extractable-text": 0,
                    "unreadable": 0,
                    "quarantined": 1
                }
            })
        );
    }

    #[test]
    fn startup_configuration_changes_require_restart_but_vault_changes_do_not() {
        let baseline = Config::default();
        let mut vault_change = baseline.clone();
        vault_change.vaults.push(VaultRegistration {
            id: "notes".into(),
            path: PathBuf::from("/vaults/notes"),
            room: None,
        });
        assert!(!baseline.requires_restart_for(&vault_change));

        let mut bind_change = baseline.clone();
        bind_change.server.bind = "127.0.0.1:40000".into();
        assert!(baseline.requires_restart_for(&bind_change));

        let mut auth_change = baseline.clone();
        auth_change.auth.token_file = Some(PathBuf::from("other.token"));
        assert!(baseline.requires_restart_for(&auth_change));

        let mut semantic_change = baseline.clone();
        semantic_change.semantic.enabled = true;
        assert!(baseline.requires_restart_for(&semantic_change));

        let mut profile_change = baseline.clone();
        profile_change.server.profile = HostProfile::OpenClast;
        assert!(baseline.requires_restart_for(&profile_change));
    }
}
