// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};

use crate::model::{Frontmatter, MAX_FILE_BYTES, PropertyBag};

pub(crate) const MAX_EXTRACTED_SECTIONS_PER_SOURCE: usize = 100_000;
pub(crate) const MAX_EXTRACTED_HEADING_BYTES_PER_SOURCE: usize = MAX_FILE_BYTES as usize;

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
    BaseView { view: String },
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSection {
    pub heading_path: Vec<String>,
    pub content: String,
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
