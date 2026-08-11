// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};

use crate::model::{Frontmatter, MAX_FILE_BYTES, PropertyBag};

pub(crate) const MAX_EXTRACTED_SECTIONS_PER_SOURCE: usize = 100_000;
pub(crate) const MAX_EXTRACTED_HEADING_BYTES_PER_SOURCE: usize = MAX_FILE_BYTES as usize;

#[derive(Debug, Default)]
pub(crate) struct ExtractionBudget {
    sections: usize,
    heading_bytes: usize,
}

impl ExtractionBudget {
    pub(crate) fn reserve_section(
        &mut self,
        heading_path: &[String],
    ) -> Result<(), ExtractionError> {
        if self.sections == MAX_EXTRACTED_SECTIONS_PER_SOURCE {
            return Err(ExtractionError::limit(
                "prepared source exceeds the chunk inventory limit",
            ));
        }
        let heading_bytes = heading_path
            .iter()
            .try_fold(0_usize, |total, heading| total.checked_add(heading.len()));
        self.heading_bytes = heading_bytes
            .and_then(|heading_bytes| self.heading_bytes.checked_add(heading_bytes))
            .filter(|total| *total <= MAX_EXTRACTED_HEADING_BYTES_PER_SOURCE)
            .ok_or_else(|| {
                ExtractionError::limit("prepared source exceeds the heading-path memory limit")
            })?;
        self.sections += 1;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExtractionCoverage {
    IndexedComplete,
    IndexedPartial,
    SkippedNoExtractableText,
    Unreadable,
    Quarantined,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionCompleteness {
    Complete,
    Partial,
}

impl ExtractionCoverage {
    pub const fn indexed(completeness: ExtractionCompleteness) -> Self {
        match completeness {
            ExtractionCompleteness::Complete => Self::IndexedComplete,
            ExtractionCompleteness::Partial => Self::IndexedPartial,
        }
    }

    pub const fn completeness(self) -> Option<ExtractionCompleteness> {
        match self {
            Self::IndexedComplete => Some(ExtractionCompleteness::Complete),
            Self::IndexedPartial => Some(ExtractionCompleteness::Partial),
            Self::SkippedNoExtractableText | Self::Unreadable | Self::Quarantined => None,
        }
    }

    pub const fn is_indexed(self) -> bool {
        self.completeness().is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceLocator {
    BaseView {
        view: String,
    },
    /// 1-based page in `/Pages` order.
    ///
    /// The PDF candidate rests its whole "the page is a locator, never a
    /// heading" argument on this variant existing: `heading_path` is joined into
    /// `heading_text`, normalized and matched against queries, so putting
    /// "Page 7" there would make every page of every PDF a lexical match for
    /// the word *page*. Without a locator to carry it instead, the page number
    /// was simply discarded and the justification for the empty heading path
    /// had no other half.
    ///
    /// Produced by `formats::pdf::extract` for every indexed page and copied
    /// verbatim into `PreparedChunk::source_locator` by the chunk-split loop,
    /// so all chunks of one page report the same page. It is stored and never
    /// tokenized: `STORED`-only in the Tantivy schema and a plain `locator_json`
    /// column in FTS5, so it cannot reach a ranking decision.
    PdfPage {
        page: u32,
    },
    /// Stored-only sheet and A1 cell coordinate for an Excel section.
    ExcelCell {
        sheet: String,
        cell: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtractionNotice {
    pub code: String,
    pub message: String,
}

impl ExtractionNotice {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ContentRole {
    #[default]
    Primary,
    Supporting,
    Latent,
}

impl ContentRole {
    pub const fn is_primary(&self) -> bool {
        matches!(self, Self::Primary)
    }
}

// A role never transforms a score. Contract §10.5 makes text evidence supreme
// across every format — weaker evidence may never outrank stronger — so a
// class band like primary=[2,3) would both demote a strong Excel match below
// any mid-strength Markdown match and promote a weak Excel match above one,
// in the same clamp. Amendment 10b asks only that non-primary content be
// "searchable, never boosted": it takes no boost fields at preparation, and
// it loses ties to primary content through the role byte that leads the
// chunk ID, which both engines already order ascending after score.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSection {
    pub heading_path: Vec<String>,
    pub content: String,
    pub role: ContentRole,
    pub locator: Option<SourceLocator>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSource {
    pub properties: PropertyBag,
    pub frontmatter: Frontmatter,
    pub aliases: Vec<String>,
    pub links_out: Vec<String>,
    pub sections: Vec<ExtractedSection>,
    pub coverage: ExtractionCoverage,
    pub notices: Vec<ExtractionNotice>,
}

impl ExtractedSource {
    pub(crate) fn indexed(
        properties: PropertyBag,
        frontmatter: Frontmatter,
        aliases: Vec<String>,
        links_out: Vec<String>,
        sections: Vec<ExtractedSection>,
        completeness: ExtractionCompleteness,
        notices: Vec<ExtractionNotice>,
    ) -> Self {
        Self {
            properties,
            frontmatter,
            aliases,
            links_out,
            sections,
            coverage: ExtractionCoverage::indexed(completeness),
            notices,
        }
    }

    pub(crate) fn skipped(coverage: ExtractionCoverage, notice: ExtractionNotice) -> Self {
        debug_assert!(!coverage.is_indexed());
        Self {
            properties: PropertyBag::default(),
            frontmatter: Frontmatter::default(),
            aliases: Vec::new(),
            links_out: Vec::new(),
            sections: Vec::new(),
            coverage,
            notices: vec![notice],
        }
    }

    pub(crate) fn warning(&self) -> Option<String> {
        match self.notices.as_slice() {
            [] => None,
            [notice] => Some(notice.message.clone()),
            notices => Some(
                notices
                    .iter()
                    .map(|notice| notice.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ExtractionError {
    pub code: String,
    pub message: String,
}

impl ExtractionError {
    pub(crate) fn limit(message: impl Into<String>) -> Self {
        Self {
            code: "index_limit_exceeded".to_owned(),
            message: message.into(),
        }
    }
}
