// SPDX-License-Identifier: MIT OR Apache-2.0

use kwiry_core::{
    ApiSearchRequest, DaemonStatus, LexicalQueryPlan, SourceDescriptor, SourcePreparation,
    prepare_lexical_query, prepare_source_buffer,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(tag = "encoding", rename_all = "snake_case", deny_unknown_fields)]
enum FixtureContent {
    Utf8 { text: String },
    Bytes { values: Vec<u8> },
    Repeat { text: String, count: usize },
}

impl FixtureContent {
    fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Utf8 { text } => text.into_bytes(),
            Self::Bytes { values } => values,
            Self::Repeat { text, count } => text.repeat(count).into_bytes(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum FixtureCase {
    PrepareSource {
        name: String,
        descriptor: SourceDescriptor,
        content: FixtureContent,
    },
    PrepareQuery {
        name: String,
        query: String,
        #[serde(default)]
        metadata_probe_match: Option<bool>,
    },
    ApiRequest {
        name: String,
        request: ApiSearchRequest,
    },
    DaemonStatus {
        name: String,
        status: DaemonStatus,
    },
}

#[derive(Debug, Serialize)]
struct FixtureOutput {
    name: String,
    #[serde(flatten)]
    result: FixtureResult,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum FixtureResult {
    PreparedSource { preparation: SourcePreparation },
    PreparedQuery { plan: LexicalQueryPlan },
    ApiRequest { request: ApiSearchRequest },
    DaemonStatus { daemon_status: DaemonStatus },
    Error { code: String, message: String },
}

fn execute_case(case: FixtureCase) -> FixtureOutput {
    match case {
        FixtureCase::PrepareSource {
            name,
            descriptor,
            content,
        } => {
            let bytes = content.into_bytes();
            let result = match prepare_source_buffer(&descriptor, &bytes) {
                Ok(preparation) => FixtureResult::PreparedSource { preparation },
                Err(error) => FixtureResult::Error {
                    code: error.code,
                    message: error.message,
                },
            };
            FixtureOutput { name, result }
        }
        FixtureCase::PrepareQuery {
            name,
            query,
            metadata_probe_match,
        } => {
            let result = match prepare_lexical_query(&query) {
                Ok(plan) => FixtureResult::PreparedQuery {
                    plan: match metadata_probe_match {
                        Some(matched) => plan.finalize_metadata_probe(matched),
                        None => plan,
                    },
                },
                Err(error) => FixtureResult::Error {
                    code: error.code,
                    message: error.message,
                },
            };
            FixtureOutput { name, result }
        }
        FixtureCase::ApiRequest { name, request } => FixtureOutput {
            name,
            result: FixtureResult::ApiRequest { request },
        },
        FixtureCase::DaemonStatus { name, status } => FixtureOutput {
            name,
            result: FixtureResult::DaemonStatus {
                daemon_status: status,
            },
        },
    }
}

pub fn run_cases_json(input: &str) -> Result<String, String> {
    let cases: Vec<FixtureCase> =
        serde_json::from_str(input).map_err(|error| format!("input:deserialize: {error}"))?;
    let output: Vec<_> = cases.into_iter().map(execute_case).collect();
    serde_json::to_string(&output).map_err(|error| format!("output:serialize: {error}"))
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    pub fn run_cases(input: &str) -> Result<String, JsValue> {
        super::run_cases_json(input).map_err(|error| JsValue::from_str(&error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kwiry_core::LEXICAL_QUERY_PLAN_SCHEMA_VERSION;

    #[test]
    fn adapter_returns_versioned_portable_data() {
        let input = r#"[{"operation":"prepare_query","name":"identifier","query":"IIA 2 line"}]"#;
        let output = run_cases_json(input).expect("fixture should execute");
        let output: serde_json::Value =
            serde_json::from_str(&output).expect("fixture output should deserialize");
        assert_eq!(
            output[0]["plan"]["schema_version"],
            LEXICAL_QUERY_PLAN_SCHEMA_VERSION
        );
        assert_eq!(output[0]["plan"]["kind"], "identifier");
    }

    #[test]
    fn adapter_keeps_sql_looking_input_inert() {
        let input = r#"[{"operation":"prepare_query","name":"sql","query":"title:notes OR '); DROP TABLE chunks; --"}]"#;
        let output = run_cases_json(input).expect("fixture should execute");
        assert!(output.contains("DROP TABLE"));
        assert!(output.contains("\"status\":\"prepared_query\""));
    }
}
