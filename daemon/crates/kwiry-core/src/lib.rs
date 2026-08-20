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
mod extract;
#[cfg(feature = "portable")]
mod format;
#[cfg(feature = "portable")]
mod formats;
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
mod policy;
#[cfg(feature = "portable")]
mod query;
#[cfg(feature = "internal-d5c-preview")]
mod ranking;
#[cfg(feature = "internal-d5c-preview")]
mod ranking_eval;
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
#[cfg(feature = "portable")]
pub use extract::{
    ContentRole, ExtractedSection, ExtractedSource, ExtractionCompleteness, ExtractionCoverage,
    ExtractionError, ExtractionNotice, SourceLocator,
};
#[cfg(feature = "portable")]
pub use format::{FormatPolicy, FormatSpec, SourceFormat, format_specs};
#[cfg(feature = "portable")]
pub use formats::extract_source;
#[cfg(feature = "internal-docx-extractor")]
pub use formats::{
    DocxCandidate, DocxProperties, ExtractionScope, SemanticSection, extract_candidate_outcome,
};
// Feature-gated direct SpreadsheetML candidate API. Normal Excel extraction is
// admitted through the portable SourceFormat registry and dispatcher.
#[cfg(feature = "internal-excel-extractor")]
pub use formats::{
    ExcelCandidate, ExcelCellLocator, ExcelSection, extract_excel_candidate_outcome,
};
// Admission-disabled Excalidraw spike; see `formats::excalidraw`. Exposing the
// entry point does not admit the format: there is no `SourceFormat` variant, no
// registry entry, and no discovery or source-preparation route.
#[cfg(feature = "internal-excalidraw-extractor")]
pub use formats::{
    MAX_EXCALIDRAW_NOTICES, MAX_EXCALIDRAW_PROPERTY_BYTES, MAX_EXCALIDRAW_PROPERTY_ENTRIES,
    extract_excalidraw_candidate,
};
// The PDF reader's vocabulary; see `formats::pdf`. PDF itself is admitted and
// needs none of this — `extract_source` reaches the extractor through the
// registry like every other format — but the geometry and tier harnesses name
// these directly, and the reader now compiles in every `portable` build, so the
// export is unconditional rather than feature-gated.
pub use formats::{
    PdfCandidate, PdfDocumentGeometry, PdfLimits, PdfPageGeometry, PdfPageLocator, PdfReadError,
    PdfSection, PdfTextRun, PdfWritingMode, extract_pdf_candidate, pdf_limits, read_pdf_geometry,
};
#[cfg(feature = "native")]
pub use generation::{DataRoot, DataRootLock, DiscardedGeneration, GenerationPaths};
#[cfg(feature = "native")]
pub use index::build_index;
#[cfg(feature = "portable")]
pub use lexical::normalize_lexical_value;
#[cfg(feature = "native")]
pub use manifest::{
    EvictedSource, EvictionReport, INDEX_FORMAT_VERSION, MANIFEST_VERSION, Manifest, ManifestFile,
    ManifestFileOutcome, ManifestOnDisk, registration_fingerprint,
};
#[cfg(feature = "native")]
pub use model::{
    AuthConfig, Config, DEFAULT_BIND, HostProfile, IndexStats, IndexingConfig, IngestReport,
    IngestWarning, LexicalSearchRequest, OpenClastAuthConfig, ResourceKey, SemanticConfig,
    ServerConfig, VaultRegistration,
};
#[cfg(feature = "portable")]
pub use model::{
    CHUNKING_VERSION, Chunk, ExtractionCoverageCounts, Frontmatter, IndexFreshnessBasis,
    MAX_FILE_BYTES, PreparedChunk, PropertyBag, PropertyValue, RetrievalMetadata, SearchHit,
    SourceFormatCounts,
};
#[cfg(feature = "portable")]
pub use policy::{
    EXTRACTION_POLICY_SCHEMA_VERSION, ExtractionProfile, FORMAT_IDENTITY_SCHEMA_VERSION,
    active_extraction_policy, active_format_identities, extraction_policy_fingerprint,
    extraction_profile_for, extractor_version_for, format_identity_fingerprint,
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
#[cfg(feature = "internal-d5c-preview")]
pub use ranking::{
    D5C_PREVIEW_PROFILE_ID, D5cRelevanceProfile, FolderRule, HierarchyRules,
    LEXICAL_RELEVANCE_PROFILE_ID, LexicalEvidenceTier, MAX_ABSOLUTE_METADATA_POINTS,
    MAX_FOLDER_RANKING_RULES_PER_FAMILY, MAX_PROPERTY_RANKING_RULES,
    MAX_PROPERTY_VALUES_PER_SOURCE_OBSERVATION, MAX_RANKING_FOLDER_PREFIX_BYTES,
    MAX_RANKING_JSON_POINTER_BYTES, MAX_RANKING_PATH_DEPTH, MAX_RANKING_PROPERTY_NAME_BYTES,
    MAX_RANKING_RULE_ID_BYTES, MAX_RANKING_VALUE_BYTES, MAX_RANKING_WORK_UNITS,
    MAX_RELEVANCE_PROFILE_BYTES, MAX_RERANK_CANDIDATES, MAX_RERANK_SOURCE_OBSERVATIONS,
    MAX_TOTAL_RANKING_RULES, PathDepthPredicate, PathDepthRule, PropertyPredicate, PropertyRule,
    PropertyScalarObservation, QualifiedSourceId, RELEVANCE_PROFILE_SCHEMA_VERSION,
    RERANK_INPUT_SCHEMA_VERSION, RankingError, RankingScalar, RecencyClock, RecencyHorizon,
    RecencyRule, RelevanceProfile, RerankCandidate, RerankEvidence, RerankEvidenceEntry,
    RerankInput, RerankResult, RuleEffect, RuleStrength, SourceSignalObservation,
    rerank_candidates,
};
#[cfg(feature = "internal-d5c-preview")]
pub use ranking_eval::{
    BALANCED_COMPARISON_ENVELOPE_SCHEMA_VERSION, BALANCED_EVALUATION_SOURCE_FACTS_SCHEMA_VERSION,
    BALANCED_EXPLANATION_SCHEMA_VERSION, BALANCED_PLAYGROUND_CASE_SCHEMA_VERSION,
    BALANCED_PLAYGROUND_CONFIGURATION_SCHEMA_VERSION, BALANCED_PLAYGROUND_SCENARIO_ID,
    BalancedCandidateRuleExplanation, BalancedComparisonEnvelope, BalancedEvaluationDisposition,
    BalancedExplanationLevel, BalancedExplanationProjection, BalancedExplanationSummary,
    BalancedFatalReason, BalancedPlaygroundCase, BalancedPlaygroundConfiguration,
    BalancedPropertyFixturePack, BalancedSafeRuleExplanation, BalancedSafeRuleKind,
    BalancedSafeSignalOutcome, ComparisonRanking, ComparisonRankingEntry, ComparisonRankingLabel,
    DISCREPANCY_DECISION_TABLE, DiscrepancyDecision, EvaluationPropertySignal,
    EvaluationSignalState, EvaluationSourceFacts, MAX_BALANCED_PROPERTY_RULES, balanced_case_hash,
    balanced_configuration_hash, discrepancy_decision, evaluate_balanced_playground,
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
    MAX_PREPARED_CHUNKS_PER_SOURCE, SOURCE_PREPARATION_SCHEMA_VERSION, SourceDescriptor,
    SourceExactMetadata, SourcePreparation, SourcePreparationError, SourcePreparationKind,
    prepare_oversized_source, prepare_source_buffer, source_key,
};
#[cfg(feature = "portable")]
pub use status::{
    DaemonState, DaemonStatus, IndexFreshness, IndexFreshnessState, ModelStatus, VaultStatus,
    owned_format_identities,
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
