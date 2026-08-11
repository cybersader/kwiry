// SPDX-License-Identifier: MIT OR Apache-2.0

use std::io::Write;

#[derive(Clone, Copy)]
pub(crate) enum Method {
    Store,
    Deflate,
    Other(u16),
}

impl Method {
    fn id(self) -> u16 {
        match self {
            Self::Store => 0,
            Self::Deflate => 8,
            Self::Other(id) => id,
        }
    }
}

pub(crate) struct TestEntry<'a> {
    pub(crate) name: &'a str,
    pub(crate) bytes: &'a [u8],
    pub(crate) method: Method,
    pub(crate) flags: u16,
    pub(crate) descriptor: bool,
}

impl<'a> TestEntry<'a> {
    pub(crate) fn stored(name: &'a str, bytes: &'a [u8]) -> Self {
        Self {
            name,
            bytes,
            method: Method::Store,
            flags: 1 << 11,
            descriptor: false,
        }
    }

    pub(crate) fn deflated(name: &'a str, bytes: &'a [u8]) -> Self {
        Self {
            name,
            bytes,
            method: Method::Deflate,
            flags: 1 << 11,
            descriptor: false,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EntryLocation {
    pub(crate) local_offset: usize,
    pub(crate) data_offset: usize,
    pub(crate) compressed_len: usize,
    pub(crate) descriptor_offset: Option<usize>,
    pub(crate) central_offset: usize,
}

pub(crate) struct BuiltZip {
    pub(crate) bytes: Vec<u8>,
    pub(crate) entries: Vec<EntryLocation>,
}

pub(crate) fn build_zip(entries: &[TestEntry<'_>]) -> BuiltZip {
    struct Pending {
        name: Vec<u8>,
        flags: u16,
        method: u16,
        crc: u32,
        compressed_size: u32,
        uncompressed_size: u32,
        local_offset: u32,
        location: EntryLocation,
    }

    let mut bytes = Vec::new();
    let mut pending = Vec::new();
    for entry in entries {
        let compressed = match entry.method {
            Method::Store | Method::Other(_) => entry.bytes.to_vec(),
            Method::Deflate => {
                let mut encoder =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(entry.bytes).expect("test compression");
                encoder.finish().expect("test compression finish")
            }
        };
        let crc = rawzip::crc32(entry.bytes);
        let local_offset = bytes.len();
        let flags = entry.flags | if entry.descriptor { 1 << 3 } else { 0 };
        push_u32(&mut bytes, 0x0403_4b50);
        push_u16(&mut bytes, 20);
        push_u16(&mut bytes, flags);
        push_u16(&mut bytes, entry.method.id());
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u32(&mut bytes, if entry.descriptor { 0 } else { crc });
        push_u32(
            &mut bytes,
            if entry.descriptor {
                0
            } else {
                compressed.len() as u32
            },
        );
        push_u32(
            &mut bytes,
            if entry.descriptor {
                0
            } else {
                entry.bytes.len() as u32
            },
        );
        push_u16(&mut bytes, entry.name.len() as u16);
        push_u16(&mut bytes, 0);
        bytes.extend_from_slice(entry.name.as_bytes());
        let data_offset = bytes.len();
        bytes.extend_from_slice(&compressed);
        let descriptor_offset = entry.descriptor.then(|| {
            let offset = bytes.len();
            push_u32(&mut bytes, 0x0807_4b50);
            push_u32(&mut bytes, crc);
            push_u32(&mut bytes, compressed.len() as u32);
            push_u32(&mut bytes, entry.bytes.len() as u32);
            offset
        });
        pending.push(Pending {
            name: entry.name.as_bytes().to_vec(),
            flags,
            method: entry.method.id(),
            crc,
            compressed_size: compressed.len() as u32,
            uncompressed_size: entry.bytes.len() as u32,
            local_offset: local_offset as u32,
            location: EntryLocation {
                local_offset,
                data_offset,
                compressed_len: compressed.len(),
                descriptor_offset,
                central_offset: 0,
            },
        });
    }

    let directory_offset = bytes.len();
    for entry in &mut pending {
        entry.location.central_offset = bytes.len();
        push_u32(&mut bytes, 0x0201_4b50);
        push_u16(&mut bytes, 20);
        push_u16(&mut bytes, 20);
        push_u16(&mut bytes, entry.flags);
        push_u16(&mut bytes, entry.method);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u32(&mut bytes, entry.crc);
        push_u32(&mut bytes, entry.compressed_size);
        push_u32(&mut bytes, entry.uncompressed_size);
        push_u16(&mut bytes, entry.name.len() as u16);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, entry.local_offset);
        bytes.extend_from_slice(&entry.name);
    }
    let directory_size = bytes.len() - directory_offset;
    push_u32(&mut bytes, 0x0605_4b50);
    push_u16(&mut bytes, 0);
    push_u16(&mut bytes, 0);
    push_u16(&mut bytes, pending.len() as u16);
    push_u16(&mut bytes, pending.len() as u16);
    push_u32(&mut bytes, directory_size as u32);
    push_u32(&mut bytes, directory_offset as u32);
    push_u16(&mut bytes, 0);

    BuiltZip {
        bytes,
        entries: pending.into_iter().map(|entry| entry.location).collect(),
    }
}

pub(crate) fn set_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(crate) const CONTENT_TYPES_TRANSITIONAL: &str =
    "http://schemas.openxmlformats.org/package/2006/content-types";
pub(crate) const RELATIONSHIPS_TRANSITIONAL: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships";
pub(crate) const OFFICE_REL_TRANSITIONAL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
pub(crate) const WORDPROCESSING_TRANSITIONAL: &str =
    "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
pub(crate) const CONTENT_TYPES_STRICT: &str = "http://purl.oclc.org/ooxml/package/content-types";
pub(crate) const RELATIONSHIPS_STRICT: &str = "http://purl.oclc.org/ooxml/package/relationships";
pub(crate) const OFFICE_REL_STRICT: &str =
    "http://purl.oclc.org/ooxml/officeDocument/relationships";
pub(crate) const WORDPROCESSING_STRICT: &str = "http://purl.oclc.org/ooxml/wordprocessingml/main";

pub(crate) fn content_types(namespace: &str, main_part: &str, main_type: &str) -> String {
    format!(
        r#"<Types xmlns="{namespace}"><Override PartName="{main_part}" ContentType="{main_type}"/></Types>"#,
    )
}

pub(crate) fn root_relationships(namespace: &str, office_prefix: &str, target: &str) -> String {
    format!(
        r#"<Relationships xmlns="{namespace}"><Relationship Id="rId1" Type="{office_prefix}/officeDocument" Target="{target}"/></Relationships>"#,
    )
}

pub(crate) fn document(namespace: &str) -> String {
    format!(r#"<w:document xmlns:w="{namespace}"><w:body><w:p/></w:body></w:document>"#)
}

pub(crate) const MAIN_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
