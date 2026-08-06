// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for the admission-disabled PDF reader.
//!
//! Three groups, in this order:
//!
//! 1. **Admission invariants.** The feature being on must change nothing about
//!    what the product will index. These run first because every other test in
//!    this file would still pass if PDF had been quietly admitted.
//! 2. **Geometry.** Positions are asserted as arithmetic that can be checked by
//!    hand from the fixture's content stream, not by re-running the code under
//!    test and recording what it said.
//! 3. **Budgets and hostile input.** Each budget gets a `limit` / `limit + 1`
//!    pair, so widening a constant fails the suite instead of the field.

use super::test_support::{
    DocumentSpec, PageSpec, build_pdf, helvetica_document, measured_document, stream_object,
};
use super::{PdfReadError, PdfWritingMode, limits, pdf_limits, read_pdf_geometry};

/// Positions are exact rationals in these fixtures, but they arrive through
/// `f32` operands, so comparisons carry a tolerance rather than pretending the
/// arithmetic is integral.
const EPSILON: f64 = 1e-6;

#[track_caller]
fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < EPSILON,
        "expected {expected}, got {actual}"
    );
}

#[track_caller]
fn assert_point(actual: [f64; 2], expected: [f64; 2]) {
    assert_close(actual[0], expected[0]);
    assert_close(actual[1], expected[1]);
}

fn geometry(bytes: &[u8]) -> super::PdfDocumentGeometry {
    read_pdf_geometry(bytes).expect("fixture should read")
}

fn notice_codes(geometry: &super::PdfDocumentGeometry) -> Vec<&str> {
    geometry
        .notices
        .iter()
        .map(|notice| notice.code.as_str())
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Admission invariants
// ---------------------------------------------------------------------------

#[test]
fn pdf_is_not_admitted_while_the_reader_feature_is_on() {
    use crate::extract::ExtractionCoverage;
    use crate::format::SourceFormat;

    assert!(!SourceFormat::Pdf.is_extractable());
    assert!(!SourceFormat::Pdf.spec().extraction_supported);
    // Discovery goes through `from_extractable_path`, so a `.pdf` on disk is
    // still invisible to it.
    assert!(SourceFormat::from_extractable_path("notes.pdf").is_none());

    let extracted = crate::formats::extract_source(
        SourceFormat::Pdf,
        &measured_document(b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hi) Tj ET"),
    )
    .expect("the stub never errors");
    assert_eq!(
        extracted.coverage,
        ExtractionCoverage::SkippedNoExtractableText
    );
    assert!(extracted.sections.is_empty());
    assert_eq!(
        extracted
            .notices
            .iter()
            .map(|notice| notice.code.as_str())
            .collect::<Vec<_>>(),
        vec!["format_not_yet_supported"]
    );
}

#[test]
fn reading_geometry_produces_no_extracted_source() {
    // The reader's return type carries no sections, properties, or coverage, so
    // there is no route from it into an index. Pinning the shape keeps a later
    // refactor from quietly growing one.
    let geometry = geometry(&measured_document(b"BT /F1 12 Tf 1 0 0 1 0 0 Tm (a) Tj ET"));
    assert_eq!(geometry.page_count, 1);
    assert_eq!(geometry.pages[0].runs.len(), 1);
}

// ---------------------------------------------------------------------------
// 2. Geometry
// ---------------------------------------------------------------------------

#[test]
fn a_show_operator_lands_at_its_text_matrix_origin_in_device_space() {
    let geometry = geometry(&measured_document(
        b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hi) Tj ET",
    ));
    let page = &geometry.pages[0];
    assert_close(page.width, 612.0);
    assert_close(page.height, 792.0);
    assert_eq!(page.rotate, 0);

    let run = &page.runs[0];
    assert_eq!(run.text, "Hi");
    assert_eq!(run.page_number, 1);
    assert_eq!(run.text_object_index, 1);
    assert_eq!(run.run_index, 0);
    assert_eq!(run.glyph_count, 2);
    assert_eq!(run.writing_mode, PdfWritingMode::Horizontal);
    assert!(run.geometry_exact);
    // User (72, 700) on a 792pt-tall page is 92pt from the top.
    assert_point(run.origin, [72.0, 92.0]);
    // Two glyphs at 500/1000 em and 12pt: 12pt of advance.
    assert_point(run.end, [84.0, 92.0]);
    assert_close(run.font_size, 12.0);
}

#[test]
fn td_accumulates_onto_the_line_matrix_and_t_star_uses_leading() {
    // `Td` is relative to the *line* matrix, not absolute, and `T*` is
    // `Td(0, -TL)`. No file in the fidelity corpus contains `Td` at all —
    // ReportLab and WeasyPrint cannot emit it — so this is the shape that a
    // corpus-only check would have left unverified.
    let geometry = geometry(&measured_document(
        b"BT /F1 10 Tf 12 TL 50 700 Td (a) Tj T* (b) Tj 10 -12 Td (c) Tj ET",
    ));
    let runs = &geometry.pages[0].runs;
    assert_eq!(runs.len(), 3);
    assert_point(runs[0].origin, [50.0, 92.0]);
    assert_point(runs[1].origin, [50.0, 104.0]);
    assert_point(runs[2].origin, [60.0, 116.0]);
}

#[test]
fn td_uppercase_sets_leading_as_a_side_effect() {
    let geometry = geometry(&measured_document(
        b"BT /F1 10 Tf 50 700 Td 0 -20 TD (a) Tj T* (b) Tj ET",
    ));
    let runs = &geometry.pages[0].runs;
    assert_point(runs[0].origin, [50.0, 112.0]);
    assert_point(runs[1].origin, [50.0, 132.0]);
}

#[test]
fn quote_operators_advance_a_line_and_set_spacing() {
    let geometry = geometry(&measured_document(
        b"BT /F1 10 Tf 14 TL 50 700 Td (a) Tj (b) ' 3 1 (c) \" ET",
    ));
    let runs = &geometry.pages[0].runs;
    assert_eq!(runs.len(), 3);
    assert_point(runs[0].origin, [50.0, 92.0]);
    // `'` is `T* Tj`.
    assert_eq!(runs[1].text, "b");
    assert_point(runs[1].origin, [50.0, 106.0]);
    // `"` sets word spacing then char spacing, then behaves as `'`.
    assert_eq!(runs[2].text, "c");
    assert_close(runs[2].word_spacing, 3.0);
    assert_close(runs[2].char_spacing, 1.0);
    assert_point(runs[2].origin, [50.0, 120.0]);
}

#[test]
fn tj_array_numbers_break_runs_and_shift_the_text_matrix() {
    let geometry = geometry(&measured_document(
        b"BT /F1 10 Tf 1 0 0 1 0 700 Tm [(ab) -1000 (cd)] TJ ET",
    ));
    let runs = &geometry.pages[0].runs;
    assert_eq!(runs.len(), 2, "a TJ adjustment starts a new run");
    assert_eq!(runs[0].text, "ab");
    assert_point(runs[0].origin, [0.0, 92.0]);
    // Two glyphs at 500/1000 em and 10pt.
    assert_point(runs[0].end, [10.0, 92.0]);
    // -1000 thousandths at 10pt is +10pt of extra advance.
    assert_eq!(runs[1].text, "cd");
    assert_point(runs[1].origin, [20.0, 92.0]);
}

#[test]
fn char_and_word_spacing_enter_the_advance() {
    let geometry = geometry(&measured_document(
        b"BT /F1 10 Tf 2 Tc 5 Tw 1 0 0 1 0 700 Tm (a b) Tj ET",
    ));
    let run = &geometry.pages[0].runs[0];
    // 3 glyphs x (0.5 x 10 + 2 Tc) = 21, plus 5 Tw on the single space.
    assert_point(run.end, [26.0, 92.0]);
}

#[test]
fn horizontal_scaling_scales_the_advance_but_not_the_font_size() {
    let geometry = geometry(&measured_document(
        b"BT /F1 10 Tf 50 Tz 1 0 0 1 0 700 Tm (ab) Tj ET",
    ));
    let run = &geometry.pages[0].runs[0];
    assert_close(run.horizontal_scale, 0.5);
    assert_point(run.end, [5.0, 92.0]);
    assert_close(run.font_size, 10.0);
}

#[test]
fn rise_offsets_the_origin_without_leaving_the_baseline_advance() {
    let geometry = geometry(&measured_document(
        b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (a) Tj 4 Ts (1) Tj ET",
    ));
    let runs = &geometry.pages[0].runs;
    assert_close(runs[1].rise, 4.0);
    // The raised run is 4pt higher, i.e. 4pt smaller in device y.
    assert_point(runs[1].origin, [5.0, 88.0]);
}

#[test]
fn a_cm_flip_composes_with_a_flipped_text_matrix() {
    // The shape WeasyPrint emits: a page-level y-flip in `cm`, cancelled by a
    // flipped `Tm`. An implementation that ignores `cm` reads these pages in
    // inverted vertical order, which is the defect this asserts against.
    let geometry = geometry(&measured_document(
        b"1 0 0 -1 0 792 cm BT /F1 10 Tf 1 0 0 -1 72 78 Tm (Top) Tj ET",
    ));
    let run = &geometry.pages[0].runs[0];
    assert_point(run.origin, [72.0, 78.0]);
    assert_close(run.font_size, 10.0);
    assert_point(run.end, [87.0, 78.0]);
}

#[test]
fn cm_scales_the_reported_font_size() {
    let geometry = geometry(&measured_document(
        b"0.75 0 0 0.75 0 0 cm BT /F1 20 Tf 1 0 0 1 0 792 Tm (a) Tj ET",
    ));
    let run = &geometry.pages[0].runs[0];
    assert_close(run.font_size, 15.0);
    // User (0, 792) scaled by 0.75 is (0, 594); device y = 792 - 594.
    assert_point(run.origin, [0.0, 198.0]);
}

#[test]
fn text_state_survives_bt_but_the_text_matrix_does_not() {
    // `TL` is set in one BT/ET block and used in a later one — the shape every
    // ReportLab file in the corpus emits — while `Tm` resets at each `BT`.
    let geometry = geometry(&measured_document(
        b"BT /F1 10 Tf 20 TL ET BT 1 0 0 1 30 700 Tm (a) Tj T* (b) Tj ET",
    ));
    let runs = &geometry.pages[0].runs;
    assert_point(runs[0].origin, [30.0, 92.0]);
    assert_point(runs[1].origin, [30.0, 112.0]);
}

#[test]
fn q_and_q_scope_text_state_but_not_the_text_matrix() {
    let geometry = geometry(&measured_document(
        b"q 7 Tc Q BT /F1 10 Tf 1 0 0 1 0 700 Tm (ab) Tj ET",
    ));
    let run = &geometry.pages[0].runs[0];
    assert_close(run.char_spacing, 0.0);
    assert_point(run.end, [10.0, 92.0]);
}

#[test]
fn q_restores_the_ctm() {
    let geometry = geometry(&measured_document(
        b"q 2 0 0 2 0 0 cm BT /F1 10 Tf 1 0 0 1 0 300 Tm (a) Tj ET Q \
          BT /F1 10 Tf 1 0 0 1 0 300 Tm (b) Tj ET",
    ));
    let runs = &geometry.pages[0].runs;
    assert_point(runs[0].origin, [0.0, 192.0]);
    assert_point(runs[1].origin, [0.0, 492.0]);
}

#[test]
fn page_rotation_is_applied_to_device_space() {
    let content: &[u8] = b"BT /F1 10 Tf 1 0 0 1 100 200 Tm (a) Tj ET";
    let font_id = DocumentSpec::first_font_id(1);
    let font = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>";

    let rotated = |degrees: i64| {
        DocumentSpec {
            pages: vec![PageSpec {
                content,
                media_box: "[0 0 612 792]",
                extra_page_entries: Box::leak(format!("/Rotate {degrees}").into_boxed_str()),
            }],
            font_objects: vec![font.to_vec()],
            font_resources: format!("/F1 {font_id} 0 R"),
            extra_trailer: String::new(),
        }
        .build()
    };

    let zero = geometry(&rotated(0));
    assert_close(zero.pages[0].width, 612.0);
    assert_close(zero.pages[0].height, 792.0);
    assert_point(zero.pages[0].runs[0].origin, [100.0, 592.0]);

    let ninety = geometry(&rotated(90));
    assert_eq!(ninety.pages[0].rotate, 90);
    assert_close(ninety.pages[0].width, 792.0);
    assert_close(ninety.pages[0].height, 612.0);
    assert_point(ninety.pages[0].runs[0].origin, [200.0, 100.0]);

    let one_eighty = geometry(&rotated(180));
    assert_point(one_eighty.pages[0].runs[0].origin, [512.0, 200.0]);

    let two_seventy = geometry(&rotated(270));
    assert_close(two_seventy.pages[0].width, 792.0);
    assert_point(two_seventy.pages[0].runs[0].origin, [592.0, 512.0]);

    // Negative and over-turn rotations normalize rather than being refused.
    let negative = geometry(&rotated(-90));
    assert_eq!(negative.pages[0].rotate, 270);
    let over = geometry(&rotated(450));
    assert_eq!(over.pages[0].rotate, 90);
}

#[test]
fn a_rotation_that_is_not_a_multiple_of_ninety_is_reported_and_ignored() {
    let font_id = DocumentSpec::first_font_id(1);
    let bytes = DocumentSpec {
        pages: vec![PageSpec {
            content: b"BT /F1 10 Tf 1 0 0 1 100 200 Tm (a) Tj ET",
            media_box: "[0 0 612 792]",
            extra_page_entries: "/Rotate 45",
        }],
        font_objects: vec![
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_vec(),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build();

    let geometry = geometry(&bytes);
    assert_eq!(geometry.pages[0].rotate, 0);
    assert!(notice_codes(&geometry).contains(&"pdf_page_rotation_invalid"));
    assert_point(geometry.pages[0].runs[0].origin, [100.0, 592.0]);
}

#[test]
fn the_crop_box_wins_over_the_media_box_and_moves_the_origin() {
    let font_id = DocumentSpec::first_font_id(1);
    let bytes = DocumentSpec {
        pages: vec![PageSpec {
            content: b"BT /F1 10 Tf 1 0 0 1 100 700 Tm (a) Tj ET",
            media_box: "[0 0 612 792]",
            extra_page_entries: "/CropBox [50 50 550 750]",
        }],
        font_objects: vec![
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_vec(),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build();

    let geometry = geometry(&bytes);
    assert_close(geometry.pages[0].width, 500.0);
    assert_close(geometry.pages[0].height, 700.0);
    assert_point(geometry.pages[0].runs[0].origin, [50.0, 50.0]);
}

#[test]
fn a_page_box_is_inherited_from_the_page_tree() {
    // /MediaBox on /Pages, absent on /Page.
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] /MediaBox [0 0 200 400] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            .to_vec(),
        stream_object("", b"BT /F1 10 Tf 1 0 0 1 10 300 Tm (a) Tj ET"),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    ];
    let geometry = geometry(&build_pdf(&objects, 1, ""));
    assert_close(geometry.pages[0].width, 200.0);
    assert_close(geometry.pages[0].height, 400.0);
    assert_point(geometry.pages[0].runs[0].origin, [10.0, 100.0]);
}

#[test]
fn a_font_without_widths_reports_inexact_geometry() {
    // Six of the eleven fidelity-corpus files use base-14 Type1 fonts with no
    // /Widths, no /FirstChar, and no /FontDescriptor. The advance is therefore
    // unknown, and saying so is the point: a segmentation step that trusted
    // `end` here would be measuring nothing.
    let geometry = geometry(&helvetica_document(&[
        b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (abc) Tj ET",
    ]));
    let run = &geometry.pages[0].runs[0];
    assert_eq!(run.text, "abc");
    assert!(!run.geometry_exact);
    assert_point(run.end, run.origin);
}

#[test]
fn missing_width_is_used_when_widths_are_absent() {
    let font_id = DocumentSpec::first_font_id(1);
    let bytes = DocumentSpec {
        pages: vec![PageSpec::new(b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (ab) Tj ET")],
        font_objects: vec![
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /Nowidths /Encoding /WinAnsiEncoding \
                 /FontDescriptor {} 0 R >>",
                font_id + 1
            )
            .into_bytes(),
            b"<< /Type /FontDescriptor /FontName /Nowidths /MissingWidth 600 /Flags 32 >>".to_vec(),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build();

    let run = &geometry(&bytes).pages[0].runs[0];
    assert!(!run.geometry_exact);
    assert_point(run.end, [12.0, 92.0]);
}

#[test]
fn a_code_outside_the_widths_array_falls_back_and_clears_exactness() {
    let font_id = DocumentSpec::first_font_id(1);
    let bytes = DocumentSpec {
        // 'a' (0x61) is inside 97..=98, 'z' is not.
        pages: vec![PageSpec::new(
            b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (a) Tj (z) Tj ET",
        )],
        font_objects: vec![
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Narrow /Encoding /WinAnsiEncoding \
              /FirstChar 97 /LastChar 98 /Widths [400 400] >>"
                .to_vec(),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build();

    let runs = &geometry(&bytes).pages[0].runs;
    assert!(runs[0].geometry_exact);
    assert_point(runs[0].end, [4.0, 92.0]);
    assert!(!runs[1].geometry_exact);
}

#[test]
fn a_composite_font_segments_two_byte_codes_and_reads_the_w_array() {
    let font_id = DocumentSpec::first_font_id(1);
    let bytes = DocumentSpec {
        // <00410042> is two Identity-H codes, 0x41 and 0x42.
        pages: vec![PageSpec::new(
            b"BT /F1 10 Tf 1 0 0 1 0 700 Tm <00410042> Tj ET",
        )],
        font_objects: vec![
            format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /Sub /Encoding /Identity-H \
                 /DescendantFonts [{} 0 R] /ToUnicode {} 0 R >>",
                font_id + 1,
                font_id + 2
            )
            .into_bytes(),
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Sub /DW 1000 \
              /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
              /W [65 [700 800]] >>"
                .to_vec(),
            stream_object("", TO_UNICODE_AB),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build();

    let run = &geometry(&bytes).pages[0].runs[0];
    assert_eq!(run.text, "AB");
    assert_eq!(
        run.glyph_count, 2,
        "two-byte codes, not four one-byte codes"
    );
    assert!(run.geometry_exact);
    // 700 + 800 thousandths at 10pt.
    assert_point(run.end, [15.0, 92.0]);
}

/// A minimal `/ToUnicode` CMap mapping CIDs 0x0041/0x0042 to `A`/`B`.
const TO_UNICODE_AB: &[u8] = b"/CIDInit /ProcSet findresource begin\n\
12 dict begin\nbegincmap\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
2 beginbfchar\n<0041> <0041>\n<0042> <0042>\nendbfchar\nendcmap\n\
CMapName currentdict /CMap defineresource pop\nend\nend";

#[test]
fn word_spacing_never_applies_to_a_two_byte_code() {
    // PDF 1.7 §9.3.3: `Tw` applies to the single-byte code 32 only. A CID that
    // happens to equal 32 must not pick it up.
    let font_id = DocumentSpec::first_font_id(1);
    let bytes = DocumentSpec {
        pages: vec![PageSpec::new(
            b"BT /F1 10 Tf 100 Tw 1 0 0 1 0 700 Tm <0020> Tj ET",
        )],
        font_objects: vec![
            format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /Sub /Encoding /Identity-H \
                 /DescendantFonts [{} 0 R] >>",
                font_id + 1
            )
            .into_bytes(),
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Sub /DW 500 \
              /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>"
                .to_vec(),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build();

    let run = &geometry(&bytes).pages[0].runs[0];
    assert_point(run.end, [5.0, 92.0]);
}

#[test]
fn vertical_writing_is_reported_rather_than_guessed_at() {
    let font_id = DocumentSpec::first_font_id(1);
    let bytes = DocumentSpec {
        pages: vec![PageSpec::new(
            b"BT /F1 10 Tf 1 0 0 1 0 700 Tm <00410042> Tj ET",
        )],
        font_objects: vec![
            format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /Sub /Encoding /Identity-V \
                 /DescendantFonts [{} 0 R] /ToUnicode {} 0 R >>",
                font_id + 1,
                font_id + 2
            )
            .into_bytes(),
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Sub /DW 1000 \
              /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>"
                .to_vec(),
            stream_object("", TO_UNICODE_AB),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build();

    let geometry = geometry(&bytes);
    let run = &geometry.pages[0].runs[0];
    assert_eq!(run.text, "AB", "the glyphs are still decoded");
    assert_eq!(run.writing_mode, PdfWritingMode::Vertical);
    assert!(!run.geometry_exact);
    assert!(geometry.truncated);
    assert!(notice_codes(&geometry).contains(&"pdf_vertical_writing_unsupported"));
}

#[test]
fn render_mode_is_reported_and_never_filtered() {
    // Mode 3 is invisible text, the shape an OCR-under-image PDF uses. Whether
    // it belongs in an index is a product decision; dropping it here would make
    // that decision silently.
    let geometry = geometry(&measured_document(
        b"BT /F1 10 Tf 3 Tr 1 0 0 1 0 700 Tm (hidden) Tj ET",
    ));
    let run = &geometry.pages[0].runs[0];
    assert_eq!(run.render_mode, 3);
    assert_eq!(run.text, "hidden");
}

#[test]
fn pages_are_enumerated_in_order_with_a_shared_page_number() {
    let geometry = geometry(&helvetica_document(&[
        b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (one) Tj ET",
        b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (two) Tj ET",
    ]));
    assert_eq!(geometry.page_count, 2);
    let texts: Vec<_> = geometry.runs().map(|run| run.text.as_str()).collect();
    assert_eq!(texts, vec!["one", "two"]);
    let numbers: Vec<_> = geometry.runs().map(|run| run.page_number).collect();
    assert_eq!(numbers, vec![1, 2]);
}

// ---------------------------------------------------------------------------
// 3. Budgets
// ---------------------------------------------------------------------------

#[test]
fn the_reported_limits_match_the_constants_the_reader_enforces() {
    let reported = pdf_limits();
    assert_eq!(reported.max_input_bytes, limits::MAX_INPUT_BYTES);
    assert_eq!(
        reported.max_decompressed_stream_bytes,
        limits::MAX_DECOMPRESSED_STREAM_BYTES
    );
    assert_eq!(reported.max_pages, limits::MAX_PAGES);
    assert_eq!(
        reported.max_content_stream_bytes,
        limits::MAX_CONTENT_STREAM_BYTES
    );
    assert_eq!(
        reported.max_total_content_stream_bytes,
        limits::MAX_TOTAL_CONTENT_STREAM_BYTES
    );
    assert_eq!(
        reported.max_object_nesting_depth,
        limits::MAX_OBJECT_NESTING_DEPTH
    );
    assert_eq!(
        reported.max_operations_per_page,
        limits::MAX_OPERATIONS_PER_PAGE
    );
    assert_eq!(reported.max_runs_per_page, limits::MAX_RUNS_PER_PAGE);
    assert_eq!(reported.max_glyphs_per_page, limits::MAX_GLYPHS_PER_PAGE);
    assert_eq!(
        reported.max_graphics_stack_depth,
        limits::MAX_GRAPHICS_STACK_DEPTH
    );
    assert_eq!(reported.max_fonts_per_page, limits::MAX_FONTS_PER_PAGE);
    assert_eq!(reported.max_tounicode_bytes, limits::MAX_TOUNICODE_BYTES);
    assert_eq!(
        reported.max_extracted_text_bytes,
        limits::MAX_EXTRACTED_TEXT_BYTES
    );
    assert_eq!(reported.max_notices, limits::MAX_NOTICES);
}

#[test]
fn input_bytes_boundary() {
    // At the limit the size gate passes and the file is judged on its merits;
    // one byte past it, nothing is parsed at all.
    let mut at_limit = b"%PDF-1.7\n".to_vec();
    at_limit.resize(limits::MAX_INPUT_BYTES, b'x');
    assert_eq!(at_limit.len(), limits::MAX_INPUT_BYTES);
    assert_eq!(
        read_pdf_geometry(&at_limit).unwrap_err(),
        PdfReadError::InvalidDocument
    );

    let mut over_limit = at_limit;
    over_limit.push(b'x');
    assert_eq!(
        read_pdf_geometry(&over_limit).unwrap_err(),
        PdfReadError::InputTooLarge
    );
}

fn nested_array_document(depth: usize) -> Vec<u8> {
    let mut nested = Vec::new();
    nested.extend(std::iter::repeat_n(b'[', depth));
    nested.extend(std::iter::repeat_n(b']', depth));
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_vec(),
        stream_object("", b""),
        nested,
    ];
    build_pdf(&objects, 1, "")
}

#[test]
fn the_pre_screen_measures_nesting_without_counting_stream_payloads() {
    use super::prescreen::prescreen;

    // A stream payload full of `<<` and `[` bytes must not register as
    // structure — compressed data routinely contains them, and counting them
    // would refuse ordinary files.
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_vec(),
        stream_object("", &vec![b'['; 4096]),
    ];
    let measured = prescreen(&build_pdf(&objects, 1, "")).expect("well-formed");
    assert!(
        measured.max_nesting_depth <= 3,
        "measured {}",
        measured.max_nesting_depth
    );

    // A literal string is skipped too, brackets and all.
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".to_vec(),
        b"([[[[[[[[ \\) not a close )".to_vec(),
    ];
    let measured = prescreen(&build_pdf(&objects, 1, "")).expect("well-formed");
    assert!(measured.max_nesting_depth <= 3);
}

#[test]
fn object_nesting_depth_boundary() {
    let at_limit = nested_array_document(limits::MAX_OBJECT_NESTING_DEPTH);
    assert_eq!(geometry(&at_limit).page_count, 1);

    let over_limit = nested_array_document(limits::MAX_OBJECT_NESTING_DEPTH + 1);
    assert_eq!(
        read_pdf_geometry(&over_limit).unwrap_err(),
        PdfReadError::ObjectNestingTooDeep
    );
}

fn many_page_document(pages: usize) -> Vec<u8> {
    let contents: Vec<&[u8]> = vec![b"BT /F1 10 Tf 1 0 0 1 0 100 Tm (a) Tj ET"; pages];
    helvetica_document(&contents)
}

#[test]
fn page_count_boundary() {
    let at_limit = many_page_document(limits::MAX_PAGES);
    assert_eq!(geometry(&at_limit).page_count, limits::MAX_PAGES);

    let over_limit = many_page_document(limits::MAX_PAGES + 1);
    assert_eq!(
        read_pdf_geometry(&over_limit).unwrap_err(),
        PdfReadError::PageLimitExceeded
    );
}

fn stacked_document(depth: usize) -> Vec<u8> {
    let mut content = Vec::new();
    for _ in 0..depth {
        content.extend_from_slice(b"q ");
    }
    content.extend_from_slice(b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (a) Tj ET ");
    for _ in 0..depth {
        content.extend_from_slice(b"Q ");
    }
    measured_document(&content)
}

#[test]
fn graphics_stack_depth_boundary() {
    let at_limit = geometry(&stacked_document(limits::MAX_GRAPHICS_STACK_DEPTH));
    assert!(!notice_codes(&at_limit).contains(&"pdf_graphics_stack_limit"));
    assert!(!notice_codes(&at_limit).contains(&"pdf_graphics_stack_underflow"));

    let over_limit = geometry(&stacked_document(limits::MAX_GRAPHICS_STACK_DEPTH + 1));
    let codes = notice_codes(&over_limit);
    assert!(codes.contains(&"pdf_graphics_stack_limit"));
    // The extra `Q` finds nothing to pop, which is the honest consequence of
    // refusing the push rather than silently growing the stack.
    assert!(codes.contains(&"pdf_graphics_stack_underflow"));
    // Text still comes out.
    assert_eq!(over_limit.pages[0].runs[0].text, "a");
}

fn many_font_document(fonts: usize) -> Vec<u8> {
    let first_font_id = DocumentSpec::first_font_id(1);
    let mut resources = String::new();
    let mut objects = Vec::new();
    for index in 0..fonts {
        resources.push_str(&format!("/F{index} {} 0 R ", first_font_id + index));
        objects.push(
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding \
                 /FirstChar 97 /LastChar 97 /Widths [{}] >>",
                500 + index
            )
            .into_bytes(),
        );
    }
    // `/F0` sorts first in the BTreeMap `get_page_fonts` returns, so it is the
    // one kept when the tail is dropped; the run below therefore stays readable
    // in both the at-limit and over-limit cases and only the notice differs.
    DocumentSpec {
        pages: vec![PageSpec::new(b"BT /F0 10 Tf 1 0 0 1 0 700 Tm (a) Tj ET")],
        font_objects: objects,
        font_resources: resources,
        extra_trailer: String::new(),
    }
    .build()
}

#[test]
fn fonts_per_page_boundary() {
    let at_limit = geometry(&many_font_document(limits::MAX_FONTS_PER_PAGE));
    assert!(!notice_codes(&at_limit).contains(&"pdf_page_font_limit_exceeded"));
    assert!(!at_limit.truncated);

    let over_limit = geometry(&many_font_document(limits::MAX_FONTS_PER_PAGE + 1));
    assert!(notice_codes(&over_limit).contains(&"pdf_page_font_limit_exceeded"));
    assert!(over_limit.truncated);
}

fn padded_content_document(content_len: usize) -> Vec<u8> {
    let mut content = b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (a) Tj ET".to_vec();
    assert!(content.len() <= content_len);
    content.resize(content_len, b' ');
    measured_document(&content)
}

#[test]
fn per_page_content_stream_boundary() {
    let at_limit = geometry(&padded_content_document(limits::MAX_CONTENT_STREAM_BYTES));
    assert!(!at_limit.truncated);
    assert_eq!(at_limit.pages[0].runs[0].text, "a");

    let over_limit = geometry(&padded_content_document(
        limits::MAX_CONTENT_STREAM_BYTES + 1,
    ));
    assert!(over_limit.truncated);
    assert!(over_limit.pages[0].runs.is_empty());
    assert!(over_limit.pages[0].truncated);
    assert!(notice_codes(&over_limit).contains(&"pdf_page_content_limit_exceeded"));
}

fn many_glyph_document(glyphs: usize) -> Vec<u8> {
    let mut content = b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (".to_vec();
    content.extend(std::iter::repeat_n(b'a', glyphs));
    content.extend_from_slice(b") Tj ET");
    measured_document(&content)
}

#[test]
fn glyphs_per_page_boundary() {
    let at_limit = geometry(&many_glyph_document(limits::MAX_GLYPHS_PER_PAGE));
    assert!(!at_limit.truncated);
    assert_eq!(
        at_limit.pages[0].runs[0].glyph_count,
        limits::MAX_GLYPHS_PER_PAGE
    );

    let over_limit = geometry(&many_glyph_document(limits::MAX_GLYPHS_PER_PAGE + 1));
    assert!(over_limit.truncated);
    assert!(notice_codes(&over_limit).contains(&"pdf_page_glyph_limit_exceeded"));
    assert_eq!(
        over_limit.pages[0].runs[0].glyph_count,
        limits::MAX_GLYPHS_PER_PAGE
    );
}

fn many_run_document(runs: usize) -> Vec<u8> {
    let mut content = b"BT /F1 10 Tf 1 0 0 1 0 700 Tm ".to_vec();
    for _ in 0..runs {
        content.extend_from_slice(b"(a) Tj ");
    }
    content.extend_from_slice(b"ET");
    measured_document(&content)
}

#[test]
fn runs_per_page_boundary() {
    let at_limit = geometry(&many_run_document(limits::MAX_RUNS_PER_PAGE));
    assert_eq!(at_limit.pages[0].runs.len(), limits::MAX_RUNS_PER_PAGE);
    assert!(!at_limit.truncated);

    let over_limit = geometry(&many_run_document(limits::MAX_RUNS_PER_PAGE + 1));
    assert_eq!(over_limit.pages[0].runs.len(), limits::MAX_RUNS_PER_PAGE);
    assert!(over_limit.truncated);
    assert!(notice_codes(&over_limit).contains(&"pdf_page_run_limit_exceeded"));
}

fn operation_document(filler_operations: usize) -> Vec<u8> {
    let mut content = b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (a) Tj ".to_vec();
    // `n` is the end-path operator: a no-op to this interpreter, and exactly one
    // operation each, which is what the budget counts.
    for _ in 0..filler_operations {
        content.extend_from_slice(b"n ");
    }
    content.extend_from_slice(b"(b) Tj ET");
    measured_document(&content)
}

#[test]
fn operations_per_page_boundary() {
    // Four operators precede the filler (`BT Tf Tm Tj`) and two follow it
    // (`Tj ET`).
    const FIXED_OPERATIONS: usize = 6;

    let at_limit = geometry(&operation_document(
        limits::MAX_OPERATIONS_PER_PAGE - FIXED_OPERATIONS,
    ));
    assert!(!at_limit.truncated);
    assert_eq!(at_limit.pages[0].runs.len(), 2);

    let over_limit = geometry(&operation_document(
        limits::MAX_OPERATIONS_PER_PAGE - FIXED_OPERATIONS + 1,
    ));
    assert!(over_limit.truncated);
    assert!(notice_codes(&over_limit).contains(&"pdf_page_operation_limit_exceeded"));
}

fn text_volume_document(pages: usize, glyphs_per_page: usize) -> Vec<u8> {
    let mut content = b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (".to_vec();
    content.extend(std::iter::repeat_n(b'a', glyphs_per_page));
    content.extend_from_slice(b") Tj ET");
    let contents: Vec<&[u8]> = vec![content.as_slice(); pages];
    // `measured_document` is single-page; this is the multi-page variant of the
    // same metric font, so decoded text is exactly one byte per glyph.
    let widths = (32..=126).map(|_| "500").collect::<Vec<_>>().join(" ");
    let font_id = DocumentSpec::first_font_id(pages);
    DocumentSpec {
        pages: contents.iter().map(|c| PageSpec::new(c)).collect(),
        font_objects: vec![
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /Fixed500 \
                 /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 126 /Widths [{widths}] >>"
            )
            .into_bytes(),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build()
}

#[test]
fn extracted_text_boundary() {
    // The per-page glyph budget caps a single page below the document text
    // budget, so reaching the text budget takes several pages by construction.
    let per_page = limits::MAX_GLYPHS_PER_PAGE;
    let full_pages = limits::MAX_EXTRACTED_TEXT_BYTES / per_page;
    let remainder = limits::MAX_EXTRACTED_TEXT_BYTES % per_page;

    let under_limit = geometry(&text_volume_document(full_pages, per_page));
    let produced: usize = under_limit.runs().map(|run| run.text.len()).sum();
    assert_eq!(produced, full_pages * per_page);
    assert!(!notice_codes(&under_limit).contains(&"pdf_extracted_text_limit_exceeded"));

    let over_limit = geometry(&text_volume_document(full_pages + 1, per_page));
    let produced: usize = over_limit.runs().map(|run| run.text.len()).sum();
    assert_eq!(
        produced,
        limits::MAX_EXTRACTED_TEXT_BYTES,
        "the budget is a hard ceiling on retained text, not a soft target"
    );
    assert!(over_limit.truncated);
    assert!(notice_codes(&over_limit).contains(&"pdf_extracted_text_limit_exceeded"));
    assert_eq!(
        over_limit.pages[full_pages].runs[0].text.len(),
        remainder,
        "the overflowing page contributes exactly the remaining budget"
    );
}

fn many_large_pages(pages: usize, content_len: usize) -> Vec<u8> {
    let mut content = b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (a) Tj ET".to_vec();
    content.resize(content_len, b' ');
    let contents: Vec<&[u8]> = vec![content.as_slice(); pages];
    helvetica_document(&contents)
}

/// Runs in about ten seconds against a ~160 MiB fixture, which is more than the
/// rest of the suite combined. Excluded from the default run rather than
/// deleted, because a budget nobody exercises is a comment:
///
/// ```text
/// cargo test -p kwiry-core --features internal-pdf-extractor \
///     -- --ignored total_content_stream_boundary
/// ```
#[test]
#[ignore = "builds a ~160 MiB fixture; see the doc comment"]
fn total_content_stream_boundary() {
    // `get_page_content_with_limit` appends one newline per content stream, so a
    // page charges the aggregate budget its stream length plus one. Sizing the
    // pages one byte short makes the arithmetic exact instead of approximately
    // right, which is the only way a boundary test is worth writing.
    let per_page = limits::MAX_CONTENT_STREAM_BYTES - 1;
    let pages = limits::MAX_TOTAL_CONTENT_STREAM_BYTES / limits::MAX_CONTENT_STREAM_BYTES;

    let at_limit = geometry(&many_large_pages(pages, per_page));
    assert!(!at_limit.truncated);
    assert_eq!(at_limit.page_count, pages);

    // One page more: the aggregate budget is spent, so the last page is refused
    // under the document-level code rather than the per-page one.
    let over_limit = geometry(&many_large_pages(pages + 1, per_page));
    assert!(over_limit.truncated);
    assert!(over_limit.pages[pages].runs.is_empty());
    assert!(over_limit.pages[pages].truncated);
    assert!(notice_codes(&over_limit).contains(&"pdf_document_content_limit_exceeded"));
}

// ---------------------------------------------------------------------------
// 3b. Hostile input
// ---------------------------------------------------------------------------

#[test]
fn non_pdf_input_is_refused_without_parsing() {
    assert_eq!(
        read_pdf_geometry(b"").unwrap_err(),
        PdfReadError::NotAPdf,
        "an empty buffer"
    );
    assert_eq!(
        read_pdf_geometry(b"PK\x03\x04rest of a zip").unwrap_err(),
        PdfReadError::NotAPdf,
        "a DOCX, which shares an extension-adjacent user mistake"
    );
    assert_eq!(
        read_pdf_geometry(&[0u8; 4096]).unwrap_err(),
        PdfReadError::NotAPdf,
        "a block of NULs"
    );
    // The header must be near the front, not merely present somewhere.
    let mut buried = vec![b'x'; 4096];
    buried.extend_from_slice(b"%PDF-1.7\n");
    assert_eq!(
        read_pdf_geometry(&buried).unwrap_err(),
        PdfReadError::NotAPdf
    );
}

#[test]
fn a_truncated_or_damaged_file_is_refused_rather_than_half_read() {
    let full = measured_document(b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (a) Tj ET");
    let truncated = &full[..full.len() / 2];
    // Whatever the parser makes of it, it must be an error or an empty read —
    // never a fabricated page of geometry.
    match read_pdf_geometry(truncated) {
        Err(_) => {}
        Ok(geometry) => assert!(geometry.runs().next().is_none()),
    }

    // A startxref pointing into the middle of nowhere.
    let mut damaged = full.clone();
    let offset = damaged
        .windows(9)
        .position(|window| window == b"startxref")
        .expect("fixture has a startxref");
    damaged.splice(offset + 10..offset + 14, *b"9999");
    match read_pdf_geometry(&damaged) {
        Err(_) => {}
        Ok(geometry) => {
            assert!(geometry.page_count <= 1);
        }
    }
}

#[test]
fn an_encrypted_document_is_refused_and_never_decrypted() {
    let font_id = DocumentSpec::first_font_id(1);
    let bytes = DocumentSpec {
        pages: vec![PageSpec::new(
            b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (secret) Tj ET",
        )],
        font_objects: vec![
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_vec(),
            b"<< /Filter /Standard /V 2 /R 3 /Length 128 /P -3904 \
              /O <0102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F20> \
              /U <202122232425262728292A2B2C2D2E2F303132333435363738393A3B3C3D3E3F> >>"
                .to_vec(),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: format!(
            "/Encrypt {} 0 R /ID [<0102030405060708090A0B0C0D0E0F10> \
             <0102030405060708090A0B0C0D0E0F10>] ",
            font_id + 1
        ),
    }
    .build();

    assert_eq!(
        read_pdf_geometry(&bytes).unwrap_err(),
        PdfReadError::EncryptedDocument
    );
}

#[test]
fn a_decompression_bomb_is_bounded_rather_than_inflated() {
    use std::io::Write as _;

    // ~192 MiB of zeros, which compresses to a few hundred kilobytes.
    let payload = vec![0u8; 192 * 1024 * 1024];
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(&payload).expect("compress");
    let compressed = encoder.finish().expect("finish");
    drop(payload);
    assert!(
        compressed.len() < 1024 * 1024,
        "the bomb has to be small to be a bomb"
    );

    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_vec(),
        stream_object("/Filter /FlateDecode", &compressed),
    ];
    let bytes = build_pdf(&objects, 1, "");

    let geometry = geometry(&bytes);
    assert!(geometry.truncated);
    assert!(geometry.pages[0].runs.is_empty());
    assert!(notice_codes(&geometry).contains(&"pdf_page_content_limit_exceeded"));
}

#[test]
fn a_parent_cycle_in_the_page_tree_terminates() {
    // /Page points at /Pages, /Pages points back at /Page. Neither carries a
    // /MediaBox, so the inheritance walk has to give up on its own.
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] /Parent 3 0 R >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>".to_vec(),
        stream_object("", b"BT /F1 10 Tf 1 0 0 1 0 100 Tm (a) Tj ET"),
    ];
    let geometry = geometry(&build_pdf(&objects, 1, ""));
    assert!(geometry.truncated);
    assert!(geometry.pages[0].runs.is_empty());
    assert!(notice_codes(&geometry).contains(&"pdf_page_geometry_invalid"));
}

#[test]
fn a_degenerate_page_box_is_refused_rather_than_divided_by() {
    let font_id = DocumentSpec::first_font_id(1);
    let bytes = DocumentSpec {
        pages: vec![PageSpec {
            content: b"BT /F1 10 Tf 1 0 0 1 0 100 Tm (a) Tj ET",
            media_box: "[10 10 10 400]",
            extra_page_entries: "",
        }],
        font_objects: vec![
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_vec(),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build();

    let geometry = geometry(&bytes);
    assert!(notice_codes(&geometry).contains(&"pdf_page_geometry_invalid"));
    assert!(geometry.pages[0].runs.is_empty());
}

#[test]
fn an_inverted_page_box_is_normalized() {
    let font_id = DocumentSpec::first_font_id(1);
    let bytes = DocumentSpec {
        pages: vec![PageSpec {
            content: b"BT /F1 10 Tf 1 0 0 1 0 300 Tm (a) Tj ET",
            media_box: "[200 400 0 0]",
            extra_page_entries: "",
        }],
        font_objects: vec![
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_vec(),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build();

    let geometry = geometry(&bytes);
    assert_close(geometry.pages[0].width, 200.0);
    assert_close(geometry.pages[0].height, 400.0);
    assert_point(geometry.pages[0].runs[0].origin, [0.0, 100.0]);
}

#[test]
fn a_show_operator_without_a_font_is_skipped_and_reported() {
    let font_id = DocumentSpec::first_font_id(1);
    let bytes = DocumentSpec {
        pages: vec![PageSpec::new(
            b"BT /Missing 10 Tf 1 0 0 1 0 700 Tm (a) Tj ET",
        )],
        font_objects: vec![
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_vec(),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build();

    let geometry = geometry(&bytes);
    assert!(geometry.pages[0].runs.is_empty());
    assert!(geometry.truncated);
    assert!(notice_codes(&geometry).contains(&"pdf_font_unresolved"));
}

#[test]
fn a_nested_text_object_is_reported_and_treated_as_a_reset() {
    let geometry = geometry(&measured_document(
        b"BT 1 0 0 1 30 700 Tm BT 1 0 0 1 60 600 Tm (a) Tj ET ET",
    ));
    assert!(notice_codes(&geometry).contains(&"pdf_nested_text_object"));
    // No `Tf` ran, so no run is emitted; the point is that neither BT panics
    // nor the matrix state leaks.
    assert!(geometry.pages[0].runs.is_empty());
}

#[test]
fn an_unbalanced_q_is_tolerated() {
    // 122 `q` against 121 `Q` is what WeasyPrint emits; a reader that treated
    // the imbalance as fatal would refuse two of the eleven corpus files.
    let geometry = geometry(&measured_document(
        b"q q BT /F1 10 Tf 1 0 0 1 0 700 Tm (a) Tj ET Q",
    ));
    assert_eq!(geometry.pages[0].runs[0].text, "a");
    assert!(!notice_codes(&geometry).contains(&"pdf_graphics_stack_underflow"));
}

#[test]
fn a_non_finite_matrix_is_refused_rather_than_propagated() {
    // PDF numbers have no exponent syntax, so the fixture spells the scale out.
    // Twenty compositions of 1e18 put the CTM past f64's range, which is the
    // point: the overflow has to be caught where it happens rather than carried
    // into a run as an infinite or NaN coordinate.
    let mut content = Vec::new();
    for _ in 0..20 {
        content.extend_from_slice(b"1000000000000000000.0 0 0 1000000000000000000.0 0 0 cm ");
    }
    content.extend_from_slice(b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (a) Tj ET");
    let geometry = geometry(&measured_document(&content));
    assert!(notice_codes(&geometry).contains(&"pdf_non_invertible_matrix"));
}

#[test]
fn a_page_without_content_yields_an_empty_page_not_a_failure() {
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".to_vec(),
    ];
    let geometry = geometry(&build_pdf(&objects, 1, ""));
    assert_eq!(geometry.page_count, 1);
    assert!(geometry.pages[0].runs.is_empty());
    assert!(!geometry.truncated);
}

#[test]
fn notices_are_deduplicated_and_bounded() {
    // One repeated condition must not consume the notice budget.
    let mut content = Vec::new();
    for _ in 0..(limits::MAX_NOTICES * 4) {
        content.extend_from_slice(b"Q ");
    }
    content.extend_from_slice(b"BT /F1 10 Tf 1 0 0 1 0 700 Tm (a) Tj ET");
    let geometry = geometry(&measured_document(&content));
    assert_eq!(
        geometry
            .notices
            .iter()
            .filter(|notice| notice.code == "pdf_graphics_stack_underflow")
            .count(),
        1
    );
    assert!(geometry.notices.len() <= limits::MAX_NOTICES);
}

// ---------------------------------------------------------------------------
// 4. Regressions
//
// One test per accepted finding from the adversarial review. The budgets get a
// `limit` / `limit + 1` pair like every other budget here; the windowed decode
// gets a differential test instead, because its correctness claim is an
// equivalence and not a threshold.
// ---------------------------------------------------------------------------

/// Content streams covering every token the splitter has to get right. Each one
/// is decoded whole and in windows and the two operation sequences must match.
const SPLIT_CORPUS: &[&[u8]] = &[
    b"",
    b"   \n\t\r  ",
    b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hi) Tj ET",
    // Literal strings: nested parentheses, escaped delimiters, octal escapes,
    // a line-continuation backslash, and an operator's name inside a string.
    b"BT (a(b)c) Tj (esc\\) not close) Tj (\\101\\102) Tj (line\\\r\nwrap) Tj (ET Tj BT) Tj ET",
    // Hex strings, and the `<<` versus `<` disambiguation next to each other.
    b"BT <414243> Tj <41 42 43> Tj ET /Name << /Sub << /Deep [1 2 3] >> >> Do",
    // Arrays, including the TJ shape and a nested one.
    b"BT [(a) -40 (b) 12.5 (c)] TJ [[1 2] [3 4]] X ET",
    // Names with `#` escapes and a bare trailing `/`.
    b"/A#20B gs / gs /Plain gs",
    // Numbers in every accepted spelling.
    b"1 -2 +3 .5 -.5 1. 0.0 6 x",
    // Comments between operations, inside an array, and trailing.
    b"%header\nBT (a) Tj %inline\n ET\n%trailer\n",
    // `true`, `false`, `null`, and a reference inside an array.
    b"[true false null 1 0 R] X",
    // The quote operators, whose operands include a string.
    b"BT 1 2 (spaced) \" (next) ' ET",
    // Dangling operands with no operator: `many0` stops, and so must the
    // splitter — in the same place.
    b"BT (a) Tj ET 5 5",
    // An operator run with no operands at all, repeated: the shape of the
    // compressed operator bomb.
    b"q Q q Q q Q q Q q Q q Q q Q q Q ",
    // An inline image, including one whose data contains bytes that look like
    // operators.
    b"q BI /W 2 /H 2 /BPC 8 /CS /G ID \x00\x01(BT)\\ EI Q BT (after) Tj ET",
];

/// Windowing exists to keep peak allocation off the decompressed stream length;
/// it is only allowed to do that if it changes nothing about what is
/// interpreted. Window sizes down to one byte force a boundary decision at
/// every operation, so a splitter that disagreed with `lopdf` about where an
/// operation ends fails here rather than in the field.
#[test]
fn windowed_decoding_reproduces_the_single_shot_operation_sequence() {
    use lopdf::content::Content;

    fn shape(content: &[u8]) -> Vec<(String, usize)> {
        Content::decode(content).map_or_else(
            |_| Vec::new(),
            |decoded| {
                decoded
                    .operations
                    .iter()
                    .map(|operation| (operation.operator.clone(), operation.operands.len()))
                    .collect()
            },
        )
    }

    for source in SPLIT_CORPUS {
        let expected = shape(source);
        for budget in [1usize, 2, 7, 64, 4096] {
            let mut actual = Vec::new();
            let mut start = 0usize;
            loop {
                if start >= source.len() {
                    break;
                }
                let window = super::split::window(source, start, budget);
                let end = window.end();
                if end <= start {
                    break;
                }
                actual.extend(shape(&source[start..end]));
                if matches!(
                    window,
                    super::split::Window::End { .. } | super::split::Window::Unparsable { .. }
                ) {
                    break;
                }
                start = end;
            }
            assert_eq!(
                actual,
                expected,
                "window {budget} diverged on {:?}",
                String::from_utf8_lossy(source)
            );
        }
    }
}

/// A page of `q Q ` filling the per-page content budget drove 9.5 GB of peak
/// RSS from a 33 KB file, with `pdf_page_operation_limit_exceeded` firing on
/// schedule the whole time: the limit bounded interpretation, and
/// `Content::decode` had already materialized the entire page.
///
/// Peak allocation is not observable from inside the process, so the property
/// asserted here is the one that produces it — no window handed to the decoder
/// exceeds the budget by more than the single operation that straddles it.
#[test]
fn no_decode_window_exceeds_the_window_budget_by_more_than_one_operation() {
    let content = b"q Q ".repeat(200_000);
    let budget = limits::MAX_CONTENT_WINDOW_BYTES;
    let mut start = 0usize;
    let mut windows = 0usize;
    loop {
        if start >= content.len() {
            break;
        }
        let window = super::split::window(&content, start, budget);
        let end = window.end();
        assert!(end > start, "splitter made no progress at {start}");
        // `q Q ` is four bytes, so the straddling operation adds at most two.
        assert!(
            end - start <= budget + 4,
            "window of {} bytes exceeds the {budget}-byte budget",
            end - start
        );
        windows += 1;
        if matches!(
            window,
            super::split::Window::End { .. } | super::split::Window::Unparsable { .. }
        ) {
            break;
        }
        start = end;
    }
    assert!(windows > 10, "the stream was not windowed at all");
}

/// An operation is never split, so the window budget alone cannot bound one
/// enormous operation: a `TJ` array of six million numbers decoded to 768 MB
/// whatever the window was, and a compressed one reached that from a few
/// kilobytes of file. The count is bounded instead, and an operation past the
/// cap is skipped rather than materialized — declared, not dropped in silence.
#[test]
fn operands_per_operation_boundary() {
    fn array_document(elements: usize) -> Vec<u8> {
        let mut content = b"BT /F1 12 Tf 1 0 0 1 72 700 Tm [".to_vec();
        for _ in 0..elements {
            content.extend_from_slice(b"(a) 1 ");
        }
        content.extend_from_slice(b"] TJ ET");
        measured_document(&content)
    }

    // Two operands per element, plus the array itself.
    let at = geometry(&array_document(limits::MAX_OPERANDS_PER_OPERATION / 2 - 1));
    assert!(
        !notice_codes(&at).contains(&"pdf_operation_operand_limit_exceeded"),
        "an operation inside the budget was skipped"
    );
    assert!(at.pages[0].runs.iter().any(|run| run.text == "a"));

    let over = geometry(&array_document(limits::MAX_OPERANDS_PER_OPERATION));
    assert!(notice_codes(&over).contains(&"pdf_operation_operand_limit_exceeded"));
    assert!(over.truncated);
    assert!(
        over.pages[0].runs.is_empty(),
        "an over-budget operation was interpreted after all"
    );
}

/// `/Widths` and `/W` were materialized with no budget of their own, and
/// `MAX_FONTS_PER_PAGE` multiplies whatever one font costs: 1.5 million `/W`
/// entries inside a compressed object stream reached 1.16 GB from a 3.5 MB file
/// and reported `IndexedComplete` with no notice at all.
#[test]
fn font_width_entries_boundary() {
    fn widths_document(entries: usize) -> Vec<u8> {
        let widths = (0..entries).map(|_| "500").collect::<Vec<_>>().join(" ");
        let font_id = DocumentSpec::first_font_id(1);
        DocumentSpec {
            pages: vec![PageSpec::new(b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (A) Tj ET")],
            font_objects: vec![
                format!(
                    "<< /Type /Font /Subtype /Type1 /BaseFont /Wide \
                     /Encoding /WinAnsiEncoding /FirstChar 0 /Widths [{widths}] >>"
                )
                .into_bytes(),
            ],
            font_resources: format!("/F1 {font_id} 0 R"),
            extra_trailer: String::new(),
        }
        .build()
    }

    let at = geometry(&widths_document(limits::MAX_FONT_WIDTH_ENTRIES));
    assert!(
        !notice_codes(&at).contains(&"pdf_font_width_limit_exceeded"),
        "a font inside the budget was truncated"
    );
    assert!(!at.truncated);
    // The cap is lossless on a well-formed font, so the width still measures.
    assert!(at.pages[0].runs[0].geometry_exact);

    let over = geometry(&widths_document(limits::MAX_FONT_WIDTH_ENTRIES + 1));
    assert!(notice_codes(&over).contains(&"pdf_font_width_limit_exceeded"));
    assert!(over.truncated);
}

/// The page locator had no receiving end: `SourceLocator` named only
/// `BaseView`, so an admitted PDF would have produced `locator: None` on every
/// section and discarded the page number — leaving the empty `heading_path`
/// with no other half to its justification.
#[test]
fn a_page_locator_maps_onto_the_preparation_locator_vocabulary() {
    use crate::extract::SourceLocator;

    let candidate = super::extract_pdf_candidate(&measured_document(
        b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (page one) Tj ET",
    ));
    let section = &candidate.sections[0];

    assert!(
        section.heading_path.is_empty(),
        "the page must never become a ranking heading"
    );
    assert_eq!(
        section.locator.to_source_locator(),
        SourceLocator::PdfPage { page: 1 }
    );
    // `PreparedChunk::source_locator` is carried verbatim across the wire, so
    // the tag has to survive a round trip as well as exist.
    let encoded = serde_json::to_string(&section.locator.to_source_locator()).expect("encode");
    assert_eq!(encoded, r#"{"kind":"pdf_page","page":1}"#);
    assert_eq!(
        serde_json::from_str::<SourceLocator>(&encoded).expect("decode"),
        SourceLocator::PdfPage { page: 1 }
    );
}
