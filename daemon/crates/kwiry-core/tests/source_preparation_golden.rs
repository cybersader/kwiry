// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use kwiry_core::{
    Frontmatter, PropertyValue, SourceDescriptor, SourceFormat, SourcePreparation,
    SourcePreparationKind, prepare_source_buffer,
};

const LINK_COUNT: usize = 5_000;
const TAG_COUNT: usize = 5_000;
const ALIAS_COUNT: usize = 512;
const OPEN_PROPERTY_COUNT: usize = 1_000;
const PROPERTY_ARRAY_COUNT: usize = 1_200;
const PROPERTY_MAP_DEPTH: usize = 32;
const BASE_SOURCE: &str = include_str!("fixtures/base/well-formed.base");
const CANVAS_SOURCE: &str = include_str!("fixtures/canvas/well-formed.canvas");
const FIXTURE_COUNT: usize = 16;

struct Fixture {
    file_name: &'static str,
    preparation: SourcePreparation,
}

#[test]
fn writes_real_source_preparation_goldens() {
    let fixtures = fixtures();
    assert_eq!(fixtures.len(), FIXTURE_COUNT);

    let output_directory = output_directory();
    fs::create_dir_all(&output_directory).expect("create golden fixture directory");

    // A removed producer case must not leave an old JSON file that the consumer
    // suite can continue accepting after Rust stops generating it.
    remove_stale_json(&output_directory, &fixtures);

    for fixture in &fixtures {
        assert_adversarial_shape(fixture);
        let mut json = serde_json::to_string_pretty(&fixture.preparation)
            .expect("serialize source preparation fixture");
        json.push('\n');
        fs::write(output_directory.join(fixture.file_name), json)
            .expect("write source preparation fixture");
    }

    println!(
        "generated {} source preparation fixtures in {}",
        fixtures.len(),
        output_directory.display()
    );
}

fn fixtures() -> Vec<Fixture> {
    vec![
        prepare(
            "01-thousands-of-wikilinks.json",
            "Golden/Thousands of Wikilinks.md",
            wikilink_source(),
            Some("fixture-room"),
        ),
        prepare(
            "02-deep-heading-nesting.json",
            "Golden/Deep Heading Nesting.md",
            deep_heading_source(),
            None,
        ),
        prepare(
            "03-very-large-single-section.json",
            "Golden/Very Large Single Section.md",
            large_section_source(),
            Some("fixture-room"),
        ),
        prepare(
            "04-large-frontmatter-tags.json",
            "Golden/Large Frontmatter Tags.md",
            large_frontmatter_source(),
            None,
        ),
        prepare(
            "05-empty-note.json",
            "Golden/Empty Note.md",
            String::new(),
            Some("fixture-room"),
        ),
        prepare(
            "06-frontmatter-only.json",
            "Golden/Frontmatter Only.md",
            frontmatter_only_source(),
            None,
        ),
        prepare(
            "07-unusual-valid-utf8.json",
            "Golden/Unusual Valid UTF-8.md",
            unusual_utf8_source(),
            Some("fixture-room"),
        ),
        prepare(
            "08-thousand-open-properties.json",
            "Golden/Thousand Open Properties.md",
            thousand_open_properties_source(),
            None,
        ),
        prepare(
            "09-deep-property-map.json",
            "Golden/Deep Property Map.md",
            deep_property_map_source(),
            Some("fixture-room"),
        ),
        prepare(
            "10-large-property-array.json",
            "Golden/Large Property Array.md",
            large_property_array_source(),
            None,
        ),
        prepare(
            "11-shared-key-integer.json",
            "Golden/Shared Key Integer.md",
            typed_shared_key_source("7"),
            Some("fixture-room"),
        ),
        prepare(
            "12-shared-key-string.json",
            "Golden/Shared Key String.md",
            typed_shared_key_source("'7'"),
            Some("fixture-room"),
        ),
        prepare(
            "13-shared-key-boolean.json",
            "Golden/Shared Key Boolean.md",
            typed_shared_key_source("true"),
            Some("fixture-room"),
        ),
        prepare(
            "14-property-key-and-scalar-edges.json",
            "Golden/Property Key and Scalar Edges.md",
            property_key_and_scalar_edges_source(),
            None,
        ),
        prepare_format(
            "15-base-project-dashboard.json",
            "Golden/Project Dashboard.base",
            BASE_SOURCE.to_owned(),
            Some("fixture-room"),
            SourceFormat::Base,
        ),
        prepare_format(
            "16-canvas-research-board.json",
            "Golden/Research Board.canvas",
            CANVAS_SOURCE.to_owned(),
            Some("fixture-room"),
            SourceFormat::Canvas,
        ),
    ]
}

fn prepare(file_name: &'static str, path: &str, source: String, room: Option<&str>) -> Fixture {
    prepare_format(file_name, path, source, room, SourceFormat::Markdown)
}

fn prepare_format(
    file_name: &'static str,
    path: &str,
    source: String,
    room: Option<&str>,
    format: SourceFormat,
) -> Fixture {
    let bytes = source.as_bytes();
    let descriptor = SourceDescriptor {
        vault_id: "golden-vault".to_owned(),
        room: room.map(str::to_owned),
        path: path.to_owned(),
        format,
        byte_length: bytes.len() as u64,
        mtime: 1_785_253_671_659,
        mtime_nanos: 1_785_253_671_659_123_456,
    };
    let preparation = prepare_source_buffer(&descriptor, bytes).expect("prepare golden source");
    Fixture {
        file_name,
        preparation,
    }
}

fn wikilink_source() -> String {
    let mut source = String::from("# Link map\n\n");
    for index in 0..LINK_COUNT {
        write!(source, "[[{index:x}]] ").expect("write wikilink source");
    }
    source.push('\n');
    source
}

fn deep_heading_source() -> String {
    [
        "# Level One\nFirst body.\n",
        "## Level Two\nSecond body.\n",
        "### Level Three\nThird body.\n",
        "#### Level Four\nFourth body.\n",
        "##### Level Five\nFifth body.\n",
        "###### Level Six\nSixth body.\n",
    ]
    .concat()
}

fn large_section_source() -> String {
    let mut source = String::from("# Oversized section\n\n");
    for index in 0..2_000 {
        write!(source, "segment-{index:04} ").expect("write large section source");
    }
    source.push('\n');
    source
}

fn large_frontmatter_source() -> String {
    let tags = (0..TAG_COUNT)
        .map(|index| format!("t{index:x}"))
        .collect::<Vec<_>>()
        .join(", ");
    let aliases = (0..ALIAS_COUNT)
        .map(|index| format!("a{index:x}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "---\ntitle: Large properties\ntags: [{tags}]\naliases: [{aliases}]\nstatus: active\ndate: 2026-07-28\n---\n# Body\nFrontmatter remains attached to this chunk.\n"
    )
}

fn frontmatter_only_source() -> String {
    "---\ntitle: Metadata only\ntags: [alpha, beta]\naliases: [First alias, Second alias]\nstatus: draft\ndate: 2026-07-28\n---\n"
        .to_owned()
}

fn unusual_utf8_source() -> String {
    "# Καλημέρα 世界\n\nnaïve café e\u{301}; العربية; עברית; देवनागरी; 𐍈; 🧭; 👩‍💻; fullwidth ＡＢＣ; non-breaking space.\n"
        .to_owned()
}

fn thousand_open_properties_source() -> String {
    let mut source = String::from("---\n");
    for index in 0..OPEN_PROPERTY_COUNT {
        writeln!(source, "property_{index}: value_{index}").expect("write open property source");
    }
    source.push_str("---\n# Body\nEvery property came through the real parser.\n");
    source
}

fn deep_property_map_source() -> String {
    // This remains intentionally deep while staying below the explicit 64-level
    // corruption/call-stack boundary.
    let mut source = String::from("---\nnested:\n");
    for depth in 0..PROPERTY_MAP_DEPTH {
        writeln!(source, "{}level_{depth}:", "  ".repeat(depth + 1))
            .expect("write nested property source");
    }
    write!(
        source,
        "{}leaf: value\n---\n# Body\nNested property map.\n",
        "  ".repeat(PROPERTY_MAP_DEPTH + 1)
    )
    .expect("write nested property leaf");
    source
}

fn large_property_array_source() -> String {
    let values = (0..PROPERTY_ARRAY_COUNT)
        .map(|index| index.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!("---\nitems: [{values}]\n---\n# Body\nLarge property array.\n")
}

fn typed_shared_key_source(value: &str) -> String {
    format!("---\nshared_signal: {value}\n---\n# Body\nCross-note property typing.\n")
}

fn property_key_and_scalar_edges_source() -> String {
    r#"---
"émoji-🧭": "unicode key"
"ключ": true
"内部字段": 7
chunk_id: property-owned-value
yaml_date: 2026-07-28
yaml_timestamp: 2026-07-28T12:34:56Z
quoted_numeral: "007"
unsafe_i64: -9007199254740993
max_u64: 18446744073709551615
integral_float: 125.0
empty_value:
explicit_null: null
empty_string: ""
empty_array: []
empty_map: {}
---
# Body
Property key and scalar edges.
"#
    .to_owned()
}

fn assert_adversarial_shape(fixture: &Fixture) {
    assert_eq!(fixture.preparation.kind, SourcePreparationKind::Indexed);
    assert!(fixture.preparation.warning.is_none());

    match fixture.file_name {
        "01-thousands-of-wikilinks.json" => {
            assert!(!fixture.preparation.chunks.is_empty());
            assert!(
                fixture
                    .preparation
                    .chunks
                    .iter()
                    .all(|chunk| chunk.links_out.len() == LINK_COUNT)
            );
        }
        "02-deep-heading-nesting.json" => {
            assert!(
                fixture
                    .preparation
                    .chunks
                    .iter()
                    .any(|chunk| chunk.heading_path.len() == 6)
            );
        }
        "03-very-large-single-section.json" => {
            assert!(fixture.preparation.chunks.len() >= 4);
        }
        "04-large-frontmatter-tags.json" => {
            assert_eq!(fixture.preparation.retrieval.aliases.len(), ALIAS_COUNT);
            assert_eq!(fixture.preparation.chunks.len(), 1);
            let Some(PropertyValue::Sequence(tags)) = fixture.preparation.frontmatter.get("tags")
            else {
                panic!("large tags fixture must retain its source-owned tags");
            };
            assert_eq!(tags.len(), TAG_COUNT);
            assert_eq!(
                fixture.preparation.chunks[0].frontmatter,
                Frontmatter::default()
            );
        }
        "05-empty-note.json" => assert!(fixture.preparation.chunks.is_empty()),
        "06-frontmatter-only.json" => {
            assert_eq!(fixture.preparation.chunks.len(), 1);
            assert!(fixture.preparation.chunks[0].content.is_empty());
            assert_eq!(fixture.preparation.retrieval.aliases.len(), 2);
            assert_eq!(
                fixture.preparation.chunks[0].frontmatter,
                Frontmatter::default()
            );
            assert_eq!(
                fixture.preparation.frontmatter.get("title"),
                Some(&PropertyValue::String("Metadata only".to_owned()))
            );
            assert_eq!(
                fixture.preparation.frontmatter.get("tags"),
                Some(&PropertyValue::Sequence(vec![
                    PropertyValue::String("alpha".to_owned()),
                    PropertyValue::String("beta".to_owned()),
                ]))
            );
        }
        "07-unusual-valid-utf8.json" => {
            assert_eq!(fixture.preparation.chunks.len(), 1);
            assert!(fixture.preparation.chunks[0].content.contains("𐍈"));
            assert_eq!(
                fixture.preparation.chunks[0].heading_path,
                ["Καλημέρα 世界"]
            );
        }
        "08-thousand-open-properties.json" => {
            let frontmatter = &fixture.preparation.frontmatter;
            assert_eq!(frontmatter.len(), OPEN_PROPERTY_COUNT);
            assert_eq!(
                frontmatter.get("property_999"),
                Some(&PropertyValue::String("value_999".to_owned()))
            );
        }
        "09-deep-property-map.json" => {
            let mut value = fixture
                .preparation
                .frontmatter
                .get("nested")
                .expect("nested property");
            for depth in 0..PROPERTY_MAP_DEPTH {
                let PropertyValue::Map(map) = value else {
                    panic!("level {depth} must remain a map");
                };
                value = map
                    .get(&format!("level_{depth}"))
                    .expect("nested property level");
            }
            let PropertyValue::Map(leaf) = value else {
                panic!("deepest property value must remain a map");
            };
            assert_eq!(
                leaf.get("leaf"),
                Some(&PropertyValue::String("value".to_owned()))
            );
        }
        "10-large-property-array.json" => {
            let Some(PropertyValue::Sequence(items)) = fixture.preparation.frontmatter.get("items")
            else {
                panic!("items must remain a sequence");
            };
            assert_eq!(items.len(), PROPERTY_ARRAY_COUNT);
            assert_eq!(items.first(), Some(&PropertyValue::I64(0)));
            assert_eq!(items.last(), Some(&PropertyValue::I64(1_199)));
        }
        "11-shared-key-integer.json" => assert_eq!(
            fixture.preparation.frontmatter.get("shared_signal"),
            Some(&PropertyValue::I64(7))
        ),
        "12-shared-key-string.json" => assert_eq!(
            fixture.preparation.frontmatter.get("shared_signal"),
            Some(&PropertyValue::String("7".to_owned()))
        ),
        "13-shared-key-boolean.json" => assert_eq!(
            fixture.preparation.frontmatter.get("shared_signal"),
            Some(&PropertyValue::Bool(true))
        ),
        "14-property-key-and-scalar-edges.json" => {
            let frontmatter = &fixture.preparation.frontmatter;
            assert_eq!(
                frontmatter.get("émoji-🧭"),
                Some(&PropertyValue::String("unicode key".to_owned()))
            );
            assert_eq!(frontmatter.get("ключ"), Some(&PropertyValue::Bool(true)));
            assert_eq!(frontmatter.get("内部字段"), Some(&PropertyValue::I64(7)));
            assert_eq!(
                frontmatter.get("chunk_id"),
                Some(&PropertyValue::String("property-owned-value".to_owned()))
            );
            assert_eq!(
                frontmatter.get("yaml_date"),
                Some(&PropertyValue::String("2026-07-28".to_owned()))
            );
            assert_eq!(
                frontmatter.get("yaml_timestamp"),
                Some(&PropertyValue::String("2026-07-28T12:34:56Z".to_owned()))
            );
            assert_eq!(
                frontmatter.get("quoted_numeral"),
                Some(&PropertyValue::String("007".to_owned()))
            );
            assert_eq!(
                frontmatter.get("unsafe_i64"),
                Some(&PropertyValue::I64(-9_007_199_254_740_993))
            );
            assert_eq!(
                frontmatter.get("max_u64"),
                Some(&PropertyValue::U64(u64::MAX))
            );
            assert_eq!(
                frontmatter.get("integral_float"),
                Some(&PropertyValue::F64(125.0))
            );
            let encoded =
                serde_json::to_value(&fixture.preparation).expect("serialize edge fixture");
            assert_eq!(encoded["frontmatter"]["unsafe_i64"]["type"], "i64");
            assert_eq!(encoded["frontmatter"]["max_u64"]["type"], "u64");
            assert_eq!(encoded["frontmatter"]["integral_float"]["type"], "f64");
            let compact = &encoded["chunks"][0]["chunk"]["frontmatter"];
            assert!(compact.get("unsafe_i64").is_none());
            assert!(compact.get("max_u64").is_none());
            assert!(compact.get("integral_float").is_none());
            assert_eq!(frontmatter.get("empty_value"), Some(&PropertyValue::Null));
            assert_eq!(frontmatter.get("explicit_null"), Some(&PropertyValue::Null));
            assert_eq!(
                frontmatter.get("empty_string"),
                Some(&PropertyValue::String(String::new()))
            );
            assert_eq!(
                frontmatter.get("empty_array"),
                Some(&PropertyValue::Sequence(Vec::new()))
            );
            assert_eq!(
                frontmatter.get("empty_map"),
                Some(&PropertyValue::Map(Default::default()))
            );
        }
        "15-base-project-dashboard.json" => {
            assert_eq!(fixture.preparation.format, SourceFormat::Base);
            assert_eq!(fixture.preparation.chunks.len(), 4);
            assert_eq!(fixture.preparation.chunks[1].heading_path, ["Active"]);
            assert_eq!(fixture.preparation.chunks[2].heading_path, ["Gallery"]);
            assert_eq!(fixture.preparation.chunks[3].heading_path, ["Active (2)"]);
            assert_eq!(
                fixture.preparation.chunks[3].source_locator,
                Some(kwiry_core::SourceLocator::BaseView {
                    view: "Active".to_owned()
                })
            );
        }
        "16-canvas-research-board.json" => {
            assert_eq!(fixture.preparation.schema_version, 7);
            assert_eq!(fixture.preparation.format, SourceFormat::Canvas);
            assert_eq!(
                fixture.preparation.coverage,
                kwiry_core::ExtractionCoverage::IndexedComplete
            );
            assert_eq!(fixture.preparation.chunks.len(), 9);
            assert_eq!(fixture.preparation.chunks[1].heading_path, ["Alpha"]);
            assert_eq!(
                fixture.preparation.chunks[2].heading_path,
                ["Alpha", "Detail"]
            );
            assert_eq!(fixture.preparation.chunks[6].heading_path, ["Closing"]);
            assert!(
                fixture
                    .preparation
                    .chunks
                    .iter()
                    .all(|chunk| chunk.source_locator.is_none())
            );
            assert!(fixture.preparation.frontmatter.get("title").is_none());
            let Some(PropertyValue::Map(canvas)) = fixture.preparation.frontmatter.get("canvas")
            else {
                panic!("Canvas golden must retain its complete typed JSON root");
            };
            let Some(PropertyValue::Sequence(nodes)) = canvas.get("nodes") else {
                panic!("Canvas golden must retain its node sequence");
            };
            let PropertyValue::Map(first_node) = &nodes[0] else {
                panic!("Canvas golden first node must remain an object");
            };
            assert_eq!(
                first_node.get("id"),
                Some(&PropertyValue::String("1111111111111111".to_owned()))
            );
            let Some(PropertyValue::Sequence(edges)) = canvas.get("edges") else {
                panic!("Canvas golden must retain its edge sequence");
            };
            let PropertyValue::Map(first_edge) = &edges[0] else {
                panic!("Canvas golden first edge must remain an object");
            };
            assert_eq!(
                first_edge.get("id"),
                Some(&PropertyValue::String("aaaaaaaaaaaaaaaa".to_owned()))
            );
        }
        _ => unreachable!("every fixture has an adversarial shape assertion"),
    }
}

fn remove_stale_json(output_directory: &Path, fixtures: &[Fixture]) {
    let expected = fixtures
        .iter()
        .map(|fixture| fixture.file_name)
        .collect::<BTreeSet<_>>();
    for entry in fs::read_dir(output_directory).expect("read golden fixture directory") {
        let path = entry.expect("read golden fixture entry").path();
        let is_json = path
            .extension()
            .is_some_and(|extension| extension == "json");
        let is_expected = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| expected.contains(name));
        if is_json && !is_expected {
            fs::remove_file(path).expect("remove stale source preparation fixture");
        }
    }
}

fn output_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("kwiry-core lives below the repository root")
        .join("clients/obsidian/test/fixtures/source-preparations")
}
