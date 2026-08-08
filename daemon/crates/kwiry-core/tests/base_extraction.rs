// SPDX-License-Identifier: MIT OR Apache-2.0

use kwiry_core::{
    ExtractionCoverage, PropertyValue, SourceDescriptor, SourceFormat, SourceLocator,
    SourcePreparation, SourcePreparationKind, extract_source, prepare_source_buffer,
};

const WELL_FORMED: &[u8] = include_bytes!("fixtures/base/well-formed.base");
const MALFORMED: &[u8] = include_bytes!("fixtures/base/malformed.base");
const EMPTY: &[u8] = include_bytes!("fixtures/base/empty.base");

fn prepare(path: &str, bytes: &[u8]) -> SourcePreparation {
    prepare_source_buffer(
        &SourceDescriptor {
            vault_id: "base-fixture".to_owned(),
            room: None,
            path: path.to_owned(),
            format: SourceFormat::Base,
            byte_length: bytes.len() as u64,
            mtime: 42,
            mtime_nanos: 42_000_000_000,
        },
        bytes,
    )
    .expect("fixture must produce a source preparation")
}

#[test]
fn well_formed_base_fixture_preserves_typed_configuration_and_view_order() {
    let extracted = extract_source(SourceFormat::Base, WELL_FORMED).unwrap();

    assert_eq!(extracted.coverage, ExtractionCoverage::IndexedComplete);
    assert_eq!(extracted.sections.len(), 4);
    assert!(extracted.sections[0].heading_path.is_empty());
    assert_eq!(extracted.sections[1].heading_path, ["Active"]);
    assert_eq!(extracted.sections[2].heading_path, ["Gallery"]);
    assert_eq!(extracted.sections[3].heading_path, ["Active (2)"]);
    assert!(!extracted.sections[0].content.contains("views"));

    assert_eq!(
        extracted.properties.get("title"),
        Some(&PropertyValue::String("Project dashboard".to_owned()))
    );
    assert!(matches!(
        extracted.properties.get("tags"),
        Some(PropertyValue::Sequence(tags)) if tags.len() == 2
    ));
    assert!(extracted.properties.get("enabled").is_none());

    let Some(PropertyValue::Map(base)) = extracted.properties.get("base") else {
        panic!("the complete typed YAML tree must remain under the base property root");
    };
    assert_eq!(base.get("enabled"), Some(&PropertyValue::Bool(true)));
    assert_eq!(base.get("threshold"), Some(&PropertyValue::F64(12.5)));
    assert_eq!(base.get("max_items"), Some(&PropertyValue::U64(u64::MAX)));
    assert_eq!(base.get("optional"), Some(&PropertyValue::Null));
    assert!(matches!(
        base.get("views"),
        Some(PropertyValue::Sequence(views)) if views.len() == 3
    ));
}

#[test]
fn shared_title_and_tags_are_projected_only_from_authored_top_level_fields() {
    let source = br#"views:
  - name: View metadata
    title: View-only title
    tags: [view-only]
"#;
    let extracted = extract_source(SourceFormat::Base, source).unwrap();

    assert_eq!(extracted.sections.len(), 2);
    assert!(extracted.sections[0].heading_path.is_empty());
    assert!(extracted.properties.get("title").is_none());
    assert!(extracted.properties.get("tags").is_none());
    assert!(extracted.frontmatter.title.is_none());
    assert!(extracted.frontmatter.tags.is_empty());
    assert!(matches!(
        extracted.properties.get("base"),
        Some(PropertyValue::Map(base)) if base.contains_key("views")
    ));
}

#[test]
fn malformed_base_fixture_is_quarantined_while_empty_valid_base_is_skipped() {
    let malformed = prepare("fixtures/malformed.base", MALFORMED);
    assert_eq!(malformed.kind, SourcePreparationKind::Skipped);
    assert_eq!(malformed.coverage, ExtractionCoverage::Quarantined);
    assert!(malformed.chunks.is_empty());
    assert!(malformed.content_hash.is_some());
    assert_eq!(
        malformed.warning.as_deref(),
        Some("invalid Base YAML; quarantined as an invalid source preparation")
    );

    let empty = prepare("fixtures/empty.base", EMPTY);
    assert_eq!(empty.kind, SourcePreparationKind::Skipped);
    assert_eq!(empty.coverage, ExtractionCoverage::SkippedNoExtractableText);
    assert!(empty.chunks.is_empty());
    assert!(
        empty
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("no authored configuration"))
    );
}

#[test]
fn base_chunk_ids_are_deterministic_and_rename_sensitive() {
    let reordered_views = br#"title: Project dashboard
tags: [projects, active]
enabled: true
threshold: 12.5
max_items: 18446744073709551615
filters:
  and:
    - file.inFolder("Projects")
    - status == "active"
optional: null
views:
  - type: cards
    name: Gallery
    cover: note.cover
  - type: list
    name: Active
    groupBy: status
  - type: table
    name: Active
    order: [file.name, status]
"#;
    let first = prepare("dashboards/project.base", WELL_FORMED);
    let second = prepare("dashboards/project.base", WELL_FORMED);
    let reordered = prepare("dashboards/project.base", reordered_views);
    let renamed = prepare("dashboards/renamed-project.base", WELL_FORMED);

    let first_headings = first
        .chunks
        .iter()
        .map(|chunk| chunk.heading_path.join(" / "))
        .collect::<Vec<_>>();
    let reordered_headings = reordered
        .chunks
        .iter()
        .map(|chunk| chunk.heading_path.join(" / "))
        .collect::<Vec<_>>();
    let first_ids = first
        .chunks
        .iter()
        .map(|chunk| chunk.chunk_id.as_str())
        .collect::<Vec<_>>();
    let second_ids = second
        .chunks
        .iter()
        .map(|chunk| chunk.chunk_id.as_str())
        .collect::<Vec<_>>();
    let reordered_ids = reordered
        .chunks
        .iter()
        .map(|chunk| chunk.chunk_id.as_str())
        .collect::<Vec<_>>();
    let renamed_ids = renamed
        .chunks
        .iter()
        .map(|chunk| chunk.chunk_id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(first_ids, second_ids);
    assert_eq!(first_ids.len(), reordered_ids.len());
    assert_ne!(first_ids, reordered_ids);
    assert_ne!(first_ids[1], reordered_ids[1]);
    assert_ne!(first_ids[2], reordered_ids[2]);
    assert_eq!(first_headings, ["", "Active", "Gallery", "Active (2)"]);
    assert_eq!(reordered_headings, ["", "Gallery", "Active", "Active (2)"]);
    assert_eq!(first_ids.len(), renamed_ids.len());
    assert!(
        first_ids
            .iter()
            .zip(renamed_ids)
            .all(|(before, after)| *before != after)
    );
}

#[test]
fn base_view_locator_carries_the_authored_view_name() {
    let prepared = prepare("dashboards/project.base", WELL_FORMED);

    assert_eq!(prepared.kind, SourcePreparationKind::Indexed);
    assert_eq!(prepared.chunks[3].heading_path, ["Active (2)"]);
    assert_eq!(
        prepared.chunks[3].source_locator,
        Some(SourceLocator::BaseView {
            view: "Active".to_owned()
        })
    );
}
