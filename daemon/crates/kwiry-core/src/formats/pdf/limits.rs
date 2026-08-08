// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project-owned budgets for the PDF reader.
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

/// Passed to `lopdf` as `LoadOptions::max_decompressed_size`.
///
/// # This bounds decompressed bytes, not peak allocation
///
/// The dependency gate recorded 100,670,328 B of peak allocation at a 4 GiB cap
/// and concluded that `lopdf`'s peak "tracks the cap". That holds for the
/// *streams* the gate measured; it does not hold for the **object graph**.
/// `Document::load_mem_with_options` parses every object eagerly, and a
/// decompressed array of numbers becomes one `Object` per element:
///
/// | decompressed `/W` array | file | peak RSS |
/// |---|---|---|
/// |  6 MiB | 1,011,770 B |   215 MB |
/// | 12 MiB | 2,033,062 B |   427 MB |
/// | 24 MiB | 4,061,256 B |   789 MB |
///
/// Linear at roughly 33 bytes of heap per decompressed byte, so this cap admits
/// about 3.2 GB of peak allocation from a ~16 MB file. That is upstream of
/// every budget in this file — it happens before any code here runs — and the
/// per-font [`MAX_FONT_WIDTH_ENTRIES`] cap bounds only this crate's own copy.
///
/// Lowering the cap is the lever that would bound it, and lowering it changes
/// which real documents are readable, so the value is left where the owner set
/// it and the discrepancy is recorded here rather than in a claim that the
/// measurement does not support.
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

/// Content-stream bytes decoded into `Vec<Operation>` at one time.
///
/// `MAX_OPERATIONS_PER_PAGE` bounds interpretation, not allocation:
/// [`lopdf::content::Content::decode`] materializes the whole page before the
/// first operator is seen. Measured peak allocation is about 285 bytes of heap
/// per content byte, so a page decoded in one shot at
/// [`MAX_CONTENT_STREAM_BYTES`] peaks near 9.5 GB — unallocatable in an
/// Obsidian worker, and an abort rather than an unwind. Decoding a window at a
/// time puts the ceiling here instead: 64 KiB × ~285 ≈ 18 MiB.
///
/// This is a floor on the window, never a ceiling on an operation: one
/// operation larger than the window is decoded whole rather than split, because
/// splitting it would change what the file says. See `super::split`.
pub(super) const MAX_CONTENT_WINDOW_BYTES: usize = 64 * 1024;

/// Operands materialized for one operation, counting nested array and
/// dictionary members.
///
/// An operation is never split, so the window budget above cannot bound one
/// enormous operation: a 12 MB `TJ` array of six million numbers decodes to
/// 768 MB whatever the window is, and a compressed one reaches that from a few
/// kilobytes of file. The byte span is the wrong instrument — a one-megabyte
/// literal string is a single cheap operand — so the count is bounded instead.
/// A `TJ` array at this cap already holds far more elements than
/// [`MAX_GLYPHS_PER_PAGE`] admits glyphs, so no page that this reader would
/// have measured in full is affected.
pub(super) const MAX_OPERANDS_PER_OPERATION: usize = 262_144;

/// Positioned text runs emitted per page.
pub(super) const MAX_RUNS_PER_PAGE: usize = 100_000;

/// Positioned text runs retained across the whole document.
///
/// # Why the per-page bound is not a document bound
///
/// The reader builds one [`PdfDocumentGeometry`](super::PdfDocumentGeometry)
/// holding *every* page's runs at once, and nothing is released until
/// `candidate::compose` has run. `MAX_PAGES * MAX_RUNS_PER_PAGE` is 409.6
/// million runs, so the per-page bound alone bounds nothing — the same reasoning
/// [`MAX_TOTAL_CONTENT_STREAM_BYTES`] already records for content bytes, which
/// was never applied to runs.
///
/// Neither existing document-wide budget bounds them:
///
/// * [`MAX_EXTRACTED_TEXT_BYTES`] charges only *decoded* bytes. A show operator
///   whose codes map to nothing — `()Tj`, or any font whose encoding drops the
///   code — costs zero text budget and still pushes a full run, because
///   `content::show` pushes unconditionally and empty runs are only discarded
///   later, in `layout::collect`.
/// * [`MAX_TOTAL_CONTENT_STREAM_BYTES`] costs about 5 bytes per `()Tj`, so
///   128 MiB of content admits roughly 26.8 million runs.
///
/// Measured on the admitted `prepare_source_buffer` path, a `()Tj` document
/// reached 403 MB of peak RSS from 4,263 bytes (20 pages, 2 million runs) and
/// 1.97 GB from 16,597 bytes (100 pages, 10 million runs) — about 197 bytes of
/// heap per retained run, linear, and reported as a benign "no text layer" skip
/// with nothing in the coverage vocabulary recording the cost. A 47,797-byte
/// file reached 5.25 GB, and a 63,397-byte one aborted under a 4 GiB cap. Both
/// hosts cap a source at 10 MiB, so the size gate never sees any of them.
///
/// # Why 2,000,000
///
/// The value is set where this budget and [`MAX_EXTRACTED_TEXT_BYTES`] bite
/// together rather than one masking the other. A word-granular generator emits
/// roughly 5 to 6 text bytes per run, so 10 MiB of text is about 2 million runs:
/// below this cap the text budget is always the first to stop a document that is
/// actually carrying text, and this cap only bites on documents whose runs are
/// mostly empty — which is exactly the shape that has no legitimate reading.
/// Line-granular real documents are nowhere near it: 4,096 pages at 488 runs per
/// page is the break-even, and real prose runs 50 to 400.
///
/// Retained-run memory is therefore bounded at about 400 MB rather than at the
/// 5.25 GB above, and any shortfall is declared through
/// `pdf_document_run_limit_exceeded` instead of being reported as a clean skip.
pub(super) const MAX_TOTAL_RUNS: usize = 2_000_000;

/// Glyphs measured per page. A run holds glyphs, so this bounds the work done
/// inside a single enormous show operand as well as across many small ones.
pub(super) const MAX_GLYPHS_PER_PAGE: usize = 500_000;

/// `q` depth. A `q` past this is ignored with a notice rather than growing the
/// stack; `Q` underflow is likewise ignored with a notice.
pub(super) const MAX_GRAPHICS_STACK_DEPTH: usize = 64;

/// Distinct font resources resolved per page.
pub(super) const MAX_FONTS_PER_PAGE: usize = 256;

/// Width entries retained per font: `/Widths` slots for a simple font, `/W`
/// ranges for a composite one.
///
/// Neither array had a budget of its own, and `MAX_FONTS_PER_PAGE` multiplies
/// whatever one font costs: a 3.5 MB file carrying 1.5 million `/W` entries
/// inside a compressed object stream reached 1.16 GB and reported
/// `IndexedComplete` with no notice.
///
/// The cap is lossless on every well-formed font. A simple font's codes are
/// single bytes, so at most 256 `/Widths` slots are reachable; a composite
/// font's CIDs come from two-byte codes, so at most 65,536 `/W` ranges are.
/// Entries past the cap fall back to `/MissingWidth` or `/DW` — the existing
/// "not measured" path, which already clears `geometry_exact` — and the
/// shortfall is declared through `pdf_font_width_limit_exceeded`.
pub(super) const MAX_FONT_WIDTH_ENTRIES: usize = 65_536;

/// Bound handed to `Dictionary::get_font_encoding_with_limit` so a crafted
/// `/ToUnicode` CMap stream cannot inflate without limit.
pub(super) const MAX_TOUNICODE_BYTES: usize = 4 * 1024 * 1024;

/// Aggregate decoded text retained across the whole document, in UTF-8 bytes.
/// Matches the DOCX extractor's ceiling.
pub(super) const MAX_EXTRACTED_TEXT_BYTES: usize = 10 * 1024 * 1024;

/// Enhanced tier only. Decompressed bytes of one embedded font program, read by the enhanced tier
/// to recover Unicode for an `Identity-H` subset with no `/ToUnicode`. A font
/// file is small next to a document; this is generous for a full CJK face and
/// still nowhere near the content-stream budget.
#[cfg(feature = "native-pdf-extractor")]
pub(super) const MAX_EMBEDDED_FONT_BYTES: usize = 16 * 1024 * 1024;

/// Glyph-id to character entries retained per embedded font. Above a full
/// Unicode BMP face.
///
/// This bounds the map, which is not the same as bounding the walk that fills
/// it — see `super::embedded` for the crafted `cmap` that made the difference
/// an hours-long hang.
#[cfg(feature = "native-pdf-extractor")]
pub(super) const MAX_GLYPH_MAP_ENTRIES: usize = 200_000;

/// Unicode `cmap` subtables inverted per embedded font. A real face ships one
/// to three; the `cmap` header admits 65,535, and each one costs a full walk of
/// the Unicode scalar range.
#[cfg(feature = "native-pdf-extractor")]
pub(super) const MAX_GLYPH_MAP_SUBTABLES: usize = 8;

/// Notices retained on a document. Matches the DOCX extractor's ceiling.
pub(super) const MAX_NOTICES: usize = 32;
