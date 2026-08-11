// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::extract::{
    ExtractedSection, ExtractedSource, ExtractionCompleteness, ExtractionCoverage, ExtractionNotice,
};
use crate::model::{Frontmatter, PropertyBag};

pub(crate) mod error;
pub(crate) mod limits;
pub(crate) mod opc;
mod properties;
mod wordprocessing;
pub(crate) mod xml;
pub(crate) mod zip;

#[cfg(test)]
pub(crate) mod test_support;

/// Production DOCX extraction. The scope is `AllContent` so latent material
/// (tracked deletions, hidden text, field instructions) stays searchable; the
/// per-section `ContentRole` preserves the distinction for future weighting
/// without giving DOCX its own ranking rule today.
pub(super) fn extract(bytes: &[u8]) -> ExtractedSource {
    let candidate = extract_candidate_outcome(bytes, ExtractionScope::AllContent);
    if !candidate.coverage.is_indexed() {
        let mut source = ExtractedSource::skipped(
            candidate.coverage,
            candidate.notices.first().cloned().unwrap_or_else(|| {
                ExtractionNotice::new("docx_unreadable", "DOCX was not indexed")
            }),
        );
        source.notices = candidate.notices;
        return source;
    }

    let completeness = if candidate.coverage == ExtractionCoverage::IndexedPartial {
        ExtractionCompleteness::Partial
    } else {
        ExtractionCompleteness::Complete
    };
    ExtractedSource::indexed(
        PropertyBag::from_properties(candidate.properties.into_property_map()),
        Frontmatter::default(),
        Vec::new(),
        candidate.links_out,
        candidate
            .sections
            .into_iter()
            .map(|section| ExtractedSection {
                heading_path: section.heading_path,
                content: section.content,
                role: section.role,
                locator: None,
            })
            .collect(),
        completeness,
        candidate.notices,
    )
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocxCandidate {
    pub scope: ExtractionScope,
    pub sections: Vec<SemanticSection>,
    pub links_out: Vec<String>,
    pub properties: DocxProperties,
    pub coverage: crate::extract::ExtractionCoverage,
    pub notices: Vec<crate::extract::ExtractionNotice>,
}

pub use crate::extract::ContentRole;
pub use properties::DocxProperties;
pub use wordprocessing::{ExtractionScope, SemanticSection};

#[cfg(test)]
fn extract_candidate(bytes: &[u8]) -> Result<DocxCandidate, error::DocxError> {
    extract_candidate_with_scope(bytes, ExtractionScope::AllContent)
}

fn extract_candidate_with_scope(
    bytes: &[u8],
    scope: ExtractionScope,
) -> Result<DocxCandidate, error::DocxError> {
    use std::collections::BTreeSet;

    use opc::PackagePartKind;
    use wordprocessing::HeaderFooterKind;

    let mut inventory = zip::ArchiveInventory::new(bytes)?;
    let mut package = opc::PackageFoundation::load(&mut inventory)?;
    let mut xml_budget = std::mem::take(&mut package.xml_budget);
    let properties = properties::extract_properties(&package, &mut xml_budget)?;
    let mut output_budget = wordprocessing::OutputBudget::default();

    let styles = singleton_part(&package, PackagePartKind::Styles)?
        .map(|part| wordprocessing::parse_styles(&part.bytes, &mut xml_budget))
        .transpose()?
        .unwrap_or_default();
    let main = package
        .part(&package.main_part)
        .ok_or(error::DocxError::RequiredPartInvalid)?;
    let document = wordprocessing::extract_document(
        &main.bytes,
        &styles,
        &mut xml_budget,
        &mut output_budget,
    )?;
    let mut encountered = document.sections;
    let mut unsupported_markup = document.unsupported_markup;

    if let Some(part) = singleton_part(&package, PackagePartKind::Footnotes)? {
        let extracted = wordprocessing::extract_notes(
            &part.bytes,
            wordprocessing::PartKind::Footnotes,
            &styles,
            &mut xml_budget,
            &mut output_budget,
        )?;
        encountered.extend(extracted.sections);
        unsupported_markup |= extracted.unsupported_markup;
    }
    if let Some(part) = singleton_part(&package, PackagePartKind::Endnotes)? {
        let extracted = wordprocessing::extract_notes(
            &part.bytes,
            wordprocessing::PartKind::Endnotes,
            &styles,
            &mut xml_budget,
            &mut output_budget,
        )?;
        encountered.extend(extracted.sections);
        unsupported_markup |= extracted.unsupported_markup;
    }

    let mut extracted_header_footer_targets = BTreeSet::new();
    for (reference_kind, part_kind) in [
        (HeaderFooterKind::Header, PackagePartKind::Header),
        (HeaderFooterKind::Footer, PackagePartKind::Footer),
    ] {
        for reference in document
            .header_footer_references
            .iter()
            .filter(|reference| reference.kind == reference_kind)
        {
            let Some(target) = package.relationship_target(
                &package.main_part,
                &reference.relationship_id,
                part_kind,
            ) else {
                push_notice(
                    &mut package.notices,
                    "docx_unresolved_relationship",
                    "a section header or footer reference could not be resolved",
                );
                continue;
            };
            if !extracted_header_footer_targets.insert(target.to_owned()) {
                continue;
            }
            let part = package
                .part(target)
                .ok_or(error::DocxError::RequiredPartInvalid)?;
            let extracted = wordprocessing::extract_supporting_story(
                &part.bytes,
                &styles,
                &mut xml_budget,
                &mut output_budget,
            )?;
            encountered.extend(extracted.sections);
            unsupported_markup |= extracted.unsupported_markup;
        }
    }

    // A header or footer part can be related without any w:headerReference to
    // it. Such a part is invisible in the current view, so it is only extracted
    // for AllContent; either way the omission is disclosed rather than being
    // reported as complete coverage.
    let unreferenced_header_footer_targets = [PackagePartKind::Header, PackagePartKind::Footer]
        .into_iter()
        .flat_map(|kind| package.parts_of_kind(kind).map(|part| part.uri.clone()))
        .filter(|uri| !extracted_header_footer_targets.contains(uri))
        .collect::<Vec<_>>();
    if !unreferenced_header_footer_targets.is_empty() {
        // An unreferenced header or footer is not part of what a reader sees,
        // so it belongs to AllContent only rather than to every scope that
        // happens to admit supporting content.
        if scope == ExtractionScope::AllContent {
            for target in &unreferenced_header_footer_targets {
                let part = package
                    .part(target)
                    .ok_or(error::DocxError::RequiredPartInvalid)?;
                let extracted = wordprocessing::extract_supporting_story(
                    &part.bytes,
                    &styles,
                    &mut xml_budget,
                    &mut output_budget,
                )?;
                encountered.extend(extracted.sections);
                unsupported_markup |= extracted.unsupported_markup;
            }
        }
        push_notice(
            &mut package.notices,
            "docx_unreferenced_header_footer",
            "a header or footer part is present without a section reference",
        );
    }

    if let Some(part) = singleton_part(&package, PackagePartKind::Comments)? {
        let extracted = wordprocessing::extract_comments(
            &part.bytes,
            &styles,
            &mut xml_budget,
            &mut output_budget,
        )?;
        encountered.extend(extracted.sections);
        unsupported_markup |= extracted.unsupported_markup;
    }
    if unsupported_markup {
        push_notice(
            &mut package.notices,
            "docx_unsupported_markup",
            "supported DOCX text was extracted while unsupported markup was omitted",
        );
    }

    let mut sections = Vec::new();
    for role in [
        ContentRole::Primary,
        ContentRole::Supporting,
        ContentRole::Latent,
    ] {
        if scope.includes(role) {
            sections.extend(
                encountered
                    .iter()
                    .filter(|section| section.role == role)
                    .cloned(),
            );
        }
    }

    let coverage = if sections.is_empty() && properties.title.is_none() {
        push_notice(
            &mut package.notices,
            "docx_no_extractable_text",
            "the selected DOCX extraction scope contains no searchable text",
        );
        crate::extract::ExtractionCoverage::SkippedNoExtractableText
    } else if package.notices.is_empty() {
        crate::extract::ExtractionCoverage::IndexedComplete
    } else {
        crate::extract::ExtractionCoverage::IndexedPartial
    };

    Ok(DocxCandidate {
        scope,
        sections,
        links_out: package.links_out,
        properties,
        coverage,
        notices: package.notices,
    })
}

pub fn extract_candidate_outcome(bytes: &[u8], scope: ExtractionScope) -> DocxCandidate {
    match extract_candidate_with_scope(bytes, scope) {
        Ok(candidate) => candidate,
        Err(error) => DocxCandidate {
            scope,
            sections: Vec::new(),
            links_out: Vec::new(),
            properties: properties::DocxProperties::default(),
            coverage: match error {
                error::DocxError::EncryptedPackage
                | error::DocxError::UnsupportedCompression
                | error::DocxError::UnsupportedXmlEncoding => {
                    crate::extract::ExtractionCoverage::Unreadable
                }
                _ => crate::extract::ExtractionCoverage::Quarantined,
            },
            notices: vec![crate::extract::ExtractionNotice::new(
                error.notice_code(),
                error.to_string(),
            )],
        },
    }
}

fn singleton_part(
    package: &opc::PackageFoundation,
    kind: opc::PackagePartKind,
) -> Result<Option<&opc::PackagePart>, error::DocxError> {
    let mut parts = package.parts_of_kind(kind);
    let first = parts.next();
    if parts.next().is_some() {
        return Err(error::DocxError::RequiredPartInvalid);
    }
    Ok(first)
}

fn push_notice(
    notices: &mut Vec<crate::extract::ExtractionNotice>,
    code: &'static str,
    message: &'static str,
) {
    if notices.len() < limits::MAX_NOTICES - 1 {
        notices.push(crate::extract::ExtractionNotice::new(code, message));
    } else if notices.len() == limits::MAX_NOTICES - 1 {
        notices.push(crate::extract::ExtractionNotice::new(
            "docx_notices_truncated",
            "additional DOCX extraction notices were omitted",
        ));
    }
}

#[cfg(test)]
mod tests {
    use crate::extract::ExtractionCoverage;

    use super::error::DocxError;
    use super::test_support::{
        CONTENT_TYPES_STRICT, CONTENT_TYPES_TRANSITIONAL, MAIN_CONTENT_TYPE, Method,
        OFFICE_REL_STRICT, OFFICE_REL_TRANSITIONAL, RELATIONSHIPS_STRICT,
        RELATIONSHIPS_TRANSITIONAL, TestEntry, WORDPROCESSING_STRICT, WORDPROCESSING_TRANSITIONAL,
        build_zip, content_types, document, root_relationships, set_u16, set_u32,
    };
    use super::{extract, extract_candidate};

    fn minimal_package(
        method: Method,
        strict: bool,
        main_bytes: Option<Vec<u8>>,
    ) -> super::test_support::BuiltZip {
        let (content_ns, relationships_ns, office_ns, word_ns) = if strict {
            (
                CONTENT_TYPES_STRICT,
                RELATIONSHIPS_STRICT,
                OFFICE_REL_STRICT,
                WORDPROCESSING_STRICT,
            )
        } else {
            (
                CONTENT_TYPES_TRANSITIONAL,
                RELATIONSHIPS_TRANSITIONAL,
                OFFICE_REL_TRANSITIONAL,
                WORDPROCESSING_TRANSITIONAL,
            )
        };
        let content_types = content_types(content_ns, "/custom/main.xml", MAIN_CONTENT_TYPE);
        let relationships = root_relationships(relationships_ns, office_ns, "custom/main.xml");
        let main = main_bytes.unwrap_or_else(|| document(word_ns).into_bytes());
        build_zip(&[
            TestEntry::stored("[Content_Types].xml", content_types.as_bytes()),
            TestEntry::stored("_rels/.rels", relationships.as_bytes()),
            TestEntry {
                name: "custom/main.xml",
                bytes: &main,
                method,
                flags: 1 << 11,
                descriptor: false,
            },
        ])
    }

    fn utf16_document(namespace: &str, little_endian: bool) -> Vec<u8> {
        let endian = if little_endian {
            "UTF-16LE"
        } else {
            "UTF-16BE"
        };
        let xml = format!(
            r#"<?xml version="1.0" encoding="{endian}"?><w:document xmlns:w="{namespace}"><w:body/></w:document>"#,
        );
        let mut bytes = if little_endian {
            vec![0xFF, 0xFE]
        } else {
            vec![0xFE, 0xFF]
        };
        for unit in xml.encode_utf16() {
            let encoded = if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            };
            bytes.extend_from_slice(&encoded);
        }
        bytes
    }

    fn semantic_package(
        reversed: bool,
        missing_optional_header: bool,
    ) -> super::test_support::BuiltZip {
        let content_types = format!(
            r#"<Types xmlns="{CONTENT_TYPES_TRANSITIONAL}"><Override PartName="/word/document.xml" ContentType="{MAIN_CONTENT_TYPE}"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/><Override PartName="/word/endnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/><Override PartName="/docProps/custom.xml" ContentType="application/vnd.openxmlformats-officedocument.custom-properties+xml"/></Types>"#,
        );
        let root_rels = format!(
            r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="rMain" Type="{OFFICE_REL_TRANSITIONAL}/officeDocument" Target="word/document.xml"/><Relationship Id="rCore" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rApp" Type="{OFFICE_REL_TRANSITIONAL}/extended-properties" Target="docProps/app.xml"/><Relationship Id="rCustom" Type="{OFFICE_REL_TRANSITIONAL}/custom-properties" Target="docProps/custom.xml"/></Relationships>"#,
        );
        let optional_relationship = if missing_optional_header {
            format!(
                r#"<Relationship Id="rMissing" Type="{OFFICE_REL_TRANSITIONAL}/header" Target="missing.xml"/>"#,
            )
        } else {
            String::new()
        };
        let main_rels = format!(
            r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="rStyles" Type="{OFFICE_REL_TRANSITIONAL}/styles" Target="styles.xml"/><Relationship Id="rFootnotes" Type="{OFFICE_REL_TRANSITIONAL}/footnotes" Target="footnotes.xml"/><Relationship Id="rEndnotes" Type="{OFFICE_REL_TRANSITIONAL}/endnotes" Target="endnotes.xml"/><Relationship Id="rHeader" Type="{OFFICE_REL_TRANSITIONAL}/header" Target="header1.xml"/><Relationship Id="rFooter" Type="{OFFICE_REL_TRANSITIONAL}/footer" Target="footer1.xml"/><Relationship Id="rComments" Type="{OFFICE_REL_TRANSITIONAL}/comments" Target="comments.xml"/><Relationship Id="rLink" Type="{OFFICE_REL_TRANSITIONAL}/hyperlink" Target="https://example.test/docx" TargetMode="External"/>{optional_relationship}</Relationships>"#,
        );
        let missing_reference = if missing_optional_header {
            r#"<w:headerReference w:type="even" r:id="rMissing"/>"#
        } else {
            ""
        };
        let main = format!(
            r#"<w:document xmlns:w="{WORDPROCESSING_TRANSITIONAL}" xmlns:r="{OFFICE_REL_TRANSITIONAL}"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Overview</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Visible </w:t></w:r><w:ins><w:r><w:t xml:space="preserve">Inserted </w:t></w:r></w:ins><w:moveTo><w:r><w:t xml:space="preserve">Moved here </w:t></w:r></w:moveTo><w:del><w:r><w:delText xml:space="preserve">Deleted </w:delText></w:r></w:del><w:moveFrom><w:r><w:delText xml:space="preserve">Moved away </w:delText></w:r></w:moveFrom><w:r><w:rPr><w:vanish/></w:rPr><w:t xml:space="preserve">Hidden </w:t></w:r><w:fldSimple w:instr=" AUTHOR "><w:r><w:t>Alice</w:t></w:r></w:fldSimple></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Cell A</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Cell B</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> DATE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>Rendered date</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:hyperlink r:id="rLink"><w:r><w:t>Linked text</w:t></w:r></w:hyperlink></w:p><w:sectPr><w:headerReference w:type="default" r:id="rHeader"/><w:headerReference w:type="first" r:id="rHeader"/><w:footerReference w:type="default" r:id="rFooter"/>{missing_reference}</w:sectPr></w:body></w:document>"#,
        );
        let styles = format!(
            r#"<w:styles xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr></w:style></w:styles>"#,
        );
        let footnotes = format!(
            r#"<w:footnotes xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:footnote w:id="2"><w:p><w:r><w:t>Footnote two</w:t></w:r></w:p></w:footnote><w:footnote w:id="-1"><w:p><w:r><w:t>Separator</w:t></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:t>Footnote one</w:t></w:r></w:p></w:footnote><w:footnote w:id="0"><w:p><w:r><w:t>Continuation</w:t></w:r></w:p></w:footnote></w:footnotes>"#,
        );
        let endnotes = format!(
            r#"<w:endnotes xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:endnote w:id="1"><w:p><w:r><w:t>Endnote one</w:t></w:r></w:p></w:endnote></w:endnotes>"#,
        );
        let header = format!(
            r#"<w:hdr xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:p><w:r><w:t>Shared header</w:t></w:r></w:p></w:hdr>"#,
        );
        let footer = format!(
            r#"<w:ftr xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:p><w:r><w:t>Shared footer</w:t></w:r></w:p></w:ftr>"#,
        );
        let comments = format!(
            r#"<w:comments xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:comment w:id="7"><w:p><w:r><w:t>Comment seven</w:t></w:r></w:p></w:comment><w:comment w:id="3"><w:p><w:r><w:t>Comment three</w:t></w:r></w:p></w:comment></w:comments>"#,
        );
        let core = r#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Semantic title</dc:title><cp:keywords>alpha; beta, gamma</cp:keywords></cp:coreProperties>"#;
        let app = r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>Kwiry Fixture</Application></Properties>"#;
        let custom = r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property fmtid="x" pid="2" name="ReviewState"><vt:lpwstr>approved</vt:lpwstr></property></Properties>"#;

        let values = vec![
            ("[Content_Types].xml", content_types.into_bytes()),
            ("_rels/.rels", root_rels.into_bytes()),
            ("word/document.xml", main.into_bytes()),
            ("word/_rels/document.xml.rels", main_rels.into_bytes()),
            ("word/styles.xml", styles.into_bytes()),
            ("word/footnotes.xml", footnotes.into_bytes()),
            ("word/endnotes.xml", endnotes.into_bytes()),
            ("word/header1.xml", header.into_bytes()),
            ("word/footer1.xml", footer.into_bytes()),
            ("word/comments.xml", comments.into_bytes()),
            ("docProps/core.xml", core.as_bytes().to_vec()),
            ("docProps/app.xml", app.as_bytes().to_vec()),
            ("docProps/custom.xml", custom.as_bytes().to_vec()),
        ];
        let indexes: Vec<usize> = if reversed {
            (0..values.len()).rev().collect()
        } else {
            (0..values.len()).collect()
        };
        let entries = indexes
            .iter()
            .map(|index| {
                let (name, bytes) = &values[*index];
                TestEntry::deflated(name, bytes)
            })
            .collect::<Vec<_>>();
        build_zip(&entries)
    }

    fn package_with_styles(body: &str, styles: &str) -> super::test_support::BuiltZip {
        let content_types = format!(
            r#"<Types xmlns="{CONTENT_TYPES_TRANSITIONAL}"><Override PartName="/word/document.xml" ContentType="{MAIN_CONTENT_TYPE}"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#,
        );
        let root = format!(
            r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="rMain" Type="{OFFICE_REL_TRANSITIONAL}/officeDocument" Target="word/document.xml"/></Relationships>"#,
        );
        let document_rels = format!(
            r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="rStyles" Type="{OFFICE_REL_TRANSITIONAL}/styles" Target="styles.xml"/></Relationships>"#,
        );
        let main = format!(
            r#"<w:document xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:body>{body}</w:body></w:document>"#,
        );
        build_zip(&[
            TestEntry::stored("[Content_Types].xml", content_types.as_bytes()),
            TestEntry::stored("_rels/.rels", root.as_bytes()),
            TestEntry::stored("word/_rels/document.xml.rels", document_rels.as_bytes()),
            TestEntry::stored("word/styles.xml", styles.as_bytes()),
            TestEntry::stored("word/document.xml", main.as_bytes()),
        ])
    }

    fn package_with_custom_properties(entries: &str) -> super::test_support::BuiltZip {
        let content_types = format!(
            r#"<Types xmlns="{CONTENT_TYPES_TRANSITIONAL}"><Override PartName="/word/document.xml" ContentType="{MAIN_CONTENT_TYPE}"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/></Types>"#,
        );
        let root = format!(
            r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="rMain" Type="{OFFICE_REL_TRANSITIONAL}/officeDocument" Target="word/document.xml"/><Relationship Id="rApp" Type="{OFFICE_REL_TRANSITIONAL}/extended-properties" Target="docProps/app.xml"/></Relationships>"#,
        );
        let app = format!(
            r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">{entries}</Properties>"#,
        );
        let main = format!(
            r#"<w:document xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:body><w:p><w:r><w:t>Body</w:t></w:r></w:p></w:body></w:document>"#,
        );
        build_zip(&[
            TestEntry::stored("[Content_Types].xml", content_types.as_bytes()),
            TestEntry::stored("_rels/.rels", root.as_bytes()),
            TestEntry::stored("docProps/app.xml", app.as_bytes()),
            TestEntry::stored("word/document.xml", main.as_bytes()),
        ])
    }

    fn package_with_unreferenced_header() -> super::test_support::BuiltZip {
        let content_types = format!(
            r#"<Types xmlns="{CONTENT_TYPES_TRANSITIONAL}"><Override PartName="/word/document.xml" ContentType="{MAIN_CONTENT_TYPE}"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#,
        );
        let root = format!(
            r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="rMain" Type="{OFFICE_REL_TRANSITIONAL}/officeDocument" Target="word/document.xml"/></Relationships>"#,
        );
        // Related but never referenced by a w:headerReference.
        let document_rels = format!(
            r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="rHeader" Type="{OFFICE_REL_TRANSITIONAL}/header" Target="header1.xml"/></Relationships>"#,
        );
        let header = format!(
            r#"<w:hdr xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:p><w:r><w:t>Orphan header</w:t></w:r></w:p></w:hdr>"#,
        );
        let main = format!(
            r#"<w:document xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:body><w:p><w:r><w:t>Body</w:t></w:r></w:p></w:body></w:document>"#,
        );
        build_zip(&[
            TestEntry::stored("[Content_Types].xml", content_types.as_bytes()),
            TestEntry::stored("_rels/.rels", root.as_bytes()),
            TestEntry::stored("word/_rels/document.xml.rels", document_rels.as_bytes()),
            TestEntry::stored("word/header1.xml", header.as_bytes()),
            TestEntry::stored("word/document.xml", main.as_bytes()),
        ])
    }

    fn padded_package(padding: &[u8]) -> super::test_support::BuiltZip {
        let content_types = content_types(
            CONTENT_TYPES_TRANSITIONAL,
            "/custom/main.xml",
            MAIN_CONTENT_TYPE,
        );
        let relationships = root_relationships(
            RELATIONSHIPS_TRANSITIONAL,
            OFFICE_REL_TRANSITIONAL,
            "custom/main.xml",
        );
        let main = document(WORDPROCESSING_TRANSITIONAL);
        build_zip(&[
            TestEntry::stored("[Content_Types].xml", content_types.as_bytes()),
            TestEntry::stored("_rels/.rels", relationships.as_bytes()),
            TestEntry::stored("custom/main.xml", main.as_bytes()),
            TestEntry::stored("custom/padding.bin", padding),
        ])
    }

    fn package_with_main_body(body: &str, title: Option<&str>) -> super::test_support::BuiltZip {
        let core_override = title.map_or("", |_| {
            r#"<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>"#
        });
        let content_types = format!(
            r#"<Types xmlns="{CONTENT_TYPES_TRANSITIONAL}"><Override PartName="/word/document.xml" ContentType="{MAIN_CONTENT_TYPE}"/>{core_override}</Types>"#,
        );
        let core_relationship = title.map_or(String::new(), |_| {
            r#"<Relationship Id="rCore" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>"#.to_owned()
        });
        let root = format!(
            r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="rMain" Type="{OFFICE_REL_TRANSITIONAL}/officeDocument" Target="word/document.xml"/>{core_relationship}</Relationships>"#,
        );
        let main = format!(
            r#"<w:document xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:body>{body}</w:body></w:document>"#,
        );
        let core = title.map(|title| {
            format!(
                r#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{title}</dc:title></cp:coreProperties>"#,
            )
        });
        let mut entries = vec![
            TestEntry::stored("[Content_Types].xml", content_types.as_bytes()),
            TestEntry::stored("_rels/.rels", root.as_bytes()),
            TestEntry::stored("word/document.xml", main.as_bytes()),
        ];
        if let Some(core) = core.as_ref() {
            entries.push(TestEntry::stored("docProps/core.xml", core.as_bytes()));
        }
        build_zip(&entries)
    }

    #[test]
    fn candidate_accepts_store_deflate_strict_and_utf16_foundations() {
        for package in [
            minimal_package(Method::Store, false, None),
            minimal_package(Method::Deflate, false, None),
            minimal_package(Method::Store, true, None),
            minimal_package(
                Method::Deflate,
                false,
                Some(utf16_document(WORDPROCESSING_TRANSITIONAL, true)),
            ),
            minimal_package(
                Method::Deflate,
                true,
                Some(utf16_document(WORDPROCESSING_STRICT, false)),
            ),
        ] {
            let candidate = extract_candidate(&package.bytes).expect("valid package foundation");
            assert!(candidate.sections.is_empty());
            assert!(candidate.links_out.is_empty());
            assert_eq!(
                candidate.coverage,
                ExtractionCoverage::SkippedNoExtractableText
            );
            assert_eq!(candidate.notices[0].code, "docx_no_extractable_text");
        }
    }

    #[test]
    fn semantic_extraction_is_role_major_scope_stable_and_package_order_independent() {
        use super::{ContentRole, ExtractionScope, extract_candidate_with_scope};

        let ordinary = semantic_package(false, false);
        let reversed = semantic_package(true, false);
        let all = extract_candidate_with_scope(&ordinary.bytes, ExtractionScope::AllContent)
            .expect("semantic package");
        let current = extract_candidate_with_scope(&ordinary.bytes, ExtractionScope::CurrentView)
            .expect("current-view package");
        let repacked = extract_candidate_with_scope(&reversed.bytes, ExtractionScope::AllContent)
            .expect("repacked package");

        assert_eq!(all.coverage, ExtractionCoverage::IndexedComplete);
        assert_eq!(all, repacked);
        assert_eq!(current.sections, all.sections[..current.sections.len()]);
        assert!(
            current
                .sections
                .iter()
                .all(|section| section.role != ContentRole::Latent)
        );
        assert!(
            all.sections
                .windows(2)
                .all(|pair| pair[0].role <= pair[1].role)
        );

        let primary = all
            .sections
            .iter()
            .filter(|section| section.role == ContentRole::Primary)
            .map(|section| section.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(primary[0], "Overview");
        assert!(primary.contains(&"Visible Inserted Moved here Alice"));
        assert!(primary.contains(&"Cell A\tCell B"));
        assert!(primary.contains(&"Rendered date"));
        assert!(primary.contains(&"Linked text"));
        assert!(
            all.sections
                .iter()
                .filter(|section| section.role == ContentRole::Primary)
                .all(|section| section.heading_path == ["Overview"])
        );

        let supporting = all
            .sections
            .iter()
            .filter(|section| section.role == ContentRole::Supporting)
            .map(|section| section.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            supporting,
            [
                "Footnote one",
                "Footnote two",
                "Endnote one",
                "Shared header",
                "Shared footer",
                "Comment seven",
                "Comment three",
            ]
        );
        let latent = all
            .sections
            .iter()
            .filter(|section| section.role == ContentRole::Latent)
            .map(|section| section.content.as_str())
            .collect::<Vec<_>>();
        assert!(latent.iter().any(|content| {
            content.contains("Deleted")
                && content.contains("Moved away")
                && content.contains("Hidden")
                && content.contains("AUTHOR")
        }));
        assert!(latent.iter().any(|content| content.contains("DATE")));
        assert!(!latent.contains(&"Separator"));
        assert!(!latent.contains(&"Continuation"));
        assert_eq!(all.links_out, ["https://example.test/docx"]);
        assert_eq!(all.properties.title.as_deref(), Some("Semantic title"));
        assert_eq!(all.properties.docx["core.keywords"], ["alpha; beta, gamma"]);
        assert_eq!(all.properties.docx["custom.ReviewState"], ["approved"]);
        assert!(all.notices.is_empty());
    }

    #[test]
    fn optional_parts_are_partial_and_typed_outcomes_remain_content_free() {
        use super::{ExtractionScope, extract_candidate_outcome, extract_candidate_with_scope};

        let package = semantic_package(false, true);
        let candidate = extract_candidate_with_scope(&package.bytes, ExtractionScope::AllContent)
            .expect("optional missing part remains readable");
        assert_eq!(candidate.coverage, ExtractionCoverage::IndexedPartial);
        assert!(
            candidate
                .notices
                .iter()
                .any(|notice| { notice.code == "docx_optional_part_unavailable" })
        );
        assert!(
            candidate
                .notices
                .iter()
                .any(|notice| { notice.code == "docx_unresolved_relationship" })
        );
        assert!(
            candidate
                .notices
                .iter()
                .all(|notice| !notice.message.contains("Semantic title"))
        );

        let invalid = extract_candidate_outcome(b"not a zip", ExtractionScope::AllContent);
        assert_eq!(invalid.coverage, ExtractionCoverage::Quarantined);
        assert!(invalid.sections.is_empty());
        assert_eq!(invalid.notices[0].code, "invalid_docx_package");
    }

    #[test]
    fn empty_property_only_and_latent_only_documents_have_typed_scope_outcomes() {
        use super::{ContentRole, ExtractionScope, extract_candidate_with_scope};

        let empty = package_with_main_body("", None);
        let empty = extract_candidate_with_scope(&empty.bytes, ExtractionScope::AllContent)
            .expect("valid empty document");
        assert_eq!(empty.coverage, ExtractionCoverage::SkippedNoExtractableText);

        let property_only = package_with_main_body("", Some("Property title"));
        let property_only =
            extract_candidate_with_scope(&property_only.bytes, ExtractionScope::CurrentView)
                .expect("property-only document");
        assert_eq!(property_only.coverage, ExtractionCoverage::IndexedComplete);
        assert!(property_only.sections.is_empty());
        assert_eq!(
            property_only.properties.title.as_deref(),
            Some("Property title")
        );

        let latent = package_with_main_body(
            r#"<w:p><w:del><w:r><w:delText>Historical only</w:delText></w:r></w:del></w:p>"#,
            None,
        );
        let current = extract_candidate_with_scope(&latent.bytes, ExtractionScope::CurrentView)
            .expect("latent current view");
        let all = extract_candidate_with_scope(&latent.bytes, ExtractionScope::AllContent)
            .expect("latent all content");
        assert_eq!(
            current.coverage,
            ExtractionCoverage::SkippedNoExtractableText
        );
        assert!(current.sections.is_empty());
        assert_eq!(all.coverage, ExtractionCoverage::IndexedComplete);
        assert_eq!(all.sections.len(), 1);
        assert_eq!(all.sections[0].role, ContentRole::Latent);
        assert_eq!(all.sections[0].content, "Historical only");
    }

    #[test]
    fn unsupported_semantic_markup_is_partial_without_losing_safe_text() {
        use super::{ExtractionScope, extract_candidate_with_scope};

        let package = package_with_main_body(
            r#"<w:p><w:r><w:t>Safe text</w:t></w:r></w:p><w:altChunk/>"#,
            None,
        );
        let candidate = extract_candidate_with_scope(&package.bytes, ExtractionScope::AllContent)
            .expect("supported text plus unsupported markup");
        assert_eq!(candidate.coverage, ExtractionCoverage::IndexedPartial);
        assert_eq!(candidate.sections[0].content, "Safe text");
        assert_eq!(candidate.notices[0].code, "docx_unsupported_markup");
    }

    #[test]
    fn candidate_retains_only_inert_supported_external_hyperlinks() {
        let content_types = content_types(
            CONTENT_TYPES_TRANSITIONAL,
            "/custom/main.xml",
            MAIN_CONTENT_TYPE,
        );
        let root = root_relationships(
            RELATIONSHIPS_TRANSITIONAL,
            OFFICE_REL_TRANSITIONAL,
            "custom/main.xml",
        );
        let main = document(WORDPROCESSING_TRANSITIONAL);
        let rels = format!(
            r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="rId1" Type="{OFFICE_REL_TRANSITIONAL}/hyperlink" Target="https://example.test/path" TargetMode="External"/><Relationship Id="rId2" Type="{OFFICE_REL_TRANSITIONAL}/hyperlink" Target="file:///private/path" TargetMode="External"/><Relationship Id="rId3" Type="{OFFICE_REL_TRANSITIONAL}/header" Target="missing-header.xml"/></Relationships>"#,
        );
        let package = build_zip(&[
            TestEntry::stored("[Content_Types].xml", content_types.as_bytes()),
            TestEntry::stored("_rels/.rels", root.as_bytes()),
            TestEntry::deflated("custom/main.xml", main.as_bytes()),
            TestEntry::stored("custom/_rels/main.xml.rels", rels.as_bytes()),
        ]);
        let candidate = extract_candidate(&package.bytes).expect("valid hyperlink package");
        assert_eq!(candidate.links_out, ["https://example.test/path"]);
        assert_eq!(candidate.notices.len(), 3);
        assert_eq!(candidate.notices[0].code, "docx_unresolved_relationship");
        assert_eq!(candidate.notices[1].code, "docx_optional_part_unavailable");
        assert_eq!(candidate.notices[2].code, "docx_no_extractable_text");
    }

    #[test]
    fn candidate_traverses_valid_related_parts_and_rejects_vba_relationships() {
        let content_types = format!(
            r#"<Types xmlns="{CONTENT_TYPES_TRANSITIONAL}"><Override PartName="/custom/main.xml" ContentType="{MAIN_CONTENT_TYPE}"/><Override PartName="/parts/header.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#,
        );
        let root = root_relationships(
            RELATIONSHIPS_TRANSITIONAL,
            OFFICE_REL_TRANSITIONAL,
            "custom/main.xml",
        );
        let main = document(WORDPROCESSING_TRANSITIONAL);
        let header = format!(r#"<w:hdr xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:p/></w:hdr>"#,);
        let rels = format!(
            r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="rId1" Type="{OFFICE_REL_TRANSITIONAL}/header" Target="../parts/header.xml"/></Relationships>"#,
        );
        let package = build_zip(&[
            TestEntry::stored("[Content_Types].xml", content_types.as_bytes()),
            TestEntry::stored("_rels/.rels", root.as_bytes()),
            TestEntry::deflated("custom/main.xml", main.as_bytes()),
            TestEntry::stored("custom/_rels/main.xml.rels", rels.as_bytes()),
            TestEntry::deflated("parts/header.xml", header.as_bytes()),
        ]);
        assert!(extract_candidate(&package.bytes).is_ok());

        let forbidden_rels = format!(
            r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="rId1" Type="{OFFICE_REL_TRANSITIONAL}/vbaProject" Target="vbaProject.bin"/></Relationships>"#,
        );
        let package = build_zip(&[
            TestEntry::stored("[Content_Types].xml", content_types.as_bytes()),
            TestEntry::stored("_rels/.rels", root.as_bytes()),
            TestEntry::stored("custom/main.xml", main.as_bytes()),
            TestEntry::stored("custom/_rels/main.xml.rels", forbidden_rels.as_bytes()),
        ]);
        assert_eq!(
            extract_candidate(&package.bytes).unwrap_err(),
            DocxError::RequiredPartInvalid
        );
    }

    #[test]
    fn docx_is_admitted_and_extracts_through_the_shared_source_model() {
        let package =
            package_with_main_body(r#"<w:p><w:r><w:t>Admitted body</w:t></w:r></w:p>"#, None);
        let extracted = extract(&package.bytes);
        assert_eq!(extracted.coverage, ExtractionCoverage::IndexedComplete);
        assert_eq!(
            extracted
                .sections
                .iter()
                .map(|section| section.content.as_str())
                .collect::<Vec<_>>(),
            ["Admitted body"]
        );
        assert!(extracted.notices.is_empty());
        assert!(crate::format::SourceFormat::Docx.is_extractable());

        // An empty package still carries no text, but it must be reported as a
        // real extraction outcome rather than an unsupported format.
        let empty = extract(&minimal_package(Method::Store, false, None).bytes);
        assert_eq!(empty.coverage, ExtractionCoverage::SkippedNoExtractableText);
        assert!(
            empty
                .notices
                .iter()
                .all(|notice| notice.code != "format_not_yet_supported")
        );
    }

    #[test]
    fn crc_failures_are_rejected_for_store_and_deflate() {
        for method in [Method::Store, Method::Deflate] {
            let mut package = minimal_package(method, false, None);
            let location = &package.entries[2];
            let bad_crc = 0x1122_3344;
            set_u32(&mut package.bytes, location.local_offset + 14, bad_crc);
            set_u32(&mut package.bytes, location.central_offset + 16, bad_crc);
            assert_eq!(
                extract_candidate(&package.bytes).unwrap_err(),
                DocxError::IntegrityFailed
            );
        }
    }

    #[test]
    fn descriptors_are_required_and_must_match_central_claims() {
        let content_types = content_types(
            CONTENT_TYPES_TRANSITIONAL,
            "/custom/main.xml",
            MAIN_CONTENT_TYPE,
        );
        let root = root_relationships(
            RELATIONSHIPS_TRANSITIONAL,
            OFFICE_REL_TRANSITIONAL,
            "custom/main.xml",
        );
        let main = document(WORDPROCESSING_TRANSITIONAL);
        let mut package = build_zip(&[
            TestEntry::stored("[Content_Types].xml", content_types.as_bytes()),
            TestEntry::stored("_rels/.rels", root.as_bytes()),
            TestEntry {
                name: "custom/main.xml",
                bytes: main.as_bytes(),
                method: Method::Deflate,
                flags: 1 << 11,
                descriptor: true,
            },
        ]);
        assert!(extract_candidate(&package.bytes).is_ok());
        let descriptor = package.entries[2].descriptor_offset.expect("descriptor");
        set_u32(&mut package.bytes, descriptor + 4, 0x5566_7788);
        assert_eq!(
            extract_candidate(&package.bytes).unwrap_err(),
            DocxError::IntegrityFailed
        );

        let mut absent = minimal_package(Method::Deflate, false, None);
        let location = absent.entries[2].clone();
        let descriptor_flags = (1 << 11) | (1 << 3);
        set_u16(
            &mut absent.bytes,
            location.local_offset + 6,
            descriptor_flags,
        );
        set_u16(
            &mut absent.bytes,
            location.central_offset + 8,
            descriptor_flags,
        );
        assert_eq!(
            extract_candidate(&absent.bytes).unwrap_err(),
            DocxError::IntegrityFailed
        );
    }

    #[test]
    fn central_and_local_metadata_must_agree() {
        let base = minimal_package(Method::Store, false, None);
        let location = base.entries[2].clone();
        for mutate in 0..5 {
            let mut bytes = base.bytes.clone();
            match mutate {
                0 => bytes[location.local_offset + 30] ^= 1,
                1 => set_u16(&mut bytes, location.local_offset + 8, 8),
                2 => set_u16(&mut bytes, location.local_offset + 6, 0),
                3 => set_u32(
                    &mut bytes,
                    location.local_offset + 18,
                    location.compressed_len as u32 + 1,
                ),
                4 => set_u32(&mut bytes, location.local_offset + 22, 1),
                _ => unreachable!(),
            }
            assert!(matches!(
                extract_candidate(&bytes),
                Err(DocxError::InvalidPackage | DocxError::IntegrityFailed)
            ));
        }
    }

    #[test]
    fn duplicate_alias_offset_overlap_encryption_and_aes_are_rejected() {
        for second_name in [
            "word/document.xml",
            "WORD/DOCUMENT.XML",
            "word/%64ocument.xml",
        ] {
            let package = build_zip(&[
                TestEntry::stored("word/document.xml", b"a"),
                TestEntry::stored(second_name, b"b"),
            ]);
            assert_eq!(
                super::zip::ArchiveInventory::new(&package.bytes).unwrap_err(),
                DocxError::InvalidPackage
            );
        }

        let mut duplicate_offset = build_zip(&[
            TestEntry::stored("a.xml", b"a"),
            TestEntry::stored("b.xml", b"b"),
        ]);
        let first_offset = duplicate_offset.entries[0].local_offset as u32;
        let second_central = duplicate_offset.entries[1].central_offset;
        set_u32(
            &mut duplicate_offset.bytes,
            second_central + 42,
            first_offset,
        );
        assert_eq!(
            super::zip::ArchiveInventory::new(&duplicate_offset.bytes).unwrap_err(),
            DocxError::InvalidPackage
        );

        let mut overlap = build_zip(&[
            TestEntry::stored("a.xml", b"1234"),
            TestEntry::stored("b.xml", b"5678"),
        ]);
        let first = overlap.entries[0].clone();
        let second = overlap.entries[1].clone();
        let overlapping_size = second.data_offset + 1 - first.data_offset;
        set_u32(
            &mut overlap.bytes,
            first.local_offset + 18,
            overlapping_size as u32,
        );
        set_u32(
            &mut overlap.bytes,
            first.central_offset + 20,
            overlapping_size as u32,
        );
        assert_eq!(
            super::zip::ArchiveInventory::new(&overlap.bytes).unwrap_err(),
            DocxError::InvalidPackage
        );

        for entry in [
            TestEntry {
                name: "encrypted.bin",
                bytes: b"x",
                method: Method::Store,
                flags: (1 << 11) | 1,
                descriptor: false,
            },
            TestEntry {
                name: "aes.bin",
                bytes: b"x",
                method: Method::Other(99),
                flags: 1 << 11,
                descriptor: false,
            },
        ] {
            let package = build_zip(&[entry]);
            assert_eq!(
                super::zip::ArchiveInventory::new(&package.bytes).unwrap_err(),
                DocxError::EncryptedPackage
            );
        }
    }

    #[test]
    fn unsupported_selected_compression_and_truncated_directory_are_rejected() {
        let package = minimal_package(Method::Other(12), false, None);
        assert_eq!(
            extract_candidate(&package.bytes).unwrap_err(),
            DocxError::UnsupportedCompression
        );
        let mut truncated = minimal_package(Method::Store, false, None).bytes;
        truncated.truncate(truncated.len() - 12);
        assert!(matches!(
            extract_candidate(&truncated),
            Err(DocxError::InvalidPackage | DocxError::PackageLimitExceeded)
        ));
    }

    #[test]
    fn invalid_opc_roots_relationships_and_content_types_are_rejected() {
        let main = document(WORDPROCESSING_TRANSITIONAL);
        let valid_content = content_types(
            CONTENT_TYPES_TRANSITIONAL,
            "/custom/main.xml",
            MAIN_CONTENT_TYPE,
        );
        let cases = [
            root_relationships(
                RELATIONSHIPS_TRANSITIONAL,
                OFFICE_REL_TRANSITIONAL,
                "../escape.xml",
            ),
            format!(
                r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="rId1" Type="{OFFICE_REL_TRANSITIONAL}/officeDocument" Target="custom/main.xml" TargetMode="External"/></Relationships>"#,
            ),
            format!(
                r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="same" Type="{OFFICE_REL_TRANSITIONAL}/officeDocument" Target="custom/main.xml"/><Relationship Id="same" Type="{OFFICE_REL_TRANSITIONAL}/hyperlink" Target="https://example.test" TargetMode="External"/></Relationships>"#,
            ),
        ];
        for root in cases {
            let package = build_zip(&[
                TestEntry::stored("[Content_Types].xml", valid_content.as_bytes()),
                TestEntry::stored("_rels/.rels", root.as_bytes()),
                TestEntry::stored("custom/main.xml", main.as_bytes()),
            ]);
            assert_eq!(
                extract_candidate(&package.bytes).unwrap_err(),
                DocxError::RequiredPartInvalid
            );
        }

        let macro_content = content_types(
            CONTENT_TYPES_TRANSITIONAL,
            "/custom/main.xml",
            "application/vnd.ms-word.document.macroEnabled.main+xml",
        );
        let root = root_relationships(
            RELATIONSHIPS_TRANSITIONAL,
            OFFICE_REL_TRANSITIONAL,
            "custom/main.xml",
        );
        let package = build_zip(&[
            TestEntry::stored("[Content_Types].xml", macro_content.as_bytes()),
            TestEntry::stored("_rels/.rels", root.as_bytes()),
            TestEntry::stored("custom/main.xml", main.as_bytes()),
        ]);
        assert_eq!(
            extract_candidate(&package.bytes).unwrap_err(),
            DocxError::RequiredPartInvalid
        );
    }

    #[test]
    fn heading_paths_are_budgeted_and_empty_roles_never_clone_the_heading_stack() {
        use super::{ContentRole, ExtractionScope, extract_candidate_with_scope};

        // Nine nested headings carrying ~100 KiB each. Every emitted section
        // duplicates the whole stack, so a small package can otherwise
        // manufacture unbounded output that no budget ever charges.
        let heading_text = "h".repeat(100_000);
        let mut body = String::new();
        for level in 0..9 {
            body.push_str(&format!(
                r#"<w:p><w:pPr><w:outlineLvl w:val="{level}"/></w:pPr><w:r><w:t>{heading_text}</w:t></w:r></w:p>"#,
            ));
        }
        for index in 0..12 {
            body.push_str(&format!(r#"<w:p><w:r><w:t>body {index}</w:t></w:r></w:p>"#));
        }
        let package = package_with_main_body(&body, None);
        assert_eq!(
            extract_candidate(&package.bytes).unwrap_err(),
            DocxError::XmlLimitExceeded
        );

        // Empty paragraphs must cost nothing: they emit no section, so a
        // document of them stays cheap no matter how deep the heading stack is.
        let mut cheap = String::new();
        for level in 0..9 {
            cheap.push_str(&format!(
                r#"<w:p><w:pPr><w:outlineLvl w:val="{level}"/></w:pPr><w:r><w:t>H{level}</w:t></w:r></w:p>"#,
            ));
        }
        cheap.push_str(&"<w:p/>".repeat(20_000));
        let package = package_with_main_body(&cheap, None);
        let candidate = extract_candidate_with_scope(&package.bytes, ExtractionScope::AllContent)
            .expect("empty paragraphs are free");
        assert_eq!(candidate.sections.len(), 9);
        assert!(
            candidate
                .sections
                .iter()
                .all(|section| section.role == ContentRole::Primary)
        );
        assert_eq!(candidate.sections[8].heading_path.len(), 9);

        // w:outlineLvl is defined over 0-8, so an out-of-range level must not
        // extend the heading stack.
        let body = format!(
            r#"<w:p><w:pPr><w:outlineLvl w:val="{}"/></w:pPr><w:r><w:t>Not a heading</w:t></w:r></w:p><w:p><w:r><w:t>Body</w:t></w:r></w:p>"#,
            usize::from(u8::MAX),
        );
        let package = package_with_main_body(&body, None);
        let candidate = extract_candidate(&package.bytes).expect("out-of-range outline level");
        assert!(
            candidate
                .sections
                .iter()
                .all(|section| section.heading_path.is_empty())
        );
    }

    #[test]
    fn style_hidden_text_is_latent_and_excluded_from_the_current_view() {
        use super::{ContentRole, ExtractionScope, extract_candidate_with_scope};

        let styles = format!(
            r#"<w:styles xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:style w:type="character" w:styleId="Secret"><w:rPr><w:vanish/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Ghost"><w:rPr><w:webHidden/></w:rPr></w:style></w:styles>"#,
        );
        let body = concat!(
            r#"<w:p><w:r><w:rPr><w:rStyle w:val="Secret"/></w:rPr><w:t>CONFIDENTIAL</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:pStyle w:val="Ghost"/></w:pPr><w:r><w:t>Ghost body</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>Visible</w:t></w:r></w:p>"#,
        );
        let package = package_with_styles(body, &styles);

        let current = extract_candidate_with_scope(&package.bytes, ExtractionScope::CurrentView)
            .expect("current view");
        let visible = current
            .sections
            .iter()
            .map(|section| section.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(visible, ["Visible"]);

        let all = extract_candidate_with_scope(&package.bytes, ExtractionScope::AllContent)
            .expect("all content");
        for hidden in ["CONFIDENTIAL", "Ghost body"] {
            let section = all
                .sections
                .iter()
                .find(|section| section.content == hidden)
                .expect("hidden text is still extracted for AllContent");
            assert_eq!(section.role, ContentRole::Latent);
        }
    }

    #[test]
    fn forbidden_relationships_are_rejected_at_the_package_root() {
        for suffix in ["vbaProject", "attachedTemplate", "oleObject"] {
            let content_types = format!(
                r#"<Types xmlns="{CONTENT_TYPES_TRANSITIONAL}"><Override PartName="/word/document.xml" ContentType="{MAIN_CONTENT_TYPE}"/></Types>"#,
            );
            let root = format!(
                r#"<Relationships xmlns="{RELATIONSHIPS_TRANSITIONAL}"><Relationship Id="rMain" Type="{OFFICE_REL_TRANSITIONAL}/officeDocument" Target="word/document.xml"/><Relationship Id="rBad" Type="{OFFICE_REL_TRANSITIONAL}/{suffix}" Target="word/vbaProject.bin"/></Relationships>"#,
            );
            let main = format!(
                r#"<w:document xmlns:w="{WORDPROCESSING_TRANSITIONAL}"><w:body/></w:document>"#,
            );
            let package = build_zip(&[
                TestEntry::stored("[Content_Types].xml", content_types.as_bytes()),
                TestEntry::stored("_rels/.rels", root.as_bytes()),
                TestEntry::stored("word/document.xml", main.as_bytes()),
            ]);
            assert_eq!(
                extract_candidate(&package.bytes).unwrap_err(),
                DocxError::RequiredPartInvalid,
                "root relationship {suffix} must be refused",
            );
        }
    }

    #[test]
    fn property_output_boundaries_are_enforced() {
        let entries = (0..=super::limits::MAX_PROPERTY_ENTRIES)
            .map(|index| format!("<p{index}>v</p{index}>"))
            .collect::<String>();
        let package = package_with_custom_properties(&entries);
        assert_eq!(
            extract_candidate(&package.bytes).unwrap_err(),
            DocxError::XmlLimitExceeded
        );

        let value = "v".repeat(4_096);
        let entries = (0..=super::limits::MAX_PROPERTY_BYTES / 4_096)
            .map(|index| format!("<p{index}>{value}</p{index}>"))
            .collect::<String>();
        let package = package_with_custom_properties(&entries);
        assert_eq!(
            extract_candidate(&package.bytes).unwrap_err(),
            DocxError::XmlLimitExceeded
        );
    }

    #[test]
    fn unreferenced_header_parts_are_disclosed_rather_than_silently_dropped() {
        use super::{ExtractionScope, extract_candidate_with_scope};

        let package = package_with_unreferenced_header();
        let current = extract_candidate_with_scope(&package.bytes, ExtractionScope::CurrentView)
            .expect("current view");
        assert_eq!(current.coverage, ExtractionCoverage::IndexedPartial);
        assert!(
            current
                .sections
                .iter()
                .all(|section| section.content != "Orphan header")
        );

        let all = extract_candidate_with_scope(&package.bytes, ExtractionScope::AllContent)
            .expect("all content");
        assert_eq!(all.coverage, ExtractionCoverage::IndexedPartial);
        assert!(
            all.sections
                .iter()
                .any(|section| section.content == "Orphan header")
        );
    }

    #[test]
    fn package_input_size_boundary_admits_at_limit_and_rejects_one_over() {
        let limit = crate::model::MAX_FILE_BYTES as usize;
        // Stored bytes contribute one-for-one, so a single correction lands
        // exactly on the limit and proves the guard admits it.
        let mut padding = vec![b'a'; limit / 2];
        for _ in 0..4 {
            let built = padded_package(&padding);
            if built.bytes.len() == limit {
                break;
            }
            let next = padding.len() as isize + (limit as isize - built.bytes.len() as isize);
            assert!(next > 0, "padding correction must stay positive");
            padding = vec![b'a'; next as usize];
        }

        let at_limit = padded_package(&padding);
        assert_eq!(at_limit.bytes.len(), limit);
        assert!(super::zip::ArchiveInventory::new(&at_limit.bytes).is_ok());

        padding.push(b'a');
        let over_limit = padded_package(&padding);
        assert_eq!(over_limit.bytes.len(), limit + 1);
        assert_eq!(
            super::zip::ArchiveInventory::new(&over_limit.bytes).unwrap_err(),
            DocxError::PackageLimitExceeded
        );
    }

    #[test]
    fn archive_count_and_declared_size_boundaries_are_enforced() {
        let at_input_limit = vec![0_u8; crate::model::MAX_FILE_BYTES as usize];
        assert_eq!(
            super::zip::ArchiveInventory::new(&at_input_limit).unwrap_err(),
            DocxError::InvalidPackage
        );
        let over_input_limit = vec![0_u8; crate::model::MAX_FILE_BYTES as usize + 1];
        assert_eq!(
            super::zip::ArchiveInventory::new(&over_input_limit).unwrap_err(),
            DocxError::PackageLimitExceeded
        );

        let names = (0..=super::limits::MAX_CENTRAL_DIRECTORY_ENTRIES)
            .map(|index| format!("part-{index}.xml"))
            .collect::<Vec<_>>();
        let entries = names[..super::limits::MAX_CENTRAL_DIRECTORY_ENTRIES]
            .iter()
            .map(|name| TestEntry::stored(name, b""))
            .collect::<Vec<_>>();
        let at_limit = build_zip(&entries);
        assert!(super::zip::ArchiveInventory::new(&at_limit.bytes).is_ok());
        let entries = names
            .iter()
            .map(|name| TestEntry::stored(name, b""))
            .collect::<Vec<_>>();
        let over_limit = build_zip(&entries);
        assert_eq!(
            super::zip::ArchiveInventory::new(&over_limit.bytes).unwrap_err(),
            DocxError::PackageLimitExceeded
        );

        let mut per_entry = build_zip(&[TestEntry::stored("part.xml", b"")]);
        let location = per_entry.entries[0].clone();
        set_u32(
            &mut per_entry.bytes,
            location.local_offset + 22,
            super::limits::MAX_DECLARED_ENTRY_BYTES as u32,
        );
        set_u32(
            &mut per_entry.bytes,
            location.central_offset + 24,
            super::limits::MAX_DECLARED_ENTRY_BYTES as u32,
        );
        assert!(super::zip::ArchiveInventory::new(&per_entry.bytes).is_ok());
        set_u32(
            &mut per_entry.bytes,
            location.local_offset + 22,
            super::limits::MAX_DECLARED_ENTRY_BYTES as u32 + 1,
        );
        set_u32(
            &mut per_entry.bytes,
            location.central_offset + 24,
            super::limits::MAX_DECLARED_ENTRY_BYTES as u32 + 1,
        );
        assert_eq!(
            super::zip::ArchiveInventory::new(&per_entry.bytes).unwrap_err(),
            DocxError::PackageLimitExceeded
        );

        let mut package_total = build_zip(&[
            TestEntry::stored("a.xml", b""),
            TestEntry::stored("b.xml", b""),
            TestEntry::stored("c.xml", b""),
            TestEntry::stored("d.xml", b""),
            TestEntry::stored("e.xml", b""),
        ]);
        for location in &package_total.entries[..4] {
            set_u32(
                &mut package_total.bytes,
                location.local_offset + 22,
                super::limits::MAX_DECLARED_ENTRY_BYTES as u32,
            );
            set_u32(
                &mut package_total.bytes,
                location.central_offset + 24,
                super::limits::MAX_DECLARED_ENTRY_BYTES as u32,
            );
        }
        let fifth = package_total.entries[4].clone();
        set_u32(&mut package_total.bytes, fifth.local_offset + 22, 1);
        set_u32(&mut package_total.bytes, fifth.central_offset + 24, 1);
        assert_eq!(
            super::zip::ArchiveInventory::new(&package_total.bytes).unwrap_err(),
            DocxError::PackageLimitExceeded
        );
        for location in &package_total.entries[4..] {
            set_u32(&mut package_total.bytes, location.local_offset + 22, 0);
            set_u32(&mut package_total.bytes, location.central_offset + 24, 0);
        }
        assert!(super::zip::ArchiveInventory::new(&package_total.bytes).is_ok());
    }

    #[test]
    fn zip_metadata_boundary_is_enforced() {
        let long_names = (0..1_025)
            .map(|index| format!("{index:04}-{}", "a".repeat(1_018)))
            .collect::<Vec<_>>();
        let mut entries = long_names
            .iter()
            .map(|name| TestEntry::stored(name, b""))
            .collect::<Vec<_>>();
        entries.push(TestEntry::stored("z", b""));
        let at_limit = build_zip(&entries);
        assert!(super::zip::ArchiveInventory::new(&at_limit.bytes).is_ok());

        let mut over_limit = at_limit.bytes;
        let eocd_offset = over_limit.len() - 22;
        set_u16(&mut over_limit, eocd_offset + 20, 1);
        over_limit.push(b'x');
        assert_eq!(
            super::zip::ArchiveInventory::new(&over_limit).unwrap_err(),
            DocxError::PackageLimitExceeded
        );
    }

    #[test]
    fn selected_xml_size_ratio_and_aggregate_boundaries_are_enforced() {
        fn bounded_ratio_bytes(length: usize) -> Vec<u8> {
            let mut value = 0x1234_5678_u32;
            let mut bytes = Vec::with_capacity(length);
            for _ in 0..100_000.min(length) {
                value ^= value << 13;
                value ^= value >> 17;
                value ^= value << 5;
                bytes.push(value as u8);
            }
            bytes.resize(length, 0);
            bytes
        }

        let at_limit_bytes =
            bounded_ratio_bytes(super::limits::MAX_SELECTED_XML_PART_BYTES as usize);
        let at_limit = build_zip(&[TestEntry::deflated("part.xml", &at_limit_bytes)]);
        let mut inventory = super::zip::ArchiveInventory::new(&at_limit.bytes).expect("inventory");
        assert_eq!(
            inventory
                .open_selected_xml("/part.xml")
                .expect("at limit")
                .len(),
            super::limits::MAX_SELECTED_XML_PART_BYTES as usize
        );

        let over_limit_bytes =
            bounded_ratio_bytes(super::limits::MAX_SELECTED_XML_PART_BYTES as usize + 1);
        let over_limit = build_zip(&[TestEntry::deflated("part.xml", &over_limit_bytes)]);
        let mut inventory =
            super::zip::ArchiveInventory::new(&over_limit.bytes).expect("inventory");
        assert_eq!(
            inventory.open_selected_xml("/part.xml").unwrap_err(),
            DocxError::PackageLimitExceeded
        );

        let aggregate = build_zip(&[
            TestEntry::deflated("a.xml", &at_limit_bytes),
            TestEntry::deflated("b.xml", &at_limit_bytes),
            TestEntry::stored("c.xml", b"x"),
        ]);
        let mut inventory = super::zip::ArchiveInventory::new(&aggregate.bytes).expect("inventory");
        assert!(inventory.open_selected_xml("/a.xml").is_ok());
        assert!(inventory.open_selected_xml("/b.xml").is_ok());
        assert_eq!(
            inventory.open_selected_xml("/c.xml").unwrap_err(),
            DocxError::PackageLimitExceeded
        );

        let bomb = vec![0_u8; 2 * 1024 * 1024];
        let bomb = build_zip(&[TestEntry::deflated("bomb.xml", &bomb)]);
        let mut inventory = super::zip::ArchiveInventory::new(&bomb.bytes).expect("inventory");
        assert_eq!(
            inventory.open_selected_xml("/bomb.xml").unwrap_err(),
            DocxError::PackageLimitExceeded
        );

        let mut ratio = build_zip(&[TestEntry::deflated("ratio.xml", b"small data")]);
        let location = ratio.entries[0].clone();
        let ratio_limit = (location.compressed_len as u64)
            .checked_mul(super::limits::MAX_EXPANSION_RATIO)
            .and_then(|value| value.checked_add(super::limits::EXPANSION_RATIO_ALLOWANCE_BYTES))
            .expect("ratio limit") as u32;
        set_u32(&mut ratio.bytes, location.local_offset + 22, ratio_limit);
        set_u32(&mut ratio.bytes, location.central_offset + 24, ratio_limit);
        let mut inventory = super::zip::ArchiveInventory::new(&ratio.bytes).expect("inventory");
        assert_eq!(
            inventory.open_selected_xml("/ratio.xml").unwrap_err(),
            DocxError::IntegrityFailed
        );
        set_u32(
            &mut ratio.bytes,
            location.local_offset + 22,
            ratio_limit + 1,
        );
        set_u32(
            &mut ratio.bytes,
            location.central_offset + 24,
            ratio_limit + 1,
        );
        let mut inventory = super::zip::ArchiveInventory::new(&ratio.bytes).expect("inventory");
        assert_eq!(
            inventory.open_selected_xml("/ratio.xml").unwrap_err(),
            DocxError::PackageLimitExceeded
        );
    }

    #[test]
    fn xml_depth_attribute_namespace_qname_and_text_boundaries_are_enforced() {
        fn parse(source: &str) -> Result<(), DocxError> {
            super::xml::parse_xml(
                source.as_bytes(),
                &mut super::xml::XmlBudget::default(),
                |_| Ok(()),
            )
        }

        let nested = |depth: usize| format!("{}{}", "<a>".repeat(depth), "</a>".repeat(depth));
        assert!(parse(&nested(super::limits::MAX_XML_DEPTH)).is_ok());
        assert_eq!(
            parse(&nested(super::limits::MAX_XML_DEPTH + 1)).unwrap_err(),
            DocxError::XmlLimitExceeded
        );

        let attributes = |count: usize| {
            let attributes = (0..count)
                .map(|index| format!(" a{index}=\"x\""))
                .collect::<String>();
            format!("<a{attributes}/>")
        };
        assert!(parse(&attributes(super::limits::MAX_ATTRIBUTES_PER_ELEMENT)).is_ok());
        assert_eq!(
            parse(&attributes(super::limits::MAX_ATTRIBUTES_PER_ELEMENT + 1)).unwrap_err(),
            DocxError::XmlLimitExceeded
        );

        let namespaces = |count: usize| {
            let declarations = (0..count)
                .map(|index| format!(" xmlns:p{index}=\"urn:{index}\""))
                .collect::<String>();
            format!("<a{declarations}/>")
        };
        assert!(
            parse(&namespaces(
                super::limits::MAX_NAMESPACE_DECLARATIONS_PER_ELEMENT
            ))
            .is_ok()
        );
        assert_eq!(
            parse(&namespaces(
                super::limits::MAX_NAMESPACE_DECLARATIONS_PER_ELEMENT + 1
            ))
            .unwrap_err(),
            DocxError::XmlLimitExceeded
        );

        let qname = "a".repeat(super::limits::MAX_QNAME_BYTES);
        assert!(parse(&format!("<{qname}/>")).is_ok());
        let qname = "a".repeat(super::limits::MAX_QNAME_BYTES + 1);
        assert_eq!(
            parse(&format!("<{qname}/>")).unwrap_err(),
            DocxError::XmlLimitExceeded
        );

        let text = "x".repeat(super::limits::MAX_SINGLE_TEXT_EVENT_BYTES);
        assert!(parse(&format!("<a>{text}</a>")).is_ok());
        let text = "x".repeat(super::limits::MAX_SINGLE_TEXT_EVENT_BYTES + 1);
        assert_eq!(
            parse(&format!("<a>{text}</a>")).unwrap_err(),
            DocxError::XmlLimitExceeded
        );
        assert!(parse("<a>&amp;&#x41;&#65;</a>").is_ok());
        assert_eq!(
            parse("<a>&undeclared;</a>").unwrap_err(),
            DocxError::ForbiddenXmlConstruct
        );
    }

    #[test]
    fn malformed_encoding_doctype_and_wrong_namespace_are_rejected() {
        let malformed_utf8 = b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\xff</w:document>".to_vec();
        let odd_utf16 = vec![0xFF, 0xFE, 0x3C];
        let conflicting = {
            let xml = r#"<?xml version="1.0" encoding="UTF-16BE"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;
            let mut bytes = vec![0xFF, 0xFE];
            for unit in xml.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            bytes
        };
        for bytes in [malformed_utf8, odd_utf16, conflicting] {
            let package = minimal_package(Method::Store, false, Some(bytes));
            assert_eq!(
                extract_candidate(&package.bytes).unwrap_err(),
                DocxError::UnsupportedXmlEncoding
            );
        }

        for xml in [
            format!(
                r#"<!DOCTYPE w:document [<!ENTITY x "bad">]><w:document xmlns:w="{WORDPROCESSING_TRANSITIONAL}"/>"#,
            ),
            "<document xmlns=\"urn:wrong\"/>".to_owned(),
            format!(r#"<w:document xmlns:w="{WORDPROCESSING_TRANSITIONAL}">"#),
        ] {
            let package = minimal_package(Method::Store, false, Some(xml.into_bytes()));
            assert!(matches!(
                extract_candidate(&package.bytes),
                Err(DocxError::ForbiddenXmlConstruct | DocxError::RequiredPartInvalid)
            ));
        }
    }
}
