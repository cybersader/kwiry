// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::extract::ExtractionNotice;

use super::error::DocxError;
use super::limits::{
    MAX_CANONICAL_PART_URI_BYTES, MAX_NOTICES, MAX_RELATIONSHIP_TARGET_BYTES,
    MAX_RELATIONSHIPS_PER_PART, MAX_RELATIONSHIPS_TOTAL, MAX_SELECTED_XML_PARTS,
};
use super::wordprocessing;
use super::xml::{XmlBudget, XmlElement, parse_xml};
use super::zip::ArchiveInventory;

const CONTENT_TYPES_PART: &str = "/[Content_Types].xml";
const ROOT_RELATIONSHIPS_PART: &str = "/_rels/.rels";
const CONTENT_TYPES_NS_TRANSITIONAL: &[u8] =
    b"http://schemas.openxmlformats.org/package/2006/content-types";
const CONTENT_TYPES_NS_STRICT: &[u8] = b"http://purl.oclc.org/ooxml/package/content-types";
const RELATIONSHIPS_NS_TRANSITIONAL: &[u8] =
    b"http://schemas.openxmlformats.org/package/2006/relationships";
const RELATIONSHIPS_NS_STRICT: &[u8] = b"http://purl.oclc.org/ooxml/package/relationships";
const OFFICE_REL_TRANSITIONAL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
const OFFICE_REL_STRICT: &str = "http://purl.oclc.org/ooxml/officeDocument/relationships/";
const MAIN_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum PackagePartKind {
    Document,
    Header,
    Footer,
    Footnotes,
    Endnotes,
    Comments,
    Styles,
    CoreProperties,
    ExtendedProperties,
    CustomProperties,
}

impl PackagePartKind {
    fn wordprocessing(self) -> Option<wordprocessing::PartKind> {
        match self {
            Self::Document => Some(wordprocessing::PartKind::Document),
            Self::Header => Some(wordprocessing::PartKind::Header),
            Self::Footer => Some(wordprocessing::PartKind::Footer),
            Self::Footnotes => Some(wordprocessing::PartKind::Footnotes),
            Self::Endnotes => Some(wordprocessing::PartKind::Endnotes),
            Self::Comments => Some(wordprocessing::PartKind::Comments),
            Self::Styles => Some(wordprocessing::PartKind::Styles),
            Self::CoreProperties | Self::ExtendedProperties | Self::CustomProperties => None,
        }
    }
}

#[derive(Debug)]
pub(super) struct PackagePart {
    pub(super) uri: String,
    pub(super) kind: PackagePartKind,
    pub(super) bytes: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct PackageFoundation {
    pub(super) main_part: String,
    pub(super) parts: BTreeMap<String, PackagePart>,
    relationship_targets: BTreeMap<String, BTreeMap<String, (PackagePartKind, String)>>,
    pub(super) xml_budget: XmlBudget,
    pub(super) links_out: Vec<String>,
    pub(super) notices: Vec<ExtractionNotice>,
}

impl PackageFoundation {
    pub(super) fn part(&self, uri: &str) -> Option<&PackagePart> {
        self.parts.get(uri)
    }

    pub(super) fn parts_of_kind(
        &self,
        kind: PackagePartKind,
    ) -> impl Iterator<Item = &PackagePart> {
        self.parts.values().filter(move |part| part.kind == kind)
    }

    pub(super) fn relationship_target(
        &self,
        source_part: &str,
        relationship_id: &str,
        expected_kind: PackagePartKind,
    ) -> Option<&str> {
        let (kind, target) = self
            .relationship_targets
            .get(source_part)?
            .get(relationship_id)?;
        (*kind == expected_kind).then_some(target.as_str())
    }
}

#[derive(Debug, Clone)]
struct Relationship {
    id: String,
    relationship_type: String,
    target: String,
    external: bool,
}

#[derive(Default)]
struct ContentTypes {
    defaults: BTreeMap<String, String>,
    overrides: BTreeMap<String, String>,
    folded_overrides: BTreeSet<String>,
}

impl PackageFoundation {
    pub(super) fn load(inventory: &mut ArchiveInventory<'_>) -> Result<Self, DocxError> {
        if !inventory.contains(CONTENT_TYPES_PART) || !inventory.contains(ROOT_RELATIONSHIPS_PART) {
            return Err(DocxError::RequiredPartInvalid);
        }

        let mut xml_budget = XmlBudget::default();
        let mut selected_parts = 0_usize;
        let content_types_bytes = open_xml(inventory, CONTENT_TYPES_PART, &mut selected_parts)?;
        let content_types = ContentTypes::parse(&content_types_bytes, &mut xml_budget)?;
        let root_relationships_bytes =
            open_xml(inventory, ROOT_RELATIONSHIPS_PART, &mut selected_parts)?;
        let mut relationships_total = 0_usize;
        let root_relationships = parse_relationships(
            &root_relationships_bytes,
            &mut xml_budget,
            &mut relationships_total,
        )?;

        let mut office_documents = root_relationships.iter().filter(|relationship| {
            relationship_suffix(&relationship.relationship_type) == Some("officeDocument")
        });
        let office_document = office_documents
            .next()
            .ok_or(DocxError::RequiredPartInvalid)?;
        if office_documents.next().is_some() || office_document.external {
            return Err(DocxError::RequiredPartInvalid);
        }
        let main_part = resolve_relationship_target("/", &office_document.target)?;
        if !inventory.contains(&main_part)
            || content_types.content_type(&main_part) != Some(MAIN_CONTENT_TYPE)
        {
            return Err(DocxError::RequiredPartInvalid);
        }

        let main_bytes = open_xml(inventory, &main_part, &mut selected_parts)?;
        wordprocessing::validate_part(
            &main_bytes,
            wordprocessing::PartKind::Document,
            &mut xml_budget,
        )?;

        let mut parts = BTreeMap::from([(
            main_part.clone(),
            PackagePart {
                uri: main_part.clone(),
                kind: PackagePartKind::Document,
                bytes: main_bytes,
            },
        )]);
        let mut visited_parts = BTreeMap::from([(main_part.clone(), PackagePartKind::Document)]);
        let mut relationship_targets =
            BTreeMap::<String, BTreeMap<String, (PackagePartKind, String)>>::new();
        let mut links_out = Vec::new();
        let mut seen_links = BTreeSet::new();
        let mut notices = Vec::new();

        for relationship in &root_relationships {
            // Screened here as well as during traversal: relocating a forbidden
            // relationship into the package root must not evade the policy.
            if relationship_suffix(&relationship.relationship_type)
                .is_some_and(is_forbidden_relationship)
            {
                return Err(DocxError::RequiredPartInvalid);
            }
            let Some(kind) = package_relationship_kind(&relationship.relationship_type) else {
                continue;
            };
            if kind == PackagePartKind::Document {
                continue;
            }
            if relationship.external {
                return Err(DocxError::RequiredPartInvalid);
            }
            select_related_part(
                inventory,
                &content_types,
                "/",
                relationship,
                kind,
                &mut selected_parts,
                &mut xml_budget,
                &mut visited_parts,
                &mut parts,
                &mut relationship_targets,
                &mut notices,
            )?;
        }

        let mut pending = VecDeque::from([main_part.clone()]);
        while let Some(source_part) = pending.pop_front() {
            let relationships_part = relationships_part_uri(&source_part)?;
            if !inventory.contains(&relationships_part) {
                continue;
            }
            let relationships_bytes =
                open_xml(inventory, &relationships_part, &mut selected_parts)?;
            let relationships = parse_relationships(
                &relationships_bytes,
                &mut xml_budget,
                &mut relationships_total,
            )?;
            for relationship in relationships {
                let suffix = relationship_suffix(&relationship.relationship_type);
                if suffix.is_some_and(is_forbidden_relationship) {
                    return Err(DocxError::RequiredPartInvalid);
                }
                if relationship.external {
                    if suffix == Some("hyperlink") {
                        if let Some(target) = inert_external_target(&relationship.target) {
                            // Ordered output with a set-backed membership test:
                            // a linear scan over full-length targets is
                            // quadratic in the attacker-controlled link count.
                            if seen_links.insert(target.clone()) {
                                links_out.push(target);
                            }
                        } else {
                            add_notice(
                                &mut notices,
                                "docx_unresolved_relationship",
                                "ignored an external hyperlink with an unsupported target scheme",
                            );
                        }
                    } else if package_relationship_kind(&relationship.relationship_type).is_some() {
                        return Err(DocxError::RequiredPartInvalid);
                    } else {
                        add_notice(
                            &mut notices,
                            "docx_unresolved_relationship",
                            "ignored an external relationship that is not a hyperlink",
                        );
                    }
                    continue;
                }

                let Some(kind) = package_relationship_kind(&relationship.relationship_type) else {
                    continue;
                };
                let was_new = select_related_part(
                    inventory,
                    &content_types,
                    &source_part,
                    &relationship,
                    kind,
                    &mut selected_parts,
                    &mut xml_budget,
                    &mut visited_parts,
                    &mut parts,
                    &mut relationship_targets,
                    &mut notices,
                )?;
                if was_new && kind.wordprocessing().is_some() {
                    pending.push_back(
                        relationship_targets[&source_part][&relationship.id]
                            .1
                            .clone(),
                    );
                }
            }
        }

        Ok(Self {
            main_part,
            parts,
            relationship_targets,
            xml_budget,
            links_out,
            notices,
        })
    }
}

impl ContentTypes {
    fn parse(bytes: &[u8], budget: &mut XmlBudget) -> Result<Self, DocxError> {
        let mut result = Self::default();
        let mut element_index = 0_usize;
        parse_xml(bytes, budget, |element| {
            element_index = element_index
                .checked_add(1)
                .ok_or(DocxError::XmlLimitExceeded)?;
            if element_index == 1 {
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
                        return Err(DocxError::RequiredPartInvalid);
                    }
                }
                b"Override" => {
                    let raw_part = required_attribute(element, b"PartName")?;
                    let part = canonicalize_part_uri(raw_part)?;
                    let folded = part.to_ascii_lowercase();
                    let content_type = required_attribute(element, b"ContentType")?.to_owned();
                    if !result.folded_overrides.insert(folded)
                        || result.overrides.insert(part, content_type).is_some()
                    {
                        return Err(DocxError::RequiredPartInvalid);
                    }
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(result)
    }

    fn content_type(&self, part_uri: &str) -> Option<&str> {
        if let Some(content_type) = self.overrides.get(part_uri) {
            return Some(content_type);
        }
        let extension = part_uri.rsplit_once('.')?.1.to_ascii_lowercase();
        self.defaults.get(&extension).map(String::as_str)
    }
}

fn parse_relationships(
    bytes: &[u8],
    budget: &mut XmlBudget,
    aggregate_count: &mut usize,
) -> Result<Vec<Relationship>, DocxError> {
    let mut relationships = Vec::new();
    let mut ids = BTreeSet::new();
    let mut element_index = 0_usize;
    parse_xml(bytes, budget, |element| {
        element_index = element_index
            .checked_add(1)
            .ok_or(DocxError::XmlLimitExceeded)?;
        if element_index == 1 {
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
            return Err(DocxError::PackageLimitExceeded);
        }
        *aggregate_count = aggregate_count
            .checked_add(1)
            .filter(|count| *count <= MAX_RELATIONSHIPS_TOTAL)
            .ok_or(DocxError::PackageLimitExceeded)?;
        let id = required_attribute(element, b"Id")?;
        if !ids.insert(id.to_owned()) {
            return Err(DocxError::RequiredPartInvalid);
        }
        let relationship_type = required_attribute(element, b"Type")?.to_owned();
        let target = required_attribute(element, b"Target")?.to_owned();
        if target.is_empty() || target.len() > MAX_RELATIONSHIP_TARGET_BYTES {
            return Err(DocxError::PackageLimitExceeded);
        }
        let external = match optional_attribute(element, b"TargetMode") {
            None => false,
            Some("External") => true,
            Some(_) => return Err(DocxError::RequiredPartInvalid),
        };
        relationships.push(Relationship {
            id: id.to_owned(),
            relationship_type,
            target,
            external,
        });
        Ok(())
    })?;
    Ok(relationships)
}

#[allow(clippy::too_many_arguments)]
fn select_related_part(
    inventory: &mut ArchiveInventory<'_>,
    content_types: &ContentTypes,
    source_part: &str,
    relationship: &Relationship,
    kind: PackagePartKind,
    selected_parts: &mut usize,
    xml_budget: &mut XmlBudget,
    visited_parts: &mut BTreeMap<String, PackagePartKind>,
    parts: &mut BTreeMap<String, PackagePart>,
    relationship_targets: &mut BTreeMap<String, BTreeMap<String, (PackagePartKind, String)>>,
    notices: &mut Vec<ExtractionNotice>,
) -> Result<bool, DocxError> {
    let target = resolve_relationship_target(source_part, &relationship.target)?;
    if !inventory.contains(&target) {
        add_notice(
            notices,
            "docx_optional_part_unavailable",
            "an optional related DOCX text or metadata part is unavailable",
        );
        return Ok(false);
    }
    if !content_type_matches_kind(content_types.content_type(&target), kind) {
        return Err(DocxError::RequiredPartInvalid);
    }
    relationship_targets
        .entry(source_part.to_owned())
        .or_default()
        .insert(relationship.id.clone(), (kind, target.clone()));

    if let Some(existing_kind) = visited_parts.get(&target) {
        if *existing_kind != kind {
            return Err(DocxError::RequiredPartInvalid);
        }
        return Ok(false);
    }

    let bytes = open_xml(inventory, &target, selected_parts)?;
    validate_selected_part(&bytes, kind, xml_budget)?;
    visited_parts.insert(target.clone(), kind);
    parts.insert(
        target.clone(),
        PackagePart {
            uri: target,
            kind,
            bytes,
        },
    );
    Ok(true)
}

fn validate_selected_part(
    bytes: &[u8],
    kind: PackagePartKind,
    budget: &mut XmlBudget,
) -> Result<(), DocxError> {
    if let Some(kind) = kind.wordprocessing() {
        return wordprocessing::validate_part(bytes, kind, budget);
    }
    let (root, namespaces): (&[u8], &[&[u8]]) = match kind {
        PackagePartKind::CoreProperties => (
            b"coreProperties",
            &[
                b"http://schemas.openxmlformats.org/package/2006/metadata/core-properties",
                b"http://purl.oclc.org/ooxml/package/metadata/core-properties",
            ],
        ),
        PackagePartKind::ExtendedProperties => (
            b"Properties",
            &[
                b"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties",
                b"http://purl.oclc.org/ooxml/officeDocument/extended-properties",
            ],
        ),
        PackagePartKind::CustomProperties => (
            b"Properties",
            &[
                b"http://schemas.openxmlformats.org/officeDocument/2006/custom-properties",
                b"http://purl.oclc.org/ooxml/officeDocument/custom-properties",
            ],
        ),
        _ => return Err(DocxError::RequiredPartInvalid),
    };
    let mut root_checked = false;
    parse_xml(bytes, budget, |element| {
        if !root_checked {
            root_checked = true;
            require_element(element, namespaces, root)?;
        }
        Ok(())
    })?;
    root_checked
        .then_some(())
        .ok_or(DocxError::RequiredPartInvalid)
}

fn open_xml(
    inventory: &mut ArchiveInventory<'_>,
    part_uri: &str,
    selected_parts: &mut usize,
) -> Result<Vec<u8>, DocxError> {
    *selected_parts = selected_parts
        .checked_add(1)
        .filter(|count| *count <= MAX_SELECTED_XML_PARTS)
        .ok_or(DocxError::PackageLimitExceeded)?;
    inventory.open_selected_xml(part_uri)
}

fn required_attribute<'a>(element: &'a XmlElement, name: &[u8]) -> Result<&'a str, DocxError> {
    optional_attribute(element, name).ok_or(DocxError::RequiredPartInvalid)
}

fn optional_attribute<'a>(element: &'a XmlElement, name: &[u8]) -> Option<&'a str> {
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
) -> Result<(), DocxError> {
    if element.local_name == local_name
        && namespace_matches(element.namespace.as_deref(), namespaces)
    {
        Ok(())
    } else {
        Err(DocxError::RequiredPartInvalid)
    }
}

fn namespace_matches(namespace: Option<&[u8]>, expected: &[&[u8]]) -> bool {
    namespace.is_some_and(|namespace| expected.contains(&namespace))
}

fn relationship_suffix(relationship_type: &str) -> Option<&str> {
    relationship_type
        .strip_prefix(OFFICE_REL_TRANSITIONAL)
        .or_else(|| relationship_type.strip_prefix(OFFICE_REL_STRICT))
}

fn package_relationship_kind(relationship_type: &str) -> Option<PackagePartKind> {
    if matches!(
        relationship_type,
        "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties"
            | "http://purl.oclc.org/ooxml/package/relationships/metadata/core-properties"
    ) {
        return Some(PackagePartKind::CoreProperties);
    }
    match relationship_suffix(relationship_type)? {
        "officeDocument" => Some(PackagePartKind::Document),
        "header" => Some(PackagePartKind::Header),
        "footer" => Some(PackagePartKind::Footer),
        "footnotes" => Some(PackagePartKind::Footnotes),
        "endnotes" => Some(PackagePartKind::Endnotes),
        "comments" => Some(PackagePartKind::Comments),
        "styles" => Some(PackagePartKind::Styles),
        "extended-properties" => Some(PackagePartKind::ExtendedProperties),
        "custom-properties" => Some(PackagePartKind::CustomProperties),
        _ => None,
    }
}

fn is_forbidden_relationship(suffix: &str) -> bool {
    matches!(
        suffix,
        "vbaProject" | "oleObject" | "package" | "control" | "attachedTemplate"
    )
}

fn content_type_matches_kind(content_type: Option<&str>, kind: PackagePartKind) -> bool {
    let expected = match kind {
        PackagePartKind::Document => MAIN_CONTENT_TYPE,
        PackagePartKind::Header => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"
        }
        PackagePartKind::Footer => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"
        }
        PackagePartKind::Footnotes => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"
        }
        PackagePartKind::Endnotes => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml"
        }
        PackagePartKind::Comments => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"
        }
        PackagePartKind::Styles => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"
        }
        PackagePartKind::CoreProperties => {
            "application/vnd.openxmlformats-package.core-properties+xml"
        }
        PackagePartKind::ExtendedProperties => {
            "application/vnd.openxmlformats-officedocument.extended-properties+xml"
        }
        PackagePartKind::CustomProperties => {
            "application/vnd.openxmlformats-officedocument.custom-properties+xml"
        }
    };
    content_type == Some(expected)
}

fn inert_external_target(target: &str) -> Option<String> {
    if target.len() > MAX_RELATIONSHIP_TARGET_BYTES {
        return None;
    }
    let scheme = target.split_once(':')?.0.to_ascii_lowercase();
    matches!(scheme.as_str(), "http" | "https" | "mailto").then(|| target.to_owned())
}

fn add_notice(notices: &mut Vec<ExtractionNotice>, code: &'static str, message: &'static str) {
    if notices.len() < MAX_NOTICES - 1 {
        notices.push(ExtractionNotice::new(code, message));
    } else if notices.len() == MAX_NOTICES - 1 {
        notices.push(ExtractionNotice::new(
            "docx_notices_truncated",
            "additional DOCX extraction notices were omitted",
        ));
    }
}

pub(super) fn canonicalize_zip_name(name: &str, is_directory: bool) -> Result<String, DocxError> {
    if name.is_empty()
        || name.starts_with('/')
        || name.contains(['\\', '\0', '?', '#'])
        || (!is_directory && name.ends_with('/'))
    {
        return Err(DocxError::InvalidPackage);
    }
    let body = if is_directory {
        name.strip_suffix('/').ok_or(DocxError::InvalidPackage)?
    } else {
        name
    };
    let mut canonical = String::from("/");
    let mut first = true;
    for segment in body.split('/') {
        let segment = canonicalize_segment(segment)?;
        if !first {
            canonical.push('/');
        }
        canonical.push_str(&segment);
        first = false;
    }
    if is_directory {
        canonical.push('/');
    }
    if canonical.len() > MAX_CANONICAL_PART_URI_BYTES {
        return Err(DocxError::PackageLimitExceeded);
    }
    Ok(canonical)
}

fn canonicalize_part_uri(uri: &str) -> Result<String, DocxError> {
    if !uri.starts_with('/') || uri.ends_with('/') || uri.contains(['\\', '\0', '?', '#']) {
        return Err(DocxError::RequiredPartInvalid);
    }
    let mut canonical = String::from("/");
    let mut first = true;
    for segment in uri[1..].split('/') {
        let segment = canonicalize_segment(segment)?;
        if !first {
            canonical.push('/');
        }
        canonical.push_str(&segment);
        first = false;
    }
    if canonical.len() > MAX_CANONICAL_PART_URI_BYTES {
        return Err(DocxError::PackageLimitExceeded);
    }
    Ok(canonical)
}

fn canonicalize_segment(segment: &str) -> Result<String, DocxError> {
    if segment.is_empty() {
        return Err(DocxError::InvalidPackage);
    }
    let bytes = segment.as_bytes();
    let mut output = String::with_capacity(segment.len());
    let mut index = 0_usize;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            let character = segment[index..]
                .chars()
                .next()
                .ok_or(DocxError::InvalidPackage)?;
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        let encoded = bytes
            .get(index + 1..index + 3)
            .ok_or(DocxError::InvalidPackage)?;
        let high = hex_value(encoded[0]).ok_or(DocxError::InvalidPackage)?;
        let low = hex_value(encoded[1]).ok_or(DocxError::InvalidPackage)?;
        let decoded = (high << 4) | low;
        if matches!(decoded, b'/' | b'\\' | b'\0') {
            return Err(DocxError::InvalidPackage);
        }
        if decoded.is_ascii_alphanumeric() || matches!(decoded, b'-' | b'.' | b'_' | b'~') {
            output.push(char::from(decoded));
        } else {
            output.push('%');
            output.push(char::from(encoded[0]).to_ascii_uppercase());
            output.push(char::from(encoded[1]).to_ascii_uppercase());
        }
        index += 3;
    }
    if output == "." || output == ".." {
        return Err(DocxError::InvalidPackage);
    }
    Ok(output)
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn resolve_relationship_target(source_part: &str, target: &str) -> Result<String, DocxError> {
    if target.is_empty()
        || target.len() > MAX_RELATIONSHIP_TARGET_BYTES
        || target.contains(['\\', '\0', '?', '#'])
    {
        return Err(DocxError::RequiredPartInvalid);
    }
    let mut segments = if target.starts_with('/') || source_part == "/" {
        Vec::new()
    } else {
        source_part[1..]
            .rsplit_once('/')
            .map_or(Vec::new(), |(parent, _)| {
                parent.split('/').map(str::to_owned).collect()
            })
    };
    for raw_segment in target.trim_start_matches('/').split('/') {
        if raw_segment.is_empty() || raw_segment == "." {
            return Err(DocxError::RequiredPartInvalid);
        }
        if raw_segment == ".." {
            segments.pop().ok_or(DocxError::RequiredPartInvalid)?;
            continue;
        }
        let segment = canonicalize_segment(raw_segment).map_err(|error| match error {
            DocxError::PackageLimitExceeded => error,
            _ => DocxError::RequiredPartInvalid,
        })?;
        segments.push(segment);
    }
    let resolved = format!("/{}", segments.join("/"));
    if resolved.len() > MAX_CANONICAL_PART_URI_BYTES {
        return Err(DocxError::PackageLimitExceeded);
    }
    Ok(resolved)
}

fn relationships_part_uri(source_part: &str) -> Result<String, DocxError> {
    if source_part == "/" {
        return Ok(ROOT_RELATIONSHIPS_PART.to_owned());
    }
    let (parent, name) = source_part
        .rsplit_once('/')
        .ok_or(DocxError::RequiredPartInvalid)?;
    let relationship_part = format!("{parent}/_rels/{name}.rels");
    if relationship_part.len() > MAX_CANONICAL_PART_URI_BYTES {
        return Err(DocxError::PackageLimitExceeded);
    }
    Ok(relationship_part)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::docx::test_support::{TestEntry, build_zip};

    fn relationships(count: usize, target_length: usize) -> String {
        let target = "x".repeat(target_length);
        let items = (0..count)
            .map(|index| {
                format!(r#"<Relationship Id="r{index}" Type="urn:test" Target="{target}"/>"#,)
            })
            .collect::<String>();
        let namespace = std::str::from_utf8(RELATIONSHIPS_NS_TRANSITIONAL).expect("namespace");
        format!(r#"<Relationships xmlns="{namespace}">{items}</Relationships>"#)
    }

    #[test]
    fn relationship_count_and_target_boundaries_are_enforced() {
        let source = relationships(MAX_RELATIONSHIPS_PER_PART, 1);
        let mut aggregate = MAX_RELATIONSHIPS_TOTAL - MAX_RELATIONSHIPS_PER_PART;
        assert_eq!(
            parse_relationships(
                &source.into_bytes(),
                &mut XmlBudget::default(),
                &mut aggregate
            )
            .expect("relationship limit")
            .len(),
            MAX_RELATIONSHIPS_PER_PART
        );
        assert_eq!(aggregate, MAX_RELATIONSHIPS_TOTAL);

        // The aggregate cap must reject, not merely land on, its limit.
        let source = relationships(1, 1);
        let mut exhausted = MAX_RELATIONSHIPS_TOTAL;
        assert_eq!(
            parse_relationships(
                &source.into_bytes(),
                &mut XmlBudget::default(),
                &mut exhausted
            )
            .unwrap_err(),
            DocxError::PackageLimitExceeded
        );

        let source = relationships(MAX_RELATIONSHIPS_PER_PART + 1, 1);
        assert_eq!(
            parse_relationships(&source.into_bytes(), &mut XmlBudget::default(), &mut 0)
                .unwrap_err(),
            DocxError::PackageLimitExceeded
        );

        let source = relationships(1, MAX_RELATIONSHIP_TARGET_BYTES);
        assert!(
            parse_relationships(&source.into_bytes(), &mut XmlBudget::default(), &mut 0).is_ok()
        );
        let source = relationships(1, MAX_RELATIONSHIP_TARGET_BYTES + 1);
        assert_eq!(
            parse_relationships(&source.into_bytes(), &mut XmlBudget::default(), &mut 0)
                .unwrap_err(),
            DocxError::PackageLimitExceeded
        );
    }

    #[test]
    fn canonical_part_uri_boundaries_are_enforced() {
        let at_limit = "x".repeat(MAX_CANONICAL_PART_URI_BYTES - 1);
        assert_eq!(
            canonicalize_zip_name(&at_limit, false)
                .expect("URI limit")
                .len(),
            MAX_CANONICAL_PART_URI_BYTES
        );
        let over_limit = "x".repeat(MAX_CANONICAL_PART_URI_BYTES);
        assert_eq!(
            canonicalize_zip_name(&over_limit, false).unwrap_err(),
            DocxError::PackageLimitExceeded
        );
        assert_eq!(
            resolve_relationship_target("/", &at_limit)
                .expect("target URI limit")
                .len(),
            MAX_CANONICAL_PART_URI_BYTES
        );
        assert_eq!(
            resolve_relationship_target("/", &over_limit).unwrap_err(),
            DocxError::PackageLimitExceeded
        );
    }

    #[test]
    fn selected_part_and_notice_boundaries_are_enforced() {
        let package = build_zip(&[TestEntry::stored("part.xml", b"<a/>")]);
        let mut inventory = ArchiveInventory::new(&package.bytes).expect("inventory");
        let mut selected = MAX_SELECTED_XML_PARTS - 1;
        assert!(open_xml(&mut inventory, "/part.xml", &mut selected).is_ok());
        assert_eq!(selected, MAX_SELECTED_XML_PARTS);
        assert_eq!(
            open_xml(&mut inventory, "/part.xml", &mut selected).unwrap_err(),
            DocxError::PackageLimitExceeded
        );

        let mut notices = Vec::new();
        for _ in 0..(MAX_NOTICES + 5) {
            add_notice(&mut notices, "test", "test notice");
        }
        assert_eq!(notices.len(), MAX_NOTICES);
        assert_eq!(notices[MAX_NOTICES - 1].code, "docx_notices_truncated");
    }
}
