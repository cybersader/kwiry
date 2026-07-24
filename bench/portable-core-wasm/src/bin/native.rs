// SPDX-License-Identifier: MIT OR Apache-2.0

use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = env::args_os().nth(1) else {
        eprintln!("usage: kwiry-portable-core-native <fixture-json>");
        return ExitCode::from(2);
    };

    let input = match fs::read_to_string(path) {
        Ok(input) => input,
        Err(error) => {
            eprintln!("fixture:read: {error}");
            return ExitCode::FAILURE;
        }
    };
    match kwiry_portable_core_wasm_probe::run_cases_json(&input) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
