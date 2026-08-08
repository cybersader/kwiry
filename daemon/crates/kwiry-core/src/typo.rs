// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::query::{
    MAX_QUERY_BYTES, QueryAssistanceEligibility, QueryPlanKind, prepare_lexical_query,
};

pub const TYPO_SUGGESTION_SCHEMA_VERSION: u32 = 1;
pub const TYPO_PREFIX_CHARS: usize = 4;
pub const TYPO_MIN_TERM_CHARS: usize = 5;
pub const TYPO_MAX_TERM_BYTES: usize = 48;
pub const TYPO_MAX_VOCABULARY_CANDIDATES: usize = 40;
pub const TYPO_MAX_CANDIDATE_BYTES: usize = 96;
pub const TYPO_MAX_EDIT_DISTANCE: usize = 1;
pub const TYPO_MAX_OUTPUT_SUGGESTIONS: usize = 1;
pub const TYPO_MAX_WORK_UNITS: usize = TYPO_MAX_VOCABULARY_CANDIDATES * TYPO_MAX_CANDIDATE_BYTES;
pub const TYPO_PREFIX_LIMITATION: &str = "Bounded prefix vocabulary only considers terms sharing the first four ASCII characters; it cannot catch early-character errors such as rettrieval.";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TypoSuggestionDisposition {
    Probe,
    ExplicitSyntaxBypass,
    Ineligible,
    Suggestion,
    NoCandidate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TypoSuggestionBounds {
    pub prefix_chars: usize,
    pub max_term_bytes: usize,
    pub max_vocabulary_candidates: usize,
    pub max_candidate_bytes: usize,
    pub max_edit_distance: usize,
    pub max_output_suggestions: usize,
    pub max_work_units: usize,
}

impl TypoSuggestionBounds {
    fn prototype() -> Self {
        Self {
            prefix_chars: TYPO_PREFIX_CHARS,
            max_term_bytes: TYPO_MAX_TERM_BYTES,
            max_vocabulary_candidates: TYPO_MAX_VOCABULARY_CANDIDATES,
            max_candidate_bytes: TYPO_MAX_CANDIDATE_BYTES,
            max_edit_distance: TYPO_MAX_EDIT_DISTANCE,
            max_output_suggestions: TYPO_MAX_OUTPUT_SUGGESTIONS,
            max_work_units: TYPO_MAX_WORK_UNITS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TypoSuggestionPlan {
    pub schema_version: u32,
    pub disposition: TypoSuggestionDisposition,
    pub query: String,
    pub term: Option<String>,
    pub prefix_pattern: Option<String>,
    pub bounds: TypoSuggestionBounds,
    pub limitation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TypoVocabularyCandidate {
    pub term: String,
    pub document_frequency: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TypoSuggestionResult {
    pub schema_version: u32,
    pub disposition: TypoSuggestionDisposition,
    pub original_query: String,
    pub suggested_query: Option<String>,
    pub candidates_examined: usize,
    pub work_units: usize,
    pub bounds: TypoSuggestionBounds,
    pub limitation: String,
}

pub fn prepare_typo_suggestion(query: &str) -> TypoSuggestionPlan {
    let bounds = TypoSuggestionBounds::prototype();
    let base = |disposition, term, prefix_pattern| TypoSuggestionPlan {
        schema_version: TYPO_SUGGESTION_SCHEMA_VERSION,
        disposition,
        query: query.to_owned(),
        term,
        prefix_pattern,
        bounds: bounds.clone(),
        limitation: TYPO_PREFIX_LIMITATION.to_owned(),
    };

    if query.is_empty() || query.len() > MAX_QUERY_BYTES {
        return base(TypoSuggestionDisposition::Ineligible, None, None);
    }
    let Ok(plan) = prepare_lexical_query(query) else {
        return base(TypoSuggestionDisposition::Ineligible, None, None);
    };
    if plan.assistance == QueryAssistanceEligibility::ExplicitSyntaxBypass {
        return base(TypoSuggestionDisposition::ExplicitSyntaxBypass, None, None);
    }
    if plan.kind != QueryPlanKind::Ordinary || plan.terms.len() != 1 {
        return base(TypoSuggestionDisposition::Ineligible, None, None);
    }
    let term = plan.terms[0].clone();
    let authored = query.trim();
    if term.len() > TYPO_MAX_TERM_BYTES
        || term.chars().count() < TYPO_MIN_TERM_CHARS
        || !term.bytes().all(|byte| byte.is_ascii_lowercase())
        || !authored.bytes().all(|byte| byte.is_ascii_alphabetic())
        || !authored.eq_ignore_ascii_case(&term)
    {
        return base(TypoSuggestionDisposition::Ineligible, None, None);
    }
    let prefix: String = term.chars().take(TYPO_PREFIX_CHARS).collect();
    if prefix.chars().count() != TYPO_PREFIX_CHARS {
        return base(TypoSuggestionDisposition::Ineligible, None, None);
    }
    base(
        TypoSuggestionDisposition::Probe,
        Some(term),
        Some(format!("{prefix}%")),
    )
}

pub fn finalize_typo_suggestion(
    query: &str,
    candidates: Vec<TypoVocabularyCandidate>,
) -> Result<TypoSuggestionResult, String> {
    let plan = prepare_typo_suggestion(query);
    if candidates.len() > TYPO_MAX_VOCABULARY_CANDIDATES {
        return Err("typo vocabulary candidate count exceeds its bound".to_owned());
    }
    if plan.disposition != TypoSuggestionDisposition::Probe {
        if !candidates.is_empty() {
            return Err("ineligible typo suggestion received vocabulary candidates".to_owned());
        }
        return Ok(result_from_plan(plan, None, 0, 0));
    }

    let term = plan.term.as_deref().expect("probe plan has a term");
    let prefix = plan
        .prefix_pattern
        .as_deref()
        .and_then(|pattern| pattern.strip_suffix('%'))
        .expect("probe plan has a bounded prefix");
    let mut seen = BTreeSet::new();
    let mut ranked: Vec<(String, u64)> = Vec::new();
    let mut work_units = 0usize;
    for candidate in &candidates {
        if candidate.document_frequency == 0
            || candidate.term.is_empty()
            || candidate.term.len() > TYPO_MAX_CANDIDATE_BYTES
            || !candidate.term.bytes().all(|byte| byte.is_ascii_lowercase())
            || !candidate.term.starts_with(prefix)
            || !seen.insert(candidate.term.clone())
        {
            return Err("typo vocabulary candidate is invalid".to_owned());
        }
        let (matches, work) = one_edit_apart(term.as_bytes(), candidate.term.as_bytes());
        work_units = work_units
            .checked_add(work)
            .ok_or_else(|| "typo suggestion work counter overflowed".to_owned())?;
        if work_units > TYPO_MAX_WORK_UNITS {
            return Err("typo suggestion work exceeds its bound".to_owned());
        }
        if matches {
            ranked.push((candidate.term.clone(), candidate.document_frequency));
        }
    }
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let suggestion = ranked.first().map(|(candidate, _)| candidate.clone());
    Ok(result_from_plan(
        plan,
        suggestion,
        candidates.len(),
        work_units,
    ))
}

fn result_from_plan(
    plan: TypoSuggestionPlan,
    suggestion: Option<String>,
    candidates_examined: usize,
    work_units: usize,
) -> TypoSuggestionResult {
    let disposition = match plan.disposition {
        TypoSuggestionDisposition::Probe if suggestion.is_some() => {
            TypoSuggestionDisposition::Suggestion
        }
        TypoSuggestionDisposition::Probe => TypoSuggestionDisposition::NoCandidate,
        other => other,
    };
    TypoSuggestionResult {
        schema_version: TYPO_SUGGESTION_SCHEMA_VERSION,
        disposition,
        original_query: plan.query,
        suggested_query: suggestion,
        candidates_examined,
        work_units,
        bounds: plan.bounds,
        limitation: plan.limitation,
    }
}

fn one_edit_apart(left: &[u8], right: &[u8]) -> (bool, usize) {
    if left == right || left.len().abs_diff(right.len()) > TYPO_MAX_EDIT_DISTANCE {
        return (false, 1);
    }
    let mut left_index = 0usize;
    let mut right_index = 0usize;
    let mut edits = 0usize;
    let mut work = 0usize;
    while left_index < left.len() && right_index < right.len() {
        work += 1;
        if left[left_index] == right[right_index] {
            left_index += 1;
            right_index += 1;
            continue;
        }
        edits += 1;
        if edits > TYPO_MAX_EDIT_DISTANCE {
            return (false, work);
        }
        match left.len().cmp(&right.len()) {
            std::cmp::Ordering::Equal => {
                left_index += 1;
                right_index += 1;
            }
            std::cmp::Ordering::Greater => left_index += 1,
            std::cmp::Ordering::Less => right_index += 1,
        }
    }
    if left_index < left.len() || right_index < right.len() {
        edits += 1;
        work += 1;
    }
    (edits == TYPO_MAX_EDIT_DISTANCE, work)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(term: &str, document_frequency: u64) -> TypoVocabularyCandidate {
        TypoVocabularyCandidate {
            term: term.to_owned(),
            document_frequency,
        }
    }

    #[test]
    fn explicit_and_non_single_ascii_queries_bypass_the_prototype() {
        for query in ["title:retrievel", "retrievel?"] {
            assert_eq!(
                prepare_typo_suggestion(query).disposition,
                TypoSuggestionDisposition::ExplicitSyntaxBypass,
                "{query}"
            );
        }
        for query in ["retrievel cache", "résumé", "CVE-2026-1234", "abc"] {
            assert_eq!(
                prepare_typo_suggestion(query).disposition,
                TypoSuggestionDisposition::Ineligible,
                "{query}"
            );
        }
    }

    #[test]
    fn bounded_edit_distance_one_prefers_frequency_then_lexical_identity() {
        let plan = prepare_typo_suggestion("retrievel");
        assert_eq!(plan.disposition, TypoSuggestionDisposition::Probe);
        assert_eq!(plan.prefix_pattern.as_deref(), Some("retr%"));
        let result = finalize_typo_suggestion(
            "retrievel",
            vec![
                candidate("retriever", 2),
                candidate("retrieval", 7),
                candidate("retrieved", 7),
            ],
        )
        .unwrap();
        assert_eq!(result.disposition, TypoSuggestionDisposition::Suggestion);
        assert_eq!(result.suggested_query.as_deref(), Some("retrieval"));
        assert_eq!(result.candidates_examined, 3);
        assert!(result.work_units <= TYPO_MAX_WORK_UNITS);
        assert_eq!(result.bounds.max_edit_distance, 1);
        assert_eq!(result.bounds.max_output_suggestions, 1);
    }

    #[test]
    fn early_character_limitation_is_visible_and_reproducible() {
        let plan = prepare_typo_suggestion("rettrieval");
        assert_eq!(plan.prefix_pattern.as_deref(), Some("rett%"));
        assert_eq!(plan.limitation, TYPO_PREFIX_LIMITATION);
        let result = finalize_typo_suggestion("rettrieval", Vec::new()).unwrap();
        assert_eq!(result.disposition, TypoSuggestionDisposition::NoCandidate);
        assert_eq!(result.suggested_query, None);
        assert!(result.limitation.contains("rettrieval"));
    }

    #[test]
    fn candidate_count_identity_and_memory_inputs_are_fail_closed() {
        let too_many = std::iter::repeat_with(|| candidate("retrieval", 1))
            .take(TYPO_MAX_VOCABULARY_CANDIDATES + 1)
            .collect();
        assert!(finalize_typo_suggestion("retrievel", too_many).is_err());
        assert!(
            finalize_typo_suggestion(
                "retrievel",
                vec![candidate(&"r".repeat(TYPO_MAX_CANDIDATE_BYTES + 1), 1)],
            )
            .is_err()
        );
        assert!(
            finalize_typo_suggestion(
                "retrievel",
                vec![candidate("retrieval", 1), candidate("retrieval", 2)],
            )
            .is_err()
        );
    }
}
