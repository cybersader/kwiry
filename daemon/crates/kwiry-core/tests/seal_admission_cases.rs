// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pins OpenClast's shared seal-admission case list
//! (`fixtures/seal-admission/seal-admission-cases.v1.json`, copied verbatim;
//! see the README beside it) against `check_room`, the function
//! `kwiry checkroom` runs, which drives the governed build's own labelling pass
//! (`room_check::prepare_room`).
//!
//! Each case becomes a one-room directory holding the case's exact path and
//! bytes plus a clean control note, so the case file is the only thing that can
//! withhold the label. Kwiry's verdict must equal the list's stated column
//! (`engine_rule_657bced6`); where that column is null the measured column
//! (`engine_0ce9ff91`) is used. A withheld case must also carry the Kwiry reason
//! and class that the partner's reason code names. For the cases with no stated
//! verdict, Kwiry must also give exactly the verdict, exit code, reason and
//! class the partner recorded from `kwiry checkroom` at 4e3f5446
//! (`engine_checkroom_4e3f5446`). Disagreements are listed in
//! `KNOWN_DISAGREEMENTS` with Kwiry's actual verdict, so they stay documented
//! and a change in either direction fails this test. Kwiry's behaviour is not
//! adjusted to the list.
#![cfg(all(feature = "native", target_os = "linux"))]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use base64::Engine as _;
use kwiry_core::{
    Config, RoomCheckClass, RoomCheckReason, RoomCheckReport, VaultRegistration, check_room,
};
use serde_json::Value;

const CASES: &str = include_str!("fixtures/seal-admission/seal-admission-cases.v1.json");
const STATED: &str = "engine_rule_657bced6";
const MEASURED: &str = "engine_0ce9ff91";
const CHECKROOM: &str = "engine_checkroom_4e3f5446";
const CONTROL: &str = "control.md";

/// What Kwiry decides for one case: `None` when labelled, otherwise the
/// reason and class of the case file's finding.
type Verdict = Option<(RoomCheckReason, RoomCheckClass)>;

/// Cases where Kwiry's verdict differs from the stated column, with Kwiry's
/// actual verdict. Each entry is a reported disagreement, not a fix.
const KNOWN_DISAGREEMENTS: &[(&str, Verdict)] = &[];

/// Cases this test does not build, with the reason. Every case in the list is
/// one regular file at a plain path, so none needs special filesystem setup.
const SKIPPED: &[(&str, &str)] = &[];

struct Case {
    id: String,
    path: String,
    bytes: Vec<u8>,
    /// `true` when the stated (or, if null, measured) column says labelled.
    labelled: bool,
    stated_is_null: bool,
    partner_reason: Option<String>,
    /// The partner's recorded `kwiry checkroom` result for this case.
    checkroom: Verdict,
}

/// Parses an `engine_checkroom_4e3f5446` entry: `{verdict, exit, reason, class}`.
fn checkroom_column(id: &str, column: &Value) -> Verdict {
    let text = |key: &str| column[key].as_str();
    let exit = column["exit"].as_i64().unwrap();
    match text("verdict").unwrap() {
        "labelled" => {
            assert_eq!(exit, 0, "{id}");
            assert!(
                column["reason"].is_null() && column["class"].is_null(),
                "{id}"
            );
            None
        }
        "withheld" => {
            assert_eq!(exit, 1, "{id}");
            let reason = RoomCheckReason::ALL
                .into_iter()
                .find(|reason| Some(reason.as_str()) == text("reason"))
                .unwrap_or_else(|| panic!("{id}: unknown reason in {column}"));
            assert_eq!(
                serde_json::to_value(reason.class()).unwrap(),
                column["class"],
                "{id}: reason and class disagree in the column"
            );
            Some((reason, reason.class()))
        }
        other => panic!("{id}: unknown checkroom verdict {other:?}"),
    }
}

fn cases() -> Vec<Case> {
    let list: Value = serde_json::from_str(CASES).unwrap();
    assert_eq!(list["format"], "openclast-seal-admission-cases.v1");
    list["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            let text = |key: &str| case[key].as_str().map(str::to_owned);
            let bytes = match text("base64") {
                Some(encoded) => base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap(),
                None => {
                    let times = usize::try_from(case["times"].as_u64().unwrap()).unwrap();
                    let mut bytes = text("prefix").unwrap().into_bytes();
                    bytes.extend(text("repeat").unwrap().repeat(times).into_bytes());
                    bytes.extend(text("suffix").unwrap().into_bytes());
                    bytes
                }
            };
            let stated_is_null = case[STATED].is_null();
            let column = if stated_is_null { MEASURED } else { STATED };
            let labelled = match case[column].as_str().unwrap() {
                "labelled" => true,
                "withheld" => false,
                other => panic!("unknown verdict {other:?}"),
            };
            Case {
                id: text("id").unwrap(),
                path: text("path").unwrap(),
                bytes,
                labelled,
                stated_is_null,
                partner_reason: text("reason"),
                checkroom: checkroom_column(&text("id").unwrap(), &case[CHECKROOM]),
            }
        })
        .collect()
}

/// The Kwiry reason a partner reason code names, for a withheld case.
fn kwiry_reason(partner: &str) -> RoomCheckReason {
    match partner {
        "frontmatter_invalid" => RoomCheckReason::InvalidFrontmatter,
        "nul_byte" => RoomCheckReason::NulByte,
        "invalid_utf8" => RoomCheckReason::NonUtf8Content,
        "too_large" => RoomCheckReason::OverSizeBudget,
        "section_limit" => RoomCheckReason::SectionLimit,
        "heading_path_limit" => RoomCheckReason::HeadingPathLimit,
        "not_markdown" => RoomCheckReason::NonMarkdownFormat,
        "path_unsafe" => RoomCheckReason::InvalidPath,
        other => panic!("no Kwiry reason for partner reason {other:?}"),
    }
}

fn check(scratch: &Path, case: &Case) -> RoomCheckReport {
    let room = tempfile::Builder::new()
        .prefix("seal-case-")
        .tempdir_in(scratch)
        .unwrap();
    let root = room.path().canonicalize().unwrap();
    fs::write(root.join(CONTROL), "# Control\nclean body\n").unwrap();
    fs::write(root.join(&case.path), &case.bytes).unwrap();
    let config = Config {
        vaults: vec![VaultRegistration {
            snapshot_id: None,
            id: "seal-case".into(),
            path: root,
            room: Some("seal-case".into()),
        }],
        ..Config::default()
    };
    check_room(&config, 0, None).unwrap()
}

fn verdict(case: &Case, report: &RoomCheckReport) -> Verdict {
    assert!(
        report.walk_defects.is_empty(),
        "{}: walk defects {report:?}",
        case.id
    );
    assert_eq!(
        report.would_label,
        report.offenders.is_empty(),
        "{}: verdict and findings disagree: {report:?}",
        case.id
    );
    match report.offenders.as_slice() {
        [] => None,
        [finding] => {
            assert_eq!(
                finding.path, case.path,
                "{}: the control note was refused",
                case.id
            );
            assert_eq!(finding.class, finding.reason.class(), "{}", case.id);
            Some((finding.reason, finding.class))
        }
        _ => panic!("{}: more than one offender {report:?}", case.id),
    }
}

#[test]
fn seal_admission_cases_match_the_stated_verdict() {
    let cases = cases();
    assert_eq!(cases.len(), 90);
    let ids: BTreeSet<_> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), cases.len(), "case ids are unique");
    let skipped: BTreeMap<_, _> = SKIPPED.iter().copied().collect();
    let known: BTreeMap<_, _> = KNOWN_DISAGREEMENTS.iter().copied().collect();
    for id in skipped.keys().chain(known.keys()) {
        assert!(ids.contains(id), "listed case {id} is not in the case list");
    }

    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("seal-admission-cases");
    fs::create_dir_all(&scratch).unwrap();
    let mut failures = Vec::new();
    let mut agreed = 0;
    let mut null_stated = Vec::new();
    for case in cases
        .iter()
        .filter(|case| !skipped.contains_key(case.id.as_str()))
    {
        let report = check(&scratch, case);
        let actual = verdict(case, &report);
        if case.id == "name-backslash" {
            // A `\` is a legal Linux name byte; the walk admits the file and
            // source preparation refuses the path, as the daemon does.
            assert_eq!(case.path, "back\\slash.md");
            assert_eq!(
                actual,
                Some((RoomCheckReason::InvalidPath, RoomCheckClass::Structural))
            );
        }
        let expected: Verdict = if case.labelled {
            None
        } else {
            let reason = kwiry_reason(case.partner_reason.as_deref().unwrap());
            Some((reason, reason.class()))
        };
        if case.stated_is_null {
            null_stated.push(case.id.as_str());
            if actual != case.checkroom {
                failures.push(format!(
                    "{}: {CHECKROOM} records {:?}, Kwiry {actual:?}",
                    case.id, case.checkroom
                ));
            }
        }
        match known.get(case.id.as_str()) {
            Some(listed) if *listed == actual && actual != expected => {}
            Some(listed) => failures.push(format!(
                "{}: listed as a disagreement giving {listed:?}, but Kwiry gives {actual:?} (stated {expected:?})",
                case.id
            )),
            None if actual == expected => agreed += 1,
            None => failures.push(format!(
                "{}: stated {expected:?}, Kwiry {actual:?}",
                case.id
            )),
        }
    }
    let _ = fs::remove_dir_all(&scratch);
    eprintln!(
        "seal-admission cases: {agreed} agree, {} known disagreements, {} skipped; null stated column (measured used): {null_stated:?}",
        known.len(),
        skipped.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    assert_eq!(agreed + known.len() + skipped.len(), cases.len());
    // 79 cases carry a stated verdict; these 11 do not and are held to the
    // measured column instead.
    assert_eq!(
        null_stated,
        [
            "dots-then-thematic-break",
            "dots-then-thematic-break-valid-yaml",
            "lf-split-cr-inside-block",
            "lf-split-cr-in-closer-line",
            "merge-scalar-value",
            "key-1024-bytes",
            "key-1025-bytes",
            "tab-in-quoted",
            "surrogate-escape",
            "sections-100k",
            "headings-in-fence-200k",
        ]
    );
}

/// The full, closed set of reasons and classes `checkroom` emits. Adding,
/// removing or renaming a reason, or moving one between classes, fails here:
/// the match is exhaustive, and every name is compared as serialized.
#[test]
fn room_check_reasons_and_classes_are_pinned() {
    fn pinned(reason: RoomCheckReason) -> (&'static str, &'static str) {
        use RoomCheckReason as R;
        match reason {
            R::InvalidFrontmatter => ("invalid_frontmatter", "content"),
            R::FrontmatterOverSize => ("frontmatter_over_size", "content"),
            R::NonUtf8Content => ("non_utf8_content", "content"),
            R::NulByte => ("nul_byte", "content"),
            R::OverSizeBudget => ("over_size_budget", "content"),
            R::SectionLimit => ("section_limit", "content"),
            R::HeadingPathLimit => ("heading_path_limit", "content"),
            R::ChunkInventoryLimit => ("chunk_inventory_limit", "content"),
            R::IncompleteExtraction => ("incomplete_extraction", "content"),
            R::IoUnavailable => ("io_unavailable", "transient"),
            R::ChangedDuringRead => ("changed_during_read", "transient"),
            R::NonMarkdownFormat => ("non_markdown_format", "structural"),
            R::InvalidPath => ("invalid_path", "structural"),
            R::HardLink => ("hard_link", "structural"),
            R::SpecialFile => ("special_file", "structural"),
            R::BoundaryEscape => ("boundary_escape", "structural"),
            R::NonUtf8Name => ("non_utf8_name", "structural"),
            R::RoomBoundary => ("room_boundary", "structural"),
            R::Unclassified => ("unclassified", "structural"),
        }
    }
    let expected = [
        "invalid_frontmatter",
        "frontmatter_over_size",
        "non_utf8_content",
        "nul_byte",
        "over_size_budget",
        "section_limit",
        "heading_path_limit",
        "chunk_inventory_limit",
        "incomplete_extraction",
        "non_markdown_format",
        "invalid_path",
        "hard_link",
        "special_file",
        "boundary_escape",
        "non_utf8_name",
        "io_unavailable",
        "changed_during_read",
        "room_boundary",
        "unclassified",
    ];
    let names: Vec<_> = RoomCheckReason::ALL.iter().map(|r| r.as_str()).collect();
    assert_eq!(names, expected);
    for reason in RoomCheckReason::ALL {
        let (name, class) = pinned(reason);
        assert_eq!(reason.as_str(), name);
        assert_eq!(serde_json::to_value(reason).unwrap(), name);
        assert_eq!(serde_json::to_value(reason.class()).unwrap(), class);
    }
    // Exhaustive, so a new class fails to compile here.
    let class_name = |class: RoomCheckClass| match class {
        RoomCheckClass::Content => "content",
        RoomCheckClass::Transient => "transient",
        RoomCheckClass::Structural => "structural",
    };
    for class in [
        RoomCheckClass::Content,
        RoomCheckClass::Transient,
        RoomCheckClass::Structural,
    ] {
        assert_eq!(serde_json::to_value(class).unwrap(), class_name(class));
    }
}
