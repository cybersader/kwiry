// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use kwiry_core::{
    SourceDescriptor, SourceFormat, SourcePreparation, SourcePreparationKind, prepare_source_buffer,
};

const LINK_COUNT: usize = 5_000;
const TAG_COUNT: usize = 5_000;
const ALIAS_COUNT: usize = 512;
const FIXTURE_COUNT: usize = 7;

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
    ]
}

fn prepare(file_name: &'static str, path: &str, source: String, room: Option<&str>) -> Fixture {
    let bytes = source.as_bytes();
    let descriptor = SourceDescriptor {
        vault_id: "golden-vault".to_owned(),
        room: room.map(str::to_owned),
        path: path.to_owned(),
        format: SourceFormat::Markdown,
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
            assert_eq!(
                fixture.preparation.chunks[0].frontmatter.tags.len(),
                TAG_COUNT
            );
        }
        "05-empty-note.json" => assert!(fixture.preparation.chunks.is_empty()),
        "06-frontmatter-only.json" => {
            assert!(fixture.preparation.chunks.is_empty());
            assert_eq!(fixture.preparation.retrieval.aliases.len(), 2);
        }
        "07-unusual-valid-utf8.json" => {
            assert_eq!(fixture.preparation.chunks.len(), 1);
            assert!(fixture.preparation.chunks[0].content.contains("𐍈"));
            assert_eq!(
                fixture.preparation.chunks[0].heading_path,
                ["Καλημέρα 世界"]
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
