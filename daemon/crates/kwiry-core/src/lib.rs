#[cfg(not(feature = "portable"))]
compile_error!("kwiry-core requires either the `portable` or `native` feature");

#[cfg(feature = "portable")]
mod api;
#[cfg(feature = "native")]
mod auth;
#[cfg(feature = "native")]
mod bootstrap;
#[cfg(feature = "native")]
mod chunk;
#[cfg(feature = "native")]
mod config;
#[cfg(feature = "native")]
mod connection;
#[cfg(feature = "native")]
mod error;
#[cfg(feature = "portable")]
mod frontmatter;
#[cfg(feature = "native")]
mod generation;
#[cfg(feature = "native")]
mod index;
#[cfg(feature = "portable")]
mod lexical;
#[cfg(feature = "portable")]
mod links;
#[cfg(feature = "native")]
mod manifest;
#[cfg(feature = "portable")]
mod model;
#[cfg(feature = "native")]
mod partition;
#[cfg(feature = "portable")]
mod query;
#[cfg(feature = "native")]
mod reconcile;
#[cfg(feature = "native")]
mod runtime;
#[cfg(feature = "native")]
mod search;
#[cfg(feature = "native")]
mod semantic;
#[cfg(feature = "portable")]
mod source;
#[cfg(feature = "native")]
mod state;
#[cfg(feature = "portable")]
mod status;
#[cfg(feature = "portable")]
mod typo;
#[cfg(feature = "native")]
mod walk;

#[cfg(feature = "portable")]
pub use api::{
    ApiErrorBody, ApiErrorEnvelope, ApiRequestError, ApiSearchRequest, ApiSearchResponse,
    HealthResponse, SearchFilters, SearchMode,
};
#[cfg(feature = "native")]
pub use auth::{Principal, Scope, load_or_create_token, load_token, token_matches};
#[cfg(feature = "native")]
pub use bootstrap::{DesktopBootstrap, bootstrap_desktop};
#[cfg(feature = "native")]
pub use chunk::ingest_vault;
#[cfg(feature = "native")]
pub use config::{
    ConfigLock, Paths, VaultRegistrationDisposition, acquire_config_lock, acquire_setup_lock,
    add_vault, ensure_vault_registration, load_config, save_config, update_config,
};
#[cfg(feature = "native")]
pub use connection::{
    CONNECTION_SCHEMA_VERSION, ConnectionDescriptor, load_connection_descriptor,
    write_connection_descriptor,
};
#[cfg(feature = "native")]
pub use error::{Error, Result};
#[cfg(feature = "native")]
pub use generation::{DataRoot, DataRootLock, GenerationPaths};
#[cfg(feature = "native")]
pub use index::build_index;
#[cfg(feature = "portable")]
pub use lexical::normalize_lexical_value;
#[cfg(feature = "native")]
pub use manifest::{
    INDEX_FORMAT_VERSION, MANIFEST_VERSION, Manifest, ManifestFile, ManifestFileOutcome,
    registration_fingerprint,
};
#[cfg(feature = "native")]
pub use model::{
    AuthConfig, Config, DEFAULT_BIND, HostProfile, IndexStats, IndexingConfig, IngestReport,
    IngestWarning, LexicalSearchRequest, OpenClastAuthConfig, ResourceKey, SemanticConfig,
    ServerConfig, VaultRegistration,
};
#[cfg(feature = "portable")]
pub use model::{
    CHUNKING_VERSION, Chunk, Frontmatter, IndexFreshnessBasis, MAX_FILE_BYTES, PreparedChunk,
    PropertyBag, PropertyValue, RetrievalMetadata, SearchHit,
};
#[cfg(feature = "portable")]
pub use query::{
    LEXICAL_QUERY_PLAN_SCHEMA_VERSION, LexicalQueryPlan, MAX_CANDIDATES_PER_STAGE,
    MAX_EVIDENCE_STAGES, MAX_PARTIAL_COVERAGE_TERMS, MAX_PREFIX_EXPANSIONS_PER_TERM,
    MAX_PREFIX_TERMS, MAX_QUERY_BYTES, MAX_QUERY_TERMS, MAX_TERM_SUPPORT_PROBES,
    MAX_TOTAL_CANDIDATES, MIN_PREFIX_CHARS, QueryAssistanceEligibility, QueryBounds,
    QueryEvidenceReport, QueryEvidenceStage, QueryEvidenceStageKind, QueryExactIntent,
    QueryExecutionDisposition, QueryField, QueryFieldGroup, QueryFieldGroups, QueryMatchOperator,
    QueryMetadataField, QueryMetadataProbe, QueryPhraseIntent, QueryPlanError, QueryPlanKind,
    QueryTermIntent, QueryTermProjection, QueryTermRole, QueryTermSupport,
    QueryTermSupportObservation, QueryTermSupportProbe, QueryTypoStage, prepare_lexical_query,
};
#[cfg(feature = "native")]
pub use reconcile::ReconcileScope;
#[cfg(feature = "native")]
pub use runtime::{GenerationSearchResult, IndexManager, ReconcileReport, SearchRuntime};
#[cfg(feature = "native")]
pub use search::search_index;
#[cfg(feature = "semantic-onnx")]
pub use semantic::FastembedEmbedder;
#[cfg(feature = "native")]
pub use semantic::{
    Embedder, EmbeddingProfile, SemanticHit, SemanticRuntime, SemanticStore, embedding_text,
    rrf_fuse,
};
#[cfg(feature = "portable")]
pub use source::{
    SOURCE_PREPARATION_SCHEMA_VERSION, SourceDescriptor, SourceExactMetadata, SourceFormat,
    SourcePreparation, SourcePreparationError, SourcePreparationKind, prepare_oversized_source,
    prepare_source_buffer, source_key,
};
#[cfg(feature = "portable")]
pub use status::{
    DaemonState, DaemonStatus, IndexFreshness, IndexFreshnessState, ModelStatus, VaultStatus,
};
#[cfg(feature = "portable")]
pub use typo::{
    TYPO_MAX_CANDIDATE_BYTES, TYPO_MAX_EDIT_DISTANCE, TYPO_MAX_OUTPUT_SUGGESTIONS,
    TYPO_MAX_TERM_BYTES, TYPO_MAX_VOCABULARY_CANDIDATES, TYPO_MAX_WORK_UNITS, TYPO_MIN_TERM_CHARS,
    TYPO_PREFIX_CHARS, TYPO_PREFIX_LIMITATION, TYPO_SUGGESTION_SCHEMA_VERSION,
    TypoSuggestionBounds, TypoSuggestionDisposition, TypoSuggestionPlan, TypoSuggestionResult,
    TypoVocabularyCandidate, finalize_typo_suggestion, prepare_typo_suggestion,
};

#[cfg(feature = "native")]
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
