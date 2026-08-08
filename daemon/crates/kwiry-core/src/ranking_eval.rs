use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ranking::{
    D5C_PREVIEW_PROFILE_ID, D5cRelevanceProfile, FolderRule, HierarchyRules,
    LEXICAL_RELEVANCE_PROFILE_ID, LexicalEvidenceTier, MAX_FOLDER_RANKING_RULES_PER_FAMILY,
    MAX_PROPERTY_RANKING_RULES, MAX_RANKING_RULE_ID_BYTES, MAX_RELEVANCE_PROFILE_BYTES,
    MAX_RERANK_CANDIDATES, MAX_RERANK_SOURCE_OBSERVATIONS, PropertyRule, QualifiedSourceId,
    RERANK_INPUT_SCHEMA_VERSION, RecencyClock, RecencyHorizon, RecencyRule, RelevanceProfile,
    RerankCandidate, RerankInput, RuleEffect, RuleStrength, SourceSignalObservation,
    rerank_candidates,
};

pub const BALANCED_PLAYGROUND_SCENARIO_ID: &str = "balanced-playground-v1";
pub const BALANCED_PLAYGROUND_CONFIGURATION_SCHEMA_VERSION: u32 = 1;
pub const BALANCED_PLAYGROUND_CASE_SCHEMA_VERSION: u32 = 1;
pub const BALANCED_EVALUATION_SOURCE_FACTS_SCHEMA_VERSION: u32 = 1;
pub const BALANCED_COMPARISON_ENVELOPE_SCHEMA_VERSION: u32 = 1;
pub const BALANCED_EXPLANATION_SCHEMA_VERSION: u32 = 1;
pub const MAX_BALANCED_PROPERTY_RULES: usize = 2;

const BALANCED_RECENCY_RULE_ID: &str = "00-balanced-recency";
const CONFIGURATION_HASH_DOMAIN: &[u8] = b"kwiry:balanced-playground-v1:configuration\0";
const CASE_HASH_DOMAIN: &[u8] = b"kwiry:balanced-playground-v1:case\0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BalancedPlaygroundConfiguration {
    pub schema_version: u32,
    pub scenario_id: String,
    pub authority_folders: Vec<FolderRule>,
    pub archive_folders: Vec<FolderRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property_fixture_pack: Option<BalancedPropertyFixturePack>,
}

impl BalancedPlaygroundConfiguration {
    pub fn candidate(authority_folders: Vec<FolderRule>, archive_folders: Vec<FolderRule>) -> Self {
        Self {
            schema_version: BALANCED_PLAYGROUND_CONFIGURATION_SCHEMA_VERSION,
            scenario_id: BALANCED_PLAYGROUND_SCENARIO_ID.to_owned(),
            authority_folders,
            archive_folders,
            property_fixture_pack: None,
        }
    }

    pub fn configuration_hash(&self) -> String {
        canonical_hash(CONFIGURATION_HASH_DOMAIN, self)
    }

    fn profile(&self) -> Result<D5cRelevanceProfile, BalancedFatalReason> {
        validate_configuration_shape(self)?;
        let profile = D5cRelevanceProfile {
            schema_version: crate::ranking::RELEVANCE_PROFILE_SCHEMA_VERSION,
            profile_id: D5C_PREVIEW_PROFILE_ID.to_owned(),
            retrieval_profile_id: LEXICAL_RELEVANCE_PROFILE_ID.to_owned(),
            recency: Some(RecencyRule {
                id: BALANCED_RECENCY_RULE_ID.to_owned(),
                clock: RecencyClock::SourceMtime,
                horizon: RecencyHorizon::Quarter,
                strength: RuleStrength::Low,
            }),
            hierarchy: HierarchyRules {
                depth: None,
                authority_folders: self.authority_folders.clone(),
                archive_folders: self.archive_folders.clone(),
            },
            property_rules: self
                .property_fixture_pack
                .as_ref()
                .map_or_else(Vec::new, |pack| pack.rules.clone()),
        };
        profile.validate().map_err(classify_ranking_error)?;
        Ok(profile)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BalancedPropertyFixturePack {
    pub fixture_pack_id: String,
    pub rules: Vec<PropertyRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BalancedPlaygroundCase {
    pub schema_version: u32,
    pub scenario_id: String,
    pub query_time_epoch_seconds: u64,
    pub candidates: Vec<RerankCandidate>,
    pub source_facts: Vec<EvaluationSourceFacts>,
    pub explanation_level: BalancedExplanationLevel,
}

impl BalancedPlaygroundCase {
    pub fn case_hash(&self) -> String {
        canonical_hash(CASE_HASH_DOMAIN, self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvaluationSourceFacts {
    pub schema_version: u32,
    pub source: QualifiedSourceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_mtime_epoch_seconds: Option<u64>,
    pub recency: EvaluationSignalState,
    pub authority: EvaluationSignalState,
    pub archive: EvaluationSignalState,
    #[serde(default)]
    pub property_rules: Vec<EvaluationPropertySignal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvaluationPropertySignal {
    pub rule_id: String,
    pub state: EvaluationSignalState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationSignalState {
    Matched,
    Nonmatched,
    Absent,
    Unsupported,
    Stale,
    Unavailable,
    Untrusted,
    Malformed,
    Conflicting,
    Missing,
    Unauthorized,
    OverLimit,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DiscrepancyDecision {
    StrictBalanced,
    NeutralizedCounterfactual,
    Fatal,
}

pub const DISCREPANCY_DECISION_TABLE: [(EvaluationSignalState, DiscrepancyDecision); 12] = [
    (
        EvaluationSignalState::Matched,
        DiscrepancyDecision::StrictBalanced,
    ),
    (
        EvaluationSignalState::Nonmatched,
        DiscrepancyDecision::StrictBalanced,
    ),
    (
        EvaluationSignalState::Absent,
        DiscrepancyDecision::StrictBalanced,
    ),
    (
        EvaluationSignalState::Unsupported,
        DiscrepancyDecision::NeutralizedCounterfactual,
    ),
    (
        EvaluationSignalState::Stale,
        DiscrepancyDecision::NeutralizedCounterfactual,
    ),
    (
        EvaluationSignalState::Unavailable,
        DiscrepancyDecision::NeutralizedCounterfactual,
    ),
    (
        EvaluationSignalState::Untrusted,
        DiscrepancyDecision::NeutralizedCounterfactual,
    ),
    (EvaluationSignalState::Malformed, DiscrepancyDecision::Fatal),
    (
        EvaluationSignalState::Conflicting,
        DiscrepancyDecision::Fatal,
    ),
    (EvaluationSignalState::Missing, DiscrepancyDecision::Fatal),
    (
        EvaluationSignalState::Unauthorized,
        DiscrepancyDecision::Fatal,
    ),
    (EvaluationSignalState::OverLimit, DiscrepancyDecision::Fatal),
];

pub fn discrepancy_decision(state: EvaluationSignalState) -> DiscrepancyDecision {
    match state {
        EvaluationSignalState::Matched
        | EvaluationSignalState::Nonmatched
        | EvaluationSignalState::Absent => DiscrepancyDecision::StrictBalanced,
        EvaluationSignalState::Unsupported
        | EvaluationSignalState::Stale
        | EvaluationSignalState::Unavailable
        | EvaluationSignalState::Untrusted => DiscrepancyDecision::NeutralizedCounterfactual,
        EvaluationSignalState::Malformed
        | EvaluationSignalState::Conflicting
        | EvaluationSignalState::Missing
        | EvaluationSignalState::Unauthorized
        | EvaluationSignalState::OverLimit => DiscrepancyDecision::Fatal,
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BalancedExplanationLevel {
    Off,
    Summary,
    Rules,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BalancedEvaluationDisposition {
    StrictBalanced,
    NeutralizedCounterfactual {
        neutralized_states: Vec<EvaluationSignalState>,
    },
    Fatal {
        reasons: Vec<BalancedFatalReason>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum BalancedFatalReason {
    Malformed,
    Conflicting,
    Missing,
    Unauthorized,
    OverLimit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BalancedComparisonEnvelope {
    pub schema_version: u32,
    pub scenario_id: String,
    pub configuration_hash: String,
    pub case_hash: String,
    pub disposition: BalancedEvaluationDisposition,
    pub text_results: ComparisonRanking,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balanced_results: Option<ComparisonRanking>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<BalancedExplanationProjection>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonRankingLabel {
    Text,
    StrictBalanced,
    NeutralizedCounterfactual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ComparisonRanking {
    pub label: ComparisonRankingLabel,
    pub ordered_candidate_ordinals: Vec<usize>,
    pub entries: Vec<ComparisonRankingEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ComparisonRankingEntry {
    pub candidate_ordinal: usize,
    pub tier: LexicalEvidenceTier,
    pub metadata_points: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BalancedExplanationProjection {
    pub schema_version: u32,
    pub level: BalancedExplanationLevel,
    pub summary: BalancedExplanationSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<BalancedCandidateRuleExplanation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BalancedExplanationSummary {
    pub candidate_count: usize,
    pub moved_candidate_count: usize,
    pub matched_signal_count: usize,
    pub nonmatched_signal_count: usize,
    pub absent_signal_count: usize,
    pub neutralized_signal_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BalancedCandidateRuleExplanation {
    pub candidate_ordinal: usize,
    pub metadata_points: i32,
    pub rules: Vec<BalancedSafeRuleExplanation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BalancedSafeRuleExplanation {
    pub rule: BalancedSafeRuleKind,
    pub outcome: BalancedSafeSignalOutcome,
    pub points: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BalancedSafeRuleKind {
    Recency,
    Authority,
    Archive,
    Property { ordinal: usize },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BalancedSafeSignalOutcome {
    Matched,
    Nonmatched,
    Absent,
    Neutralized,
}

pub fn balanced_configuration_hash(configuration: &BalancedPlaygroundConfiguration) -> String {
    configuration.configuration_hash()
}

pub fn balanced_case_hash(case: &BalancedPlaygroundCase) -> String {
    case.case_hash()
}

pub fn evaluate_balanced_playground(
    configuration: &BalancedPlaygroundConfiguration,
    case: &BalancedPlaygroundCase,
) -> BalancedComparisonEnvelope {
    let configuration_hash = configuration.configuration_hash();
    let case_hash = case.case_hash();
    let text_results = text_ranking(case);
    let mut fatal_reasons = BTreeSet::new();

    let profile = match configuration.profile() {
        Ok(profile) => Some(profile),
        Err(reason) => {
            fatal_reasons.insert(reason);
            None
        }
    };
    validate_case_shape(case, profile.as_ref(), &mut fatal_reasons);

    let lexical_validation = RerankInput {
        schema_version: RERANK_INPUT_SCHEMA_VERSION,
        query_time_epoch_seconds: case.query_time_epoch_seconds,
        candidates: case.candidates.clone(),
        source_signals: case
            .source_facts
            .iter()
            .map(|facts| SourceSignalObservation {
                source: facts.source.clone(),
                source_mtime_epoch_seconds: None,
                matched_property_rule_ids: Vec::new(),
                present_properties: Vec::new(),
                property_values: Vec::new(),
            })
            .collect(),
    };
    if let Err(error) = rerank_candidates(&RelevanceProfile::LexicalV1, &lexical_validation) {
        fatal_reasons.insert(classify_ranking_error(error));
    }

    if !fatal_reasons.is_empty() {
        return fatal_envelope(configuration_hash, case_hash, text_results, fatal_reasons);
    }

    let profile = profile.expect("validated profile is present when no fatal reason exists");
    let facts_by_source: BTreeMap<_, _> = case
        .source_facts
        .iter()
        .map(|facts| (facts.source.clone(), facts))
        .collect();
    validate_complete_states(configuration, case, &facts_by_source, &mut fatal_reasons);
    if !fatal_reasons.is_empty() {
        return fatal_envelope(configuration_hash, case_hash, text_results, fatal_reasons);
    }

    let neutralized_states = collect_neutralized_states(case);
    let neutralized_path = (!neutralized_states.is_empty()).then(|| neutral_path(configuration));
    let counterfactual_candidates = case
        .candidates
        .iter()
        .cloned()
        .map(|mut candidate| {
            let facts = facts_by_source
                .get(&candidate.source)
                .expect("validated source facts are complete");
            if should_neutralize_path(configuration, &candidate.path, facts) {
                candidate.path = neutralized_path
                    .as_ref()
                    .expect("counterfactual path exists")
                    .clone();
            }
            candidate
        })
        .collect();
    let source_signals = case
        .source_facts
        .iter()
        .map(|facts| source_signal(configuration, facts))
        .collect();
    let rerank_input = RerankInput {
        schema_version: RERANK_INPUT_SCHEMA_VERSION,
        query_time_epoch_seconds: case.query_time_epoch_seconds,
        candidates: counterfactual_candidates,
        source_signals,
    };
    let result = match rerank_candidates(&RelevanceProfile::D5cPreviewV1(profile), &rerank_input) {
        Ok(result) => result,
        Err(error) => {
            fatal_reasons.insert(classify_ranking_error(error));
            return fatal_envelope(configuration_hash, case_hash, text_results, fatal_reasons);
        }
    };

    let label = if neutralized_states.is_empty() {
        ComparisonRankingLabel::StrictBalanced
    } else {
        ComparisonRankingLabel::NeutralizedCounterfactual
    };
    let balanced_results = ComparisonRanking {
        label,
        ordered_candidate_ordinals: result
            .evidence()
            .entries
            .iter()
            .map(|entry| entry.ordinal)
            .collect(),
        entries: result
            .evidence()
            .entries
            .iter()
            .map(|entry| ComparisonRankingEntry {
                candidate_ordinal: entry.ordinal,
                tier: entry.tier,
                metadata_points: entry.points,
            })
            .collect(),
    };
    let explanation = explanation_projection(
        configuration,
        case,
        &facts_by_source,
        &text_results,
        &balanced_results,
    );
    let disposition = if neutralized_states.is_empty() {
        BalancedEvaluationDisposition::StrictBalanced
    } else {
        BalancedEvaluationDisposition::NeutralizedCounterfactual { neutralized_states }
    };

    BalancedComparisonEnvelope {
        schema_version: BALANCED_COMPARISON_ENVELOPE_SCHEMA_VERSION,
        scenario_id: BALANCED_PLAYGROUND_SCENARIO_ID.to_owned(),
        configuration_hash,
        case_hash,
        disposition,
        text_results,
        balanced_results: Some(balanced_results),
        explanation,
    }
}

fn validate_configuration_shape(
    configuration: &BalancedPlaygroundConfiguration,
) -> Result<(), BalancedFatalReason> {
    if configuration.schema_version != BALANCED_PLAYGROUND_CONFIGURATION_SCHEMA_VERSION
        || configuration.scenario_id != BALANCED_PLAYGROUND_SCENARIO_ID
        || configuration.authority_folders.is_empty()
        || configuration.archive_folders.is_empty()
    {
        return Err(BalancedFatalReason::Malformed);
    }
    if configuration.authority_folders.len() > MAX_FOLDER_RANKING_RULES_PER_FAMILY
        || configuration.archive_folders.len() > MAX_FOLDER_RANKING_RULES_PER_FAMILY
    {
        return Err(BalancedFatalReason::OverLimit);
    }
    if configuration
        .authority_folders
        .iter()
        .chain(&configuration.archive_folders)
        .any(|rule| rule.strength != RuleStrength::Standard)
    {
        return Err(BalancedFatalReason::Malformed);
    }
    if let Some(pack) = &configuration.property_fixture_pack {
        if pack.fixture_pack_id.is_empty()
            || pack.fixture_pack_id.len() > MAX_RANKING_RULE_ID_BYTES
            || pack.rules.is_empty()
        {
            return Err(BalancedFatalReason::Malformed);
        }
        if pack.rules.len() > MAX_BALANCED_PROPERTY_RULES
            || pack.rules.len() > MAX_PROPERTY_RANKING_RULES
        {
            return Err(BalancedFatalReason::OverLimit);
        }
        if pack
            .rules
            .iter()
            .any(|rule| rule.strength != RuleStrength::Low)
        {
            return Err(BalancedFatalReason::Malformed);
        }
    }
    let serialized = serde_json::to_vec(configuration)
        .expect("balanced playground configuration has no fallible serialization");
    if serialized.len() > MAX_RELEVANCE_PROFILE_BYTES {
        return Err(BalancedFatalReason::OverLimit);
    }
    Ok(())
}

fn validate_case_shape(
    case: &BalancedPlaygroundCase,
    profile: Option<&D5cRelevanceProfile>,
    fatal_reasons: &mut BTreeSet<BalancedFatalReason>,
) {
    if case.schema_version != BALANCED_PLAYGROUND_CASE_SCHEMA_VERSION
        || case.scenario_id != BALANCED_PLAYGROUND_SCENARIO_ID
    {
        fatal_reasons.insert(BalancedFatalReason::Malformed);
    }
    if case.candidates.len() > MAX_RERANK_CANDIDATES
        || case.source_facts.len() > MAX_RERANK_SOURCE_OBSERVATIONS
    {
        fatal_reasons.insert(BalancedFatalReason::OverLimit);
    }
    if case
        .source_facts
        .iter()
        .any(|facts| facts.schema_version != BALANCED_EVALUATION_SOURCE_FACTS_SCHEMA_VERSION)
        || case
            .source_facts
            .windows(2)
            .any(|pair| pair[0].source >= pair[1].source)
    {
        fatal_reasons.insert(BalancedFatalReason::Malformed);
    }

    let candidate_sources: BTreeSet<_> = case
        .candidates
        .iter()
        .map(|candidate| candidate.source.clone())
        .collect();
    let fact_sources: BTreeSet<_> = case
        .source_facts
        .iter()
        .map(|facts| facts.source.clone())
        .collect();
    if fact_sources
        .iter()
        .any(|source| !candidate_sources.contains(source))
    {
        fatal_reasons.insert(BalancedFatalReason::Unauthorized);
    }
    if candidate_sources
        .iter()
        .any(|source| !fact_sources.contains(source))
    {
        fatal_reasons.insert(BalancedFatalReason::Missing);
    }
    if fact_sources.len() != case.source_facts.len() {
        fatal_reasons.insert(BalancedFatalReason::Conflicting);
    }

    let configured_property_ids: BTreeSet<_> = profile
        .map(|profile| {
            profile
                .property_rules
                .iter()
                .map(|rule| rule.id.as_str())
                .collect()
        })
        .unwrap_or_default();
    for facts in &case.source_facts {
        if facts.property_rules.len() > MAX_BALANCED_PROPERTY_RULES {
            fatal_reasons.insert(BalancedFatalReason::OverLimit);
        }
        if facts
            .property_rules
            .windows(2)
            .any(|pair| pair[0].rule_id >= pair[1].rule_id)
        {
            fatal_reasons.insert(BalancedFatalReason::Malformed);
        }
        let observed_ids: BTreeSet<_> = facts
            .property_rules
            .iter()
            .map(|signal| signal.rule_id.as_str())
            .collect();
        if observed_ids.len() != facts.property_rules.len() {
            fatal_reasons.insert(BalancedFatalReason::Conflicting);
        }
        if observed_ids
            .iter()
            .any(|rule_id| !configured_property_ids.contains(rule_id))
        {
            fatal_reasons.insert(BalancedFatalReason::Unauthorized);
        }
        if configured_property_ids
            .iter()
            .any(|rule_id| !observed_ids.contains(rule_id))
        {
            fatal_reasons.insert(BalancedFatalReason::Missing);
        }
        for state in states(facts) {
            if discrepancy_decision(state) == DiscrepancyDecision::Fatal {
                fatal_reasons.insert(fatal_reason_for_state(state));
            }
        }
    }
}

fn validate_complete_states(
    configuration: &BalancedPlaygroundConfiguration,
    case: &BalancedPlaygroundCase,
    facts_by_source: &BTreeMap<QualifiedSourceId, &EvaluationSourceFacts>,
    fatal_reasons: &mut BTreeSet<BalancedFatalReason>,
) {
    for candidate in &case.candidates {
        let facts = facts_by_source
            .get(&candidate.source)
            .expect("source facts were checked for completeness");
        if recency_state_conflicts(
            facts.recency,
            facts.source_mtime_epoch_seconds.map(|mtime| {
                mtime > 0
                    && mtime <= case.query_time_epoch_seconds
                    && case.query_time_epoch_seconds - mtime <= RecencyHorizon::Quarter.seconds()
            }),
        ) || path_state_conflicts(
            facts.authority,
            path_matches_any(&candidate.path, &configuration.authority_folders),
        ) || path_state_conflicts(
            facts.archive,
            path_matches_any(&candidate.path, &configuration.archive_folders),
        ) {
            fatal_reasons.insert(BalancedFatalReason::Conflicting);
        }
    }
}

fn recency_state_conflicts(state: EvaluationSignalState, matched: Option<bool>) -> bool {
    match state {
        EvaluationSignalState::Matched => matched != Some(true),
        EvaluationSignalState::Nonmatched => matched != Some(false),
        EvaluationSignalState::Absent => matched.is_some(),
        _ => false,
    }
}

fn path_state_conflicts(state: EvaluationSignalState, matched: bool) -> bool {
    match state {
        EvaluationSignalState::Matched => !matched,
        EvaluationSignalState::Nonmatched | EvaluationSignalState::Absent => matched,
        _ => false,
    }
}

fn collect_neutralized_states(case: &BalancedPlaygroundCase) -> Vec<EvaluationSignalState> {
    case.source_facts
        .iter()
        .flat_map(states)
        .filter(|state| {
            discrepancy_decision(*state) == DiscrepancyDecision::NeutralizedCounterfactual
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn states(facts: &EvaluationSourceFacts) -> impl Iterator<Item = EvaluationSignalState> + '_ {
    [facts.recency, facts.authority, facts.archive]
        .into_iter()
        .chain(facts.property_rules.iter().map(|signal| signal.state))
}

fn source_signal(
    configuration: &BalancedPlaygroundConfiguration,
    facts: &EvaluationSourceFacts,
) -> SourceSignalObservation {
    let matched_property_rule_ids = facts
        .property_rules
        .iter()
        .filter(|signal| signal.state == EvaluationSignalState::Matched)
        .map(|signal| signal.rule_id.clone())
        .collect();
    let source_mtime_epoch_seconds = match facts.recency {
        EvaluationSignalState::Matched | EvaluationSignalState::Nonmatched => {
            facts.source_mtime_epoch_seconds
        }
        EvaluationSignalState::Absent
        | EvaluationSignalState::Unsupported
        | EvaluationSignalState::Stale
        | EvaluationSignalState::Unavailable
        | EvaluationSignalState::Untrusted
        | EvaluationSignalState::Malformed
        | EvaluationSignalState::Conflicting
        | EvaluationSignalState::Missing
        | EvaluationSignalState::Unauthorized
        | EvaluationSignalState::OverLimit => None,
    };
    debug_assert_eq!(
        configuration
            .property_fixture_pack
            .as_ref()
            .map_or(0, |pack| pack.rules.len()),
        facts.property_rules.len()
    );
    SourceSignalObservation {
        source: facts.source.clone(),
        source_mtime_epoch_seconds,
        matched_property_rule_ids,
        present_properties: Vec::new(),
        property_values: Vec::new(),
    }
}

fn should_neutralize_path(
    configuration: &BalancedPlaygroundConfiguration,
    path: &str,
    facts: &EvaluationSourceFacts,
) -> bool {
    is_neutralized(facts.authority) && path_matches_any(path, &configuration.authority_folders)
        || is_neutralized(facts.archive) && path_matches_any(path, &configuration.archive_folders)
}

fn is_neutralized(state: EvaluationSignalState) -> bool {
    discrepancy_decision(state) == DiscrepancyDecision::NeutralizedCounterfactual
}

fn neutral_path(configuration: &BalancedPlaygroundConfiguration) -> String {
    for ordinal in 0..=MAX_FOLDER_RANKING_RULES_PER_FAMILY * 2 {
        let path = format!("__kwiry_balanced_neutral_{ordinal}/note.md");
        if !path_matches_any(&path, &configuration.authority_folders)
            && !path_matches_any(&path, &configuration.archive_folders)
        {
            return path;
        }
    }
    unreachable!("bounded folder rules cannot occupy every neutral root")
}

fn path_matches_any(path: &str, rules: &[FolderRule]) -> bool {
    rules.iter().any(|rule| {
        path.strip_prefix(&rule.prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

fn text_ranking(case: &BalancedPlaygroundCase) -> ComparisonRanking {
    ComparisonRanking {
        label: ComparisonRankingLabel::Text,
        ordered_candidate_ordinals: (0..case.candidates.len()).collect(),
        entries: case
            .candidates
            .iter()
            .enumerate()
            .map(|(candidate_ordinal, candidate)| ComparisonRankingEntry {
                candidate_ordinal,
                tier: candidate.evidence_tier,
                metadata_points: 0,
            })
            .collect(),
    }
}

fn explanation_projection(
    configuration: &BalancedPlaygroundConfiguration,
    case: &BalancedPlaygroundCase,
    facts_by_source: &BTreeMap<QualifiedSourceId, &EvaluationSourceFacts>,
    text_results: &ComparisonRanking,
    balanced_results: &ComparisonRanking,
) -> Option<BalancedExplanationProjection> {
    if case.explanation_level == BalancedExplanationLevel::Off {
        return None;
    }
    let mut matched_signal_count = 0;
    let mut nonmatched_signal_count = 0;
    let mut absent_signal_count = 0;
    let mut neutralized_signal_count = 0;
    for state in case.source_facts.iter().flat_map(states) {
        match safe_outcome(state) {
            BalancedSafeSignalOutcome::Matched => matched_signal_count += 1,
            BalancedSafeSignalOutcome::Nonmatched => nonmatched_signal_count += 1,
            BalancedSafeSignalOutcome::Absent => absent_signal_count += 1,
            BalancedSafeSignalOutcome::Neutralized => neutralized_signal_count += 1,
        }
    }
    let moved_candidate_count = text_results
        .ordered_candidate_ordinals
        .iter()
        .zip(&balanced_results.ordered_candidate_ordinals)
        .filter(|(left, right)| left != right)
        .count();
    let summary = BalancedExplanationSummary {
        candidate_count: case.candidates.len(),
        moved_candidate_count,
        matched_signal_count,
        nonmatched_signal_count,
        absent_signal_count,
        neutralized_signal_count,
    };
    let rules = if case.explanation_level == BalancedExplanationLevel::Rules {
        let points_by_ordinal: BTreeMap<_, _> = balanced_results
            .entries
            .iter()
            .map(|entry| (entry.candidate_ordinal, entry.metadata_points))
            .collect();
        case.candidates
            .iter()
            .enumerate()
            .map(|(candidate_ordinal, candidate)| {
                let facts = facts_by_source
                    .get(&candidate.source)
                    .expect("validated source facts are complete");
                let mut rules = vec![
                    safe_rule(BalancedSafeRuleKind::Recency, facts.recency, 1),
                    safe_rule(BalancedSafeRuleKind::Authority, facts.authority, 2),
                    safe_rule(BalancedSafeRuleKind::Archive, facts.archive, -2),
                ];
                if let Some(pack) = &configuration.property_fixture_pack {
                    rules.extend(pack.rules.iter().enumerate().map(|(ordinal, rule)| {
                        let state = facts.property_rules[ordinal].state;
                        let points = match rule.effect {
                            RuleEffect::Boost => 1,
                            RuleEffect::Demote => -1,
                        };
                        safe_rule(BalancedSafeRuleKind::Property { ordinal }, state, points)
                    }));
                }
                BalancedCandidateRuleExplanation {
                    candidate_ordinal,
                    metadata_points: points_by_ordinal[&candidate_ordinal],
                    rules,
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    Some(BalancedExplanationProjection {
        schema_version: BALANCED_EXPLANATION_SCHEMA_VERSION,
        level: case.explanation_level,
        summary,
        rules,
    })
}

fn safe_rule(
    rule: BalancedSafeRuleKind,
    state: EvaluationSignalState,
    matched_points: i32,
) -> BalancedSafeRuleExplanation {
    let outcome = safe_outcome(state);
    BalancedSafeRuleExplanation {
        rule,
        outcome,
        points: if outcome == BalancedSafeSignalOutcome::Matched {
            matched_points
        } else {
            0
        },
    }
}

fn safe_outcome(state: EvaluationSignalState) -> BalancedSafeSignalOutcome {
    match state {
        EvaluationSignalState::Matched => BalancedSafeSignalOutcome::Matched,
        EvaluationSignalState::Nonmatched => BalancedSafeSignalOutcome::Nonmatched,
        EvaluationSignalState::Absent => BalancedSafeSignalOutcome::Absent,
        EvaluationSignalState::Unsupported
        | EvaluationSignalState::Stale
        | EvaluationSignalState::Unavailable
        | EvaluationSignalState::Untrusted => BalancedSafeSignalOutcome::Neutralized,
        EvaluationSignalState::Malformed
        | EvaluationSignalState::Conflicting
        | EvaluationSignalState::Missing
        | EvaluationSignalState::Unauthorized
        | EvaluationSignalState::OverLimit => {
            unreachable!("fatal states never receive explanation projection")
        }
    }
}

fn fatal_envelope(
    configuration_hash: String,
    case_hash: String,
    text_results: ComparisonRanking,
    reasons: BTreeSet<BalancedFatalReason>,
) -> BalancedComparisonEnvelope {
    BalancedComparisonEnvelope {
        schema_version: BALANCED_COMPARISON_ENVELOPE_SCHEMA_VERSION,
        scenario_id: BALANCED_PLAYGROUND_SCENARIO_ID.to_owned(),
        configuration_hash,
        case_hash,
        disposition: BalancedEvaluationDisposition::Fatal {
            reasons: reasons.into_iter().collect(),
        },
        text_results,
        balanced_results: None,
        explanation: None,
    }
}

fn fatal_reason_for_state(state: EvaluationSignalState) -> BalancedFatalReason {
    match state {
        EvaluationSignalState::Malformed => BalancedFatalReason::Malformed,
        EvaluationSignalState::Conflicting => BalancedFatalReason::Conflicting,
        EvaluationSignalState::Missing => BalancedFatalReason::Missing,
        EvaluationSignalState::Unauthorized => BalancedFatalReason::Unauthorized,
        EvaluationSignalState::OverLimit => BalancedFatalReason::OverLimit,
        EvaluationSignalState::Matched
        | EvaluationSignalState::Nonmatched
        | EvaluationSignalState::Absent
        | EvaluationSignalState::Unsupported
        | EvaluationSignalState::Stale
        | EvaluationSignalState::Unavailable
        | EvaluationSignalState::Untrusted => {
            unreachable!("nonfatal states do not have fatal reasons")
        }
    }
}

fn classify_ranking_error(error: crate::ranking::RankingError) -> BalancedFatalReason {
    if error.code == "ranking_work_limit_exceeded"
        || error.message.contains("limit")
        || error.message.contains("cap")
    {
        BalancedFatalReason::OverLimit
    } else if error.message.contains("duplicate")
        || error.message.contains("overlap")
        || error.message.contains("inconsistent")
    {
        BalancedFatalReason::Conflicting
    } else {
        BalancedFatalReason::Malformed
    }
}

fn canonical_hash<T: Serialize>(domain: &[u8], value: &T) -> String {
    let canonical = serde_json::to_vec(value)
        .expect("balanced playground wire models have no fallible serialization");
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((canonical.len() as u64).to_be_bytes());
    digest.update(canonical);
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ranking::{PropertyPredicate, RankingScalar};

    fn folder(id: &str, prefix: &str) -> FolderRule {
        FolderRule {
            id: id.to_owned(),
            prefix: prefix.to_owned(),
            strength: RuleStrength::Standard,
        }
    }

    fn configuration() -> BalancedPlaygroundConfiguration {
        BalancedPlaygroundConfiguration::candidate(
            vec![folder("10-authority", "reference")],
            vec![folder("20-archive", "archive")],
        )
    }

    fn source(key: &str) -> QualifiedSourceId {
        QualifiedSourceId {
            authorization_scope: "fixture-scope".to_owned(),
            source_key: key.to_owned(),
        }
    }

    fn candidate(key: &str, path: &str, tier: LexicalEvidenceTier) -> RerankCandidate {
        RerankCandidate {
            source: source(key),
            chunk_id: format!("chunk-{key}"),
            path: path.to_owned(),
            evidence_tier: tier,
            lexical_score: 1.0,
        }
    }

    fn facts(
        key: &str,
        mtime: Option<u64>,
        recency: EvaluationSignalState,
        authority: EvaluationSignalState,
        archive: EvaluationSignalState,
    ) -> EvaluationSourceFacts {
        EvaluationSourceFacts {
            schema_version: BALANCED_EVALUATION_SOURCE_FACTS_SCHEMA_VERSION,
            source: source(key),
            source_mtime_epoch_seconds: mtime,
            recency,
            authority,
            archive,
            property_rules: Vec::new(),
        }
    }

    fn case(
        candidates: Vec<RerankCandidate>,
        source_facts: Vec<EvaluationSourceFacts>,
    ) -> BalancedPlaygroundCase {
        BalancedPlaygroundCase {
            schema_version: BALANCED_PLAYGROUND_CASE_SCHEMA_VERSION,
            scenario_id: BALANCED_PLAYGROUND_SCENARIO_ID.to_owned(),
            query_time_epoch_seconds: 2_000_000_000,
            candidates,
            source_facts,
            explanation_level: BalancedExplanationLevel::Rules,
        }
    }

    #[test]
    fn discrepancy_table_is_complete_and_exact() {
        assert_eq!(DISCREPANCY_DECISION_TABLE.len(), 12);
        for (state, expected) in DISCREPANCY_DECISION_TABLE {
            assert_eq!(discrepancy_decision(state), expected);
        }
        assert_eq!(
            DISCREPANCY_DECISION_TABLE
                .iter()
                .map(|(state, _)| *state)
                .collect::<BTreeSet<_>>()
                .len(),
            12
        );
    }

    #[test]
    fn candidate_configuration_is_fixed_to_balanced_strengths() {
        let mut configuration = configuration();
        let profile = configuration.profile().unwrap();
        let recency = profile.recency.unwrap();
        assert_eq!(recency.horizon, RecencyHorizon::Quarter);
        assert_eq!(recency.strength, RuleStrength::Low);
        assert!(profile.hierarchy.depth.is_none());
        assert!(
            profile
                .hierarchy
                .authority_folders
                .iter()
                .all(|rule| rule.strength == RuleStrength::Standard)
        );
        assert!(
            profile
                .hierarchy
                .archive_folders
                .iter()
                .all(|rule| rule.strength == RuleStrength::Standard)
        );
        assert!(profile.property_rules.is_empty());

        configuration.property_fixture_pack = Some(BalancedPropertyFixturePack {
            fixture_pack_id: "property-pack-a".to_owned(),
            rules: vec![PropertyRule {
                id: "30-priority".to_owned(),
                property: "priority-secret-name".to_owned(),
                predicate: PropertyPredicate::Exact {
                    pointer: Some(String::new()),
                    value: RankingScalar::String("private-value".to_owned()),
                },
                effect: RuleEffect::Boost,
                strength: RuleStrength::Standard,
            }],
        });
        assert_eq!(
            configuration.profile().unwrap_err(),
            BalancedFatalReason::Malformed
        );
    }

    #[test]
    fn strict_balanced_uses_tier_first_order_and_keeps_text_independent() {
        let case = case(
            vec![
                candidate("strong-old", "notes/old.md", LexicalEvidenceTier::AllTerms),
                candidate("weak-new", "reference/new.md", LexicalEvidenceTier::Prefix),
            ],
            vec![
                facts(
                    "strong-old",
                    Some(1),
                    EvaluationSignalState::Nonmatched,
                    EvaluationSignalState::Nonmatched,
                    EvaluationSignalState::Nonmatched,
                ),
                facts(
                    "weak-new",
                    Some(1_999_999_999),
                    EvaluationSignalState::Matched,
                    EvaluationSignalState::Matched,
                    EvaluationSignalState::Nonmatched,
                ),
            ],
        );
        let configuration = configuration();
        let envelope = evaluate_balanced_playground(&configuration, &case);
        assert_eq!(
            envelope,
            evaluate_balanced_playground(&configuration, &case)
        );
        let serialized = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            serde_json::from_str::<BalancedComparisonEnvelope>(&serialized).unwrap(),
            envelope
        );
        assert_eq!(
            envelope.disposition,
            BalancedEvaluationDisposition::StrictBalanced
        );
        assert_eq!(envelope.text_results.label, ComparisonRankingLabel::Text);
        assert_eq!(envelope.text_results.ordered_candidate_ordinals, [0, 1]);
        let balanced = envelope.balanced_results.unwrap();
        assert_eq!(balanced.label, ComparisonRankingLabel::StrictBalanced);
        assert_eq!(balanced.ordered_candidate_ordinals, [0, 1]);
        assert_eq!(
            balanced
                .entries
                .iter()
                .map(|entry| entry.metadata_points)
                .collect::<Vec<_>>(),
            [0, 3]
        );
    }

    #[test]
    fn every_refusal_state_yields_a_labeled_neutralized_counterfactual() {
        for refusal in [
            EvaluationSignalState::Unsupported,
            EvaluationSignalState::Stale,
            EvaluationSignalState::Unavailable,
            EvaluationSignalState::Untrusted,
        ] {
            let case = case(
                vec![
                    candidate("plain", "notes/plain.md", LexicalEvidenceTier::AllTerms),
                    candidate(
                        "authority",
                        "reference/note.md",
                        LexicalEvidenceTier::AllTerms,
                    ),
                ],
                vec![
                    facts(
                        "authority",
                        Some(1),
                        EvaluationSignalState::Nonmatched,
                        refusal,
                        EvaluationSignalState::Nonmatched,
                    ),
                    facts(
                        "plain",
                        Some(1),
                        EvaluationSignalState::Nonmatched,
                        EvaluationSignalState::Nonmatched,
                        EvaluationSignalState::Nonmatched,
                    ),
                ],
            );
            let envelope = evaluate_balanced_playground(&configuration(), &case);
            assert_eq!(envelope.text_results.ordered_candidate_ordinals, [0, 1]);
            assert_eq!(
                envelope.disposition,
                BalancedEvaluationDisposition::NeutralizedCounterfactual {
                    neutralized_states: vec![refusal],
                }
            );
            let balanced = envelope.balanced_results.unwrap();
            assert_eq!(
                balanced.label,
                ComparisonRankingLabel::NeutralizedCounterfactual
            );
            assert_eq!(balanced.ordered_candidate_ordinals, [0, 1]);
            assert!(
                balanced
                    .entries
                    .iter()
                    .all(|entry| entry.metadata_points == 0)
            );
        }
    }

    #[test]
    fn every_fatal_state_refuses_balanced_without_relabeling_text() {
        for state in [
            EvaluationSignalState::Malformed,
            EvaluationSignalState::Conflicting,
            EvaluationSignalState::Missing,
            EvaluationSignalState::Unauthorized,
            EvaluationSignalState::OverLimit,
        ] {
            let case = case(
                vec![candidate(
                    "note",
                    "notes/note.md",
                    LexicalEvidenceTier::AllTerms,
                )],
                vec![facts(
                    "note",
                    None,
                    state,
                    EvaluationSignalState::Nonmatched,
                    EvaluationSignalState::Nonmatched,
                )],
            );
            let envelope = evaluate_balanced_playground(&configuration(), &case);
            assert!(matches!(
                envelope.disposition,
                BalancedEvaluationDisposition::Fatal { .. }
            ));
            assert_eq!(envelope.text_results.label, ComparisonRankingLabel::Text);
            assert_eq!(envelope.text_results.ordered_candidate_ordinals, [0]);
            assert!(envelope.balanced_results.is_none());
            assert!(envelope.explanation.is_none());
        }
    }

    #[test]
    fn missing_and_unauthorized_source_facts_are_fatal() {
        let missing = case(
            vec![candidate(
                "note",
                "notes/note.md",
                LexicalEvidenceTier::AllTerms,
            )],
            Vec::new(),
        );
        let missing = evaluate_balanced_playground(&configuration(), &missing);
        assert_eq!(
            missing.disposition,
            BalancedEvaluationDisposition::Fatal {
                reasons: vec![BalancedFatalReason::Missing],
            }
        );

        let unauthorized = case(
            vec![candidate(
                "note",
                "notes/note.md",
                LexicalEvidenceTier::AllTerms,
            )],
            vec![facts(
                "other",
                None,
                EvaluationSignalState::Absent,
                EvaluationSignalState::Nonmatched,
                EvaluationSignalState::Nonmatched,
            )],
        );
        let unauthorized = evaluate_balanced_playground(&configuration(), &unauthorized);
        assert_eq!(
            unauthorized.disposition,
            BalancedEvaluationDisposition::Fatal {
                reasons: vec![
                    BalancedFatalReason::Missing,
                    BalancedFatalReason::Unauthorized,
                ],
            }
        );
    }

    #[test]
    fn property_fixture_pack_is_single_bounded_and_low_strength() {
        let property = |id: &str| PropertyRule {
            id: id.to_owned(),
            property: id.to_owned(),
            predicate: PropertyPredicate::Presence,
            effect: RuleEffect::Boost,
            strength: RuleStrength::Low,
        };
        let mut configuration = configuration();
        configuration.property_fixture_pack = Some(BalancedPropertyFixturePack {
            fixture_pack_id: "pack".to_owned(),
            rules: vec![property("30-a"), property("31-b")],
        });
        assert_eq!(configuration.profile().unwrap().property_rules.len(), 2);

        configuration
            .property_fixture_pack
            .as_mut()
            .unwrap()
            .rules
            .push(property("32-c"));
        assert_eq!(
            configuration.profile().unwrap_err(),
            BalancedFatalReason::OverLimit
        );
    }

    #[test]
    fn rules_explanation_contains_no_sensitive_fixture_data() {
        let mut configuration = configuration();
        configuration.property_fixture_pack = Some(BalancedPropertyFixturePack {
            fixture_pack_id: "secret-pack".to_owned(),
            rules: vec![PropertyRule {
                id: "30-secret-rule".to_owned(),
                property: "secret-property".to_owned(),
                predicate: PropertyPredicate::Exact {
                    pointer: Some("/secret-pointer".to_owned()),
                    value: RankingScalar::String("secret-value".to_owned()),
                },
                effect: RuleEffect::Boost,
                strength: RuleStrength::Low,
            }],
        });
        let mut facts = facts(
            "secret-source",
            Some(1_999_999_999),
            EvaluationSignalState::Matched,
            EvaluationSignalState::Matched,
            EvaluationSignalState::Nonmatched,
        );
        facts.property_rules = vec![EvaluationPropertySignal {
            rule_id: "30-secret-rule".to_owned(),
            state: EvaluationSignalState::Matched,
        }];
        let case = case(
            vec![candidate(
                "secret-source",
                "reference/secret-path.md",
                LexicalEvidenceTier::AllTerms,
            )],
            vec![facts],
        );
        let envelope = evaluate_balanced_playground(&configuration, &case);
        let explanation = serde_json::to_string(&envelope.explanation.unwrap()).unwrap();
        for forbidden in [
            "secret-pack",
            "secret-rule",
            "secret-property",
            "secret-pointer",
            "secret-value",
            "secret-source",
            "secret-path",
            "fixture-scope",
            "1999999999",
        ] {
            assert!(!explanation.contains(forbidden), "leaked {forbidden}");
        }
        assert!(explanation.contains("property"));
        assert!(explanation.contains("candidate_ordinal"));
    }

    #[test]
    fn explanation_levels_are_off_summary_and_rules_only() {
        let mut case = case(
            vec![candidate(
                "note",
                "notes/note.md",
                LexicalEvidenceTier::AllTerms,
            )],
            vec![facts(
                "note",
                None,
                EvaluationSignalState::Absent,
                EvaluationSignalState::Absent,
                EvaluationSignalState::Absent,
            )],
        );
        case.explanation_level = BalancedExplanationLevel::Off;
        assert!(
            evaluate_balanced_playground(&configuration(), &case)
                .explanation
                .is_none()
        );

        case.explanation_level = BalancedExplanationLevel::Summary;
        let summary = evaluate_balanced_playground(&configuration(), &case)
            .explanation
            .unwrap();
        assert_eq!(summary.level, BalancedExplanationLevel::Summary);
        assert!(summary.rules.is_empty());

        case.explanation_level = BalancedExplanationLevel::Rules;
        let rules = evaluate_balanced_playground(&configuration(), &case)
            .explanation
            .unwrap();
        assert_eq!(rules.level, BalancedExplanationLevel::Rules);
        assert_eq!(rules.rules.len(), 1);
        assert_eq!(rules.rules[0].rules.len(), 3);
    }

    #[test]
    fn evaluation_cardinality_ceiling_is_fatal_without_a_counterfactual() {
        let candidates = (0..=MAX_RERANK_CANDIDATES)
            .map(|ordinal| {
                candidate(
                    &format!("source-{ordinal:03}"),
                    &format!("notes/note-{ordinal:03}.md"),
                    LexicalEvidenceTier::AllTerms,
                )
            })
            .collect();
        let source_facts = (0..=MAX_RERANK_CANDIDATES)
            .map(|ordinal| {
                facts(
                    &format!("source-{ordinal:03}"),
                    None,
                    EvaluationSignalState::Absent,
                    EvaluationSignalState::Absent,
                    EvaluationSignalState::Absent,
                )
            })
            .collect();
        let case = case(candidates, source_facts);
        let envelope = evaluate_balanced_playground(&configuration(), &case);
        assert_eq!(
            envelope.disposition,
            BalancedEvaluationDisposition::Fatal {
                reasons: vec![BalancedFatalReason::OverLimit],
            }
        );
        assert_eq!(envelope.text_results.label, ComparisonRankingLabel::Text);
        assert!(envelope.balanced_results.is_none());
    }

    #[test]
    fn configuration_and_case_hashes_are_canonical_and_domain_separated() {
        let configuration = configuration();
        let case = case(
            vec![candidate(
                "note",
                "notes/note.md",
                LexicalEvidenceTier::AllTerms,
            )],
            vec![facts(
                "note",
                None,
                EvaluationSignalState::Absent,
                EvaluationSignalState::Nonmatched,
                EvaluationSignalState::Nonmatched,
            )],
        );
        let configuration_json = serde_json::to_string(&configuration).unwrap();
        let configuration_round_trip: BalancedPlaygroundConfiguration =
            serde_json::from_str(&configuration_json).unwrap();
        let case_json = serde_json::to_string(&case).unwrap();
        let case_round_trip: BalancedPlaygroundCase = serde_json::from_str(&case_json).unwrap();

        assert_eq!(
            configuration.configuration_hash(),
            configuration_round_trip.configuration_hash()
        );
        assert_eq!(case.case_hash(), case_round_trip.case_hash());
        assert_eq!(
            configuration.configuration_hash(),
            "91bbda6f56de228edb1517457a4c5aed0a21fc689a5030fadb26d174a20340ed"
        );
        assert_eq!(
            case.case_hash(),
            "22e35d8bc49343b3c8ad819f8abe7dbd8d43b02f094ac2535047aed7c081f2f0"
        );
        assert_ne!(configuration.configuration_hash(), case.case_hash());
        assert_eq!(configuration.configuration_hash().len(), 64);
        assert_eq!(case.case_hash().len(), 64);
        assert!(
            configuration
                .configuration_hash()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );

        let unknown = configuration_json.replacen(
            "\"schema_version\":1",
            "\"schema_version\":1,\"unknown\":true",
            1,
        );
        assert!(serde_json::from_str::<BalancedPlaygroundConfiguration>(&unknown).is_err());
    }
}
