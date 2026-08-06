// SPDX-License-Identifier: MIT OR Apache-2.0

/// Document-level refusals from the admission-disabled PDF reader.
///
/// These are the conditions under which no geometry is produced at all. A
/// *page*-level budget exhaustion is not an error: it truncates that page,
/// records a notice, and leaves the rest of the document readable. See
/// [`super::PdfPageGeometry::truncated`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PdfReadError {
    #[error("PDF input exceeds the accepted byte budget")]
    InputTooLarge,
    #[error("input is not a PDF")]
    NotAPdf,
    #[error("PDF object nesting exceeds the accepted depth")]
    ObjectNestingTooDeep,
    #[error("PDF is malformed or unreadable")]
    InvalidDocument,
    #[error("PDF is encrypted")]
    EncryptedDocument,
    #[error("PDF exceeds the accepted page budget")]
    PageLimitExceeded,
}

impl PdfReadError {
    /// Stable notice code. Mirrors `DocxError::notice_code` so a later
    /// admission wave can surface these through `ExtractionNotice` unchanged.
    pub const fn notice_code(self) -> &'static str {
        match self {
            Self::InputTooLarge => "pdf_input_too_large",
            Self::NotAPdf => "pdf_not_a_pdf",
            Self::ObjectNestingTooDeep => "pdf_object_nesting_too_deep",
            Self::InvalidDocument => "pdf_invalid_document",
            Self::EncryptedDocument => "pdf_encrypted_document",
            Self::PageLimitExceeded => "pdf_page_limit_exceeded",
        }
    }
}

/// Notice codes emitted for recoverable conditions. Kept as constants so tests
/// assert against the same strings the reader emits.
pub(super) mod notice {
    pub(in crate::formats::pdf) const PAGE_CONTENT_LIMIT: &str = "pdf_page_content_limit_exceeded";
    pub(in crate::formats::pdf) const DOCUMENT_CONTENT_LIMIT: &str =
        "pdf_document_content_limit_exceeded";
    pub(in crate::formats::pdf) const OPERATION_LIMIT: &str = "pdf_page_operation_limit_exceeded";
    pub(in crate::formats::pdf) const RUN_LIMIT: &str = "pdf_page_run_limit_exceeded";
    pub(in crate::formats::pdf) const GLYPH_LIMIT: &str = "pdf_page_glyph_limit_exceeded";
    pub(in crate::formats::pdf) const TEXT_LIMIT: &str = "pdf_extracted_text_limit_exceeded";
    pub(in crate::formats::pdf) const FONT_LIMIT: &str = "pdf_page_font_limit_exceeded";
    pub(in crate::formats::pdf) const GRAPHICS_STACK_LIMIT: &str = "pdf_graphics_stack_limit";
    pub(in crate::formats::pdf) const GRAPHICS_STACK_UNDERFLOW: &str =
        "pdf_graphics_stack_underflow";
    pub(in crate::formats::pdf) const CONTENT_UNPARSABLE: &str = "pdf_page_content_unparsable";
    pub(in crate::formats::pdf) const PAGE_GEOMETRY_INVALID: &str = "pdf_page_geometry_invalid";
    pub(in crate::formats::pdf) const PAGE_ROTATION_INVALID: &str = "pdf_page_rotation_invalid";
    pub(in crate::formats::pdf) const NESTED_TEXT_OBJECT: &str = "pdf_nested_text_object";
    pub(in crate::formats::pdf) const FONT_UNRESOLVED: &str = "pdf_font_unresolved";
    pub(in crate::formats::pdf) const VERTICAL_WRITING: &str = "pdf_vertical_writing_unsupported";
    pub(in crate::formats::pdf) const NON_INVERTIBLE_MATRIX: &str = "pdf_non_invertible_matrix";
}
