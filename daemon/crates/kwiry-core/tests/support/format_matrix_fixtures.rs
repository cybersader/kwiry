// SPDX-License-Identifier: MIT OR Apache-2.0

use kwiry_core::SourceFormat;

pub struct RawFormatFixture {
    pub format: SourceFormat,
    pub path: &'static str,
    pub query: &'static str,
    pub bytes: Vec<u8>,
}

pub const STANDARD_PATH: &str = "Format Matrix/standard-source.md";

pub fn format_matrix_fixtures() -> Vec<RawFormatFixture> {
    vec![
        RawFormatFixture {
            format: SourceFormat::Markdown,
            path: "Format Matrix/amberfield-cedarway.md",
            query: "amber cedar marker",
            bytes: b"# Fixture\n\nCompact neutral Markdown body.\n".to_vec(),
        },
        RawFormatFixture {
            format: SourceFormat::Text,
            path: "Format Matrix/birchstone-deltafield.txt",
            query: "birch delta inlet",
            bytes: b"Compact neutral plain-text body.\n".to_vec(),
        },
        RawFormatFixture {
            format: SourceFormat::Base,
            path: "Format Matrix/cobaltview-elmbridge.base",
            query: "cobalt elm lantern",
            bytes: b"title: Compact Base\nviews:\n  - type: table\n    name: Inventory\n".to_vec(),
        },
        RawFormatFixture {
            format: SourceFormat::Canvas,
            path: "Format Matrix/forestline-gardenway.canvas",
            query: "forest garden grove",
            bytes: br#"{"nodes":[{"id":"1111111111111111","type":"text","x":0,"y":0,"width":320,"height":180,"text":"Compact neutral canvas body."}],"edges":[]}"#.to_vec(),
        },
        RawFormatFixture {
            format: SourceFormat::Docx,
            path: "Format Matrix/harborstone-islandway.docx",
            query: "harbor island orchard",
            bytes: docx_fixture(),
        },
        RawFormatFixture {
            format: SourceFormat::Pdf,
            path: "Format Matrix/juniperfield-kestrelway.pdf",
            query: "juniper kestrel prairie",
            bytes: pdf_fixture(),
        },
        RawFormatFixture {
            format: SourceFormat::Excalidraw,
            path: "Format Matrix/lakewood-maplefield.excalidraw",
            query: "lake maple quartz",
            bytes: br#"{"type":"excalidraw","version":2,"elements":[{"id":"text00000000001","type":"text","originalText":"Compact neutral drawing body.","text":"Compact neutral drawing body."}],"appState":{},"files":{}}"#.to_vec(),
        },
        RawFormatFixture {
            format: SourceFormat::Excel,
            path: "Format Matrix/meadowstone-northway.xlsx",
            query: "meadow north river",
            bytes: excel_fixture(),
        },
        RawFormatFixture {
            format: SourceFormat::Html,
            path: "Format Matrix/oakfield-pineway.html",
            query: "oak pine summit",
            bytes: b"<!doctype html><html><head><title>Compact HTML</title></head><body><p>Compact neutral HTML body.</p></body></html>".to_vec(),
        },
    ]
}

pub fn standard_markdown() -> Vec<u8> {
    b"# Standard source\n\n\
      amber cedar marker\n\
      birch delta inlet\n\
      cobalt elm lantern\n\
      forest garden grove\n\
      harbor island orchard\n\
      juniper kestrel prairie\n\
      lake maple quartz\n\
      meadow north river\n\
      oak pine summit\n"
        .to_vec()
}

fn docx_fixture() -> Vec<u8> {
    const CONTENT_TYPES: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
    const RELATIONSHIPS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
    const OFFICE_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const WORDPROCESSING: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    const MAIN_TYPE: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";

    let content_types = format!(
        r#"<Types xmlns="{CONTENT_TYPES}"><Override PartName="/word/document.xml" ContentType="{MAIN_TYPE}"/></Types>"#,
    );
    let relationships = format!(
        r#"<Relationships xmlns="{RELATIONSHIPS}"><Relationship Id="rId1" Type="{OFFICE_REL}/officeDocument" Target="word/document.xml"/></Relationships>"#,
    );
    let document = format!(
        r#"<w:document xmlns:w="{WORDPROCESSING}"><w:body><w:p><w:r><w:t>Compact neutral document body.</w:t></w:r></w:p></w:body></w:document>"#,
    );
    stored_zip(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", relationships.as_bytes()),
        ("word/document.xml", document.as_bytes()),
    ])
}

fn excel_fixture() -> Vec<u8> {
    const CONTENT_TYPES: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
    const RELATIONSHIPS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
    const OFFICE_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const SPREADSHEET: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    const WORKBOOK_TYPE: &str =
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";
    const WORKSHEET_TYPE: &str =
        "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml";

    let content_types = format!(
        r#"<Types xmlns="{CONTENT_TYPES}"><Override PartName="/xl/workbook.xml" ContentType="{WORKBOOK_TYPE}"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="{WORKSHEET_TYPE}"/></Types>"#,
    );
    let root_relationships = format!(
        r#"<Relationships xmlns="{RELATIONSHIPS}"><Relationship Id="rId1" Type="{OFFICE_REL}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
    );
    let workbook = format!(
        r#"<workbook xmlns="{SPREADSHEET}" xmlns:r="{OFFICE_REL}"><sheets><sheet name="Ledger" sheetId="1" r:id="rSheet"/></sheets></workbook>"#,
    );
    let workbook_relationships = format!(
        r#"<Relationships xmlns="{RELATIONSHIPS}"><Relationship Id="rSheet" Type="{OFFICE_REL}/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
    );
    let worksheet = format!(
        r#"<worksheet xmlns="{SPREADSHEET}"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Compact neutral spreadsheet body.</t></is></c></row></sheetData></worksheet>"#,
    );
    stored_zip(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root_relationships.as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        (
            "xl/_rels/workbook.xml.rels",
            workbook_relationships.as_bytes(),
        ),
        ("xl/worksheets/sheet1.xml", worksheet.as_bytes()),
    ])
}

fn pdf_fixture() -> Vec<u8> {
    let content = b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Compact neutral PDF body.) Tj ET";
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        pdf_stream(content),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
    ];
    build_pdf(&objects)
}

fn pdf_stream(data: &[u8]) -> Vec<u8> {
    let mut body = format!("<< /Length {} >>\nstream\n", data.len()).into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(b"\nendstream");
    body
}

fn build_pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        bytes.extend_from_slice(object);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    struct Pending<'a> {
        name: &'a str,
        crc: u32,
        size: u32,
        local_offset: u32,
    }

    let mut bytes = Vec::new();
    let mut pending = Vec::with_capacity(entries.len());
    for (name, content) in entries {
        let crc = rawzip::crc32(content);
        let size = u32::try_from(content.len()).expect("test ZIP entry length");
        let local_offset = u32::try_from(bytes.len()).expect("test ZIP local offset");
        push_u32(&mut bytes, 0x0403_4b50);
        push_u16(&mut bytes, 20);
        push_u16(&mut bytes, 1 << 11);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u32(&mut bytes, crc);
        push_u32(&mut bytes, size);
        push_u32(&mut bytes, size);
        push_u16(
            &mut bytes,
            u16::try_from(name.len()).expect("test ZIP entry name length"),
        );
        push_u16(&mut bytes, 0);
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(content);
        pending.push(Pending {
            name,
            crc,
            size,
            local_offset,
        });
    }

    let directory_offset = u32::try_from(bytes.len()).expect("test ZIP directory offset");
    for entry in &pending {
        push_u32(&mut bytes, 0x0201_4b50);
        push_u16(&mut bytes, 20);
        push_u16(&mut bytes, 20);
        push_u16(&mut bytes, 1 << 11);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u32(&mut bytes, entry.crc);
        push_u32(&mut bytes, entry.size);
        push_u32(&mut bytes, entry.size);
        push_u16(
            &mut bytes,
            u16::try_from(entry.name.len()).expect("test ZIP entry name length"),
        );
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u16(&mut bytes, 0);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, entry.local_offset);
        bytes.extend_from_slice(entry.name.as_bytes());
    }
    let directory_size =
        u32::try_from(bytes.len()).expect("test ZIP directory end") - directory_offset;
    push_u32(&mut bytes, 0x0605_4b50);
    push_u16(&mut bytes, 0);
    push_u16(&mut bytes, 0);
    push_u16(
        &mut bytes,
        u16::try_from(pending.len()).expect("test ZIP entry count"),
    );
    push_u16(
        &mut bytes,
        u16::try_from(pending.len()).expect("test ZIP entry count"),
    );
    push_u32(&mut bytes, directory_size);
    push_u32(&mut bytes, directory_offset);
    push_u16(&mut bytes, 0);
    bytes
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}
