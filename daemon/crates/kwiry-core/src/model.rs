#[cfg(feature = "native")]
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const CHUNKING_VERSION: u64 = 1;
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Frontmatter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
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
    pub technical_identifiers: Vec<String>,
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
    pub score: f32,
    pub excerpt: String,
    pub frontmatter: Frontmatter,
}

#[cfg(feature = "native")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexStats {
    pub documents: usize,
    pub chunks: usize,
    pub warnings: Vec<IngestWarning>,
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
