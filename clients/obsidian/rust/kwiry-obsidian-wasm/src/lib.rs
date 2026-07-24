// SPDX-License-Identifier: GPL-3.0-only

use kwiry_core::{
    LEXICAL_QUERY_PLAN_SCHEMA_VERSION, LexicalQueryPlan, MAX_FILE_BYTES, QueryMatchOperator,
    QueryPlanKind, SOURCE_PREPARATION_SCHEMA_VERSION, SourceDescriptor, SourcePreparation,
    prepare_lexical_query, prepare_source_buffer,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

pub const ADAPTER_ABI_VERSION: u32 = 1;
pub const FTS5_MATCH_PLAN_SCHEMA_VERSION: u32 = 1;
pub const MAX_ADAPTER_REQUEST_BYTES: usize = 16 * 1024;
pub const MAX_SOURCE_BUFFER_BYTES: usize = MAX_FILE_BYTES as usize + 1;
const MAX_MATCH_VALUE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdapterOperation {
    PrepareSource,
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
    pub max_request_bytes: usize,
    pub max_source_buffer_bytes: usize,
    pub operations: [AdapterOperation; 3],
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreparedSourceResult {
    pub preparation: SourcePreparation,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreparedQueryResult {
    pub plan: LexicalQueryPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_probe: Option<Fts5MetadataProbePlan>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Fts5MetadataProbePlanId {
    MetadataProbeV1,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Fts5MetadataProbePlan {
    pub schema_version: u32,
    pub plan_id: Fts5MetadataProbePlanId,
    pub match_value: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Fts5MatchPlanId {
    LexicalAnyV1,
    LexicalAllV1,
    LexicalExplicitV1,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Fts5MatchPlan {
    pub schema_version: u32,
    pub plan_id: Fts5MatchPlanId,
    pub match_value: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FinalizedQueryResult {
    pub plan: LexicalQueryPlan,
    pub match_plan: Fts5MatchPlan,
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
    metadata_probe_matched: bool,
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
        max_request_bytes: MAX_ADAPTER_REQUEST_BYTES,
        max_source_buffer_bytes: MAX_SOURCE_BUFFER_BYTES,
        operations: [
            AdapterOperation::PrepareSource,
            AdapterOperation::PrepareQuery,
            AdapterOperation::FinalizeQuery,
        ],
    })
    .unwrap_or_else(|_| "{\"abi_version\":1,\"adapter\":\"kwiry-obsidian-wasm\"}".to_owned())
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
        Ok(plan) => match metadata_probe_plan(&plan) {
            Ok(metadata_probe) => success_response(
                operation,
                PreparedQueryResult {
                    plan,
                    metadata_probe,
                },
            ),
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
    if prepared.metadata_probe.is_none() && request.metadata_probe_matched {
        return error_response(
            operation,
            adapter_error(
                "invalid_request",
                "Metadata probe result was not expected for this query.",
            ),
        );
    }

    let plan = prepared.finalize_metadata_probe(request.metadata_probe_matched);
    match fts5_match_plan(&plan) {
        Ok(match_plan) => success_response(operation, FinalizedQueryResult { plan, match_plan }),
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

fn metadata_probe_plan(
    plan: &LexicalQueryPlan,
) -> Result<Option<Fts5MetadataProbePlan>, AdapterError> {
    if plan.metadata_probe.is_none() {
        return Ok(None);
    }
    let terms = match_terms(&plan.terms, "AND")?;
    bounded_match_value(format!(
        "{{filename stem aliases title heading_text}} : ({terms})"
    ))
    .map(|match_value| {
        Some(Fts5MetadataProbePlan {
            schema_version: FTS5_MATCH_PLAN_SCHEMA_VERSION,
            plan_id: Fts5MetadataProbePlanId::MetadataProbeV1,
            match_value,
        })
    })
}

fn fts5_match_plan(plan: &LexicalQueryPlan) -> Result<Fts5MatchPlan, AdapterError> {
    let (plan_id, match_value) = match (plan.kind, plan.match_operator) {
        (QueryPlanKind::Ordinary, QueryMatchOperator::Any) => (
            Fts5MatchPlanId::LexicalAnyV1,
            match_terms(&plan.terms, "OR")?,
        ),
        (QueryPlanKind::Identifier, QueryMatchOperator::All) => (
            Fts5MatchPlanId::LexicalAllV1,
            match_terms(&plan.terms, "AND")?,
        ),
        (QueryPlanKind::Explicit, QueryMatchOperator::Explicit) => (
            Fts5MatchPlanId::LexicalExplicitV1,
            translate_explicit_query(&plan.query)?,
        ),
        _ => {
            return Err(adapter_error(
                "invalid_query_plan",
                "Query plan kind and match operator do not agree.",
            ));
        }
    };
    Ok(Fts5MatchPlan {
        schema_version: FTS5_MATCH_PLAN_SCHEMA_VERSION,
        plan_id,
        match_value: bounded_match_value(match_value)?,
    })
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
        "{\"status\":\"error\",\"abi_version\":1,\"operation\":\"prepare_query\",\"error\":{\"code\":\"serialization_failed\",\"message\":\"Adapter response serialization failed.\"}}".to_owned()
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

    fn finalize_request(query: &str, matched: bool) -> String {
        serde_json::json!({
            "abi_version": ADAPTER_ABI_VERSION,
            "operation": "finalize_query",
            "query": query,
            "metadata_probe_matched": matched,
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
        assert_eq!(
            identity["operations"],
            serde_json::json!(["prepare_source", "prepare_query", "finalize_query"])
        );
    }

    #[test]
    fn ordinary_and_identifier_queries_produce_fixed_match_plans() {
        let ordinary = response(finalize_query(&finalize_request(
            "dungeons and dragons",
            false,
        )));
        assert_eq!(
            ordinary["result"]["match_plan"]["plan_id"],
            "lexical_any_v1"
        );
        assert_eq!(
            ordinary["result"]["match_plan"]["match_value"],
            "\"dungeons\" OR \"and\" OR \"dragons\""
        );

        let identifier = response(finalize_query(&finalize_request("IIA 2 line", false)));
        assert_eq!(
            identifier["result"]["match_plan"]["plan_id"],
            "lexical_all_v1"
        );
        assert_eq!(
            identifier["result"]["match_plan"]["match_value"],
            "\"iia\" AND \"2\" AND \"line\""
        );
    }

    #[test]
    fn metadata_probe_is_fixed_and_finalization_can_promote_to_all() {
        let prepared = response(prepare_query(&query_request("iia 2 line")));
        assert_eq!(prepared["result"]["plan"]["match_operator"], "any");
        assert_eq!(
            prepared["result"]["metadata_probe"]["plan_id"],
            "metadata_probe_v1"
        );
        assert_eq!(
            prepared["result"]["metadata_probe"]["match_value"],
            "{filename stem aliases title heading_text} : (\"iia\" AND \"2\" AND \"line\")"
        );

        let finalized = response(finalize_query(&finalize_request("iia 2 line", true)));
        assert_eq!(finalized["result"]["plan"]["kind"], "identifier");
        assert_eq!(
            finalized["result"]["match_plan"]["plan_id"],
            "lexical_all_v1"
        );
    }

    #[test]
    fn explicit_translation_is_allowlisted_and_sql_looking_input_is_rejected() {
        let phrase = response(finalize_query(&finalize_request(
            "title:\"IIA guide\" OR content:cache*",
            false,
        )));
        assert_eq!(
            phrase["result"]["match_plan"]["plan_id"],
            "lexical_explicit_v1"
        );
        assert_eq!(
            phrase["result"]["match_plan"]["match_value"],
            "(title : \"IIA guide\" OR content : \"cache\"*)"
        );

        let sql = response(finalize_query(&finalize_request(
            "title:notes OR '); DROP TABLE chunks; --",
            false,
        )));
        assert_eq!(sql["status"], "error");
        assert_eq!(sql["error"]["code"], "explicit_query_unsupported");
        assert!(!sql.to_string().contains("DROP TABLE"));
    }

    #[test]
    fn unknown_fields_and_wrong_abi_fail_closed() {
        let unknown = response(prepare_query(
            r#"{"abi_version":1,"operation":"prepare_query","query":"a","extra":true}"#,
        ));
        assert_eq!(unknown["error"]["code"], "invalid_request");

        let abi = response(prepare_query(
            r#"{"abi_version":2,"operation":"prepare_query","query":"a"}"#,
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
