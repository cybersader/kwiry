// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;
#[cfg(feature = "internal-d5c-preview")]
use std::collections::BTreeSet;

#[cfg(feature = "internal-d5c-preview")]
use kwiry_core::{
    BalancedComparisonEnvelope, BalancedPlaygroundCase, BalancedPlaygroundConfiguration,
    D5cRelevanceProfile, MAX_PROPERTY_VALUES_PER_SOURCE_OBSERVATION, MAX_RERANK_CANDIDATES,
    PropertyScalarObservation, QualifiedSourceId, RERANK_INPUT_SCHEMA_VERSION, RelevanceProfile,
    RerankCandidate, RerankEvidence, RerankInput, SourceSignalObservation,
    evaluate_balanced_playground, rerank_candidates,
};
use kwiry_core::{
    CHUNKING_VERSION, FORMAT_IDENTITY_SCHEMA_VERSION, LEXICAL_QUERY_PLAN_SCHEMA_VERSION,
    LexicalQueryPlan, MAX_FILE_BYTES, QueryAssistanceEligibility, QueryEvidenceReport,
    QueryEvidenceStageKind, QueryExecutionDisposition, QueryField, QueryFieldGroup,
    QueryMatchOperator, QueryPlanKind, QueryTermProjection, SOURCE_PREPARATION_SCHEMA_VERSION,
    SourceDescriptor, SourcePreparation, active_extraction_policy, active_format_identities,
    extraction_policy_fingerprint, normalize_lexical_value, prepare_lexical_query,
    prepare_oversized_source as prepare_oversized_source_descriptor, prepare_source_buffer,
};
#[cfg(feature = "internal-docx-extractor")]
use kwiry_core::{ExtractionScope, extract_candidate_outcome};
#[cfg(feature = "internal-typo-prototype")]
use kwiry_core::{
    TypoSuggestionPlan, TypoSuggestionResult, TypoVocabularyCandidate, finalize_typo_suggestion,
    prepare_typo_suggestion,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

pub const ADAPTER_ABI_VERSION: u32 = 3;
pub const FTS5_MATCH_PLAN_SCHEMA_VERSION: u32 = 4;
pub const MAX_ADAPTER_REQUEST_BYTES: usize = 64 * 1024;
#[cfg(feature = "internal-d5c-preview")]
pub const MAX_D5C_PREVIEW_REQUEST_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_SOURCE_BUFFER_BYTES: usize = MAX_FILE_BYTES as usize + 1;
const MAX_MATCH_VALUE_BYTES: usize = 16 * 1024;
const MAX_PREFIX_TERM_BYTES: usize = 96;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdapterOperation {
    PrepareSource,
    PrepareOversizedSource,
    PrepareQuery,
    FinalizeQuery,
    #[cfg(feature = "internal-docx-extractor")]
    InternalDocxExtract,
    #[cfg(feature = "internal-d5c-preview")]
    PrepareD5cPreview,
    #[cfg(feature = "internal-d5c-preview")]
    FinalizeD5cPreview,
    #[cfg(feature = "internal-d5c-preview")]
    InternalD5cEvaluate,
    #[cfg(feature = "internal-typo-prototype")]
    PrepareTypoSuggestion,
    #[cfg(feature = "internal-typo-prototype")]
    FinalizeTypoSuggestion,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AdapterError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AbiIdentity {
    pub abi_version: u32,
    pub adapter: &'static str,
    pub adapter_version: &'static str,
    pub source_preparation_schema_version: u32,
    /// The extraction-policy identity this adapter compiles, and the per-format
    /// profiles behind it. The host mirrors both as constants because it has to
    /// decide whether to attempt a cache restore before the adapter is up; this
    /// is what a test compares the mirror against.
    pub extraction_policy_fingerprint: &'static str,
    pub extraction_policy:
        std::collections::BTreeMap<kwiry_core::SourceFormat, kwiry_core::ExtractionProfile>,
    /// The shape of a per-format identity. Core: a new component here means no
    /// row of any format is reusable, so the host mirrors it into its core
    /// policy hash.
    pub format_identity_schema_version: u32,
    /// Every format's compiled identity. Per-row, not core: a cached source row
    /// carries the identity it was built under, and only rows whose format's
    /// identity moved are evicted. The host mirrors the whole map as constants
    /// because it must decide about a restore before the adapter exists, and
    /// `tests/typescript_mirror.rs` compares both the values and the key set.
    pub format_identities: std::collections::BTreeMap<kwiry_core::SourceFormat, &'static str>,
    /// Which formats can be linked to at a matched heading. A client must not
    /// decide this by testing for one format name: note links work for every
    /// admitted file, and only these formats have headings a `#` link reaches.
    pub section_link_formats: std::collections::BTreeMap<kwiry_core::SourceFormat, bool>,
    pub lexical_query_plan_schema_version: u32,
    pub fts5_match_plan_schema_version: u32,
    /// The chunker the adapter will actually apply. Chunk rows carry it too,
    /// but a generation with no chunks still has to be able to name the
    /// chunking contract its cached image was produced under.
    pub chunking_version: u64,
    pub max_request_bytes: usize,
    pub max_source_buffer_bytes: usize,
    pub operations: [AdapterOperation; 4],
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreparedSourceResult {
    pub preparation: SourcePreparation,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreparedQueryResult {
    pub plan: LexicalQueryPlan,
    pub probes: Vec<Fts5EvidenceProbePlan>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "plan_id", rename_all = "snake_case")]
pub enum Fts5EvidenceProbePlan {
    IdentifierMetadataV3 {
        schema_version: u32,
        match_value: String,
    },
    TermSupportV3 {
        schema_version: u32,
        probe_id: u16,
        term_index: u16,
        #[serde(skip_serializing_if = "Option::is_none")]
        match_value: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        exact_identifier: Option<String>,
        prefix_pattern: Option<String>,
        max_prefix_expansions: usize,
        max_prefix_expansion_scan: usize,
        max_prefix_term_bytes: usize,
    },
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Fts5PrefixExpansionObservation {
    pub probe_id: u16,
    pub term_index: u16,
    pub terms: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Fts5ExecutionDisposition {
    ExplicitBypass,
    Ready,
    EmptyNoEvidence,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Fts5StagePlanId {
    LexicalExplicitV3,
    LexicalExactMetadataV3,
    LexicalExactPhraseV3,
    LexicalAllTermsV3,
    LexicalPartialCoverageV3,
    LexicalPrefixMetadataV3,
    LexicalPrefixV3,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Fts5StagePlan {
    pub ordinal: u8,
    pub plan_id: Fts5StagePlanId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact_value: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required_identifiers: Vec<String>,
    pub max_candidates: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Fts5ExecutionPlan {
    pub schema_version: u32,
    pub profile_id: &'static str,
    pub disposition: Fts5ExecutionDisposition,
    pub max_total_candidates: usize,
    pub stages: Vec<Fts5StagePlan>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FinalizedQueryResult {
    pub plan: LexicalQueryPlan,
    pub execution_plan: Fts5ExecutionPlan,
}

#[cfg(feature = "internal-d5c-preview")]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct D5cPreviewSignalPlan {
    pub schema_version: u32,
    pub requires_source_mtime: bool,
    pub property_names: Vec<String>,
    pub max_candidates: usize,
    pub max_candidates_per_stage: usize,
    pub max_property_values_per_source: usize,
}

#[cfg(feature = "internal-d5c-preview")]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreparedD5cPreviewResult {
    pub signal_plan: D5cPreviewSignalPlan,
}

#[cfg(feature = "internal-d5c-preview")]
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct D5cPreviewCandidateObservation {
    pub source: QualifiedSourceId,
    pub chunk_id: String,
    pub path: String,
    pub evidence_tier: kwiry_core::LexicalEvidenceTier,
    pub lexical_score: f32,
    pub lexical_ordinal: usize,
}

#[cfg(feature = "internal-d5c-preview")]
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct D5cPreviewSourceObservation {
    pub source: QualifiedSourceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_mtime_epoch_seconds: Option<String>,
    #[serde(default)]
    pub present_properties: Vec<String>,
    #[serde(default)]
    pub property_values: Vec<PropertyScalarObservation>,
}

#[cfg(feature = "internal-d5c-preview")]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FinalizedD5cPreviewResult {
    pub ordered_candidate_ordinals: Vec<usize>,
    pub evidence: RerankEvidence,
}

#[cfg(feature = "internal-typo-prototype")]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreparedTypoSuggestionResult {
    pub plan: TypoSuggestionPlan,
}

#[cfg(feature = "internal-typo-prototype")]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FinalizedTypoSuggestionResult {
    pub suggestion: TypoSuggestionResult,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum AdapterResponse<T> {
    Ok {
        abi_version: u32,
        operation: AdapterOperation,
        result: T,
    },
    Error {
        abi_version: u32,
        operation: AdapterOperation,
        error: AdapterError,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareSourceRequest {
    abi_version: u32,
    #[serde(rename = "operation")]
    _operation: PrepareSourceOperation,
    descriptor: SourceDescriptor,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PrepareSourceOperation {
    PrepareSource,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareOversizedSourceRequest {
    abi_version: u32,
    #[serde(rename = "operation")]
    _operation: PrepareOversizedSourceOperation,
    descriptor: SourceDescriptor,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PrepareOversizedSourceOperation {
    PrepareOversizedSource,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareQueryRequest {
    abi_version: u32,
    #[serde(rename = "operation")]
    _operation: PrepareQueryOperation,
    query: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PrepareQueryOperation {
    PrepareQuery,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalizeQueryRequest {
    abi_version: u32,
    #[serde(rename = "operation")]
    _operation: FinalizeQueryOperation,
    query: String,
    evidence_report: QueryEvidenceReport,
    prefix_expansions: Vec<Fts5PrefixExpansionObservation>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FinalizeQueryOperation {
    FinalizeQuery,
}

#[cfg(feature = "internal-docx-extractor")]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InternalDocxExtractRequest {
    abi_version: u32,
    #[serde(rename = "operation")]
    _operation: InternalDocxExtractOperation,
    scope: ExtractionScope,
}

#[cfg(feature = "internal-docx-extractor")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InternalDocxExtractOperation {
    InternalDocxExtract,
}

#[cfg(feature = "internal-d5c-preview")]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareD5cPreviewRequest {
    abi_version: u32,
    #[serde(rename = "operation")]
    _operation: PrepareD5cPreviewOperation,
    profile: D5cRelevanceProfile,
}

#[cfg(feature = "internal-d5c-preview")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PrepareD5cPreviewOperation {
    PrepareD5cPreview,
}

#[cfg(feature = "internal-d5c-preview")]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalizeD5cPreviewRequest {
    abi_version: u32,
    #[serde(rename = "operation")]
    _operation: FinalizeD5cPreviewOperation,
    profile: D5cRelevanceProfile,
    query_time_epoch_seconds: String,
    candidates: Vec<D5cPreviewCandidateObservation>,
    source_signals: Vec<D5cPreviewSourceObservation>,
}

#[cfg(feature = "internal-d5c-preview")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FinalizeD5cPreviewOperation {
    FinalizeD5cPreview,
}

#[cfg(feature = "internal-d5c-preview")]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InternalD5cEvaluateRequest {
    abi_version: u32,
    #[serde(rename = "operation")]
    _operation: InternalD5cEvaluateOperation,
    configuration: BalancedPlaygroundConfiguration,
    case: BalancedPlaygroundCase,
}

#[cfg(feature = "internal-d5c-preview")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InternalD5cEvaluateOperation {
    InternalD5cEvaluate,
}

#[cfg(feature = "internal-typo-prototype")]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareTypoSuggestionRequest {
    abi_version: u32,
    #[serde(rename = "operation")]
    _operation: PrepareTypoSuggestionOperation,
    query: String,
}

#[cfg(feature = "internal-typo-prototype")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PrepareTypoSuggestionOperation {
    PrepareTypoSuggestion,
}

#[cfg(feature = "internal-typo-prototype")]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalizeTypoSuggestionRequest {
    abi_version: u32,
    #[serde(rename = "operation")]
    _operation: FinalizeTypoSuggestionOperation,
    query: String,
    candidates: Vec<TypoVocabularyCandidate>,
}

#[cfg(feature = "internal-typo-prototype")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FinalizeTypoSuggestionOperation {
    FinalizeTypoSuggestion,
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn abi_identity() -> String {
    serde_json::to_string(&AbiIdentity {
        abi_version: ADAPTER_ABI_VERSION,
        adapter: "kwiry-obsidian-wasm",
        adapter_version: env!("CARGO_PKG_VERSION"),
        source_preparation_schema_version: SOURCE_PREPARATION_SCHEMA_VERSION,
        extraction_policy_fingerprint: extraction_policy_fingerprint(),
        extraction_policy: active_extraction_policy(),
        format_identity_schema_version: FORMAT_IDENTITY_SCHEMA_VERSION,
        format_identities: active_format_identities(),
        section_link_formats: active_format_identities()
            .into_keys()
            .map(|format| (format, format.supports_section_links()))
            .collect(),
        lexical_query_plan_schema_version: LEXICAL_QUERY_PLAN_SCHEMA_VERSION,
        fts5_match_plan_schema_version: FTS5_MATCH_PLAN_SCHEMA_VERSION,
        chunking_version: CHUNKING_VERSION,
        max_request_bytes: MAX_ADAPTER_REQUEST_BYTES,
        max_source_buffer_bytes: MAX_SOURCE_BUFFER_BYTES,
        operations: [
            AdapterOperation::PrepareSource,
            AdapterOperation::PrepareOversizedSource,
            AdapterOperation::PrepareQuery,
            AdapterOperation::FinalizeQuery,
        ],
    })
    .unwrap_or_else(|_| "{\"abi_version\":3,\"adapter\":\"kwiry-obsidian-wasm\"}".to_owned())
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn prepare_source(request_json: &str, source_bytes: Vec<u8>) -> String {
    let operation = AdapterOperation::PrepareSource;
    let request = match parse_request::<PrepareSourceRequest>(request_json) {
        Ok(request) => request,
        Err(error) => return error_response(operation, error),
    };
    if let Err(error) = check_abi(request.abi_version) {
        return error_response(operation, error);
    }
    if source_bytes.len() > MAX_SOURCE_BUFFER_BYTES {
        return error_response(
            operation,
            adapter_error(
                "source_too_large",
                "Source buffer exceeds the adapter limit.",
            ),
        );
    }

    match prepare_source_buffer(&request.descriptor, &source_bytes) {
        Ok(preparation) => success_response(operation, PreparedSourceResult { preparation }),
        Err(error) => error_response(
            operation,
            AdapterError {
                code: error.code,
                message: error.message,
            },
        ),
    }
}

#[cfg(feature = "internal-docx-extractor")]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn internal_docx_extract(request_json: &str, source_bytes: Vec<u8>) -> String {
    let operation = AdapterOperation::InternalDocxExtract;
    let request = match parse_request::<InternalDocxExtractRequest>(request_json) {
        Ok(request) => request,
        Err(error) => return error_response(operation, error),
    };
    if let Err(error) = check_abi(request.abi_version) {
        return error_response(operation, error);
    }
    if source_bytes.len() > MAX_SOURCE_BUFFER_BYTES {
        return error_response(
            operation,
            adapter_error(
                "source_too_large",
                "Source buffer exceeds the adapter limit.",
            ),
        );
    }
    success_response(
        operation,
        extract_candidate_outcome(&source_bytes, request.scope),
    )
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn prepare_oversized_source(request_json: &str) -> String {
    let operation = AdapterOperation::PrepareOversizedSource;
    let request = match parse_request::<PrepareOversizedSourceRequest>(request_json) {
        Ok(request) => request,
        Err(error) => return error_response(operation, error),
    };
    if let Err(error) = check_abi(request.abi_version) {
        return error_response(operation, error);
    }

    match prepare_oversized_source_descriptor(&request.descriptor) {
        Ok(preparation) => success_response(operation, PreparedSourceResult { preparation }),
        Err(error) => error_response(
            operation,
            AdapterError {
                code: error.code,
                message: error.message,
            },
        ),
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn prepare_query(request_json: &str) -> String {
    let operation = AdapterOperation::PrepareQuery;
    let request = match parse_request::<PrepareQueryRequest>(request_json) {
        Ok(request) => request,
        Err(error) => return error_response(operation, error),
    };
    if let Err(error) = check_abi(request.abi_version) {
        return error_response(operation, error);
    }

    match prepare_lexical_query(&request.query) {
        Ok(plan) => match evidence_probe_plans(&plan) {
            Ok(probes) => success_response(operation, PreparedQueryResult { plan, probes }),
            Err(error) => error_response(operation, error),
        },
        Err(error) => error_response(
            operation,
            AdapterError {
                code: error.code,
                message: error.message,
            },
        ),
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn finalize_query(request_json: &str) -> String {
    let operation = AdapterOperation::FinalizeQuery;
    let request = match parse_request::<FinalizeQueryRequest>(request_json) {
        Ok(request) => request,
        Err(error) => return error_response(operation, error),
    };
    if let Err(error) = check_abi(request.abi_version) {
        return error_response(operation, error);
    }

    let prepared = match prepare_lexical_query(&request.query) {
        Ok(plan) => plan,
        Err(error) => {
            return error_response(
                operation,
                AdapterError {
                    code: error.code,
                    message: error.message,
                },
            );
        }
    };
    let prefix_expansions = match validate_prefix_observations(
        &prepared,
        &request.evidence_report,
        request.prefix_expansions,
    ) {
        Ok(expansions) => expansions,
        Err(error) => return error_response(operation, error),
    };
    let plan = match prepared.finalize_evidence(request.evidence_report) {
        Ok(plan) => plan,
        Err(error) => {
            return error_response(
                operation,
                AdapterError {
                    code: error.code,
                    message: error.message,
                },
            );
        }
    };
    match fts5_execution_plan(&plan, &prefix_expansions) {
        Ok(execution_plan) => success_response(
            operation,
            FinalizedQueryResult {
                plan,
                execution_plan,
            },
        ),
        Err(error) => error_response(operation, error),
    }
}

#[cfg(feature = "internal-d5c-preview")]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn prepare_d5c_preview(request_json: &str) -> String {
    let operation = AdapterOperation::PrepareD5cPreview;
    let request = match parse_d5c_request::<PrepareD5cPreviewRequest>(request_json) {
        Ok(request) => request,
        Err(error) => return error_response(operation, error),
    };
    if let Err(error) = check_abi(request.abi_version) {
        return error_response(operation, error);
    }
    if let Err(error) = request.profile.validate() {
        return error_response(
            operation,
            AdapterError {
                code: error.code,
                message: error.message,
            },
        );
    }
    let property_names = request
        .profile
        .property_rules
        .iter()
        .map(|rule| rule.property.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    success_response(
        operation,
        PreparedD5cPreviewResult {
            signal_plan: D5cPreviewSignalPlan {
                schema_version: RERANK_INPUT_SCHEMA_VERSION,
                requires_source_mtime: request.profile.recency.is_some(),
                property_names,
                max_candidates: MAX_RERANK_CANDIDATES,
                max_candidates_per_stage: kwiry_core::MAX_CANDIDATES_PER_STAGE,
                max_property_values_per_source: MAX_PROPERTY_VALUES_PER_SOURCE_OBSERVATION,
            },
        },
    )
}

#[cfg(feature = "internal-d5c-preview")]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn finalize_d5c_preview(request_json: &str) -> String {
    let operation = AdapterOperation::FinalizeD5cPreview;
    let request = match parse_d5c_request::<FinalizeD5cPreviewRequest>(request_json) {
        Ok(request) => request,
        Err(error) => return error_response(operation, error),
    };
    if let Err(error) = check_abi(request.abi_version) {
        return error_response(operation, error);
    }
    if let Err(error) = request.profile.validate() {
        return error_response(
            operation,
            AdapterError {
                code: error.code,
                message: error.message,
            },
        );
    }
    let query_time_epoch_seconds = match parse_canonical_u64(&request.query_time_epoch_seconds) {
        Some(value) => value,
        None => {
            return error_response(
                operation,
                adapter_error(
                    "invalid_rerank_input",
                    "D5C query time is not a canonical unsigned integer.",
                ),
            );
        }
    };
    if request
        .candidates
        .iter()
        .enumerate()
        .any(|(ordinal, candidate)| candidate.lexical_ordinal != ordinal)
    {
        return error_response(
            operation,
            adapter_error(
                "invalid_rerank_input",
                "D5C candidate lexical ordinals are incomplete.",
            ),
        );
    }

    let candidates = request
        .candidates
        .iter()
        .map(|candidate| RerankCandidate {
            source: candidate.source.clone(),
            chunk_id: candidate.chunk_id.clone(),
            path: candidate.path.clone(),
            evidence_tier: candidate.evidence_tier,
            lexical_score: candidate.lexical_score,
        })
        .collect::<Vec<_>>();
    let candidate_sources = candidates
        .iter()
        .map(|candidate| candidate.source.clone())
        .collect::<BTreeSet<_>>();
    let mut observed_sources = BTreeSet::new();
    let mut source_signals = Vec::with_capacity(request.source_signals.len());
    for signal in request.source_signals {
        if !candidate_sources.contains(&signal.source)
            || !observed_sources.insert(signal.source.clone())
        {
            return error_response(
                operation,
                adapter_error(
                    "invalid_rerank_input",
                    "D5C source observations escaped the candidate source set.",
                ),
            );
        }
        let source_mtime_epoch_seconds = match signal.source_mtime_epoch_seconds {
            Some(value) => match parse_canonical_u64(&value) {
                Some(value) => Some(value),
                None => {
                    return error_response(
                        operation,
                        adapter_error(
                            "invalid_rerank_input",
                            "D5C source mtime is not a canonical unsigned integer.",
                        ),
                    );
                }
            },
            None => None,
        };
        source_signals.push(SourceSignalObservation {
            source: signal.source,
            source_mtime_epoch_seconds,
            matched_property_rule_ids: Vec::new(),
            present_properties: signal.present_properties,
            property_values: signal.property_values,
        });
    }
    if observed_sources != candidate_sources {
        return error_response(
            operation,
            adapter_error(
                "incomplete_rerank_input",
                "D5C source observations are incomplete.",
            ),
        );
    }

    let input = RerankInput {
        schema_version: RERANK_INPUT_SCHEMA_VERSION,
        query_time_epoch_seconds,
        candidates,
        source_signals,
    };
    let result = match rerank_candidates(&RelevanceProfile::D5cPreviewV1(request.profile), &input) {
        Ok(result) => result,
        Err(error) => {
            return error_response(
                operation,
                AdapterError {
                    code: error.code,
                    message: error.message,
                },
            );
        }
    };
    let ordinals = input
        .candidates
        .iter()
        .enumerate()
        .map(|(ordinal, candidate)| {
            (
                (
                    candidate.source.clone(),
                    candidate.chunk_id.clone(),
                    candidate.path.clone(),
                ),
                ordinal,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let ordered_candidate_ordinals = match result
        .candidates()
        .iter()
        .map(|candidate| {
            ordinals
                .get(&(
                    candidate.source.clone(),
                    candidate.chunk_id.clone(),
                    candidate.path.clone(),
                ))
                .copied()
                .ok_or_else(|| {
                    adapter_error(
                        "invalid_rerank_result",
                        "D5C reranker returned an unknown candidate.",
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(ordinals) => ordinals,
        Err(error) => return error_response(operation, error),
    };
    success_response(
        operation,
        FinalizedD5cPreviewResult {
            ordered_candidate_ordinals,
            evidence: result.evidence().clone(),
        },
    )
}

#[cfg(feature = "internal-d5c-preview")]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn internal_d5c_evaluate(request_json: &str) -> String {
    let operation = AdapterOperation::InternalD5cEvaluate;
    let request = match parse_d5c_request::<InternalD5cEvaluateRequest>(request_json) {
        Ok(request) => request,
        Err(error) => return error_response(operation, error),
    };
    if let Err(error) = check_abi(request.abi_version) {
        return error_response(operation, error);
    }
    let envelope: BalancedComparisonEnvelope =
        evaluate_balanced_playground(&request.configuration, &request.case);
    success_response(operation, envelope)
}

#[cfg(feature = "internal-typo-prototype")]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn prepare_typo_suggestion_probe(request_json: &str) -> String {
    let operation = AdapterOperation::PrepareTypoSuggestion;
    let request = match parse_request::<PrepareTypoSuggestionRequest>(request_json) {
        Ok(request) => request,
        Err(error) => return error_response(operation, error),
    };
    if let Err(error) = check_abi(request.abi_version) {
        return error_response(operation, error);
    }
    success_response(
        operation,
        PreparedTypoSuggestionResult {
            plan: prepare_typo_suggestion(&request.query),
        },
    )
}

#[cfg(feature = "internal-typo-prototype")]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn finalize_typo_suggestion_probe(request_json: &str) -> String {
    let operation = AdapterOperation::FinalizeTypoSuggestion;
    let request = match parse_request::<FinalizeTypoSuggestionRequest>(request_json) {
        Ok(request) => request,
        Err(error) => return error_response(operation, error),
    };
    if let Err(error) = check_abi(request.abi_version) {
        return error_response(operation, error);
    }
    match finalize_typo_suggestion(&request.query, request.candidates) {
        Ok(suggestion) => success_response(operation, FinalizedTypoSuggestionResult { suggestion }),
        Err(message) => error_response(operation, adapter_error("invalid_typo_evidence", &message)),
    }
}

fn parse_request<T: DeserializeOwned>(request_json: &str) -> Result<T, AdapterError> {
    if request_json.len() > MAX_ADAPTER_REQUEST_BYTES {
        return Err(adapter_error(
            "invalid_request",
            "Adapter request exceeds the supported size.",
        ));
    }
    serde_json::from_str(request_json)
        .map_err(|_| adapter_error("invalid_request", "Invalid adapter request."))
}

#[cfg(feature = "internal-d5c-preview")]
fn parse_d5c_request<T: DeserializeOwned>(request_json: &str) -> Result<T, AdapterError> {
    if request_json.len() > MAX_D5C_PREVIEW_REQUEST_BYTES {
        return Err(adapter_error(
            "invalid_request",
            "D5C adapter request exceeds the supported size.",
        ));
    }
    serde_json::from_str(request_json)
        .map_err(|_| adapter_error("invalid_request", "Invalid D5C adapter request."))
}

#[cfg(feature = "internal-d5c-preview")]
fn parse_canonical_u64(value: &str) -> Option<u64> {
    let parsed = value.parse::<u64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn check_abi(abi_version: u32) -> Result<(), AdapterError> {
    if abi_version == ADAPTER_ABI_VERSION {
        Ok(())
    } else {
        Err(adapter_error(
            "abi_mismatch",
            "Unsupported Kwiry adapter ABI.",
        ))
    }
}

fn evidence_probe_plans(
    plan: &LexicalQueryPlan,
) -> Result<Vec<Fts5EvidenceProbePlan>, AdapterError> {
    plan.validate().map_err(|error| AdapterError {
        code: error.code,
        message: error.message,
    })?;
    if plan.assistance == QueryAssistanceEligibility::ExplicitSyntaxBypass
        || plan.execution == QueryExecutionDisposition::EmptyNoEvidence
    {
        return Ok(Vec::new());
    }

    let mut probes =
        Vec::with_capacity(plan.support_probes.len() + usize::from(plan.metadata_probe.is_some()));
    if let Some(probe) = &plan.metadata_probe {
        let terms = match_terms(&plan.terms, "AND")?;
        let fields = metadata_probe_fields(&probe.fields)?;
        probes.push(Fts5EvidenceProbePlan::IdentifierMetadataV3 {
            schema_version: FTS5_MATCH_PLAN_SCHEMA_VERSION,
            match_value: bounded_match_value(format!("{{{fields}}} : ({terms})"))?,
        });
    }
    for (probe_index, probe) in plan.support_probes.iter().enumerate() {
        let intent = plan
            .term_intents
            .get(probe.term_index as usize)
            .ok_or_else(|| adapter_error("invalid_query_plan", "Unknown support probe term."))?;
        let exact_identifier = (intent.projection == QueryTermProjection::ExactIdentifier)
            .then(|| intent.text.clone());
        let match_value = (intent.projection == QueryTermProjection::AnalyzedText)
            .then(|| {
                scoped_analyzed_terms(
                    plan,
                    probe.field_group,
                    std::slice::from_ref(&probe.term_index),
                )
            })
            .transpose()?;
        let prefix_pattern = (intent.projection == QueryTermProjection::AnalyzedText
            && probe_index < plan.bounds.max_prefix_terms)
            .then(|| prefix_pattern(&probe.term, plan.bounds.min_prefix_chars))
            .flatten();
        probes.push(Fts5EvidenceProbePlan::TermSupportV3 {
            schema_version: FTS5_MATCH_PLAN_SCHEMA_VERSION,
            probe_id: probe.probe_id,
            term_index: probe.term_index,
            match_value,
            exact_identifier,
            prefix_pattern,
            max_prefix_expansions: plan.bounds.max_prefix_expansions_per_term,
            max_prefix_expansion_scan: plan.bounds.max_prefix_expansion_scan,
            max_prefix_term_bytes: MAX_PREFIX_TERM_BYTES,
        });
    }
    Ok(probes)
}

fn validate_prefix_observations(
    plan: &LexicalQueryPlan,
    report: &QueryEvidenceReport,
    observations: Vec<Fts5PrefixExpansionObservation>,
) -> Result<BTreeMap<u16, Vec<String>>, AdapterError> {
    if plan.assistance == QueryAssistanceEligibility::ExplicitSyntaxBypass {
        if !observations.is_empty() {
            return Err(adapter_error(
                "invalid_query_plan",
                "Explicit query prefix observations must be empty.",
            ));
        }
        return Ok(BTreeMap::new());
    }
    if observations.len() != plan.support_probes.len()
        || report.term_support.len() != plan.support_probes.len()
    {
        return Err(adapter_error(
            "invalid_query_plan",
            "Prefix observations must exactly match requested term probes.",
        ));
    }

    let mut expansions = BTreeMap::new();
    for ((probe, support), observation) in plan
        .support_probes
        .iter()
        .zip(&report.term_support)
        .zip(observations)
    {
        let exact_identifier = plan
            .term_intents
            .get(probe.term_index as usize)
            .is_some_and(|intent| intent.projection == QueryTermProjection::ExactIdentifier);
        if observation.probe_id != probe.probe_id
            || observation.term_index != probe.term_index
            || support.probe_id != probe.probe_id
            || support.term_index != probe.term_index
            || support.prefix_expansions != observation.terms.len()
            || observation.terms.len() > plan.bounds.max_prefix_expansions_per_term
            || (exact_identifier && !observation.terms.is_empty())
        {
            return Err(adapter_error(
                "invalid_query_plan",
                "Prefix observation does not match its requested probe.",
            ));
        }
        let expected_prefix = normalize_lexical_value(&probe.term).ok_or_else(|| {
            adapter_error(
                "invalid_query_plan",
                "Prefix probe term is not normalizable.",
            )
        })?;
        let mut previous: Option<&str> = None;
        for term in &observation.terms {
            if term.is_empty()
                || term.len() > MAX_PREFIX_TERM_BYTES
                || !term.starts_with(&expected_prefix)
                || previous.is_some_and(|value| value >= term.as_str())
            {
                return Err(adapter_error(
                    "invalid_query_plan",
                    "Prefix expansion observation is invalid.",
                ));
            }
            previous = Some(term);
        }
        if !observation.terms.is_empty() {
            expansions.insert(probe.term_index, observation.terms);
        }
    }
    Ok(expansions)
}

fn fts5_execution_plan(
    plan: &LexicalQueryPlan,
    prefix_expansions: &BTreeMap<u16, Vec<String>>,
) -> Result<Fts5ExecutionPlan, AdapterError> {
    plan.validate().map_err(|error| AdapterError {
        code: error.code,
        message: error.message,
    })?;
    let (disposition, stages) = match plan.execution {
        QueryExecutionDisposition::ExplicitBypass => {
            if plan.kind != QueryPlanKind::Explicit
                || plan.match_operator != QueryMatchOperator::Explicit
            {
                return Err(adapter_error(
                    "invalid_query_plan",
                    "Explicit query plan is inconsistent.",
                ));
            }
            (
                Fts5ExecutionDisposition::ExplicitBypass,
                vec![Fts5StagePlan {
                    ordinal: 0,
                    plan_id: Fts5StagePlanId::LexicalExplicitV3,
                    match_value: Some(translate_explicit_query(&plan.query)?),
                    exact_value: None,
                    required_identifiers: Vec::new(),
                    max_candidates: plan.bounds.max_total_candidates,
                }],
            )
        }
        QueryExecutionDisposition::EmptyNoEvidence => {
            (Fts5ExecutionDisposition::EmptyNoEvidence, Vec::new())
        }
        QueryExecutionDisposition::Ready => {
            let mut stages = Vec::with_capacity(plan.evidence_stages.len());
            for stage in &plan.evidence_stages {
                let required_identifiers = exact_identifier_requirements(plan);
                let (plan_id, match_value, exact_value) = match stage.kind {
                    QueryEvidenceStageKind::ExactMetadata => (
                        Fts5StagePlanId::LexicalExactMetadataV3,
                        None,
                        Some(exact_stage_value(plan)?),
                    ),
                    QueryEvidenceStageKind::ExactPhrase => (
                        Fts5StagePlanId::LexicalExactPhraseV3,
                        Some(scoped_phrase(plan, stage.field_group)?),
                        None,
                    ),
                    QueryEvidenceStageKind::AllTerms => (
                        Fts5StagePlanId::LexicalAllTermsV3,
                        scoped_optional_analyzed_terms(
                            plan,
                            stage.field_group,
                            &stage.required_term_indexes,
                        )?,
                        None,
                    ),
                    QueryEvidenceStageKind::PartialCoverage => (
                        Fts5StagePlanId::LexicalPartialCoverageV3,
                        scoped_optional_analyzed_terms(
                            plan,
                            stage.field_group,
                            &stage.required_term_indexes,
                        )?,
                        None,
                    ),
                    // Both halves of the prefix block share one expansion set
                    // and differ only in the fields they may match.
                    QueryEvidenceStageKind::PrefixMetadata => (
                        Fts5StagePlanId::LexicalPrefixMetadataV3,
                        Some(scoped_prefix_terms(plan, stage, prefix_expansions)?),
                        None,
                    ),
                    QueryEvidenceStageKind::Prefix => (
                        Fts5StagePlanId::LexicalPrefixV3,
                        Some(scoped_prefix_terms(plan, stage, prefix_expansions)?),
                        None,
                    ),
                };
                if plan_id == Fts5StagePlanId::LexicalExactMetadataV3
                    && (exact_value.as_deref().is_none_or(str::is_empty) || match_value.is_some())
                {
                    return Err(adapter_error(
                        "invalid_query_plan",
                        "Exact evidence stage has no exact value.",
                    ));
                }
                if plan_id != Fts5StagePlanId::LexicalExactMetadataV3
                    && match_value.is_none()
                    && required_identifiers.is_empty()
                {
                    return Err(adapter_error(
                        "invalid_query_plan",
                        "Evidence stage has no executable constraints.",
                    ));
                }
                stages.push(Fts5StagePlan {
                    ordinal: stage.ordinal,
                    plan_id,
                    match_value: match_value.map(bounded_match_value).transpose()?,
                    exact_value,
                    required_identifiers,
                    max_candidates: stage.max_candidates,
                });
            }
            (Fts5ExecutionDisposition::Ready, stages)
        }
        QueryExecutionDisposition::AwaitingEvidence => {
            return Err(adapter_error(
                "invalid_query_plan",
                "Query evidence must be finalized before execution.",
            ));
        }
    };
    Ok(Fts5ExecutionPlan {
        schema_version: FTS5_MATCH_PLAN_SCHEMA_VERSION,
        profile_id: "lexical-v1",
        disposition,
        max_total_candidates: plan.bounds.max_total_candidates,
        stages,
    })
}

fn exact_stage_value(plan: &LexicalQueryPlan) -> Result<String, AdapterError> {
    let intent = plan
        .exact_intent
        .as_ref()
        .ok_or_else(|| adapter_error("invalid_query_plan", "Exact stage has no exact intent."))?;
    validate_exact_fields(plan, intent.field_group)?;
    Ok(intent.normalized.clone())
}

fn scoped_phrase(plan: &LexicalQueryPlan, group: QueryFieldGroup) -> Result<String, AdapterError> {
    let intent = plan
        .phrase_intent
        .as_ref()
        .ok_or_else(|| adapter_error("invalid_query_plan", "Phrase stage has no phrase intent."))?;
    let fields = fts5_fields(plan, group)?;
    Ok(format!(
        "{{{fields}}} : {}",
        quote_fts_phrase(&intent.terms.join(" "))
    ))
}

fn scoped_analyzed_terms(
    plan: &LexicalQueryPlan,
    group: QueryFieldGroup,
    indexes: &[u16],
) -> Result<String, AdapterError> {
    scoped_optional_analyzed_terms(plan, group, indexes)?.ok_or_else(|| {
        adapter_error(
            "invalid_query_plan",
            "Analyzed term probe has no analyzed term.",
        )
    })
}

fn scoped_optional_analyzed_terms(
    plan: &LexicalQueryPlan,
    group: QueryFieldGroup,
    indexes: &[u16],
) -> Result<Option<String>, AdapterError> {
    let terms = indexes
        .iter()
        .map(|index| {
            plan.term_intents
                .get(*index as usize)
                .ok_or_else(|| adapter_error("invalid_query_plan", "Unknown term index."))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|intent| intent.projection == QueryTermProjection::AnalyzedText)
        .map(|intent| intent.text.clone())
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Ok(None);
    }
    let fields = fts5_fields(plan, group)?;
    Ok(Some(format!(
        "{{{fields}}} : ({})",
        match_terms(&terms, "AND")?
    )))
}

fn exact_identifier_requirements(plan: &LexicalQueryPlan) -> Vec<String> {
    plan.term_intents
        .iter()
        .filter(|intent| intent.projection == QueryTermProjection::ExactIdentifier)
        .map(|intent| intent.text.clone())
        .collect()
}

fn scoped_prefix_terms(
    plan: &LexicalQueryPlan,
    stage: &kwiry_core::QueryEvidenceStage,
    expansions: &BTreeMap<u16, Vec<String>>,
) -> Result<String, AdapterError> {
    let mut clauses = Vec::new();
    for index in &stage.required_term_indexes {
        let intent = plan.term_intents.get(*index as usize).ok_or_else(|| {
            adapter_error("invalid_query_plan", "Unknown required prefix term index.")
        })?;
        if intent.projection == QueryTermProjection::AnalyzedText {
            clauses.push(quote_fts_phrase(&intent.text));
        }
    }
    for index in &stage.prefix_term_indexes {
        let values = expansions.get(index).ok_or_else(|| {
            adapter_error(
                "invalid_query_plan",
                "Prefix stage has no bounded expansions.",
            )
        })?;
        if values.is_empty() || values.len() > plan.bounds.max_prefix_expansions_per_term {
            return Err(adapter_error(
                "invalid_query_plan",
                "Prefix stage expansion set is invalid.",
            ));
        }
        clauses.push(format!(
            "({})",
            values
                .iter()
                .map(|value| quote_fts_phrase(value))
                .collect::<Vec<_>>()
                .join(" OR ")
        ));
    }
    let fields = fts5_fields(plan, stage.field_group)?;
    Ok(format!("{{{fields}}} : ({})", clauses.join(" AND ")))
}

fn validate_exact_fields(
    plan: &LexicalQueryPlan,
    group: QueryFieldGroup,
) -> Result<(), AdapterError> {
    let fields = match group {
        QueryFieldGroup::Exact => &plan.field_groups.exact,
        _ => {
            return Err(adapter_error(
                "invalid_query_plan",
                "Exact intent uses the wrong field group.",
            ));
        }
    };
    if fields.is_empty() {
        return Err(adapter_error(
            "invalid_query_plan",
            "Exact field group is empty.",
        ));
    }
    if fields.contains(&QueryField::Content) {
        return Err(adapter_error(
            "invalid_query_plan",
            "Exact intent cannot search content.",
        ));
    }
    Ok(())
}

fn fts5_fields(plan: &LexicalQueryPlan, group: QueryFieldGroup) -> Result<String, AdapterError> {
    let fields = match group {
        QueryFieldGroup::SearchableText => &plan.field_groups.searchable_text,
        QueryFieldGroup::Metadata => &plan.field_groups.metadata,
        QueryFieldGroup::Exact => &plan.field_groups.exact,
        QueryFieldGroup::Phrase => &plan.field_groups.phrase,
        QueryFieldGroup::Prefix => &plan.field_groups.prefix,
        QueryFieldGroup::PrefixMetadata => &plan.field_groups.prefix_metadata,
    };
    let rendered = fields
        .iter()
        .map(|field| match field {
            QueryField::Filename => Ok("filename"),
            QueryField::Stem => Ok("stem"),
            QueryField::Aliases => Ok("aliases"),
            QueryField::Title => Ok("title"),
            QueryField::Heading => Ok("heading_text"),
            QueryField::Tags => Ok("tags"),
            QueryField::Content => Ok("content"),
            QueryField::ContentIdentifiers => Ok("identifiers"),
        })
        .collect::<Result<Vec<_>, AdapterError>>()?;
    if rendered.is_empty() {
        return Err(adapter_error("invalid_query_plan", "Field group is empty."));
    }
    Ok(rendered.join(" "))
}

fn metadata_probe_fields(
    fields: &[kwiry_core::QueryMetadataField],
) -> Result<String, AdapterError> {
    let rendered = fields
        .iter()
        .map(|field| match field {
            kwiry_core::QueryMetadataField::Filename => "filename",
            kwiry_core::QueryMetadataField::Stem => "stem",
            kwiry_core::QueryMetadataField::Aliases => "aliases",
            kwiry_core::QueryMetadataField::Title => "title",
            kwiry_core::QueryMetadataField::Heading => "heading_text",
            kwiry_core::QueryMetadataField::Tags => "tags",
        })
        .collect::<Vec<_>>();
    if rendered.is_empty() {
        return Err(adapter_error(
            "invalid_query_plan",
            "Metadata probe fields are empty.",
        ));
    }
    Ok(rendered.join(" "))
}

fn prefix_pattern(term: &str, minimum_chars: usize) -> Option<String> {
    if term.chars().count() < minimum_chars
        || !term
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
    {
        return None;
    }
    let escaped = term
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    Some(format!("{escaped}%"))
}

fn match_terms(terms: &[String], operator: &str) -> Result<String, AdapterError> {
    if terms.is_empty() {
        return Err(adapter_error(
            "invalid_query",
            "Query does not contain searchable terms.",
        ));
    }
    Ok(terms
        .iter()
        .map(|term| quote_fts_phrase(term))
        .collect::<Vec<_>>()
        .join(&format!(" {operator} ")))
}

fn bounded_match_value(value: String) -> Result<String, AdapterError> {
    if value.len() > MAX_MATCH_VALUE_BYTES {
        Err(adapter_error(
            "invalid_query",
            "Prepared match value exceeds the supported size.",
        ))
    } else {
        Ok(value)
    }
}

fn quote_fts_phrase(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExplicitToken {
    Word(String),
    Phrase(String),
    And,
    Or,
    Not,
    LeftParen,
    RightParen,
    Colon,
}

fn translate_explicit_query(query: &str) -> Result<String, AdapterError> {
    let tokens = tokenize_explicit(query).map_err(|()| unsupported_explicit())?;
    let mut parser = ExplicitParser { tokens, cursor: 0 };
    let rendered = parser.parse_or().map_err(|()| unsupported_explicit())?;
    if parser.cursor != parser.tokens.len() {
        return Err(unsupported_explicit());
    }
    bounded_match_value(rendered)
}

fn tokenize_explicit(query: &str) -> Result<Vec<ExplicitToken>, ()> {
    let mut tokens = Vec::new();
    let mut characters = query.chars().peekable();
    while let Some(character) = characters.peek().copied() {
        if character.is_whitespace() {
            characters.next();
            continue;
        }
        match character {
            '(' => {
                characters.next();
                tokens.push(ExplicitToken::LeftParen);
            }
            ')' => {
                characters.next();
                tokens.push(ExplicitToken::RightParen);
            }
            ':' => {
                characters.next();
                tokens.push(ExplicitToken::Colon);
            }
            '"' => {
                characters.next();
                let mut phrase = String::new();
                let mut closed = false;
                for next in characters.by_ref() {
                    if next == '"' {
                        closed = true;
                        break;
                    }
                    if next == '\\' || next == '\0' {
                        return Err(());
                    }
                    phrase.push(next);
                }
                if !closed || phrase.trim().is_empty() {
                    return Err(());
                }
                tokens.push(ExplicitToken::Phrase(phrase));
            }
            _ => {
                let mut word = String::new();
                while let Some(next) = characters.peek().copied() {
                    if next.is_whitespace() || matches!(next, '(' | ')' | ':' | '"') {
                        break;
                    }
                    characters.next();
                    word.push(next);
                }
                if word.is_empty()
                    || word.starts_with(['+', '-'])
                    || word.contains(['\\', '\0', '[', ']', '{', '}', '^', '~', '?'])
                {
                    return Err(());
                }
                tokens.push(match word.as_str() {
                    "AND" => ExplicitToken::And,
                    "OR" => ExplicitToken::Or,
                    "NOT" => ExplicitToken::Not,
                    _ => ExplicitToken::Word(word),
                });
            }
        }
    }
    if tokens.is_empty() {
        Err(())
    } else {
        Ok(tokens)
    }
}

struct ExplicitParser {
    tokens: Vec<ExplicitToken>,
    cursor: usize,
}

impl ExplicitParser {
    fn parse_or(&mut self) -> Result<String, ()> {
        let mut left = self.parse_and()?;
        while self.consume(&ExplicitToken::Or) {
            let right = self.parse_and()?;
            left = format!("({left} OR {right})");
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<String, ()> {
        let mut left = self.parse_primary()?;
        loop {
            if self.consume(&ExplicitToken::And) {
                let right = self.parse_primary()?;
                left = format!("({left} AND {right})");
            } else if self.consume(&ExplicitToken::Not) {
                let right = self.parse_primary()?;
                left = format!("({left} NOT {right})");
            } else {
                return Ok(left);
            }
        }
    }

    fn parse_primary(&mut self) -> Result<String, ()> {
        if self.consume(&ExplicitToken::LeftParen) {
            let inner = self.parse_or()?;
            if !self.consume(&ExplicitToken::RightParen) {
                return Err(());
            }
            return Ok(format!("({inner})"));
        }

        let token = self.next().ok_or(())?;
        match token {
            ExplicitToken::Word(field) if self.consume(&ExplicitToken::Colon) => {
                let field = fts5_field(&field).ok_or(())?;
                let value = self.next().ok_or(())?;
                let rendered = render_explicit_value(value)?;
                Ok(format!("{field} : {rendered}"))
            }
            value => render_explicit_value(value),
        }
    }

    fn consume(&mut self, expected: &ExplicitToken) -> bool {
        if self.tokens.get(self.cursor) == Some(expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn next(&mut self) -> Option<ExplicitToken> {
        let token = self.tokens.get(self.cursor)?.clone();
        self.cursor += 1;
        Some(token)
    }
}

fn render_explicit_value(token: ExplicitToken) -> Result<String, ()> {
    match token {
        ExplicitToken::Phrase(value) => Ok(quote_fts_phrase(&value)),
        ExplicitToken::Word(value) => {
            let star_count = value.chars().filter(|character| *character == '*').count();
            if star_count == 0 {
                return Ok(quote_fts_phrase(&value));
            }
            let Some(authored_prefix) = value.strip_suffix('*') else {
                return Err(());
            };
            let Some(prefix) = normalize_lexical_value(authored_prefix) else {
                return Err(());
            };
            if star_count != 1
                || !prefix
                    .chars()
                    .all(|character| character.is_alphanumeric() || character == '_')
            {
                return Err(());
            }
            Ok(format!("{}*", quote_fts_phrase(&prefix)))
        }
        _ => Err(()),
    }
}

fn fts5_field(value: &str) -> Option<&'static str> {
    match value {
        "filename" => Some("filename"),
        "stem" => Some("stem"),
        "aliases" => Some("aliases"),
        "title" => Some("title"),
        "heading_text" => Some("heading_text"),
        "content" => Some("content"),
        "path" => Some("path_text"),
        "tags" => Some("tags"),
        _ => None,
    }
}

fn unsupported_explicit() -> AdapterError {
    adapter_error(
        "explicit_query_unsupported",
        "This explicit query is unavailable in the in-plugin backend.",
    )
}

fn adapter_error(code: &str, message: &str) -> AdapterError {
    AdapterError {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

fn success_response<T: Serialize>(operation: AdapterOperation, result: T) -> String {
    serialize_response(&AdapterResponse::Ok {
        abi_version: ADAPTER_ABI_VERSION,
        operation,
        result,
    })
}

fn error_response(operation: AdapterOperation, error: AdapterError) -> String {
    serialize_response(&AdapterResponse::<()>::Error {
        abi_version: ADAPTER_ABI_VERSION,
        operation,
        error,
    })
}

fn serialize_response<T: Serialize>(response: &AdapterResponse<T>) -> String {
    serde_json::to_string(response).unwrap_or_else(|_| {
        "{\"status\":\"error\",\"abi_version\":2,\"operation\":\"prepare_query\",\"error\":{\"code\":\"serialization_failed\",\"message\":\"Adapter response serialization failed.\"}}".to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn query_request(query: &str) -> String {
        serde_json::json!({
            "abi_version": ADAPTER_ABI_VERSION,
            "operation": "prepare_query",
            "query": query,
        })
        .to_string()
    }

    fn finalize_request(
        query: &str,
        matched: Option<bool>,
        document_frequencies: &[u64],
        prefix_terms: &[Vec<&str>],
    ) -> String {
        let term_support = document_frequencies
            .iter()
            .enumerate()
            .map(|(index, document_frequency)| {
                serde_json::json!({
                    "probe_id": index,
                    "term_index": index,
                    "document_frequency": document_frequency,
                    "prefix_expansions": prefix_terms[index].len(),
                })
            })
            .collect::<Vec<_>>();
        let prefix_expansions = prefix_terms
            .iter()
            .enumerate()
            .map(|(index, terms)| {
                serde_json::json!({
                    "probe_id": index,
                    "term_index": index,
                    "terms": terms,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "abi_version": ADAPTER_ABI_VERSION,
            "operation": "finalize_query",
            "query": query,
            "evidence_report": {
                "schema_version": LEXICAL_QUERY_PLAN_SCHEMA_VERSION,
                "identifier_probe_matched": matched,
                "term_support": term_support,
            },
            "prefix_expansions": prefix_expansions,
        })
        .to_string()
    }

    fn response(input: String) -> Value {
        serde_json::from_str(&input).expect("adapter response should be JSON")
    }

    #[test]
    fn identity_is_versioned_and_bounded() {
        let identity = response(abi_identity());
        assert_eq!(identity["abi_version"], ADAPTER_ABI_VERSION);
        assert_eq!(
            identity["lexical_query_plan_schema_version"],
            LEXICAL_QUERY_PLAN_SCHEMA_VERSION
        );
        assert_eq!(identity["chunking_version"], CHUNKING_VERSION);
        assert_eq!(
            identity["operations"],
            serde_json::json!([
                "prepare_source",
                "prepare_oversized_source",
                "prepare_query",
                "finalize_query"
            ])
        );
    }

    #[cfg(feature = "internal-d5c-preview")]
    #[test]
    fn internal_d5c_preview_validates_profiles_reranks_and_returns_private_evidence() {
        let profile = serde_json::json!({
            "schema_version": 1,
            "profile_id": "d5c-preview-v1",
            "retrieval_profile_id": "lexical-v1",
            "hierarchy": {
                "authority_folders": [],
                "archive_folders": []
            },
            "property_rules": [{
                "id": "priority-rule",
                "property": "priority",
                "predicate": {
                    "kind": "exact",
                    "pointer": "",
                    "value": { "type": "i64", "value": "7" }
                },
                "effect": "boost",
                "strength": "standard"
            }]
        });
        let prepared = response(prepare_d5c_preview(
            &serde_json::json!({
                "abi_version": ADAPTER_ABI_VERSION,
                "operation": "prepare_d5c_preview",
                "profile": profile
            })
            .to_string(),
        ));
        assert_eq!(prepared["status"], "ok");
        assert_eq!(
            prepared["result"]["signal_plan"]["property_names"],
            serde_json::json!(["priority"])
        );
        assert_eq!(prepared["result"]["signal_plan"]["max_candidates"], 512);
        assert_eq!(
            prepared["result"]["signal_plan"]["max_candidates_per_stage"],
            256
        );
        assert_eq!(
            response(abi_identity())["operations"]
                .as_array()
                .unwrap()
                .len(),
            4
        );

        let source_a = serde_json::json!({
            "authorization_scope": "obsidian-active-vault",
            "source_key": "a"
        });
        let source_b = serde_json::json!({
            "authorization_scope": "obsidian-active-vault",
            "source_key": "b"
        });
        let request = serde_json::json!({
            "abi_version": ADAPTER_ABI_VERSION,
            "operation": "finalize_d5c_preview",
            "profile": profile,
            "query_time_epoch_seconds": "2000000000",
            "candidates": [
                {
                    "source": source_a,
                    "chunk_id": "chunk-a",
                    "path": "a.md",
                    "evidence_tier": "all_terms",
                    "lexical_score": 2.0,
                    "lexical_ordinal": 0
                },
                {
                    "source": source_b,
                    "chunk_id": "chunk-b",
                    "path": "b.md",
                    "evidence_tier": "all_terms",
                    "lexical_score": 1.0,
                    "lexical_ordinal": 1
                }
            ],
            "source_signals": [
                {
                    "source": source_a,
                    "present_properties": [],
                    "property_values": []
                },
                {
                    "source": source_b,
                    "present_properties": ["priority"],
                    "property_values": [{
                        "property": "priority",
                        "pointer": "",
                        "value": { "type": "i64", "value": "7" }
                    }]
                }
            ]
        });
        let finalized = response(finalize_d5c_preview(&request.to_string()));
        assert_eq!(finalized["status"], "ok");
        assert_eq!(
            finalized["result"]["ordered_candidate_ordinals"],
            serde_json::json!([1, 0])
        );
        assert_eq!(finalized["result"]["evidence"]["entries"][0]["points"], 2);
        let evidence = finalized["result"]["evidence"].to_string();
        for forbidden in [
            "priority",
            "chunk-a",
            "chunk-b",
            "a.md",
            "b.md",
            "source_key",
        ] {
            assert!(!evidence.contains(forbidden));
        }

        let mut incomplete = request.clone();
        incomplete["source_signals"] = serde_json::json!([]);
        let refused = response(finalize_d5c_preview(&incomplete.to_string()));
        assert_eq!(refused["status"], "error");
        assert_eq!(refused["error"]["code"], "incomplete_rerank_input");

        let mut malformed = profile;
        malformed["profile_id"] = serde_json::json!("unapproved");
        let refused = response(prepare_d5c_preview(
            &serde_json::json!({
                "abi_version": ADAPTER_ABI_VERSION,
                "operation": "prepare_d5c_preview",
                "profile": malformed
            })
            .to_string(),
        ));
        assert_eq!(refused["status"], "error");
        assert_eq!(refused["error"]["code"], "invalid_relevance_profile");
    }

    #[cfg(feature = "internal-d5c-preview")]
    #[test]
    fn internal_balanced_evaluation_returns_the_rust_envelope_and_fails_closed() {
        let corpus: Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/retrieval/d5c-balanced/corpus.json"
        ))
        .unwrap();
        let evaluations = corpus["evaluations"].as_array().unwrap();
        let strict = evaluations
            .iter()
            .find(|evaluation| evaluation["id"] == "stronger-text-counterexample-native")
            .unwrap();
        let strict = response(internal_d5c_evaluate(&strict["request"].to_string()));
        assert_eq!(strict["status"], "ok");
        assert_eq!(strict["operation"], "internal_d5c_evaluate");
        assert_eq!(strict["result"]["disposition"]["kind"], "strict_balanced");
        assert_eq!(strict["result"]["text_results"]["label"], "text");
        assert_eq!(
            strict["result"]["balanced_results"]["ordered_candidate_ordinals"],
            serde_json::json!([0, 1])
        );

        let fatal = evaluations
            .iter()
            .find(|evaluation| evaluation["id"] == "discrepancy-malformed")
            .unwrap();
        let fatal = response(internal_d5c_evaluate(&fatal["request"].to_string()));
        assert_eq!(fatal["status"], "ok");
        assert_eq!(fatal["result"]["disposition"]["kind"], "fatal");
        assert!(fatal["result"].get("balanced_results").is_none());
        assert!(fatal["result"].get("explanation").is_none());

        let unknown = response(internal_d5c_evaluate(
            r#"{"abi_version":2,"operation":"internal_d5c_evaluate","configuration":{},"case":{},"extra":true}"#,
        ));
        assert_eq!(unknown["status"], "error");
        assert_eq!(unknown["error"]["code"], "invalid_request");
        assert_eq!(
            response(abi_identity())["operations"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }

    #[cfg(feature = "internal-typo-prototype")]
    #[test]
    fn internal_typo_prototype_keeps_policy_in_rust_and_out_of_public_identity() {
        let prepared = response(prepare_typo_suggestion_probe(
            &serde_json::json!({
                "abi_version": ADAPTER_ABI_VERSION,
                "operation": "prepare_typo_suggestion",
                "query": "retrievel",
            })
            .to_string(),
        ));
        assert_eq!(prepared["status"], "ok");
        assert_eq!(prepared["result"]["plan"]["disposition"], "probe");
        assert_eq!(prepared["result"]["plan"]["prefix_pattern"], "retr%");
        assert_eq!(
            prepared["result"]["plan"]["bounds"]["max_vocabulary_candidates"],
            40
        );

        let finalized = response(finalize_typo_suggestion_probe(
            &serde_json::json!({
                "abi_version": ADAPTER_ABI_VERSION,
                "operation": "finalize_typo_suggestion",
                "query": "retrievel",
                "candidates": [
                    { "term": "retrieved", "document_frequency": 2 },
                    { "term": "retrieval", "document_frequency": 7 }
                ],
            })
            .to_string(),
        ));
        assert_eq!(finalized["status"], "ok");
        assert_eq!(
            finalized["result"]["suggestion"]["suggested_query"],
            "retrieval"
        );
        assert_eq!(
            finalized["result"]["suggestion"]["disposition"],
            "suggestion"
        );

        let explicit = response(prepare_typo_suggestion_probe(
            &serde_json::json!({
                "abi_version": ADAPTER_ABI_VERSION,
                "operation": "prepare_typo_suggestion",
                "query": "title:retrievel",
            })
            .to_string(),
        ));
        assert_eq!(
            explicit["result"]["plan"]["disposition"],
            "explicit_syntax_bypass"
        );
    }

    #[test]
    fn oversized_preparation_records_metadata_without_a_buffer() {
        let request = serde_json::json!({
            "abi_version": ADAPTER_ABI_VERSION,
            "operation": "prepare_oversized_source",
            "descriptor": {
                "vault_id": "active-vault",
                "path": "large.md",
                "format": "markdown",
                "byte_length": MAX_FILE_BYTES + 1,
                "mtime": 1,
                "mtime_nanos": "1000000"
            }
        })
        .to_string();
        let prepared = response(prepare_oversized_source(&request));
        assert_eq!(prepared["status"], "ok");
        assert_eq!(prepared["result"]["preparation"]["kind"], "skipped");
        assert_eq!(
            prepared["result"]["preparation"]["content_hash"],
            Value::Null
        );
        assert_eq!(
            prepared["result"]["preparation"]["byte_length"],
            MAX_FILE_BYTES + 1
        );
    }

    #[test]
    fn ordinary_and_identifier_queries_produce_ordered_execution_plans() {
        let ordinary = response(finalize_query(&finalize_request(
            "dungeons and dragons",
            None,
            &[1, 1, 1],
            &[vec![], vec![], vec![]],
        )));
        assert_eq!(ordinary["result"]["execution_plan"]["disposition"], "ready");
        assert_eq!(
            ordinary["result"]["execution_plan"]["stages"][0]["plan_id"],
            "lexical_exact_metadata_v3"
        );
        assert_eq!(
            ordinary["result"]["execution_plan"]["stages"][0]["exact_value"],
            "dungeons and dragons"
        );
        assert!(
            ordinary["result"]["execution_plan"]["stages"][0]
                .get("match_value")
                .is_none()
        );
        assert_eq!(
            ordinary["result"]["execution_plan"]["stages"][1]["match_value"],
            "{filename stem aliases title heading_text tags content} : \"dungeons and dragons\""
        );
        assert_eq!(
            ordinary["result"]["execution_plan"]["stages"][2]["match_value"],
            "{filename stem aliases title heading_text tags content} : (\"dungeons\" AND \"and\" AND \"dragons\")"
        );

        let mixed = response(finalize_query(&finalize_request(
            "orchard adop",
            None,
            &[1, 0],
            &[vec![], vec!["adoption"]],
        )));
        assert_eq!(
            mixed["result"]["plan"]["evidence_stages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|stage| stage["kind"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "exact_metadata",
                "exact_phrase",
                "all_terms",
                "prefix_metadata",
                "prefix",
                "partial_coverage",
            ]
        );
        assert_eq!(
            mixed["result"]["execution_plan"]["stages"][3]["plan_id"],
            "lexical_prefix_metadata_v3"
        );
        // The metadata half is scoped to the fields a person names a note by;
        // the text half carries the identical expansion set over everything.
        assert_eq!(
            mixed["result"]["execution_plan"]["stages"][3]["match_value"],
            "{filename stem aliases title} : (\"orchard\" AND (\"adoption\"))"
        );
        assert_eq!(
            mixed["result"]["execution_plan"]["stages"][4]["plan_id"],
            "lexical_prefix_v3"
        );
        assert_eq!(
            mixed["result"]["execution_plan"]["stages"][4]["match_value"],
            "{filename stem aliases title heading_text tags content} : (\"orchard\" AND (\"adoption\"))"
        );
        assert_eq!(
            mixed["result"]["execution_plan"]["stages"][5]["plan_id"],
            "lexical_partial_coverage_v3"
        );

        let identifier = response(finalize_query(&finalize_request(
            "IIA 2 line",
            None,
            &[1, 1, 0],
            &[vec![], vec![], vec![]],
        )));
        assert_eq!(identifier["result"]["plan"]["kind"], "identifier");
        assert_eq!(
            identifier["result"]["execution_plan"]["stages"][3]["plan_id"],
            "lexical_partial_coverage_v3"
        );
    }

    #[test]
    fn probes_are_fixed_and_metadata_promotion_preserves_anchors() {
        let prepared = response(prepare_query(&query_request("iia 2 line")));
        assert_eq!(prepared["result"]["plan"]["match_operator"], "any");
        assert_eq!(
            prepared["result"]["probes"][0]["plan_id"],
            "identifier_metadata_v3"
        );
        assert_eq!(
            prepared["result"]["probes"][1]["plan_id"],
            "term_support_v3"
        );

        let finalized = response(finalize_query(&finalize_request(
            "iia 2 line",
            Some(true),
            &[1, 1, 0],
            &[vec![], vec![], vec![]],
        )));
        assert_eq!(finalized["result"]["plan"]["kind"], "identifier");
        assert_eq!(
            finalized["result"]["plan"]["term_intents"][0]["role"],
            "required_identifier_anchor"
        );
    }

    #[test]
    fn no_evidence_is_typed_empty_and_prefix_is_bounded_last() {
        let empty = response(finalize_query(&finalize_request(
            "quasar",
            None,
            &[0],
            &[vec![]],
        )));
        assert_eq!(
            empty["result"]["execution_plan"]["disposition"],
            "empty_no_evidence"
        );
        assert_eq!(
            empty["result"]["execution_plan"]["stages"],
            serde_json::json!([])
        );

        let prefix = response(finalize_query(&finalize_request(
            "quasar",
            None,
            &[0],
            &[vec!["quasarish", "quasars"]],
        )));
        let stages = prefix["result"]["execution_plan"]["stages"]
            .as_array()
            .expect("stages");
        assert_eq!(stages.last().unwrap()["plan_id"], "lexical_prefix_v3");
        assert_eq!(
            stages[stages.len() - 2]["plan_id"],
            "lexical_prefix_metadata_v3"
        );

        let prepared = response(prepare_query(&query_request(
            "aaa bbb ccc ddd eee fff ggg hhh iii",
        )));
        assert_eq!(prepared["result"]["probes"][7]["max_prefix_term_bytes"], 96);
        assert_eq!(prepared["result"]["probes"][7]["max_prefix_expansions"], 16);
        assert_eq!(
            prepared["result"]["probes"][7]["max_prefix_expansion_scan"],
            256
        );
        assert!(prepared["result"]["probes"][7]["prefix_pattern"].is_string());
        assert!(prepared["result"]["probes"][8]["prefix_pattern"].is_null());

        let oversized = format!("quasar{}", "x".repeat(MAX_PREFIX_TERM_BYTES));
        let rejected = response(finalize_query(&finalize_request(
            "quasar",
            None,
            &[0],
            &[vec![oversized.as_str()]],
        )));
        assert_eq!(rejected["status"], "error");
        assert_eq!(rejected["error"]["code"], "invalid_query_plan");
    }

    #[test]
    fn explicit_translation_is_allowlisted_and_sql_looking_input_is_rejected() {
        let phrase = response(finalize_query(&finalize_request(
            "title:\"IIA guide\" OR content:cache*",
            None,
            &[],
            &[],
        )));
        assert_eq!(
            phrase["result"]["execution_plan"]["stages"][0]["plan_id"],
            "lexical_explicit_v3"
        );
        assert_eq!(
            phrase["result"]["execution_plan"]["stages"][0]["match_value"],
            "(title : \"IIA guide\" OR content : \"cache\"*)"
        );

        let spaced = response(finalize_query(&finalize_request(
            "title : notes",
            None,
            &[],
            &[],
        )));
        assert_eq!(
            spaced["result"]["execution_plan"]["stages"][0]["match_value"],
            "title : \"notes\""
        );

        let unknown = response(finalize_query(&finalize_request(
            "bogus:notes",
            None,
            &[],
            &[],
        )));
        assert_eq!(unknown["status"], "error");
        assert_eq!(unknown["error"]["code"], "explicit_query_unsupported");

        let sql = response(finalize_query(&finalize_request(
            "title:notes OR '); DROP TABLE chunks; --",
            None,
            &[],
            &[],
        )));
        assert_eq!(sql["status"], "error");
        assert_eq!(sql["error"]["code"], "explicit_query_unsupported");
        assert!(!sql.to_string().contains("DROP TABLE"));
    }

    #[test]
    fn query_boundaries_are_normalized_and_classified_in_rust() {
        for query in ["cache governance?", "cache governance (draft)"] {
            let prepared = response(prepare_query(&query_request(query)));
            assert_eq!(prepared["status"], "ok", "{query}");
            assert_eq!(prepared["result"]["plan"]["kind"], "ordinary", "{query}");
            assert_eq!(
                prepared["result"]["plan"]["assistance"], "eligible",
                "{query}"
            );
        }

        let numeric_field = response(prepare_query(&query_request("title:2026")));
        assert_eq!(numeric_field["result"]["plan"]["kind"], "explicit");
        assert_eq!(
            numeric_field["result"]["plan"]["assistance"],
            "explicit_syntax_bypass"
        );

        for query in ["Résumé Cache", "Resume Cache", "Re\u{301}sume\u{301} Cache"] {
            let prepared = response(prepare_query(&query_request(query)));
            assert_eq!(
                prepared["result"]["plan"]["normalized_exact"], "resume cache",
                "{query}"
            );
            assert_eq!(
                prepared["result"]["plan"]["terms"],
                serde_json::json!(["resume", "cache"]),
                "{query}"
            );
        }

        let complete = "a".repeat(kwiry_core::MAX_QUERY_BYTES);
        let prepared = response(prepare_query(&query_request(&complete)));
        assert_eq!(
            prepared["result"]["plan"]["normalized_exact"]
                .as_str()
                .unwrap()
                .len(),
            kwiry_core::MAX_QUERY_BYTES
        );
        let rejected = response(prepare_query(&query_request(&(complete + "a"))));
        assert_eq!(rejected["error"]["code"], "invalid_query");
    }

    #[test]
    fn exact_identifier_constraints_are_explicit_portable_plan_inputs() {
        for (query, identifier) in [
            ("RFC 9110", "rfc 9110"),
            ("CVE-2026-1234", "cve-2026-1234"),
            ("CVE 2026 1234", "cve 2026 1234"),
            ("product/v2.4.1", "product/v2.4.1"),
        ] {
            let prepared = response(prepare_query(&query_request(query)));
            let probe = &prepared["result"]["probes"][0];
            assert_eq!(probe["plan_id"], "term_support_v3", "{query}");
            assert_eq!(probe["exact_identifier"], identifier, "{query}");
            assert!(probe.get("match_value").is_none(), "{query}");
            assert!(probe["prefix_pattern"].is_null(), "{query}");

            let finalized = response(finalize_query(&finalize_request(
                query,
                None,
                &[1],
                &[vec![]],
            )));
            assert_eq!(finalized["status"], "ok", "{query}");
            for stage in finalized["result"]["execution_plan"]["stages"]
                .as_array()
                .unwrap()
            {
                assert_eq!(
                    stage["required_identifiers"],
                    serde_json::json!([identifier]),
                    "{query}"
                );
            }
        }
    }

    #[test]
    fn trailing_prefix_translation_is_accent_insensitive_and_bounded() {
        for query in ["résu*", "re\u{301}su*", "resu*"] {
            let finalized = response(finalize_query(&finalize_request(query, None, &[], &[])));
            assert_eq!(finalized["status"], "ok", "{query}");
            assert_eq!(
                finalized["result"]["execution_plan"]["stages"][0]["match_value"], "\"resu\"*",
                "{query}"
            );
        }
        for query in ["re*su", "*resu", "re su*"] {
            let finalized = response(finalize_query(&finalize_request(query, None, &[], &[])));
            assert_eq!(finalized["status"], "error", "{query}");
        }
    }

    #[test]
    fn unknown_fields_and_wrong_abi_fail_closed() {
        let unknown = response(prepare_query(
            r#"{"abi_version":2,"operation":"prepare_query","query":"a","extra":true}"#,
        ));
        assert_eq!(unknown["error"]["code"], "invalid_request");

        let abi = response(prepare_query(
            r#"{"abi_version":1,"operation":"prepare_query","query":"a"}"#,
        ));
        assert_eq!(abi["error"]["code"], "abi_mismatch");
    }

    #[test]
    fn source_preparation_uses_descriptor_and_raw_bytes() {
        let source = b"# Heading\nportable adapter".to_vec();
        let request = serde_json::json!({
            "abi_version": ADAPTER_ABI_VERSION,
            "operation": "prepare_source",
            "descriptor": {
                "vault_id": "active",
                "path": "note.md",
                "format": "markdown",
                "byte_length": source.len(),
                "mtime": 1,
                "mtime_nanos": "1000000001"
            }
        })
        .to_string();
        let prepared = response(prepare_source(&request, source));
        assert_eq!(prepared["status"], "ok");
        assert_eq!(
            prepared["result"]["preparation"]["chunks"][0]["chunk"]["path"],
            "note.md"
        );
    }
}
