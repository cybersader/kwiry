mod api;
mod auth;
mod bootstrap;
mod chunk;
mod config;
mod connection;
mod error;
mod frontmatter;
mod generation;
mod index;
mod lexical;
mod links;
mod manifest;
mod model;
mod partition;
mod runtime;
mod search;
mod semantic;
mod state;
mod status;
mod walk;

pub use api::{
    ApiErrorBody, ApiErrorEnvelope, ApiRequestError, ApiSearchRequest, ApiSearchResponse,
    HealthResponse, SearchFilters, SearchMode,
};
pub use auth::{Principal, Scope, load_or_create_token, load_token, token_matches};
pub use bootstrap::{DesktopBootstrap, bootstrap_desktop};
pub use chunk::ingest_vault;
pub use config::{
    ConfigLock, Paths, VaultRegistrationDisposition, acquire_config_lock, acquire_setup_lock,
    add_vault, ensure_vault_registration, load_config, save_config, update_config,
};
pub use connection::{
    CONNECTION_SCHEMA_VERSION, ConnectionDescriptor, load_connection_descriptor,
    write_connection_descriptor,
};
pub use error::{Error, Result};
pub use generation::{DataRoot, DataRootLock, GenerationPaths};
pub use index::build_index;
pub use manifest::{
    INDEX_FORMAT_VERSION, MANIFEST_VERSION, Manifest, ManifestFile, ManifestFileOutcome,
    registration_fingerprint, source_key,
};
pub use model::{
    AuthConfig, CHUNKING_VERSION, Chunk, Config, DEFAULT_BIND, Frontmatter, HostProfile,
    IndexStats, IngestReport, IngestWarning, OpenClastAuthConfig, ResourceKey, SearchHit,
    SearchRequest, SemanticConfig, ServerConfig, VaultRegistration,
};
pub use runtime::{IndexManager, ReconcileReport, SearchRuntime};
pub use search::search_index;
#[cfg(feature = "semantic-onnx")]
pub use semantic::FastembedEmbedder;
pub use semantic::{
    Embedder, EmbeddingProfile, SemanticHit, SemanticRuntime, SemanticStore, embedding_text,
    rrf_fuse,
};
pub use status::{DaemonState, DaemonStatus, ModelStatus, VaultStatus};

pub fn ingest_config(config: &Config) -> Result<IngestReport> {
    if config.vaults.is_empty() {
        return Err(Error::NoVaults);
    }

    let mut combined = IngestReport::default();
    for vault in &config.vaults {
        let mut report = ingest_vault(vault);
        combined.documents += report.documents;
        combined.chunks.append(&mut report.chunks);
        combined.warnings.append(&mut report.warnings);
    }
    Ok(combined)
}
