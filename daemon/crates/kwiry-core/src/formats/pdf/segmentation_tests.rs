// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for the segmentation layer: positioned runs to page text.
//!
//! Every fixture is a content stream written by hand, so each assertion is
//! checkable against the operators above it. The measured font advances every
//! glyph by exactly half the font size, which makes an expected `x` an integer
//! arithmetic statement rather than a number recorded from a previous run.
//!
//! The tests are grouped by the decision they pin:
//!
//! 1. **Space versus nothing.** Including the boundary the previous crate
//!    fused: two runs separated by a column-width gap.
//! 2. **Space versus line break versus paragraph.**
//! 3. **Reading order.** Two columns must not interleave; a table must not be
//!    read column-major.
//! 4. **Outcomes.** Page sections, the page locator, no invented text, partial
//!    coverage with the failure declared, and content-free notices.

use crate::extract::ExtractionCoverage;

use super::test_support::{helvetica_document, measured_document, measured_pages};
use super::{PdfCandidate, extract_pdf_candidate};

/// The composed text of a single-page fixture.
#[track_caller]
fn page_text(bytes: &[u8]) -> String {
    let candidate = extract_pdf_candidate(bytes);
    assert!(
        candidate.coverage.is_indexed(),
        "fixture should index: {:?} {:?}",
        candidate.coverage,
        candidate.notices
    );
    assert_eq!(candidate.sections.len(), 1, "fixture should be one page");
    candidate.sections[0].content.clone()
}

fn codes(candidate: &PdfCandidate) -> Vec<&str> {
    candidate
        .notices
        .iter()
        .map(|notice| notice.code.as_str())
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Space versus nothing
// ---------------------------------------------------------------------------

#[test]
fn adjacent_show_operators_do_not_gain_a_space() {
    // Two shows with no reposition between them: the second starts exactly
    // where the first ended, so there is no gap and no space belongs there.
    // 'H','e','l' at 10pt is 3 * 5 = 15pt, so the second run starts at 87.
    let text = page_text(&measured_document(
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (Hel) Tj (lo) Tj ET",
    ));
    assert_eq!(text, "Hello");
}

#[test]
fn a_word_sized_gap_becomes_exactly_one_space() {
    // 'H','i' is 10pt wide, so the first run ends at 82 and the second begins
    // at 92: a 10pt hole where the producer drew no space glyph.
    let text = page_text(&measured_document(
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (Hi) Tj 1 0 0 1 92 700 Tm (there) Tj ET",
    ));
    assert_eq!(text, "Hi there");
}

#[test]
fn a_drawn_space_is_never_doubled() {
    // The producer drew the separator itself. Inferring a second one turns
    // "Hi there" into "Hi  there" and then into a different token stream.
    let text = page_text(&measured_document(
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (Hi ) Tj (there) Tj ET",
    ));
    assert_eq!(text, "Hi there");
}

#[test]
fn a_column_width_gap_never_fuses_two_runs() {
    // This is the exact shape the previous extractor failed on: one BT block,
    // cells positioned with Td, and no space glyph anywhere in the stream. It
    // reacted to no positioning operator at all and emitted `nameqtyprice`.
    //
    // 'N','a','m','e' at 11pt is 4 * 5.5 = 22pt, so the first cell ends at 94
    // and the second begins at 192. A 98pt hole is not a kerning pair.
    let text = page_text(&measured_document(
        b"BT /F1 11 Tf 72 700 Td (Name) Tj 120 0 Td (Qty) Tj 74 0 Td (Price) Tj ET",
    ));

    assert!(!text.contains("NameQty"), "runs fused: {text}");
    assert!(!text.contains("QtyPrice"), "runs fused: {text}");
    assert_eq!(text, "Name Qty Price");
}

#[test]
fn a_column_width_gap_survives_a_font_that_ships_no_widths() {
    // The failing fixture used Helvetica, which carries no /Widths at all. The
    // reader reports a zero advance for it rather than guessing, so the whole
    // decision rests on the layer's own estimate: 'N','a','m','e' is
    // 720+500+720+500 = 2440/1000 em, i.e. 26.84pt at 11pt, against a measured
    // 29.3pt. The estimate is wrong by 8% and the decision is unchanged,
    // because the gap it has to resolve is 93pt.
    let text = page_text(&helvetica_document(&[
        b"BT /F1 11 Tf 72 700 Td (Name) Tj 120 0 Td (Qty) Tj ET",
    ]));

    assert!(!text.contains("NameQty"), "runs fused: {text}");
    assert_eq!(text, "Name Qty");
}

#[test]
fn a_kerning_sized_gap_does_not_become_a_space() {
    // A TJ adjustment of 40/1000 em at 10pt moves the pen 0.4pt. That is a
    // kerning pair, not a word boundary, and each TJ element is its own run —
    // so this boundary is decided by the gap test and nothing else.
    let text = page_text(&measured_document(
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm [(tight) -40 (pair)] TJ ET",
    ));
    assert_eq!(text, "tightpair");
}

#[test]
fn a_raised_marker_does_not_glue_onto_the_word_it_annotates() {
    // A footnote marker is drawn at the measured end of the word, so its gap is
    // zero and the gap test alone emits `word3`. The size-and-baseline test is
    // what keeps the marker a separate token.
    let text = page_text(&measured_document(
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (word) Tj /F1 6 Tf 1 0 0 1 92 704 Tm (3) Tj ET",
    ));

    assert!(!text.contains("word3"), "marker fused: {text}");
    assert_eq!(text, "word 3");
}

// ---------------------------------------------------------------------------
// 2. Space versus line break versus paragraph
// ---------------------------------------------------------------------------

#[test]
fn a_new_baseline_is_a_line_break_and_not_a_space() {
    let text = page_text(&measured_document(
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (first) Tj 1 0 0 1 72 686 Tm (second) Tj ET",
    ));
    assert_eq!(text, "first\nsecond");
}

#[test]
fn a_wider_baseline_step_starts_a_paragraph() {
    // Three lines at 14pt leading, then one at 20pt. 20 is 1.43 times the
    // median leading — over the paragraph threshold and under the band-cut
    // threshold, so this is the paragraph rule deciding and not the XY-cut.
    let text = page_text(&measured_document(
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (alpha) Tj \
          1 0 0 1 72 686 Tm (bravo) Tj \
          1 0 0 1 72 672 Tm (charlie) Tj \
          1 0 0 1 72 652 Tm (delta) Tj ET",
    ));
    assert_eq!(text, "alpha\nbravo\ncharlie\n\ndelta");
}

#[test]
fn a_line_ending_hyphen_is_never_repaired() {
    // Whether the hyphen is a line break or a spelling is not in the geometry.
    // Joining the halves would rewrite the author's text on a guess.
    let text = page_text(&measured_document(
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (recon-) Tj 1 0 0 1 72 686 Tm (ciliation) Tj ET",
    ));
    assert_eq!(text, "recon-\nciliation");
}

// ---------------------------------------------------------------------------
// 3. Reading order
// ---------------------------------------------------------------------------

/// Two columns sharing every baseline, drawn row by row in the content stream —
/// the layout that makes sorting by `y` produce sentences nobody wrote.
///
/// The lines fill their columns, because that is what two-column prose *is*:
/// wide columns separated by a thin gutter. The distinction is load-bearing.
/// Four short words at `x=72` and `x=320` with two hundred points of white
/// space between them are not prose in two columns — they are a label/value
/// table, and no local measurement can tell that geometry apart from
/// [`TWO_COLUMN_TABLE`] below. `is_table` therefore separates the two on the
/// corridor-to-content ratio, and this fixture has to be honest about which
/// side of it the layout it claims to be actually sits on.
const TWO_COLUMNS: &[u8] = b"BT /F1 10 Tf \
    1 0 0 1 72 700 Tm (Alpha rendering keeps this column intact) Tj \
    1 0 0 1 296 700 Tm (Echo opens the right column of the page) Tj \
    1 0 0 1 72 686 Tm (Bravo follows in the same column below) Tj \
    1 0 0 1 296 686 Tm (Foxtrot wraps onto the second line here) Tj \
    1 0 0 1 72 672 Tm (Charlie continues the left column here) Tj \
    1 0 0 1 296 672 Tm (Golf keeps the right column filled out) Tj \
    1 0 0 1 72 658 Tm (Delta closes the left column of prose) Tj \
    1 0 0 1 296 658 Tm (Hotel ends the right column of this page) Tj ET";

#[test]
fn two_columns_are_read_column_by_column_and_never_interleaved() {
    let text = page_text(&measured_document(TWO_COLUMNS));

    assert_eq!(
        text,
        "Alpha rendering keeps this column intact\n\
         Bravo follows in the same column below\n\
         Charlie continues the left column here\n\
         Delta closes the left column of prose\n\
         \n\
         Echo opens the right column of the page\n\
         Foxtrot wraps onto the second line here\n\
         Golf keeps the right column filled out\n\
         Hotel ends the right column of this page"
    );
    for line in text.lines() {
        assert!(
            !(line.contains("Alpha") && line.contains("Echo")),
            "columns spliced onto one line: {line}"
        );
    }
}

#[test]
fn a_positioned_table_is_read_row_major_rather_than_column_major() {
    // Three columns with wide gaps and narrow cells. Recursing this as columns
    // would emit every heading, then every quantity, then every price, and
    // destroy all four rows.
    let text = page_text(&measured_document(
        b"BT /F1 10 Tf \
          1 0 0 1 72 700 Tm (Name) Tj 1 0 0 1 200 700 Tm (Qty) Tj 1 0 0 1 330 700 Tm (Price) Tj \
          1 0 0 1 72 686 Tm (widget) Tj 1 0 0 1 200 686 Tm (12) Tj 1 0 0 1 330 686 Tm (3.50) Tj \
          1 0 0 1 72 672 Tm (gadget) Tj 1 0 0 1 200 672 Tm (7) Tj 1 0 0 1 330 672 Tm (11.25) Tj \
          1 0 0 1 72 658 Tm (bolt) Tj 1 0 0 1 200 658 Tm (9) Tj 1 0 0 1 330 658 Tm (0.75) Tj ET",
    ));

    assert_eq!(
        text,
        "Name\tQty\tPrice\nwidget\t12\t3.50\ngadget\t7\t11.25\nbolt\t9\t0.75"
    );
}

#[test]
fn a_full_width_heading_stays_above_the_columns_it_introduces() {
    // The heading crosses the gutter, so there is no full-height corridor at
    // the root and the region splits horizontally first. Vertical precedence is
    // safe precisely because a corridor has to survive the whole region.
    let mut content = b"BT /F1 18 Tf 1 0 0 1 72 740 Tm (Heading) Tj ET ".to_vec();
    content.extend_from_slice(TWO_COLUMNS);
    let text = page_text(&measured_document(&content));

    assert!(
        text.starts_with("Heading\n"),
        "heading was displaced: {text}"
    );
    assert!(
        text.find("Alpha").unwrap() < text.find("Echo").unwrap(),
        "columns interleaved: {text}"
    );
}

#[test]
fn a_page_of_scattered_marks_is_read_by_line_rather_than_by_column() {
    // Seventy single-glyph runs per baseline produce sixty-nine full-height
    // holes. That is scattered marks, not a seventy-column layout, and treating
    // it as one would also make the child lookup quadratic in the run budget.
    let mut content = String::from("BT /F1 6 Tf");
    for row in 0..5 {
        for column in 0..70 {
            content.push_str(&format!(
                " 1 0 0 1 {} {} Tm (x) Tj",
                20 + column * 8,
                700 - row * 12
            ));
        }
    }
    content.push_str(" ET");
    let text = page_text(&measured_document(content.as_bytes()));

    assert_eq!(text.lines().count(), 5, "rows were reordered: {text}");
    for line in text.lines() {
        assert_eq!(line, ["x"; 70].join(" "));
    }
}

#[test]
fn rotated_text_is_kept_but_never_placed_into_the_reading_order() {
    let mut content = TWO_COLUMNS.to_vec();
    content.extend_from_slice(b" BT /F1 10 Tf 0 1 -1 0 300 400 Tm (Sideways) Tj ET");
    let candidate = extract_pdf_candidate(&measured_document(&content));
    let text = &candidate.sections[0].content;

    assert!(
        text.contains("Sideways"),
        "rotated text was dropped: {text}"
    );
    assert!(
        text.ends_with("Sideways"),
        "rotated text was interleaved: {text}"
    );
    assert!(codes(&candidate).contains(&"pdf_unordered_text_appended"));
    assert_eq!(candidate.coverage, ExtractionCoverage::IndexedPartial);
}

// ---------------------------------------------------------------------------
// 4. Outcomes
// ---------------------------------------------------------------------------

#[test]
fn pdf_is_still_not_admitted_now_that_a_candidate_exists() {
    use crate::format::SourceFormat;

    let bytes = measured_document(b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (Indexable) Tj ET");
    // The candidate composes real text …
    let candidate = extract_pdf_candidate(&bytes);
    assert_eq!(candidate.coverage, ExtractionCoverage::IndexedComplete);
    assert_eq!(candidate.sections[0].content, "Indexable");

    // … and none of it reaches the product.
    assert!(!SourceFormat::Pdf.is_extractable());
    assert!(!SourceFormat::Pdf.spec().extraction_supported);
    assert!(SourceFormat::from_extractable_path("notes.pdf").is_none());
    let extracted =
        crate::formats::extract_source(SourceFormat::Pdf, &bytes).expect("the stub never errors");
    assert_eq!(
        extracted.coverage,
        ExtractionCoverage::SkippedNoExtractableText
    );
    assert!(extracted.sections.is_empty());
    assert_eq!(extracted.notices[0].code, "format_not_yet_supported");
}

#[test]
fn every_section_belongs_to_exactly_one_page() {
    // Preparation splits a section and never merges two, so one section per
    // page is what makes "a chunk never spans two pages" a property of the
    // extraction rather than a hope about the chunker.
    let candidate = extract_pdf_candidate(&measured_pages(&[
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (alpha) Tj ET",
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (bravo) Tj ET",
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (charlie) Tj ET",
    ]));

    assert_eq!(candidate.page_count, 3);
    assert_eq!(candidate.coverage, ExtractionCoverage::IndexedComplete);
    let pages: Vec<u32> = candidate
        .sections
        .iter()
        .map(|section| section.locator.page)
        .collect();
    assert_eq!(pages, vec![1, 2, 3]);

    let vocabulary = ["alpha", "bravo", "charlie"];
    for (index, section) in candidate.sections.iter().enumerate() {
        assert_eq!(section.content, vocabulary[index]);
        for (other, word) in vocabulary.iter().enumerate() {
            assert_eq!(
                section.content.contains(word),
                other == index,
                "page {} leaked into page {}",
                other + 1,
                index + 1
            );
        }
    }
}

#[test]
fn the_page_is_a_locator_and_never_ranked_text() {
    // heading_path is joined into heading_text, normalized, and matched against
    // queries. A page number there would make every page of every PDF a lexical
    // match for a number the author never wrote.
    let candidate = extract_pdf_candidate(&measured_pages(&[
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (alpha) Tj ET",
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (bravo) Tj ET",
    ]));

    for section in &candidate.sections {
        assert!(section.heading_path.is_empty());
        assert!(!section.content.contains("Page"));
        assert!(!section.content.contains(&section.locator.page.to_string()));
    }
    assert_eq!(candidate.sections[1].locator.page, 2);
}

#[test]
fn a_page_without_a_text_layer_is_declared_rather_than_invented() {
    let bytes = measured_document(b"0.5 0.5 0.5 rg 10 10 100 100 re f");
    let candidate = extract_pdf_candidate(&bytes);

    assert_eq!(
        candidate.coverage,
        ExtractionCoverage::SkippedNoExtractableText
    );
    assert!(candidate.sections.is_empty());
    assert_eq!(codes(&candidate), vec!["pdf_no_text_layer"]);
    // Deterministic: the same bytes produce the same outcome, and the outcome
    // is an absence rather than a placeholder.
    assert_eq!(candidate, extract_pdf_candidate(&bytes));
}

#[test]
fn a_document_mixing_text_and_textless_pages_is_partial_with_the_gap_declared() {
    let candidate = extract_pdf_candidate(&measured_pages(&[
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (alpha) Tj ET",
        b"0.5 0.5 0.5 rg 10 10 100 100 re f",
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (charlie) Tj ET",
    ]));

    assert_eq!(candidate.page_count, 3);
    assert_eq!(candidate.coverage, ExtractionCoverage::IndexedPartial);
    assert_eq!(
        candidate
            .sections
            .iter()
            .map(|section| section.locator.page)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert!(codes(&candidate).contains(&"pdf_page_without_text_layer"));
}

#[test]
fn a_page_the_reader_could_not_interpret_leaves_the_rest_indexed() {
    let candidate = extract_pdf_candidate(&measured_pages(&[
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (alpha) Tj ET",
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (bravo) Tj Q ET",
    ]));

    assert_eq!(candidate.coverage, ExtractionCoverage::IndexedPartial);
    assert_eq!(candidate.sections.len(), 2);
    assert!(codes(&candidate).contains(&"pdf_graphics_stack_underflow"));
}

#[test]
fn typed_outcomes_carry_no_document_content() {
    let candidate = extract_pdf_candidate(&measured_pages(&[
        b"BT /F1 10 Tf 1 0 0 1 72 700 Tm (Confidential) Tj ET",
        b"0.5 0.5 0.5 rg 10 10 100 100 re f",
    ]));

    assert!(!candidate.notices.is_empty());
    for notice in &candidate.notices {
        assert!(
            !notice.message.contains("Confidential") && !notice.code.contains("Confidential"),
            "a notice carried document content: {notice:?}"
        );
    }
}

#[test]
fn an_unreadable_document_is_typed_without_sections() {
    let candidate = extract_pdf_candidate(b"not a pdf at all");

    assert_eq!(candidate.coverage, ExtractionCoverage::Quarantined);
    assert!(candidate.sections.is_empty());
    assert_eq!(candidate.page_count, 0);
    assert_eq!(codes(&candidate), vec!["pdf_not_a_pdf"]);
}

#[test]
fn composition_is_deterministic() {
    let bytes = measured_document(TWO_COLUMNS);
    let first = extract_pdf_candidate(&bytes);
    for _ in 0..4 {
        assert_eq!(extract_pdf_candidate(&bytes), first);
    }
}

// ---------------------------------------------------------------------------
// 5. Regressions
//
// One test per accepted finding from the adversarial review, each written so
// that reverting the fix fails it. Where a threshold decides, both sides of the
// threshold are pinned: a guard that fires on everything is not a fix.
// ---------------------------------------------------------------------------

/// Six label/value rows: short cells at `x=72` and `x=320`, separated by far
/// more white space than either column occupies.
const TWO_COLUMN_TABLE: &[u8] = b"BT /F1 10 Tf \
    1 0 0 1 72 700 Tm (Setting) Tj 1 0 0 1 320 700 Tm (Value) Tj \
    1 0 0 1 72 686 Tm (Debounce) Tj 1 0 0 1 320 686 Tm (250 ms) Tj \
    1 0 0 1 72 672 Tm (Batch size) Tj 1 0 0 1 320 672 Tm (512) Tj \
    1 0 0 1 72 658 Tm (Retries) Tj 1 0 0 1 320 658 Tm (3) Tj \
    1 0 0 1 72 644 Tm (Timeout) Tj 1 0 0 1 320 644 Tm (30 s) Tj \
    1 0 0 1 72 630 Tm (Workers) Tj 1 0 0 1 320 630 Tm (4) Tj ET";

/// A two-column table was read column-major and reported `IndexedComplete` with
/// no notice, so every row pairing was destroyed silently: no line held both a
/// label and its value, and `Debounce` sat six lines from `250 ms`. Label/value
/// tables, glossaries, contents pages and invoices are the dominant real
/// two-column table shape, and the corpus that scored 1.000 contained none.
#[test]
fn a_two_column_label_value_table_is_read_row_major() {
    let text = page_text(&measured_document(TWO_COLUMN_TABLE));

    assert_eq!(
        text,
        "Setting\tValue\n\
         Debounce\t250 ms\n\
         Batch size\t512\n\
         Retries\t3\n\
         Timeout\t30 s\n\
         Workers\t4"
    );
    for (label, value) in [("Debounce", "250 ms"), ("Timeout", "30 s")] {
        assert!(
            text.lines()
                .any(|line| line.contains(label) && line.contains(value)),
            "row pairing lost for {label}: {text}"
        );
    }
}

/// The other side of the same threshold. Two children are admitted as a table
/// only when the corridor is at least as wide as the columns it separates; two
/// columns of prose are wide with a thin gutter and must still be read
/// column-major, or every line of the page becomes two spliced sentences.
#[test]
fn two_columns_of_prose_are_not_stolen_by_the_two_column_table_rule() {
    let text = page_text(&measured_document(TWO_COLUMNS));

    assert!(
        !text.contains('\t'),
        "prose columns were read as a table: {text}"
    );
    for line in text.lines() {
        assert!(
            !(line.contains("Alpha") && line.contains("Echo")),
            "columns spliced onto one line: {line}"
        );
    }
}

/// `horizontal_split` ran before the table branch, so a table whose header sat
/// above a wider-than-normal gap was cut in two: the header row lost its cell
/// separators to a plain block render while the body kept its tabs, and any
/// consumer of the tab structure saw a headerless table.
#[test]
fn a_table_with_a_header_gap_keeps_its_header_row_in_the_table() {
    let text = page_text(&measured_document(
        b"BT /F1 10 Tf \
          1 0 0 1 72 700 Tm (Vault) Tj 1 0 0 1 200 700 Tm (Created) Tj \
          1 0 0 1 330 700 Tm (Modified) Tj \
          1 0 0 1 72 660 Tm (atlas) Tj 1 0 0 1 200 660 Tm (412) Tj \
          1 0 0 1 330 660 Tm (9038) Tj \
          1 0 0 1 72 646 Tm (borealis) Tj 1 0 0 1 200 646 Tm (58) Tj \
          1 0 0 1 330 646 Tm (1276) Tj \
          1 0 0 1 72 632 Tm (cinder) Tj 1 0 0 1 200 632 Tm (2033) Tj \
          1 0 0 1 330 632 Tm (44190) Tj ET",
    ));

    assert_eq!(
        text,
        "Vault\tCreated\tModified\n\
         atlas\t412\t9038\n\
         borealis\t58\t1276\n\
         cinder\t2033\t44190"
    );
}

/// A negative `Tc` on a font that ships no `/Widths` accumulates a *negative*
/// advance, because every glyph width is zero and only the letterspacing is
/// left. Reading direction off that displacement classified ordinary upright
/// prose as rotated: the line was appended after the page's ordered text and
/// the page was downgraded to `IndexedPartial` with
/// `pdf_unordered_text_appended`. Direction now comes from the rendering
/// matrix, which no font metric can affect.
#[test]
fn negative_letterspacing_on_a_metrics_free_font_is_not_rotation() {
    let candidate = extract_pdf_candidate(&helvetica_document(&[b"BT /F1 11 Tf \
        -0.35 Tc 1 0 0 1 72 700 Tm (Tight glyph spacing pulls letters together) Tj \
        0 Tc 1 0 0 1 72 686 Tm (A normal following line of prose.) Tj ET"]));

    assert_eq!(candidate.coverage, ExtractionCoverage::IndexedComplete);
    assert!(
        codes(&candidate).is_empty(),
        "upright text was reported as unordered: {:?}",
        candidate.notices
    );
    assert_eq!(
        candidate.sections[0].content,
        "Tight glyph spacing pulls letters together\nA normal following line of prose."
    );
}

/// The other side of that change: genuinely rotated text must still be
/// recognized, or the fix would have replaced one wrong answer with another.
#[test]
fn genuinely_rotated_text_is_still_detected_without_font_metrics() {
    let candidate = extract_pdf_candidate(&helvetica_document(&[b"BT /F1 11 Tf \
        1 0 0 1 72 700 Tm (Upright body text on the page.) Tj \
        0 1 -1 0 300 400 Tm (Sideways) Tj ET"]));

    let text = &candidate.sections[0].content;
    assert!(
        text.ends_with("Sideways"),
        "rotated run was ordered: {text}"
    );
    assert!(codes(&candidate).contains(&"pdf_unordered_text_appended"));
}
