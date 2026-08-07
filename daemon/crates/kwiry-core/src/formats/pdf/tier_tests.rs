// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the two extraction tiers do differently, asserted from both sides.
//!
//! Every test here is compiled in both configurations. The ones whose expected
//! answer depends on the tier are `cfg`-gated in pairs, so neither tier's
//! behaviour is described only by the absence of an assertion.

use crate::extract::ExtractionCoverage;
use crate::format::SourceFormat;
use crate::policy::{ExtractionProfile, extraction_profile_for};

use super::candidate::extract_pdf_candidate;
#[cfg(feature = "native-pdf-extractor")]
use super::test_support::truetype_font_with_cmap_groups;
use super::test_support::{
    DocumentSpec, PageSpec, helvetica_document, stream_object, truetype_font,
};

/// Glyph ids 3, 4, 5 shown as two-byte `Identity-H` codes: the fixture font
/// maps them to `K`, `W`, `R`.
const GLYPH_ID_SHOW: &[u8] = b"BT /F1 12 Tf 72 700 Td <000300040005> Tj ET";

/// Shift_JIS for `日本語`, preceded by ASCII `PDF ` so one show operand
/// exercises both halves of a legacy CMap's mixed codespace.
const MIXED_WIDTH_SHOW: &[u8] = b"BT /F1 12 Tf 72 700 Td <50444620 93FA967B8CEA> Tj ET";

/// A composite font with the given `/Encoding` CMap name and no `/ToUnicode`.
/// `embed` controls whether the descendant carries a real font program, which
/// is the only thing the enhanced tier can recover glyph ids from.
/// `composite_document` with an author-supplied font program, so a test can
/// hand the enhanced tier a `cmap` it did not build itself.
#[cfg(feature = "native-pdf-extractor")]
fn composite_document_with_program(pages: &[&[u8]], cmap_name: &str, program: Vec<u8>) -> Vec<u8> {
    let font_id = DocumentSpec::first_font_id(pages.len());
    let descriptor_id = font_id + 2;
    let program_id = font_id + 3;
    let font_objects = vec![
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /Subset+Body \
             /Encoding /{cmap_name} /DescendantFonts [{} 0 R] >>",
            font_id + 1
        )
        .into_bytes(),
        format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Subset+Body \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
             /CIDToGIDMap /Identity /DW 1000 /FontDescriptor {descriptor_id} 0 R >>"
        )
        .into_bytes(),
        format!(
            "<< /Type /FontDescriptor /FontName /Subset+Body /Flags 4 \
             /FontFile2 {program_id} 0 R >>"
        )
        .into_bytes(),
        stream_object("", &program),
    ];
    DocumentSpec {
        pages: pages.iter().map(|page| PageSpec::new(page)).collect(),
        font_objects,
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build()
}

fn composite_document(pages: &[&[u8]], cmap_name: &str, embed: bool) -> Vec<u8> {
    let font_id = DocumentSpec::first_font_id(pages.len());
    let descriptor_id = font_id + 2;
    let program_id = font_id + 3;
    let mut font_objects = vec![
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /Subset+Body \
             /Encoding /{cmap_name} /DescendantFonts [{} 0 R] >>",
            font_id + 1
        )
        .into_bytes(),
        format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Subset+Body \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
             /CIDToGIDMap /Identity /DW 1000 {} >>",
            if embed {
                format!("/FontDescriptor {descriptor_id} 0 R")
            } else {
                String::new()
            }
        )
        .into_bytes(),
    ];
    if embed {
        font_objects.push(
            format!(
                "<< /Type /FontDescriptor /FontName /Subset+Body /Flags 4 \
                 /FontFile2 {program_id} 0 R >>"
            )
            .into_bytes(),
        );
        let program = truetype_font(&[('K', 3), ('W', 4), ('R', 5)], 6);
        font_objects.push(stream_object("", &program));
    }
    // `/F2` is an ordinary Latin font both tiers read. A page that uses only
    // `/F2` is readable by the portable tier and is still declined with the
    // rest of the source, which is the whole-source rule under test.
    let helvetica_id = font_id + font_objects.len();
    font_objects.push(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );
    DocumentSpec {
        pages: pages.iter().map(|page| PageSpec::new(page)).collect(),
        font_objects,
        font_resources: format!("/F1 {font_id} 0 R /F2 {helvetica_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build()
}

#[test]
fn the_compiled_profile_matches_the_compiled_extractor_set() {
    // The profile is not a label someone remembered to set: it names the same
    // compile-time fact the recovery is gated on.
    let expected = if cfg!(feature = "native-pdf-extractor") {
        ExtractionProfile::Enhanced
    } else {
        ExtractionProfile::Portable
    };
    assert_eq!(extraction_profile_for(SourceFormat::Pdf), expected);
}

#[cfg(not(feature = "native-pdf-extractor"))]
#[test]
fn the_portable_tier_declines_an_identity_subset_whole() {
    let bytes = composite_document(&[GLYPH_ID_SHOW], "Identity-H", true);
    let candidate = extract_pdf_candidate(&bytes);

    assert_eq!(
        candidate.coverage,
        ExtractionCoverage::SkippedNoExtractableText
    );
    // No section at all, so no chunk identity at all. That is what makes
    // switching tiers a pure insertion instead of a
    // same-identity/different-content collision.
    assert!(candidate.sections.is_empty());
    assert!(
        candidate
            .notices
            .iter()
            .any(|notice| notice.code == "pdf_undecodable_font")
    );
}

#[cfg(not(feature = "native-pdf-extractor"))]
#[test]
fn the_portable_tier_declines_the_whole_source_not_the_offending_page() {
    // Page 1 is plain Latin text this tier reads perfectly well. It is still
    // declined, because a document indexed with page 2 missing would have a
    // different section inventory than the same document read by the enhanced
    // tier — and both inventories would mint chunk 0 for the same path.
    let latin = b"BT /F2 12 Tf 72 700 Td (Readable page) Tj ET".to_vec();
    let bytes = composite_document(&[&latin, GLYPH_ID_SHOW], "Identity-H", true);
    let candidate = extract_pdf_candidate(&bytes);

    assert_eq!(candidate.page_count, 2);
    assert!(candidate.sections.is_empty());
    assert_eq!(
        candidate.coverage,
        ExtractionCoverage::SkippedNoExtractableText
    );
}

#[cfg(feature = "native-pdf-extractor")]
#[test]
fn the_enhanced_tier_recovers_text_from_the_embedded_font() {
    let bytes = composite_document(&[GLYPH_ID_SHOW], "Identity-H", true);
    let candidate = extract_pdf_candidate(&bytes);

    assert!(candidate.coverage.is_indexed(), "{:?}", candidate.coverage);
    assert_eq!(candidate.sections.len(), 1);
    // Glyph ids 3, 4, 5 inverted through the font's own `cmap`.
    assert_eq!(candidate.sections[0].content, "KWR");
    assert!(
        !candidate
            .notices
            .iter()
            .any(|notice| notice.code == "pdf_undecodable_font")
    );
}

#[cfg(feature = "native-pdf-extractor")]
#[test]
fn the_enhanced_tier_reads_every_page_of_an_identity_subset() {
    let latin = b"BT /F2 12 Tf 72 700 Td (Readable page) Tj ET".to_vec();
    let bytes = composite_document(&[&latin, GLYPH_ID_SHOW], "Identity-H", true);
    let candidate = extract_pdf_candidate(&bytes);

    assert_eq!(candidate.sections.len(), 2);
    assert_eq!(candidate.sections[0].locator.page, 1);
    assert_eq!(candidate.sections[1].locator.page, 2);
    assert_eq!(candidate.sections[0].content, "Readable page");
    assert_eq!(candidate.sections[1].content, "KWR");
}

/// The enhanced tier is not a licence to guess. A font that is not embedded has
/// nothing to recover from, so it is declined in *both* tiers rather than being
/// decoded as if the glyph ids were characters.
#[test]
fn an_identity_subset_with_no_embedded_program_is_declined_in_both_tiers() {
    let bytes = composite_document(&[GLYPH_ID_SHOW], "Identity-H", false);
    let candidate = extract_pdf_candidate(&bytes);

    assert!(candidate.sections.is_empty());
    assert_eq!(
        candidate.coverage,
        ExtractionCoverage::SkippedNoExtractableText
    );
    assert!(
        candidate
            .notices
            .iter()
            .any(|notice| notice.code == "pdf_undecodable_font")
    );
}

/// Legacy predefined CJK CMaps are **not** the divergence: `encoding_rs` is
/// already in the portable graph via `quick-xml`, so both tiers decode them and
/// this assertion is compiled unchanged in both.
///
/// It also pins the fix it represents. `lopdf` has no table for these names and
/// falls back to `STANDARD_ENCODING`, which would read the Shift_JIS bytes
/// below as Latin — text nobody wrote.
#[test]
fn a_legacy_cjk_cmap_is_decoded_identically_in_both_tiers() {
    let bytes = composite_document(&[MIXED_WIDTH_SHOW], "90ms-RKSJ-H", false);
    let candidate = extract_pdf_candidate(&bytes);

    assert_eq!(candidate.sections.len(), 1);
    let content = &candidate.sections[0].content;
    assert!(content.contains("日本語"), "{content:?}");
    // The mixed codespace is segmented, not assumed: the single-byte ASCII run
    // and the two-byte kanji run come out of the same show operand.
    assert!(content.contains("PDF"), "{content:?}");
    assert!(!content.contains('\u{fffd}'), "{content:?}");
}

/// The tiers must agree byte-for-byte on every source neither one declines.
/// This is the invariant that keeps chunk identities from colliding, and it is
/// asserted against a literal expected string so both builds are checked
/// against the same fixed answer rather than against each other.
#[test]
fn a_source_neither_tier_declines_composes_identically() {
    let bytes = helvetica_document(&[
        b"BT /F1 11 Tf 72 700 Td (Name) Tj 120 0 Td (Qty) Tj 74 0 Td (Price) Tj ET",
    ]);
    let candidate = extract_pdf_candidate(&bytes);

    assert_eq!(candidate.sections.len(), 1);
    assert_eq!(candidate.sections[0].content, "Name Qty Price");
    assert_eq!(candidate.sections[0].locator.page, 1);
    assert!(candidate.coverage.is_indexed());
}

/// The inverse of the pre-admission `neither_tier_makes_pdf_admissible`: both
/// tiers now reach the dispatch seam, and a source neither declines is admitted
/// identically by either. Which tier is compiled is a coverage fact, never an
/// admission fact.
#[test]
fn both_tiers_admit_pdf_through_the_dispatch_seam() {
    let bytes = helvetica_document(&[b"BT /F1 11 Tf 72 700 Td (Admissible) Tj ET"]);
    assert!(!extract_pdf_candidate(&bytes).sections.is_empty());

    assert!(SourceFormat::Pdf.is_extractable());
    assert_eq!(
        SourceFormat::from_extractable_path("paper.pdf"),
        Some(SourceFormat::Pdf)
    );
    let extracted = crate::formats::extract_source(SourceFormat::Pdf, &bytes).unwrap();
    assert_eq!(extracted.coverage, ExtractionCoverage::IndexedComplete);
    assert_eq!(
        extracted
            .sections
            .iter()
            .map(|section| section.content.as_str())
            .collect::<Vec<_>>(),
        ["Admissible"]
    );
    // Admission is tier-independent; only the reported profile differs.
    assert_ne!(
        extraction_profile_for(SourceFormat::Pdf),
        ExtractionProfile::None
    );
}

// ---------------------------------------------------------------------------
// Regressions
// ---------------------------------------------------------------------------

/// `Subtable::codepoints` iterates `start_char_code..=end_char_code` for every
/// group, with no cap and no check that the end is a Unicode scalar value. The
/// glyph-map cap bounded the map, not the walk, so it never fired: one group
/// spanning the whole `u32` range cost 2.5 s, and a 1,753-byte PDF declaring
/// twenty thousand of them did not finish in an hour.
///
/// Twenty full-range groups is 85 billion code points under the old walk. The
/// assertion is that this returns at all, and returns the right text.
#[test]
#[cfg(feature = "native-pdf-extractor")]
fn a_cmap_declaring_full_range_groups_terminates() {
    let mut groups: Vec<(u32, u32, u16)> = (0..20)
        .map(|index| {
            let start = 0x2_0000 + index * 0x1000;
            (start, u32::MAX, 9u16)
        })
        .collect();
    // The group the shown glyph actually resolves through, ahead of the
    // hostile ones so "first entry wins" is still exercised.
    groups.push((u32::from('Q'), u32::from('Q'), 3));

    let started = std::time::Instant::now();
    let candidate = extract_pdf_candidate(&composite_document_with_program(
        &[b"BT /F1 12 Tf 72 700 Td <0003> Tj ET"],
        "Identity-H",
        truetype_font_with_cmap_groups(&groups, 12),
    ));
    let elapsed = started.elapsed();

    assert_eq!(candidate.coverage, ExtractionCoverage::IndexedComplete);
    assert_eq!(candidate.sections[0].content, "Q");
    // Two orders of magnitude of headroom over the measured cost and eight over
    // the old one, so this fails on the defect and not on a slow machine.
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "glyph-map inversion took {elapsed:?}"
    );
}

/// A `cmap` may map a glyph to a C0 control. `char::from_u32` accepts every one
/// of them, so an `Identity-H` subset with no `/ToUnicode` put a literal
/// `U+0000` into indexable section content — routing around the rule
/// `formats::decode_utf8` applies to every other format, which refuses any file
/// containing a NUL byte outright.
#[test]
#[cfg(feature = "native-pdf-extractor")]
fn a_glyph_that_maps_to_a_control_character_contributes_no_text() {
    let candidate = extract_pdf_candidate(&composite_document_with_program(
        // Glyph 3 maps to NUL, 4 to TAB, 5 to LINE FEED, 6 to `A`.
        &[b"BT /F1 12 Tf 72 700 Td <0003000400050006> Tj ET"],
        "Identity-H",
        truetype_font_with_cmap_groups(
            &[
                (0, 0, 3),
                (9, 9, 4),
                (10, 10, 5),
                (u32::from('A'), u32::from('A'), 6),
            ],
            8,
        ),
    ));

    assert_eq!(candidate.sections.len(), 1);
    let content = &candidate.sections[0].content;
    assert_eq!(content, "A");
    assert!(
        !content.chars().any(char::is_control),
        "a control character reached section content: {content:?}"
    );
}
