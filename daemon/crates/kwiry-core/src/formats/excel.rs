// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};

use crate::extract::{
    ContentRole, ExtractedSection, ExtractedSource, ExtractionCoverage, ExtractionNotice,
    SourceLocator,
};
use crate::model::{Frontmatter, PropertyBag};

use super::ooxml::limits::MAX_NOTICES;

mod error;
mod limits;
mod opc;
mod spreadsheet;

use error::ExcelError;
use limits::{MAX_OUTPUT_BYTES, MAX_OUTPUT_HEADING_BYTES, MAX_OUTPUT_SECTIONS};
use opc::{
    COMMENTS_CONTENT_TYPE, ExcelPackage, SHARED_STRINGS_CONTENT_TYPE, WORKSHEET_CONTENT_TYPE,
};
use spreadsheet::{
    DefinedName, SheetDescriptor, WorkbookModel, merge_comments, parse_comments,
    parse_shared_strings, parse_workbook, parse_worksheet,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExcelCellLocator {
    pub sheet: String,
    pub cell: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExcelSection {
    pub heading_path: Vec<String>,
    pub content: String,
    pub role: ContentRole,
    pub locator: Option<ExcelCellLocator>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExcelCandidate {
    pub sections: Vec<ExcelSection>,
    pub coverage: ExtractionCoverage,
    pub notices: Vec<ExtractionNotice>,
}

#[derive(Debug, Default)]
struct OutputBudget {
    bytes: usize,
    sections: usize,
    heading_bytes: usize,
}

impl OutputBudget {
    fn push(
        &mut self,
        output: &mut Vec<ExcelSection>,
        heading_path: &[String],
        content: String,
        role: ContentRole,
        locator: Option<ExcelCellLocator>,
    ) -> Result<(), ExcelError> {
        if content.is_empty() {
            return Ok(());
        }
        self.bytes = self
            .bytes
            .checked_add(content.len())
            .filter(|bytes| *bytes <= MAX_OUTPUT_BYTES)
            .ok_or(ExcelError::XmlLimitExceeded)?;
        self.sections = self
            .sections
            .checked_add(1)
            .filter(|sections| *sections <= MAX_OUTPUT_SECTIONS)
            .ok_or(ExcelError::XmlLimitExceeded)?;
        let heading_bytes = heading_path
            .iter()
            .try_fold(0_usize, |total, heading| total.checked_add(heading.len()))
            .ok_or(ExcelError::XmlLimitExceeded)?;
        self.heading_bytes = self
            .heading_bytes
            .checked_add(heading_bytes)
            .filter(|bytes| *bytes <= MAX_OUTPUT_HEADING_BYTES)
            .ok_or(ExcelError::XmlLimitExceeded)?;
        output.push(ExcelSection {
            heading_path: heading_path.to_vec(),
            content,
            role,
            locator,
        });
        Ok(())
    }
}

fn extract_candidate(bytes: &[u8]) -> Result<ExcelCandidate, ExcelError> {
    let mut package = ExcelPackage::open(bytes)?;
    let workbook_bytes = package.workbook_bytes().to_vec();
    let workbook = parse_workbook(&workbook_bytes, &mut package.xml_budget)?;
    let shared_strings = load_shared_strings(&mut package)?;
    let mut sections = Vec::new();
    let mut notices = Vec::new();
    let mut output_budget = OutputBudget::default();

    emit_global_defined_names(&workbook, &mut output_budget, &mut sections)?;

    for (sheet_index, sheet) in workbook.sheets.iter().enumerate() {
        emit_sheet_prelude(
            sheet_index,
            sheet,
            &workbook.defined_names,
            &mut output_budget,
            &mut sections,
        )?;

        let relationship = package
            .workbook_relationship(&sheet.relationship_id)
            .cloned()
            .ok_or(ExcelError::RequiredPartInvalid)?;
        if !relationship.is_worksheet() {
            return Err(ExcelError::RequiredPartInvalid);
        }
        let worksheet_uri = package.resolve_internal(package.workbook_uri(), &relationship)?;
        if !package.contains(&worksheet_uri) {
            return Err(ExcelError::RequiredPartInvalid);
        }
        let worksheet_bytes = package.open_typed_xml(&worksheet_uri, WORKSHEET_CONTENT_TYPE)?;
        let mut worksheet = parse_worksheet(
            &worksheet_bytes,
            shared_strings.as_deref(),
            &mut package.xml_budget,
        )?;

        let relationships = package.relationships_for_part(&worksheet_uri)?;
        let mut comments_relationship = None;
        for relationship in relationships {
            if relationship.is_comments() {
                match comments_relationship.replace(relationship) {
                    None => {}
                    Some(_) => return Err(ExcelError::RequiredPartInvalid),
                }
            } else if relationship.is_threaded_comments() {
                push_notice(
                    &mut notices,
                    "excel_threaded_comments_unopened",
                    "threaded Excel comments were left unopened",
                );
            }
        }
        if let Some(relationship) = comments_relationship {
            let target = package.resolve_internal(&worksheet_uri, &relationship)?;
            if package.contains(&target) {
                let comment_bytes = package.open_typed_xml(&target, COMMENTS_CONTENT_TYPE)?;
                let comments = parse_comments(&comment_bytes, &mut package.xml_budget)?;
                merge_comments(&mut worksheet, comments, sheet.visibility.is_hidden());
            } else {
                push_notice(
                    &mut notices,
                    "excel_optional_comments_unavailable",
                    "an Excel comments part referenced by a worksheet is unavailable",
                );
            }
        }

        let heading_path = vec![sheet.name.clone()];
        for (coordinate, cell) in worksheet.cells {
            let locator = Some(ExcelCellLocator {
                sheet: sheet.name.clone(),
                cell: coordinate.a1(),
            });
            if let Some(value) = cell.value {
                output_budget.push(
                    &mut sections,
                    &heading_path,
                    value,
                    if sheet.visibility.is_hidden() || cell.hidden {
                        ContentRole::Latent
                    } else {
                        ContentRole::Primary
                    },
                    locator.clone(),
                )?;
            }
            if let Some(formula) = cell.formula {
                output_budget.push(
                    &mut sections,
                    &heading_path,
                    formula,
                    ContentRole::Latent,
                    locator.clone(),
                )?;
            }
            if let Some(comment) = cell.comment {
                output_budget.push(
                    &mut sections,
                    &heading_path,
                    comment,
                    ContentRole::Latent,
                    locator,
                )?;
            }
        }
    }

    let coverage = if sections.is_empty() {
        push_notice(
            &mut notices,
            "excel_no_extractable_text",
            "the Excel workbook contains no searchable text",
        );
        ExtractionCoverage::SkippedNoExtractableText
    } else if notices.is_empty() {
        ExtractionCoverage::IndexedComplete
    } else {
        ExtractionCoverage::IndexedPartial
    };
    Ok(ExcelCandidate {
        sections,
        coverage,
        notices,
    })
}

fn load_shared_strings(package: &mut ExcelPackage<'_>) -> Result<Option<Vec<String>>, ExcelError> {
    let relationships = package
        .workbook_relationships()
        .iter()
        .filter(|relationship| relationship.is_shared_strings())
        .cloned()
        .collect::<Vec<_>>();
    let Some(relationship) = relationships.first() else {
        return Ok(None);
    };
    if relationships.len() != 1 {
        return Err(ExcelError::RequiredPartInvalid);
    }
    let target = package.resolve_internal(package.workbook_uri(), relationship)?;
    if !package.contains(&target) {
        return Ok(None);
    }
    let bytes = package.open_typed_xml(&target, SHARED_STRINGS_CONTENT_TYPE)?;
    parse_shared_strings(&bytes, &mut package.xml_budget).map(Some)
}

fn emit_global_defined_names(
    workbook: &WorkbookModel,
    output_budget: &mut OutputBudget,
    sections: &mut Vec<ExcelSection>,
) -> Result<(), ExcelError> {
    for name in workbook
        .defined_names
        .iter()
        .filter(|name| name.local_sheet.is_none())
    {
        emit_defined_name(name, None, false, output_budget, sections)?;
    }
    Ok(())
}

fn emit_sheet_prelude(
    sheet_index: usize,
    sheet: &SheetDescriptor,
    names: &[DefinedName],
    output_budget: &mut OutputBudget,
    sections: &mut Vec<ExcelSection>,
) -> Result<(), ExcelError> {
    let heading_path = vec![sheet.name.clone()];
    output_budget.push(
        sections,
        &heading_path,
        sheet.name.clone(),
        if sheet.visibility.is_hidden() {
            ContentRole::Latent
        } else {
            ContentRole::Primary
        },
        None,
    )?;
    for name in names
        .iter()
        .filter(|name| name.local_sheet == Some(sheet_index))
    {
        emit_defined_name(
            name,
            Some(&heading_path),
            sheet.visibility.is_hidden(),
            output_budget,
            sections,
        )?;
    }
    Ok(())
}

fn emit_defined_name(
    name: &DefinedName,
    heading_path: Option<&[String]>,
    sheet_hidden: bool,
    output_budget: &mut OutputBudget,
    sections: &mut Vec<ExcelSection>,
) -> Result<(), ExcelError> {
    let heading_path = heading_path.unwrap_or(&[]);
    let latent = name.hidden || sheet_hidden;
    output_budget.push(
        sections,
        heading_path,
        name.name.clone(),
        if latent {
            ContentRole::Latent
        } else {
            ContentRole::Primary
        },
        None,
    )?;
    output_budget.push(
        sections,
        heading_path,
        name.definition.clone(),
        ContentRole::Latent,
        None,
    )
}

pub(super) fn extract(bytes: &[u8]) -> ExtractedSource {
    let candidate = extract_excel_candidate_outcome(bytes);
    ExtractedSource {
        properties: PropertyBag::default(),
        frontmatter: Frontmatter::default(),
        aliases: Vec::new(),
        links_out: Vec::new(),
        sections: candidate
            .sections
            .into_iter()
            .map(|section| ExtractedSection {
                heading_path: section.heading_path,
                content: section.content,
                role: section.role,
                locator: section.locator.map(|locator| SourceLocator::ExcelCell {
                    sheet: locator.sheet,
                    cell: locator.cell,
                }),
            })
            .collect(),
        coverage: candidate.coverage,
        notices: candidate.notices,
    }
}

pub fn extract_excel_candidate_outcome(bytes: &[u8]) -> ExcelCandidate {
    match extract_candidate(bytes) {
        Ok(candidate) => candidate,
        Err(error) => ExcelCandidate {
            sections: Vec::new(),
            coverage: match error {
                ExcelError::NotSpreadsheet => ExtractionCoverage::SkippedNoExtractableText,
                ExcelError::EncryptedPackage
                | ExcelError::UnsupportedCompression
                | ExcelError::UnsupportedXmlEncoding => ExtractionCoverage::Unreadable,
                _ => ExtractionCoverage::Quarantined,
            },
            notices: vec![ExtractionNotice::new(
                error.notice_code(),
                error.to_string(),
            )],
        },
    }
}

fn push_notice(notices: &mut Vec<ExtractionNotice>, code: &'static str, message: &'static str) {
    if notices.len() < MAX_NOTICES - 1 {
        notices.push(ExtractionNotice::new(code, message));
    } else if notices.len() == MAX_NOTICES - 1 {
        notices.push(ExtractionNotice::new(
            "excel_notices_truncated",
            "additional Excel extraction notices were omitted",
        ));
    }
}

#[cfg(test)]
pub(crate) mod tests;
