// SPDX-License-Identifier: GPL-3.0-only

use std::{env, fs, process};

use kwiry_obsidian_wasm::{abi_identity, finalize_query, prepare_query, prepare_source};
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
    let path = env::args()
        .nth(1)
        .ok_or_else(|| "expected fixture path".to_owned())?;
    let source = fs::read_to_string(path).map_err(|_| "could not read fixtures".to_owned())?;
    let cases: Vec<FixtureCase> =
        serde_json::from_str(&source).map_err(|_| "could not parse fixtures".to_owned())?;
    let output = cases
        .into_iter()
        .map(execute)
        .collect::<Result<Vec<_>, _>>()?;
    println!(
        "{}",
        serde_json::to_string(&output).map_err(|_| "could not serialize output".to_owned())?
    );
    Ok(())
}

fn execute(case: FixtureCase) -> Result<FixtureOutput, String> {
    let (name, raw_output) = match case {
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
    };
    let output = serde_json::from_str(&raw_output)
        .map_err(|_| "adapter returned invalid JSON".to_owned())?;
    Ok(FixtureOutput { name, output })
}
