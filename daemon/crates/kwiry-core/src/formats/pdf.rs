// SPDX-License-Identifier: MIT OR Apache-2.0

//! PDF extraction: the reader, the layout layer, and the dispatch entry point.
//!
//! # Admission status
//!
//! PDF is admitted. `SourceFormat::Pdf` reports `extraction_supported() ==
//! true`, discovery accepts `.pdf`, and [`extract`] composes a real
//! [`ExtractedSource`] from [`extract_pdf_candidate`]. The reader is part of the
//! `portable` feature set, so the daemon and the plugin's WASM bundle compile
//! byte-identical extraction — which is what [`ExtractionProfile::Portable`]
//! asserts and what the extraction-policy fingerprint gates.
//!
//! Exactly one thing still varies by feature: `native-pdf-extractor` adds the
//! embedded-font tier (`embedded`), which recovers Unicode for subset fonts the
//! portable profile declines. That is a *coverage* superset and never a
//! different segmentation of a source both tiers index — see `embedded` and
//! `tier_tests`. The `internal-pdf-extractor` feature no longer changes what is
//! compiled; it only gates the reader's internal vocabulary as a re-export.
//!
//! [`ExtractionProfile::Portable`]: crate::policy::ExtractionProfile::Portable
//!
//! # Layering
//!
//! Document loading, page enumeration, per-page content-stream access, the
//! budgets `lopdf` does not provide, and **positioned glyph runs in device
//! space** ([`read_pdf_geometry`]); then, on top of that and only on top of
//! that, space inference, line and paragraph assembly, column detection,
//! reading-order recovery, and per-page sections
//! ([`extract_pdf_candidate`]).
//!
//! The split is deliberate and stays. The reader reports what the file says and
//! refuses to guess — a font with no metrics gets a zero advance and an
//! `geometry_exact == false`, not an invented width. Every guess lives in
//! `layout`, next to the threshold that encodes it and the reason that
//! threshold has the value it has.
//!
//! # Why `lopdf`
//!
//! It was the best-bounded candidate the PDF dependency gate measured: 96.0 MiB
//! worst case against 448 MiB (hayro) and 574 MiB (pdf-extract) on the streams
//! the gate exercised. The cap is `LoadOptions::max_decompressed_size`, which is
//! `Option<usize>` defaulting to `None` — so it only exists if the caller sets
//! it, and this module always does. It is also the reason every page and font
//! accessor used here is the `*_with_limit` variant: an unbounded accessor
//! re-inflates a stream even after a bounded load.
//!
//! What the gate did **not** establish is that peak allocation tracks that cap
//! in general. It does not: decompressed bytes become `Object`s and
//! `Vec<Operation>`s at a large constant factor. Both places that showed up are
//! bounded here now — content streams are decoded in windows (`split`) and
//! width tables are capped (`fonts`) — and the one that is not, because it is
//! upstream of this module entirely, is written down on
//! `limits::MAX_DECOMPRESSED_STREAM_BYTES`.
//!
//! # Encryption
//!
//! Refused, not decrypted. This reader has no password channel — there is no
//! prompt, no config key, no argument — so it never supplies one and never
//! accepts a document that needed one. `lopdf` attempts an empty-user-password
//! authentication internally while parsing the trailer, before any caller code
//! runs; that cannot be switched off through `LoadOptions`. What this module
//! guarantees is the part it owns: a document that declares `/Encrypt` is
//! rejected with [`PdfReadError::EncryptedDocument`] before a single content
//! stream is touched, so no text from an encrypted PDF is ever produced.

use crate::extract::{
    ContentRole, ExtractedSection, ExtractedSource, ExtractionCompleteness, ExtractionCoverage,
    ExtractionNotice,
};
use crate::model::{Frontmatter, PropertyBag};

/// Production PDF extraction.
///
/// A PDF carries no frontmatter, no aliases, and no outbound links this project
/// is willing to claim, so the only things that survive into the index are the
/// per-page text and the page number — and the page number travels as
/// [`SourceLocator::PdfPage`] rather than as a heading, because `heading_path`
/// is ranked text and "Page 7" is not text the author wrote. See `candidate`.
///
/// [`SourceLocator::PdfPage`]: crate::extract::SourceLocator::PdfPage
pub(super) fn extract(bytes: &[u8]) -> ExtractedSource {
    let candidate = extract_pdf_candidate(bytes);
    if !candidate.coverage.is_indexed() {
        let mut source =
            ExtractedSource::skipped(
                candidate.coverage,
                candidate.notices.first().cloned().unwrap_or_else(|| {
                    ExtractionNotice::new("pdf_unreadable", "PDF was not indexed")
                }),
            );
        source.notices = candidate.notices;
        return source;
    }

    let completeness = if candidate.coverage == ExtractionCoverage::IndexedPartial {
        ExtractionCompleteness::Partial
    } else {
        ExtractionCompleteness::Complete
    };
    ExtractedSource::indexed(
        PropertyBag::default(),
        Frontmatter::default(),
        Vec::new(),
        Vec::new(),
        candidate
            .sections
            .into_iter()
            .map(|section| ExtractedSection {
                heading_path: section.heading_path,
                content: section.content,
                role: ContentRole::Primary,
                locator: Some(section.locator.to_source_locator()),
            })
            .collect(),
        completeness,
        candidate.notices,
    )
}

mod candidate;
mod cmap;
mod content;
mod embedded;
mod error;
mod fonts;
mod layout;
mod limits;
mod prescreen;
mod split;
mod state;

#[cfg(test)]
mod test_support;

pub use candidate::{PdfCandidate, PdfPageLocator, PdfSection, extract_pdf_candidate};
pub use error::PdfReadError;
pub use reader::{
    PdfDocumentGeometry, PdfLimits, PdfPageGeometry, PdfTextRun, PdfWritingMode, pdf_limits,
    read_pdf_geometry,
};

mod reader {
    use lopdf::{Document, LoadOptions, Object};
    use serde::{Deserialize, Serialize};

    use crate::extract::ExtractionNotice;

    use super::content::{self, TextBudget};
    use super::limits;
    use super::prescreen;
    use super::state::Matrix;

    use super::error::PdfReadError;

    /// `/Rotate` values are constrained to multiples of 90 (PDF 1.7 §7.7.3.3).
    const ROTATION_STEP: i64 = 90;
    /// Longest `/Parent` chain walked when resolving an inheritable page
    /// attribute. A malformed tree can be cyclic; the walk is bounded and the
    /// visited set is checked, so neither costs unbounded time.
    const MAX_PAGE_TREE_DEPTH: usize = 64;

    /// Direction the glyphs of a run advance in.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum PdfWritingMode {
        Horizontal,
        /// `/WMode 1` (`Identity-V` and the `…-V` predefined CMaps). Detected
        /// and reported, never guessed at: the run's glyphs are decoded but its
        /// advance is zero and `geometry_exact` is `false`.
        Vertical,
    }

    /// One maximal contiguous show sequence under an unchanged text state.
    ///
    /// Coordinates are **device space**: origin at the top-left corner of the
    /// effective crop box, `x` increasing right, `y` increasing **down**, in
    /// points, after `/Rotate` has been applied. This is the space a viewer
    /// paints in, so "reads earlier" is "smaller `y`, then smaller `x`" without
    /// any per-page sign juggling in the step that consumes these.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct PdfTextRun {
        /// 1-based page number in `/Pages` order.
        pub page_number: u32,
        /// 1-based ordinal of the enclosing `BT` block within the page.
        pub text_object_index: u32,
        /// 0-based ordinal of this run within the page, in content-stream order.
        pub run_index: u32,
        /// Decoded text. Codes that the font's encoding does not map contribute
        /// nothing rather than a replacement character.
        pub text: String,
        /// Glyph origin of the first glyph, device space.
        pub origin: [f64; 2],
        /// Pen position after the run, device space. `end - origin` is the
        /// advance; it is exactly zero for a vertical-mode run.
        pub end: [f64; 2],
        /// Device-space direction one unit of text-space advance points in,
        /// `(Tm × CTM)` applied to `(1, 0)`.
        ///
        /// Reported separately from `end - origin` because that difference is
        /// only a direction when the glyph widths behind it were measured. A
        /// font that ships no metrics contributes a zero width per glyph, so
        /// the advance collapses to the accumulated `Tc`/`Tw` — and a perfectly
        /// ordinary negative `Tc` then makes `end` sit to the *left* of
        /// `origin` on upright text. This vector comes from the matrix alone
        /// and is therefore independent of whether the font declared its
        /// widths. `[0.0, 0.0]` means the matrix is degenerate and carries no
        /// orientation at all.
        pub advance_direction: [f64; 2],
        /// Font size in device-space points, `Tfs · sqrt(|det(Tm × CTM)|)`.
        pub font_size: f64,
        /// Resource name from `Tf` (for example `F1`), lossy-decoded.
        pub font_resource: String,
        pub char_spacing: f64,
        pub word_spacing: f64,
        /// `Tz / 100`.
        pub horizontal_scale: f64,
        pub rise: f64,
        /// `Tr`. Mode 3 (invisible) and 7 (clip-only) are reported, not
        /// filtered: whether OCR-under-image text belongs in an index is a
        /// product decision, not a parser one.
        pub render_mode: i64,
        pub glyph_count: usize,
        pub writing_mode: PdfWritingMode,
        /// `true` only when every glyph width in the run came from `/Widths` or
        /// `/W`. When `false`, `end` is an estimate — the font shipped no
        /// metrics — and the segmentation step must widen its thresholds
        /// accordingly instead of treating the gap as measured.
        pub geometry_exact: bool,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct PdfPageGeometry {
        pub page_number: u32,
        /// Device-space width in points, after `/Rotate`.
        pub width: f64,
        /// Device-space height in points, after `/Rotate`.
        pub height: f64,
        /// Normalized clockwise rotation actually applied: 0, 90, 180, or 270.
        pub rotate: i64,
        pub runs: Vec<PdfTextRun>,
        /// `true` when a budget stopped this page short of its content.
        pub truncated: bool,
        /// `true` when a show operator ran under a font whose codes the
        /// compiled extraction profile cannot decode. This is a *source*-level
        /// fact recorded per page: the whole source is declined, because
        /// decoding only the mappable subset would change section content and
        /// therefore chunk boundaries.
        pub undecodable_font: bool,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct PdfDocumentGeometry {
        pub page_count: usize,
        pub pages: Vec<PdfPageGeometry>,
        /// `true` when any page was truncated or a document-wide budget was
        /// exhausted. The geometry below it is real; it is just not all of it.
        pub truncated: bool,
        /// `true` when any page used a font this extraction profile cannot
        /// decode. Unlike `truncated`, the geometry below it is not "real but
        /// incomplete" — it is a source this profile declines.
        pub undecodable_font: bool,
        pub notices: Vec<ExtractionNotice>,
    }

    impl PdfDocumentGeometry {
        /// Every run on every page, in page then content-stream order.
        pub fn runs(&self) -> impl Iterator<Item = &PdfTextRun> {
            self.pages.iter().flat_map(|page| page.runs.iter())
        }
    }

    /// The budgets this reader enforces. Exposed so a probe can report them and
    /// so the boundary tests assert against the same numbers the reader uses,
    /// rather than against a second copy that could drift.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[non_exhaustive]
    pub struct PdfLimits {
        pub max_input_bytes: usize,
        pub max_decompressed_stream_bytes: usize,
        pub max_pages: usize,
        pub max_content_stream_bytes: usize,
        pub max_total_content_stream_bytes: usize,
        pub max_object_nesting_depth: usize,
        pub max_operations_per_page: usize,
        pub max_content_window_bytes: usize,
        pub max_runs_per_page: usize,
        pub max_total_runs: usize,
        pub max_glyphs_per_page: usize,
        pub max_graphics_stack_depth: usize,
        pub max_fonts_per_page: usize,
        pub max_tounicode_bytes: usize,
        pub max_extracted_text_bytes: usize,
        pub max_notices: usize,
    }

    pub const fn pdf_limits() -> PdfLimits {
        PdfLimits {
            max_input_bytes: limits::MAX_INPUT_BYTES,
            max_decompressed_stream_bytes: limits::MAX_DECOMPRESSED_STREAM_BYTES,
            max_pages: limits::MAX_PAGES,
            max_content_stream_bytes: limits::MAX_CONTENT_STREAM_BYTES,
            max_total_content_stream_bytes: limits::MAX_TOTAL_CONTENT_STREAM_BYTES,
            max_object_nesting_depth: limits::MAX_OBJECT_NESTING_DEPTH,
            max_operations_per_page: limits::MAX_OPERATIONS_PER_PAGE,
            max_content_window_bytes: limits::MAX_CONTENT_WINDOW_BYTES,
            max_runs_per_page: limits::MAX_RUNS_PER_PAGE,
            max_total_runs: limits::MAX_TOTAL_RUNS,
            max_glyphs_per_page: limits::MAX_GLYPHS_PER_PAGE,
            max_graphics_stack_depth: limits::MAX_GRAPHICS_STACK_DEPTH,
            max_fonts_per_page: limits::MAX_FONTS_PER_PAGE,
            max_tounicode_bytes: limits::MAX_TOUNICODE_BYTES,
            max_extracted_text_bytes: limits::MAX_EXTRACTED_TEXT_BYTES,
            max_notices: limits::MAX_NOTICES,
        }
    }

    /// Read a PDF's positioned text geometry.
    ///
    /// This does **not** extract a source: it produces no sections, no
    /// properties, and no text stream, and calling it does not make PDF an
    /// indexable format.
    pub fn read_pdf_geometry(bytes: &[u8]) -> Result<PdfDocumentGeometry, PdfReadError> {
        prescreen::prescreen(bytes)?;

        let document = Document::load_mem_with_options(
            bytes,
            LoadOptions {
                // Never a password: an encrypted document is refused, and a
                // reader that quietly tries one is a reader that quietly
                // succeeds on documents its user believed were sealed.
                password: None,
                filter: None,
                strict: false,
                max_decompressed_size: Some(limits::MAX_DECOMPRESSED_STREAM_BYTES),
            },
        )
        .map_err(|_| PdfReadError::InvalidDocument)?;

        if document.is_encrypted() || document.was_encrypted() {
            return Err(PdfReadError::EncryptedDocument);
        }

        let pages = document.get_pages();
        if pages.len() > limits::MAX_PAGES {
            return Err(PdfReadError::PageLimitExceeded);
        }

        let mut geometry = PdfDocumentGeometry {
            page_count: pages.len(),
            pages: Vec::with_capacity(pages.len()),
            truncated: false,
            undecodable_font: false,
            notices: Vec::new(),
        };
        let mut text_budget = TextBudget::new();
        let mut content_budget = limits::MAX_TOTAL_CONTENT_STREAM_BYTES;
        // Every page's runs stay live in `geometry.pages` until `compose` runs,
        // so this is the bound on the reader's own peak. See
        // `limits::MAX_TOTAL_RUNS`.
        let mut run_budget = limits::MAX_TOTAL_RUNS;

        for (page_number, page_id) in pages {
            let box_geometry = page_box(&document, page_id);
            let (rotate, rotation_valid) = page_rotation(&document, page_id);
            if !rotation_valid {
                note(
                    &mut geometry,
                    super::error::notice::PAGE_ROTATION_INVALID,
                    "page /Rotate is not a multiple of 90; treated as 0".to_string(),
                );
            }
            let Some(media) = box_geometry else {
                note(
                    &mut geometry,
                    super::error::notice::PAGE_GEOMETRY_INVALID,
                    "page box is missing or degenerate; the page was skipped".to_string(),
                );
                geometry.truncated = true;
                geometry.pages.push(PdfPageGeometry {
                    page_number,
                    width: 0.0,
                    height: 0.0,
                    rotate,
                    runs: Vec::new(),
                    truncated: true,
                    undecodable_font: false,
                });
                continue;
            };
            let (base, width, height) = device_transform(media, rotate);

            let page_budget = limits::MAX_CONTENT_STREAM_BYTES.min(content_budget);
            let content = document.get_page_content_with_limit(page_id, page_budget);
            let content = match content {
                Ok(content) => content,
                Err(_) => {
                    let code = if content_budget < limits::MAX_CONTENT_STREAM_BYTES {
                        super::error::notice::DOCUMENT_CONTENT_LIMIT
                    } else {
                        super::error::notice::PAGE_CONTENT_LIMIT
                    };
                    note(
                        &mut geometry,
                        code,
                        format!(
                            "page {page_number} content stream exceeds its {page_budget}-byte budget; the page was skipped"
                        ),
                    );
                    geometry.truncated = true;
                    geometry.pages.push(PdfPageGeometry {
                        page_number,
                        width,
                        height,
                        rotate,
                        runs: Vec::new(),
                        truncated: true,
                        undecodable_font: false,
                    });
                    continue;
                }
            };
            content_budget = content_budget.saturating_sub(content.len());

            let outcome = content::interpret_page(
                &document,
                page_id,
                page_number,
                base,
                &content,
                limits::MAX_RUNS_PER_PAGE.min(run_budget),
                &mut text_budget,
            );
            run_budget = run_budget.saturating_sub(outcome.runs.len());
            for (code, message) in outcome.notices {
                note(&mut geometry, code, message);
            }
            geometry.truncated |= outcome.truncated;
            geometry.undecodable_font |= outcome.undecodable_font;
            geometry.pages.push(PdfPageGeometry {
                page_number,
                width,
                height,
                rotate,
                runs: outcome.runs,
                truncated: outcome.truncated,
                undecodable_font: outcome.undecodable_font,
            });
        }

        Ok(geometry)
    }

    fn note(geometry: &mut PdfDocumentGeometry, code: &'static str, message: String) {
        if geometry.notices.len() >= limits::MAX_NOTICES
            || geometry.notices.iter().any(|notice| notice.code == code)
        {
            return;
        }
        geometry
            .notices
            .push(ExtractionNotice::new(code, message.as_str()));
    }

    /// Effective page box: `/CropBox` when present and usable, `/MediaBox`
    /// otherwise, both inheritable through `/Parent`. Returned normalized as
    /// `[x0, y0, x1, y1]` with `x0 < x1` and `y0 < y1`.
    fn page_box(document: &Document, page_id: lopdf::ObjectId) -> Option<[f64; 4]> {
        let media = inherited(document, page_id, b"MediaBox").and_then(|object| rectangle(&object));
        let crop = inherited(document, page_id, b"CropBox").and_then(|object| rectangle(&object));
        crop.or(media)
    }

    fn rectangle(object: &Object) -> Option<[f64; 4]> {
        let array = object.as_array().ok()?;
        let [a, b, c, d] = array.as_slice() else {
            return None;
        };
        let x0 = super::fonts::number(a)?;
        let y0 = super::fonts::number(b)?;
        let x1 = super::fonts::number(c)?;
        let y1 = super::fonts::number(d)?;
        if ![x0, y0, x1, y1].iter().all(|value| value.is_finite()) {
            return None;
        }
        let normalized = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
        if normalized[2] - normalized[0] <= 0.0 || normalized[3] - normalized[1] <= 0.0 {
            return None;
        }
        Some(normalized)
    }

    fn page_rotation(document: &Document, page_id: lopdf::ObjectId) -> (i64, bool) {
        let Some(raw) = inherited(document, page_id, b"Rotate").and_then(|object| match object {
            Object::Integer(value) => Some(value),
            Object::Real(value) => Some(value as i64),
            _ => None,
        }) else {
            return (0, true);
        };
        if raw % ROTATION_STEP != 0 {
            return (0, false);
        }
        (raw.rem_euclid(360), true)
    }

    fn inherited(document: &Document, page_id: lopdf::ObjectId, key: &[u8]) -> Option<Object> {
        let mut current = page_id;
        let mut visited = std::collections::BTreeSet::new();
        for _ in 0..MAX_PAGE_TREE_DEPTH {
            if !visited.insert(current) {
                return None;
            }
            let dictionary = document.get_dictionary(current).ok()?;
            if let Ok(object) = dictionary.get_deref(key, document) {
                return Some(object.clone());
            }
            current = dictionary.get(b"Parent").ok()?.as_reference().ok()?;
        }
        None
    }

    /// User space → device space: translate the box origin to `(0, 0)`, apply
    /// `/Rotate` clockwise, then flip `y` so it increases downward. Returned
    /// with the post-rotation page size.
    fn device_transform(media: [f64; 4], rotate: i64) -> (Matrix, f64, f64) {
        let [x0, y0, x1, y1] = media;
        let box_width = x1 - x0;
        let box_height = y1 - y0;
        let translate = Matrix::translate(-x0, -y0);
        let (rotation, width, height) = match rotate {
            90 => (
                Matrix([0.0, 1.0, 1.0, 0.0, 0.0, 0.0]),
                box_height,
                box_width,
            ),
            180 => (
                Matrix([-1.0, 0.0, 0.0, 1.0, box_width, 0.0]),
                box_width,
                box_height,
            ),
            270 => (
                Matrix([0.0, -1.0, -1.0, 0.0, box_height, box_width]),
                box_height,
                box_width,
            ),
            _ => (
                Matrix([1.0, 0.0, 0.0, -1.0, 0.0, box_height]),
                box_width,
                box_height,
            ),
        };
        (translate.multiply(rotation), width, height)
    }
}

#[cfg(test)]
mod segmentation_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tier_tests;
