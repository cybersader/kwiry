// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::extract::ExtractionCoverage;
use crate::formats::docx::ContentRole;
use crate::formats::ooxml::{Method, TestEntry, XmlBudget, build_zip};

use super::limits::{
    MAX_CELL_RECORDS, MAX_COLUMN_RANGES_PER_SHEET, MAX_COMMENT_BYTES, MAX_COMMENTS,
    MAX_DEFINED_NAME_BYTES, MAX_DEFINED_NAMES, MAX_OUTPUT_BYTES, MAX_OUTPUT_HEADING_BYTES,
    MAX_OUTPUT_SECTIONS, MAX_SHARED_STRING_BYTES, MAX_SHARED_STRING_ENTRIES, MAX_SHEET_NAME_BYTES,
    MAX_WORKBOOK_SHEETS,
};
use super::opc::{
    COMMENTS_CONTENT_TYPE, SHARED_STRINGS_CONTENT_TYPE, WORKSHEET_CONTENT_TYPE,
    XLSM_WORKBOOK_CONTENT_TYPE, XLSX_WORKBOOK_CONTENT_TYPE,
};
use super::spreadsheet::{
    SPREADSHEET_NS_STRICT, SPREADSHEET_NS_TRANSITIONAL, parse_comments, parse_shared_strings,
    parse_workbook, parse_worksheet,
};
use super::{ExcelError, ExcelSection, OutputBudget, extract_excel_candidate_outcome};

const CONTENT_NS: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const OFFICE_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const STRICT_CONTENT_NS: &str = "http://purl.oclc.org/ooxml/package/content-types";
const STRICT_REL_NS: &str = "http://purl.oclc.org/ooxml/package/relationships";
const STRICT_OFFICE_REL: &str = "http://purl.oclc.org/ooxml/officeDocument/relationships";

#[derive(Clone)]
struct OwnedEntry {
    name: String,
    bytes: Vec<u8>,
    method: Method,
}

impl OwnedEntry {
    fn xml(name: &str, source: impl Into<Vec<u8>>, method: Method) -> Self {
        Self {
            name: name.to_owned(),
            bytes: source.into(),
            method,
        }
    }

    fn opaque(name: &str, bytes: &[u8], method: Method) -> Self {
        Self::xml(name, bytes.to_vec(), method)
    }
}

fn zip(entries: Vec<OwnedEntry>) -> Vec<u8> {
    let borrowed = entries
        .iter()
        .map(|entry| TestEntry {
            name: &entry.name,
            bytes: &entry.bytes,
            method: entry.method,
            flags: 1 << 11,
            descriptor: false,
        })
        .collect::<Vec<_>>();
    build_zip(&borrowed).bytes
}

fn package(
    method: Method,
    strict: bool,
    macro_enabled: bool,
    workbook: Vec<u8>,
    workbook_relationships: Vec<u8>,
    mut parts: Vec<OwnedEntry>,
) -> Vec<u8> {
    let (content_ns, relationship_ns, office_rel) = if strict {
        (STRICT_CONTENT_NS, STRICT_REL_NS, STRICT_OFFICE_REL)
    } else {
        (CONTENT_NS, REL_NS, OFFICE_REL)
    };
    let workbook_type = if macro_enabled {
        XLSM_WORKBOOK_CONTENT_TYPE
    } else {
        XLSX_WORKBOOK_CONTENT_TYPE
    };
    let mut overrides = vec![format!(
        r#"<Override PartName="/xl/workbook.xml" ContentType="{workbook_type}"/>"#
    )];
    for part in &parts {
        let content_type =
            if part.name.starts_with("xl/worksheets/") && !part.name.contains("/_rels/") {
                Some(WORKSHEET_CONTENT_TYPE)
            } else if part.name == "xl/sharedStrings.xml" {
                Some(SHARED_STRINGS_CONTENT_TYPE)
            } else if part.name.starts_with("xl/comments") {
                Some(COMMENTS_CONTENT_TYPE)
            } else {
                None
            };
        if let Some(content_type) = content_type {
            overrides.push(format!(
                r#"<Override PartName="/{}" ContentType="{content_type}"/>"#,
                part.name
            ));
        }
    }
    if macro_enabled {
        overrides.push(
            r#"<Override PartName="/xl/vbaProject.bin" ContentType="application/vnd.ms-office.vbaProject"/>"#
                .to_owned(),
        );
    }
    let content_types = format!(
        r#"<Types xmlns="{content_ns}">{}</Types>"#,
        overrides.concat()
    );
    let root = format!(
        r#"<Relationships xmlns="{relationship_ns}"><Relationship Id="rId1" Type="{office_rel}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#
    );
    let mut entries = vec![
        OwnedEntry::xml("[Content_Types].xml", content_types, method),
        OwnedEntry::xml("_rels/.rels", root, method),
        OwnedEntry::xml("xl/workbook.xml", workbook, method),
        OwnedEntry::xml("xl/_rels/workbook.xml.rels", workbook_relationships, method),
    ];
    entries.append(&mut parts);
    zip(entries)
}

fn workbook(ns: &str, sheets: &str, names: &str) -> Vec<u8> {
    format!(
        r#"<workbook xmlns="{ns}" xmlns:r="{OFFICE_REL}"><sheets>{sheets}</sheets><definedNames>{names}</definedNames></workbook>"#
    )
    .into_bytes()
}

fn relationships(items: &str) -> Vec<u8> {
    format!(r#"<Relationships xmlns="{REL_NS}">{items}</Relationships>"#).into_bytes()
}

fn worksheet(ns: &str, body: &str) -> Vec<u8> {
    format!(r#"<worksheet xmlns="{ns}">{body}</worksheet>"#).into_bytes()
}

fn utf16(source: &str, little_endian: bool) -> Vec<u8> {
    let mut bytes = if little_endian {
        vec![0xff, 0xfe]
    } else {
        vec![0xfe, 0xff]
    };
    for unit in source.encode_utf16() {
        let encoded = if little_endian {
            unit.to_le_bytes()
        } else {
            unit.to_be_bytes()
        };
        bytes.extend_from_slice(&encoded);
    }
    bytes
}

pub(crate) fn ranking_fixture() -> Vec<u8> {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let latent_cells = r#"<c><f>rankbeacon</f></c>"#.repeat(31);
    let primary = format!(r#"rankbeacon {}"#, "primaryfiller ".repeat(250));
    let sheet = format!(
        r#"<sheetData><row><c r="A1" t="str"><f>rankbeacon</f><v>cached</v></c><c r="B1" t="inlineStr"><is><t>{primary}</t></is></c>{latent_cells}</row></sheetData>"#,
    );
    package(
        Method::Store,
        false,
        false,
        workbook(ns, r#"<sheet name="Roles" sheetId="1" r:id="rS"/>"#, ""),
        relationships(&format!(
            r#"<Relationship Id="rS" Type="{OFFICE_REL}/worksheet" Target="worksheets/s.xml"/>"#,
        )),
        vec![OwnedEntry::xml(
            "xl/worksheets/s.xml",
            worksheet(ns, &sheet),
            Method::Store,
        )],
    )
}

fn core_fixture(method: Method, strict: bool, macro_enabled: bool) -> Vec<u8> {
    let ns = if strict {
        std::str::from_utf8(SPREADSHEET_NS_STRICT).expect("strict namespace")
    } else {
        std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace")
    };
    let office_rel = if strict {
        STRICT_OFFICE_REL
    } else {
        OFFICE_REL
    };
    let sheets = r#"<sheet name="Second" sheetId="9" r:id="rSecond"/><sheet name="Hidden" sheetId="2" state="hidden" r:id="rHidden"/><sheet name="First" sheetId="1" r:id="rFirst"/>"#;
    let names = r#"<definedName name="GlobalName">First!$A$1</definedName><definedName name="LocalHidden" localSheetId="1" hidden="1">Hidden!$A$1</definedName>"#;
    let workbook = String::from_utf8(workbook(ns, sheets, names))
        .expect("workbook UTF-8")
        .replace(OFFICE_REL, office_rel)
        .into_bytes();
    let workbook_relationships = relationships(&format!(
        r#"<Relationship Id="rFirst" Type="{office_rel}/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rSecond" Type="{office_rel}/worksheet" Target="worksheets/sheet2.xml"/><Relationship Id="rHidden" Type="{office_rel}/worksheet" Target="worksheets/sheet3.xml"/><Relationship Id="rShared" Type="{office_rel}/sharedStrings" Target="sharedStrings.xml"/>{}"#,
        if macro_enabled {
            format!(
                r#"<Relationship Id="rVba" Type="{office_rel}/vbaProject" Target="vbaProject.bin"/>"#
            )
        } else {
            String::new()
        }
    ));
    let shared = format!(
        r#"<sst xmlns="{ns}" count="2" uniqueCount="2"><si><t>Shared</t></si><si><r><t>Rich </t></r><rPh><t>phonetic</t></rPh><r><t>text</t></r></si></sst>"#
    );
    let second = worksheet(
        ns,
        r#"<sheetData><row r="2"><c r="B2" t="inlineStr"><is><r><t>Second sheet</t></r></is></c></row></sheetData>"#,
    );
    let hidden = worksheet(
        ns,
        r#"<sheetData><row><c t="s"><v>1</v></c></row></sheetData>"#,
    );
    let first = worksheet(
        ns,
        r#"<cols><col min="4" max="5" hidden="1"/><col min="5" max="6" hidden="1"/></cols><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><f>SUM(A1,1)</f><v>42.00</v></c><c r="C1" t="str"><f>TEXT(A1,"0")</f><v>cached string</v></c><c r="D1"><v>45123.00</v></c></row><row r="3" hidden="1"><c r="A3" t="b"><v>1</v></c><c r="B3" t="e"><v>#N/A</v></c><c r="C3" t="d"><v>2026-08-11</v></c><c r="D3" t="inlineStr"><is><t>hidden inline</t></is></c></row></sheetData>"#,
    );
    let sheet_rels = relationships(&format!(
        r#"<Relationship Id="rComment" Type="{office_rel}/comments" Target="../comments1.xml"/><Relationship Id="rVml" Type="{office_rel}/vmlDrawing" Target="../drawings/vml1.vml"/>"#
    ));
    let comments = format!(
        r#"<comments xmlns="{ns}"><authors><author>Ignored</author></authors><commentList><comment ref="Z9" authorId="0"><text><t>empty cell comment</t></text></comment><comment ref="B1" authorId="0"><text><r><t>Cell </t></r><r><t>comment</t></r></text></comment></commentList></comments>"#
    );
    let mut parts = vec![
        OwnedEntry::xml("xl/worksheets/sheet1.xml", first, method),
        OwnedEntry::xml("xl/worksheets/sheet3.xml", hidden, method),
        OwnedEntry::xml("xl/worksheets/sheet2.xml", second, method),
        OwnedEntry::xml("xl/sharedStrings.xml", shared, method),
        OwnedEntry::xml("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels, method),
        OwnedEntry::xml("xl/comments1.xml", comments, method),
        OwnedEntry::opaque("xl/drawings/vml1.vml", b"unopened-vml", Method::Other(12)),
    ];
    if macro_enabled {
        parts.push(OwnedEntry::opaque(
            "xl/vbaProject.bin",
            b"sentinel-macro-payload-must-never-open",
            Method::Other(12),
        ));
    }
    package(
        method,
        strict,
        macro_enabled,
        workbook,
        workbook_relationships,
        parts,
    )
}

#[test]
fn store_and_deflate_packages_extract_in_sheet_then_row_major_order() {
    for method in [Method::Store, Method::Deflate] {
        let candidate = extract_excel_candidate_outcome(&core_fixture(method, false, false));
        assert_eq!(
            candidate.coverage,
            ExtractionCoverage::IndexedComplete,
            "{:?}",
            candidate.notices
        );
        let content = candidate
            .sections
            .iter()
            .map(|section| section.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            content,
            [
                "GlobalName",
                "First!$A$1",
                "Second",
                "Second sheet",
                "Hidden",
                "LocalHidden",
                "Hidden!$A$1",
                "Rich text",
                "First",
                "Shared",
                "42.00",
                "SUM(A1,1)",
                "Cell comment",
                "cached string",
                "TEXT(A1,\"0\")",
                "45123.00",
                "1",
                "#N/A",
                "2026-08-11",
                "hidden inline",
                "empty cell comment",
            ]
        );
        assert_eq!(candidate.sections[3].heading_path, ["Second"]);
        assert_eq!(
            candidate.sections[3]
                .locator
                .as_ref()
                .map(|value| value.cell.as_str()),
            Some("B2")
        );
    }
}

#[test]
fn primary_and_latent_roles_match_cached_formula_hidden_and_comment_contract() {
    let candidate = extract_excel_candidate_outcome(&core_fixture(Method::Store, false, false));
    let role = |content: &str| {
        candidate
            .sections
            .iter()
            .find(|section| section.content == content)
            .map(|section| section.role)
            .expect("section")
    };
    assert_eq!(role("42.00"), ContentRole::Primary);
    assert_eq!(role("SUM(A1,1)"), ContentRole::Latent);
    assert_eq!(role("Rich text"), ContentRole::Latent);
    assert_eq!(role("45123.00"), ContentRole::Latent);
    assert_eq!(role("1"), ContentRole::Latent);
    assert_eq!(role("Cell comment"), ContentRole::Latent);
    assert_eq!(role("GlobalName"), ContentRole::Primary);
    assert_eq!(role("First!$A$1"), ContentRole::Latent);
}

#[test]
fn strict_namespace_and_macro_enabled_package_are_supported_without_opening_vba() {
    let strict = extract_excel_candidate_outcome(&core_fixture(Method::Deflate, true, false));
    assert_eq!(strict.coverage, ExtractionCoverage::IndexedComplete);
    let macro_enabled = extract_excel_candidate_outcome(&core_fixture(Method::Store, false, true));
    assert_eq!(macro_enabled.coverage, ExtractionCoverage::IndexedComplete);
    assert!(
        macro_enabled
            .sections
            .iter()
            .any(|section| section.content == "Shared")
    );
}

#[test]
fn utf16le_workbook_and_utf16be_worksheet_are_decoded() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let workbook_source = String::from_utf8(workbook(
        ns,
        r#"<sheet name="Encoded" sheetId="1" r:id="rSheet"/>"#,
        "",
    ))
    .expect("workbook");
    let sheet_source = format!(
        r#"<?xml version="1.0" encoding="UTF-16BE"?><worksheet xmlns="{ns}"><sheetData><row><c t="inlineStr"><is><t>encoded text</t></is></c></row></sheetData></worksheet>"#
    );
    let bytes = package(
        Method::Store,
        false,
        false,
        utf16(
            &workbook_source.replacen(
                "<workbook",
                "<?xml version=\"1.0\" encoding=\"UTF-16LE\"?><workbook",
                1,
            ),
            true,
        ),
        relationships(&format!(
            r#"<Relationship Id="rSheet" Type="{OFFICE_REL}/worksheet" Target="worksheets/sheet.xml"/>"#
        )),
        vec![OwnedEntry::xml(
            "xl/worksheets/sheet.xml",
            utf16(&sheet_source, false),
            Method::Store,
        )],
    );
    let candidate = extract_excel_candidate_outcome(&bytes);
    assert_eq!(candidate.coverage, ExtractionCoverage::IndexedComplete);
    assert!(
        candidate
            .sections
            .iter()
            .any(|section| section.content == "encoded text")
    );
}

#[test]
fn shared_formula_dependent_and_formula_without_cache_do_not_invent_values() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let bytes = package(
        Method::Store,
        false,
        false,
        workbook(ns, r#"<sheet name="S" sheetId="1" r:id="rS"/>"#, ""),
        relationships(&format!(
            r#"<Relationship Id="rS" Type="{OFFICE_REL}/worksheet" Target="worksheets/s.xml"/>"#
        )),
        vec![OwnedEntry::xml(
            "xl/worksheets/s.xml",
            worksheet(
                ns,
                r#"<sheetData><row><c r="A1"><f t="shared" si="0">A2+1</f><v>3</v></c><c r="B1"><f t="shared" si="0"/></c><c r="C1"><f>NOW()</f></c></row></sheetData>"#,
            ),
            Method::Store,
        )],
    );
    let candidate = extract_excel_candidate_outcome(&bytes);
    let content = candidate
        .sections
        .iter()
        .map(|section| section.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(content, ["S", "3", "A2+1", "NOW()"]);
}

#[test]
fn all_cell_types_preserve_stored_text_without_style_rendering() {
    let source = format!(
        r#"<worksheet xmlns="{}"><sheetData><row><c><v>001.2300</v></c><c t="b"><v>0</v></c><c t="e"><v>#DIV/0!</v></c><c t="d"><v>2026-08-11T01:02:03Z</v></c><c t="str"><v>cached</v></c><c t="inlineStr"><is><r><t>in</t></r><r><t>line</t></r></is></c></row></sheetData></worksheet>"#,
        std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace")
    );
    let model = parse_worksheet(source.as_bytes(), None, &mut XmlBudget::default()).expect("sheet");
    let values = model
        .cells
        .values()
        .filter_map(|cell| cell.value.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        [
            "001.2300",
            "0",
            "#DIV/0!",
            "2026-08-11T01:02:03Z",
            "cached",
            "inline"
        ]
    );
}

#[test]
fn missing_or_bad_shared_strings_are_required_part_failures() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    for value in ["0", "9", "-1"] {
        let bytes = package(
            Method::Store,
            false,
            false,
            workbook(ns, r#"<sheet name="S" sheetId="1" r:id="rS"/>"#, ""),
            relationships(&format!(
                r#"<Relationship Id="rS" Type="{OFFICE_REL}/worksheet" Target="worksheets/s.xml"/>"#
            )),
            vec![OwnedEntry::xml(
                "xl/worksheets/s.xml",
                worksheet(
                    ns,
                    &format!(r#"<sheetData><row><c t="s"><v>{value}</v></c></row></sheetData>"#),
                ),
                Method::Store,
            )],
        );
        assert_eq!(
            extract_excel_candidate_outcome(&bytes).coverage,
            ExtractionCoverage::Quarantined
        );
    }
}

#[test]
fn comments_are_coordinate_sorted_and_duplicate_refs_are_rejected() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let source = format!(
        r#"<comments xmlns="{ns}"><commentList><comment ref="C3"><text><t>third</t></text></comment><comment ref="A1"><text><t>first</t></text></comment></commentList></comments>"#
    );
    let comments = parse_comments(source.as_bytes(), &mut XmlBudget::default()).expect("comments");
    assert_eq!(
        comments.values().map(String::as_str).collect::<Vec<_>>(),
        ["first", "third"]
    );

    let duplicate = format!(
        r#"<comments xmlns="{ns}"><commentList><comment ref="A1"><text><t>a</t></text></comment><comment ref="A1"><text><t>b</t></text></comment></commentList></comments>"#
    );
    assert!(parse_comments(duplicate.as_bytes(), &mut XmlBudget::default()).is_err());
}

#[test]
fn invalid_backward_duplicate_and_out_of_grid_references_are_rejected() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    for body in [
        r#"<sheetData><row r="2"><c r="A1"><v>x</v></c></row></sheetData>"#,
        r#"<sheetData><row><c r="B1"><v>x</v></c><c r="A1"><v>y</v></c></row></sheetData>"#,
        r#"<sheetData><row><c r="XFE1"><v>x</v></c></row></sheetData>"#,
        r#"<sheetData><row r="1048577"/></sheetData>"#,
    ] {
        assert!(parse_worksheet(&worksheet(ns, body), None, &mut XmlBudget::default()).is_err());
    }
}

#[test]
fn missing_comment_part_is_partial_and_threaded_comments_are_disclosed() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let workbook_rels = relationships(&format!(
        r#"<Relationship Id="rS" Type="{OFFICE_REL}/worksheet" Target="worksheets/s.xml"/>"#
    ));
    let sheet_rels = relationships(&format!(
        r#"<Relationship Id="c" Type="{OFFICE_REL}/comments" Target="../missing.xml"/><Relationship Id="t" Type="{OFFICE_REL}/threadedComment" Target="../threaded.xml"/>"#
    ));
    let bytes = package(
        Method::Store,
        false,
        false,
        workbook(ns, r#"<sheet name="S" sheetId="1" r:id="rS"/>"#, ""),
        workbook_rels,
        vec![
            OwnedEntry::xml(
                "xl/worksheets/s.xml",
                worksheet(ns, r#"<sheetData><row><c><v>x</v></c></row></sheetData>"#),
                Method::Store,
            ),
            OwnedEntry::xml("xl/worksheets/_rels/s.xml.rels", sheet_rels, Method::Store),
            OwnedEntry::opaque("xl/threaded.xml", b"unopened-threaded", Method::Other(12)),
        ],
    );
    let candidate = extract_excel_candidate_outcome(&bytes);
    assert_eq!(candidate.coverage, ExtractionCoverage::IndexedPartial);
    assert_eq!(candidate.notices.len(), 2);
}

#[test]
fn relationship_suffix_cannot_relabel_an_opaque_payload_as_a_worksheet() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let bytes = package(
        Method::Store,
        false,
        false,
        workbook(ns, r#"<sheet name="S" sheetId="1" r:id="rS"/>"#, ""),
        relationships(
            r#"<Relationship Id="rS" Type="https://attacker.invalid/worksheet" Target="worksheets/payload.bin"/>"#,
        ),
        vec![OwnedEntry::opaque(
            "xl/worksheets/payload.bin",
            b"opaque-payload-must-never-open",
            Method::Other(12),
        )],
    );

    let candidate = extract_excel_candidate_outcome(&bytes);
    assert_eq!(candidate.coverage, ExtractionCoverage::Quarantined);
    assert_eq!(candidate.notices[0].code, "excel_required_part_invalid");
}

#[test]
fn relabeled_vba_project_is_rejected_before_its_zip_entry_is_opened() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let content_types = format!(
        r#"<Types xmlns="{CONTENT_NS}"><Override PartName="/xl/workbook.xml" ContentType="{XLSX_WORKBOOK_CONTENT_TYPE}"/><Override PartName="/xl/vbaProject.bin" ContentType="{WORKSHEET_CONTENT_TYPE}"/></Types>"#,
    );
    let root = format!(
        r#"<Relationships xmlns="{REL_NS}"><Relationship Id="rId1" Type="{OFFICE_REL}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
    );
    let workbook_relationships = relationships(&format!(
        r#"<Relationship Id="rS" Type="{OFFICE_REL}/worksheet" Target="vbaProject.bin"/>"#,
    ));
    let bytes = zip(vec![
        OwnedEntry::xml("[Content_Types].xml", content_types, Method::Store),
        OwnedEntry::xml("_rels/.rels", root, Method::Store),
        OwnedEntry::xml(
            "xl/workbook.xml",
            workbook(ns, r#"<sheet name="S" sheetId="1" r:id="rS"/>"#, ""),
            Method::Store,
        ),
        OwnedEntry::xml(
            "xl/_rels/workbook.xml.rels",
            workbook_relationships,
            Method::Store,
        ),
        OwnedEntry::opaque(
            "xl/vbaProject.bin",
            b"sentinel-macro-payload-must-never-open",
            Method::Other(12),
        ),
    ]);

    let candidate = extract_excel_candidate_outcome(&bytes);
    assert_eq!(candidate.coverage, ExtractionCoverage::Quarantined);
    assert_eq!(candidate.notices.len(), 1);
    assert_eq!(candidate.notices[0].code, "excel_required_part_invalid");
}

#[test]
fn relabeled_vba_project_cannot_be_opened_as_the_workbook_part() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let content_types = format!(
        r#"<Types xmlns="{CONTENT_NS}"><Override PartName="/xl/vbaProject.bin" ContentType="{XLSX_WORKBOOK_CONTENT_TYPE}"/><Override PartName="/xl/worksheets/s.xml" ContentType="{WORKSHEET_CONTENT_TYPE}"/></Types>"#,
    );
    let root = format!(
        r#"<Relationships xmlns="{REL_NS}"><Relationship Id="rId1" Type="{OFFICE_REL}/officeDocument" Target="xl/vbaProject.bin"/></Relationships>"#,
    );
    let workbook_relationships = relationships(&format!(
        r#"<Relationship Id="rS" Type="{OFFICE_REL}/worksheet" Target="worksheets/s.xml"/>"#,
    ));
    let bytes = zip(vec![
        OwnedEntry::xml("[Content_Types].xml", content_types, Method::Store),
        OwnedEntry::xml("_rels/.rels", root, Method::Store),
        OwnedEntry::xml(
            "xl/vbaProject.bin",
            workbook(
                ns,
                r#"<sheet name="vba-payload-marker" sheetId="1" r:id="rS"/>"#,
                "",
            ),
            Method::Store,
        ),
        OwnedEntry::xml(
            "xl/_rels/vbaProject.bin.rels",
            workbook_relationships,
            Method::Store,
        ),
        OwnedEntry::xml(
            "xl/worksheets/s.xml",
            worksheet(
                ns,
                r#"<sheetData><row><c t="inlineStr"><is><t>worksheet marker</t></is></c></row></sheetData>"#,
            ),
            Method::Store,
        ),
    ]);

    let candidate = extract_excel_candidate_outcome(&bytes);
    assert_eq!(candidate.coverage, ExtractionCoverage::Quarantined);
    assert_eq!(candidate.notices[0].code, "excel_required_part_invalid");
    assert!(candidate.sections.is_empty());
}

#[test]
fn unknown_external_calc_style_and_drawing_parts_remain_unopened() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let extra_relationships = ["styles", "calcChain", "externalLink", "drawing"]
        .iter()
        .enumerate()
        .map(|(index, suffix)| format!(r#"<Relationship Id="x{index}" Type="{OFFICE_REL}/{suffix}" Target="opaque{index}.bin"/>"#))
        .collect::<String>();
    let bytes = package(
        Method::Store,
        false,
        false,
        workbook(ns, r#"<sheet name="S" sheetId="1" r:id="rS"/>"#, ""),
        relationships(&format!(
            r#"<Relationship Id="rS" Type="{OFFICE_REL}/worksheet" Target="worksheets/s.xml"/>{extra_relationships}"#
        )),
        std::iter::once(OwnedEntry::xml(
            "xl/worksheets/s.xml",
            worksheet(
                ns,
                r#"<sheetData><row><c s="99"><v>45123.00</v></c></row></sheetData>"#,
            ),
            Method::Store,
        ))
        .chain((0..4).map(|index| {
            OwnedEntry::opaque(
                &format!("xl/opaque{index}.bin"),
                b"unopened",
                Method::Other(12),
            )
        }))
        .collect(),
    );
    let candidate = extract_excel_candidate_outcome(&bytes);
    assert_eq!(candidate.coverage, ExtractionCoverage::IndexedComplete);
    assert!(
        candidate
            .sections
            .iter()
            .any(|section| section.content == "45123.00")
    );
}

#[test]
fn cfb_encrypted_and_valid_non_spreadsheet_inputs_are_refused_honestly() {
    let mut cfb = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1".to_vec();
    cfb.extend_from_slice(b"encrypted package");
    assert_eq!(
        extract_excel_candidate_outcome(&cfb).coverage,
        ExtractionCoverage::Unreadable
    );
    let ordinary_zip = zip(vec![OwnedEntry::xml(
        "plain.txt",
        b"hello".to_vec(),
        Method::Store,
    )]);
    assert_eq!(
        extract_excel_candidate_outcome(&ordinary_zip).coverage,
        ExtractionCoverage::SkippedNoExtractableText
    );
}

#[test]
fn dtd_is_rejected_without_entity_expansion() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let source = format!(
        r#"<!DOCTYPE sst [<!ENTITY x "boom">]><sst xmlns="{ns}"><si><t>&x;</t></si></sst>"#
    );
    assert!(parse_shared_strings(source.as_bytes(), &mut XmlBudget::default()).is_err());
}

#[test]
fn workbook_sheet_defined_name_and_column_range_count_boundaries() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let sheets = (0..MAX_WORKBOOK_SHEETS)
        .map(|index| {
            format!(
                r#"<sheet name="S{index}" sheetId="{}" r:id="r{index}"/>"#,
                index + 1
            )
        })
        .collect::<String>();
    assert_eq!(
        parse_workbook(&workbook(ns, &sheets, ""), &mut XmlBudget::default())
            .expect("sheet limit")
            .sheets
            .len(),
        MAX_WORKBOOK_SHEETS
    );
    let sheets_plus = format!(r#"{sheets}<sheet name="overflow" sheetId="999" r:id="overflow"/>"#);
    assert!(parse_workbook(&workbook(ns, &sheets_plus, ""), &mut XmlBudget::default()).is_err());

    let names = (0..MAX_DEFINED_NAMES)
        .map(|index| format!(r#"<definedName name="N{index}">A1</definedName>"#))
        .collect::<String>();
    assert_eq!(
        parse_workbook(&workbook(ns, "", &names), &mut XmlBudget::default())
            .expect("name limit")
            .defined_names
            .len(),
        MAX_DEFINED_NAMES
    );
    let names_plus = format!(r#"{names}<definedName name="overflow">A1</definedName>"#);
    assert!(parse_workbook(&workbook(ns, "", &names_plus), &mut XmlBudget::default()).is_err());

    let columns = (0..MAX_COLUMN_RANGES_PER_SHEET)
        .map(|_| r#"<col min="1" max="1" hidden="1"/>"#)
        .collect::<String>();
    let at_limit = worksheet(ns, &format!(r#"<cols>{columns}</cols><sheetData/>"#));
    assert!(parse_worksheet(&at_limit, None, &mut XmlBudget::default()).is_ok());
    let over = worksheet(
        ns,
        &format!(r#"<cols>{columns}<col min="2" max="2" hidden="1"/></cols><sheetData/>"#),
    );
    assert!(parse_worksheet(&over, None, &mut XmlBudget::default()).is_err());
}

#[test]
fn shared_string_cell_and_comment_count_boundaries() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let strings = r#"<si><t>x</t></si>"#.repeat(MAX_SHARED_STRING_ENTRIES);
    let source = format!(r#"<sst xmlns="{ns}">{strings}</sst>"#);
    assert_eq!(
        parse_shared_strings(source.as_bytes(), &mut XmlBudget::default())
            .expect("shared string limit")
            .len(),
        MAX_SHARED_STRING_ENTRIES
    );
    let over = format!(r#"<sst xmlns="{ns}">{strings}<si><t>x</t></si></sst>"#);
    assert!(parse_shared_strings(over.as_bytes(), &mut XmlBudget::default()).is_err());

    let cells = (0..MAX_CELL_RECORDS)
        .map(|index| format!(r#"<row r="{}"><c><v>x</v></c></row>"#, index + 1))
        .collect::<String>();
    let source = worksheet(ns, &format!(r#"<sheetData>{cells}</sheetData>"#));
    assert_eq!(
        parse_worksheet(&source, None, &mut XmlBudget::default())
            .expect("cell limit")
            .cells
            .len(),
        MAX_CELL_RECORDS
    );
    let over = worksheet(
        ns,
        &format!(
            r#"<sheetData>{cells}<row r="{}"><c><v>x</v></c></row></sheetData>"#,
            MAX_CELL_RECORDS + 1
        ),
    );
    assert!(parse_worksheet(&over, None, &mut XmlBudget::default()).is_err());

    let comments = (0..MAX_COMMENTS)
        .map(|index| {
            format!(
                r#"<comment ref="A{}"><text><t>x</t></text></comment>"#,
                index + 1
            )
        })
        .collect::<String>();
    let source =
        format!(r#"<comments xmlns="{ns}"><commentList>{comments}</commentList></comments>"#);
    assert_eq!(
        parse_comments(source.as_bytes(), &mut XmlBudget::default())
            .expect("comment limit")
            .len(),
        MAX_COMMENTS
    );
    let over = format!(
        r#"<comments xmlns="{ns}"><commentList>{comments}<comment ref="B1"><text><t>x</t></text></comment></commentList></comments>"#
    );
    assert!(parse_comments(over.as_bytes(), &mut XmlBudget::default()).is_err());
}

#[test]
fn retained_string_comment_name_and_sheet_byte_boundaries() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let chunk = "x".repeat(1024);
    let runs = (0..(MAX_SHARED_STRING_BYTES / chunk.len()))
        .map(|_| format!(r#"<r><t>{chunk}</t></r>"#))
        .collect::<String>();
    let shared = format!(r#"<sst xmlns="{ns}"><si>{runs}</si></sst>"#);
    let table = parse_shared_strings(shared.as_bytes(), &mut XmlBudget::default())
        .expect("shared string byte limit");
    assert_eq!(table[0].len(), MAX_SHARED_STRING_BYTES);
    let shared_over = format!(r#"<sst xmlns="{ns}"><si>{runs}<r><t>x</t></r></si></sst>"#);
    assert!(parse_shared_strings(shared_over.as_bytes(), &mut XmlBudget::default()).is_err());

    let comment = format!(
        r#"<comments xmlns="{ns}"><commentList><comment ref="A1"><text>{runs}</text></comment></commentList></comments>"#
    );
    let comments =
        parse_comments(comment.as_bytes(), &mut XmlBudget::default()).expect("comment byte limit");
    assert_eq!(
        comments.values().next().expect("comment").len(),
        MAX_COMMENT_BYTES
    );
    let comment_over = format!(
        r#"<comments xmlns="{ns}"><commentList><comment ref="A1"><text>{runs}<r><t>x</t></r></text></comment></commentList></comments>"#
    );
    assert!(parse_comments(comment_over.as_bytes(), &mut XmlBudget::default()).is_err());

    let definition = "x".repeat(MAX_DEFINED_NAME_BYTES - 1);
    let names = format!(r#"<definedName name="N">{definition}</definedName>"#);
    assert!(parse_workbook(&workbook(ns, "", &names), &mut XmlBudget::default()).is_ok());
    let definition_over = "x".repeat(MAX_DEFINED_NAME_BYTES);
    let names_over = format!(r#"<definedName name="N">{definition_over}</definedName>"#);
    assert!(parse_workbook(&workbook(ns, "", &names_over), &mut XmlBudget::default()).is_err());

    let name_prefix = "s".repeat((MAX_SHEET_NAME_BYTES / MAX_WORKBOOK_SHEETS) - 6);
    let sheets = (0..MAX_WORKBOOK_SHEETS)
        .map(|index| {
            format!(
                r#"<sheet name="{name_prefix}{index:06}" sheetId="{}" r:id="r{index}"/>"#,
                index + 1
            )
        })
        .collect::<String>();
    assert!(parse_workbook(&workbook(ns, &sheets, ""), &mut XmlBudget::default()).is_ok());
    let sheets_over = sheets.replacen(
        &format!(r#"name="{name_prefix}000000""#),
        &format!(r#"name="{name_prefix}x000000""#),
        1,
    );
    assert!(parse_workbook(&workbook(ns, &sheets_over, ""), &mut XmlBudget::default()).is_err());
}

#[test]
fn hidden_very_hidden_and_builtin_defined_names_are_retained_without_dereference() {
    let ns = std::str::from_utf8(SPREADSHEET_NS_TRANSITIONAL).expect("namespace");
    let model = parse_workbook(
        &workbook(
            ns,
            r#"<sheet name="Visible" sheetId="1" r:id="v"/><sheet name="Hidden" sheetId="2" state="hidden" r:id="h"/><sheet name="Very" sheetId="3" state="veryHidden" r:id="vh"/>"#,
            r#"<definedName name="_xlnm.Print_Area" localSheetId="0">Visible!$A$1:$B$2</definedName>"#,
        ),
        &mut XmlBudget::default(),
    )
    .expect("workbook model");
    assert!(!model.sheets[0].visibility.is_hidden());
    assert!(model.sheets[1].visibility.is_hidden());
    assert!(model.sheets[2].visibility.is_hidden());
    assert_eq!(model.defined_names[0].name, "_xlnm.Print_Area");
    assert_eq!(model.defined_names[0].definition, "Visible!$A$1:$B$2");
}

#[test]
fn output_byte_section_and_heading_boundaries_are_charged_before_push() {
    let mut output = Vec::<ExcelSection>::new();
    let mut budget = OutputBudget::default();
    budget
        .push(
            &mut output,
            &[],
            "x".repeat(MAX_OUTPUT_BYTES),
            ContentRole::Primary,
            None,
        )
        .expect("output byte limit");
    assert_eq!(
        budget.push(&mut output, &[], "x".to_owned(), ContentRole::Primary, None),
        Err(ExcelError::XmlLimitExceeded)
    );

    let mut output = Vec::new();
    let mut budget = OutputBudget::default();
    for _ in 0..MAX_OUTPUT_SECTIONS {
        budget
            .push(&mut output, &[], "x".to_owned(), ContentRole::Primary, None)
            .expect("section limit");
    }
    assert_eq!(
        budget.push(&mut output, &[], "x".to_owned(), ContentRole::Primary, None),
        Err(ExcelError::XmlLimitExceeded)
    );

    let mut output = Vec::new();
    let mut budget = OutputBudget::default();
    budget
        .push(
            &mut output,
            &["h".repeat(MAX_OUTPUT_HEADING_BYTES)],
            "x".to_owned(),
            ContentRole::Primary,
            None,
        )
        .expect("heading limit");
    assert_eq!(
        budget.push(
            &mut output,
            &["h".to_owned()],
            "x".to_owned(),
            ContentRole::Primary,
            None,
        ),
        Err(ExcelError::XmlLimitExceeded)
    );
}
