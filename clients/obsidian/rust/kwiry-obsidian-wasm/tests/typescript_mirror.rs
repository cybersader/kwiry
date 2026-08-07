// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

//! The TypeScript host mirrors two Rust constants. This is what stops them
//! drifting.
//!
//! `main.ts` computes the cache policy hash during `onload()`, before the
//! worker and the WASM module exist, because that hash decides whether a
//! restore is attempted at all. So the schema version and the extraction-policy
//! fingerprint have to be literals on the TypeScript side — and a literal that
//! nothing checks is a claim, not a fact. These tests read the real
//! `src/source-formats.ts` and compare it against what the adapter actually
//! reports through `abi_identity()`.

use std::path::{Path, PathBuf};

fn source_formats_ts() -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/source-formats.ts");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()))
}

fn identity() -> serde_json::Value {
    serde_json::from_str(&kwiry_obsidian_wasm::abi_identity()).expect("identity is JSON")
}

/// The value assigned to `export const <name> = <value> as const;`.
fn exported_constant(text: &str, name: &str) -> String {
    let marker = format!("export const {name} =");
    let start = text
        .find(&marker)
        .unwrap_or_else(|| panic!("{name} is not exported from source-formats.ts"))
        + marker.len();
    let rest = &text[start..];
    let end = rest.find(';').expect("the declaration is terminated");
    rest[..end]
        .replace("as const", "")
        .replace('"', "")
        .trim()
        .to_owned()
}

#[test]
fn the_mirrored_preparation_schema_matches_the_adapter() {
    let mirrored = exported_constant(&source_formats_ts(), "SOURCE_PREPARATION_SCHEMA_VERSION");
    assert_eq!(
        mirrored,
        identity()["source_preparation_schema_version"].to_string(),
        "source-formats.ts must mirror the schema the adapter reports"
    );
}

#[test]
fn the_mirrored_extraction_policy_fingerprint_matches_the_adapter() {
    let mirrored = exported_constant(&source_formats_ts(), "EXTRACTION_POLICY_FINGERPRINT");
    let reported = identity()["extraction_policy_fingerprint"]
        .as_str()
        .expect("the adapter reports a fingerprint")
        .to_owned();

    assert_eq!(mirrored, reported);
    assert_eq!(mirrored.len(), 64);
}

/// The adapter compiles the portable extractor set and nothing else, so every
/// format it reports is `portable` — PDF included since its admission, because
/// the portable PDF tier is part of `portable`. PDF is called out separately
/// rather than folded into the loop: it is the only format whose profile can
/// vary, so `enhanced` here would mean the WASM build had picked up the
/// daemon-only `native-pdf-extractor`, and `none` would mean it had somehow
/// dropped the reader out of `portable`.
#[test]
fn the_adapter_reports_a_portable_only_policy() {
    let identity = identity();
    let policy = identity["extraction_policy"]
        .as_object()
        .expect("the adapter reports a per-format policy");

    assert_eq!(policy["pdf"], "portable");
    for (format, profile) in policy {
        assert_eq!(profile, "portable", "{format} reported {profile}");
    }
}

/// The policy fingerprint must be part of the cache identity material, or a
/// tier switch would not invalidate a cache image.
#[test]
fn the_policy_fingerprint_is_folded_into_the_cache_policy_hash() {
    let text = source_formats_ts();
    assert!(
        text.contains("extraction-policy=${EXTRACTION_POLICY_FINGERPRINT}"),
        "the policy hash material must include the extraction policy"
    );
    assert!(
        text.contains("kwiry-source-format-policy-v2"),
        "the domain separator must name the generation of policy identity"
    );
}
