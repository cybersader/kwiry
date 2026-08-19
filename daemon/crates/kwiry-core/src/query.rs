use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::lexical::{normalize_raw, technical_identifier_spans, technical_identifiers};

pub const LEXICAL_QUERY_PLAN_SCHEMA_VERSION: u32 = 7;
pub const MAX_QUERY_BYTES: usize = 4_096;
pub const MAX_QUERY_TERMS: usize = 128;
pub const MAX_TERM_SUPPORT_PROBES: usize = 128;
pub const MAX_EVIDENCE_STAGES: usize = 6;
pub const MAX_PARTIAL_COVERAGE_TERMS: usize = 128;
pub const MIN_PREFIX_CHARS: usize = 3;
pub const MAX_PREFIX_TERMS: usize = 8;
pub const MAX_PREFIX_EXPANSIONS_PER_TERM: usize = 16;
/// Alphabetical vocabulary entries examined per prefix term before the kept
/// expansions are selected. The budget that reaches a stage stays
/// `MAX_PREFIX_EXPANSIONS_PER_TERM`; this only widens what that budget may be
/// chosen from, so a title-bearing expansion is not lost to an arbitrary
/// alphabetical cut.
pub const MAX_PREFIX_EXPANSION_SCAN: usize = 256;
pub const MAX_CANDIDATES_PER_STAGE: usize = 256;
pub const MAX_TOTAL_CANDIDATES: usize = 512;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryPlanKind {
    Explicit,
    Ordinary,
    Identifier,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryMatchOperator {
    Explicit,
    Any,
    All,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryAssistanceEligibility {
    ExplicitSyntaxBypass,
    Eligible,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryExecutionDisposition {
    ExplicitBypass,
    AwaitingEvidence,
    Ready,
    EmptyNoEvidence,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryTermRole {
    RequiredIdentifierAnchor,
    OptionalContext,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum QueryTermProjection {
    AnalyzedText,
    ExactIdentifier,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryTermSupport {
    Unknown,
    Useful,
    Unsupported,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryField {
    Filename,
    Stem,
    Aliases,
    Title,
    Heading,
    Tags,
    Content,
    ContentIdentifiers,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryFieldGroup {
    SearchableText,
    Metadata,
    Exact,
    Phrase,
    Prefix,
    PrefixMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryFieldGroups {
    pub searchable_text: Vec<QueryField>,
    pub metadata: Vec<QueryField>,
    pub exact: Vec<QueryField>,
    pub phrase: Vec<QueryField>,
    pub prefix: Vec<QueryField>,
    pub prefix_metadata: Vec<QueryField>,
}

impl QueryFieldGroups {
    fn lexical_v1() -> Self {
        let searchable_text = vec![
            QueryField::Filename,
            QueryField::Stem,
            QueryField::Aliases,
            QueryField::Title,
            QueryField::Heading,
            QueryField::Tags,
            QueryField::Content,
        ];
        Self {
            metadata: searchable_text[..6].to_vec(),
            exact: vec![
                QueryField::Filename,
                QueryField::Stem,
                QueryField::Aliases,
                QueryField::Title,
                QueryField::Heading,
                QueryField::ContentIdentifiers,
            ],
            phrase: searchable_text.clone(),
            prefix: searchable_text.clone(),
            prefix_metadata: searchable_text[..4].to_vec(),
            searchable_text,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryTypoStage {
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryBounds {
    pub max_query_bytes: usize,
    pub max_query_terms: usize,
    pub max_term_support_probes: usize,
    pub max_evidence_stages: usize,
    pub max_partial_coverage_terms: usize,
    pub min_prefix_chars: usize,
    pub max_prefix_terms: usize,
    pub max_prefix_expansions_per_term: usize,
    pub max_prefix_expansion_scan: usize,
    pub max_candidates_per_stage: usize,
    pub max_total_candidates: usize,
}

impl QueryBounds {
    fn lexical_v1() -> Self {
        Self {
            max_query_bytes: MAX_QUERY_BYTES,
            max_query_terms: MAX_QUERY_TERMS,
            max_term_support_probes: MAX_TERM_SUPPORT_PROBES,
            max_evidence_stages: MAX_EVIDENCE_STAGES,
            max_partial_coverage_terms: MAX_PARTIAL_COVERAGE_TERMS,
            min_prefix_chars: MIN_PREFIX_CHARS,
            max_prefix_terms: MAX_PREFIX_TERMS,
            max_prefix_expansions_per_term: MAX_PREFIX_EXPANSIONS_PER_TERM,
            max_prefix_expansion_scan: MAX_PREFIX_EXPANSION_SCAN,
            max_candidates_per_stage: MAX_CANDIDATES_PER_STAGE,
            max_total_candidates: MAX_TOTAL_CANDIDATES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryTermIntent {
    pub index: u16,
    pub text: String,
    pub role: QueryTermRole,
    pub projection: QueryTermProjection,
    pub support: QueryTermSupport,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryExactIntent {
    pub normalized: String,
    pub field_group: QueryFieldGroup,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryPhraseIntent {
    pub terms: Vec<String>,
    pub field_group: QueryFieldGroup,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryTermSupportProbe {
    pub probe_id: u16,
    pub term_index: u16,
    pub term: String,
    pub field_group: QueryFieldGroup,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryTermSupportObservation {
    pub probe_id: u16,
    pub term_index: u16,
    pub document_frequency: u64,
    pub prefix_expansions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryEvidenceReport {
    pub schema_version: u32,
    pub identifier_probe_matched: Option<bool>,
    pub term_support: Vec<QueryTermSupportObservation>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
/// The evidence ladder, strongest first.
///
/// `PrefixMetadata` sits above `AllTerms` because a note *named* for what was
/// typed is better evidence than a note that merely mentions it. Stages fill the
/// visible window in order, so a later stage never competes on score with an
/// earlier one: with `AllTerms` first, a passing sentence in body prose
/// containing the literal typed tokens outranked a filename or title carrying
/// the words those tokens abbreviate.
pub enum QueryEvidenceStageKind {
    ExactMetadata,
    ExactPhrase,
    PrefixMetadata,
    AllTerms,
    Prefix,
    PartialCoverage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryEvidenceStage {
    pub ordinal: u8,
    pub kind: QueryEvidenceStageKind,
    pub field_group: QueryFieldGroup,
    pub required_term_indexes: Vec<u16>,
    pub prefix_term_indexes: Vec<u16>,
    pub max_candidates: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryMetadataField {
    Filename,
    Stem,
    Aliases,
    Title,
    Heading,
    Tags,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryMetadataProbe {
    pub query: String,
    pub fields: Vec<QueryMetadataField>,
    pub conjunction: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LexicalQueryPlan {
    pub schema_version: u32,
    pub query: String,
    pub kind: QueryPlanKind,
    pub match_operator: QueryMatchOperator,
    pub assistance: QueryAssistanceEligibility,
    pub execution: QueryExecutionDisposition,
    pub terms: Vec<String>,
    pub term_intents: Vec<QueryTermIntent>,
    pub normalized_exact: Option<String>,
    pub exact_intent: Option<QueryExactIntent>,
    pub phrase_boost: bool,
    pub phrase_intent: Option<QueryPhraseIntent>,
    pub field_groups: QueryFieldGroups,
    pub bounds: QueryBounds,
    pub typo_stage: QueryTypoStage,
    pub support_probes: Vec<QueryTermSupportProbe>,
    pub evidence_stages: Vec<QueryEvidenceStage>,
    pub metadata_probe: Option<QueryMetadataProbe>,
}

impl LexicalQueryPlan {
    pub fn validate(&self) -> Result<(), QueryPlanError> {
        validate_plan(self)
    }

    pub fn finalize_metadata_probe(mut self, matched: bool) -> Self {
        if self.metadata_probe.take().is_some() && matched {
            promote_identifier_candidate(&mut self);
        }
        self
    }

    pub fn finalize_evidence(
        mut self,
        report: QueryEvidenceReport,
    ) -> Result<Self, QueryPlanError> {
        self.validate()?;
        if report.schema_version != LEXICAL_QUERY_PLAN_SCHEMA_VERSION {
            return Err(invalid_plan(
                "evidence report schema version is unsupported",
            ));
        }

        if self.assistance == QueryAssistanceEligibility::ExplicitSyntaxBypass {
            if report.identifier_probe_matched.is_some() || !report.term_support.is_empty() {
                return Err(invalid_plan("explicit query evidence report must be empty"));
            }
            return Ok(self);
        }

        match self.metadata_probe.take() {
            Some(_) => {
                let matched = report.identifier_probe_matched.ok_or_else(|| {
                    invalid_plan("identifier metadata probe observation is required")
                })?;
                if matched {
                    promote_identifier_candidate(&mut self);
                }
            }
            None if report.identifier_probe_matched.is_some() => {
                return Err(invalid_plan(
                    "unexpected identifier metadata probe observation",
                ));
            }
            None => {}
        }

        if report.term_support.len() != self.support_probes.len() {
            return Err(invalid_plan(
                "term support observations must exactly match requested probes",
            ));
        }
        for ((probe, observation), intent) in self
            .support_probes
            .iter()
            .zip(&report.term_support)
            .zip(&mut self.term_intents)
        {
            if observation.probe_id != probe.probe_id
                || observation.term_index != probe.term_index
                || intent.index != probe.term_index
                || intent.text != probe.term
            {
                return Err(invalid_plan(
                    "term support observation does not match its requested probe",
                ));
            }
            if observation.prefix_expansions > self.bounds.max_prefix_expansions_per_term {
                return Err(invalid_plan(
                    "prefix expansion observation exceeds its bound",
                ));
            }
            intent.support = if observation.document_frequency > 0 {
                QueryTermSupport::Useful
            } else {
                QueryTermSupport::Unsupported
            };
        }

        self.support_probes.clear();
        self.evidence_stages.clear();

        if self.term_intents.iter().any(|intent| {
            intent.role == QueryTermRole::RequiredIdentifierAnchor
                && intent.support != QueryTermSupport::Useful
        }) {
            self.execution = QueryExecutionDisposition::EmptyNoEvidence;
            self.validate()?;
            return Ok(self);
        }

        let useful_indexes: Vec<_> = self
            .term_intents
            .iter()
            .filter(|intent| intent.support == QueryTermSupport::Useful)
            .map(|intent| intent.index)
            .collect();
        let prefix_indexes: Vec<_> = self
            .term_intents
            .iter()
            .zip(&report.term_support)
            .filter(|(intent, observation)| {
                // Support is deliberately not consulted. A term whose exact
                // form exists somewhere in the vault can still be an
                // abbreviation of what the reader meant, and the stem's own
                // token is one of its expansions, so nothing is lost by
                // offering the wider disjunction.
                intent.role == QueryTermRole::OptionalContext
                    && intent.text.chars().count() >= self.bounds.min_prefix_chars
                    && observation.prefix_expansions > 0
            })
            .take(self.bounds.max_prefix_terms)
            .map(|(intent, _)| intent.index)
            .collect();

        if useful_indexes.is_empty() && prefix_indexes.is_empty() {
            self.execution = QueryExecutionDisposition::EmptyNoEvidence;
            self.validate()?;
            return Ok(self);
        }

        let all_indexes: Vec<_> = self
            .term_intents
            .iter()
            .map(|intent| intent.index)
            .collect();
        if self.exact_intent.is_some() {
            push_stage(
                &mut self.evidence_stages,
                QueryEvidenceStageKind::ExactMetadata,
                QueryFieldGroup::Exact,
                Vec::new(),
                Vec::new(),
                self.bounds.max_candidates_per_stage,
            );
        }
        if self.phrase_intent.is_some() {
            push_stage(
                &mut self.evidence_stages,
                QueryEvidenceStageKind::ExactPhrase,
                QueryFieldGroup::Phrase,
                Vec::new(),
                Vec::new(),
                self.bounds.max_candidates_per_stage,
            );
        }
        let has_unsupported_context = self.term_intents.iter().any(|intent| {
            intent.role == QueryTermRole::OptionalContext
                && intent.support == QueryTermSupport::Unsupported
        });
        let partial_indexes: Vec<_> = self
            .term_intents
            .iter()
            .filter(|intent| {
                intent.role == QueryTermRole::RequiredIdentifierAnchor
                    || intent.support == QueryTermSupport::Useful
            })
            .take(self.bounds.max_partial_coverage_terms)
            .map(|intent| intent.index)
            .collect();
        // Name evidence outranks body evidence: a note whose filename, stem,
        // alias or title carries the words the typed stems abbreviate is a
        // better answer than one that merely mentions those stems in prose.
        // Stages fill the window in order, so this ordering is what makes the
        // precedence real rather than a scoring preference.
        let prefix_required = prefix_stage_required_indexes(&partial_indexes, &prefix_indexes);
        if !prefix_indexes.is_empty() {
            push_stage(
                &mut self.evidence_stages,
                QueryEvidenceStageKind::PrefixMetadata,
                QueryFieldGroup::PrefixMetadata,
                prefix_required.clone(),
                prefix_indexes.clone(),
                self.bounds.max_candidates_per_stage,
            );
        }
        push_stage(
            &mut self.evidence_stages,
            QueryEvidenceStageKind::AllTerms,
            QueryFieldGroup::SearchableText,
            all_indexes.clone(),
            Vec::new(),
            self.bounds.max_candidates_per_stage,
        );
        if !prefix_indexes.is_empty() {
            // A prefix term may also be independently supported, and a stage
            // may not both require a term exactly and expand it. The expansion
            // carries the term, so the exact form is dropped from the
            // requirement rather than the other way round.
            push_stage(
                &mut self.evidence_stages,
                QueryEvidenceStageKind::Prefix,
                QueryFieldGroup::Prefix,
                prefix_required,
                prefix_indexes,
                self.bounds.max_candidates_per_stage,
            );
        }
        if has_unsupported_context && !partial_indexes.is_empty() && partial_indexes != all_indexes
        {
            push_stage(
                &mut self.evidence_stages,
                QueryEvidenceStageKind::PartialCoverage,
                QueryFieldGroup::SearchableText,
                partial_indexes,
                Vec::new(),
                self.bounds.max_candidates_per_stage,
            );
        }

        self.execution = QueryExecutionDisposition::Ready;
        self.validate()?;
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct QueryPlanError {
    pub code: String,
    pub message: String,
}

pub fn prepare_lexical_query(query: &str) -> Result<LexicalQueryPlan, QueryPlanError> {
    if query.trim().is_empty() {
        return Err(invalid_query("query must not be empty"));
    }
    if query.len() > MAX_QUERY_BYTES {
        return Err(invalid_query(&format!(
            "query must not exceed {MAX_QUERY_BYTES} UTF-8 bytes"
        )));
    }
    if query.split_whitespace().take(MAX_QUERY_TERMS + 1).count() > MAX_QUERY_TERMS {
        return Err(invalid_query(&format!(
            "query must not exceed {MAX_QUERY_TERMS} terms"
        )));
    }

    let kind = classify_query(query);
    let assistance = if kind == QueryPlanKind::Explicit {
        QueryAssistanceEligibility::ExplicitSyntaxBypass
    } else {
        QueryAssistanceEligibility::Eligible
    };
    let metadata_probe = (kind == QueryPlanKind::Ordinary
        && is_lowercase_identifier_candidate(query))
    .then(|| QueryMetadataProbe {
        query: query.to_owned(),
        fields: vec![
            QueryMetadataField::Filename,
            QueryMetadataField::Stem,
            QueryMetadataField::Aliases,
            QueryMetadataField::Title,
            QueryMetadataField::Heading,
            QueryMetadataField::Tags,
        ],
        conjunction: true,
    });
    let match_operator = match kind {
        QueryPlanKind::Explicit => QueryMatchOperator::Explicit,
        QueryPlanKind::Ordinary => QueryMatchOperator::Any,
        QueryPlanKind::Identifier => QueryMatchOperator::All,
    };
    let term_intents = if kind == QueryPlanKind::Explicit {
        Vec::new()
    } else {
        query_term_intents(query, kind)
    };
    let terms = term_intents
        .iter()
        .map(|intent| intent.text.clone())
        .collect::<Vec<_>>();
    let support_probes: Vec<_> = term_intents
        .iter()
        .map(|intent| QueryTermSupportProbe {
            probe_id: intent.index,
            term_index: intent.index,
            term: intent.text.clone(),
            field_group: QueryFieldGroup::SearchableText,
        })
        .collect();
    if support_probes.len() > MAX_TERM_SUPPORT_PROBES {
        return Err(invalid_query(&format!(
            "query must not exceed {MAX_TERM_SUPPORT_PROBES} evidence terms"
        )));
    }

    let normalized_exact = (assistance == QueryAssistanceEligibility::Eligible)
        .then(|| normalize_raw(query))
        .flatten();
    let exact_intent = normalized_exact
        .as_ref()
        .map(|normalized| QueryExactIntent {
            normalized: normalized.clone(),
            field_group: QueryFieldGroup::Exact,
        });
    let phrase_boost = assistance == QueryAssistanceEligibility::Eligible && terms.len() > 1;
    let phrase_intent = phrase_boost.then(|| QueryPhraseIntent {
        terms: terms.clone(),
        field_group: QueryFieldGroup::Phrase,
    });
    let execution = match assistance {
        QueryAssistanceEligibility::ExplicitSyntaxBypass => {
            QueryExecutionDisposition::ExplicitBypass
        }
        QueryAssistanceEligibility::Eligible if terms.is_empty() => {
            QueryExecutionDisposition::EmptyNoEvidence
        }
        QueryAssistanceEligibility::Eligible => QueryExecutionDisposition::AwaitingEvidence,
    };

    let plan = LexicalQueryPlan {
        schema_version: LEXICAL_QUERY_PLAN_SCHEMA_VERSION,
        query: query.to_owned(),
        kind,
        match_operator,
        assistance,
        execution,
        terms,
        term_intents,
        normalized_exact,
        exact_intent,
        phrase_boost,
        phrase_intent,
        field_groups: QueryFieldGroups::lexical_v1(),
        bounds: QueryBounds::lexical_v1(),
        typo_stage: QueryTypoStage::Disabled,
        support_probes,
        evidence_stages: Vec::new(),
        metadata_probe,
    };
    plan.validate()?;
    Ok(plan)
}

fn validate_plan(plan: &LexicalQueryPlan) -> Result<(), QueryPlanError> {
    if plan.schema_version != LEXICAL_QUERY_PLAN_SCHEMA_VERSION {
        return Err(invalid_plan("query plan schema version is unsupported"));
    }
    if plan.query.trim().is_empty()
        || plan.query.len() > MAX_QUERY_BYTES
        || plan
            .query
            .split_whitespace()
            .take(MAX_QUERY_TERMS + 1)
            .count()
            > MAX_QUERY_TERMS
    {
        return Err(invalid_plan("query plan contains an invalid query"));
    }
    if plan.field_groups != QueryFieldGroups::lexical_v1() {
        return Err(invalid_plan("query plan field groups are not recognized"));
    }
    if plan.bounds != QueryBounds::lexical_v1() {
        return Err(invalid_plan("query plan bounds are not recognized"));
    }
    if plan.typo_stage != QueryTypoStage::Disabled {
        return Err(invalid_plan("query plan typo stage must remain disabled"));
    }

    let expected_operator = match plan.kind {
        QueryPlanKind::Explicit => QueryMatchOperator::Explicit,
        QueryPlanKind::Ordinary => QueryMatchOperator::Any,
        QueryPlanKind::Identifier => QueryMatchOperator::All,
    };
    if plan.match_operator != expected_operator {
        return Err(invalid_plan(
            "query plan kind and match operator do not agree",
        ));
    }

    if plan.kind == QueryPlanKind::Explicit {
        if plan.assistance != QueryAssistanceEligibility::ExplicitSyntaxBypass
            || plan.execution != QueryExecutionDisposition::ExplicitBypass
            || !plan.terms.is_empty()
            || !plan.term_intents.is_empty()
            || plan.normalized_exact.is_some()
            || plan.exact_intent.is_some()
            || plan.phrase_boost
            || plan.phrase_intent.is_some()
            || !plan.support_probes.is_empty()
            || !plan.evidence_stages.is_empty()
            || plan.metadata_probe.is_some()
        {
            return Err(invalid_plan(
                "explicit query plan must bypass every assistance stage",
            ));
        }
        return Ok(());
    }

    if plan.assistance != QueryAssistanceEligibility::Eligible {
        return Err(invalid_plan("assisted query is not marked eligible"));
    }
    if plan.terms.len() != plan.term_intents.len()
        || plan.term_intents.len() > plan.bounds.max_term_support_probes
    {
        return Err(invalid_plan("query terms and term intents do not agree"));
    }
    for (index, (term, intent)) in plan.terms.iter().zip(&plan.term_intents).enumerate() {
        if intent.index != index as u16 || intent.text != *term {
            return Err(invalid_plan("query term intent is not canonical"));
        }
        if intent.projection == QueryTermProjection::ExactIdentifier
            && (intent.role != QueryTermRole::RequiredIdentifierAnchor
                || !technical_identifiers(&intent.text)
                    .iter()
                    .any(|identifier| identifier == &intent.text))
        {
            return Err(invalid_plan(
                "exact identifier term intent is not a complete recognized identity",
            ));
        }
    }
    if plan.kind == QueryPlanKind::Identifier
        && !plan
            .term_intents
            .iter()
            .any(|intent| intent.role == QueryTermRole::RequiredIdentifierAnchor)
    {
        return Err(invalid_plan(
            "identifier query must declare at least one required anchor",
        ));
    }
    if plan.normalized_exact.as_deref()
        != plan
            .exact_intent
            .as_ref()
            .map(|intent| intent.normalized.as_str())
        || plan
            .exact_intent
            .as_ref()
            .is_some_and(|intent| intent.field_group != QueryFieldGroup::Exact)
    {
        return Err(invalid_plan("exact query intent is inconsistent"));
    }
    if plan.phrase_boost != plan.phrase_intent.is_some()
        || plan.phrase_intent.as_ref().is_some_and(|intent| {
            intent.terms != plan.terms
                || intent.terms.len() < 2
                || intent.field_group != QueryFieldGroup::Phrase
        })
    {
        return Err(invalid_plan("phrase query intent is inconsistent"));
    }

    match plan.execution {
        QueryExecutionDisposition::ExplicitBypass => {
            return Err(invalid_plan(
                "assisted query cannot use explicit execution disposition",
            ));
        }
        QueryExecutionDisposition::AwaitingEvidence => {
            if plan.terms.is_empty()
                || !plan.evidence_stages.is_empty()
                || plan.support_probes.len() != plan.term_intents.len()
                || plan
                    .term_intents
                    .iter()
                    .any(|intent| intent.support != QueryTermSupport::Unknown)
            {
                return Err(invalid_plan("awaiting-evidence query plan is inconsistent"));
            }
            for (index, probe) in plan.support_probes.iter().enumerate() {
                if probe.probe_id != index as u16
                    || probe.term_index != index as u16
                    || probe.term != plan.terms[index]
                    || probe.field_group != QueryFieldGroup::SearchableText
                {
                    return Err(invalid_plan("term support probe is not canonical"));
                }
            }
        }
        QueryExecutionDisposition::Ready => {
            if plan.evidence_stages.is_empty()
                || !plan.support_probes.is_empty()
                || plan
                    .term_intents
                    .iter()
                    .any(|intent| intent.support == QueryTermSupport::Unknown)
            {
                return Err(invalid_plan("ready query plan is inconsistent"));
            }
            validate_stages(plan)?;
        }
        QueryExecutionDisposition::EmptyNoEvidence => {
            if !plan.evidence_stages.is_empty() || !plan.support_probes.is_empty() {
                return Err(invalid_plan("empty query plan must not be executable"));
            }
            if !plan.terms.is_empty()
                && plan
                    .term_intents
                    .iter()
                    .any(|intent| intent.support == QueryTermSupport::Unknown)
            {
                return Err(invalid_plan("empty query plan has unresolved evidence"));
            }
        }
    }
    Ok(())
}

fn validate_stages(plan: &LexicalQueryPlan) -> Result<(), QueryPlanError> {
    if plan.evidence_stages.len() > plan.bounds.max_evidence_stages {
        return Err(invalid_plan("query plan has too many evidence stages"));
    }
    let all_indexes: Vec<_> = plan
        .term_intents
        .iter()
        .map(|intent| intent.index)
        .collect();
    let anchor_indexes: BTreeSet<_> = plan
        .term_intents
        .iter()
        .filter(|intent| intent.role == QueryTermRole::RequiredIdentifierAnchor)
        .map(|intent| intent.index)
        .collect();
    let relaxed_indexes: Vec<_> = plan
        .term_intents
        .iter()
        .filter(|intent| {
            intent.role == QueryTermRole::RequiredIdentifierAnchor
                || intent.support == QueryTermSupport::Useful
        })
        .take(plan.bounds.max_partial_coverage_terms)
        .map(|intent| intent.index)
        .collect();
    let has_unsupported_context = plan.term_intents.iter().any(|intent| {
        intent.role == QueryTermRole::OptionalContext
            && intent.support == QueryTermSupport::Unsupported
    });
    let has_prefix = plan
        .evidence_stages
        .iter()
        .any(|stage| stage.kind == QueryEvidenceStageKind::Prefix);
    let mut expected_kinds = Vec::new();
    if plan.exact_intent.is_some() {
        expected_kinds.push(QueryEvidenceStageKind::ExactMetadata);
    }
    if plan.phrase_intent.is_some() {
        expected_kinds.push(QueryEvidenceStageKind::ExactPhrase);
    }
    // Bounded prefix evidence is always emitted as a pair straddling
    // `AllTerms`: the metadata half above it, the searchable-text half below.
    // A plan carrying only one half is rejected by the sequence comparison
    // below rather than silently losing name precedence.
    if has_prefix {
        expected_kinds.push(QueryEvidenceStageKind::PrefixMetadata);
    }
    expected_kinds.push(QueryEvidenceStageKind::AllTerms);
    if has_prefix {
        expected_kinds.push(QueryEvidenceStageKind::Prefix);
    }
    if has_unsupported_context && !relaxed_indexes.is_empty() && relaxed_indexes != all_indexes {
        expected_kinds.push(QueryEvidenceStageKind::PartialCoverage);
    }
    let actual_kinds: Vec<_> = plan
        .evidence_stages
        .iter()
        .map(|stage| stage.kind)
        .collect();
    if actual_kinds != expected_kinds {
        return Err(invalid_plan(
            "query evidence stages do not preserve the required evidence ladder",
        ));
    }
    let mut previous_kind = None;
    for (ordinal, stage) in plan.evidence_stages.iter().enumerate() {
        if stage.ordinal != ordinal as u8
            || stage.max_candidates == 0
            || stage.max_candidates > plan.bounds.max_candidates_per_stage
            || previous_kind.is_some_and(|kind| kind >= stage.kind)
        {
            return Err(invalid_plan(
                "query evidence stage order or bound is invalid",
            ));
        }
        previous_kind = Some(stage.kind);
        if stage
            .required_term_indexes
            .iter()
            .chain(&stage.prefix_term_indexes)
            .any(|index| !all_indexes.contains(index))
        {
            return Err(invalid_plan(
                "query evidence stage references an unknown term",
            ));
        }
        if stage
            .required_term_indexes
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
            || stage
                .prefix_term_indexes
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || stage
                .prefix_term_indexes
                .iter()
                .any(|index| stage.required_term_indexes.contains(index))
        {
            return Err(invalid_plan(
                "query evidence stage term indexes are not canonical",
            ));
        }
        match stage.kind {
            QueryEvidenceStageKind::ExactMetadata => {
                if plan.exact_intent.is_none()
                    || stage.field_group != QueryFieldGroup::Exact
                    || !stage.required_term_indexes.is_empty()
                    || !stage.prefix_term_indexes.is_empty()
                {
                    return Err(invalid_plan("exact metadata stage is inconsistent"));
                }
            }
            QueryEvidenceStageKind::ExactPhrase => {
                if plan.phrase_intent.is_none()
                    || stage.field_group != QueryFieldGroup::Phrase
                    || !stage.required_term_indexes.is_empty()
                    || !stage.prefix_term_indexes.is_empty()
                {
                    return Err(invalid_plan("exact phrase stage is inconsistent"));
                }
            }
            QueryEvidenceStageKind::AllTerms => {
                if stage.field_group != QueryFieldGroup::SearchableText
                    || stage.required_term_indexes != all_indexes
                    || !stage.prefix_term_indexes.is_empty()
                {
                    return Err(invalid_plan("all-terms stage is inconsistent"));
                }
            }
            QueryEvidenceStageKind::PartialCoverage => {
                if stage.field_group != QueryFieldGroup::SearchableText
                    || stage.required_term_indexes != relaxed_indexes
                    || stage.required_term_indexes.is_empty()
                    || stage.required_term_indexes == all_indexes
                    || stage.required_term_indexes.len() > plan.bounds.max_partial_coverage_terms
                    || !stage.prefix_term_indexes.is_empty()
                    || !anchor_indexes
                        .iter()
                        .all(|anchor| stage.required_term_indexes.contains(anchor))
                {
                    return Err(invalid_plan("partial-coverage stage is inconsistent"));
                }
            }
            QueryEvidenceStageKind::PrefixMetadata | QueryEvidenceStageKind::Prefix => {
                let expected_group = if stage.kind == QueryEvidenceStageKind::PrefixMetadata {
                    QueryFieldGroup::PrefixMetadata
                } else {
                    QueryFieldGroup::Prefix
                };
                // An expanded term is never also required exactly, so the
                // requirement is the relaxed set minus this stage's own prefix
                // terms. Identifier anchors are never expanded, so they always
                // survive that subtraction and are asserted below.
                let expected_required =
                    prefix_stage_required_indexes(&relaxed_indexes, &stage.prefix_term_indexes);
                if stage.field_group != expected_group
                    || stage.required_term_indexes != expected_required
                    || stage.prefix_term_indexes.is_empty()
                    || stage.prefix_term_indexes.len() > plan.bounds.max_prefix_terms
                    || !anchor_indexes
                        .iter()
                        .all(|anchor| stage.required_term_indexes.contains(anchor))
                    || stage.prefix_term_indexes.iter().any(|index| {
                        plan.term_intents
                            .get(*index as usize)
                            .is_none_or(|intent| intent.role != QueryTermRole::OptionalContext)
                    })
                {
                    return Err(invalid_plan("prefix stage is inconsistent"));
                }
            }
        }
    }
    Ok(())
}

/// The relaxed requirement of a prefix stage: every term the stage would
/// otherwise require, minus the ones it expands.
///
/// Shared by construction and validation so the two cannot disagree about a
/// plan's canonical shape.
fn prefix_stage_required_indexes(relaxed: &[u16], prefix: &[u16]) -> Vec<u16> {
    relaxed
        .iter()
        .copied()
        .filter(|index| !prefix.contains(index))
        .collect()
}

fn push_stage(
    stages: &mut Vec<QueryEvidenceStage>,
    kind: QueryEvidenceStageKind,
    field_group: QueryFieldGroup,
    required_term_indexes: Vec<u16>,
    prefix_term_indexes: Vec<u16>,
    max_candidates: usize,
) {
    stages.push(QueryEvidenceStage {
        ordinal: stages.len() as u8,
        kind,
        field_group,
        required_term_indexes,
        prefix_term_indexes,
        max_candidates,
    });
}

fn promote_identifier_candidate(plan: &mut LexicalQueryPlan) {
    plan.kind = QueryPlanKind::Identifier;
    plan.match_operator = QueryMatchOperator::All;
    for index in lowercase_identifier_anchor_indexes(&plan.query, &plan.terms) {
        plan.term_intents[index as usize].role = QueryTermRole::RequiredIdentifierAnchor;
    }
}

fn invalid_query(message: &str) -> QueryPlanError {
    QueryPlanError {
        code: "invalid_query".to_owned(),
        message: message.to_owned(),
    }
}

fn invalid_plan(message: &str) -> QueryPlanError {
    QueryPlanError {
        code: "invalid_query_plan".to_owned(),
        message: message.to_owned(),
    }
}

pub(crate) fn classify_query(query: &str) -> QueryPlanKind {
    if has_explicit_syntax(query) {
        QueryPlanKind::Explicit
    } else if is_standalone_technical_identifier(query) || is_identifier_like(query) {
        QueryPlanKind::Identifier
    } else {
        QueryPlanKind::Ordinary
    }
}

pub(crate) fn is_lowercase_identifier_candidate(query: &str) -> bool {
    lowercase_identifier_components(query).is_some()
}

fn lowercase_identifier_components(query: &str) -> Option<(Vec<&str>, usize)> {
    const ORDINARY_NUMBER_PREFIXES: &[&str] = &[
        "best", "chapter", "episode", "first", "last", "level", "page", "part", "section", "step",
        "top", "volume",
    ];

    let tokens: Vec<_> = query.split_whitespace().collect();
    if tokens.is_empty() || tokens.len() > 6 {
        return None;
    }
    let number_index = tokens
        .iter()
        .position(|token| token.chars().all(|character| character.is_ascii_digit()))?;
    let has_candidate = tokens[..number_index].iter().any(|token| {
        (2..=4).contains(&token.chars().count())
            && token.chars().all(|character| character.is_alphabetic())
            && !ORDINARY_NUMBER_PREFIXES.contains(&token.to_ascii_lowercase().as_str())
    });
    has_candidate.then_some((tokens, number_index))
}

fn has_explicit_syntax(query: &str) -> bool {
    let boolean_syntax = query
        .split_whitespace()
        .any(|token| matches!(token, "AND" | "OR" | "NOT"));
    if boolean_syntax
        || query
            .chars()
            .any(|character| matches!(character, '"' | '[' | ']' | '{' | '}' | '^' | '~' | '*'))
        || query.contains(':')
        || query
            .split_whitespace()
            .any(|token| token.starts_with('+') || token.starts_with('-'))
    {
        return true;
    }

    let trimmed = query.trim_end();
    let natural_terminal_question = trimmed.ends_with('?')
        && trimmed[..trimmed.len() - 1].contains(char::is_whitespace)
        && !trimmed[..trimmed.len() - 1].contains('?');
    if query.contains('?') && !natural_terminal_question {
        return true;
    }

    false
}

fn is_standalone_technical_identifier(query: &str) -> bool {
    let trimmed = query.trim();
    normalize_raw(trimmed).is_some_and(|normalized| {
        technical_identifiers(trimmed)
            .into_iter()
            .any(|identifier| identifier == normalized)
    })
}

fn is_identifier_like(query: &str) -> bool {
    let tokens: Vec<_> = query.split_whitespace().collect();
    if tokens.is_empty() || tokens.len() > 6 {
        return false;
    }
    if !technical_identifiers(query).is_empty() {
        return true;
    }
    let has_number = tokens
        .iter()
        .any(|token| token.chars().all(|character| character.is_ascii_digit()));
    let has_acronym = tokens.iter().any(|token| {
        (2..=8).contains(&token.chars().count())
            && token.chars().all(|character| character.is_alphabetic())
            && token.chars().any(|character| character.is_uppercase())
            && token.chars().all(|character| !character.is_lowercase())
    });
    let has_mixed_alphanumeric = tokens.iter().any(|token| {
        token.chars().any(char::is_alphabetic)
            && token.chars().any(|character| character.is_ascii_digit())
    });
    has_mixed_alphanumeric || (has_number && has_acronym)
}

fn query_term_intents(query: &str, kind: QueryPlanKind) -> Vec<QueryTermIntent> {
    let mut seen = BTreeSet::new();
    let mut planned = Vec::new();
    let mut cursor = 0;
    for (range, identifier) in technical_identifier_spans(query) {
        push_analyzed_terms(&query[cursor..range.start], &mut seen, &mut planned);
        let key = (QueryTermProjection::ExactIdentifier, identifier.clone());
        if seen.insert(key) {
            planned.push((identifier, QueryTermProjection::ExactIdentifier));
        }
        cursor = range.end;
    }
    push_analyzed_terms(&query[cursor..], &mut seen, &mut planned);

    let mut fallback_anchors = BTreeSet::new();
    if kind == QueryPlanKind::Identifier {
        for token in query.split_whitespace() {
            let is_mixed = token.chars().any(char::is_alphabetic)
                && token.chars().any(|character| character.is_ascii_digit());
            let is_acronym = (2..=8).contains(&token.chars().count())
                && token.chars().all(|character| character.is_alphabetic())
                && token.chars().all(|character| !character.is_lowercase());
            let is_number = token.chars().all(|character| character.is_ascii_digit());
            if (is_mixed || is_acronym || is_number)
                && let Some(normalized) = normalize_token(token)
            {
                fallback_anchors.insert(normalized);
            }
        }
    }

    planned
        .into_iter()
        .enumerate()
        .map(|(index, (text, projection))| QueryTermIntent {
            index: index as u16,
            role: if projection == QueryTermProjection::ExactIdentifier
                || fallback_anchors.contains(&text)
            {
                QueryTermRole::RequiredIdentifierAnchor
            } else {
                QueryTermRole::OptionalContext
            },
            text,
            projection,
            support: QueryTermSupport::Unknown,
        })
        .collect()
}

fn push_analyzed_terms(
    source: &str,
    seen: &mut BTreeSet<(QueryTermProjection, String)>,
    planned: &mut Vec<(String, QueryTermProjection)>,
) {
    for authored in source.split_whitespace() {
        let authored = authored.trim_matches(|character: char| !character.is_alphanumeric());
        let Some(folded) = normalize_raw(authored) else {
            continue;
        };
        for component in
            folded.split(|character: char| !character.is_alphanumeric() && character != '_')
        {
            let Some(normalized) = normalize_token(component) else {
                continue;
            };
            let key = (QueryTermProjection::AnalyzedText, normalized.clone());
            if seen.insert(key) {
                planned.push((normalized, QueryTermProjection::AnalyzedText));
            }
        }
    }
}

fn lowercase_identifier_anchor_indexes(query: &str, terms: &[String]) -> BTreeSet<u16> {
    let Some((tokens, number_index)) = lowercase_identifier_components(query) else {
        return BTreeSet::new();
    };
    let mut anchors = BTreeSet::new();
    for token in &tokens[..number_index] {
        if (2..=4).contains(&token.chars().count())
            && token.chars().all(|character| character.is_alphabetic())
            && let Some(normalized) = normalize_token(token)
        {
            anchors.insert(normalized);
        }
    }
    if let Some(normalized) = normalize_token(tokens[number_index]) {
        anchors.insert(normalized);
    }
    terms
        .iter()
        .enumerate()
        .filter(|(_, term)| anchors.contains(*term))
        .map(|(index, _)| index as u16)
        .collect()
}

fn normalize_token(term: &str) -> Option<String> {
    let term = term.trim_matches(|character: char| !character.is_alphanumeric());
    normalize_raw(term)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence_report(
        plan: &LexicalQueryPlan,
        identifier_probe_matched: Option<bool>,
        evidence: &[(u64, usize)],
    ) -> QueryEvidenceReport {
        assert_eq!(plan.support_probes.len(), evidence.len());
        QueryEvidenceReport {
            schema_version: LEXICAL_QUERY_PLAN_SCHEMA_VERSION,
            identifier_probe_matched,
            term_support: plan
                .support_probes
                .iter()
                .zip(evidence)
                .map(|(probe, (document_frequency, prefix_expansions))| {
                    QueryTermSupportObservation {
                        probe_id: probe.probe_id,
                        term_index: probe.term_index,
                        document_frequency: *document_frequency,
                        prefix_expansions: *prefix_expansions,
                    }
                })
                .collect(),
        }
    }

    fn stage_kinds(plan: &LexicalQueryPlan) -> Vec<QueryEvidenceStageKind> {
        plan.evidence_stages
            .iter()
            .map(|stage| stage.kind)
            .collect()
    }

    #[test]
    fn classifier_is_conservative_and_preserves_explicit_syntax() {
        assert_eq!(classify_query("IIA 2 line"), QueryPlanKind::Identifier);
        assert_eq!(classify_query("iia 2 line"), QueryPlanKind::Ordinary);
        assert_eq!(
            classify_query("RFC 9110 caching"),
            QueryPlanKind::Identifier
        );
        assert_eq!(classify_query("CVE-2026-1234"), QueryPlanKind::Identifier);
        assert_eq!(
            classify_query("dungeons and dragons"),
            QueryPlanKind::Ordinary
        );
        assert_eq!(classify_query("top 10 books"), QueryPlanKind::Ordinary);
        assert_eq!(classify_query("\"IIA 2 line\""), QueryPlanKind::Explicit);
        assert_eq!(classify_query("IIA OR line"), QueryPlanKind::Explicit);
        assert_eq!(classify_query("title:IIA"), QueryPlanKind::Explicit);
        assert_eq!(classify_query("title:2026"), QueryPlanKind::Explicit);
        assert_eq!(classify_query("title : IIA"), QueryPlanKind::Explicit);
        assert_eq!(classify_query("bogus:IIA"), QueryPlanKind::Explicit);
        assert_eq!(classify_query("CVE-*"), QueryPlanKind::Explicit);
        assert_eq!(classify_query("cache governance?"), QueryPlanKind::Ordinary);
        assert_eq!(
            classify_query("cache governance (draft)"),
            QueryPlanKind::Ordinary
        );
        assert_eq!(classify_query("alph?"), QueryPlanKind::Explicit);
    }

    #[test]
    fn ordinary_punctuation_and_duplicates_have_backend_neutral_term_boundaries() {
        let comma = prepare_lexical_query("alpha,beta alpha").unwrap();
        assert_eq!(comma.terms, ["alpha", "beta"]);
        assert_eq!(comma.support_probes.len(), 2);

        let parenthetical = prepare_lexical_query("cache governance (draft)").unwrap();
        assert_eq!(parenthetical.terms, ["cache", "governance", "draft"]);
        assert_eq!(
            parenthetical.assistance,
            QueryAssistanceEligibility::Eligible
        );
    }

    #[test]
    fn ordinary_unsupported_filler_relaxes_only_after_all_terms() {
        let prepared = prepare_lexical_query("cache unfindablefiller").unwrap();
        let report = evidence_report(&prepared, None, &[(8, 0), (0, 0)]);
        let finalized = prepared.finalize_evidence(report).unwrap();

        assert_eq!(finalized.execution, QueryExecutionDisposition::Ready);
        assert_eq!(
            stage_kinds(&finalized),
            [
                QueryEvidenceStageKind::ExactMetadata,
                QueryEvidenceStageKind::ExactPhrase,
                QueryEvidenceStageKind::AllTerms,
                QueryEvidenceStageKind::PartialCoverage,
            ]
        );
        assert_eq!(finalized.evidence_stages[2].required_term_indexes, [0, 1]);
        assert_eq!(finalized.evidence_stages[3].required_term_indexes, [0]);
    }

    #[test]
    fn all_supported_ordinary_terms_do_not_enable_partial_coverage() {
        let prepared = prepare_lexical_query("dungeons dragons").unwrap();
        let report = evidence_report(&prepared, None, &[(3, 0), (4, 0)]);
        let finalized = prepared.finalize_evidence(report).unwrap();

        assert_eq!(
            stage_kinds(&finalized),
            [
                QueryEvidenceStageKind::ExactMetadata,
                QueryEvidenceStageKind::ExactPhrase,
                QueryEvidenceStageKind::AllTerms,
            ]
        );
        assert!(finalized.term_intents.iter().all(|intent| {
            intent.role == QueryTermRole::OptionalContext
                && intent.support == QueryTermSupport::Useful
        }));
    }

    #[test]
    fn identifier_anchors_remain_required_while_context_can_relax() {
        let prepared = prepare_lexical_query("RFC 9110 caching").unwrap();
        assert_eq!(prepared.kind, QueryPlanKind::Identifier);
        assert_eq!(prepared.terms, ["rfc 9110", "caching"]);
        assert_eq!(
            prepared
                .term_intents
                .iter()
                .map(|intent| (intent.role, intent.projection))
                .collect::<Vec<_>>(),
            [
                (
                    QueryTermRole::RequiredIdentifierAnchor,
                    QueryTermProjection::ExactIdentifier,
                ),
                (
                    QueryTermRole::OptionalContext,
                    QueryTermProjection::AnalyzedText,
                ),
            ]
        );

        let report = evidence_report(&prepared, None, &[(3, 0), (0, 0)]);
        let finalized = prepared.clone().finalize_evidence(report).unwrap();
        let partial = finalized
            .evidence_stages
            .iter()
            .find(|stage| stage.kind == QueryEvidenceStageKind::PartialCoverage)
            .unwrap();
        assert_eq!(partial.required_term_indexes, [0]);

        let missing_anchor = evidence_report(&prepared, None, &[(0, 4), (9, 0)]);
        let empty = prepared.finalize_evidence(missing_anchor).unwrap();
        assert_eq!(empty.execution, QueryExecutionDisposition::EmptyNoEvidence);
        assert!(empty.evidence_stages.is_empty());
    }

    #[test]
    fn lowercase_metadata_promotion_declares_required_anchors() {
        let prepared = prepare_lexical_query("iia 2 line").unwrap();
        assert!(prepared.metadata_probe.is_some());
        let report = evidence_report(&prepared, Some(true), &[(4, 0), (4, 0), (0, 0)]);
        let finalized = prepared.finalize_evidence(report).unwrap();

        assert_eq!(finalized.kind, QueryPlanKind::Identifier);
        assert_eq!(finalized.match_operator, QueryMatchOperator::All);
        assert_eq!(
            finalized
                .term_intents
                .iter()
                .map(|intent| intent.role)
                .collect::<Vec<_>>(),
            [
                QueryTermRole::RequiredIdentifierAnchor,
                QueryTermRole::RequiredIdentifierAnchor,
                QueryTermRole::OptionalContext,
            ]
        );
    }

    #[test]
    fn assisted_plan_carries_exact_phrase_fields_bounds_and_disabled_typo_intent() {
        let plan = prepare_lexical_query("Résumé Cache").unwrap();
        assert_eq!(plan.normalized_exact.as_deref(), Some("resume cache"));
        assert_eq!(
            plan.exact_intent.as_ref().unwrap().field_group,
            QueryFieldGroup::Exact
        );
        assert_eq!(
            plan.phrase_intent.as_ref().unwrap().terms,
            ["resume", "cache"]
        );
        assert_eq!(plan.field_groups, QueryFieldGroups::lexical_v1());
        assert_eq!(plan.bounds, QueryBounds::lexical_v1());
        assert_eq!(plan.typo_stage, QueryTypoStage::Disabled);
    }

    #[test]
    fn every_recognized_explicit_form_bypasses_assistance_and_probes() {
        for query in [
            "title:cache",
            "cache AND policy",
            "\"cache policy\"",
            "cache*",
            "cach?",
            "(cache OR policy)",
            "year:[2020 TO 2026]",
        ] {
            let plan = prepare_lexical_query(query).unwrap();
            assert_eq!(plan.kind, QueryPlanKind::Explicit, "{query}");
            assert_eq!(
                plan.assistance,
                QueryAssistanceEligibility::ExplicitSyntaxBypass,
                "{query}"
            );
            assert_eq!(
                plan.execution,
                QueryExecutionDisposition::ExplicitBypass,
                "{query}"
            );
            assert!(plan.support_probes.is_empty(), "{query}");
            assert!(plan.evidence_stages.is_empty(), "{query}");
            assert!(plan.exact_intent.is_none(), "{query}");
            assert!(plan.phrase_intent.is_none(), "{query}");
            let finalized = plan
                .finalize_evidence(QueryEvidenceReport {
                    schema_version: LEXICAL_QUERY_PLAN_SCHEMA_VERSION,
                    identifier_probe_matched: None,
                    term_support: Vec::new(),
                })
                .unwrap();
            assert_eq!(
                finalized.execution,
                QueryExecutionDisposition::ExplicitBypass
            );
        }
    }

    #[test]
    fn empty_term_and_no_evidence_queries_emit_no_executable_stages() {
        let punctuation = prepare_lexical_query("...").unwrap();
        assert_eq!(
            punctuation.execution,
            QueryExecutionDisposition::EmptyNoEvidence
        );
        assert!(punctuation.evidence_stages.is_empty());

        let prepared = prepare_lexical_query("unfindable nowhereword").unwrap();
        let report = evidence_report(&prepared, None, &[(0, 0), (0, 0)]);
        let finalized = prepared.finalize_evidence(report).unwrap();
        assert_eq!(
            finalized.execution,
            QueryExecutionDisposition::EmptyNoEvidence
        );
        assert!(finalized.evidence_stages.is_empty());
    }

    /// A supported term may also be expanded, and then it is required by the
    /// expansion rather than exactly. Requiring both would defeat the point.
    #[test]
    fn a_supported_term_that_is_also_expanded_leaves_the_exact_requirement_behind() {
        let prepared = prepare_lexical_query("cache filler").unwrap();
        // Both terms are supported, and the second also has expansions.
        let report = evidence_report(&prepared, None, &[(5, 0), (3, 2)]);
        let finalized = prepared.finalize_evidence(report).unwrap();

        assert_eq!(
            stage_kinds(&finalized),
            [
                QueryEvidenceStageKind::ExactMetadata,
                QueryEvidenceStageKind::ExactPhrase,
                QueryEvidenceStageKind::PrefixMetadata,
                QueryEvidenceStageKind::AllTerms,
                QueryEvidenceStageKind::Prefix,
            ]
        );
        for kind in [
            QueryEvidenceStageKind::PrefixMetadata,
            QueryEvidenceStageKind::Prefix,
        ] {
            let stage = finalized
                .evidence_stages
                .iter()
                .find(|stage| stage.kind == kind)
                .expect("both prefix halves are present");
            assert_eq!(stage.prefix_term_indexes, [1]);
            // Index 1 is expanded, so it is absent from the requirement even
            // though it is Useful and would otherwise be relaxed into it.
            assert_eq!(stage.required_term_indexes, [0]);
            assert!(
                !stage
                    .required_term_indexes
                    .iter()
                    .any(|index| stage.prefix_term_indexes.contains(index))
            );
        }
        // The all-terms tier still demands both exact tokens.
        let all_terms = finalized
            .evidence_stages
            .iter()
            .find(|stage| stage.kind == QueryEvidenceStageKind::AllTerms)
            .expect("the all-terms tier is present");
        assert_eq!(all_terms.required_term_indexes, [0, 1]);

        // Putting the expanded term back into the requirement is refused.
        let mut restated = finalized.clone();
        let metadata_ordinal = restated
            .evidence_stages
            .iter()
            .position(|stage| stage.kind == QueryEvidenceStageKind::PrefixMetadata)
            .expect("the metadata half is present");
        restated.evidence_stages[metadata_ordinal].required_term_indexes = vec![0, 1];
        assert!(restated.validate().is_err());
    }

    #[test]
    fn bounded_prefix_precedes_partial_coverage_and_never_relaxes_identifier_anchors() {
        let prepared = prepare_lexical_query("cache unfindable").unwrap();
        let report = evidence_report(&prepared, None, &[(5, 0), (0, 2)]);
        let finalized = prepared.finalize_evidence(report).unwrap();
        assert_eq!(
            stage_kinds(&finalized),
            [
                QueryEvidenceStageKind::ExactMetadata,
                QueryEvidenceStageKind::ExactPhrase,
                QueryEvidenceStageKind::PrefixMetadata,
                QueryEvidenceStageKind::AllTerms,
                QueryEvidenceStageKind::Prefix,
                QueryEvidenceStageKind::PartialCoverage,
            ]
        );
        // The prefix pair now straddles the all-terms tier: name evidence above
        // it, searchable-text evidence below.
        assert_eq!(
            finalized.evidence_stages[2].field_group,
            QueryFieldGroup::PrefixMetadata
        );
        assert_eq!(
            finalized.evidence_stages[4].field_group,
            QueryFieldGroup::Prefix
        );
        // Both halves carry the same relaxed requirement and the same bounded
        // expansion terms; only the fields they may match differ.
        assert_eq!(finalized.evidence_stages[2].prefix_term_indexes, [1]);
        assert_eq!(finalized.evidence_stages[4].prefix_term_indexes, [1]);
        assert_eq!(finalized.evidence_stages[2].required_term_indexes, [0]);
        assert_eq!(finalized.evidence_stages[5].required_term_indexes, [0]);

        let mut old_order = finalized.clone();
        old_order.evidence_stages.swap(4, 5);
        for (ordinal, stage) in old_order.evidence_stages.iter_mut().enumerate() {
            stage.ordinal = ordinal as u8;
        }
        assert!(old_order.validate().is_err());

        let mut swapped_prefixes = finalized.clone();
        swapped_prefixes.evidence_stages.swap(2, 4);
        for (ordinal, stage) in swapped_prefixes.evidence_stages.iter_mut().enumerate() {
            stage.ordinal = ordinal as u8;
        }
        assert!(swapped_prefixes.validate().is_err());

        // A plan that drops the metadata half keeps a valid-looking ladder but
        // loses categorical title precedence, so it must fail closed.
        let mut metadata_removed = finalized.clone();
        metadata_removed.evidence_stages.remove(2);
        for (ordinal, stage) in metadata_removed.evidence_stages.iter_mut().enumerate() {
            stage.ordinal = ordinal as u8;
        }
        assert!(metadata_removed.validate().is_err());

        let mut text_removed = finalized.clone();
        text_removed.evidence_stages.remove(4);
        for (ordinal, stage) in text_removed.evidence_stages.iter_mut().enumerate() {
            stage.ordinal = ordinal as u8;
        }
        assert!(text_removed.validate().is_err());

        let mut regrouped = finalized.clone();
        regrouped.evidence_stages[2].field_group = QueryFieldGroup::Prefix;
        assert!(regrouped.validate().is_err());

        let prepared = prepare_lexical_query("CVE-2026-1234 context").unwrap();
        let report = evidence_report(&prepared, None, &[(0, 2), (4, 0)]);
        let finalized = prepared.finalize_evidence(report).unwrap();
        assert_eq!(
            finalized.execution,
            QueryExecutionDisposition::EmptyNoEvidence
        );
    }

    #[test]
    fn unicode_and_query_bounds_are_byte_and_term_stable() {
        let unicode = prepare_lexical_query("NAÏVE café 東京").unwrap();
        assert_eq!(unicode.terms, ["naive", "cafe", "東京"]);

        for term_count in [1, 2, 7, MAX_QUERY_TERMS] {
            let query = (0..term_count)
                .map(|index| format!("é{index}"))
                .collect::<Vec<_>>()
                .join(" ");
            let plan = prepare_lexical_query(&query).unwrap();
            assert_eq!(plan.terms.len(), term_count);
            assert_eq!(plan.support_probes.len(), term_count);
        }

        let repeated = std::iter::repeat_n("é", MAX_QUERY_TERMS)
            .collect::<Vec<_>>()
            .join(" ");
        let repeated = prepare_lexical_query(&repeated).unwrap();
        assert_eq!(repeated.terms, ["e"]);
        assert_eq!(repeated.support_probes.len(), 1);

        let bytes = "é".repeat(MAX_QUERY_BYTES / 2 + 1);
        let byte_error = prepare_lexical_query(&bytes).unwrap_err();
        assert_eq!(byte_error.code, "invalid_query");
        assert!(byte_error.message.contains("UTF-8 bytes"));

        let terms = std::iter::repeat_n("a", MAX_QUERY_TERMS + 1)
            .collect::<Vec<_>>()
            .join(" ");
        let term_error = prepare_lexical_query(&terms).unwrap_err();
        assert!(term_error.message.contains("terms"));
    }

    #[test]
    fn exact_values_preserve_complete_identity_up_to_the_query_byte_ceiling() {
        let query = "a".repeat(MAX_QUERY_BYTES);
        let plan = prepare_lexical_query(&query).unwrap();
        assert_eq!(plan.normalized_exact.as_deref(), Some(query.as_str()));
        assert_eq!(plan.exact_intent.unwrap().normalized.len(), MAX_QUERY_BYTES);

        let error = prepare_lexical_query(&(query + "a")).unwrap_err();
        assert_eq!(error.code, "invalid_query");
    }

    #[test]
    fn recognized_identifiers_are_single_exact_projection_anchors() {
        for (query, expected) in [
            ("RFC 9110 optional", "rfc 9110"),
            ("CVE-2026-1234 optional", "cve-2026-1234"),
            ("CVE 2026 1234 optional", "cve 2026 1234"),
            ("product/v2.4.1 optional", "product/v2.4.1"),
        ] {
            let plan = prepare_lexical_query(query).unwrap();
            assert_eq!(plan.kind, QueryPlanKind::Identifier, "{query}");
            assert_eq!(plan.terms[0], expected, "{query}");
            assert_eq!(
                plan.term_intents[0].projection,
                QueryTermProjection::ExactIdentifier,
                "{query}"
            );
            assert_eq!(
                plan.term_intents[0].role,
                QueryTermRole::RequiredIdentifierAnchor,
                "{query}"
            );
        }

        let iia = prepare_lexical_query("IIA 2 optional").unwrap();
        assert_eq!(iia.terms, ["iia", "2", "optional"]);
        assert!(iia.term_intents[..2].iter().all(|intent| {
            intent.role == QueryTermRole::RequiredIdentifierAnchor
                && intent.projection == QueryTermProjection::AnalyzedText
        }));
    }

    #[test]
    fn serialization_is_deterministic_and_legacy_shapes_are_refused() {
        let plan = prepare_lexical_query("RFC 9110 caching").unwrap();
        let first = serde_json::to_string(&plan).unwrap();
        let second = serde_json::to_string(&plan.clone()).unwrap();
        assert_eq!(first, second);
        assert!(first.starts_with("{\"schema_version\":7,\"query\":\"RFC 9110 caching\""));
        let decoded: LexicalQueryPlan = serde_json::from_str(&first).unwrap();
        assert_eq!(serde_json::to_string(&decoded).unwrap(), first);

        let legacy = r#"{"schema_version":2,"query":"cache","kind":"ordinary","match_operator":"any","terms":["cache"],"normalized_exact":"cache","phrase_boost":false}"#;
        assert!(serde_json::from_str::<LexicalQueryPlan>(legacy).is_err());

        let invalid_typo = first.replace("\"disabled\"", "\"enabled\"");
        assert!(serde_json::from_str::<LexicalQueryPlan>(&invalid_typo).is_err());
    }

    #[test]
    fn exact_validation_rejects_invalid_kind_operator_stage_and_schema_combinations() {
        let mut wrong_operator = prepare_lexical_query("cache").unwrap();
        wrong_operator.match_operator = QueryMatchOperator::All;
        assert!(wrong_operator.validate().is_err());

        let mut wrong_schema = prepare_lexical_query("cache").unwrap();
        wrong_schema.schema_version = 2;
        assert!(wrong_schema.validate().is_err());

        let prepared = prepare_lexical_query("cache filler").unwrap();
        let mut wrong_report = evidence_report(&prepared, None, &[(1, 0), (0, 0)]);
        wrong_report.schema_version = 2;
        assert!(prepared.clone().finalize_evidence(wrong_report).is_err());

        let report = evidence_report(&prepared, None, &[(1, 0), (0, 0)]);
        let finalized = prepared.finalize_evidence(report).unwrap();

        let mut reordered = finalized.clone();
        reordered.evidence_stages.swap(0, 1);
        assert!(reordered.validate().is_err());

        let mut missing_exact = finalized.clone();
        missing_exact.evidence_stages.remove(0);
        for (ordinal, stage) in missing_exact.evidence_stages.iter_mut().enumerate() {
            stage.ordinal = ordinal as u8;
        }
        assert!(missing_exact.validate().is_err());

        let mut relaxed_supported_context = finalized;
        let partial = relaxed_supported_context
            .evidence_stages
            .iter_mut()
            .find(|stage| stage.kind == QueryEvidenceStageKind::PartialCoverage)
            .unwrap();
        partial.required_term_indexes.clear();
        assert!(relaxed_supported_context.validate().is_err());
    }

    #[test]
    fn evidence_observations_are_exact_bounded_and_ordered() {
        let prepared = prepare_lexical_query("alpha beta").unwrap();

        let mut missing = evidence_report(&prepared, None, &[(1, 0), (1, 0)]);
        missing.term_support.pop();
        assert!(prepared.clone().finalize_evidence(missing).is_err());

        let mut reordered = evidence_report(&prepared, None, &[(1, 0), (1, 0)]);
        reordered.term_support.swap(0, 1);
        assert!(prepared.clone().finalize_evidence(reordered).is_err());

        let mut excessive_prefix = evidence_report(&prepared, None, &[(1, 0), (0, 0)]);
        excessive_prefix.term_support[1].prefix_expansions = MAX_PREFIX_EXPANSIONS_PER_TERM + 1;
        assert!(prepared.finalize_evidence(excessive_prefix).is_err());
    }
}
