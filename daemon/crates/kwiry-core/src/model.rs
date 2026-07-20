use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const CHUNKING_VERSION: u64 = 1;
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_CHUNK_CHARS: usize = 4_000;
pub const CHUNK_OVERLAP_CHARS: usize = 400;
pub const DEFAULT_BIND: &str = "127.0.0.1:32189";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default = "default_config_version")]
    pub version: u32,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub vaults: Vec<VaultRegistration>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: default_config_version(),
            server: ServerConfig::default(),
            auth: AuthConfig::default(),
            vaults: Vec::new(),
        }
    }
}

const fn default_config_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
        }
    }
}

fn default_bind() -> String {
    DEFAULT_BIND.to_owned()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_file: Option<PathBuf>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestWarning {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestReport {
    pub documents: usize,
    pub chunks: Vec<Chunk>,
    pub warnings: Vec<IngestWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchRequest {
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexStats {
    pub documents: usize,
    pub chunks: usize,
    pub warnings: Vec<IngestWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveredFile {
    pub absolute_path: PathBuf,
    pub relative_path: String,
    pub extension: String,
    pub byte_length: u64,
    pub mtime: u64,
    pub mtime_nanos: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileOutcomeKind {
    Indexed,
    Skipped,
    TransientError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileIngestOutcome {
    pub vault_id: String,
    pub path: String,
    pub content_hash: Option<String>,
    pub byte_length: u64,
    pub mtime: u64,
    pub mtime_nanos: u128,
    pub chunks: Vec<Chunk>,
    pub kind: FileOutcomeKind,
    pub warning: Option<IngestWarning>,
}
