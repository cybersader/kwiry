use serde::{Deserialize, Serialize};

use crate::lexical::{normalize_raw, technical_identifiers};

pub const LEXICAL_QUERY_PLAN_SCHEMA_VERSION: u32 = 2;
pub const MAX_QUERY_BYTES: usize = 4_096;
pub const MAX_QUERY_TERMS: usize = 128;

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
pub enum QueryMetadataField {
    Filename,
    Stem,
    Aliases,
    Title,
    Heading,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryMetadataProbe {
    pub query: String,
    pub fields: Vec<QueryMetadataField>,
    pub conjunction: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LexicalQueryPlan {
    pub schema_version: u32,
    pub query: String,
    pub kind: QueryPlanKind,
    pub match_operator: QueryMatchOperator,
    pub terms: Vec<String>,
    pub normalized_exact: Option<String>,
    pub phrase_boost: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_probe: Option<QueryMetadataProbe>,
}

impl LexicalQueryPlan {
    pub fn finalize_metadata_probe(mut self, matched: bool) -> Self {
        if self.metadata_probe.take().is_some() && matched {
            self.kind = QueryPlanKind::Identifier;
            self.match_operator = QueryMatchOperator::All;
        }
        self
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
        ],
        conjunction: true,
    });

    let match_operator = match kind {
        QueryPlanKind::Explicit => QueryMatchOperator::Explicit,
        QueryPlanKind::Ordinary => QueryMatchOperator::Any,
        QueryPlanKind::Identifier => QueryMatchOperator::All,
    };
    let terms = if kind == QueryPlanKind::Explicit {
        Vec::new()
    } else {
        query_terms(query)
    };
    Ok(LexicalQueryPlan {
        schema_version: LEXICAL_QUERY_PLAN_SCHEMA_VERSION,
        query: query.to_owned(),
        kind,
        match_operator,
        terms,
        normalized_exact: normalize_raw(query),
        phrase_boost: query.split_whitespace().count() > 1,
        metadata_probe,
    })
}

fn invalid_query(message: &str) -> QueryPlanError {
    QueryPlanError {
        code: "invalid_query".to_owned(),
        message: message.to_owned(),
    }
}

pub(crate) fn classify_query(query: &str) -> QueryPlanKind {
    if has_explicit_syntax(query) {
        QueryPlanKind::Explicit
    } else if is_identifier_like(query) {
        QueryPlanKind::Identifier
    } else {
        QueryPlanKind::Ordinary
    }
}

pub(crate) fn is_lowercase_identifier_candidate(query: &str) -> bool {
    const ORDINARY_NUMBER_PREFIXES: &[&str] = &[
        "best", "chapter", "episode", "first", "last", "level", "page", "part", "section", "step",
        "top", "volume",
    ];

    let tokens: Vec<_> = query.split_whitespace().collect();
    if tokens.is_empty() || tokens.len() > 6 {
        return false;
    }
    let Some(number_index) = tokens
        .iter()
        .position(|token| token.chars().all(|character| character.is_ascii_digit()))
    else {
        return false;
    };
    tokens[..number_index].iter().any(|token| {
        (2..=4).contains(&token.chars().count())
            && token.chars().all(|character| character.is_alphabetic())
            && !ORDINARY_NUMBER_PREFIXES.contains(&token.to_ascii_lowercase().as_str())
    })
}

fn has_explicit_syntax(query: &str) -> bool {
    if query.chars().any(|character| {
        matches!(
            character,
            '"' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '~' | '*' | '?'
        )
    }) {
        return true;
    }
    const FIELDS: &[&str] = &[
        "filename",
        "stem",
        "aliases",
        "title",
        "heading_text",
        "content",
        "path",
        "vault_id",
        "room",
        "tags",
    ];
    query.split_whitespace().any(|token| {
        matches!(token, "AND" | "OR" | "NOT")
            || token.starts_with('+')
            || token.starts_with('-')
            || FIELDS
                .iter()
                .any(|field| token.starts_with(&format!("{field}:")))
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

fn query_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .filter_map(|term| {
            let term = term.trim_matches(|character: char| !character.is_alphanumeric());
            normalize_raw(term)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(classify_query("CVE-*"), QueryPlanKind::Explicit);
    }

    #[test]
    fn plan_exposes_backend_neutral_boolean_intent() {
        let ordinary = prepare_lexical_query("dungeons and dragons").unwrap();
        assert_eq!(ordinary.kind, QueryPlanKind::Ordinary);
        assert_eq!(ordinary.match_operator, QueryMatchOperator::Any);
        assert_eq!(ordinary.terms, ["dungeons", "and", "dragons"]);

        let identifier = prepare_lexical_query("IIA 2 line").unwrap();
        assert_eq!(identifier.kind, QueryPlanKind::Identifier);
        assert_eq!(identifier.match_operator, QueryMatchOperator::All);
        assert_eq!(identifier.terms, ["iia", "2", "line"]);
    }

    #[test]
    fn lowercase_identifier_candidate_emits_fixed_metadata_probe() {
        let plan = prepare_lexical_query("iia 2 line").unwrap();
        assert_eq!(plan.kind, QueryPlanKind::Ordinary);
        assert_eq!(plan.match_operator, QueryMatchOperator::Any);
        assert_eq!(plan.terms, ["iia", "2", "line"]);
        let probe = plan.metadata_probe.as_ref().unwrap();
        assert!(probe.conjunction);
        assert_eq!(probe.fields.len(), 5);
        let unmatched = plan.clone().finalize_metadata_probe(false);
        assert_eq!(unmatched.kind, QueryPlanKind::Ordinary);
        assert_eq!(unmatched.match_operator, QueryMatchOperator::Any);
        let matched = plan.finalize_metadata_probe(true);
        assert_eq!(matched.kind, QueryPlanKind::Identifier);
        assert_eq!(matched.match_operator, QueryMatchOperator::All);
    }

    #[test]
    fn plan_is_versioned_data_and_sql_like_text_stays_inert() {
        let plan = prepare_lexical_query("title:notes OR '); DROP TABLE chunks; --").unwrap();
        assert_eq!(plan.schema_version, LEXICAL_QUERY_PLAN_SCHEMA_VERSION);
        assert_eq!(plan.kind, QueryPlanKind::Explicit);
        assert_eq!(plan.match_operator, QueryMatchOperator::Explicit);
        assert!(plan.terms.is_empty());
        assert!(plan.query.contains("DROP TABLE"));
    }

    #[test]
    fn rejects_oversized_queries_before_allocating_plan_fields() {
        let bytes = "a".repeat(MAX_QUERY_BYTES + 1);
        let byte_error = prepare_lexical_query(&bytes).unwrap_err();
        assert_eq!(byte_error.code, "invalid_query");
        assert!(byte_error.message.contains("UTF-8 bytes"));

        let terms = std::iter::repeat_n("a", MAX_QUERY_TERMS + 1)
            .collect::<Vec<_>>()
            .join(" ");
        let term_error = prepare_lexical_query(&terms).unwrap_err();
        assert_eq!(term_error.code, "invalid_query");
        assert!(term_error.message.contains("terms"));
    }
}
