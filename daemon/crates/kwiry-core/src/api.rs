use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::SearchHit;

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;
const FRONTMATTER_FILTERS: &[&str] = &["title", "description", "status", "date"];

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Lexical,
    Semantic,
    #[default]
    Hybrid,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SearchFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub frontmatter_equals: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiSearchRequest {
    pub q: String,
    #[serde(default)]
    pub mode: SearchMode,
    #[serde(default)]
    pub filters: SearchFilters,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

impl ApiSearchRequest {
    /// Vertical 2 behavior: only explicit lexical mode is served.
    pub fn validate_vertical_2(&self) -> std::result::Result<(), ApiRequestError> {
        self.validate(false)
    }

    /// `semantic_available` reflects whether an embedding model is loaded;
    /// without one, semantic and hybrid stay explicitly unavailable.
    pub fn validate(&self, semantic_available: bool) -> std::result::Result<(), ApiRequestError> {
        if self.q.trim().is_empty() {
            return Err(ApiRequestError::new(
                "invalid_query",
                "q must contain non-whitespace text",
            ));
        }
        if self.mode != SearchMode::Lexical && !semantic_available {
            return Err(ApiRequestError::new(
                "mode_unavailable",
                "semantic and hybrid modes require the daemon to run with an embedding model; use mode \"lexical\"",
            ));
        }
        if !(1..=MAX_LIMIT).contains(&self.limit) {
            return Err(ApiRequestError::new(
                "invalid_request",
                "limit must be between 1 and 100",
            ));
        }
        if self.cursor.is_some() {
            return Err(ApiRequestError::new(
                "cursor_unavailable",
                "cursor pagination is not available until a later vertical",
            ));
        }
        validate_optional_filter("vault_id", self.filters.vault_id.as_deref())?;
        validate_optional_filter("room", self.filters.room.as_deref())?;
        if let Some(prefix) = self.filters.path_prefix.as_deref() {
            validate_path_prefix(prefix)?;
        }
        for tag in &self.filters.tags {
            if tag.trim().is_empty() {
                return Err(ApiRequestError::new(
                    "invalid_filter",
                    "tags must not contain empty values",
                ));
            }
        }
        for key in self.filters.frontmatter_equals.keys() {
            if !FRONTMATTER_FILTERS.contains(&key.as_str()) {
                return Err(ApiRequestError::new(
                    "invalid_filter",
                    format!("unsupported frontmatter filter: {key}"),
                ));
            }
        }
        Ok(())
    }
}

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

fn validate_optional_filter(
    name: &str,
    value: Option<&str>,
) -> std::result::Result<(), ApiRequestError> {
    if value.is_some_and(|value| value.trim().is_empty()) {
        return Err(ApiRequestError::new(
            "invalid_filter",
            format!("{name} must not be empty"),
        ));
    }
    Ok(())
}

fn validate_path_prefix(prefix: &str) -> std::result::Result<(), ApiRequestError> {
    if prefix.is_empty()
        || prefix.starts_with('/')
        || prefix.contains('\\')
        || prefix.contains('\0')
        || prefix.split('/').any(|component| component == "..")
    {
        return Err(ApiRequestError::new(
            "invalid_filter",
            "path_prefix must be a normalized vault-relative forward-slash path",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiSearchResponse {
    pub hits: Vec<SearchHit>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiErrorEnvelope {
    pub error: ApiErrorBody,
}

impl ApiErrorEnvelope {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: ApiErrorBody {
                code: code.into(),
                message: message.into(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRequestError {
    pub code: &'static str,
    pub message: String,
}

impl ApiRequestError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: String,
}

impl Default for HealthResponse {
    fn default() -> Self {
        Self {
            status: "ok".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_mode_remains_hybrid_and_is_unavailable_in_vertical_two() {
        let request: ApiSearchRequest = serde_json::from_str(r#"{"q":"notes"}"#).unwrap();
        assert_eq!(request.mode, SearchMode::Hybrid);
        let error = request.validate_vertical_2().unwrap_err();
        assert_eq!(error.code, "mode_unavailable");
    }

    #[test]
    fn lexical_request_defaults_and_validates() {
        let request: ApiSearchRequest =
            serde_json::from_str(r#"{"q":"notes","mode":"lexical"}"#).unwrap();
        assert_eq!(request.limit, 20);
        assert_eq!(request.filters, SearchFilters::default());
        request.validate_vertical_2().unwrap();
    }

    #[test]
    fn invalid_prefix_and_frontmatter_filter_are_rejected() {
        let prefix: ApiSearchRequest = serde_json::from_str(
            r#"{"q":"notes","mode":"lexical","filters":{"path_prefix":"../secret"}}"#,
        )
        .unwrap();
        assert_eq!(
            prefix.validate_vertical_2().unwrap_err().code,
            "invalid_filter"
        );

        let field: ApiSearchRequest = serde_json::from_str(
            r#"{"q":"notes","mode":"lexical","filters":{"frontmatter_equals":{"owner":"me"}}}"#,
        )
        .unwrap();
        assert_eq!(
            field.validate_vertical_2().unwrap_err().code,
            "invalid_filter"
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let error = serde_json::from_str::<ApiSearchRequest>(
            r#"{"q":"notes","mode":"lexical","surprise":true}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn search_response_shape_excludes_internal_retrieval_metadata() {
        let response = ApiSearchResponse {
            hits: vec![SearchHit {
                chunk_id: "chunk-1".into(),
                vault_id: "notes".into(),
                path: "folder/note.md".into(),
                heading_path: vec!["Heading".into()],
                score: 1.5,
                excerpt: "excerpt".into(),
                frontmatter: crate::model::Frontmatter::default(),
            }],
            next_cursor: None,
        };

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({
                "hits": [{
                    "chunk_id": "chunk-1",
                    "vault_id": "notes",
                    "path": "folder/note.md",
                    "heading_path": ["Heading"],
                    "score": 1.5,
                    "excerpt": "excerpt",
                    "frontmatter": {}
                }],
                "next_cursor": null
            })
        );
    }

    #[test]
    fn semantic_and_hybrid_validate_only_with_a_model() {
        for mode in ["semantic", "hybrid"] {
            let request: ApiSearchRequest =
                serde_json::from_str(&format!(r#"{{"q":"notes","mode":"{mode}"}}"#)).unwrap();
            let error = request.validate(false).unwrap_err();
            assert_eq!(error.code, "mode_unavailable");
            request.validate(true).unwrap();
        }
        // Lexical validates regardless of model availability.
        let lexical: ApiSearchRequest =
            serde_json::from_str(r#"{"q":"notes","mode":"lexical"}"#).unwrap();
        lexical.validate(false).unwrap();
        lexical.validate(true).unwrap();
    }
}
