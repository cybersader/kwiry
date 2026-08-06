// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project-owned budgets for the admission-disabled PDF reader.
//!
//! `lopdf` provides exactly one bound of its own —
//! [`LoadOptions::max_decompressed_size`](lopdf::LoadOptions::max_decompressed_size),
//! which is `Option<usize>` defaulting to `None`, i.e. **unbounded unless the
//! caller sets it**. Everything else below is a bound `lopdf` does not have and
//! that this crate therefore has to own: total input bytes, page count, per-page
//! content-stream bytes, object nesting depth (screened before the file is
//! handed to the parser at all), and aggregate extracted text.
//!
//! Every constant here is pinned by a `limit` / `limit + 1` pair in
//! `super::tests`, so a silent widening fails the suite rather than the field.

/// Largest PDF this reader will look at. Bounds the copy `lopdf` makes of the
/// buffer as well as the pre-screen scan.
pub(super) const MAX_INPUT_BYTES: usize = 256 * 1024 * 1024;

/// Passed to `lopdf` as `LoadOptions::max_decompressed_size`. Sized to the
/// measured worst-case peak allocation recorded by the PDF dependency gate
/// (100,670,328 B at a 4 GiB cap), which is the property that selected `lopdf`
/// over the alternatives: it is the only candidate whose peak allocation
/// actually tracks the cap.
pub(super) const MAX_DECOMPRESSED_STREAM_BYTES: usize = 96 * 1024 * 1024;

/// Pages enumerated per document.
pub(super) const MAX_PAGES: usize = 4_096;

/// Decompressed content-stream bytes for a single page, passed to
/// `Document::get_page_content_with_limit`.
pub(super) const MAX_CONTENT_STREAM_BYTES: usize = 32 * 1024 * 1024;

/// Decompressed content-stream bytes summed across every page of a document.
/// `MAX_PAGES * MAX_CONTENT_STREAM_BYTES` is 128 TiB, so the per-page bound
/// alone is not a document bound.
pub(super) const MAX_TOTAL_CONTENT_STREAM_BYTES: usize = 128 * 1024 * 1024;

/// Maximum `<< >>` / `[ ]` nesting depth accepted by the pre-screen. PDF 2.0
/// only requires readers to support 28 levels of array/dictionary nesting;
/// 64 is generous and still far below any stack that a recursive walker of the
/// object graph would need.
pub(super) const MAX_OBJECT_NESTING_DEPTH: usize = 64;

/// Content-stream operators interpreted per page.
pub(super) const MAX_OPERATIONS_PER_PAGE: usize = 2_000_000;

/// Positioned text runs emitted per page.
pub(super) const MAX_RUNS_PER_PAGE: usize = 100_000;

/// Glyphs measured per page. A run holds glyphs, so this bounds the work done
/// inside a single enormous show operand as well as across many small ones.
pub(super) const MAX_GLYPHS_PER_PAGE: usize = 500_000;

/// `q` depth. A `q` past this is ignored with a notice rather than growing the
/// stack; `Q` underflow is likewise ignored with a notice.
pub(super) const MAX_GRAPHICS_STACK_DEPTH: usize = 64;

/// Distinct font resources resolved per page.
pub(super) const MAX_FONTS_PER_PAGE: usize = 256;

/// Bound handed to `Dictionary::get_font_encoding_with_limit` so a crafted
/// `/ToUnicode` CMap stream cannot inflate without limit.
pub(super) const MAX_TOUNICODE_BYTES: usize = 4 * 1024 * 1024;

/// Aggregate decoded text retained across the whole document, in UTF-8 bytes.
/// Matches the DOCX extractor's ceiling.
pub(super) const MAX_EXTRACTED_TEXT_BYTES: usize = 10 * 1024 * 1024;

/// Notices retained on a document. Matches the DOCX extractor's ceiling.
pub(super) const MAX_NOTICES: usize = 32;
