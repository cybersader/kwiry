// SPDX-License-Identifier: MIT OR Apache-2.0

mod base;
mod canvas;
mod docx;
mod markdown;
mod pdf;
mod text;

use crate::extract::{ExtractedSource, ExtractionCoverage, ExtractionError, ExtractionNotice};
use crate::format::SourceFormat;

pub fn extract_source(
    format: SourceFormat,
    bytes: &[u8],
) -> Result<ExtractedSource, ExtractionError> {
    match format {
        SourceFormat::Markdown => markdown::extract(bytes),
        SourceFormat::Text => text::extract(bytes),
        SourceFormat::Base => base::extract(bytes),
        SourceFormat::Canvas => Ok(canvas::extract(bytes)),
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
