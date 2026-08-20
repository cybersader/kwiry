// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::extract::{ExtractedSource, ExtractionCoverage, ExtractionError, ExtractionNotice};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HtmlStage {
    Decode,
    Tokenize,
    Recover,
    Project,
}

impl HtmlStage {
    fn notice(self, coverage: ExtractionCoverage) -> ExtractionNotice {
        match (self, coverage) {
            (Self::Decode, ExtractionCoverage::Unreadable) => {
                ExtractionNotice::new("html_decode_unreadable", "HTML source is not valid UTF-8")
            }
            (Self::Tokenize, ExtractionCoverage::Unreadable) => ExtractionNotice::new(
                "html_tokenize_unreadable",
                "HTML tokenization could not recover safely",
            ),
            (Self::Recover, ExtractionCoverage::Unreadable) => ExtractionNotice::new(
                "html_recover_unreadable",
                "HTML tree recovery could not continue safely",
            ),
            (Self::Project, ExtractionCoverage::Unreadable) => ExtractionNotice::new(
                "html_project_unreadable",
                "HTML reader projection could not continue safely",
            ),
            (_, ExtractionCoverage::Quarantined) => ExtractionNotice::new(
                "html_budget_exceeded",
                "HTML extraction exceeded a mandatory budget",
            ),
            _ => unreachable!("HTML failures are unreadable or quarantined"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HtmlError {
    Unreadable(HtmlStage),
    Limit(HtmlStage),
}

impl HtmlError {
    pub(super) const fn unreadable(stage: HtmlStage) -> Self {
        Self::Unreadable(stage)
    }

    pub(super) const fn limit(stage: HtmlStage) -> Self {
        Self::Limit(stage)
    }

    pub(super) fn into_extraction(self) -> Result<ExtractedSource, ExtractionError> {
        match self {
            Self::Unreadable(stage) => Ok(ExtractedSource::skipped(
                ExtractionCoverage::Unreadable,
                stage.notice(ExtractionCoverage::Unreadable),
            )),
            Self::Limit(_) => Err(ExtractionError::limit(
                "HTML extraction exceeded a mandatory budget",
            )),
        }
    }
}
