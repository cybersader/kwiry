// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;

use kwiry_core::{
    CHUNKING_VERSION, LEXICAL_QUERY_PLAN_SCHEMA_VERSION, LexicalQueryPlan, MAX_FILE_BYTES,
    QueryAssistanceEligibility, QueryEvidenceReport, QueryEvidenceStageKind,
    QueryExecutionDisposition, QueryField, QueryFieldGroup, QueryMatchOperator, QueryPlanKind,
    SOURCE_PREPARATION_SCHEMA_VERSION, SourceDescriptor, SourcePreparation, prepare_lexical_query,
    prepare_oversized_source as prepare_oversized_source_descriptor, prepare_source_buffer,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

pub const ADAPTER_ABI_VERSION: u32 = 2;
pub const FTS5_MATCH_PLAN_SCHEMA_VERSION: u32 = 2;
pub const MAX_ADAPTER_REQUEST_BYTES: usize = 64 * 1024;
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
    IdentifierMetadataV2 {
        schema_version: u32,
        match_value: String,
    },
    TermSupportV2 {
        schema_version: u32,
        probe_id: u16,
        term_index: u16,
        match_value: String,
        prefix_pattern: Option<String>,
        max_prefix_expansions: usize,
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
    LexicalExplicitV2,
    LexicalExactMetadataV2,
    LexicalExactPhraseV2,
    LexicalAllTermsV2,
    LexicalPartialCoverageV2,
    LexicalPrefixV2,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Fts5StagePlan {
    pub ordinal: u8,
    pub plan_id: Fts5StagePlanId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact_value: Option<String>,
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

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn abi_identity() -> String {
    serde_json::to_string(&AbiIdentity {
        abi_version: ADAPTER_ABI_VERSION,
        adapter: "kwiry-obsidian-wasm",
        adapter_version: env!("CARGO_PKG_VERSION"),
        source_preparation_schema_version: SOURCE_PREPARATION_SCHEMA_VERSION,
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
    .unwrap_or_else(|_| "{\"abi_version\":2,\"adapter\":\"kwiry-obsidian-wasm\"}".to_owned())
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
        probes.push(Fts5EvidenceProbePlan::IdentifierMetadataV2 {
            schema_version: FTS5_MATCH_PLAN_SCHEMA_VERSION,
            match_value: bounded_match_value(format!("{{{fields}}} : ({terms})"))?,
        });
    }
    for (probe_index, probe) in plan.support_probes.iter().enumerate() {
        let match_value = scoped_required_terms(
            plan,
            probe.field_group,
            std::slice::from_ref(&probe.term_index),
        )?;
        let prefix_pattern = (probe_index < plan.bounds.max_prefix_terms)
            .then(|| prefix_pattern(&probe.term, plan.bounds.min_prefix_chars))
            .flatten();
        probes.push(Fts5EvidenceProbePlan::TermSupportV2 {
            schema_version: FTS5_MATCH_PLAN_SCHEMA_VERSION,
            probe_id: probe.probe_id,
            term_index: probe.term_index,
            match_value,
            prefix_pattern,
            max_prefix_expansions: plan.bounds.max_prefix_expansions_per_term,
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
                "invalid_request",
                "Explicit query prefix observations must be empty.",
            ));
        }
        return Ok(BTreeMap::new());
    }
    if observations.len() != plan.support_probes.len()
        || report.term_support.len() != plan.support_probes.len()
    {
        return Err(adapter_error(
            "invalid_request",
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
        if observation.probe_id != probe.probe_id
            || observation.term_index != probe.term_index
            || support.probe_id != probe.probe_id
            || support.term_index != probe.term_index
            || support.prefix_expansions != observation.terms.len()
            || observation.terms.len() > plan.bounds.max_prefix_expansions_per_term
        {
            return Err(adapter_error(
                "invalid_request",
                "Prefix observation does not match its requested probe.",
            ));
        }
        let expected_prefix = probe.term.to_lowercase();
        let mut previous: Option<&str> = None;
        for term in &observation.terms {
            if term.is_empty()
                || term.len() > MAX_PREFIX_TERM_BYTES
                || !term.starts_with(&expected_prefix)
                || previous.is_some_and(|value| value >= term.as_str())
            {
                return Err(adapter_error(
                    "invalid_request",
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
                    plan_id: Fts5StagePlanId::LexicalExplicitV2,
                    match_value: Some(translate_explicit_query(&plan.query)?),
                    exact_value: None,
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
                let (plan_id, match_value, exact_value) = match stage.kind {
                    QueryEvidenceStageKind::ExactMetadata => (
                        Fts5StagePlanId::LexicalExactMetadataV2,
                        None,
                        Some(exact_stage_value(plan)?),
                    ),
                    QueryEvidenceStageKind::ExactPhrase => (
                        Fts5StagePlanId::LexicalExactPhraseV2,
                        Some(scoped_phrase(plan, stage.field_group)?),
                        None,
                    ),
                    QueryEvidenceStageKind::AllTerms => (
                        Fts5StagePlanId::LexicalAllTermsV2,
                        Some(scoped_required_terms(
                            plan,
                            stage.field_group,
                            &stage.required_term_indexes,
                        )?),
                        None,
                    ),
                    QueryEvidenceStageKind::PartialCoverage => (
                        Fts5StagePlanId::LexicalPartialCoverageV2,
                        Some(scoped_required_terms(
                            plan,
                            stage.field_group,
                            &stage.required_term_indexes,
                        )?),
                        None,
                    ),
                    QueryEvidenceStageKind::Prefix => (
                        Fts5StagePlanId::LexicalPrefixV2,
                        Some(scoped_prefix_terms(plan, stage, prefix_expansions)?),
                        None,
                    ),
                };
                if plan_id == Fts5StagePlanId::LexicalExactMetadataV2
                    && (exact_value.as_deref().is_none_or(str::is_empty) || match_value.is_some())
                {
                    return Err(adapter_error(
                        "invalid_query_plan",
                        "Exact evidence stage has no exact value.",
                    ));
                }
                stages.push(Fts5StagePlan {
                    ordinal: stage.ordinal,
                    plan_id,
                    match_value: match_value.map(bounded_match_value).transpose()?,
                    exact_value,
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

fn scoped_required_terms(
    plan: &LexicalQueryPlan,
    group: QueryFieldGroup,
    indexes: &[u16],
) -> Result<String, AdapterError> {
    let terms = indexes
        .iter()
        .map(|index| {
            plan.term_intents
                .get(*index as usize)
                .ok_or_else(|| adapter_error("invalid_query_plan", "Unknown term index."))
                .map(|intent| intent.text.clone())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let fields = fts5_fields(plan, group)?;
    Ok(format!("{{{fields}}} : ({})", match_terms(&terms, "AND")?))
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
        clauses.push(quote_fts_phrase(&intent.text));
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
    };
    let rendered = fields
        .iter()
        .map(|field| match field {
            QueryField::Filename => Ok("filename"),
            QueryField::Stem => Ok("stem"),
            QueryField::Aliases => Ok("aliases"),
            QueryField::Title => Ok("title"),
            QueryField::Heading => Ok("heading_text"),
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
            let Some(prefix) = value.strip_suffix('*') else {
                return Err(());
            };
            if star_count != 1
                || prefix.is_empty()
                || !prefix
                    .chars()
                    .all(|character| character.is_alphanumeric() || character == '_')
            {
                return Err(());
            }
            Ok(format!("{}*", quote_fts_phrase(prefix)))
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
            "lexical_exact_metadata_v2"
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
            "{filename stem aliases title heading_text content} : \"dungeons and dragons\""
        );
        assert_eq!(
            ordinary["result"]["execution_plan"]["stages"][2]["match_value"],
            "{filename stem aliases title heading_text content} : (\"dungeons\" AND \"and\" AND \"dragons\")"
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
            "lexical_partial_coverage_v2"
        );
    }

    #[test]
    fn probes_are_fixed_and_metadata_promotion_preserves_anchors() {
        let prepared = response(prepare_query(&query_request("iia 2 line")));
        assert_eq!(prepared["result"]["plan"]["match_operator"], "any");
        assert_eq!(
            prepared["result"]["probes"][0]["plan_id"],
            "identifier_metadata_v2"
        );
        assert_eq!(
            prepared["result"]["probes"][1]["plan_id"],
            "term_support_v2"
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
        assert_eq!(stages.last().unwrap()["plan_id"], "lexical_prefix_v2");

        let prepared = response(prepare_query(&query_request(
            "aaa bbb ccc ddd eee fff ggg hhh iii",
        )));
        assert_eq!(prepared["result"]["probes"][7]["max_prefix_term_bytes"], 96);
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
        assert_eq!(rejected["error"]["code"], "invalid_request");
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
            "lexical_explicit_v2"
        );
        assert_eq!(
            phrase["result"]["execution_plan"]["stages"][0]["match_value"],
            "(title : \"IIA guide\" OR content : \"cache\"*)"
        );

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
