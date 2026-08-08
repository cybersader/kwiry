use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::model::{PropertyBag, PropertyValue};
use crate::query::QueryEvidenceStageKind;

pub const RELEVANCE_PROFILE_SCHEMA_VERSION: u32 = 1;
pub const RERANK_INPUT_SCHEMA_VERSION: u32 = 1;
pub const LEXICAL_RELEVANCE_PROFILE_ID: &str = "lexical-v1";
pub const D5C_PREVIEW_PROFILE_ID: &str = "d5c-preview-v1";
pub const MAX_RELEVANCE_PROFILE_BYTES: usize = 64 * 1024;
pub const MAX_RANKING_RULE_ID_BYTES: usize = 64;
pub const MAX_RANKING_PROPERTY_NAME_BYTES: usize = 256;
pub const MAX_RANKING_JSON_POINTER_BYTES: usize = 1_024;
pub const MAX_RANKING_VALUE_BYTES: usize = 4_096;
pub const MAX_RANKING_FOLDER_PREFIX_BYTES: usize = 4_096;
pub const MAX_RANKING_PATH_DEPTH: usize = 64;
pub const MAX_PROPERTY_RANKING_RULES: usize = 8;
pub const MAX_FOLDER_RANKING_RULES_PER_FAMILY: usize = 16;
pub const MAX_TOTAL_RANKING_RULES: usize = 32;
pub const MAX_ABSOLUTE_METADATA_POINTS: i32 = 32;
pub const MAX_RERANK_CANDIDATES: usize = crate::query::MAX_TOTAL_CANDIDATES;
pub const MAX_RERANK_SOURCE_OBSERVATIONS: usize = MAX_RERANK_CANDIDATES;
pub const MAX_PROPERTY_VALUES_PER_SOURCE_OBSERVATION: usize = 256;
pub const MAX_RANKING_WORK_UNITS: usize = 65_536;

const MAX_AUTHORIZATION_SCOPE_BYTES: usize = 1_024;
const MAX_SOURCE_KEY_BYTES: usize = 256;
const MAX_CHUNK_ID_BYTES: usize = 256;

#[derive(Debug, Clone, Default)]
pub enum RelevanceProfile {
    #[default]
    LexicalV1,
    D5cPreviewV1(D5cRelevanceProfile),
}

impl RelevanceProfile {
    pub fn profile_id(&self) -> &str {
        match self {
            Self::LexicalV1 => LEXICAL_RELEVANCE_PROFILE_ID,
            Self::D5cPreviewV1(profile) => &profile.profile_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct D5cRelevanceProfile {
    pub schema_version: u32,
    pub profile_id: String,
    pub retrieval_profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recency: Option<RecencyRule>,
    #[serde(default)]
    pub hierarchy: HierarchyRules,
    #[serde(default)]
    pub property_rules: Vec<PropertyRule>,
}

impl D5cRelevanceProfile {
    pub fn preview() -> Self {
        Self {
            schema_version: RELEVANCE_PROFILE_SCHEMA_VERSION,
            profile_id: D5C_PREVIEW_PROFILE_ID.to_owned(),
            retrieval_profile_id: LEXICAL_RELEVANCE_PROFILE_ID.to_owned(),
            recency: None,
            hierarchy: HierarchyRules::default(),
            property_rules: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), RankingError> {
        validate_profile(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecencyRule {
    pub id: String,
    pub clock: RecencyClock,
    pub horizon: RecencyHorizon,
    pub strength: RuleStrength,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecencyClock {
    SourceMtime,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecencyHorizon {
    Week,
    Month,
    Quarter,
    Year,
}

impl RecencyHorizon {
    pub(crate) fn seconds(self) -> u64 {
        match self {
            Self::Week => 7 * 24 * 60 * 60,
            Self::Month => 30 * 24 * 60 * 60,
            Self::Quarter => 90 * 24 * 60 * 60,
            Self::Year => 365 * 24 * 60 * 60,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HierarchyRules {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<PathDepthRule>,
    #[serde(default)]
    pub authority_folders: Vec<FolderRule>,
    #[serde(default)]
    pub archive_folders: Vec<FolderRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PathDepthRule {
    pub id: String,
    pub predicate: PathDepthPredicate,
    pub effect: RuleEffect,
    pub strength: RuleStrength,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "segments",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PathDepthPredicate {
    AtMost(u8),
    AtLeast(u8),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FolderRule {
    pub id: String,
    pub prefix: String,
    pub strength: RuleStrength,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PropertyRule {
    pub id: String,
    pub property: String,
    pub predicate: PropertyPredicate,
    pub effect: RuleEffect,
    pub strength: RuleStrength,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PropertyPredicate {
    Presence,
    Exact {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pointer: Option<String>,
        value: RankingScalar,
    },
    I64Range {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pointer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<String>,
    },
    U64Range {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pointer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<String>,
    },
    F64Range {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pointer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<String>,
    },
    DateRange {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pointer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<String>,
    },
}

impl PropertyPredicate {
    fn pointer(&self) -> Option<&str> {
        match self {
            Self::Presence => None,
            Self::Exact { pointer, .. }
            | Self::I64Range { pointer, .. }
            | Self::U64Range { pointer, .. }
            | Self::F64Range { pointer, .. }
            | Self::DateRange { pointer, .. } => pointer.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RankingScalar {
    Null,
    Boolean(bool),
    I64(String),
    U64(String),
    F64(String),
    String(String),
    Date(String),
}

impl RankingScalar {
    pub fn i64(value: i64) -> Self {
        Self::I64(value.to_string())
    }

    pub fn u64(value: u64) -> Self {
        Self::U64(value.to_string())
    }

    pub fn f64(value: f64) -> Self {
        Self::F64(format!("{:016x}", value.to_bits()))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuleEffect {
    Boost,
    Demote,
}

impl RuleEffect {
    fn apply(self, points: i32) -> i32 {
        match self {
            Self::Boost => points,
            Self::Demote => -points,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RuleStrength {
    Low,
    Standard,
    High,
}

impl RuleStrength {
    fn points(self) -> i32 {
        match self {
            Self::Low => 1,
            Self::Standard => 2,
            Self::High => 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct QualifiedSourceId {
    pub authorization_scope: String,
    pub source_key: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LexicalEvidenceTier {
    Explicit,
    ExactMetadata,
    ExactPhrase,
    AllTerms,
    PartialCoverage,
    Prefix,
}

impl From<QueryEvidenceStageKind> for LexicalEvidenceTier {
    fn from(kind: QueryEvidenceStageKind) -> Self {
        match kind {
            QueryEvidenceStageKind::ExactMetadata => Self::ExactMetadata,
            QueryEvidenceStageKind::ExactPhrase => Self::ExactPhrase,
            QueryEvidenceStageKind::AllTerms => Self::AllTerms,
            QueryEvidenceStageKind::PartialCoverage => Self::PartialCoverage,
            QueryEvidenceStageKind::Prefix => Self::Prefix,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RerankCandidate {
    pub source: QualifiedSourceId,
    pub chunk_id: String,
    pub path: String,
    pub evidence_tier: LexicalEvidenceTier,
    pub lexical_score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RerankInput {
    pub schema_version: u32,
    pub query_time_epoch_seconds: u64,
    pub candidates: Vec<RerankCandidate>,
    #[serde(default)]
    pub source_signals: Vec<SourceSignalObservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceSignalObservation {
    pub source: QualifiedSourceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_mtime_epoch_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_property_rule_ids: Vec<String>,
    #[serde(default)]
    pub present_properties: Vec<String>,
    #[serde(default)]
    pub property_values: Vec<PropertyScalarObservation>,
}

impl SourceSignalObservation {
    pub fn from_property_bag(
        source: QualifiedSourceId,
        source_mtime_epoch_seconds: Option<u64>,
        properties: &PropertyBag,
        profile: &D5cRelevanceProfile,
    ) -> Result<Self, RankingError> {
        profile.validate()?;
        let configured: BTreeSet<_> = profile
            .property_rules
            .iter()
            .map(|rule| rule.property.as_str())
            .collect();
        let mut present_properties = Vec::new();
        let mut property_values = Vec::new();
        for property in configured {
            let Some(value) = properties.get(property) else {
                continue;
            };
            present_properties.push(property.to_owned());
            collect_property_values(property, "", value, 0, &mut property_values)?;
        }
        property_values.sort_by(|left, right| {
            (&left.property, &left.pointer).cmp(&(&right.property, &right.pointer))
        });
        Ok(Self {
            source,
            source_mtime_epoch_seconds,
            matched_property_rule_ids: Vec::new(),
            present_properties,
            property_values,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PropertyScalarObservation {
    pub property: String,
    pub pointer: String,
    pub value: RankingScalar,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RerankEvidence {
    pub schema_version: u32,
    pub candidate_count: usize,
    pub source_count: usize,
    pub entries: Vec<RerankEvidenceEntry>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RerankEvidenceEntry {
    pub tier: LexicalEvidenceTier,
    pub ordinal: usize,
    pub points: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RerankResult {
    profile_id: String,
    candidates: Vec<RerankCandidate>,
    evidence: RerankEvidence,
}

impl RerankResult {
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub fn candidates(&self) -> &[RerankCandidate] {
        &self.candidates
    }

    pub fn evidence(&self) -> &RerankEvidence {
        &self.evidence
    }

    pub fn into_candidates(self) -> Vec<RerankCandidate> {
        self.candidates
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RankingError {
    pub code: String,
    pub message: String,
}

#[derive(Debug)]
struct InternalRankingEvidence {
    lexical_tier: LexicalEvidenceTier,
    metadata_points: i32,
    matched_rule_ordinals: Vec<u8>,
}

#[derive(Debug)]
struct RankedCandidate {
    original_ordinal: usize,
    candidate: RerankCandidate,
    evidence: InternalRankingEvidence,
}

#[derive(Debug, Default)]
struct SourceScore {
    points: i32,
    matched_rule_ordinals: Vec<u8>,
}

pub fn rerank_candidates(
    profile: &RelevanceProfile,
    input: &RerankInput,
) -> Result<RerankResult, RankingError> {
    rerank_candidates_with_initial_work(profile, input, 0)
}

pub(crate) fn rerank_candidates_with_initial_work(
    profile: &RelevanceProfile,
    input: &RerankInput,
    initial_work_units: usize,
) -> Result<RerankResult, RankingError> {
    if initial_work_units > MAX_RANKING_WORK_UNITS {
        return Err(work_limit_error());
    }
    validate_rerank_input(input)?;
    if let RelevanceProfile::LexicalV1 = profile {
        let mut sources = BTreeSet::new();
        for candidate in &input.candidates {
            sources.insert(candidate.source.clone());
        }
        return Ok(RerankResult {
            profile_id: LEXICAL_RELEVANCE_PROFILE_ID.to_owned(),
            candidates: input.candidates.clone(),
            evidence: RerankEvidence {
                schema_version: 1,
                candidate_count: input.candidates.len(),
                source_count: sources.len(),
                entries: input
                    .candidates
                    .iter()
                    .enumerate()
                    .map(|(ordinal, candidate)| RerankEvidenceEntry {
                        tier: candidate.evidence_tier,
                        ordinal,
                        points: 0,
                    })
                    .collect(),
            },
        });
    }

    let RelevanceProfile::D5cPreviewV1(profile) = profile else {
        unreachable!("all relevance profiles are handled")
    };
    profile.validate()?;
    validate_precomputed_property_matches(profile, input)?;

    let signal_by_source: BTreeMap<_, _> = input
        .source_signals
        .iter()
        .map(|signal| (signal.source.clone(), signal))
        .collect();
    let mut source_paths = BTreeMap::new();
    for candidate in &input.candidates {
        source_paths
            .entry(candidate.source.clone())
            .or_insert_with(|| candidate.path.clone());
    }

    let mut work_units = initial_work_units;
    let mut source_scores = BTreeMap::new();
    for (source, path) in source_paths {
        let signal = signal_by_source.get(&source).copied();
        let score = score_source(
            profile,
            input.query_time_epoch_seconds,
            &path,
            signal,
            &mut work_units,
        )?;
        source_scores.insert(source, score);
    }

    let mut ranked = Vec::with_capacity(input.candidates.len());
    for (original_ordinal, candidate) in input.candidates.iter().cloned().enumerate() {
        let source_score = source_scores
            .get(&candidate.source)
            .expect("every candidate source was scored");
        let evidence = InternalRankingEvidence {
            lexical_tier: candidate.evidence_tier,
            metadata_points: source_score.points,
            matched_rule_ordinals: source_score.matched_rule_ordinals.clone(),
        };
        debug_assert!(evidence.matched_rule_ordinals.len() <= MAX_TOTAL_RANKING_RULES);
        ranked.push(RankedCandidate {
            original_ordinal,
            candidate,
            evidence,
        });
    }
    ranked.sort_by(compare_ranked_candidates);
    let evidence = RerankEvidence {
        schema_version: 1,
        candidate_count: ranked.len(),
        source_count: source_scores.len(),
        entries: ranked
            .iter()
            .map(|ranked| RerankEvidenceEntry {
                tier: ranked.evidence.lexical_tier,
                ordinal: ranked.original_ordinal,
                points: ranked.evidence.metadata_points,
            })
            .collect(),
    };

    Ok(RerankResult {
        profile_id: profile.profile_id.clone(),
        candidates: ranked.into_iter().map(|ranked| ranked.candidate).collect(),
        evidence,
    })
}

fn validate_profile(profile: &D5cRelevanceProfile) -> Result<(), RankingError> {
    if profile.schema_version != RELEVANCE_PROFILE_SCHEMA_VERSION
        || profile.profile_id != D5C_PREVIEW_PROFILE_ID
        || profile.retrieval_profile_id != LEXICAL_RELEVANCE_PROFILE_ID
    {
        return Err(invalid_profile("profile identity or schema is unsupported"));
    }
    let serialized = serde_json::to_vec(profile)
        .map_err(|_| invalid_profile("profile cannot be serialized canonically"))?;
    if serialized.len() > MAX_RELEVANCE_PROFILE_BYTES {
        return Err(invalid_profile("profile exceeds its serialized size limit"));
    }
    if profile.property_rules.len() > MAX_PROPERTY_RANKING_RULES
        || profile.hierarchy.authority_folders.len() > MAX_FOLDER_RANKING_RULES_PER_FAMILY
        || profile.hierarchy.archive_folders.len() > MAX_FOLDER_RANKING_RULES_PER_FAMILY
    {
        return Err(invalid_profile("profile exceeds a rule-family limit"));
    }
    let total_rules = usize::from(profile.recency.is_some())
        + usize::from(profile.hierarchy.depth.is_some())
        + profile.hierarchy.authority_folders.len()
        + profile.hierarchy.archive_folders.len()
        + profile.property_rules.len();
    if total_rules > MAX_TOTAL_RANKING_RULES {
        return Err(invalid_profile("profile exceeds the total rule limit"));
    }

    let mut ids = BTreeSet::new();
    if let Some(rule) = &profile.recency {
        validate_rule_id(&rule.id, &mut ids)?;
    }
    if let Some(rule) = &profile.hierarchy.depth {
        validate_rule_id(&rule.id, &mut ids)?;
        let segments = match rule.predicate {
            PathDepthPredicate::AtMost(segments) | PathDepthPredicate::AtLeast(segments) => {
                segments
            }
        };
        if usize::from(segments) > MAX_RANKING_PATH_DEPTH {
            return Err(invalid_profile("path-depth rule exceeds its limit"));
        }
    }

    validate_folder_rules(&profile.hierarchy.authority_folders, &mut ids)?;
    validate_folder_rules(&profile.hierarchy.archive_folders, &mut ids)?;
    for authority in &profile.hierarchy.authority_folders {
        for archive in &profile.hierarchy.archive_folders {
            if folder_prefixes_overlap(&authority.prefix, &archive.prefix) {
                return Err(invalid_profile(
                    "authority and archive folder rules overlap",
                ));
            }
        }
    }

    if profile
        .property_rules
        .windows(2)
        .any(|pair| pair[0].id >= pair[1].id)
    {
        return Err(invalid_profile(
            "property rules must use canonical ID order",
        ));
    }
    let mut predicates = BTreeSet::new();
    for rule in &profile.property_rules {
        validate_rule_id(&rule.id, &mut ids)?;
        if rule.property.is_empty() || rule.property.len() > MAX_RANKING_PROPERTY_NAME_BYTES {
            return Err(invalid_profile("property rule name is invalid"));
        }
        validate_property_predicate(&rule.predicate)?;
        let predicate = serde_json::to_string(&(rule.property.as_str(), &rule.predicate))
            .map_err(|_| invalid_profile("property predicate is not canonical"))?;
        if !predicates.insert(predicate) {
            return Err(invalid_profile("property predicate is duplicated"));
        }
    }

    let authority_max = profile
        .hierarchy
        .authority_folders
        .iter()
        .map(|rule| rule.strength.points())
        .max()
        .unwrap_or(0);
    let archive_max = profile
        .hierarchy
        .archive_folders
        .iter()
        .map(|rule| rule.strength.points())
        .max()
        .unwrap_or(0);
    let theoretical_points = profile
        .recency
        .as_ref()
        .map_or(0, |rule| rule.strength.points())
        + profile
            .hierarchy
            .depth
            .as_ref()
            .map_or(0, |rule| rule.strength.points())
        + authority_max
        + archive_max
        + profile
            .property_rules
            .iter()
            .map(|rule| rule.strength.points())
            .sum::<i32>();
    if theoretical_points > MAX_ABSOLUTE_METADATA_POINTS {
        return Err(invalid_profile(
            "profile metadata contribution exceeds its absolute cap",
        ));
    }
    Ok(())
}

fn validate_folder_rules(
    rules: &[FolderRule],
    ids: &mut BTreeSet<String>,
) -> Result<(), RankingError> {
    if rules
        .windows(2)
        .any(|pair| (&pair[0].prefix, &pair[0].id) >= (&pair[1].prefix, &pair[1].id))
    {
        return Err(invalid_profile(
            "folder rules must use canonical prefix and ID order",
        ));
    }
    let mut prefixes = BTreeSet::new();
    for rule in rules {
        validate_rule_id(&rule.id, ids)?;
        if rule.prefix.len() > MAX_RANKING_FOLDER_PREFIX_BYTES
            || !is_normalized_relative_path(&rule.prefix)
            || path_depth(&rule.prefix) > MAX_RANKING_PATH_DEPTH
        {
            return Err(invalid_profile("folder rule prefix is invalid"));
        }
        if !prefixes.insert(rule.prefix.as_str()) {
            return Err(invalid_profile("folder rule prefix is duplicated"));
        }
    }
    Ok(())
}

fn validate_rule_id(id: &str, ids: &mut BTreeSet<String>) -> Result<(), RankingError> {
    if id.is_empty() || id.len() > MAX_RANKING_RULE_ID_BYTES {
        return Err(invalid_profile("ranking rule ID is invalid"));
    }
    if !ids.insert(id.to_owned()) {
        return Err(invalid_profile("ranking rule ID is duplicated"));
    }
    Ok(())
}

fn validate_property_predicate(predicate: &PropertyPredicate) -> Result<(), RankingError> {
    if let Some(pointer) = predicate.pointer() {
        validate_json_pointer(pointer)?;
    }
    match predicate {
        PropertyPredicate::Presence => Ok(()),
        PropertyPredicate::Exact { value, .. } => validate_scalar(value, true),
        PropertyPredicate::I64Range { min, max, .. } => {
            validate_range(min, max, parse_canonical_i64)
        }
        PropertyPredicate::U64Range { min, max, .. } => {
            validate_range(min, max, parse_canonical_u64)
        }
        PropertyPredicate::F64Range { min, max, .. } => {
            validate_range(min, max, parse_canonical_f64)
        }
        PropertyPredicate::DateRange { min, max, .. } => {
            if min.is_none() && max.is_none() {
                return Err(invalid_profile("property range must have a bound"));
            }
            if min
                .as_deref()
                .is_some_and(|value| !is_iso_calendar_date(value))
                || max
                    .as_deref()
                    .is_some_and(|value| !is_iso_calendar_date(value))
                || min
                    .as_deref()
                    .zip(max.as_deref())
                    .is_some_and(|(min, max)| min > max)
            {
                return Err(invalid_profile("date range is invalid"));
            }
            Ok(())
        }
    }
}

fn validate_range<T: PartialOrd>(
    min: &Option<String>,
    max: &Option<String>,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<(), RankingError> {
    if min.is_none() && max.is_none() {
        return Err(invalid_profile("property range must have a bound"));
    }
    let parsed_min = min
        .as_deref()
        .map(|value| parse(value).ok_or(()))
        .transpose()
        .map_err(|()| invalid_profile("numeric range is invalid"))?;
    let parsed_max = max
        .as_deref()
        .map(|value| parse(value).ok_or(()))
        .transpose()
        .map_err(|()| invalid_profile("numeric range is invalid"))?;
    if parsed_min
        .as_ref()
        .zip(parsed_max.as_ref())
        .is_some_and(|(min, max)| min > max)
    {
        return Err(invalid_profile("numeric range is reversed"));
    }
    Ok(())
}

fn validate_scalar(value: &RankingScalar, configured: bool) -> Result<(), RankingError> {
    let valid = match value {
        RankingScalar::Null | RankingScalar::Boolean(_) => true,
        RankingScalar::I64(value) => parse_canonical_i64(value).is_some(),
        RankingScalar::U64(value) => parse_canonical_u64(value).is_some(),
        RankingScalar::F64(value) => parse_canonical_f64(value).is_some(),
        RankingScalar::String(value) => {
            value.len() <= MAX_RANKING_VALUE_BYTES && !is_iso_calendar_date(value)
        }
        RankingScalar::Date(value) => is_iso_calendar_date(value),
    };
    if valid {
        Ok(())
    } else if configured {
        Err(invalid_profile("configured property scalar is invalid"))
    } else {
        Err(invalid_input("observed property scalar is invalid"))
    }
}

fn validate_json_pointer(pointer: &str) -> Result<(), RankingError> {
    if pointer.len() > MAX_RANKING_JSON_POINTER_BYTES
        || (!pointer.is_empty() && !pointer.starts_with('/'))
        || pointer.matches('/').count() > MAX_RANKING_PATH_DEPTH
    {
        return Err(invalid_profile("property JSON Pointer is invalid"));
    }
    let bytes = pointer.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'~' {
            if index + 1 == bytes.len() || !matches!(bytes[index + 1], b'0' | b'1') {
                return Err(invalid_profile("property JSON Pointer escape is invalid"));
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn validate_rerank_input(input: &RerankInput) -> Result<(), RankingError> {
    if input.schema_version != RERANK_INPUT_SCHEMA_VERSION {
        return Err(invalid_input("rerank input schema is unsupported"));
    }
    if input.candidates.len() > MAX_RERANK_CANDIDATES
        || input.source_signals.len() > MAX_RERANK_SOURCE_OBSERVATIONS
    {
        return Err(invalid_input("rerank input exceeds a cardinality limit"));
    }
    if input
        .candidates
        .windows(2)
        .any(|pair| pair[0].evidence_tier > pair[1].evidence_tier)
    {
        return Err(invalid_input(
            "candidate order does not preserve lexical evidence tiers",
        ));
    }

    let mut candidate_ids = BTreeSet::new();
    let mut source_paths = BTreeMap::new();
    for candidate in &input.candidates {
        validate_source_id(&candidate.source)?;
        if candidate.chunk_id.is_empty()
            || candidate.chunk_id.len() > MAX_CHUNK_ID_BYTES
            || !candidate.lexical_score.is_finite()
            || candidate.path.len() > MAX_RANKING_FOLDER_PREFIX_BYTES
            || !is_normalized_relative_path(&candidate.path)
            || path_depth(&candidate.path) > MAX_RANKING_PATH_DEPTH
        {
            return Err(invalid_input("rerank candidate is invalid"));
        }
        let identity = (
            candidate.source.clone(),
            candidate.chunk_id.as_str(),
            candidate.path.as_str(),
        );
        if !candidate_ids.insert(identity) {
            return Err(invalid_input("rerank candidate is duplicated"));
        }
        if source_paths
            .insert(candidate.source.clone(), candidate.path.as_str())
            .is_some_and(|previous| previous != candidate.path)
        {
            return Err(invalid_input("candidate source maps to inconsistent paths"));
        }
    }

    if input
        .source_signals
        .windows(2)
        .any(|pair| pair[0].source >= pair[1].source)
    {
        return Err(invalid_input(
            "source observations must use canonical source order",
        ));
    }
    for signal in &input.source_signals {
        validate_source_id(&signal.source)?;
        if signal.matched_property_rule_ids.len() > MAX_PROPERTY_RANKING_RULES
            || signal.present_properties.len() > MAX_PROPERTY_VALUES_PER_SOURCE_OBSERVATION
            || signal.property_values.len() > MAX_PROPERTY_VALUES_PER_SOURCE_OBSERVATION
            || signal
                .matched_property_rule_ids
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || signal
                .matched_property_rule_ids
                .iter()
                .any(|id| id.is_empty() || id.len() > MAX_RANKING_RULE_ID_BYTES)
            || signal
                .present_properties
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || signal.property_values.windows(2).any(|pair| {
                (&pair[0].property, &pair[0].pointer) >= (&pair[1].property, &pair[1].pointer)
            })
        {
            return Err(invalid_input(
                "source property observations are not canonical or bounded",
            ));
        }
        for property in &signal.present_properties {
            if property.is_empty() || property.len() > MAX_RANKING_PROPERTY_NAME_BYTES {
                return Err(invalid_input("observed property name is invalid"));
            }
        }
        for observation in &signal.property_values {
            if observation.property.is_empty()
                || observation.property.len() > MAX_RANKING_PROPERTY_NAME_BYTES
                || !signal.present_properties.contains(&observation.property)
            {
                return Err(invalid_input("observed property scalar is inconsistent"));
            }
            validate_observed_pointer(&observation.pointer)?;
            validate_scalar(&observation.value, false)?;
        }
    }
    Ok(())
}

fn validate_precomputed_property_matches(
    profile: &D5cRelevanceProfile,
    input: &RerankInput,
) -> Result<(), RankingError> {
    let configured: BTreeSet<_> = profile
        .property_rules
        .iter()
        .map(|rule| rule.id.as_str())
        .collect();
    for signal in &input.source_signals {
        if signal
            .matched_property_rule_ids
            .iter()
            .any(|id| !configured.contains(id.as_str()))
        {
            return Err(invalid_input(
                "source observation references an unknown property rule",
            ));
        }
    }
    Ok(())
}

fn validate_source_id(source: &QualifiedSourceId) -> Result<(), RankingError> {
    if source.authorization_scope.is_empty()
        || source.authorization_scope.len() > MAX_AUTHORIZATION_SCOPE_BYTES
        || source.source_key.is_empty()
        || source.source_key.len() > MAX_SOURCE_KEY_BYTES
    {
        return Err(invalid_input("qualified source identity is invalid"));
    }
    Ok(())
}

fn validate_observed_pointer(pointer: &str) -> Result<(), RankingError> {
    validate_json_pointer(pointer)
        .map_err(|_| invalid_input("observed property pointer is invalid"))
}

fn score_source(
    profile: &D5cRelevanceProfile,
    query_time_epoch_seconds: u64,
    path: &str,
    signal: Option<&SourceSignalObservation>,
    work_units: &mut usize,
) -> Result<SourceScore, RankingError> {
    let mut score = SourceScore::default();
    let mut ordinal = 0_u8;

    if let Some(rule) = &profile.recency {
        charge_work(work_units, 1)?;
        if signal
            .and_then(|signal| signal.source_mtime_epoch_seconds)
            .is_some_and(|mtime| {
                mtime > 0
                    && mtime <= query_time_epoch_seconds
                    && query_time_epoch_seconds - mtime <= rule.horizon.seconds()
            })
        {
            add_match(&mut score, ordinal, rule.strength.points());
        }
        ordinal += 1;
    }

    if let Some(rule) = &profile.hierarchy.depth {
        charge_work(work_units, 1)?;
        let folder_depth = path_depth(path).saturating_sub(1);
        let matches = match rule.predicate {
            PathDepthPredicate::AtMost(segments) => folder_depth <= usize::from(segments),
            PathDepthPredicate::AtLeast(segments) => folder_depth >= usize::from(segments),
        };
        if matches {
            add_match(
                &mut score,
                ordinal,
                rule.effect.apply(rule.strength.points()),
            );
        }
        ordinal += 1;
    }

    if let Some((index, rule)) = best_folder_match(&profile.hierarchy.authority_folders, path) {
        charge_work(work_units, profile.hierarchy.authority_folders.len())?;
        add_match(&mut score, ordinal + index as u8, rule.strength.points());
    } else {
        charge_work(work_units, profile.hierarchy.authority_folders.len())?;
    }
    ordinal += profile.hierarchy.authority_folders.len() as u8;

    if let Some((index, rule)) = best_folder_match(&profile.hierarchy.archive_folders, path) {
        charge_work(work_units, profile.hierarchy.archive_folders.len())?;
        add_match(&mut score, ordinal + index as u8, -rule.strength.points());
    } else {
        charge_work(work_units, profile.hierarchy.archive_folders.len())?;
    }
    ordinal += profile.hierarchy.archive_folders.len() as u8;

    for rule in &profile.property_rules {
        if property_rule_matches(rule, signal, work_units)? {
            add_match(
                &mut score,
                ordinal,
                rule.effect.apply(rule.strength.points()),
            );
        }
        ordinal += 1;
    }
    debug_assert_eq!(usize::from(ordinal), profile_rule_count(profile));
    Ok(score)
}

fn property_rule_matches(
    rule: &PropertyRule,
    signal: Option<&SourceSignalObservation>,
    work_units: &mut usize,
) -> Result<bool, RankingError> {
    charge_work(work_units, 1)?;
    let Some(signal) = signal else {
        return Ok(false);
    };
    if signal
        .matched_property_rule_ids
        .binary_search(&rule.id)
        .is_ok()
    {
        return Ok(true);
    }
    if matches!(rule.predicate, PropertyPredicate::Presence) {
        return Ok(signal
            .present_properties
            .binary_search(&rule.property)
            .is_ok());
    }
    if signal
        .present_properties
        .binary_search(&rule.property)
        .is_err()
    {
        return Ok(false);
    }

    let pointer = rule.predicate.pointer();
    for observation in signal
        .property_values
        .iter()
        .filter(|observation| observation.property == rule.property)
    {
        charge_work(work_units, 1)?;
        if pointer.is_some_and(|pointer| pointer != observation.pointer) {
            continue;
        }
        if scalar_matches(&rule.predicate, &observation.value) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn scalar_matches(predicate: &PropertyPredicate, value: &RankingScalar) -> bool {
    match predicate {
        PropertyPredicate::Presence => true,
        PropertyPredicate::Exact {
            value: expected, ..
        } => expected == value,
        PropertyPredicate::I64Range { min, max, .. } => {
            let RankingScalar::I64(value) = value else {
                return false;
            };
            parse_canonical_i64(value).is_some_and(|value| {
                within_range(
                    value,
                    min.as_deref().and_then(parse_canonical_i64),
                    max.as_deref().and_then(parse_canonical_i64),
                )
            })
        }
        PropertyPredicate::U64Range { min, max, .. } => {
            let RankingScalar::U64(value) = value else {
                return false;
            };
            parse_canonical_u64(value).is_some_and(|value| {
                within_range(
                    value,
                    min.as_deref().and_then(parse_canonical_u64),
                    max.as_deref().and_then(parse_canonical_u64),
                )
            })
        }
        PropertyPredicate::F64Range { min, max, .. } => {
            let RankingScalar::F64(value) = value else {
                return false;
            };
            parse_canonical_f64(value).is_some_and(|value| {
                within_range(
                    value,
                    min.as_deref().and_then(parse_canonical_f64),
                    max.as_deref().and_then(parse_canonical_f64),
                )
            })
        }
        PropertyPredicate::DateRange { min, max, .. } => {
            let RankingScalar::Date(value) = value else {
                return false;
            };
            min.as_deref().is_none_or(|min| value.as_str() >= min)
                && max.as_deref().is_none_or(|max| value.as_str() <= max)
        }
    }
}

fn within_range<T: PartialOrd>(value: T, min: Option<T>, max: Option<T>) -> bool {
    min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max)
}

fn best_folder_match<'a>(rules: &'a [FolderRule], path: &str) -> Option<(usize, &'a FolderRule)> {
    rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| path_is_within_folder(path, &rule.prefix))
        .max_by(|(left_index, left), (right_index, right)| {
            path_depth(&left.prefix)
                .cmp(&path_depth(&right.prefix))
                .then_with(|| right.id.cmp(&left.id))
                .then_with(|| right_index.cmp(left_index))
        })
}

fn add_match(score: &mut SourceScore, ordinal: u8, points: i32) {
    score.points += points;
    score.matched_rule_ordinals.push(ordinal);
}

fn compare_ranked_candidates(left: &RankedCandidate, right: &RankedCandidate) -> Ordering {
    left.evidence
        .lexical_tier
        .cmp(&right.evidence.lexical_tier)
        .then_with(|| {
            right
                .evidence
                .metadata_points
                .cmp(&left.evidence.metadata_points)
        })
        .then_with(|| left.original_ordinal.cmp(&right.original_ordinal))
        .then_with(|| left.candidate.chunk_id.cmp(&right.candidate.chunk_id))
        .then_with(|| left.candidate.path.cmp(&right.candidate.path))
}

fn collect_property_values(
    property: &str,
    pointer: &str,
    value: &PropertyValue,
    depth: usize,
    output: &mut Vec<PropertyScalarObservation>,
) -> Result<(), RankingError> {
    if depth > MAX_RANKING_PATH_DEPTH {
        return Ok(());
    }
    match value {
        PropertyValue::Sequence(values) => {
            for (index, value) in values.iter().enumerate() {
                let pointer = format!("{pointer}/{index}");
                collect_property_values(property, &pointer, value, depth + 1, output)?;
            }
        }
        PropertyValue::Map(values) => {
            for (component, value) in values {
                let pointer = format!(
                    "{pointer}/{}",
                    component.replace('~', "~0").replace('/', "~1")
                );
                collect_property_values(property, &pointer, value, depth + 1, output)?;
            }
        }
        scalar => {
            if pointer.len() > MAX_RANKING_JSON_POINTER_BYTES
                || output.len() == MAX_PROPERTY_VALUES_PER_SOURCE_OBSERVATION
            {
                return Err(work_limit_error());
            }
            let value = match scalar {
                PropertyValue::Null => RankingScalar::Null,
                PropertyValue::Bool(value) => RankingScalar::Boolean(*value),
                PropertyValue::I64(value) => RankingScalar::i64(*value),
                PropertyValue::U64(value) => RankingScalar::u64(*value),
                PropertyValue::F64(value) if value.is_finite() => RankingScalar::f64(*value),
                PropertyValue::String(value) if value.len() <= MAX_RANKING_VALUE_BYTES => {
                    if is_iso_calendar_date(value) {
                        RankingScalar::Date(value.clone())
                    } else {
                        RankingScalar::String(value.clone())
                    }
                }
                PropertyValue::F64(_) | PropertyValue::String(_) => return Ok(()),
                PropertyValue::Sequence(_) | PropertyValue::Map(_) => {
                    unreachable!("recursive property values are handled above")
                }
            };
            output.push(PropertyScalarObservation {
                property: property.to_owned(),
                pointer: pointer.to_owned(),
                value,
            });
        }
    }
    Ok(())
}

fn charge_work(work_units: &mut usize, amount: usize) -> Result<(), RankingError> {
    *work_units = work_units
        .checked_add(amount)
        .filter(|work| *work <= MAX_RANKING_WORK_UNITS)
        .ok_or_else(work_limit_error)?;
    Ok(())
}

fn profile_rule_count(profile: &D5cRelevanceProfile) -> usize {
    usize::from(profile.recency.is_some())
        + usize::from(profile.hierarchy.depth.is_some())
        + profile.hierarchy.authority_folders.len()
        + profile.hierarchy.archive_folders.len()
        + profile.property_rules.len()
}

fn parse_canonical_i64(value: &str) -> Option<i64> {
    let parsed = value.parse::<i64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn parse_canonical_u64(value: &str) -> Option<u64> {
    let parsed = value.parse::<u64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn parse_canonical_f64(value: &str) -> Option<f64> {
    if value.len() != 16
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let parsed = f64::from_bits(u64::from_str_radix(value, 16).ok()?);
    parsed.is_finite().then_some(parsed)
}

fn is_normalized_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.ends_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

fn path_depth(path: &str) -> usize {
    path.split('/').count()
}

fn path_is_within_folder(path: &str, folder: &str) -> bool {
    path.strip_prefix(folder)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

fn folder_prefixes_overlap(left: &str, right: &str) -> bool {
    left == right || path_is_within_folder(left, right) || path_is_within_folder(right, left)
}

fn is_iso_calendar_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return false;
    }
    let year = decimal_component(&bytes[0..4]);
    let month = decimal_component(&bytes[5..7]);
    let day = decimal_component(&bytes[8..10]);
    if !(1..=12).contains(&month) || day == 0 {
        return false;
    }
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    day <= days[(month - 1) as usize]
}

fn decimal_component(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0, |value, byte| value * 10 + u32::from(byte - b'0'))
}

fn invalid_profile(message: &str) -> RankingError {
    RankingError {
        code: "invalid_relevance_profile".to_owned(),
        message: message.to_owned(),
    }
}

fn invalid_input(message: &str) -> RankingError {
    RankingError {
        code: "invalid_rerank_input".to_owned(),
        message: message.to_owned(),
    }
}

fn work_limit_error() -> RankingError {
    RankingError {
        code: "ranking_work_limit_exceeded".to_owned(),
        message: "ranking work exceeded its deterministic limit".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(scope: &str, key: &str) -> QualifiedSourceId {
        QualifiedSourceId {
            authorization_scope: scope.to_owned(),
            source_key: key.to_owned(),
        }
    }

    fn candidate(
        scope: &str,
        key: &str,
        chunk: &str,
        path: &str,
        tier: LexicalEvidenceTier,
        score: f32,
    ) -> RerankCandidate {
        RerankCandidate {
            source: source(scope, key),
            chunk_id: chunk.to_owned(),
            path: path.to_owned(),
            evidence_tier: tier,
            lexical_score: score,
        }
    }

    fn input(candidates: Vec<RerankCandidate>) -> RerankInput {
        RerankInput {
            schema_version: RERANK_INPUT_SCHEMA_VERSION,
            query_time_epoch_seconds: 2_000_000_000,
            candidates,
            source_signals: Vec::new(),
        }
    }

    fn property_rule(
        id: &str,
        property: &str,
        predicate: PropertyPredicate,
        effect: RuleEffect,
        strength: RuleStrength,
    ) -> PropertyRule {
        PropertyRule {
            id: id.to_owned(),
            property: property.to_owned(),
            predicate,
            effect,
            strength,
        }
    }

    fn observation(
        scope: &str,
        key: &str,
        mtime: Option<u64>,
        present: &[&str],
        values: Vec<PropertyScalarObservation>,
    ) -> SourceSignalObservation {
        SourceSignalObservation {
            source: source(scope, key),
            source_mtime_epoch_seconds: mtime,
            matched_property_rule_ids: Vec::new(),
            present_properties: present.iter().map(|value| (*value).to_owned()).collect(),
            property_values: values,
        }
    }

    fn scalar(property: &str, pointer: &str, value: RankingScalar) -> PropertyScalarObservation {
        PropertyScalarObservation {
            property: property.to_owned(),
            pointer: pointer.to_owned(),
            value,
        }
    }

    fn paths(result: &RerankResult) -> Vec<&str> {
        result
            .candidates()
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect()
    }

    #[test]
    fn lexical_v1_default_preserves_candidate_bytes_and_order_exactly() {
        let input = input(vec![
            candidate(
                "allowed",
                "a",
                "chunk-a",
                "a.md",
                LexicalEvidenceTier::ExactMetadata,
                8.5,
            ),
            candidate(
                "allowed",
                "b",
                "chunk-b",
                "b.md",
                LexicalEvidenceTier::Prefix,
                99.0,
            ),
        ]);
        let before = serde_json::to_vec(&input.candidates).unwrap();
        let result = rerank_candidates(&RelevanceProfile::default(), &input).unwrap();
        let after = serde_json::to_vec(result.candidates()).unwrap();

        assert_eq!(result.profile_id(), LEXICAL_RELEVANCE_PROFILE_ID);
        assert_eq!(after, before);
    }

    #[test]
    fn lexical_v1_refuses_invalid_or_unbounded_rerank_inputs() {
        let mut invalid_schema = input(Vec::new());
        invalid_schema.schema_version += 1;
        assert_eq!(
            rerank_candidates(&RelevanceProfile::LexicalV1, &invalid_schema)
                .unwrap_err()
                .code,
            "invalid_rerank_input"
        );

        let mut non_finite = input(vec![candidate(
            "allowed",
            "a",
            "chunk-a",
            "a.md",
            LexicalEvidenceTier::AllTerms,
            f32::NAN,
        )]);
        assert_eq!(
            rerank_candidates(&RelevanceProfile::LexicalV1, &non_finite)
                .unwrap_err()
                .code,
            "invalid_rerank_input"
        );

        non_finite.candidates.clear();
        non_finite
            .candidates
            .extend((0..=MAX_RERANK_CANDIDATES).map(|ordinal| {
                candidate(
                    "allowed",
                    &format!("source-{ordinal}"),
                    &format!("chunk-{ordinal}"),
                    &format!("note-{ordinal}.md"),
                    LexicalEvidenceTier::AllTerms,
                    1.0,
                )
            }));
        assert_eq!(
            rerank_candidates(&RelevanceProfile::LexicalV1, &non_finite)
                .unwrap_err()
                .code,
            "invalid_rerank_input"
        );
    }

    #[test]
    fn recency_cannot_cross_lexical_evidence_tiers() {
        let mut profile = D5cRelevanceProfile::preview();
        profile.recency = Some(RecencyRule {
            id: "recent".into(),
            clock: RecencyClock::SourceMtime,
            horizon: RecencyHorizon::Week,
            strength: RuleStrength::High,
        });
        let mut input = input(vec![
            candidate(
                "allowed",
                "old-exact",
                "old-exact",
                "old.md",
                LexicalEvidenceTier::AllTerms,
                1.0,
            ),
            candidate(
                "allowed",
                "new-prefix",
                "new-prefix",
                "new.md",
                LexicalEvidenceTier::Prefix,
                100.0,
            ),
        ]);
        input.source_signals = vec![
            observation("allowed", "new-prefix", Some(1_999_999_999), &[], vec![]),
            observation("allowed", "old-exact", Some(1), &[], vec![]),
        ];
        input
            .source_signals
            .sort_by(|left, right| left.source.cmp(&right.source));

        let result = rerank_candidates(&RelevanceProfile::D5cPreviewV1(profile), &input).unwrap();
        assert_eq!(paths(&result), ["old.md", "new.md"]);
    }

    #[test]
    fn source_level_signals_fan_out_once_to_every_chunk() {
        let mut profile = D5cRelevanceProfile::preview();
        profile.property_rules.push(property_rule(
            "priority",
            "priority",
            PropertyPredicate::Exact {
                pointer: Some(String::new()),
                value: RankingScalar::i64(7),
            },
            RuleEffect::Boost,
            RuleStrength::Standard,
        ));
        let mut input = input(vec![
            candidate(
                "allowed",
                "other",
                "other",
                "other.md",
                LexicalEvidenceTier::AllTerms,
                3.0,
            ),
            candidate(
                "allowed",
                "source-a",
                "source-a-1",
                "source-a.md",
                LexicalEvidenceTier::AllTerms,
                2.0,
            ),
            candidate(
                "allowed",
                "source-a",
                "source-a-2",
                "source-a.md",
                LexicalEvidenceTier::AllTerms,
                1.0,
            ),
        ]);
        input.source_signals = vec![observation(
            "allowed",
            "source-a",
            None,
            &["priority"],
            vec![scalar("priority", "", RankingScalar::i64(7))],
        )];

        let result = rerank_candidates(&RelevanceProfile::D5cPreviewV1(profile), &input).unwrap();
        let chunks: Vec<_> = result
            .candidates()
            .iter()
            .map(|candidate| candidate.chunk_id.as_str())
            .collect();
        assert_eq!(chunks, ["source-a-1", "source-a-2", "other"]);
        assert_eq!(
            result
                .evidence()
                .entries
                .iter()
                .map(|entry| entry.points)
                .collect::<Vec<_>>(),
            [2, 2, 0]
        );
    }

    #[test]
    fn extreme_theoretical_weight_is_refused() {
        let mut profile = D5cRelevanceProfile::preview();
        profile.recency = Some(RecencyRule {
            id: "00-recency".into(),
            clock: RecencyClock::SourceMtime,
            horizon: RecencyHorizon::Year,
            strength: RuleStrength::High,
        });
        profile.hierarchy.depth = Some(PathDepthRule {
            id: "01-depth".into(),
            predicate: PathDepthPredicate::AtMost(4),
            effect: RuleEffect::Boost,
            strength: RuleStrength::High,
        });
        profile.hierarchy.authority_folders = vec![FolderRule {
            id: "02-authority".into(),
            prefix: "authority".into(),
            strength: RuleStrength::High,
        }];
        profile.hierarchy.archive_folders = vec![FolderRule {
            id: "03-archive".into(),
            prefix: "archive".into(),
            strength: RuleStrength::High,
        }];
        for index in 0..5 {
            profile.property_rules.push(property_rule(
                &format!("1{index}-property"),
                &format!("property_{index}"),
                PropertyPredicate::Presence,
                RuleEffect::Boost,
                RuleStrength::High,
            ));
        }

        let error = profile.validate().unwrap_err();
        assert_eq!(error.code, "invalid_relevance_profile");
        assert!(error.message.contains("absolute cap"));
    }

    #[test]
    fn missing_property_is_neutral_and_mixed_types_never_coerce() {
        let mut profile = D5cRelevanceProfile::preview();
        profile.property_rules.push(property_rule(
            "score-range",
            "score",
            PropertyPredicate::I64Range {
                pointer: Some(String::new()),
                min: Some("7".into()),
                max: Some("7".into()),
            },
            RuleEffect::Boost,
            RuleStrength::Standard,
        ));
        let candidates = vec![
            candidate(
                "allowed",
                "missing",
                "missing",
                "missing.md",
                LexicalEvidenceTier::AllTerms,
                3.0,
            ),
            candidate(
                "allowed",
                "string",
                "string",
                "string.md",
                LexicalEvidenceTier::AllTerms,
                2.0,
            ),
            candidate(
                "allowed",
                "integer",
                "integer",
                "integer.md",
                LexicalEvidenceTier::AllTerms,
                1.0,
            ),
        ];
        let mut input = input(candidates);
        input.source_signals = vec![
            observation(
                "allowed",
                "integer",
                None,
                &["score"],
                vec![scalar("score", "", RankingScalar::i64(7))],
            ),
            observation(
                "allowed",
                "string",
                None,
                &["score"],
                vec![scalar("score", "", RankingScalar::String("7".into()))],
            ),
        ];

        let result = rerank_candidates(&RelevanceProfile::D5cPreviewV1(profile), &input).unwrap();
        assert_eq!(paths(&result), ["integer.md", "missing.md", "string.md"]);
    }

    #[test]
    fn date_shaped_exact_strings_are_refused_in_favor_of_the_date_type() {
        let mut profile = D5cRelevanceProfile::preview();
        profile.property_rules.push(property_rule(
            "reviewed",
            "reviewed",
            PropertyPredicate::Exact {
                pointer: Some(String::new()),
                value: RankingScalar::String("2026-07-31".into()),
            },
            RuleEffect::Boost,
            RuleStrength::Standard,
        ));
        assert_eq!(
            profile.validate().unwrap_err().code,
            "invalid_relevance_profile"
        );
    }

    #[test]
    fn exact_numeric_extremes_and_calendar_dates_remain_typed() {
        let mut profile = D5cRelevanceProfile::preview();
        profile.property_rules.push(property_rule(
            "00-limit",
            "limit",
            PropertyPredicate::U64Range {
                pointer: Some(String::new()),
                min: Some(u64::MAX.to_string()),
                max: Some(u64::MAX.to_string()),
            },
            RuleEffect::Boost,
            RuleStrength::Standard,
        ));
        profile.property_rules.push(property_rule(
            "01-reviewed",
            "reviewed",
            PropertyPredicate::DateRange {
                pointer: Some(String::new()),
                min: Some("2026-07-01".into()),
                max: Some("2026-07-31".into()),
            },
            RuleEffect::Boost,
            RuleStrength::Standard,
        ));
        let mut input = input(vec![
            candidate(
                "allowed",
                "neutral",
                "neutral",
                "neutral.md",
                LexicalEvidenceTier::AllTerms,
                3.0,
            ),
            candidate(
                "allowed",
                "invalid-date",
                "invalid-date",
                "invalid-date.md",
                LexicalEvidenceTier::AllTerms,
                2.0,
            ),
            candidate(
                "allowed",
                "typed",
                "typed",
                "typed.md",
                LexicalEvidenceTier::AllTerms,
                1.0,
            ),
        ]);
        input.source_signals = vec![
            observation(
                "allowed",
                "invalid-date",
                None,
                &["limit", "reviewed"],
                vec![
                    scalar("limit", "", RankingScalar::u64(u64::MAX)),
                    scalar("reviewed", "", RankingScalar::String("2026-02-30".into())),
                ],
            ),
            observation(
                "allowed",
                "typed",
                None,
                &["limit", "reviewed"],
                vec![
                    scalar("limit", "", RankingScalar::u64(u64::MAX)),
                    scalar("reviewed", "", RankingScalar::Date("2026-07-31".into())),
                ],
            ),
        ];

        let result = rerank_candidates(&RelevanceProfile::D5cPreviewV1(profile), &input).unwrap();
        assert_eq!(
            paths(&result),
            ["typed.md", "invalid-date.md", "neutral.md"]
        );
    }

    #[test]
    fn hierarchy_depth_authority_and_archive_rules_are_component_aware() {
        let mut profile = D5cRelevanceProfile::preview();
        profile.hierarchy.depth = Some(PathDepthRule {
            id: "00-depth".into(),
            predicate: PathDepthPredicate::AtMost(1),
            effect: RuleEffect::Boost,
            strength: RuleStrength::Low,
        });
        profile.hierarchy.authority_folders = vec![FolderRule {
            id: "01-authority".into(),
            prefix: "projects/active".into(),
            strength: RuleStrength::Standard,
        }];
        profile.hierarchy.archive_folders = vec![FolderRule {
            id: "02-archive".into(),
            prefix: "archive".into(),
            strength: RuleStrength::High,
        }];
        let input = input(vec![
            candidate(
                "allowed",
                "archive",
                "archive",
                "archive/note.md",
                LexicalEvidenceTier::AllTerms,
                4.0,
            ),
            candidate(
                "allowed",
                "archive-old",
                "archive-old",
                "archive-old/note.md",
                LexicalEvidenceTier::AllTerms,
                3.0,
            ),
            candidate(
                "allowed",
                "deep-authority",
                "deep-authority",
                "projects/active/topic/note.md",
                LexicalEvidenceTier::AllTerms,
                2.0,
            ),
            candidate(
                "allowed",
                "shallow",
                "shallow",
                "inbox/note.md",
                LexicalEvidenceTier::AllTerms,
                1.0,
            ),
        ]);

        let result = rerank_candidates(&RelevanceProfile::D5cPreviewV1(profile), &input).unwrap();
        assert_eq!(
            paths(&result),
            [
                "projects/active/topic/note.md",
                "archive-old/note.md",
                "inbox/note.md",
                "archive/note.md"
            ]
        );
    }

    #[test]
    fn stable_ties_preserve_original_lexical_order() {
        let profile = RelevanceProfile::D5cPreviewV1(D5cRelevanceProfile::preview());
        let input = input(vec![
            candidate(
                "allowed",
                "b",
                "chunk-b",
                "b.md",
                LexicalEvidenceTier::AllTerms,
                2.0,
            ),
            candidate(
                "allowed",
                "a",
                "chunk-a",
                "a.md",
                LexicalEvidenceTier::AllTerms,
                1.0,
            ),
        ]);

        let first = rerank_candidates(&profile, &input).unwrap();
        let repeated = rerank_candidates(&profile, &input).unwrap();
        assert_eq!(first, repeated);
        assert_eq!(paths(&first), ["b.md", "a.md"]);
    }

    #[test]
    fn rule_candidate_and_work_bounds_fail_closed() {
        let mut too_many_rules = D5cRelevanceProfile::preview();
        for index in 0..=MAX_PROPERTY_RANKING_RULES {
            too_many_rules.property_rules.push(property_rule(
                &format!("rule-{index:02}"),
                &format!("property-{index:02}"),
                PropertyPredicate::Presence,
                RuleEffect::Boost,
                RuleStrength::Low,
            ));
        }
        assert_eq!(
            too_many_rules.validate().unwrap_err().code,
            "invalid_relevance_profile"
        );

        let mut too_many_candidates = input(
            (0..=MAX_RERANK_CANDIDATES)
                .map(|index| {
                    candidate(
                        "allowed",
                        &format!("source-{index}"),
                        &format!("chunk-{index}"),
                        &format!("note-{index}.md"),
                        LexicalEvidenceTier::AllTerms,
                        1.0,
                    )
                })
                .collect(),
        );
        too_many_candidates.source_signals.clear();
        let error = rerank_candidates(
            &RelevanceProfile::D5cPreviewV1(D5cRelevanceProfile::preview()),
            &too_many_candidates,
        )
        .unwrap_err();
        assert_eq!(error.code, "invalid_rerank_input");

        let mut properties = BTreeMap::new();
        properties.insert(
            "items".to_owned(),
            PropertyValue::Sequence(
                (0..=MAX_PROPERTY_VALUES_PER_SOURCE_OBSERVATION)
                    .map(|value| PropertyValue::I64(value as i64))
                    .collect(),
            ),
        );
        let mut profile = D5cRelevanceProfile::preview();
        profile.property_rules.push(property_rule(
            "items",
            "items",
            PropertyPredicate::Presence,
            RuleEffect::Boost,
            RuleStrength::Low,
        ));
        let error = SourceSignalObservation::from_property_bag(
            source("allowed", "items"),
            None,
            &PropertyBag::from_properties(properties),
            &profile,
        )
        .unwrap_err();
        assert_eq!(error.code, "ranking_work_limit_exceeded");

        let mut work_profile = D5cRelevanceProfile::preview();
        for property_index in 0..MAX_PROPERTY_RANKING_RULES {
            work_profile.property_rules.push(property_rule(
                &format!("work-{property_index:02}"),
                &format!("property-{property_index:02}"),
                PropertyPredicate::Exact {
                    pointer: None,
                    value: RankingScalar::i64(-1),
                },
                RuleEffect::Boost,
                RuleStrength::Low,
            ));
        }
        let candidates: Vec<_> = (0..MAX_RERANK_CANDIDATES)
            .map(|index| {
                candidate(
                    "allowed",
                    &format!("work-source-{index:03}"),
                    &format!("work-chunk-{index:03}"),
                    &format!("work-note-{index:03}.md"),
                    LexicalEvidenceTier::AllTerms,
                    1.0,
                )
            })
            .collect();
        let source_signals = candidates
            .iter()
            .map(|candidate| {
                let present_properties: Vec<_> = (0..MAX_PROPERTY_RANKING_RULES)
                    .map(|index| format!("property-{index:02}"))
                    .collect();
                let property_values = present_properties
                    .iter()
                    .flat_map(|property| {
                        (0..32).map(move |value| {
                            scalar(property, &format!("/{value:02}"), RankingScalar::i64(value))
                        })
                    })
                    .collect();
                SourceSignalObservation {
                    source: candidate.source.clone(),
                    source_mtime_epoch_seconds: None,
                    matched_property_rule_ids: Vec::new(),
                    present_properties,
                    property_values,
                }
            })
            .collect();
        let bounded_input = RerankInput {
            schema_version: RERANK_INPUT_SCHEMA_VERSION,
            query_time_epoch_seconds: 2_000_000_000,
            candidates,
            source_signals,
        };
        let error = rerank_candidates(
            &RelevanceProfile::D5cPreviewV1(work_profile),
            &bounded_input,
        )
        .unwrap_err();
        assert_eq!(error.code, "ranking_work_limit_exceeded");
    }

    #[test]
    fn resource_qualified_source_identity_prevents_cross_scope_fanout() {
        let mut profile = D5cRelevanceProfile::preview();
        profile.property_rules.push(property_rule(
            "priority",
            "priority",
            PropertyPredicate::Presence,
            RuleEffect::Boost,
            RuleStrength::High,
        ));
        let mut input = input(vec![
            candidate(
                "room-a",
                "shared-key",
                "chunk-a",
                "a.md",
                LexicalEvidenceTier::AllTerms,
                2.0,
            ),
            candidate(
                "room-b",
                "shared-key",
                "chunk-b",
                "b.md",
                LexicalEvidenceTier::AllTerms,
                1.0,
            ),
        ]);
        input.source_signals = vec![observation(
            "room-b",
            "shared-key",
            None,
            &["priority"],
            vec![],
        )];

        let result = rerank_candidates(&RelevanceProfile::D5cPreviewV1(profile), &input).unwrap();
        assert_eq!(paths(&result), ["b.md", "a.md"]);
    }

    #[test]
    fn unauthorized_source_observations_cannot_affect_allowed_candidates() {
        let mut profile = D5cRelevanceProfile::preview();
        profile.property_rules.push(property_rule(
            "priority",
            "priority",
            PropertyPredicate::Presence,
            RuleEffect::Boost,
            RuleStrength::High,
        ));
        let candidates = vec![
            candidate(
                "allowed-room",
                "shared-key",
                "allowed-a",
                "a.md",
                LexicalEvidenceTier::AllTerms,
                2.0,
            ),
            candidate(
                "allowed-room",
                "other-key",
                "allowed-b",
                "b.md",
                LexicalEvidenceTier::AllTerms,
                1.0,
            ),
        ];
        let baseline = input(candidates.clone());
        let mut with_forbidden_signal = input(candidates);
        with_forbidden_signal.source_signals = vec![observation(
            "forbidden-room",
            "shared-key",
            None,
            &["priority"],
            vec![],
        )];
        let selection = RelevanceProfile::D5cPreviewV1(profile);

        let baseline = rerank_candidates(&selection, &baseline).unwrap();
        let with_forbidden_signal = rerank_candidates(&selection, &with_forbidden_signal).unwrap();
        assert_eq!(baseline, with_forbidden_signal);
    }

    #[test]
    fn profile_and_input_serialization_are_deterministic_and_strict() {
        let mut profile = D5cRelevanceProfile::preview();
        profile.property_rules.push(property_rule(
            "reviewed",
            "reviewed",
            PropertyPredicate::DateRange {
                pointer: Some(String::new()),
                min: Some("2026-01-01".into()),
                max: Some("2026-12-31".into()),
            },
            RuleEffect::Boost,
            RuleStrength::Standard,
        ));
        profile.validate().unwrap();
        let first = serde_json::to_string(&profile).unwrap();
        let restored: D5cRelevanceProfile = serde_json::from_str(&first).unwrap();
        assert_eq!(serde_json::to_string(&restored).unwrap(), first);
        assert!(first.starts_with(
            "{\"schema_version\":1,\"profile_id\":\"d5c-preview-v1\",\"retrieval_profile_id\":\"lexical-v1\""
        ));

        let input = input(vec![candidate(
            "allowed",
            "source",
            "chunk",
            "note.md",
            LexicalEvidenceTier::AllTerms,
            1.0,
        )]);
        let serialized = serde_json::to_string(&input).unwrap();
        assert_eq!(
            serde_json::to_string(&serde_json::from_str::<RerankInput>(&serialized).unwrap())
                .unwrap(),
            serialized
        );
        let evidence =
            rerank_candidates(&RelevanceProfile::D5cPreviewV1(profile.clone()), &input).unwrap();
        let evidence = serde_json::to_string(evidence.evidence()).unwrap();
        for forbidden in [
            "reviewed",
            "allowed",
            "\"source\"",
            "source_key",
            "chunk",
            "note.md",
        ] {
            assert!(!evidence.contains(forbidden));
        }
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&evidence)
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "candidate_count".to_owned(),
                "entries".to_owned(),
                "schema_version".to_owned(),
                "source_count".to_owned(),
            ])
        );

        let unknown = first.replacen(
            "\"schema_version\":1",
            "\"schema_version\":1,\"unknown\":true",
            1,
        );
        assert!(serde_json::from_str::<D5cRelevanceProfile>(&unknown).is_err());
        let incompatible = first.replacen("\"schema_version\":1", "\"schema_version\":2", 1);
        assert!(
            serde_json::from_str::<D5cRelevanceProfile>(&incompatible)
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn property_bag_projection_preserves_typed_extremes_dates_and_pointers() {
        let mut nested = BTreeMap::new();
        nested.insert("unsafe".to_owned(), PropertyValue::U64(u64::MAX));
        nested.insert(
            "reviewed".to_owned(),
            PropertyValue::String("2026-07-31".into()),
        );
        nested.insert(
            "invalid-date".to_owned(),
            PropertyValue::String("2026-02-30".into()),
        );
        let mut properties = BTreeMap::new();
        properties.insert("metadata".to_owned(), PropertyValue::Map(nested));
        let mut profile = D5cRelevanceProfile::preview();
        profile.property_rules.push(property_rule(
            "metadata-present",
            "metadata",
            PropertyPredicate::Presence,
            RuleEffect::Boost,
            RuleStrength::Low,
        ));

        let observation = SourceSignalObservation::from_property_bag(
            source("allowed", "source"),
            None,
            &PropertyBag::from_properties(properties),
            &profile,
        )
        .unwrap();
        assert_eq!(observation.present_properties, ["metadata"]);
        assert_eq!(
            observation.property_values,
            [
                scalar(
                    "metadata",
                    "/invalid-date",
                    RankingScalar::String("2026-02-30".into())
                ),
                scalar(
                    "metadata",
                    "/reviewed",
                    RankingScalar::Date("2026-07-31".into())
                ),
                scalar("metadata", "/unsafe", RankingScalar::u64(u64::MAX)),
            ]
        );
    }
}
