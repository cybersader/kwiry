// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::error::DocxError;
use super::limits::{MAX_PROPERTY_BYTES, MAX_PROPERTY_ENTRIES};
use super::opc::{PackageFoundation, PackagePartKind};
use super::xml::{XmlBudget, XmlElement, XmlEvent, parse_xml_events};

impl DocxProperties {
    /// Projects package metadata into the shared property bag. DOCX-specific
    /// keys stay under a `docx` namespace so they cannot collide with the
    /// format-neutral properties other extractors own.
    pub(super) fn into_property_map(
        self,
    ) -> std::collections::BTreeMap<String, crate::model::PropertyValue> {
        use crate::model::PropertyValue;
        let mut map = std::collections::BTreeMap::new();
        if let Some(title) = self.title {
            map.insert("title".to_owned(), PropertyValue::String(title));
        }
        for (key, values) in self.docx {
            map.insert(
                key,
                PropertyValue::Sequence(values.into_iter().map(PropertyValue::String).collect()),
            );
        }
        map
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocxProperties {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub docx: BTreeMap<String, Vec<String>>,
}

pub(super) fn extract_properties(
    package: &PackageFoundation,
    budget: &mut XmlBudget,
) -> Result<DocxProperties, DocxError> {
    let mut properties = DocxProperties::default();
    for (kind, prefix) in [
        (PackagePartKind::CoreProperties, "core"),
        (PackagePartKind::ExtendedProperties, "app"),
        (PackagePartKind::CustomProperties, "custom"),
    ] {
        let parts = package.parts_of_kind(kind).collect::<Vec<_>>();
        if parts.len() > 1 {
            return Err(DocxError::RequiredPartInvalid);
        }
        if let Some(part) = parts.first() {
            parse_property_part(&part.bytes, kind, prefix, budget, &mut properties)?;
        }
    }
    Ok(properties)
}

fn parse_property_part(
    bytes: &[u8],
    kind: PackagePartKind,
    prefix: &str,
    budget: &mut XmlBudget,
    properties: &mut DocxProperties,
) -> Result<(), DocxError> {
    let mut stack = Vec::<String>::new();
    let mut custom_name: Option<String> = None;
    let mut custom_depth: Option<usize> = None;
    let mut text = String::new();
    let mut property_entries = 0usize;
    let mut property_bytes = 0usize;

    parse_xml_events(bytes, budget, |event| {
        match event {
            XmlEvent::Start(element) => {
                stack.push(String::from_utf8_lossy(&element.local_name).into_owned());
                if kind == PackagePartKind::CustomProperties && element.local_name == b"property" {
                    custom_name = attribute_local(element, b"name")
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned);
                    custom_depth = Some(stack.len());
                }
                text.clear();
            }
            XmlEvent::Empty(element) => {
                if kind == PackagePartKind::CustomProperties
                    && element.local_name == b"property"
                    && attribute_local(element, b"name").is_none()
                {
                    return Err(DocxError::RequiredPartInvalid);
                }
            }
            XmlEvent::Text(value) => text.push_str(value),
            XmlEvent::End { local_name, .. } => {
                let name = String::from_utf8_lossy(local_name).into_owned();
                if stack.last() != Some(&name) {
                    return Err(DocxError::RequiredPartInvalid);
                }
                let value = text.clone();
                let trimmed = value.trim();
                if !trimmed.is_empty() && stack.len() > 1 {
                    let key = match kind {
                        PackagePartKind::CustomProperties => {
                            custom_name.as_ref().map(|name| format!("{prefix}.{name}"))
                        }
                        _ => Some(format!("{prefix}.{name}")),
                    };
                    if let Some(key) = key {
                        let stored =
                            if kind == PackagePartKind::CoreProperties && name == "keywords" {
                                value.clone()
                            } else {
                                trimmed.to_owned()
                            };
                        // Property names and values are attacker-chosen, so the
                        // map is budgeted like every other extracted output
                        // instead of relying on the shared XML budget alone.
                        property_bytes = property_bytes
                            .checked_add(key.len())
                            .and_then(|total| total.checked_add(stored.len()))
                            .filter(|total| *total <= MAX_PROPERTY_BYTES)
                            .ok_or(DocxError::XmlLimitExceeded)?;
                        let entry = properties.docx.entry(key).or_default();
                        property_entries = property_entries
                            .checked_add(1)
                            .filter(|entries| *entries <= MAX_PROPERTY_ENTRIES)
                            .ok_or(DocxError::XmlLimitExceeded)?;
                        entry.push(stored);
                    }
                    if kind == PackagePartKind::CoreProperties && name == "title" {
                        let candidate = trimmed.to_owned();
                        match &properties.title {
                            None => properties.title = Some(candidate),
                            Some(existing) if existing == &candidate => {}
                            Some(_) => properties.title = None,
                        }
                    }
                }
                if custom_depth == Some(stack.len()) && name == "property" {
                    custom_name = None;
                    custom_depth = None;
                }
                stack.pop();
                text.clear();
            }
        }
        Ok(())
    })
}

fn attribute_local<'a>(element: &'a XmlElement, local_name: &[u8]) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|(name, _)| {
            name.as_slice() == local_name
                || name
                    .rsplit(|byte| *byte == b':')
                    .next()
                    .is_some_and(|name| name == local_name)
        })
        .map(|(_, value)| value.as_str())
}
