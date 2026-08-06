// SPDX-License-Identifier: MIT OR Apache-2.0

use std::io::Read as _;

use quick_xml::XmlVersion;
use quick_xml::encoding::{Decoder, DecodingReader};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

use super::error::DocxError;
use super::limits::{
    MAX_ATTRIBUTE_BYTES_PER_ELEMENT, MAX_ATTRIBUTE_BYTES_TOTAL, MAX_ATTRIBUTES_PER_ELEMENT,
    MAX_NAMESPACE_DECLARATIONS_PER_ELEMENT, MAX_NAMESPACE_DECLARATIONS_TOTAL, MAX_QNAME_BYTES,
    MAX_SINGLE_TEXT_EVENT_BYTES, MAX_XML_ATTRIBUTES_TOTAL, MAX_XML_DEPTH, MAX_XML_EVENTS,
    MAX_XML_TEXT_BYTES,
};

#[derive(Debug, Default)]
pub(super) struct XmlBudget {
    events: usize,
    attributes: usize,
    attribute_bytes: usize,
    namespace_declarations: usize,
    text_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct XmlElement {
    pub(super) namespace: Option<Vec<u8>>,
    pub(super) local_name: Vec<u8>,
    pub(super) attributes: Vec<(Vec<u8>, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum XmlEvent {
    Start(XmlElement),
    Empty(XmlElement),
    End {
        namespace: Option<Vec<u8>>,
        local_name: Vec<u8>,
    },
    Text(String),
}

pub(super) fn parse_xml(
    bytes: &[u8],
    budget: &mut XmlBudget,
    mut on_element: impl FnMut(&XmlElement) -> Result<(), DocxError>,
) -> Result<(), DocxError> {
    parse_xml_events(bytes, budget, |event| {
        if let XmlEvent::Start(element) | XmlEvent::Empty(element) = event {
            on_element(element)?;
        }
        Ok(())
    })
}

pub(super) fn parse_xml_events(
    bytes: &[u8],
    budget: &mut XmlBudget,
    mut on_event: impl FnMut(&XmlEvent) -> Result<(), DocxError>,
) -> Result<(), DocxError> {
    preflight_encoding(bytes)?;
    let mut decoded = Vec::with_capacity(bytes.len());
    DecodingReader::new(bytes)
        .read_to_end(&mut decoded)
        .map_err(|_| DocxError::UnsupportedXmlEncoding)?;
    let decoded = std::str::from_utf8(&decoded).map_err(|_| DocxError::UnsupportedXmlEncoding)?;
    let normalized = normalize_decoded_declaration(decoded)?;
    let mut reader = NsReader::from_reader(normalized.as_bytes());
    reader.config_mut().enable_all_checks(true);
    reader.config_mut().allow_dangling_amp = false;
    reader.config_mut().allow_unmatched_ends = false;

    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut roots = 0_usize;
    loop {
        buffer.clear();
        let decoder = reader.decoder();
        let (resolution, event) = reader
            .read_resolved_event_into(&mut buffer)
            .map_err(|_| DocxError::RequiredPartInvalid)?;
        budget.events = budget
            .events
            .checked_add(1)
            .filter(|events| *events <= MAX_XML_EVENTS)
            .ok_or(DocxError::XmlLimitExceeded)?;

        match event {
            Event::Start(start) => {
                if depth == 0 {
                    roots = roots
                        .checked_add(1)
                        .filter(|roots| *roots == 1)
                        .ok_or(DocxError::RequiredPartInvalid)?;
                }
                depth = depth
                    .checked_add(1)
                    .filter(|depth| *depth <= MAX_XML_DEPTH)
                    .ok_or(DocxError::XmlLimitExceeded)?;
                let element = own_element(decoder, resolution, &start, budget)?;
                on_event(&XmlEvent::Start(element))?;
            }
            Event::Empty(start) => {
                if depth == 0 {
                    roots = roots
                        .checked_add(1)
                        .filter(|roots| *roots == 1)
                        .ok_or(DocxError::RequiredPartInvalid)?;
                }
                let element = own_element(decoder, resolution, &start, budget)?;
                on_event(&XmlEvent::Empty(element))?;
            }
            Event::End(end) => {
                let namespace = own_namespace(resolution)?;
                let name = end.name();
                let local_name = end.local_name();
                if name.as_ref().len() > MAX_QNAME_BYTES
                    || local_name.as_ref().len() > MAX_QNAME_BYTES
                {
                    return Err(DocxError::XmlLimitExceeded);
                }
                depth = depth.checked_sub(1).ok_or(DocxError::RequiredPartInvalid)?;
                on_event(&XmlEvent::End {
                    namespace,
                    local_name: local_name.as_ref().to_vec(),
                })?;
            }
            Event::Text(text) => {
                let decoded = text
                    .decode()
                    .map_err(|_| DocxError::UnsupportedXmlEncoding)?;
                let unescaped = quick_xml::escape::unescape(&decoded)
                    .map_err(|_| DocxError::RequiredPartInvalid)?;
                charge_text(unescaped.len(), budget)?;
                on_event(&XmlEvent::Text(unescaped.into_owned()))?;
            }
            Event::CData(text) => {
                let decoded = text
                    .decode()
                    .map_err(|_| DocxError::UnsupportedXmlEncoding)?;
                charge_text(decoded.len(), budget)?;
                on_event(&XmlEvent::Text(decoded.into_owned()))?;
            }
            Event::DocType(_) => return Err(DocxError::ForbiddenXmlConstruct),
            Event::Decl(decl) => {
                if decl
                    .version()
                    .map_err(|_| DocxError::RequiredPartInvalid)?
                    .as_ref()
                    != b"1.0"
                {
                    return Err(DocxError::RequiredPartInvalid);
                }
            }
            Event::GeneralRef(reference) => {
                let text = if let Some(character) = reference
                    .resolve_char_ref()
                    .map_err(|_| DocxError::RequiredPartInvalid)?
                {
                    character.to_string()
                } else {
                    let name = reference
                        .decode()
                        .map_err(|_| DocxError::UnsupportedXmlEncoding)?;
                    match name.as_ref() {
                        "amp" => "&".to_owned(),
                        "apos" => "'".to_owned(),
                        "gt" => ">".to_owned(),
                        "lt" => "<".to_owned(),
                        "quot" => "\"".to_owned(),
                        _ => return Err(DocxError::ForbiddenXmlConstruct),
                    }
                };
                charge_text(text.len(), budget)?;
                on_event(&XmlEvent::Text(text))?;
            }
            Event::Eof => break,
            Event::Comment(_) | Event::PI(_) => {}
        }
    }

    if depth != 0 || roots != 1 {
        return Err(DocxError::RequiredPartInvalid);
    }
    Ok(())
}

fn own_namespace(resolution: ResolveResult<'_>) -> Result<Option<Vec<u8>>, DocxError> {
    match resolution {
        ResolveResult::Bound(namespace) => Ok(Some(namespace.as_ref().to_vec())),
        ResolveResult::Unbound => Ok(None),
        ResolveResult::Unknown(_) => Err(DocxError::RequiredPartInvalid),
    }
}

fn own_element(
    decoder: Decoder,
    resolution: ResolveResult<'_>,
    start: &BytesStart<'_>,
    budget: &mut XmlBudget,
) -> Result<XmlElement, DocxError> {
    let namespace = match resolution {
        ResolveResult::Bound(namespace) => Some(namespace.as_ref().to_vec()),
        ResolveResult::Unbound => None,
        ResolveResult::Unknown(_) => return Err(DocxError::RequiredPartInvalid),
    };
    let name = start.name();
    let local_name = start.local_name();
    if name.as_ref().len() > MAX_QNAME_BYTES || local_name.as_ref().len() > MAX_QNAME_BYTES {
        return Err(DocxError::XmlLimitExceeded);
    }

    let mut attributes = Vec::new();
    let mut attribute_count = 0_usize;
    let mut attribute_bytes = 0_usize;
    let mut namespace_count = 0_usize;
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|_| DocxError::RequiredPartInvalid)?;
        let key = attribute.key.as_ref();
        if key.len() > MAX_QNAME_BYTES {
            return Err(DocxError::XmlLimitExceeded);
        }
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Explicit1_0, decoder)
            .map_err(|_| DocxError::RequiredPartInvalid)?
            .into_owned();
        attribute_count = attribute_count
            .checked_add(1)
            .filter(|count| *count <= MAX_ATTRIBUTES_PER_ELEMENT)
            .ok_or(DocxError::XmlLimitExceeded)?;
        attribute_bytes = attribute_bytes
            .checked_add(key.len())
            .and_then(|total| total.checked_add(value.len()))
            .filter(|total| *total <= MAX_ATTRIBUTE_BYTES_PER_ELEMENT)
            .ok_or(DocxError::XmlLimitExceeded)?;
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            namespace_count = namespace_count
                .checked_add(1)
                .filter(|count| *count <= MAX_NAMESPACE_DECLARATIONS_PER_ELEMENT)
                .ok_or(DocxError::XmlLimitExceeded)?;
        }
        attributes.push((key.to_vec(), value));
    }

    budget.attributes = budget
        .attributes
        .checked_add(attribute_count)
        .filter(|count| *count <= MAX_XML_ATTRIBUTES_TOTAL)
        .ok_or(DocxError::XmlLimitExceeded)?;
    budget.attribute_bytes = budget
        .attribute_bytes
        .checked_add(attribute_bytes)
        .filter(|count| *count <= MAX_ATTRIBUTE_BYTES_TOTAL)
        .ok_or(DocxError::XmlLimitExceeded)?;
    budget.namespace_declarations = budget
        .namespace_declarations
        .checked_add(namespace_count)
        .filter(|count| *count <= MAX_NAMESPACE_DECLARATIONS_TOTAL)
        .ok_or(DocxError::XmlLimitExceeded)?;

    Ok(XmlElement {
        namespace,
        local_name: local_name.as_ref().to_vec(),
        attributes,
    })
}

fn charge_text(bytes: usize, budget: &mut XmlBudget) -> Result<(), DocxError> {
    if bytes > MAX_SINGLE_TEXT_EVENT_BYTES {
        return Err(DocxError::XmlLimitExceeded);
    }
    budget.text_bytes = budget
        .text_bytes
        .checked_add(bytes)
        .filter(|total| *total <= MAX_XML_TEXT_BYTES)
        .ok_or(DocxError::XmlLimitExceeded)?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum XmlEncoding {
    Utf8,
    Utf16Le { bom: bool },
    Utf16Be { bom: bool },
}

fn normalize_decoded_declaration(decoded: &str) -> Result<String, DocxError> {
    let source = decoded.strip_prefix('\u{feff}').unwrap_or(decoded);
    let trimmed = source.trim_start_matches(|character: char| character.is_ascii_whitespace());
    if !trimmed.starts_with("<?xml") {
        return Ok(source.to_owned());
    }
    let prefix_len = source.len() - trimmed.len();
    let end = trimmed.find("?>").ok_or(DocxError::RequiredPartInvalid)?;
    let mut normalized = String::with_capacity(source.len());
    normalized.push_str(&source[..prefix_len]);
    normalized.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    normalized.push_str(&trimmed[end + 2..]);
    Ok(normalized)
}

fn preflight_encoding(bytes: &[u8]) -> Result<(), DocxError> {
    let (encoding, body) = if let Some(body) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        (XmlEncoding::Utf8, body)
    } else if let Some(body) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        (XmlEncoding::Utf16Le { bom: true }, body)
    } else if let Some(body) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        (XmlEncoding::Utf16Be { bom: true }, body)
    } else if bytes.starts_with(&[0x3C, 0x00, 0x3F, 0x00]) {
        (XmlEncoding::Utf16Le { bom: false }, bytes)
    } else if bytes.starts_with(&[0x00, 0x3C, 0x00, 0x3F]) {
        (XmlEncoding::Utf16Be { bom: false }, bytes)
    } else {
        (XmlEncoding::Utf8, bytes)
    };

    let decoded = match encoding {
        XmlEncoding::Utf8 => std::str::from_utf8(body)
            .map_err(|_| DocxError::UnsupportedXmlEncoding)?
            .to_owned(),
        XmlEncoding::Utf16Le { .. } => decode_utf16(body, true)?,
        XmlEncoding::Utf16Be { .. } => decode_utf16(body, false)?,
    };
    validate_declaration(&decoded, encoding)
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> Result<String, DocxError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(DocxError::UnsupportedXmlEncoding);
    }
    let units = bytes.chunks_exact(2).map(|pair| {
        let pair = [pair[0], pair[1]];
        if little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }
    });
    std::char::decode_utf16(units)
        .map(|character| character.map_err(|_| DocxError::UnsupportedXmlEncoding))
        .collect()
}

fn validate_declaration(decoded: &str, encoding: XmlEncoding) -> Result<(), DocxError> {
    let source = decoded.strip_prefix('\u{feff}').unwrap_or(decoded);
    let trimmed = source.trim_start_matches(|character: char| character.is_ascii_whitespace());
    if !trimmed.starts_with("<?xml") {
        if matches!(
            encoding,
            XmlEncoding::Utf16Le { bom: false } | XmlEncoding::Utf16Be { bom: false }
        ) {
            return Err(DocxError::UnsupportedXmlEncoding);
        }
        return Ok(());
    }
    let end = trimmed
        .find("?>")
        .filter(|end| *end <= MAX_QNAME_BYTES)
        .ok_or(DocxError::RequiredPartInvalid)?;
    let declaration = &trimmed[..end];
    let version =
        declaration_attribute(declaration, "version").ok_or(DocxError::RequiredPartInvalid)?;
    if version != "1.0" {
        return Err(DocxError::RequiredPartInvalid);
    }
    let Some(declared) = declaration_attribute(declaration, "encoding") else {
        if matches!(
            encoding,
            XmlEncoding::Utf16Le { bom: false } | XmlEncoding::Utf16Be { bom: false }
        ) {
            return Err(DocxError::UnsupportedXmlEncoding);
        }
        return Ok(());
    };
    let declared = declared.to_ascii_uppercase();
    let agrees = match encoding {
        XmlEncoding::Utf8 => declared == "UTF-8",
        XmlEncoding::Utf16Le { bom: true } => declared == "UTF-16" || declared == "UTF-16LE",
        XmlEncoding::Utf16Be { bom: true } => declared == "UTF-16" || declared == "UTF-16BE",
        XmlEncoding::Utf16Le { bom: false } => declared == "UTF-16LE",
        XmlEncoding::Utf16Be { bom: false } => declared == "UTF-16BE",
    };
    if agrees {
        Ok(())
    } else {
        Err(DocxError::UnsupportedXmlEncoding)
    }
}

fn declaration_attribute<'a>(declaration: &'a str, name: &str) -> Option<&'a str> {
    let mut remaining = declaration.get("<?xml".len()..)?;
    loop {
        remaining = remaining.trim_start_matches(|character: char| character.is_ascii_whitespace());
        if remaining.is_empty() {
            return None;
        }
        let equal = remaining.find('=')?;
        let key = remaining[..equal].trim_end();
        remaining = remaining[equal + 1..].trim_start();
        let quote = remaining.as_bytes().first().copied()?;
        if quote != b'\'' && quote != b'"' {
            return None;
        }
        remaining = &remaining[1..];
        let value_end = remaining.find(char::from(quote))?;
        let value = &remaining[..value_end];
        remaining = &remaining[value_end + 1..];
        if key == name {
            return Some(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_with(source: &str, budget: &mut XmlBudget) -> Result<(), DocxError> {
        parse_xml(source.as_bytes(), budget, |_| Ok(()))
    }

    #[test]
    fn aggregate_xml_budgets_accept_limit_and_reject_limit_plus_one() {
        let mut events = XmlBudget {
            events: MAX_XML_EVENTS - 2,
            ..XmlBudget::default()
        };
        assert!(parse_with("<a/>", &mut events).is_ok());
        let mut events = XmlBudget {
            events: MAX_XML_EVENTS - 1,
            ..XmlBudget::default()
        };
        assert_eq!(
            parse_with("<a/>", &mut events).unwrap_err(),
            DocxError::XmlLimitExceeded
        );

        let mut attributes = XmlBudget {
            attributes: MAX_XML_ATTRIBUTES_TOTAL - 1,
            ..XmlBudget::default()
        };
        assert!(parse_with("<a x=\"\"/>", &mut attributes).is_ok());
        let mut attributes = XmlBudget {
            attributes: MAX_XML_ATTRIBUTES_TOTAL,
            ..XmlBudget::default()
        };
        assert_eq!(
            parse_with("<a x=\"\"/>", &mut attributes).unwrap_err(),
            DocxError::XmlLimitExceeded
        );

        let mut namespaces = XmlBudget {
            namespace_declarations: MAX_NAMESPACE_DECLARATIONS_TOTAL - 1,
            ..XmlBudget::default()
        };
        assert!(parse_with("<a xmlns:x=\"urn:x\"/>", &mut namespaces).is_ok());
        let mut namespaces = XmlBudget {
            namespace_declarations: MAX_NAMESPACE_DECLARATIONS_TOTAL,
            ..XmlBudget::default()
        };
        assert_eq!(
            parse_with("<a xmlns:x=\"urn:x\"/>", &mut namespaces).unwrap_err(),
            DocxError::XmlLimitExceeded
        );

        let mut text = XmlBudget {
            text_bytes: MAX_XML_TEXT_BYTES - 1,
            ..XmlBudget::default()
        };
        assert!(parse_with("<a>x</a>", &mut text).is_ok());
        let mut text = XmlBudget {
            text_bytes: MAX_XML_TEXT_BYTES,
            ..XmlBudget::default()
        };
        assert_eq!(
            parse_with("<a>x</a>", &mut text).unwrap_err(),
            DocxError::XmlLimitExceeded
        );
    }

    #[test]
    fn attribute_byte_budgets_accept_limit_and_reject_limit_plus_one() {
        let value = "x".repeat(MAX_ATTRIBUTE_BYTES_PER_ELEMENT - 1);
        assert!(parse_with(&format!("<a x=\"{value}\"/>"), &mut XmlBudget::default()).is_ok());
        let value = "x".repeat(MAX_ATTRIBUTE_BYTES_PER_ELEMENT);
        assert_eq!(
            parse_with(&format!("<a x=\"{value}\"/>"), &mut XmlBudget::default()).unwrap_err(),
            DocxError::XmlLimitExceeded
        );

        let attribute_bytes = 2_usize;
        let mut aggregate = XmlBudget {
            attribute_bytes: MAX_ATTRIBUTE_BYTES_TOTAL - attribute_bytes,
            ..XmlBudget::default()
        };
        assert!(parse_with("<a x=\"y\"/>", &mut aggregate).is_ok());
        let mut aggregate = XmlBudget {
            attribute_bytes: MAX_ATTRIBUTE_BYTES_TOTAL - attribute_bytes + 1,
            ..XmlBudget::default()
        };
        assert_eq!(
            parse_with("<a x=\"y\"/>", &mut aggregate).unwrap_err(),
            DocxError::XmlLimitExceeded
        );
    }
}
