// SPDX-License-Identifier: MIT OR Apache-2.0

use kwiry_core::{
    ContentRole, ExtractionCoverage, SOURCE_PREPARATION_SCHEMA_VERSION, SourceDescriptor,
    SourceFormat, SourcePreparationKind, format_identity_fingerprint, prepare_source_buffer,
};

fn prepare(path: &str, bytes: &[u8]) -> kwiry_core::SourcePreparation {
    prepare_source_buffer(
        &SourceDescriptor {
            vault_id: "html-fixture".to_owned(),
            room: None,
            path: path.to_owned(),
            format: SourceFormat::Html,
            byte_length: bytes.len() as u64,
            mtime: 42,
            mtime_nanos: 42_000_000_000,
        },
        bytes,
    )
    .expect("prepare HTML source")
}

#[test]
fn registry_identity_and_navigation_policy_are_closed_and_pinned() {
    assert_eq!(
        SourceFormat::from_path("page.html"),
        Some(SourceFormat::Html)
    );
    assert_eq!(
        SourceFormat::from_path("PAGE.HTM"),
        Some(SourceFormat::Html)
    );
    assert_eq!(SourceFormat::from_path("page.xhtml"), None);
    assert_eq!(
        SourceFormat::from_path("page.html.md"),
        Some(SourceFormat::Markdown)
    );
    assert!(!SourceFormat::Html.supports_section_links());
    assert_eq!(
        format_identity_fingerprint(SourceFormat::Html),
        "218acfdef07624a39eb071ba8221a7761b6f2ebf26e3ef180928dd7a6a65a9d7"
    );
    let policy = SourceFormat::Html.spec().policy;
    assert!(policy.role_tagged_chunk_ids);
    assert!(policy.suppress_non_primary_boosts);
    assert!(policy.metadata_only_carrier);
}

#[test]
fn title_only_html_uses_an_empty_metadata_carrier_without_body_copy() {
    let prepared = prepare("title-only.html", b"<title>Canonical only</title>");
    assert_eq!(prepared.schema_version, SOURCE_PREPARATION_SCHEMA_VERSION);
    assert_eq!(prepared.schema_version, 10);
    assert_eq!(prepared.kind, SourcePreparationKind::Indexed);
    assert_eq!(prepared.coverage, ExtractionCoverage::IndexedComplete);
    assert_eq!(
        prepared.normalized_exact.title.as_deref(),
        Some("canonical only")
    );
    assert_eq!(
        prepared
            .canonical_frontmatter
            .as_ref()
            .and_then(|frontmatter| frontmatter.title()),
        Some("Canonical only")
    );
    let encoded = serde_json::to_value(&prepared).unwrap();
    assert_eq!(encoded["canonical_frontmatter"]["title"], "Canonical only");
    let restored: kwiry_core::SourcePreparation = serde_json::from_value(encoded).unwrap();
    assert_eq!(restored, prepared);
    assert_eq!(prepared.chunks.len(), 1);
    assert!(prepared.chunks[0].content.is_empty());
    assert_eq!(prepared.chunks[0].content_role, ContentRole::Primary);
    assert!(prepared.chunks[0].source_locator.is_none());
    assert!(prepared.chunks[0].links_out.is_empty());
}

#[test]
fn long_canonical_html_title_remains_indexed_without_an_exact_projection() {
    let title = "x".repeat(4_097);
    let source = format!("<title>{title}</title>");
    let prepared = prepare("long-title.html", source.as_bytes());
    assert_eq!(prepared.kind, SourcePreparationKind::Indexed);
    assert_eq!(prepared.normalized_exact.title, None);
    assert_eq!(
        prepared
            .canonical_frontmatter
            .as_ref()
            .and_then(|frontmatter| frontmatter.title()),
        Some(title.as_str())
    );
    assert_eq!(prepared.chunks.len(), 1);
}

#[test]
fn latent_html_is_plain_searchable_but_has_no_boost_lanes() {
    let prepared = prepare(
        "roles.htm",
        b"<h1>Primary Heading</h1><p>primary CVE-2026-1234</p>\
          <nav><h2>Chrome Heading</h2><p>latent CVE-2026-9999</p></nav>\
          <p aria-label='attribute CVE-2026-7777'>reader</p>",
    );
    assert_eq!(prepared.coverage, ExtractionCoverage::IndexedComplete);
    assert!(
        prepared
            .chunks
            .iter()
            .all(|chunk| chunk.source_locator.is_none())
    );
    assert!(
        prepared
            .chunks
            .iter()
            .all(|chunk| chunk.links_out.is_empty())
    );

    let primary = prepared
        .chunks
        .iter()
        .find(|chunk| chunk.content.contains("CVE-2026-1234"))
        .expect("primary content");
    assert_eq!(primary.content_role, ContentRole::Primary);
    assert_eq!(primary.heading_text, "Primary Heading");
    assert!(
        primary
            .technical_identifiers
            .iter()
            .any(|id| id == "cve-2026-1234")
    );

    for latent in prepared
        .chunks
        .iter()
        .filter(|chunk| chunk.content_role == ContentRole::Latent)
    {
        assert!(latent.heading_text.is_empty());
        assert!(latent.normalized_heading.is_none());
        assert!(latent.technical_identifiers.is_empty());
        let first = u8::from_str_radix(&latent.chunk_id[..2], 16).unwrap();
        assert_eq!(first & 0xc0, 0x80);
    }
}

#[test]
fn unreadable_or_quarantined_html_never_keeps_an_indexed_prefix() {
    let unreadable = prepare("invalid.html", &[b'<', b'p', b'>', b'x', 0xff]);
    assert_eq!(unreadable.coverage, ExtractionCoverage::Unreadable);
    assert_eq!(unreadable.kind, SourcePreparationKind::Skipped);
    assert!(unreadable.chunks.is_empty());
    assert!(unreadable.frontmatter.is_empty());
    assert!(unreadable.canonical_frontmatter.is_none());
    assert_eq!(
        unreadable.warning.as_deref(),
        Some("HTML source is not valid UTF-8")
    );

    const HTML_MAX_NODES: usize = 100_000;
    fn late_node_boundary(node_count: usize) -> Vec<u8> {
        let br_count = node_count - 3; // root + early paragraph + its text
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"<title>early-title</title><p>early-body</p>");
        for _ in 0..br_count {
            bytes.extend_from_slice(b"<br>");
        }
        bytes
    }

    let admitted_bytes = late_node_boundary(HTML_MAX_NODES);
    let admitted = prepare("at-node-bound.html", &admitted_bytes);
    assert_eq!(admitted.coverage, ExtractionCoverage::IndexedComplete);
    assert_eq!(admitted.kind, SourcePreparationKind::Indexed);
    assert_eq!(
        admitted
            .canonical_frontmatter
            .as_ref()
            .and_then(|frontmatter| frontmatter.title()),
        Some("early-title")
    );
    assert!(
        admitted
            .chunks
            .iter()
            .any(|chunk| chunk.content == "early-body")
    );

    let breached_bytes = late_node_boundary(HTML_MAX_NODES + 1);
    let breached = prepare("past-node-bound.html", &breached_bytes);
    assert_eq!(breached.coverage, ExtractionCoverage::Quarantined);
    assert_eq!(breached.kind, SourcePreparationKind::Skipped);
    assert!(breached.chunks.is_empty());
    assert!(breached.frontmatter.is_empty());
    assert!(breached.canonical_frontmatter.is_none());
    assert!(breached.normalized_exact.title.is_none());
    assert_eq!(
        breached.warning.as_deref(),
        Some("HTML extraction exceeded a mandatory budget")
    );
}

#[test]
fn malformed_html_prepares_byte_identically_on_repeated_runs() {
    let bytes = b"<p><b><i>one</b>two</i><table>before<tr><td>cell</tr>after</table>end";
    let first = prepare("malformed.html", bytes);
    let second = prepare("malformed.html", bytes);
    assert_eq!(first, second);
    assert_eq!(first.coverage, ExtractionCoverage::IndexedComplete);
    let joined = first
        .chunks
        .iter()
        .map(|chunk| chunk.content.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    for needle in ["one", "two", "before", "after", "cell", "end"] {
        assert_eq!(joined.matches(needle).count(), 1, "{joined}");
    }
}
