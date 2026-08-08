// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deterministic PDF byte builders for the reader's tests.
//!
//! Every fixture the suite uses is assembled here from bytes written in this
//! file, so the corpus is reproducible, carries no private data, and — the
//! reason it matters for a budget test — can be authored to sit exactly on a
//! limit and exactly one unit past it.

use std::fmt::Write as _;

/// Body of one indirect object, without the `N 0 obj` / `endobj` wrapper.
pub(super) type ObjectBody = Vec<u8>;

/// Assemble a classic cross-reference-table PDF. `objects[i]` becomes object
/// `i + 1`.
pub(super) fn build_pdf(objects: &[ObjectBody], root: usize, extra_trailer: &str) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_at = out.len();
    let count = objects.len() + 1;
    out.extend_from_slice(format!("xref\n0 {count}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {count} /Root {root} 0 R {extra_trailer}>>\n").as_bytes(),
    );
    out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
    out
}

pub(super) fn stream_object(dictionary: &str, data: &[u8]) -> ObjectBody {
    let mut body = format!("<< {dictionary} /Length {} >>\nstream\n", data.len()).into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(b"\nendstream");
    body
}

/// A page-tree skeleton: catalog = 1, pages = 2, then a `(page, contents)` pair
/// per page, then the font, then any extra objects.
pub(super) struct PageSpec<'a> {
    pub(super) content: &'a [u8],
    pub(super) media_box: &'a str,
    pub(super) extra_page_entries: &'a str,
}

impl<'a> PageSpec<'a> {
    pub(super) fn new(content: &'a [u8]) -> Self {
        Self {
            content,
            media_box: "[0 0 612 792]",
            extra_page_entries: "",
        }
    }
}

pub(super) struct DocumentSpec<'a> {
    pub(super) pages: Vec<PageSpec<'a>>,
    pub(super) font_objects: Vec<ObjectBody>,
    /// `/Font << … >>` body used by every page's `/Resources`. Font object
    /// numbers are `first_font_id + n`, resolved by the caller through
    /// [`DocumentSpec::first_font_id`].
    pub(super) font_resources: String,
    pub(super) extra_trailer: String,
}

impl<'a> DocumentSpec<'a> {
    pub(super) fn first_font_id(page_count: usize) -> usize {
        3 + 2 * page_count
    }

    pub(super) fn build(self) -> Vec<u8> {
        let page_count = self.pages.len();
        let mut objects: Vec<ObjectBody> = Vec::new();
        objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());

        let mut kids = String::new();
        for index in 0..page_count {
            let _ = write!(kids, "{} 0 R ", 3 + 2 * index);
        }
        objects.push(
            format!(
                "<< /Type /Pages /Count {page_count} /Kids [{}] >>",
                kids.trim_end()
            )
            .into_bytes(),
        );

        for (index, page) in self.pages.iter().enumerate() {
            let contents_id = 4 + 2 * index;
            objects.push(
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox {} /Resources << /Font << {} >> >> /Contents {contents_id} 0 R {} >>",
                    page.media_box, self.font_resources, page.extra_page_entries
                )
                .into_bytes(),
            );
            objects.push(stream_object("", page.content));
        }
        objects.extend(self.font_objects);
        build_pdf(&objects, 1, &self.extra_trailer)
    }
}

/// The common case: `n` pages sharing one Helvetica `/F1`, no `/Widths`.
pub(super) fn helvetica_document(contents: &[&[u8]]) -> Vec<u8> {
    let font_id = DocumentSpec::first_font_id(contents.len());
    DocumentSpec {
        pages: contents
            .iter()
            .map(|content| PageSpec::new(content))
            .collect(),
        font_objects: vec![
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_vec(),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build()
}

/// A single page whose `/F1` carries real `/Widths`, so runs report
/// `geometry_exact == true`.
pub(super) fn measured_document(content: &[u8]) -> Vec<u8> {
    measured_pages(&[content])
}

/// `n` pages sharing one measured font. Every glyph is 500/1000 em, so an
/// advance is exactly `0.5 * font_size` per glyph and the arithmetic in a test
/// is checkable by hand rather than by rerunning the code under test.
pub(super) fn measured_pages(contents: &[&[u8]]) -> Vec<u8> {
    let widths = (32..=126).map(|_| "500").collect::<Vec<_>>().join(" ");
    let font_id = DocumentSpec::first_font_id(contents.len());
    DocumentSpec {
        pages: contents
            .iter()
            .map(|content| PageSpec::new(content))
            .collect(),
        font_objects: vec![
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /Fixed500 /Encoding /WinAnsiEncoding \
                 /FirstChar 32 /LastChar 126 /Widths [{widths}] >>"
            )
            .into_bytes(),
        ],
        font_resources: format!("/F1 {font_id} 0 R"),
        extra_trailer: String::new(),
    }
    .build()
}

/// A minimal but genuine sfnt TrueType font carrying only what
/// `ttf_parser::Face::parse` requires — `head`, `hhea`, `maxp` — plus a
/// `cmap` format-12 subtable expressing `mappings` as `(character, glyph id)`.
///
/// Built here rather than checked in as a binary so the corpus stays
/// reproducible, carries no third-party font licence, and can be authored to
/// map exactly the glyph ids a test's content stream shows.
pub(super) fn truetype_font(mappings: &[(char, u16)], glyph_count: u16) -> Vec<u8> {
    let groups: Vec<(u32, u32, u16)> = mappings
        .iter()
        .map(|(character, glyph)| {
            let code = u32::from(*character);
            (code, code, *glyph)
        })
        .collect();
    truetype_font_with_cmap_groups(&groups, glyph_count)
}

/// The same font, but with `cmap` format-12 groups written directly as
/// `(start_char_code, end_char_code, start_glyph_id)`.
///
/// Exposed separately because a group is a *range*, and the ranges a font may
/// declare are exactly what the enhanced tier's inversion walk has to survive:
/// a single group may span the whole `u32` space, which is neither a Unicode
/// range nor something the format forbids.
pub(super) fn truetype_font_with_cmap_groups(
    groups: &[(u32, u32, u16)],
    glyph_count: u16,
) -> Vec<u8> {
    fn table_record(tag: &[u8; 4], offset: u32, length: u32) -> Vec<u8> {
        let mut record = tag.to_vec();
        record.extend_from_slice(&0u32.to_be_bytes()); // checksum, unverified
        record.extend_from_slice(&offset.to_be_bytes());
        record.extend_from_slice(&length.to_be_bytes());
        record
    }

    // head: 54 bytes; only unitsPerEm (offset 18) and indexToLocFormat
    // (offset 50) are read.
    let mut head = vec![0u8; 54];
    head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    head[18..20].copy_from_slice(&1000u16.to_be_bytes());

    // hhea: 36 bytes; numberOfHMetrics at offset 34.
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[34..36].copy_from_slice(&glyph_count.to_be_bytes());

    let mut maxp = Vec::new();
    maxp.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    maxp.extend_from_slice(&glyph_count.to_be_bytes());

    // cmap: one (platform 3, encoding 10) record pointing at a format-12
    // subtable. Format 12 groups must be sorted ascending by start code: the
    // parser binary-searches them.
    let mut sorted = groups.to_vec();
    sorted.sort_by_key(|(start, _, _)| *start);
    let mut encoded = Vec::new();
    for (start, end, glyph) in &sorted {
        encoded.extend_from_slice(&start.to_be_bytes());
        encoded.extend_from_slice(&end.to_be_bytes());
        encoded.extend_from_slice(&u32::from(*glyph).to_be_bytes());
    }
    let mut subtable = Vec::new();
    subtable.extend_from_slice(&12u16.to_be_bytes());
    subtable.extend_from_slice(&0u16.to_be_bytes());
    subtable.extend_from_slice(&(16 + encoded.len() as u32).to_be_bytes());
    subtable.extend_from_slice(&0u32.to_be_bytes());
    subtable.extend_from_slice(&(sorted.len() as u32).to_be_bytes());
    subtable.extend_from_slice(&encoded);

    let mut cmap = Vec::new();
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&1u16.to_be_bytes());
    cmap.extend_from_slice(&3u16.to_be_bytes());
    cmap.extend_from_slice(&10u16.to_be_bytes());
    cmap.extend_from_slice(&12u32.to_be_bytes());
    cmap.extend_from_slice(&subtable);

    // Tables must be listed in ascending tag order.
    let tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"cmap", cmap),
        (b"head", head),
        (b"hhea", hhea),
        (b"maxp", maxp),
    ];
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // searchRange
    out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
    out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift

    let mut offset = 12 + 16 * tables.len();
    let mut records = Vec::new();
    let mut bodies = Vec::new();
    for (tag, body) in &tables {
        records.extend(table_record(tag, offset as u32, body.len() as u32));
        let padded = (body.len() + 3) & !3;
        let mut chunk = body.clone();
        chunk.resize(padded, 0);
        bodies.extend(chunk);
        offset += padded;
    }
    out.extend(records);
    out.extend(bodies);
    out
}
