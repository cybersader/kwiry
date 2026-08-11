// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::extract::{
    ContentRole, ExtractedSection, ExtractedSource, ExtractionCompleteness, ExtractionError,
};

use super::decode_utf8;

pub(super) fn extract(bytes: &[u8]) -> Result<ExtractedSource, ExtractionError> {
    let source = match decode_utf8(bytes) {
        Ok(source) => source,
        Err(notice) => {
            return Ok(ExtractedSource::skipped(
                crate::extract::ExtractionCoverage::Unreadable,
                notice,
            ));
        }
    };
    Ok(ExtractedSource::indexed(
        Default::default(),
        Default::default(),
        Vec::new(),
        Vec::new(),
        vec![ExtractedSection {
            heading_path: Vec::new(),
            content: source.to_owned(),
            role: ContentRole::Primary,
            locator: None,
        }],
        ExtractionCompleteness::Complete,
        Vec::new(),
    ))
}
