// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only

//! The TypeScript host mirrors a handful of Rust constants. This is what stops
//! them drifting.
//!
//! `main.ts` computes the core policy hash during `onload()`, before the worker
//! and the WASM module exist, because that hash decides whether a restore is
//! attempted at all. So the preparation schema, the identity schema, the
//! extraction-policy fingerprint, and every per-format identity have to be
//! literals on the TypeScript side — and a literal that nothing checks is a
//! claim, not a fact. These tests read the real `src/source-formats.ts` and
//! compare it against what the adapter actually reports through
//! `abi_identity()`.
//!
//! The per-format identities matter most: each cached `sources` row stores the
//! identity it was built under, and a drifted mirror would let the host reuse
//! rows this adapter's extractors could never have produced.

use std::collections::BTreeMap;
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

/// The `key: "value"` pairs of an exported `Object.freeze({ … })` literal.
///
/// A `BTreeMap` so the comparison is against `serde_json`'s own sorted map and
/// neither side's declaration order can make an unequal key set look equal.
fn exported_object(text: &str, name: &str) -> BTreeMap<String, String> {
    let marker = format!("export const {name}");
    let start = text
        .find(&marker)
        .unwrap_or_else(|| panic!("{name} is not exported from source-formats.ts"));
    let open = text[start..]
        .find('{')
        .expect("the object literal is opened")
        + start;
    let end = text[open..]
        .find("})")
        .expect("the object literal is closed")
        + open;
    let mut entries = BTreeMap::new();
    for line in text[open + 1..end].lines() {
        let line = line.trim().trim_end_matches(',');
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        let value = value.trim().trim_matches('"');
        if key.is_empty() || value.is_empty() {
            continue;
        }
        assert!(
            entries.insert(key.to_owned(), value.to_owned()).is_none(),
            "{name} declares {key} twice"
        );
    }
    assert!(!entries.is_empty(), "{name} has no entries");
    entries
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

/// The per-format identities are what the row predicate compares, so every one
/// of them must equal the adapter's — and the key sets must be equal too, or a
/// format added in Rust could go silently missing from the host's map and its
/// rows would have no identity to be checked against.
#[test]
fn the_mirrored_format_identities_match_the_adapter() {
    let mirrored = exported_object(&source_formats_ts(), "FORMAT_IDENTITIES");
    let identity = identity();
    let reported = identity["format_identities"]
        .as_object()
        .expect("the adapter reports a per-format identity map");

    let mirrored_keys: Vec<&String> = mirrored.keys().collect();
    let reported_keys: Vec<&String> = reported.keys().collect();
    assert_eq!(
        mirrored_keys, reported_keys,
        "source-formats.ts must mirror exactly the formats the adapter compiles"
    );
    for (format, expected) in reported {
        let expected = expected.as_str().expect("an identity is a string");
        assert_eq!(expected.len(), 64, "{format} identity is not a SHA-256 hex");
        assert_eq!(
            mirrored.get(format).map(String::as_str),
            Some(expected),
            "{format} identity must be mirrored exactly"
        );
    }
}

/// The identity *schema* version is core, so the host folds it into the core
/// policy hash: a new component in the per-format digest has to invalidate
/// every row of every format.
#[test]
fn the_mirrored_format_identity_schema_matches_the_adapter() {
    let mirrored = exported_constant(&source_formats_ts(), "FORMAT_IDENTITY_SCHEMA_VERSION");
    assert_eq!(
        mirrored,
        identity()["format_identity_schema_version"].to_string(),
        "source-formats.ts must mirror the identity schema the adapter reports"
    );
}

/// The core policy hash must carry the core facts and *only* the core facts.
/// The extraction policy and the enabled set are deliberately absent: folding
/// either back in would restore the whole-cache invalidation this wave removed.
#[test]
fn the_core_policy_hash_carries_only_core_material() {
    let text = source_formats_ts();
    assert!(
        text.contains("kwiry-source-core-policy-v3"),
        "the domain separator must name the generation of core policy identity"
    );
    assert!(
        text.contains("source-preparation-schema=${SOURCE_PREPARATION_SCHEMA_VERSION}"),
        "the core hash material must include the preparation schema"
    );
    assert!(
        text.contains("format-identity-schema=${FORMAT_IDENTITY_SCHEMA_VERSION}"),
        "the core hash material must include the identity schema version"
    );
    assert!(
        !text.contains("extraction-policy=${EXTRACTION_POLICY_FINGERPRINT}"),
        "the extraction policy is per-format identity now, never core material"
    );
    assert!(
        !text.contains("enabled-formats="),
        "enablement is configuration, never identity material"
    );
}
