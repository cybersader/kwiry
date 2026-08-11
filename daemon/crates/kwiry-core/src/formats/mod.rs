// SPDX-License-Identifier: MIT OR Apache-2.0

mod base;
mod canvas;
mod docx;
mod excalidraw;
pub(crate) mod excel;
mod markdown;
mod ooxml;
mod pdf;
mod text;

use crate::extract::{ExtractedSource, ExtractionError, ExtractionNotice};
use crate::format::SourceFormat;

#[cfg(feature = "internal-docx-extractor")]
pub use docx::{
    ContentRole, DocxCandidate, DocxProperties, ExtractionScope, SemanticSection,
    extract_candidate_outcome,
};

#[cfg(feature = "internal-excel-extractor")]
#[allow(unused_imports)]
pub use excel::{ExcelCandidate, ExcelCellLocator, ExcelSection, extract_excel_candidate_outcome};

// The PDF reader's own vocabulary — geometry, limits, and the page candidate.
// PDF is admitted, so `extract_source` below composes an `ExtractedSource` from
// `extract_pdf_candidate`. These are exported unconditionally now that the
// reader is part of `portable`: gating them on `internal-pdf-extractor` would
// leave the shipped portable build compiling a reader whose entry points
// nothing names, which is a dead-code warning rather than a boundary.
pub use pdf::{
    PdfCandidate, PdfDocumentGeometry, PdfLimits, PdfPageGeometry, PdfPageLocator, PdfReadError,
    PdfSection, PdfTextRun, PdfWritingMode, extract_pdf_candidate, pdf_limits, read_pdf_geometry,
};

#[cfg(feature = "internal-excalidraw-extractor")]
pub use excalidraw::{
    MAX_EXCALIDRAW_NOTICES, MAX_EXCALIDRAW_PROPERTY_BYTES, MAX_EXCALIDRAW_PROPERTY_ENTRIES,
    extract_excalidraw_candidate,
};

pub fn extract_source(
    format: SourceFormat,
    bytes: &[u8],
) -> Result<ExtractedSource, ExtractionError> {
    match format {
        SourceFormat::Markdown => markdown::extract(bytes),
        SourceFormat::Text => text::extract(bytes),
        SourceFormat::Base => base::extract(bytes),
        SourceFormat::Canvas => canvas::extract(bytes),
        SourceFormat::Excalidraw => excalidraw::extract_excalidraw_candidate(bytes),
        SourceFormat::Docx => Ok(docx::extract(bytes)),
        SourceFormat::Pdf => Ok(pdf::extract(bytes)),
        SourceFormat::Excel => Ok(excel::extract(bytes)),
    }
}

fn decode_utf8(bytes: &[u8]) -> Result<&str, ExtractionNotice> {
    let source = std::str::from_utf8(bytes).map_err(|error| {
        ExtractionNotice::new(
            "non_utf8_source",
            format!("skipped non-UTF-8 file: {error}"),
        )
    })?;
    if source.contains('\0') {
        return Err(ExtractionNotice::new(
            "binary_source",
            "skipped binary file containing NUL bytes",
        ));
    }
    Ok(source)
}

// PDF was the last format routed to a `not_yet_supported` stub. Every member of
// the closed set now has a real extractor, so the stub and its
// `format_not_yet_supported` notice code are gone rather than kept as an
// unreachable branch that a future reader would mistake for a live outcome.
