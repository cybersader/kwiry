// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::extract::ExtractedSource;
use crate::format::SourceFormat;

use super::not_yet_supported;

pub(super) fn extract(_bytes: &[u8]) -> ExtractedSource {
    not_yet_supported(SourceFormat::Docx)
}
