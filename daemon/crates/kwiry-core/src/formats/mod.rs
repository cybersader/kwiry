// SPDX-License-Identifier: MIT OR Apache-2.0

mod base;
mod canvas;
mod docx;
mod excalidraw;
mod markdown;
mod pdf;
mod text;

use crate::extract::{ExtractedSource, ExtractionCoverage, ExtractionError, ExtractionNotice};
use crate::format::SourceFormat;

#[cfg(feature = "internal-docx-extractor")]
pub use docx::{
    ContentRole, DocxCandidate, DocxProperties, ExtractionScope, SemanticSection,
    extract_candidate_outcome,
};

// Admission-disabled PDF reader foundation; see `formats::pdf`. Exposing the
// entry point does not admit the format: `SourceFormat::Pdf` still reports
// `extraction_supported() == false` and the `extract_source` arm below still
// routes PDF to the `not_yet_supported` stub.
#[cfg(feature = "internal-pdf-extractor")]
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

fn not_yet_supported(format: SourceFormat) -> ExtractedSource {
    ExtractedSource::skipped(
        ExtractionCoverage::SkippedNoExtractableText,
        ExtractionNotice::new(
            "format_not_yet_supported",
            format!(
                "{} extraction is not yet supported; skipped with no extractable text",
                format.as_str()
            ),
        ),
    )
}
