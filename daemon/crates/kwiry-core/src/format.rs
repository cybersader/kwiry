// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SourceFormat {
    Markdown,
    Text,
    Base,
    Canvas,
    Docx,
    Pdf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatSpec {
    pub format: SourceFormat,
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub extraction_supported: bool,
}

const FORMAT_SPECS: &[FormatSpec] = &[
    FormatSpec {
        format: SourceFormat::Markdown,
        name: "markdown",
        extensions: &["md", "markdown", "mdx"],
        extraction_supported: true,
    },
    FormatSpec {
        format: SourceFormat::Text,
        name: "text",
        extensions: &["txt"],
        extraction_supported: true,
    },
    FormatSpec {
        format: SourceFormat::Base,
        name: "base",
        extensions: &["base"],
        extraction_supported: true,
    },
    FormatSpec {
        format: SourceFormat::Canvas,
        name: "canvas",
        extensions: &["canvas"],
        extraction_supported: true,
    },
    FormatSpec {
        format: SourceFormat::Docx,
        name: "docx",
        extensions: &["docx"],
        extraction_supported: true,
    },
    FormatSpec {
        format: SourceFormat::Pdf,
        name: "pdf",
        extensions: &["pdf"],
        extraction_supported: false,
    },
];

impl SourceFormat {
    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        let extension = path.as_ref().extension()?.to_str()?;
        Self::from_extension(extension)
    }

    pub fn from_extension(extension: &str) -> Option<Self> {
        FORMAT_SPECS
            .iter()
            .find(|spec| {
                spec.extensions
                    .iter()
                    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
            })
            .map(|spec| spec.format)
    }

    pub fn spec(self) -> &'static FormatSpec {
        FORMAT_SPECS
            .iter()
            .find(|spec| spec.format == self)
            .expect("every source format has a registry entry")
    }

    pub fn is_extractable(self) -> bool {
        self.spec().extraction_supported
    }

    pub fn from_extractable_path(path: impl AsRef<Path>) -> Option<Self> {
        Self::from_path(path).filter(|format| format.is_extractable())
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Text => "text",
            Self::Base => "base",
            Self::Canvas => "canvas",
            Self::Docx => "docx",
            Self::Pdf => "pdf",
        }
    }
}

pub const fn format_specs() -> &'static [FormatSpec] {
    FORMAT_SPECS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_closed_complete_and_classifies_case_insensitively() {
        assert_eq!(format_specs().len(), 6);
        assert_eq!(
            SourceFormat::from_path("note.md"),
            Some(SourceFormat::Markdown)
        );
        assert_eq!(
            SourceFormat::from_path("note.MDX"),
            Some(SourceFormat::Markdown)
        );
        assert_eq!(
            SourceFormat::from_path("notes.TXT"),
            Some(SourceFormat::Text)
        );
        assert_eq!(
            SourceFormat::from_path("dashboard.base"),
            Some(SourceFormat::Base)
        );
        assert_eq!(
            SourceFormat::from_path("board.canvas"),
            Some(SourceFormat::Canvas)
        );
        assert_eq!(
            SourceFormat::from_path("report.docx"),
            Some(SourceFormat::Docx)
        );
        assert_eq!(
            SourceFormat::from_path("paper.PDF"),
            Some(SourceFormat::Pdf)
        );
        assert_eq!(SourceFormat::from_path("image.png"), None);
        assert!(SourceFormat::Canvas.spec().extraction_supported);
        assert!(SourceFormat::Docx.spec().extraction_supported);
        assert!(!SourceFormat::Pdf.spec().extraction_supported);
        assert!(SourceFormat::Canvas.is_extractable());
        assert!(SourceFormat::Docx.is_extractable());
        assert!(!SourceFormat::Pdf.is_extractable());
        assert_eq!(
            SourceFormat::from_extractable_path("board.canvas"),
            Some(SourceFormat::Canvas)
        );
        assert_eq!(
            SourceFormat::from_extractable_path("report.docx"),
            Some(SourceFormat::Docx)
        );
        assert_eq!(SourceFormat::from_extractable_path("paper.PDF"), None);

        for spec in format_specs() {
            assert_eq!(spec.format.spec(), spec);
            assert!(!spec.extensions.is_empty());
        }
    }
}
