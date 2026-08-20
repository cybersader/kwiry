// SPDX-License-Identifier: MIT OR Apache-2.0

//! Excalidraw extraction spike coverage.
//!
//! The format is deliberately **inadmissible**: there is no `SourceFormat`
//! variant, no registry entry, and no discovery or source-preparation route, so
//! these tests drive [`extract_excalidraw_candidate`] directly rather than
//! `prepare_source_buffer`. `excalidraw_stays_inadmissible_until_an_owner_amendment`
//! pins that boundary.

#![cfg(feature = "internal-excalidraw-extractor")]

use std::fmt::Write as _;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use kwiry_core::{
    ExtractedSource, ExtractionCoverage, Frontmatter, MAX_EXCALIDRAW_NOTICES,
    MAX_EXCALIDRAW_PROPERTY_BYTES, MAX_EXCALIDRAW_PROPERTY_ENTRIES, PropertyValue,
    SOURCE_PREPARATION_SCHEMA_VERSION, SourceDescriptor, SourceFormat,
    extract_excalidraw_candidate, format_specs, prepare_source_buffer,
};

const WELL_FORMED: &[u8] = include_bytes!("fixtures/excalidraw/well-formed.excalidraw");
const PARTIAL: &[u8] = include_bytes!("fixtures/excalidraw/partial.excalidraw");
const EMPTY: &[u8] = include_bytes!("fixtures/excalidraw/empty.excalidraw");
const MALFORMED: &[u8] = include_bytes!("fixtures/excalidraw/malformed.excalidraw");
const DELETED: &[u8] = include_bytes!("fixtures/excalidraw/deleted.excalidraw");

/// The chunk-inventory cap enforced by `ExtractionBudget::reserve_section`.
const MAX_SECTIONS: usize = 100_000;

#[test]
fn well_formed_excalidraw_emits_array_ordered_plain_sections_and_a_bounded_projection() {
    let extracted = extract_excalidraw_candidate(WELL_FORMED).unwrap();

    assert_eq!(extracted.coverage, ExtractionCoverage::IndexedComplete);
    assert_eq!(
        section_contents(&extracted),
        [
            // frame name at the frame's own array position
            "Research Frame",
            // rectangle link; its bound label is NOT folded in here
            "https://example.com/excalidraw-source",
            // the bound label at the bound text element's own array position,
            // taking `originalText` over the wrap-broken `text`
            "Bound label",
            // plain text, never Markdown-parsed
            "# Not a heading",
            // same element: text section first, then its link section
            "https://example.com/text-link",
            // arrow label at the label element's position, not the arrow's
            "Arrow label",
            // embeddable carries its URL in the inherited `link`
            "https://example.com/embedded-frame",
        ]
    );
    assert!(
        extracted
            .sections
            .iter()
            .all(|section| section.heading_path.is_empty()),
        "Excalidraw text is plain, so no section may carry a heading path"
    );
    assert!(
        extracted
            .sections
            .iter()
            .all(|section| section.locator.is_none())
    );

    assert_eq!(extracted.frontmatter, Frontmatter::default());
    assert!(extracted.aliases.is_empty());
    assert!(
        extracted.links_out.is_empty(),
        "element links are sections, not wikilink-graph edges"
    );

    // The image-payload elision is declared, not silent, and is a policy notice
    // rather than a defect: coverage stays indexed-complete.
    assert_eq!(
        extracted
            .notices
            .iter()
            .map(|notice| notice.code.as_str())
            .collect::<Vec<_>>(),
        ["excalidraw_image_payloads_not_retained"]
    );

    assert_eq!(extracted.properties.len(), 1);
    let Some(PropertyValue::Map(projection)) = extracted.properties.get("excalidraw") else {
        panic!("the bounded typed projection must live under the excalidraw property root");
    };
    assert_eq!(
        projection.get("type"),
        Some(&PropertyValue::String("excalidraw".to_owned()))
    );
    assert_eq!(projection.get("version"), Some(&PropertyValue::I64(2)));

    let Some(PropertyValue::Map(app_state)) = projection.get("appState") else {
        panic!("appState must be retained verbatim and typed");
    };
    assert_eq!(app_state.get("gridSize"), Some(&PropertyValue::Null));
    assert_eq!(
        app_state.get("gridModeEnabled"),
        Some(&PropertyValue::Bool(true))
    );
    assert_eq!(app_state.get("zoomValue"), Some(&PropertyValue::F64(12.5)));
    assert_eq!(
        app_state.get("maxSentinel"),
        Some(&PropertyValue::U64(u64::MAX)),
        "the projection must stay precision-safe, never stringified"
    );

    let Some(PropertyValue::Sequence(elements)) = projection.get("elements") else {
        panic!("every structurally identified element must be projected in array order");
    };
    assert_eq!(elements.len(), 8);
    let PropertyValue::Map(rectangle) = &elements[1] else {
        panic!("element projections are maps");
    };
    assert_eq!(
        rectangle.get("id"),
        Some(&PropertyValue::String("rect00000000001".to_owned()))
    );
    assert_eq!(
        rectangle.get("type"),
        Some(&PropertyValue::String("rectangle".to_owned()))
    );
    assert!(rectangle.get("boundElements").is_some());
    assert!(rectangle.get("customData").is_some());
    for dropped in ["seed", "x", "y", "width", "height", "angle", "index"] {
        assert!(
            rectangle.get(dropped).is_none(),
            "structural field {dropped} must not be retained"
        );
    }
    let PropertyValue::Map(arrow) = &elements[4] else {
        panic!("element projections are maps");
    };
    assert!(
        arrow.get("points").is_none(),
        "unbounded machine-generated point arrays must never be retained"
    );
    let PropertyValue::Map(image) = &elements[6] else {
        panic!("element projections are maps");
    };
    assert!(image.get("fileId").is_none());
    assert!(image.get("scale").is_none());
    assert!(image.get("crop").is_none());

    let Some(PropertyValue::Map(files)) = projection.get("files") else {
        panic!("files must be retained as a digest map");
    };
    let Some(PropertyValue::Map(digest)) = files.get("file00000000001") else {
        panic!("each file id key is preserved");
    };
    assert_eq!(
        digest.get("mimeType"),
        Some(&PropertyValue::String("image/png".to_owned()))
    );
    assert!(digest.get("created").is_some());
    assert!(
        digest.get("dataURL").is_none(),
        "base64 image payloads must never cross the source ABI"
    );

    let ordinary_text = section_text(&extracted);
    for structural_value in [
        "IMAGEPAYLOADSENTINELQZX",
        "a0structsentinel",
        "groupidsentinelqzx",
        "1234567890",
        "18446744073709551615",
        "file00000000001",
        "rect00000000001",
        "must not become ordinary text",
    ] {
        assert!(
            !ordinary_text.contains(structural_value),
            "structural value {structural_value} must never become ordinary text"
        );
    }
    assert!(!projection_debug(&extracted).contains("IMAGEPAYLOADSENTINELQZX"));
}

#[test]
fn excalidraw_coverage_distinguishes_partial_empty_unreadable_and_quarantined_inputs() {
    let partial = extract_excalidraw_candidate(PARTIAL).unwrap();
    assert_eq!(partial.coverage, ExtractionCoverage::IndexedPartial);
    assert_eq!(
        section_contents(&partial),
        [
            "Usable text",
            "Usable frame",
            // A dangling containerId is not an extraction defect: the element
            // still renders and still carries authored words.
            "Dangling container label",
        ]
    );
    assert_eq!(
        notice_codes(&partial),
        [
            "excalidraw_element_not_object",
            "excalidraw_element_unsupported_type",
            "excalidraw_element_missing_id",
            "excalidraw_duplicate_id",
        ],
        "cross-references are never resolved, so no dangling-reference notice exists"
    );
    assert!(!section_text(&partial).contains("must not become ordinary text"));

    let empty = extract_excalidraw_candidate(EMPTY).unwrap();
    assert_eq!(empty.coverage, ExtractionCoverage::SkippedNoExtractableText);
    assert!(empty.sections.is_empty());
    assert_eq!(notice_codes(&empty), ["excalidraw_no_extractable_text"]);

    let malformed = extract_excalidraw_candidate(MALFORMED).unwrap();
    assert_eq!(malformed.coverage, ExtractionCoverage::Quarantined);
    assert_eq!(notice_codes(&malformed), ["invalid_excalidraw_json"]);

    for (bytes, coverage, code) in [
        (
            b"[]".as_slice(),
            ExtractionCoverage::Quarantined,
            "excalidraw_root_not_object",
        ),
        (
            br#"{"elements":{}}"#.as_slice(),
            ExtractionCoverage::Quarantined,
            "excalidraw_elements_not_array",
        ),
        (
            br#"{"type":"excalidrawlib","elements":[]}"#.as_slice(),
            ExtractionCoverage::Quarantined,
            "excalidraw_unexpected_document_type",
        ),
        (
            // Zero sections plus at least one defect quarantines, Canvas parity.
            br#"{"elements":["bad"]}"#.as_slice(),
            ExtractionCoverage::Quarantined,
            "excalidraw_element_not_object",
        ),
        (
            // `elements` absent is tolerated; only a wrong type quarantines.
            br#"{}"#.as_slice(),
            ExtractionCoverage::SkippedNoExtractableText,
            "excalidraw_no_extractable_text",
        ),
        (
            br#"{"type":"excalidraw","elements":[]}"#.as_slice(),
            ExtractionCoverage::SkippedNoExtractableText,
            "excalidraw_no_extractable_text",
        ),
        (&[0xff], ExtractionCoverage::Unreadable, "non_utf8_source"),
        (
            b"{\"elements\":[]}\0".as_slice(),
            ExtractionCoverage::Unreadable,
            "binary_source",
        ),
    ] {
        let extracted = extract_excalidraw_candidate(bytes).unwrap();
        assert_eq!(extracted.coverage, coverage, "input {bytes:?}");
        assert_eq!(notice_codes(&extracted), [code], "input {bytes:?}");
        assert!(extracted.sections.is_empty());
        assert!(extracted.properties.is_empty());
    }

    let no_extractable_text = extract_excalidraw_candidate(EMPTY).unwrap();
    assert_eq!(
        no_extractable_text.notices[0].message,
        "Excalidraw drawing contains no authored text, frame names, or links; skipped with no extractable text"
    );
}

#[test]
fn soft_deleted_elements_are_never_extracted() {
    let extracted = extract_excalidraw_candidate(DELETED).unwrap();

    assert_eq!(extracted.coverage, ExtractionCoverage::IndexedComplete);
    assert_eq!(section_contents(&extracted), ["Live text"]);
    assert!(
        extracted.notices.is_empty(),
        "a soft-deleted element is a normal undo-buffer state, not a defect"
    );

    let sections = section_text(&extracted);
    let notices = extracted
        .notices
        .iter()
        .map(|notice| notice.message.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    let projection = projection_debug(&extracted);
    for sentinel in [
        "DELETEDTEXTSENTINELQZX",
        "DELETEDLINKSENTINELQZX",
        "DELETEDFRAMESENTINELQZX",
        // `customData` is arbitrary extension-owned JSON that routinely carries
        // authored strings, so it is a live-only projection key. Retaining it
        // for a tombstone would leak deleted authored content into the bag.
        "DELETEDCUSTOMDATASENTINELQZX",
    ] {
        assert!(!sections.contains(sentinel), "{sentinel} leaked into text");
        assert!(
            !notices.contains(sentinel),
            "{sentinel} leaked into notices"
        );
        assert!(
            !projection.contains(sentinel),
            "{sentinel} leaked into the property projection"
        );
    }
    // The soft-deleted elements are still projected structurally, so the state
    // is visible without their authored strings.
    let Some(PropertyValue::Map(map)) = extracted.properties.get("excalidraw") else {
        panic!("projection must be retained");
    };
    let Some(PropertyValue::Sequence(elements)) = map.get("elements") else {
        panic!("elements must be projected");
    };
    assert_eq!(elements.len(), 3);
    let PropertyValue::Map(first) = &elements[0] else {
        panic!("element projections are maps");
    };
    assert_eq!(first.get("isDeleted"), Some(&PropertyValue::Bool(true)));
    assert!(first.get("link").is_none());
}

#[test]
fn structural_data_is_identity_neutral_while_element_array_order_is_sequence_sensitive() {
    let original = extract_excalidraw_candidate(WELL_FORMED).unwrap();

    // Every structural and geometric field changes; the extracted evidence must
    // not move at all.
    let mut mutated: serde_json::Value = serde_json::from_slice(WELL_FORMED).unwrap();
    for (index, element) in mutated["elements"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        element["x"] = serde_json::json!(10_000.5 + index as f64);
        element["y"] = serde_json::json!(-20_000.25 - index as f64);
        element["width"] = serde_json::json!(900.75 + index as f64);
        element["height"] = serde_json::json!(700.5 + index as f64);
        element["angle"] = serde_json::json!(1.5 + index as f64);
        element["seed"] = serde_json::json!(555_000 + index);
        element["versionNonce"] = serde_json::json!(666_000 + index);
        element["version"] = serde_json::json!(900 + index);
        element["index"] = serde_json::json!(format!("z{index}"));
        element["updated"] = serde_json::json!(1_800_000_000_000_u64 + index as u64);
        if element["points"].is_array() {
            element["points"] = serde_json::json!([[1.0, 2.0], [3.0, 4.0]]);
        }
    }
    let mutated = extract_excalidraw_candidate(&serde_json::to_vec(&mutated).unwrap()).unwrap();
    assert_eq!(
        section_evidence(&original),
        section_evidence(&mutated),
        "geometry, seeds, nonces, fractional indices, and update stamps are display-only"
    );

    // Re-wrapping the layout-derived `text` while `originalText` is unchanged is
    // identity-neutral: this is what makes `originalText` the authored string.
    let mut rewrapped: serde_json::Value = serde_json::from_slice(WELL_FORMED).unwrap();
    for element in rewrapped["elements"].as_array_mut().unwrap() {
        if element["type"] == "text" {
            element["text"] = serde_json::json!("re\nwrapped\ndifferently");
        }
    }
    let rewrapped = extract_excalidraw_candidate(&serde_json::to_vec(&rewrapped).unwrap()).unwrap();
    assert_eq!(section_evidence(&original), section_evidence(&rewrapped));

    // Array order is authored z-order and is the emission order, so swapping two
    // elements moves the affected sections.
    let mut reordered: serde_json::Value = serde_json::from_slice(WELL_FORMED).unwrap();
    reordered["elements"].as_array_mut().unwrap().swap(0, 1);
    let reordered = extract_excalidraw_candidate(&serde_json::to_vec(&reordered).unwrap()).unwrap();
    assert_eq!(
        section_contents(&reordered)[..2],
        ["https://example.com/excalidraw-source", "Research Frame"]
    );
    assert_ne!(section_evidence(&original), section_evidence(&reordered));
}

/// Pins what reordering actually does to chunk identity, because the obvious
/// reading — "reordering changes the chunk IDs of the affected sections" — is
/// false and the wrong version of it was previously written into a helper doc
/// comment.
///
/// `chunk_id` (`src/source.rs`) hashes `vault_id`, `path`, `heading_path`, and
/// `chunk_ix`. Content is not an input, and §2.4 forces `heading_path == []` for
/// every Excalidraw section, so for a fixed source path the only surviving
/// determinant is ordinal position. Swapping two elements therefore leaves both
/// chunk IDs byte-identical and merely re-points them at each other's content;
/// inserting one element near the top re-points every later chunk ID, which is
/// why any drawing edit is a full-source chunk rewrite rather than a local one.
#[test]
fn excalidraw_chunk_identity_is_positional_not_content_addressed() {
    let original = extract_excalidraw_candidate(WELL_FORMED).unwrap();

    let mut reordered: serde_json::Value = serde_json::from_slice(WELL_FORMED).unwrap();
    reordered["elements"].as_array_mut().unwrap().swap(0, 1);
    let reordered = extract_excalidraw_candidate(&serde_json::to_vec(&reordered).unwrap()).unwrap();

    // Every input `chunk_id` still consumes is unchanged by the swap...
    assert_eq!(
        chunk_identity_inputs(&original),
        chunk_identity_inputs(&reordered),
        "reordering changes no chunk-identity input, so no chunk ID moves"
    );
    // ...while the content those identical IDs address has moved.
    assert_ne!(
        section_contents(&original)[..2],
        section_contents(&reordered)[..2]
    );

    // Inserting one element at the top shifts every later ordinal, so the whole
    // remaining chunk range is re-pointed rather than a single chunk changing.
    let mut inserted: serde_json::Value = serde_json::from_slice(WELL_FORMED).unwrap();
    inserted["elements"].as_array_mut().unwrap().insert(
        0,
        serde_json::json!({"id": "ins00000000001", "type": "text", "originalText": "Inserted"}),
    );
    let inserted = extract_excalidraw_candidate(&serde_json::to_vec(&inserted).unwrap()).unwrap();
    assert_eq!(inserted.sections.len(), original.sections.len() + 1);
    for (position, section) in original.sections.iter().enumerate() {
        assert_eq!(
            inserted.sections[position + 1].content,
            section.content,
            "every original section moved one ordinal, so its chunk ID now addresses different content"
        );
    }
}

/// The sibling-file half of this test is a **demonstration, not a falsifiable
/// assertion**: [`extract_excalidraw_candidate`] takes `&[u8]` and nothing else
/// — no path, no directory, no vault handle — so no implementation behind that
/// signature can reach `file00000000001.png`. The signature is the guarantee;
/// the sibling file only makes it concrete. The `dataURL` half *is* falsifiable:
/// the payload is present in the input bytes and must not survive extraction.
#[test]
fn no_file_or_payload_dereference() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temporary = std::env::temp_dir().join(format!(
        "kwiry-excalidraw-no-dereference-{}-{unique}",
        std::process::id()
    ));
    // A real sibling that a naive `fileId`/`files` resolver could reach.
    let referenced = temporary.join("file00000000001.png");
    fs::create_dir_all(&temporary).unwrap();
    fs::write(
        &referenced,
        "REFERENCED_FILE_SENTINEL_MUST_NEVER_ENTER_THE_DRAWING",
    )
    .unwrap();
    let drawing = temporary.join("board.excalidraw");
    fs::write(&drawing, WELL_FORMED).unwrap();

    let extracted = extract_excalidraw_candidate(&fs::read(&drawing).unwrap()).unwrap();
    let sections = section_text(&extracted);
    let projection = projection_debug(&extracted);
    for sentinel in [
        "REFERENCED_FILE_SENTINEL",
        "IMAGEPAYLOADSENTINELQZX",
        "data:image/png;base64",
    ] {
        assert!(!sections.contains(sentinel));
        assert!(!projection.contains(sentinel));
    }
    fs::remove_dir_all(temporary).unwrap();
}

#[test]
fn excalidraw_notices_and_property_budgets_are_bounded_without_losing_partial_coverage() {
    let mut elements = vec![serde_json::json!({
        "id": "live00000000001",
        "type": "text",
        "originalText": "usable"
    })];
    elements.extend((0..40).map(|_| serde_json::Value::String("bad".to_owned())));
    let bytes = serde_json::to_vec(&serde_json::json!({ "elements": elements })).unwrap();

    let extracted = extract_excalidraw_candidate(&bytes).unwrap();
    assert_eq!(extracted.coverage, ExtractionCoverage::IndexedPartial);
    assert_eq!(section_contents(&extracted), ["usable"]);
    assert_eq!(extracted.notices.len(), 32);
    assert_eq!(
        extracted.notices.last().map(|notice| notice.code.as_str()),
        Some("excalidraw_notices_truncated")
    );
    let warning = extracted
        .notices
        .iter()
        .map(|notice| notice.message.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(warning.len() < 4_096);

    // The projection of one element with `id`, `type`, and `groupIds` costs five
    // entries plus one per retained group identifier.
    let at_limit = group_id_document(MAX_EXCALIDRAW_PROPERTY_ENTRIES - 5);
    let retained = extract_excalidraw_candidate(&at_limit).unwrap();
    assert_eq!(retained.coverage, ExtractionCoverage::IndexedComplete);
    assert!(retained.properties.get("excalidraw").is_some());
    assert!(retained.notices.is_empty());

    let over_limit = group_id_document(MAX_EXCALIDRAW_PROPERTY_ENTRIES - 4);
    let dropped = extract_excalidraw_candidate(&over_limit).unwrap();
    assert_eq!(
        dropped.coverage,
        ExtractionCoverage::IndexedPartial,
        "an over-budget projection degrades honestly instead of silently"
    );
    assert_eq!(section_contents(&dropped), ["usable"]);
    assert!(dropped.properties.get("excalidraw").is_none());
    assert_eq!(
        notice_codes(&dropped),
        ["excalidraw_properties_not_retained"]
    );
}

#[test]
fn aggregate_excalidraw_section_limit_returns_a_typed_error_and_no_heading_limit_exists() {
    let at_limit = extract_excalidraw_candidate(frame_document(MAX_SECTIONS).as_bytes()).unwrap();
    assert_eq!(at_limit.sections.len(), MAX_SECTIONS);
    // At this scale the projection is far over its own entry budget, so it is
    // dropped while every section survives: text stays searchable either way.
    assert_eq!(at_limit.coverage, ExtractionCoverage::IndexedPartial);
    assert!(at_limit.properties.get("excalidraw").is_none());
    assert_eq!(
        notice_codes(&at_limit),
        ["excalidraw_properties_not_retained"]
    );

    let over_limit =
        extract_excalidraw_candidate(frame_document(MAX_SECTIONS + 1).as_bytes()).unwrap_err();
    assert_eq!(over_limit.code, "index_limit_exceeded");
    assert_eq!(
        over_limit.message,
        "prepared source exceeds the chunk inventory limit"
    );

    // Excalidraw text is plain, so the heading-byte ledger is never charged and
    // the heading-path limit Canvas can hit is unreachable here.
    let heading_shaped = "# ".to_owned() + &"h".repeat(1_100_000);
    let bytes = serde_json::to_vec(&serde_json::json!({
        "elements": [
            {"id": "a", "type": "text", "originalText": heading_shaped.clone()},
            {"id": "b", "type": "text", "originalText": heading_shaped}
        ]
    }))
    .unwrap();
    let extracted = extract_excalidraw_candidate(&bytes).unwrap();
    assert_eq!(extracted.sections.len(), 2);
    assert!(
        extracted
            .sections
            .iter()
            .all(|section| section.heading_path.is_empty())
    );
}

/// A declared elision must never be evicted by the notice cap.
///
/// The per-element notice lane is capped and its truncation is declared, but a
/// whole-document declaration arriving after the element pass used to share that
/// cap: it could pop a real defect notice and be replaced by the truncation
/// marker, which then misattributed its own cause. Terminal notices now occupy
/// a reserved lane that per-element notices are trimmed to make room for, and
/// the combined output still honours the declared cap.
#[test]
fn terminal_notices_are_never_evicted_by_a_full_per_element_notice_buffer() {
    for defects in [MAX_EXCALIDRAW_NOTICES - 2, MAX_EXCALIDRAW_NOTICES, 64] {
        let extracted = extract_excalidraw_candidate(&defective_document_with_files(defects, 0))
            .expect("a defective drawing still extracts");
        let codes = notice_codes(&extracted);
        assert!(
            codes.len() <= MAX_EXCALIDRAW_NOTICES,
            "defects={defects}: the declared notice cap still binds, got {}",
            codes.len()
        );
        assert!(
            codes.contains(&"excalidraw_image_payloads_not_retained"),
            "defects={defects}: the image-payload elision must stay declared, got {codes:?}"
        );
        if defects >= MAX_EXCALIDRAW_NOTICES {
            assert!(
                codes.contains(&"excalidraw_notices_truncated"),
                "defects={defects}: trimming the per-element lane must stay declared"
            );
        }
    }

    // Same guarantee for the terminal *defect*: the honest-degradation notice
    // must survive exactly when the source is most degraded.
    let extracted = extract_excalidraw_candidate(&defective_document_with_files(40, 5_000))
        .expect("a defective drawing still extracts");
    assert_eq!(extracted.coverage, ExtractionCoverage::IndexedPartial);
    assert!(extracted.properties.get("excalidraw").is_none());
    let codes = notice_codes(&extracted);
    assert!(codes.len() <= MAX_EXCALIDRAW_NOTICES);
    assert!(
        codes.contains(&"excalidraw_properties_not_retained"),
        "a dropped projection must be declared even with a full notice buffer, got {codes:?}"
    );
}

/// The image-payload notice must not assert a retention that did not happen.
///
/// `ExtractedSource::warning()` concatenates every notice into one user-facing
/// string, so emitting "payloads were summarized to their non-payload scalars"
/// alongside "the projection was dropped" produced a self-contradictory warning.
#[test]
fn the_image_payload_notice_is_emitted_only_when_the_projection_is_retained() {
    let retained = extract_excalidraw_candidate(&group_id_document_with_files(4)).unwrap();
    assert_eq!(retained.coverage, ExtractionCoverage::IndexedComplete);
    assert!(retained.properties.get("excalidraw").is_some());
    assert_eq!(
        notice_codes(&retained),
        ["excalidraw_image_payloads_not_retained"],
        "a retained projection declares the payload summary"
    );

    let dropped = extract_excalidraw_candidate(&group_id_document_with_files(
        MAX_EXCALIDRAW_PROPERTY_ENTRIES,
    ))
    .unwrap();
    assert!(dropped.properties.get("excalidraw").is_none());
    assert_eq!(
        notice_codes(&dropped),
        ["excalidraw_properties_not_retained"],
        "nothing was retained, so nothing may claim to have been summarized"
    );
}

/// An authored `link` must reach the index even when the element carrying it is
/// otherwise unusable.
///
/// Excalidraw's element `type` union is not a frozen spec — it has grown
/// `frame`, `magicframe`, `iframe`, and `embeddable` — so a type introduced by a
/// later release must not silently strand its URL in the retained projection
/// where lexical search cannot reach it.
#[test]
fn an_unusable_element_still_contributes_its_authored_link() {
    let bytes = br#"{"elements":[
        {"id":"a","type":"text","originalText":"usable"},
        {"id":"b","type":"future-thing","link":"https://example.com/UNSUPPORTEDTYPELINK"},
        {"id":"c","type":"frame","name":42,"link":"https://example.com/INVALIDFRAMENAMELINK"},
        {"id":"d","type":"text","link":"https://example.com/MISSINGTEXTLINK"}
    ]}"#;
    let extracted = extract_excalidraw_candidate(bytes).unwrap();

    assert_eq!(
        extracted.coverage,
        ExtractionCoverage::IndexedPartial,
        "the unusable elements are still declared as defects"
    );
    assert_eq!(
        section_contents(&extracted),
        [
            "usable",
            "https://example.com/UNSUPPORTEDTYPELINK",
            "https://example.com/INVALIDFRAMENAMELINK",
            "https://example.com/MISSINGTEXTLINK",
        ]
    );
    assert_eq!(
        notice_codes(&extracted),
        [
            "excalidraw_element_unsupported_type",
            "excalidraw_frame_invalid_name",
            "excalidraw_text_element_missing_text",
        ]
    );
}

/// The retained-byte budget is a declared limit, so its boundary is pinned:
/// removing the byte check makes the over-limit case retain and this test fail.
///
/// One projected element (`id` + `type`) plus the `elements` and `appState`
/// keys and the nested `bigSentinel` key cost a fixed 38 bytes, so the authored
/// string is sized to land the total exactly on the limit.
#[test]
fn the_retained_property_byte_budget_boundary_is_exact() {
    const FIXED_PROJECTION_BYTES: usize = 38;

    let at_limit = extract_excalidraw_candidate(&app_state_document(
        MAX_EXCALIDRAW_PROPERTY_BYTES - FIXED_PROJECTION_BYTES,
    ))
    .unwrap();
    assert_eq!(at_limit.coverage, ExtractionCoverage::IndexedComplete);
    assert!(
        at_limit.properties.get("excalidraw").is_some(),
        "a projection landing exactly on the byte budget is retained"
    );
    assert!(at_limit.notices.is_empty());

    let over_limit = extract_excalidraw_candidate(&app_state_document(
        MAX_EXCALIDRAW_PROPERTY_BYTES - FIXED_PROJECTION_BYTES + 1,
    ))
    .unwrap();
    assert_eq!(
        over_limit.coverage,
        ExtractionCoverage::IndexedPartial,
        "one byte over the budget degrades honestly instead of silently"
    );
    assert_eq!(section_contents(&over_limit), ["usable"]);
    assert!(over_limit.properties.get("excalidraw").is_none());
    assert_eq!(
        notice_codes(&over_limit),
        ["excalidraw_properties_not_retained"]
    );
}

/// The document-type gate rejects any `type` that is present and is not exactly
/// the string `"excalidraw"`, whatever its JSON type. Gating on "present and a
/// non-empty string" admitted a library file whose discriminator had been
/// corrupted to a number, null, array, or blank string.
#[test]
fn a_present_document_type_must_be_exactly_the_excalidraw_string() {
    for rejected in [
        r#"{"type":42,"elements":[{"id":"a","type":"text","originalText":"u"}]}"#,
        r#"{"type":null,"elements":[{"id":"a","type":"text","originalText":"u"}]}"#,
        r#"{"type":["excalidrawlib"],"elements":[{"id":"a","type":"text","originalText":"u"}]}"#,
        r#"{"type":{},"elements":[{"id":"a","type":"text","originalText":"u"}]}"#,
        r#"{"type":"   ","elements":[{"id":"a","type":"text","originalText":"u"}]}"#,
        r#"{"type":"","elements":[{"id":"a","type":"text","originalText":"u"}]}"#,
        r#"{"type":"excalidraw ","elements":[{"id":"a","type":"text","originalText":"u"}]}"#,
        r#"{"type":"excalidrawlib","elements":[{"id":"a","type":"text","originalText":"u"}]}"#,
    ] {
        let extracted = extract_excalidraw_candidate(rejected.as_bytes()).unwrap();
        assert_eq!(
            extracted.coverage,
            ExtractionCoverage::Quarantined,
            "input {rejected}"
        );
        assert_eq!(
            notice_codes(&extracted),
            ["excalidraw_unexpected_document_type"],
            "input {rejected}"
        );
    }

    // An absent `type` stays tolerated: bare exporter output omits it.
    let tolerated = extract_excalidraw_candidate(
        br#"{"elements":[{"id":"a","type":"text","originalText":"u"}]}"#,
    )
    .unwrap();
    assert_eq!(tolerated.coverage, ExtractionCoverage::IndexedComplete);
}

/// A soft-deleted tombstone is an undo-buffer record, not an authored element,
/// so it must not claim its ID against a live element that reuses it. It used
/// to: the live element was dropped with `excalidraw_duplicate_id`, and as the
/// only section it escalated the whole drawing to quarantined.
#[test]
fn a_soft_deleted_tombstone_does_not_claim_its_id_against_a_live_element() {
    let bytes = br#"{"elements":[
        {"id":"dup","type":"text","isDeleted":true,"originalText":"DELETEDTEXTSENTINELQZX"},
        {"id":"dup","type":"text","originalText":"LIVETEXTSENTINELQZX"}
    ]}"#;
    let extracted = extract_excalidraw_candidate(bytes).unwrap();

    assert_eq!(
        extracted.coverage,
        ExtractionCoverage::IndexedComplete,
        "a tombstone reusing a live ID is not an extraction defect"
    );
    assert_eq!(section_contents(&extracted), ["LIVETEXTSENTINELQZX"]);
    assert!(extracted.notices.is_empty());
    assert!(!projection_debug(&extracted).contains("DELETEDTEXTSENTINELQZX"));

    // Two *live* elements sharing an ID is still a defect.
    let duplicated = extract_excalidraw_candidate(
        br#"{"elements":[
            {"id":"dup","type":"text","originalText":"first"},
            {"id":"dup","type":"text","originalText":"second"}
        ]}"#,
    )
    .unwrap();
    assert_eq!(duplicated.coverage, ExtractionCoverage::IndexedPartial);
    assert_eq!(section_contents(&duplicated), ["first"]);
    assert_eq!(notice_codes(&duplicated), ["excalidraw_duplicate_id"]);
}

/// `originalText` postdates `text` in the element schema, so a pre-`originalText`
/// element falls back to the layout-wrapped `text`. That fallback is knowingly
/// **not** identity-neutral — a container resize rewrites the wrap breaks and
/// re-indexes the element — and losing the words entirely would be worse, so the
/// limit is pinned here rather than left implicit.
#[test]
fn the_layout_wrapped_text_fallback_is_indexed_but_is_not_identity_neutral() {
    let wrapped = extract_excalidraw_candidate(
        br#"{"elements":[{"id":"a","type":"text","text":"super\ncali"}]}"#,
    )
    .unwrap();
    assert_eq!(wrapped.coverage, ExtractionCoverage::IndexedComplete);
    assert_eq!(
        section_contents(&wrapped),
        ["super\ncali"],
        "a pre-originalText element is still indexed, mid-word break and all"
    );

    let rewrapped = extract_excalidraw_candidate(
        br#"{"elements":[{"id":"a","type":"text","text":"superc\nali"}]}"#,
    )
    .unwrap();
    assert_ne!(
        section_contents(&wrapped),
        section_contents(&rewrapped),
        "without originalText a resize moves the extracted evidence; this is the known limit"
    );

    // Adding `originalText` restores neutrality across both wrappings.
    let authored_a = extract_excalidraw_candidate(
        br#"{"elements":[{"id":"a","type":"text","originalText":"supercali","text":"super\ncali"}]}"#,
    )
    .unwrap();
    let authored_b = extract_excalidraw_candidate(
        br#"{"elements":[{"id":"a","type":"text","originalText":"supercali","text":"superc\nali"}]}"#,
    )
    .unwrap();
    assert_eq!(section_contents(&authored_a), ["supercali"]);
    assert_eq!(section_contents(&authored_a), section_contents(&authored_b));
}

#[test]
fn excalidraw_is_admitted_by_the_owner_amendment() {
    assert_eq!(
        format_specs().len(),
        9,
        "the HTML amendment extends the source-format set to nine members"
    );
    assert!(
        format_specs()
            .iter()
            .any(|spec| spec.extensions.contains(&"excalidraw")),
        "a registry entry must claim the .excalidraw extension"
    );
    assert_eq!(
        SourceFormat::from_path("Drawings/board.excalidraw"),
        Some(SourceFormat::Excalidraw)
    );
    assert_eq!(
        SourceFormat::from_path("Drawings/board.excalidraw.md"),
        Some(SourceFormat::Markdown),
        "the Obsidian wrapper stays classified as markdown by its last extension"
    );
    const {
        assert!(
            SOURCE_PREPARATION_SCHEMA_VERSION >= 8,
            "Excalidraw was admitted at schema 8; later waves may advance it, never retreat"
        );
    }

    // A declared format that disagrees with the path extension is still refused.
    let rejected = prepare_source_buffer(
        &SourceDescriptor {
            vault_id: "excalidraw-fixture".to_owned(),
            room: None,
            path: "Drawings/board.excalidraw".to_owned(),
            format: SourceFormat::Canvas,
            byte_length: WELL_FORMED.len() as u64,
            mtime: 42,
            mtime_nanos: 42_000_000_000,
        },
        WELL_FORMED,
    )
    .expect_err("a descriptor format must agree with the registered extension");
    assert_eq!(rejected.code, "invalid_source");

    // The admitted route extracts through the shared source model.
    let prepared = prepare_source_buffer(
        &SourceDescriptor {
            vault_id: "excalidraw-fixture".to_owned(),
            room: None,
            path: "Drawings/board.excalidraw".to_owned(),
            format: SourceFormat::Excalidraw,
            byte_length: WELL_FORMED.len() as u64,
            mtime: 42,
            mtime_nanos: 42_000_000_000,
        },
        WELL_FORMED,
    )
    .expect("an admitted Excalidraw source prepares through the shared pipeline");
    assert_eq!(prepared.schema_version, SOURCE_PREPARATION_SCHEMA_VERSION);
    assert_eq!(prepared.format, SourceFormat::Excalidraw);
    assert!(!prepared.chunks.is_empty());
}

#[test]
fn a_source_that_exhausts_its_extraction_budget_quarantines_without_failing_the_batch() {
    // One drawing with more elements than the chunk inventory allows must not
    // reject the whole source batch: a build of thousands of notes cannot hinge
    // on one oversized file.
    let mut elements = String::from("[");
    for index in 0..=crate_max_sections() {
        if index > 0 {
            elements.push(',');
        }
        elements.push_str(&format!(
            r#"{{"id":"e{index}","type":"text","originalText":"element {index}"}}"#,
        ));
    }
    elements.push(']');
    let document = format!(r#"{{"type":"excalidraw","elements":{elements}}}"#);

    let prepared = prepare_source_buffer(
        &SourceDescriptor {
            vault_id: "excalidraw-fixture".to_owned(),
            room: None,
            path: "Drawings/huge.excalidraw".to_owned(),
            format: SourceFormat::Excalidraw,
            byte_length: document.len() as u64,
            mtime: 42,
            mtime_nanos: 42_000_000_000,
        },
        document.as_bytes(),
    )
    .expect("an over-budget source is a per-source outcome, never a batch failure");
    assert_eq!(prepared.coverage, ExtractionCoverage::Quarantined);
    assert!(prepared.chunks.is_empty());
    assert!(prepared.warning.is_some());
}

fn crate_max_sections() -> usize {
    kwiry_core::MAX_PREPARED_CHUNKS_PER_SOURCE
}

fn section_contents(extracted: &ExtractedSource) -> Vec<&str> {
    extracted
        .sections
        .iter()
        .map(|section| section.content.as_str())
        .collect()
}

fn section_text(extracted: &ExtractedSource) -> String {
    section_contents(extracted).join("\n")
}

/// The extracted evidence a change is allowed to move: ordered section
/// position, heading path, and content.
///
/// This is deliberately **not** the determinant of chunk identity. `chunk_id`
/// hashes `vault_id`, `path`, `heading_path`, and `chunk_ix` only — content is
/// not an input — and every Excalidraw section carries an empty `heading_path`,
/// so for a fixed source path Excalidraw chunk identity is purely positional.
/// See `excalidraw_chunk_identity_is_positional_not_content_addressed`.
fn section_evidence(extracted: &ExtractedSource) -> Vec<(usize, Vec<String>, String)> {
    extracted
        .sections
        .iter()
        .enumerate()
        .map(|(position, section)| {
            (
                position,
                section.heading_path.clone(),
                section.content.clone(),
            )
        })
        .collect()
}

fn notice_codes(extracted: &ExtractedSource) -> Vec<&str> {
    extracted
        .notices
        .iter()
        .map(|notice| notice.code.as_str())
        .collect()
}

fn projection_debug(extracted: &ExtractedSource) -> String {
    format!("{:?}", extracted.properties.get("excalidraw"))
}

/// Every input `chunk_id` consumes that an extraction can influence: the
/// ordinal position and the heading path. Content is deliberately absent.
fn chunk_identity_inputs(extracted: &ExtractedSource) -> Vec<(usize, Vec<String>)> {
    extracted
        .sections
        .iter()
        .enumerate()
        .map(|(position, section)| (position, section.heading_path.clone()))
        .collect()
}

/// One usable element, `defects` unusable ones, a non-empty `files` map, and an
/// optional `groupIds` array sized to blow the projection budget.
fn defective_document_with_files(defects: usize, group_ids: usize) -> Vec<u8> {
    let mut elements = vec![serde_json::json!({
        "id": "live00000000001",
        "type": "text",
        "groupIds": (0..group_ids)
            .map(|_| serde_json::Value::String("g".to_owned()))
            .collect::<Vec<_>>(),
        "originalText": "usable"
    })];
    elements.extend((0..defects).map(|_| serde_json::Value::String("bad".to_owned())));
    serde_json::to_vec(&serde_json::json!({
        "elements": elements,
        "files": {"file00000000001": {"mimeType": "image/png", "dataURL": "data:image/png;base64,SGVsbG8="}}
    }))
    .unwrap()
}

fn group_id_document_with_files(group_ids: usize) -> Vec<u8> {
    defective_document_with_files(0, group_ids)
}

/// One indexable element plus an `appState` string sized to probe the retained
/// byte budget.
fn app_state_document(app_state_bytes: usize) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "appState": {"bigSentinel": "x".repeat(app_state_bytes)},
        "elements": [{"id": "a", "type": "text", "originalText": "usable"}]
    }))
    .unwrap()
}

fn group_id_document(group_ids: usize) -> Vec<u8> {
    let group_ids = (0..group_ids)
        .map(|_| serde_json::Value::String("g".to_owned()))
        .collect::<Vec<_>>();
    serde_json::to_vec(&serde_json::json!({
        "elements": [{
            "id": "a",
            "type": "text",
            "groupIds": group_ids,
            "originalText": "usable"
        }]
    }))
    .unwrap()
}

fn frame_document(frames: usize) -> String {
    let mut document = String::from("{\"elements\":[");
    for index in 0..frames {
        if index != 0 {
            document.push(',');
        }
        write!(
            document,
            "{{\"id\":\"{index:016x}\",\"type\":\"frame\",\"name\":\"f\"}}"
        )
        .unwrap();
    }
    document.push_str("]}");
    document
}
