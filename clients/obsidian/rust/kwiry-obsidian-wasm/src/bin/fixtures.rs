// SPDX-License-Identifier: GPL-3.0-only

use std::{env, fs, process};

#[cfg(feature = "internal-docx-extractor")]
use kwiry_obsidian_wasm::internal_docx_extract;
use kwiry_obsidian_wasm::{abi_identity, finalize_query, prepare_query, prepare_source};
#[cfg(feature = "internal-d5c-preview")]
use kwiry_obsidian_wasm::{finalize_d5c_preview, internal_d5c_evaluate, prepare_d5c_preview};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum FixtureCase {
    Identity {
        name: String,
    },
    PrepareSource {
        name: String,
        request: Value,
        content: FixtureContent,
    },
    PrepareQuery {
        name: String,
        request: Value,
    },
    FinalizeQuery {
        name: String,
        request: Value,
    },
    #[cfg(feature = "internal-docx-extractor")]
    InternalDocxExtract {
        name: String,
        request: Value,
        content: FixtureContent,
    },
    #[cfg(feature = "internal-d5c-preview")]
    PrepareD5cPreview {
        name: String,
        request: Value,
    },
    #[cfg(feature = "internal-d5c-preview")]
    FinalizeD5cPreview {
        name: String,
        request: Value,
    },
    #[cfg(feature = "internal-d5c-preview")]
    InternalD5cEvaluate {
        name: String,
        request: Value,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "encoding", rename_all = "snake_case", deny_unknown_fields)]
enum FixtureContent {
    Utf8 { text: String },
    Bytes { values: Vec<u8> },
}

impl FixtureContent {
    fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Utf8 { text } => text.into_bytes(),
            Self::Bytes { values } => values,
        }
    }
}

#[derive(Debug, Serialize)]
struct FixtureOutput {
    name: String,
    output: Value,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("fixture runner failed: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    let path = arguments
        .next()
        .ok_or_else(|| "expected fixture path".to_owned())?;
    let modes = arguments.collect::<Vec<_>>();
    let raw_adapter_output = modes.iter().any(|mode| mode == "--raw-adapter-output");
    let adapter_output_only = modes.iter().any(|mode| mode == "--adapter-output-only");
    let source = fs::read_to_string(path).map_err(|_| "could not read fixtures".to_owned())?;
    let cases: Vec<FixtureCase> =
        serde_json::from_str(&source).map_err(|_| "could not parse fixtures".to_owned())?;
    if adapter_output_only {
        if cases.len() != 1 {
            return Err("adapter-output-only requires exactly one fixture".to_owned());
        }
        let (_, output) = execute_adapter(cases.into_iter().next().expect("one fixture"));
        serde_json::from_str::<Value>(&output)
            .map_err(|_| "adapter returned invalid JSON".to_owned())?;
        println!("{output}");
    } else if raw_adapter_output {
        let output = cases
            .into_iter()
            .map(execute_raw)
            .collect::<Result<Vec<_>, _>>()?;
        println!("[{}]", output.join(","));
    } else {
        let output = cases
            .into_iter()
            .map(execute)
            .collect::<Result<Vec<_>, _>>()?;
        println!(
            "{}",
            serde_json::to_string(&output).map_err(|_| "could not serialize output".to_owned())?
        );
    }
    Ok(())
}

fn execute(case: FixtureCase) -> Result<FixtureOutput, String> {
    let (name, raw_output) = execute_adapter(case);
    let output = serde_json::from_str(&raw_output)
        .map_err(|_| "adapter returned invalid JSON".to_owned())?;
    Ok(FixtureOutput { name, output })
}

fn execute_raw(case: FixtureCase) -> Result<String, String> {
    let (name, raw_output) = execute_adapter(case);
    serde_json::from_str::<Value>(&raw_output)
        .map_err(|_| "adapter returned invalid JSON".to_owned())?;
    let name = serde_json::to_string(&name).map_err(|_| "could not serialize name".to_owned())?;
    Ok(format!(r#"{{"name":{name},"output":{raw_output}}}"#))
}

fn execute_adapter(case: FixtureCase) -> (String, String) {
    match case {
        FixtureCase::Identity { name } => (name, abi_identity()),
        FixtureCase::PrepareSource {
            name,
            request,
            content,
        } => (
            name,
            prepare_source(&request.to_string(), content.into_bytes()),
        ),
        FixtureCase::PrepareQuery { name, request } => (name, prepare_query(&request.to_string())),
        FixtureCase::FinalizeQuery { name, request } => {
            (name, finalize_query(&request.to_string()))
        }
        #[cfg(feature = "internal-docx-extractor")]
        FixtureCase::InternalDocxExtract {
            name,
            request,
            content,
        } => (
            name,
            internal_docx_extract(&request.to_string(), content.into_bytes()),
        ),
        #[cfg(feature = "internal-d5c-preview")]
        FixtureCase::PrepareD5cPreview { name, request } => {
            (name, prepare_d5c_preview(&request.to_string()))
        }
        #[cfg(feature = "internal-d5c-preview")]
        FixtureCase::FinalizeD5cPreview { name, request } => {
            (name, finalize_d5c_preview(&request.to_string()))
        }
        #[cfg(feature = "internal-d5c-preview")]
        FixtureCase::InternalD5cEvaluate { name, request } => {
            (name, internal_d5c_evaluate(&request.to_string()))
        }
    }
}
