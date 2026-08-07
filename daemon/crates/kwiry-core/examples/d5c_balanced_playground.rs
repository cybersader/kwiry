// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use kwiry_core::{
    BalancedComparisonEnvelope, BalancedEvaluationDisposition, BalancedPlaygroundCase,
    BalancedPlaygroundConfiguration, ComparisonRanking, EvaluationSignalState, QualifiedSourceId,
    SearchRuntime,
};
use serde::{Deserialize, Serialize};

const CORPUS_SCHEMA_VERSION: u32 = 1;
// The adapter owns this number, but it lives in the crate that depends on this
// one, so the value cannot be imported and has to be restated here. Nothing
// makes the two agree, and they silently stopped agreeing when admitting PDF
// moved the ABI to 3: the corpus was carried forward and this copy was not, so
// every evaluation was rejected as incompatible. Advance this whenever
// ADAPTER_ABI_VERSION in clients/obsidian/rust/kwiry-obsidian-wasm/src/lib.rs
// moves.
const ADAPTER_ABI_VERSION: u32 = 3;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaygroundCorpus {
    schema_version: u32,
    scenario_id: String,
    sources: Vec<FixtureSource>,
    evaluations: Vec<FixtureEvaluation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureSource {
    source: QualifiedSourceId,
    path: String,
    provider: FixtureProviderFacts,
    chunks: Vec<FixtureChunk>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum FixtureProviderFacts {
    Markdown {
        provider_id: String,
        fixture_path: String,
    },
    GoogleDocs {
        provider_id: String,
        drive_scope: String,
        modified_time_epoch_seconds: String,
        properties: BTreeMap<String, serde_json::Value>,
    },
    Canva {
        provider_id: String,
        team_scope: String,
        updated_time_epoch_seconds: String,
        properties: BTreeMap<String, serde_json::Value>,
    },
}

impl FixtureProviderFacts {
    fn provider_id(&self) -> &str {
        match self {
            Self::Markdown { provider_id, .. }
            | Self::GoogleDocs { provider_id, .. }
            | Self::Canva { provider_id, .. } => provider_id,
        }
    }

    fn provider_kind(&self) -> &'static str {
        match self {
            Self::Markdown { .. } => "markdown",
            Self::GoogleDocs { .. } => "google_docs",
            Self::Canva { .. } => "canva",
        }
    }

    fn properties(&self) -> Option<&BTreeMap<String, serde_json::Value>> {
        match self {
            Self::Markdown { .. } => None,
            Self::GoogleDocs { properties, .. } | Self::Canva { properties, .. } => {
                Some(properties)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureChunk {
    chunk_id: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum FixtureEngine {
    NativeTantivy,
    PortableFts5,
    SharedContract,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureEvaluation {
    id: String,
    engine: FixtureEngine,
    expected_disposition: ExpectedDisposition,
    request: EvaluationRequest,
    #[serde(default)]
    judgments: Vec<CandidateJudgment>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ExpectedDisposition {
    StrictBalanced,
    NeutralizedCounterfactual,
    Fatal,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvaluationRequest {
    abi_version: u32,
    operation: EvaluationOperation,
    configuration: BalancedPlaygroundConfiguration,
    case: BalancedPlaygroundCase,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EvaluationOperation {
    InternalD5cEvaluate,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateJudgment {
    candidate_ordinal: usize,
    grade: u8,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct PlaygroundReport {
    schema_version: u32,
    scenario_id: String,
    source_count: usize,
    provider_counts: BTreeMap<&'static str, usize>,
    evaluation_count: usize,
    engine_metrics: Vec<EngineMetrics>,
    evaluations: Vec<EvaluationReport>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct EvaluationReport {
    id: String,
    engine: FixtureEngine,
    envelope: BalancedComparisonEnvelope,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct EngineMetrics {
    engine: FixtureEngine,
    judged_cases: usize,
    text_mean_reciprocal_rank: f64,
    balanced_mean_reciprocal_rank: f64,
    text_mean_ndcg_at_5: f64,
    balanced_mean_ndcg_at_5: f64,
}

#[derive(Default)]
struct MetricAccumulator {
    cases: usize,
    text_rr: f64,
    balanced_rr: f64,
    text_ndcg: f64,
    balanced_ndcg: f64,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("D5C Balanced playground failed: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let path = env::args()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| "expected a playground corpus path".to_owned())?;
    let source = fs::read_to_string(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let corpus: PlaygroundCorpus = serde_json::from_str(&source)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))?;
    validate_corpus(&corpus, path.parent().unwrap_or_else(|| Path::new(".")))?;

    let runtime = SearchRuntime::new();
    let mut accumulators = BTreeMap::<FixtureEngine, MetricAccumulator>::new();
    let mut evaluations = Vec::with_capacity(corpus.evaluations.len());
    for evaluation in corpus.evaluations {
        let envelope = runtime
            .internal_d5c_evaluate(&evaluation.request.configuration, &evaluation.request.case);
        validate_disposition(evaluation.expected_disposition, &envelope, &evaluation.id)?;
        if !evaluation.judgments.is_empty()
            && matches!(
                evaluation.engine,
                FixtureEngine::NativeTantivy | FixtureEngine::PortableFts5
            )
        {
            let balanced = envelope
                .balanced_results
                .as_ref()
                .ok_or_else(|| format!("{} is judged but has no Balanced result", evaluation.id))?;
            let accumulator = accumulators.entry(evaluation.engine).or_default();
            accumulator.cases += 1;
            accumulator.text_rr += reciprocal_rank(&envelope.text_results, &evaluation.judgments);
            accumulator.balanced_rr += reciprocal_rank(balanced, &evaluation.judgments);
            accumulator.text_ndcg += ndcg_at_5(&envelope.text_results, &evaluation.judgments);
            accumulator.balanced_ndcg += ndcg_at_5(balanced, &evaluation.judgments);
        }
        evaluations.push(EvaluationReport {
            id: evaluation.id,
            engine: evaluation.engine,
            envelope,
        });
    }

    let engine_metrics = [FixtureEngine::NativeTantivy, FixtureEngine::PortableFts5]
        .into_iter()
        .map(|engine| {
            let values = accumulators.remove(&engine).unwrap_or_default();
            let denominator = values.cases.max(1) as f64;
            EngineMetrics {
                engine,
                judged_cases: values.cases,
                text_mean_reciprocal_rank: values.text_rr / denominator,
                balanced_mean_reciprocal_rank: values.balanced_rr / denominator,
                text_mean_ndcg_at_5: values.text_ndcg / denominator,
                balanced_mean_ndcg_at_5: values.balanced_ndcg / denominator,
            }
        })
        .collect();
    let provider_counts = corpus
        .sources
        .iter()
        .fold(BTreeMap::new(), |mut counts, source| {
            *counts.entry(source.provider.provider_kind()).or_insert(0) += 1;
            counts
        });
    let report = PlaygroundReport {
        schema_version: CORPUS_SCHEMA_VERSION,
        scenario_id: corpus.scenario_id,
        source_count: corpus.sources.len(),
        provider_counts,
        evaluation_count: evaluations.len(),
        engine_metrics,
        evaluations,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report)
            .map_err(|error| format!("could not serialize report: {error}"))?
    );
    Ok(())
}

fn validate_corpus(corpus: &PlaygroundCorpus, root: &Path) -> Result<(), String> {
    if corpus.schema_version != CORPUS_SCHEMA_VERSION
        || corpus.scenario_id != kwiry_core::BALANCED_PLAYGROUND_SCENARIO_ID
        || corpus.sources.is_empty()
        || corpus.evaluations.is_empty()
    {
        return Err("unsupported or empty playground corpus".to_owned());
    }
    let mut sources = BTreeMap::new();
    let mut provider_ids = BTreeMap::<(&str, &str), BTreeSet<&str>>::new();
    for source in &corpus.sources {
        if source.path.is_empty() || source.chunks.is_empty() {
            return Err("fixture source path and chunks must be nonempty".to_owned());
        }
        if sources.insert(source.source.clone(), source).is_some() {
            return Err("fixture source identity is duplicated".to_owned());
        }
        let mut chunks = BTreeSet::new();
        if source
            .chunks
            .iter()
            .any(|chunk| chunk.chunk_id.is_empty() || !chunks.insert(chunk.chunk_id.as_str()))
        {
            return Err("fixture chunk identity is empty or duplicated".to_owned());
        }
        provider_ids
            .entry((
                source.provider.provider_kind(),
                source.provider.provider_id(),
            ))
            .or_default()
            .insert(source.source.authorization_scope.as_str());
        match &source.provider {
            FixtureProviderFacts::Markdown { fixture_path, .. } => {
                let path = root.join(fixture_path);
                let metadata = fs::metadata(&path).map_err(|error| {
                    format!("missing Markdown fixture {}: {error}", path.display())
                })?;
                if !metadata.is_file() || metadata.len() > kwiry_core::MAX_FILE_BYTES {
                    return Err(format!("Markdown fixture is invalid: {}", path.display()));
                }
            }
            FixtureProviderFacts::GoogleDocs {
                drive_scope,
                modified_time_epoch_seconds,
                properties,
                ..
            } => {
                if drive_scope.is_empty()
                    || !canonical_u64(modified_time_epoch_seconds)
                    || properties.len() > kwiry_core::MAX_BALANCED_PROPERTY_RULES
                {
                    return Err("simulated Google Docs facts are invalid".to_owned());
                }
            }
            FixtureProviderFacts::Canva {
                team_scope,
                updated_time_epoch_seconds,
                properties,
                ..
            } => {
                if team_scope.is_empty()
                    || !canonical_u64(updated_time_epoch_seconds)
                    || properties.len() > kwiry_core::MAX_BALANCED_PROPERTY_RULES
                {
                    return Err("simulated Canva facts are invalid".to_owned());
                }
            }
        }
    }
    if !provider_ids.values().any(|scopes| scopes.len() > 1) {
        return Err("corpus must exercise a provider ID duplicated across scopes".to_owned());
    }

    let mut evaluation_ids = BTreeSet::new();
    for evaluation in &corpus.evaluations {
        if evaluation.id.is_empty() || !evaluation_ids.insert(evaluation.id.as_str()) {
            return Err("fixture evaluation ID is empty or duplicated".to_owned());
        }
        if evaluation.request.abi_version != ADAPTER_ABI_VERSION
            || evaluation.request.operation != EvaluationOperation::InternalD5cEvaluate
            || evaluation.request.case.scenario_id != corpus.scenario_id
            || evaluation.request.configuration.scenario_id != corpus.scenario_id
        {
            return Err(format!("{} has an incompatible request", evaluation.id));
        }
        for judgment in &evaluation.judgments {
            if judgment.grade > 3
                || judgment.candidate_ordinal >= evaluation.request.case.candidates.len()
            {
                return Err(format!("{} has an invalid judgment", evaluation.id));
            }
        }
        if evaluation.expected_disposition != ExpectedDisposition::Fatal {
            for candidate in &evaluation.request.case.candidates {
                let source = sources.get(&candidate.source).ok_or_else(|| {
                    format!("{} references an unknown fixture source", evaluation.id)
                })?;
                if source.path != candidate.path
                    || !source
                        .chunks
                        .iter()
                        .any(|chunk| chunk.chunk_id == candidate.chunk_id)
                {
                    return Err(format!(
                        "{} has an inconsistent source/chunk mapping",
                        evaluation.id
                    ));
                }
            }
            for facts in &evaluation.request.case.source_facts {
                let Some(source) = sources.get(&facts.source) else {
                    continue;
                };
                let Some(properties) = source.provider.properties() else {
                    continue;
                };
                let expected = match properties.get("approved") {
                    Some(serde_json::Value::Bool(true)) => EvaluationSignalState::Matched,
                    Some(_) => EvaluationSignalState::Nonmatched,
                    None => EvaluationSignalState::Absent,
                };
                let observed = facts
                    .property_rules
                    .iter()
                    .find(|signal| signal.rule_id == "30-approved")
                    .map(|signal| signal.state)
                    .ok_or_else(|| {
                        format!("{} omits the simulated property rule", evaluation.id)
                    })?;
                if matches!(
                    observed,
                    EvaluationSignalState::Matched
                        | EvaluationSignalState::Nonmatched
                        | EvaluationSignalState::Absent
                ) && observed != expected
                {
                    return Err(format!(
                        "{} disagrees with its simulated provider facts",
                        evaluation.id
                    ));
                }
            }
        }
    }
    Ok(())
}

fn canonical_u64(value: &str) -> bool {
    value
        .parse::<u64>()
        .is_ok_and(|parsed| parsed.to_string() == value)
}

fn validate_disposition(
    expected: ExpectedDisposition,
    envelope: &BalancedComparisonEnvelope,
    id: &str,
) -> Result<(), String> {
    let actual = match envelope.disposition {
        BalancedEvaluationDisposition::StrictBalanced => ExpectedDisposition::StrictBalanced,
        BalancedEvaluationDisposition::NeutralizedCounterfactual { .. } => {
            ExpectedDisposition::NeutralizedCounterfactual
        }
        BalancedEvaluationDisposition::Fatal { .. } => ExpectedDisposition::Fatal,
    };
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{id} disposition mismatch: {actual:?} != {expected:?}"
        ))
    }
}

fn grade_by_ordinal(judgments: &[CandidateJudgment]) -> BTreeMap<usize, u8> {
    judgments
        .iter()
        .map(|judgment| (judgment.candidate_ordinal, judgment.grade))
        .collect()
}

fn reciprocal_rank(ranking: &ComparisonRanking, judgments: &[CandidateJudgment]) -> f64 {
    let grades = grade_by_ordinal(judgments);
    ranking
        .ordered_candidate_ordinals
        .iter()
        .position(|ordinal| grades.get(ordinal).copied().unwrap_or(0) > 0)
        .map_or(0.0, |rank| 1.0 / (rank + 1) as f64)
}

fn ndcg_at_5(ranking: &ComparisonRanking, judgments: &[CandidateJudgment]) -> f64 {
    let grades = grade_by_ordinal(judgments);
    let dcg = ranking
        .ordered_candidate_ordinals
        .iter()
        .take(5)
        .enumerate()
        .map(|(rank, ordinal)| discounted_gain(grades.get(ordinal).copied().unwrap_or(0), rank))
        .sum::<f64>();
    let mut ideal = judgments
        .iter()
        .map(|judgment| judgment.grade)
        .collect::<Vec<_>>();
    ideal.sort_by(|left, right| right.cmp(left));
    let idcg = ideal
        .into_iter()
        .take(5)
        .enumerate()
        .map(|(rank, grade)| discounted_gain(grade, rank))
        .sum::<f64>();
    if idcg == 0.0 { 0.0 } else { dcg / idcg }
}

fn discounted_gain(grade: u8, rank: usize) -> f64 {
    ((1_u32 << grade) - 1) as f64 / (rank as f64 + 2.0).log2()
}
