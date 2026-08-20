// SPDX-License-Identifier: MIT OR Apache-2.0

mod entities;
mod error;
mod limits;
mod model;
mod project;
mod recovery;
mod tokenizer;

#[cfg(test)]
mod tests;

use crate::extract::{ExtractedSource, ExtractionError};

use error::{HtmlError, HtmlStage};
use limits::{Budget, HtmlLimits};

pub(super) fn extract(bytes: &[u8]) -> Result<ExtractedSource, ExtractionError> {
    extract_with_limits(bytes, &HtmlLimits::default())
}

fn extract_with_limits(
    bytes: &[u8],
    limits: &HtmlLimits,
) -> Result<ExtractedSource, ExtractionError> {
    if bytes.len() > limits.source_bytes || bytes.len() > limits.decoded_bytes {
        return HtmlError::limit(HtmlStage::Decode).into_extraction();
    }
    let source = match std::str::from_utf8(bytes) {
        Ok(source) => source.strip_prefix('\u{FEFF}').unwrap_or(source),
        Err(_) => return HtmlError::unreadable(HtmlStage::Decode).into_extraction(),
    };
    let mut budget = Budget::default();
    let document = match recovery::recover(source, &mut budget, limits) {
        Ok(document) => document,
        Err(error) => return error.into_extraction(),
    };
    match project::project(document, &mut budget, limits) {
        Ok(source) => Ok(source),
        Err(error) => error.into_extraction(),
    }
}
