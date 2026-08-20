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
    Excalidraw,
    Excel,
    Html,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FormatPolicy {
    pub role_tagged_chunk_ids: bool,
    pub suppress_non_primary_boosts: bool,
    pub metadata_only_carrier: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatSpec {
    pub format: SourceFormat,
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub extraction_supported: bool,
    pub policy: FormatPolicy,
    /// Whether a matched heading of this format resolves as an Obsidian link
    /// subpath. Every vault file can be linked to by name, but only Markdown
    /// headings are real anchors: a heading extracted from a PDF page, a DOCX
    /// outline, or a workbook sheet names a region of the extraction, not a
    /// destination a `#` link can reach. Clients must read this rather than
    /// testing for one format name, so admitting a format decides its own
    /// link behaviour here instead of in every caller.
    pub section_link_supported: bool,
}

const FORMAT_SPECS: &[FormatSpec] = &[
    FormatSpec {
        format: SourceFormat::Markdown,
        name: "markdown",
        extensions: &["md", "markdown", "mdx"],
        extraction_supported: true,
        policy: FormatPolicy {
            role_tagged_chunk_ids: false,
            suppress_non_primary_boosts: false,
            metadata_only_carrier: false,
        },
        section_link_supported: true,
    },
    FormatSpec {
        format: SourceFormat::Text,
        name: "text",
        extensions: &["txt"],
        extraction_supported: true,
        policy: FormatPolicy {
            role_tagged_chunk_ids: false,
            suppress_non_primary_boosts: false,
            metadata_only_carrier: false,
        },
        section_link_supported: false,
    },
    FormatSpec {
        format: SourceFormat::Base,
        name: "base",
        extensions: &["base"],
        extraction_supported: true,
        policy: FormatPolicy {
            role_tagged_chunk_ids: false,
            suppress_non_primary_boosts: false,
            metadata_only_carrier: false,
        },
        section_link_supported: false,
    },
    FormatSpec {
        format: SourceFormat::Canvas,
        name: "canvas",
        extensions: &["canvas"],
        extraction_supported: true,
        policy: FormatPolicy {
            role_tagged_chunk_ids: false,
            suppress_non_primary_boosts: false,
            metadata_only_carrier: false,
        },
        section_link_supported: false,
    },
    FormatSpec {
        format: SourceFormat::Excalidraw,
        name: "excalidraw",
        extensions: &["excalidraw"],
        extraction_supported: true,
        policy: FormatPolicy {
            role_tagged_chunk_ids: false,
            suppress_non_primary_boosts: false,
            metadata_only_carrier: false,
        },
        section_link_supported: false,
    },
    FormatSpec {
        format: SourceFormat::Docx,
        name: "docx",
        extensions: &["docx"],
        extraction_supported: true,
        policy: FormatPolicy {
            role_tagged_chunk_ids: false,
            suppress_non_primary_boosts: false,
            metadata_only_carrier: false,
        },
        section_link_supported: false,
    },
    FormatSpec {
        format: SourceFormat::Pdf,
        name: "pdf",
        extensions: &["pdf"],
        extraction_supported: true,
        policy: FormatPolicy {
            role_tagged_chunk_ids: false,
            suppress_non_primary_boosts: false,
            metadata_only_carrier: false,
        },
        section_link_supported: false,
    },
    FormatSpec {
        format: SourceFormat::Excel,
        name: "excel",
        extensions: &["xlsx", "xlsm"],
        extraction_supported: true,
        policy: FormatPolicy {
            role_tagged_chunk_ids: true,
            suppress_non_primary_boosts: true,
            metadata_only_carrier: false,
        },
        section_link_supported: false,
    },
    FormatSpec {
        format: SourceFormat::Html,
        name: "html",
        extensions: &["html", "htm"],
        extraction_supported: true,
        policy: FormatPolicy {
            role_tagged_chunk_ids: true,
            suppress_non_primary_boosts: true,
            metadata_only_carrier: true,
        },
        section_link_supported: false,
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

    /// Whether a matched heading of this format can be linked to directly.
    ///
    /// Note-level linking is not gated: every file admitted to an index lives
    /// in the vault and can be linked by name regardless of format.
    pub fn supports_section_links(self) -> bool {
        self.spec().section_link_supported
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
            Self::Excalidraw => "excalidraw",
            Self::Excel => "excel",
            Self::Docx => "docx",
            Self::Pdf => "pdf",
            Self::Html => "html",
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
        assert_eq!(format_specs().len(), 9);
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
        assert_eq!(
            SourceFormat::from_path("book.XLSX"),
            Some(SourceFormat::Excel)
        );
        assert_eq!(
            SourceFormat::from_path("macros.xlsm"),
            Some(SourceFormat::Excel)
        );
        assert_eq!(
            SourceFormat::from_path("page.HTML"),
            Some(SourceFormat::Html)
        );
        assert_eq!(
            SourceFormat::from_path("legacy.htm"),
            Some(SourceFormat::Html)
        );
        assert_eq!(SourceFormat::from_path("page.xhtml"), None);
        assert_eq!(SourceFormat::from_path("image.png"), None);
        assert!(SourceFormat::Canvas.spec().extraction_supported);
        assert!(SourceFormat::Docx.spec().extraction_supported);
        assert!(SourceFormat::Pdf.spec().extraction_supported);
        assert!(SourceFormat::Canvas.is_extractable());
        assert!(SourceFormat::Docx.is_extractable());
        assert!(SourceFormat::Excalidraw.is_extractable());
        assert_eq!(
            SourceFormat::from_extractable_path("Drawings/board.excalidraw"),
            Some(SourceFormat::Excalidraw)
        );
        // Last-extension-wins keeps the Obsidian wrapper a Markdown note.
        assert_eq!(
            SourceFormat::from_extractable_path("Drawings/board.excalidraw.md"),
            Some(SourceFormat::Markdown)
        );
        assert!(SourceFormat::Pdf.is_extractable());
        assert!(SourceFormat::Excel.is_extractable());
        assert_eq!(
            SourceFormat::from_extractable_path("book.XLSX"),
            Some(SourceFormat::Excel)
        );
        assert_eq!(
            SourceFormat::from_extractable_path("macros.xlsm"),
            Some(SourceFormat::Excel)
        );
        assert_eq!(
            SourceFormat::from_extractable_path("board.canvas"),
            Some(SourceFormat::Canvas)
        );
        assert_eq!(
            SourceFormat::from_extractable_path("report.docx"),
            Some(SourceFormat::Docx)
        );
        assert_eq!(
            SourceFormat::from_extractable_path("paper.PDF"),
            Some(SourceFormat::Pdf)
        );

        // The closed set is now wholly extractable: no registry entry may claim
        // a format the dispatcher has no extractor for.
        for spec in format_specs() {
            assert_eq!(spec.format.spec(), spec);
            assert!(!spec.extensions.is_empty());
            assert!(
                spec.extraction_supported,
                "{} is registered but not extractable",
                spec.name
            );
        }
    }
}
