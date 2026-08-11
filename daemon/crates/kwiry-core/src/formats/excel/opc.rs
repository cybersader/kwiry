// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use crate::formats::ooxml::limits::{
    MAX_CANONICAL_PART_URI_BYTES, MAX_RELATIONSHIP_TARGET_BYTES, MAX_RELATIONSHIPS_PER_PART,
    MAX_RELATIONSHIPS_TOTAL, MAX_SELECTED_XML_PARTS,
};
use crate::formats::ooxml::{
    ArchiveInventory, XmlBudget, XmlElement, parse_xml, relationships_part_uri,
    resolve_relationship_target,
};

use super::error::ExcelError;

const CFB_SIGNATURE: &[u8; 8] = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1";
const CONTENT_TYPES_PART: &str = "/[Content_Types].xml";
const ROOT_RELATIONSHIPS_PART: &str = "/_rels/.rels";
const CONTENT_TYPES_NS_TRANSITIONAL: &[u8] =
    b"http://schemas.openxmlformats.org/package/2006/content-types";
const CONTENT_TYPES_NS_STRICT: &[u8] = b"http://purl.oclc.org/ooxml/package/content-types";
const RELATIONSHIPS_NS_TRANSITIONAL: &[u8] =
    b"http://schemas.openxmlformats.org/package/2006/relationships";
const RELATIONSHIPS_NS_STRICT: &[u8] = b"http://purl.oclc.org/ooxml/package/relationships";
const OFFICE_DOCUMENT_REL_TRANSITIONAL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
const OFFICE_DOCUMENT_REL_STRICT: &str =
    "http://purl.oclc.org/ooxml/officeDocument/relationships/officeDocument";
const WORKSHEET_REL_TRANSITIONAL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet";
const WORKSHEET_REL_STRICT: &str =
    "http://purl.oclc.org/ooxml/officeDocument/relationships/worksheet";
const SHARED_STRINGS_REL_TRANSITIONAL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings";
const SHARED_STRINGS_REL_STRICT: &str =
    "http://purl.oclc.org/ooxml/officeDocument/relationships/sharedStrings";
const COMMENTS_REL_TRANSITIONAL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
const COMMENTS_REL_STRICT: &str =
    "http://purl.oclc.org/ooxml/officeDocument/relationships/comments";
const THREADED_COMMENT_REL: &str =
    "http://schemas.microsoft.com/office/2017/10/relationships/threadedComment";
const THREADED_COMMENTS_REL: &str =
    "http://schemas.microsoft.com/office/2017/10/relationships/threadedComments";
const THREADED_COMMENT_REL_TRANSITIONAL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/threadedComment";
const THREADED_COMMENTS_REL_TRANSITIONAL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/threadedComments";
const THREADED_COMMENT_REL_STRICT: &str =
    "http://purl.oclc.org/ooxml/officeDocument/relationships/threadedComment";
const THREADED_COMMENTS_REL_STRICT: &str =
    "http://purl.oclc.org/ooxml/officeDocument/relationships/threadedComments";
const VBA_PROJECT_REL: &str = "http://schemas.microsoft.com/office/2006/relationships/vbaProject";
const VBA_DATA_REL: &str = "http://schemas.microsoft.com/office/2006/relationships/vbaData";
const VBA_PROJECT_SIGNATURE_REL: &str =
    "http://schemas.microsoft.com/office/2006/relationships/vbaProjectSignature";
const VBA_PROJECT_SIGNATURE_AGILE_REL: &str =
    "http://schemas.microsoft.com/office/2014/relationships/vbaProjectSignatureAgile";
const VBA_PROJECT_SIGNATURE_V3_REL: &str =
    "http://schemas.microsoft.com/office/2020/07/relationships/vbaProjectSignatureV3";
const VBA_PROJECT_CONTENT_TYPE: &str = "application/vnd.ms-office.vbaProject";
const VBA_DATA_CONTENT_TYPE: &str = "application/vnd.ms-excel.vbaData+xml";
const VBA_PROJECT_SIGNATURE_CONTENT_TYPE: &str = "application/vnd.ms-office.vbaProjectSignature";
const VBA_PROJECT_SIGNATURE_AGILE_CONTENT_TYPE: &str =
    "application/vnd.ms-office.vbaProjectSignatureAgile";
const VBA_PROJECT_SIGNATURE_V3_CONTENT_TYPE: &str =
    "application/vnd.ms-office.vbaProjectSignatureV3";

pub(super) const XLSX_WORKBOOK_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";
pub(super) const XLSM_WORKBOOK_CONTENT_TYPE: &str =
    "application/vnd.ms-excel.sheet.macroEnabled.main+xml";
pub(super) const WORKSHEET_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml";
pub(super) const SHARED_STRINGS_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml";
pub(super) const COMMENTS_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml";

#[derive(Debug, Clone)]
pub(super) struct Relationship {
    pub(super) id: String,
    pub(super) relationship_type: String,
    pub(super) target: String,
    pub(super) external: bool,
}

impl Relationship {
    pub(super) fn is_office_document(&self) -> bool {
        matches!(
            self.relationship_type.as_str(),
            OFFICE_DOCUMENT_REL_TRANSITIONAL | OFFICE_DOCUMENT_REL_STRICT
        )
    }

    pub(super) fn is_worksheet(&self) -> bool {
        matches!(
            self.relationship_type.as_str(),
            WORKSHEET_REL_TRANSITIONAL | WORKSHEET_REL_STRICT
        )
    }

    pub(super) fn is_shared_strings(&self) -> bool {
        matches!(
            self.relationship_type.as_str(),
            SHARED_STRINGS_REL_TRANSITIONAL | SHARED_STRINGS_REL_STRICT
        )
    }

    pub(super) fn is_comments(&self) -> bool {
        matches!(
            self.relationship_type.as_str(),
            COMMENTS_REL_TRANSITIONAL | COMMENTS_REL_STRICT
        )
    }

    pub(super) fn is_threaded_comments(&self) -> bool {
        matches!(
            self.relationship_type.as_str(),
            THREADED_COMMENT_REL
                | THREADED_COMMENTS_REL
                | THREADED_COMMENT_REL_TRANSITIONAL
                | THREADED_COMMENTS_REL_TRANSITIONAL
                | THREADED_COMMENT_REL_STRICT
                | THREADED_COMMENTS_REL_STRICT
        )
    }

    fn is_vba_payload(&self) -> bool {
        matches!(
            self.relationship_type.as_str(),
            VBA_PROJECT_REL
                | VBA_DATA_REL
                | VBA_PROJECT_SIGNATURE_REL
                | VBA_PROJECT_SIGNATURE_AGILE_REL
                | VBA_PROJECT_SIGNATURE_V3_REL
        )
    }
}

#[derive(Debug, Default)]
struct ContentTypes {
    defaults: BTreeMap<String, String>,
    overrides: BTreeMap<String, String>,
    folded_overrides: BTreeSet<String>,
}

impl ContentTypes {
    fn parse(bytes: &[u8], budget: &mut XmlBudget) -> Result<Self, ExcelError> {
        let mut result = Self::default();
        let mut root_checked = false;
        parse_xml(bytes, budget, |element| {
            if !root_checked {
                root_checked = true;
                require_element(
                    element,
                    &[CONTENT_TYPES_NS_TRANSITIONAL, CONTENT_TYPES_NS_STRICT],
                    b"Types",
                )?;
                return Ok(());
            }
            if !namespace_matches(
                element.namespace.as_deref(),
                &[CONTENT_TYPES_NS_TRANSITIONAL, CONTENT_TYPES_NS_STRICT],
            ) {
                return Ok(());
            }
            match element.local_name.as_slice() {
                b"Default" => {
                    let extension = required_attribute(element, b"Extension")?.to_ascii_lowercase();
                    let content_type = required_attribute(element, b"ContentType")?.to_owned();
                    if extension.is_empty()
                        || extension.len() > MAX_CANONICAL_PART_URI_BYTES
                        || result.defaults.insert(extension, content_type).is_some()
                    {
                        return Err(crate::formats::ooxml::PackageError::RequiredPartInvalid);
                    }
                }
                b"Override" => {
                    let raw_part = required_attribute(element, b"PartName")?;
                    let part = canonical_part_uri(raw_part)?;
                    let folded = part.to_ascii_lowercase();
                    let content_type = required_attribute(element, b"ContentType")?.to_owned();
                    if !result.folded_overrides.insert(folded)
                        || result.overrides.insert(part, content_type).is_some()
                    {
                        return Err(crate::formats::ooxml::PackageError::RequiredPartInvalid);
                    }
                }
                _ => {}
            }
            Ok(())
        })
        .map_err(ExcelError::from)?;
        if !root_checked {
            return Err(ExcelError::RequiredPartInvalid);
        }
        Ok(result)
    }

    fn content_type(&self, part_uri: &str) -> Option<&str> {
        if let Some(value) = self.overrides.get(part_uri) {
            return Some(value);
        }
        let extension = part_uri.rsplit_once('.')?.1.to_ascii_lowercase();
        self.defaults.get(&extension).map(String::as_str)
    }
}

pub(super) struct ExcelPackage<'a> {
    inventory: ArchiveInventory<'a>,
    content_types: ContentTypes,
    workbook_uri: String,
    workbook_bytes: Vec<u8>,
    workbook_relationships: Vec<Relationship>,
    selected_parts: usize,
    relationships_total: usize,
    pub(super) xml_budget: XmlBudget,
}

impl<'a> ExcelPackage<'a> {
    pub(super) fn open(bytes: &'a [u8]) -> Result<Self, ExcelError> {
        if bytes.starts_with(CFB_SIGNATURE) {
            return Err(ExcelError::EncryptedPackage);
        }
        let mut inventory = ArchiveInventory::new(bytes).map_err(ExcelError::from)?;
        if !inventory.contains(CONTENT_TYPES_PART) || !inventory.contains(ROOT_RELATIONSHIPS_PART) {
            return Err(ExcelError::NotSpreadsheet);
        }

        let mut xml_budget = XmlBudget::default();
        let mut selected_parts = 0_usize;
        let content_types_bytes =
            open_xml(&mut inventory, CONTENT_TYPES_PART, &mut selected_parts)?;
        let content_types = ContentTypes::parse(&content_types_bytes, &mut xml_budget)?;
        let root_bytes = open_xml(&mut inventory, ROOT_RELATIONSHIPS_PART, &mut selected_parts)?;
        let mut relationships_total = 0_usize;
        let root_relationships =
            parse_relationships(&root_bytes, &mut xml_budget, &mut relationships_total)?;
        let mut office_documents = root_relationships
            .iter()
            .filter(|relationship| relationship.is_office_document());
        let Some(office_document) = office_documents.next() else {
            return Err(ExcelError::NotSpreadsheet);
        };
        if office_documents.next().is_some() || office_document.external {
            return Err(ExcelError::RequiredPartInvalid);
        }
        let workbook_uri =
            resolve_relationship_target("/", &office_document.target).map_err(ExcelError::from)?;
        if is_vba_payload_uri(&workbook_uri) {
            return Err(ExcelError::RequiredPartInvalid);
        }
        if !inventory.contains(&workbook_uri) {
            return Err(ExcelError::NotSpreadsheet);
        }
        let workbook_type = content_types.content_type(&workbook_uri);
        if !matches!(
            workbook_type,
            Some(XLSX_WORKBOOK_CONTENT_TYPE | XLSM_WORKBOOK_CONTENT_TYPE)
        ) {
            return Err(ExcelError::NotSpreadsheet);
        }
        let workbook_bytes = open_xml(&mut inventory, &workbook_uri, &mut selected_parts)?;

        let relationships_uri = relationships_part_uri(&workbook_uri).map_err(ExcelError::from)?;
        let workbook_relationships = if inventory.contains(&relationships_uri) {
            let bytes = open_xml(&mut inventory, &relationships_uri, &mut selected_parts)?;
            parse_relationships(&bytes, &mut xml_budget, &mut relationships_total)?
        } else {
            Vec::new()
        };

        Ok(Self {
            inventory,
            content_types,
            workbook_uri,
            workbook_bytes,
            workbook_relationships,
            selected_parts,
            relationships_total,
            xml_budget,
        })
    }

    pub(super) fn workbook_uri(&self) -> &str {
        &self.workbook_uri
    }

    pub(super) fn workbook_bytes(&self) -> &[u8] {
        &self.workbook_bytes
    }

    pub(super) fn workbook_relationship(&self, id: &str) -> Option<&Relationship> {
        self.workbook_relationships
            .iter()
            .find(|relationship| relationship.id == id)
    }

    pub(super) fn workbook_relationships(&self) -> &[Relationship] {
        &self.workbook_relationships
    }

    pub(super) fn resolve_internal(
        &self,
        source_part: &str,
        relationship: &Relationship,
    ) -> Result<String, ExcelError> {
        if relationship.external || relationship.is_vba_payload() {
            return Err(ExcelError::RequiredPartInvalid);
        }
        let target = resolve_relationship_target(source_part, &relationship.target)
            .map_err(ExcelError::from)?;
        if is_vba_payload_uri(&target) {
            return Err(ExcelError::RequiredPartInvalid);
        }
        Ok(target)
    }

    pub(super) fn contains(&self, uri: &str) -> bool {
        self.inventory.contains(uri)
    }

    pub(super) fn open_typed_xml(
        &mut self,
        uri: &str,
        expected_content_type: &str,
    ) -> Result<Vec<u8>, ExcelError> {
        let content_type = self.content_types.content_type(uri);
        if is_vba_payload_uri(uri)
            || content_type.is_some_and(is_vba_payload_content_type)
            || content_type != Some(expected_content_type)
        {
            return Err(ExcelError::RequiredPartInvalid);
        }
        open_xml(&mut self.inventory, uri, &mut self.selected_parts)
    }

    pub(super) fn relationships_for_part(
        &mut self,
        source_part: &str,
    ) -> Result<Vec<Relationship>, ExcelError> {
        let uri = relationships_part_uri(source_part).map_err(ExcelError::from)?;
        if !self.inventory.contains(&uri) {
            return Ok(Vec::new());
        }
        let bytes = open_xml(&mut self.inventory, &uri, &mut self.selected_parts)?;
        parse_relationships(&bytes, &mut self.xml_budget, &mut self.relationships_total)
    }
}

fn is_vba_payload_uri(uri: &str) -> bool {
    uri.rsplit('/').next().is_some_and(|name| {
        [
            "vbaProject.bin",
            "vbaData.xml",
            "vbaProjectSignature.bin",
            "vbaProjectSignatureAgile.bin",
            "vbaProjectSignatureV3.bin",
        ]
        .iter()
        .any(|forbidden| name.eq_ignore_ascii_case(forbidden))
    })
}

fn is_vba_payload_content_type(content_type: &str) -> bool {
    matches!(
        content_type,
        VBA_PROJECT_CONTENT_TYPE
            | VBA_DATA_CONTENT_TYPE
            | VBA_PROJECT_SIGNATURE_CONTENT_TYPE
            | VBA_PROJECT_SIGNATURE_AGILE_CONTENT_TYPE
            | VBA_PROJECT_SIGNATURE_V3_CONTENT_TYPE
    )
}

fn open_xml(
    inventory: &mut ArchiveInventory<'_>,
    uri: &str,
    selected_parts: &mut usize,
) -> Result<Vec<u8>, ExcelError> {
    *selected_parts = selected_parts
        .checked_add(1)
        .filter(|count| *count <= MAX_SELECTED_XML_PARTS)
        .ok_or(ExcelError::PackageLimitExceeded)?;
    inventory.open_selected_xml(uri).map_err(ExcelError::from)
}

fn parse_relationships(
    bytes: &[u8],
    budget: &mut XmlBudget,
    aggregate: &mut usize,
) -> Result<Vec<Relationship>, ExcelError> {
    let mut relationships = Vec::new();
    let mut ids = BTreeSet::new();
    let mut root_checked = false;
    parse_xml(bytes, budget, |element| {
        if !root_checked {
            root_checked = true;
            require_element(
                element,
                &[RELATIONSHIPS_NS_TRANSITIONAL, RELATIONSHIPS_NS_STRICT],
                b"Relationships",
            )?;
            return Ok(());
        }
        if !namespace_matches(
            element.namespace.as_deref(),
            &[RELATIONSHIPS_NS_TRANSITIONAL, RELATIONSHIPS_NS_STRICT],
        ) || element.local_name != b"Relationship"
        {
            return Ok(());
        }
        if relationships.len() == MAX_RELATIONSHIPS_PER_PART {
            return Err(crate::formats::ooxml::PackageError::PackageLimitExceeded);
        }
        *aggregate = aggregate
            .checked_add(1)
            .filter(|count| *count <= MAX_RELATIONSHIPS_TOTAL)
            .ok_or(crate::formats::ooxml::PackageError::PackageLimitExceeded)?;
        let id = required_attribute(element, b"Id")?;
        if !ids.insert(id.to_owned()) {
            return Err(crate::formats::ooxml::PackageError::RequiredPartInvalid);
        }
        let target = required_attribute(element, b"Target")?;
        if target.is_empty() || target.len() > MAX_RELATIONSHIP_TARGET_BYTES {
            return Err(crate::formats::ooxml::PackageError::PackageLimitExceeded);
        }
        let external = match optional_attribute(element, b"TargetMode") {
            None => false,
            Some("External") => true,
            Some(_) => return Err(crate::formats::ooxml::PackageError::RequiredPartInvalid),
        };
        relationships.push(Relationship {
            id: id.to_owned(),
            relationship_type: required_attribute(element, b"Type")?.to_owned(),
            target: target.to_owned(),
            external,
        });
        Ok(())
    })
    .map_err(ExcelError::from)?;
    if !root_checked {
        return Err(ExcelError::RequiredPartInvalid);
    }
    Ok(relationships)
}

fn required_attribute<'a>(
    element: &'a XmlElement,
    name: &[u8],
) -> Result<&'a str, crate::formats::ooxml::PackageError> {
    optional_attribute(element, name)
        .ok_or(crate::formats::ooxml::PackageError::RequiredPartInvalid)
}

pub(super) fn optional_attribute<'a>(element: &'a XmlElement, name: &[u8]) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|(key, _)| key.as_slice() == name)
        .map(|(_, value)| value.as_str())
}

fn require_element(
    element: &XmlElement,
    namespaces: &[&[u8]],
    local_name: &[u8],
) -> Result<(), crate::formats::ooxml::PackageError> {
    if element.local_name == local_name
        && namespace_matches(element.namespace.as_deref(), namespaces)
    {
        Ok(())
    } else {
        Err(crate::formats::ooxml::PackageError::RequiredPartInvalid)
    }
}

fn namespace_matches(namespace: Option<&[u8]>, expected: &[&[u8]]) -> bool {
    namespace.is_some_and(|namespace| expected.contains(&namespace))
}

fn canonical_part_uri(uri: &str) -> Result<String, crate::formats::ooxml::PackageError> {
    if !uri.starts_with('/') || uri.ends_with('/') || uri.contains(['\\', '\0', '?', '#']) {
        return Err(crate::formats::ooxml::PackageError::RequiredPartInvalid);
    }
    resolve_relationship_target("/", uri)
}
