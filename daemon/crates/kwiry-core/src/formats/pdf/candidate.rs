// SPDX-License-Identifier: MIT OR Apache-2.0

//! The PDF extraction candidate: per-page sections composed from geometry.
//!
//! # Admission status
//!
//! Admitted. `SourceFormat::Pdf` reports `extraction_supported() == true`,
//! discovery accepts `.pdf`, and `formats::pdf::extract` maps every
//! [`PdfSection`] below onto an `ExtractedSection` — content verbatim, empty
//! heading path, and [`PdfPageLocator::to_source_locator`] as the locator.
//! Everything here compiles in the `portable` feature set, so the daemon and
//! the plugin's WASM bundle produce the same sections for the same bytes.
//!
//! # Why the page is the outer section
//!
//! A PDF has no headings. It has pages, and a page is the only structural unit
//! the format guarantees, the only one a reader can navigate to, and the only
//! one whose boundary is not inferred. So the page is the mandatory outer
//! section: every section belongs to exactly one page, which is what makes
//! "a chunk never spans two pages" a property of the extraction rather than a
//! hope about the chunker. Preparation splits a section and never merges two,
//! so one section per page is sufficient to carry the invariant through.
//!
//! # Why the page is a locator and not a heading
//!
//! `heading_path` is a ranking field: it is joined into `heading_text`,
//! normalized, and matched against queries. Putting "Page 7" there would make
//! every page of every PDF a lexical match for the word *page* and would put a
//! number into the ranked text that the author never wrote. The page belongs in
//! the locator instead, which preparation carries as non-ranking metadata for
//! navigation. So the sections here have an empty heading path on purpose.

use crate::extract::{ExtractionCoverage, ExtractionNotice, SourceLocator};

use super::error::{PdfReadError, notice};
use super::layout;
use super::limits;
use super::{PdfDocumentGeometry, read_pdf_geometry};

use serde::{Deserialize, Serialize};

/// Where a section came from, for navigation only. Never ranked, never
/// tokenized, never part of the searchable text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfPageLocator {
    /// 1-based page number in `/Pages` order.
    pub page: u32,
}

impl PdfPageLocator {
    /// The preparation-level locator this page becomes.
    ///
    /// The mapping is code rather than prose so the receiving end is a fact the
    /// suite can check. `ExtractedSection::locator` is copied verbatim into
    /// `PreparedChunk::source_locator` — including into every chunk an
    /// oversized page splits into — so the page survives to navigation without
    /// ever entering ranked text.
    pub const fn to_source_locator(self) -> SourceLocator {
        SourceLocator::PdfPage { page: self.page }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfSection {
    /// Always empty in this wave: see the module documentation on why the page
    /// is a locator rather than a heading.
    pub heading_path: Vec<String>,
    pub content: String,
    pub locator: PdfPageLocator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfCandidate {
    pub page_count: usize,
    /// One section per page that produced text, in page order. A page with no
    /// text layer contributes no section and is declared through a notice.
    pub sections: Vec<PdfSection>,
    pub coverage: ExtractionCoverage,
    pub notices: Vec<ExtractionNotice>,
}

/// Compose a PDF's positioned text into per-page sections.
///
/// This is what `extract_source` reaches for `SourceFormat::Pdf`; the
/// `formats::pdf::extract` wrapper only adapts the outcome to
/// `ExtractedSource`, adding no text and dropping none.
pub fn extract_pdf_candidate(bytes: &[u8]) -> PdfCandidate {
    match read_pdf_geometry(bytes) {
        Ok(geometry) => compose(geometry),
        Err(error) => PdfCandidate {
            page_count: 0,
            sections: Vec::new(),
            // A document that declares itself encrypted is a document this
            // reader will not read, which is a different fact from a document
            // it tried to read and could not.
            coverage: match error {
                PdfReadError::EncryptedDocument => ExtractionCoverage::Unreadable,
                _ => ExtractionCoverage::Quarantined,
            },
            notices: vec![ExtractionNotice::new(
                error.notice_code(),
                error.to_string(),
            )],
        },
    }
}

fn compose(geometry: PdfDocumentGeometry) -> PdfCandidate {
    let PdfDocumentGeometry {
        page_count,
        pages,
        truncated,
        undecodable_font,
        mut notices,
    } = geometry;

    // Declined before a single section is composed. This is the whole-source
    // rule from `super::cmap`: a profile that cannot decode one of the fonts
    // must contribute no chunk identities at all rather than the subset of the
    // text it happens to be able to read. Partial text would change section
    // content, which changes chunk boundaries, which is exactly the
    // same-identity/different-content collision the profile gate exists to
    // stop. The enhanced profile indexes this source, and because this one
    // produced nothing, that switch is a pure insertion.
    if undecodable_font {
        push(
            &mut notices,
            notice::UNDECODABLE_FONT,
            "a font uses a predefined CJK CMap this extraction profile cannot decode",
        );
        return PdfCandidate {
            page_count,
            sections: Vec::new(),
            coverage: ExtractionCoverage::SkippedNoExtractableText,
            notices,
        };
    }

    let mut sections = Vec::new();
    let mut pages_without_text = 0usize;
    let mut unordered = 0usize;

    for page in &pages {
        let composed = layout::compose_page(page);
        unordered += composed.unordered;
        if composed.text.is_empty() {
            pages_without_text += 1;
            continue;
        }
        sections.push(PdfSection {
            heading_path: Vec::new(),
            content: composed.text,
            locator: PdfPageLocator {
                page: page.page_number,
            },
        });
    }

    if unordered > 0 {
        push(
            &mut notices,
            notice::UNORDERED_TEXT,
            "rotated or vertical text was kept but placed after the page's ordered text",
        );
    }

    // Ordering matters below: "no text anywhere" and "text on some pages only"
    // are different facts and get different outcomes. Neither one invents text.
    if sections.is_empty() {
        let (code, message, coverage) = if truncated {
            (
                notice::NO_READABLE_PAGE,
                "every page failed a budget or could not be interpreted",
                ExtractionCoverage::Quarantined,
            )
        } else {
            (
                notice::NO_TEXT_LAYER,
                "the document carries no text layer on any page",
                ExtractionCoverage::SkippedNoExtractableText,
            )
        };
        push(&mut notices, code, message);
        return PdfCandidate {
            page_count,
            sections,
            coverage,
            notices,
        };
    }

    if pages_without_text > 0 {
        push(
            &mut notices,
            notice::PAGE_WITHOUT_TEXT_LAYER,
            "at least one page carries no text layer and contributed no section",
        );
    }

    // Partial is the honest outcome whenever any page fell short, and a notice
    // is what makes the shortfall declared rather than merely true.
    let coverage = if truncated || pages_without_text > 0 || !notices.is_empty() {
        ExtractionCoverage::IndexedPartial
    } else {
        ExtractionCoverage::IndexedComplete
    };

    PdfCandidate {
        page_count,
        sections,
        coverage,
        notices,
    }
}

fn push(notices: &mut Vec<ExtractionNotice>, code: &'static str, message: &'static str) {
    if notices.iter().any(|notice| notice.code == code) {
        return;
    }
    if notices.len() < limits::MAX_NOTICES - 1 {
        notices.push(ExtractionNotice::new(code, message));
    } else if notices.len() == limits::MAX_NOTICES - 1 {
        notices.push(ExtractionNotice::new(
            notice::NOTICES_TRUNCATED,
            "additional PDF extraction notices were omitted",
        ));
    }
}
