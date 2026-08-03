// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fmt::Write as _;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kwiry_core::{
    ExtractionCoverage, Frontmatter, PropertyValue, SourceDescriptor, SourceFormat,
    SourcePreparation, SourcePreparationKind, extract_source, prepare_source_buffer,
};

const WELL_FORMED: &[u8] = include_bytes!("fixtures/canvas/well-formed.canvas");
const MALFORMED: &[u8] = include_bytes!("fixtures/canvas/malformed.canvas");
const EMPTY: &[u8] = include_bytes!("fixtures/canvas/empty.canvas");
const PARTIAL: &[u8] = include_bytes!("fixtures/canvas/partial.canvas");

fn prepare(path: &str, bytes: &[u8]) -> SourcePreparation {
    prepare_source_buffer(
        &SourceDescriptor {
            vault_id: "canvas-fixture".to_owned(),
            room: None,
            path: path.to_owned(),
            format: SourceFormat::Canvas,
            byte_length: bytes.len() as u64,
            mtime: 42,
            mtime_nanos: 42_000_000_000,
        },
        bytes,
    )
    .expect("fixture must produce a source preparation")
}

#[test]
fn well_formed_canvas_preserves_typed_json_and_node_then_edge_section_order() {
    let extracted = extract_source(SourceFormat::Canvas, WELL_FORMED).unwrap();

    assert_eq!(extracted.coverage, ExtractionCoverage::IndexedComplete);
    assert_eq!(extracted.sections.len(), 9);
    assert_eq!(
        extracted
            .sections
            .iter()
            .map(|section| section.heading_path.clone())
            .collect::<Vec<_>>(),
        [
            vec![],
            vec!["Alpha".to_owned()],
            vec!["Alpha".to_owned(), "Detail".to_owned()],
            vec![],
            vec![],
            vec![],
            vec!["Closing".to_owned()],
            vec![],
            vec![],
        ]
    );
    assert!(
        extracted.sections[0]
            .content
            .starts_with("---\ntitle: Card-only title")
    );
    assert!(
        extracted.sections[0]
            .content
            .contains("tags: [nested, card]")
    );
    assert_eq!(extracted.sections[3].content, "Research Cluster");
    assert_eq!(
        extracted.sections[4].content,
        "https://example.com/canvas-source"
    );
    assert_eq!(
        extracted.sections[5].content,
        "References/target.md\n#Only Authored Subpath"
    );
    assert!(extracted.sections[6].content.starts_with("## Closing"));
    assert_eq!(extracted.sections[7].content, "supports source");
    assert_eq!(extracted.sections[8].content, "resolves into");
    assert!(
        extracted
            .sections
            .iter()
            .all(|section| section.locator.is_none())
    );

    assert_eq!(extracted.frontmatter, Frontmatter::default());
    assert!(extracted.aliases.is_empty());
    assert!(extracted.links_out.is_empty());
    assert_eq!(extracted.properties.len(), 1);
    assert!(extracted.properties.get("title").is_none());
    assert!(extracted.properties.get("tags").is_none());
    let Some(PropertyValue::Map(canvas)) = extracted.properties.get("canvas") else {
        panic!("complete typed Canvas JSON must remain under the canvas property root");
    };
    let Some(PropertyValue::Sequence(nodes)) = canvas.get("nodes") else {
        panic!("Canvas nodes must remain a sequence");
    };
    let PropertyValue::Map(first_node) = &nodes[0] else {
        panic!("first Canvas node must remain an object");
    };
    assert_eq!(
        first_node.get("id"),
        Some(&PropertyValue::String("1111111111111111".to_owned()))
    );
    assert_eq!(first_node.get("x"), Some(&PropertyValue::I64(17)));
    let Some(PropertyValue::Sequence(edges)) = canvas.get("edges") else {
        panic!("Canvas edges must remain a sequence");
    };
    let PropertyValue::Map(last_edge) = &edges[2] else {
        panic!("last Canvas edge must remain an object");
    };
    assert_eq!(
        last_edge.get("id"),
        Some(&PropertyValue::String("cccccccccccccccc".to_owned()))
    );
    let Some(PropertyValue::Map(metadata)) = canvas.get("metadata") else {
        panic!("extension metadata must remain typed");
    };
    assert_eq!(metadata.get("enabled"), Some(&PropertyValue::Bool(true)));
    assert_eq!(metadata.get("threshold"), Some(&PropertyValue::F64(12.5)));
    assert_eq!(
        metadata.get("max_items"),
        Some(&PropertyValue::U64(u64::MAX))
    );
    assert_eq!(metadata.get("optional"), Some(&PropertyValue::Null));

    let ordinary_text = extracted
        .sections
        .iter()
        .map(|section| section.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for structural_value in [
        "1111111111111111",
        "aaaaaaaaaaaaaaaa",
        "geometrysentinelqzx",
        "18446744073709551615",
    ] {
        assert!(!ordinary_text.contains(structural_value));
    }
}

#[test]
fn canvas_coverage_distinguishes_partial_empty_unreadable_and_quarantined_inputs() {
    let partial = prepare("fixtures/partial.canvas", PARTIAL);
    assert_eq!(partial.kind, SourcePreparationKind::Indexed);
    assert_eq!(partial.coverage, ExtractionCoverage::IndexedPartial);
    assert_eq!(partial.chunks.len(), 3);
    assert_eq!(partial.chunks[0].heading_path, ["Usable card"]);
    assert_eq!(partial.chunks[1].content, "Usable group");
    assert_eq!(partial.chunks[2].content, "usable relation");
    assert!(
        partial
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("not an object")
                && warning.contains("unsupported type")
                && warning.contains("unavailable node"))
    );
    assert!(
        partial
            .chunks
            .iter()
            .all(|chunk| !chunk.content.contains("must not"))
    );
    let Some(PropertyValue::Map(canvas)) = partial.frontmatter.get("canvas") else {
        panic!("partial Canvas must retain the complete authored JSON root");
    };
    let Some(PropertyValue::Sequence(nodes)) = canvas.get("nodes") else {
        panic!("partial Canvas must retain every authored node entry");
    };
    assert_eq!(nodes.len(), 4);
    assert_eq!(nodes[2], PropertyValue::String("not-a-node".to_owned()));

    let empty = prepare("fixtures/empty.canvas", EMPTY);
    assert_eq!(empty.kind, SourcePreparationKind::Skipped);
    assert_eq!(empty.coverage, ExtractionCoverage::SkippedNoExtractableText);
    assert!(empty.chunks.is_empty());

    let malformed = prepare("fixtures/malformed.canvas", MALFORMED);
    assert_eq!(malformed.kind, SourcePreparationKind::Skipped);
    assert_eq!(malformed.coverage, ExtractionCoverage::Quarantined);
    assert!(malformed.chunks.is_empty());

    let wrong_root = prepare("fixtures/wrong-root.canvas", b"[]");
    assert_eq!(wrong_root.coverage, ExtractionCoverage::Quarantined);

    for (path, bytes) in [
        ("fixtures/omitted-arrays.canvas", br#"{}"#.as_slice()),
        (
            "fixtures/omitted-nodes.canvas",
            br#"{"edges":[]}"#.as_slice(),
        ),
        (
            "fixtures/omitted-edges.canvas",
            br#"{"nodes":[]}"#.as_slice(),
        ),
    ] {
        let optional_arrays = prepare(path, bytes);
        assert_eq!(
            optional_arrays.coverage,
            ExtractionCoverage::SkippedNoExtractableText
        );
        assert!(optional_arrays.warning.as_deref().is_some_and(|warning| {
            warning.contains("no authored text, labels, URLs, or file references")
        }));
    }

    let node_only = prepare(
        "fixtures/node-only.canvas",
        br#"{"nodes":[{"id":"1111111111111111","type":"text","x":0,"y":0,"width":1,"height":1,"text":"usable"}]}"#,
    );
    assert_eq!(node_only.kind, SourcePreparationKind::Indexed);
    assert_eq!(node_only.coverage, ExtractionCoverage::IndexedComplete);
    assert_eq!(node_only.chunks.len(), 1);
    assert_eq!(node_only.chunks[0].content, "usable");
    let Some(PropertyValue::Map(canvas)) = node_only.frontmatter.get("canvas") else {
        panic!("node-only Canvas must retain its complete authored JSON root");
    };
    assert!(canvas.get("edges").is_none());
    let Some(PropertyValue::Sequence(nodes)) = canvas.get("nodes") else {
        panic!("node-only Canvas must retain its authored node sequence");
    };
    assert_eq!(nodes.len(), 1);

    let wrong_nodes = prepare("fixtures/wrong-nodes.canvas", br#"{"nodes":{},"edges":[]}"#);
    assert_eq!(wrong_nodes.coverage, ExtractionCoverage::Quarantined);
    let wrong_edges = prepare("fixtures/wrong-edges.canvas", br#"{"nodes":[],"edges":{}}"#);
    assert_eq!(wrong_edges.coverage, ExtractionCoverage::Quarantined);
    let unreadable = prepare("fixtures/unreadable.canvas", &[0xff]);
    assert_eq!(unreadable.coverage, ExtractionCoverage::Unreadable);
}

#[test]
fn file_nodes_index_only_the_authored_path_and_subpath_without_dereferencing() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temporary = std::env::temp_dir().join(format!(
        "kwiry-canvas-no-dereference-{}-{unique}",
        std::process::id()
    ));
    let referenced_path = temporary.join("References/target.md");
    fs::create_dir_all(referenced_path.parent().unwrap()).unwrap();
    fs::write(
        &referenced_path,
        "REFERENCED_FILE_SENTINEL_MUST_NEVER_ENTER_THE_CANVAS",
    )
    .unwrap();
    let canvas_path = temporary.join("board.canvas");
    fs::write(&canvas_path, WELL_FORMED).unwrap();

    let bytes = fs::read(canvas_path).unwrap();
    let prepared = prepare("board.canvas", &bytes);
    let text = prepared
        .chunks
        .iter()
        .map(|chunk| chunk.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("References/target.md"));
    assert!(text.contains("#Only Authored Subpath"));
    assert!(!text.contains("REFERENCED_FILE_SENTINEL"));
    fs::remove_dir_all(temporary).unwrap();
}

#[test]
fn geometry_is_identity_neutral_while_authored_node_and_edge_order_is_sequence_sensitive() {
    let original = prepare("Boards/research.canvas", WELL_FORMED);
    let mut geometry: serde_json::Value = serde_json::from_slice(WELL_FORMED).unwrap();
    for (index, node) in geometry["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        node["x"] = serde_json::json!(10_000 + index);
        node["y"] = serde_json::json!(-20_000 - index as i64);
        node["width"] = serde_json::json!(900 + index);
        node["height"] = serde_json::json!(700 + index);
    }
    let geometry_bytes = serde_json::to_vec(&geometry).unwrap();
    let geometry_changed = prepare("Boards/research.canvas", &geometry_bytes);

    assert_ne!(original.content_hash, geometry_changed.content_hash);
    assert_eq!(
        chunk_evidence(&original),
        chunk_evidence(&geometry_changed),
        "display-only geometry must not affect rendered chunks or chunk IDs"
    );

    let mut nodes_reordered: serde_json::Value = serde_json::from_slice(WELL_FORMED).unwrap();
    nodes_reordered["nodes"].as_array_mut().unwrap().swap(1, 2);
    let nodes_reordered = prepare(
        "Boards/research.canvas",
        &serde_json::to_vec(&nodes_reordered).unwrap(),
    );
    assert_eq!(
        nodes_reordered.chunks[3].content,
        "https://example.com/canvas-source"
    );
    assert_eq!(nodes_reordered.chunks[4].content, "Research Cluster");
    assert_ne!(
        chunk_id_for_content(&original, "Research Cluster"),
        chunk_id_for_content(&nodes_reordered, "Research Cluster")
    );

    let mut edges_reordered: serde_json::Value = serde_json::from_slice(WELL_FORMED).unwrap();
    edges_reordered["edges"].as_array_mut().unwrap().swap(0, 2);
    let edges_reordered = prepare(
        "Boards/research.canvas",
        &serde_json::to_vec(&edges_reordered).unwrap(),
    );
    assert_eq!(edges_reordered.chunks[7].content, "resolves into");
    assert_eq!(edges_reordered.chunks[8].content, "supports source");
    assert_ne!(
        chunk_id_for_content(&original, "supports source"),
        chunk_id_for_content(&edges_reordered, "supports source")
    );
}

#[test]
fn aggregate_canvas_section_and_heading_limits_return_typed_errors() {
    let mut many_groups = String::from("{\"nodes\":[");
    for index in 0..=100_000_u64 {
        if index != 0 {
            many_groups.push(',');
        }
        write!(
            many_groups,
            "{{\"id\":\"{index:016x}\",\"type\":\"group\",\"x\":0,\"y\":0,\"width\":1,\"height\":1,\"label\":\"x\"}}"
        )
        .unwrap();
    }
    many_groups.push_str("],\"edges\":[]}");
    let section_error = extract_source(SourceFormat::Canvas, many_groups.as_bytes()).unwrap_err();
    assert_eq!(section_error.code, "index_limit_exceeded");
    assert_eq!(
        section_error.message,
        "prepared source exceeds the chunk inventory limit"
    );

    let large_heading = "h".repeat(1_100_000);
    let card = format!("# {large_heading}\n## a\n## b\n## c\n## d\n");
    let heading_source = serde_json::to_vec(&serde_json::json!({
        "nodes": [
            {"id":"1111111111111111","type":"text","x":0,"y":0,"width":1,"height":1,"text":card},
            {"id":"2222222222222222","type":"text","x":1,"y":1,"width":1,"height":1,"text":card}
        ],
        "edges": []
    }))
    .unwrap();
    let heading_error = extract_source(SourceFormat::Canvas, &heading_source).unwrap_err();
    assert_eq!(heading_error.code, "index_limit_exceeded");
    assert_eq!(
        heading_error.message,
        "prepared source exceeds the heading-path memory limit"
    );
}

#[test]
fn canvas_notices_are_bounded_without_losing_partial_coverage() {
    let mut nodes = vec![serde_json::json!({
        "id":"1111111111111111",
        "type":"text",
        "x":0,
        "y":0,
        "width":1,
        "height":1,
        "text":"usable"
    })];
    nodes.extend((0..40).map(|_| serde_json::Value::String("bad".to_owned())));
    let bytes = serde_json::to_vec(&serde_json::json!({"nodes": nodes, "edges": []})).unwrap();

    let extracted = extract_source(SourceFormat::Canvas, &bytes).unwrap();

    assert_eq!(extracted.coverage, ExtractionCoverage::IndexedPartial);
    assert_eq!(extracted.notices.len(), 32);
    assert_eq!(
        extracted.notices.last().map(|notice| notice.code.as_str()),
        Some("canvas_notices_truncated")
    );
    let warning = extracted
        .notices
        .iter()
        .map(|notice| notice.message.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(warning.len() < 4_096);
}

fn chunk_evidence(preparation: &SourcePreparation) -> Vec<(String, Vec<String>, String)> {
    preparation
        .chunks
        .iter()
        .map(|chunk| {
            (
                chunk.content.clone(),
                chunk.heading_path.clone(),
                chunk.chunk_id.clone(),
            )
        })
        .collect()
}

fn chunk_id_for_content<'a>(preparation: &'a SourcePreparation, content: &str) -> &'a str {
    preparation
        .chunks
        .iter()
        .find(|chunk| chunk.content == content)
        .map(|chunk| chunk.chunk_id.as_str())
        .expect("fixture content must identify one chunk")
}
