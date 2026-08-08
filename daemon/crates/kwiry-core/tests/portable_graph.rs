// SPDX-License-Identifier: MIT OR Apache-2.0

//! The enhanced tier's dependency must never reach the portable graph.
//!
//! The plugin ships a WASM bundle under a hard size ceiling, and the enhanced
//! PDF tier exists because parsing an arbitrary embedded font program does not
//! belong inside Obsidian's worker heap. A feature gate nobody checks is not a
//! gate: these tests read the real manifests and compute the real feature
//! closures, so adding `ttf-parser` to `portable` — or letting the WASM crate
//! switch on `native-pdf-extractor` — fails the suite rather than the bundle
//! budget.
//!
//! The last test states the other half deliberately. `encoding_rs`, which the
//! shared legacy-CMap decoding uses, **is** in the portable graph already, via
//! `quick-xml`. That is precisely why legacy CJK CMaps are decoded by both
//! tiers instead of being an enhanced-only capability, and asserting it here
//! keeps that reasoning from quietly becoming false.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The dependency the enhanced tier adds, and the only one it adds.
const ENHANCED_ONLY_DEPENDENCY: &str = "ttf-parser";
const ENHANCED_FEATURE: &str = "native-pdf-extractor";

fn repository_root() -> PathBuf {
    // <root>/daemon/crates/kwiry-core
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("the crate sits three levels below the repository root")
        .to_path_buf()
}

fn manifest(relative: &str) -> toml::Table {
    let path = repository_root().join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
    text.parse().expect("manifest is valid TOML")
}

fn feature_table(manifest: &toml::Table) -> BTreeMap<String, Vec<String>> {
    manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .map(|features| {
            features
                .iter()
                .map(|(name, entries)| {
                    let entries = entries
                        .as_array()
                        .expect("a feature is a list")
                        .iter()
                        .map(|entry| {
                            entry
                                .as_str()
                                .expect("a feature entry is a string")
                                .to_owned()
                        })
                        .collect();
                    (name.clone(), entries)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Everything `roots` turns on, following feature-to-feature edges. `dep:x`
/// entries are kept verbatim so an optional dependency's activation is visible
/// in the closure rather than inferred from it.
fn closure(features: &BTreeMap<String, Vec<String>>, roots: &[&str]) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut pending: Vec<String> = roots.iter().map(|root| (*root).to_owned()).collect();
    while let Some(entry) = pending.pop() {
        if !seen.insert(entry.clone()) {
            continue;
        }
        if let Some(children) = features.get(&entry) {
            pending.extend(children.iter().cloned());
        }
    }
    seen
}

#[test]
fn the_enhanced_dependency_is_optional_and_reachable_only_from_the_enhanced_feature() {
    let core = manifest("daemon/crates/kwiry-core/Cargo.toml");
    let dependency = core["dependencies"][ENHANCED_ONLY_DEPENDENCY]
        .as_table()
        .expect("the enhanced dependency is declared as a table");
    assert_eq!(
        dependency.get("optional").and_then(toml::Value::as_bool),
        Some(true),
        "{ENHANCED_ONLY_DEPENDENCY} must be optional or it lands in every graph"
    );

    let features = feature_table(&core);
    let activation = format!("dep:{ENHANCED_ONLY_DEPENDENCY}");
    let enabling: Vec<&String> = features
        .iter()
        .filter(|(_, entries)| entries.contains(&activation))
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        enabling,
        vec![ENHANCED_FEATURE],
        "only the enhanced tier may activate {ENHANCED_ONLY_DEPENDENCY}"
    );
}

#[test]
fn the_portable_feature_set_does_not_reach_the_enhanced_dependency() {
    let core = manifest("daemon/crates/kwiry-core/Cargo.toml");
    let features = feature_table(&core);
    let activation = format!("dep:{ENHANCED_ONLY_DEPENDENCY}");

    for root in ["portable", "internal-pdf-extractor", "internal-d5c-preview"] {
        let reachable = closure(&features, &[root]);
        assert!(
            !reachable.contains(&activation),
            "`{root}` must not reach {ENHANCED_ONLY_DEPENDENCY}"
        );
        assert!(
            !reachable.contains(ENHANCED_FEATURE),
            "`{root}` must not reach `{ENHANCED_FEATURE}`"
        );
    }

    // The enhanced tier is structurally native: it pulls in the whole native
    // dependency set, so it cannot be selected by a wasm32 build even by
    // accident.
    let enhanced = closure(&features, &[ENHANCED_FEATURE]);
    assert!(enhanced.contains("native"));
    assert!(enhanced.contains(&activation));
}

#[test]
fn the_wasm_adapter_selects_neither_the_native_nor_the_enhanced_feature() {
    let wasm = manifest("clients/obsidian/rust/kwiry-obsidian-wasm/Cargo.toml");
    let core_dependency = wasm["dependencies"]["kwiry-core"]
        .as_table()
        .expect("the adapter depends on kwiry-core as a table");

    assert_eq!(
        core_dependency
            .get("default-features")
            .and_then(toml::Value::as_bool),
        Some(false),
        "default features would pull in `native`"
    );

    let selected: BTreeSet<String> = core_dependency["features"]
        .as_array()
        .expect("the adapter names its kwiry-core features")
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .expect("a feature name is a string")
                .to_owned()
        })
        .collect();

    let core = manifest("daemon/crates/kwiry-core/Cargo.toml");
    let features = feature_table(&core);
    let roots: Vec<&str> = selected.iter().map(String::as_str).collect();
    let reachable = closure(&features, &roots);

    assert!(!reachable.contains("native"), "{reachable:?}");
    assert!(!reachable.contains(ENHANCED_FEATURE), "{reachable:?}");
    assert!(
        !reachable.contains(&format!("dep:{ENHANCED_ONLY_DEPENDENCY}")),
        "{reachable:?}"
    );
    // And the adapter must not name the dependency itself either.
    assert!(
        !wasm["dependencies"]
            .as_table()
            .expect("dependencies is a table")
            .contains_key(ENHANCED_ONLY_DEPENDENCY)
    );
}

#[test]
fn the_shared_cjk_decoder_is_already_in_the_portable_graph() {
    // `quick-xml`'s `encoding` feature pulls `encoding_rs` into every build
    // that has DOCX — which is every portable build. So naming `encoding_rs`
    // from `internal-pdf-extractor` adds no bytes to the plugin bundle, and
    // legacy CMap decoding is correctly a shared capability rather than a tier
    // divergence. If this stops being true, the reasoning in
    // `formats/pdf/cmap.rs` needs revisiting, not silently keeping.
    let workspace = manifest("daemon/Cargo.toml");
    let quick_xml = workspace["workspace"]["dependencies"]["quick-xml"]
        .as_table()
        .expect("quick-xml is declared as a table");
    let features: Vec<&str> = quick_xml["features"]
        .as_array()
        .expect("quick-xml names its features")
        .iter()
        .map(|entry| entry.as_str().expect("a feature name is a string"))
        .collect();
    assert!(features.contains(&"encoding"), "{features:?}");

    let core = manifest("daemon/crates/kwiry-core/Cargo.toml");
    let core_features = feature_table(&core);
    assert!(
        closure(&core_features, &["internal-pdf-extractor"]).contains("dep:encoding_rs"),
        "the PDF reader names the decoder it uses instead of relying on DOCX's"
    );
}
