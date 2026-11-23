//! Acceptance for the offline room preflight: on every fixture room, the
//! governed daemon's publication label and `check_room` give the same answer.
//! The daemon side is observed twice, as in `label_edges`: after a cold build
//! plus boot reconcile, and after a label change forces a strict-hash walk.
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::*;
use crate::room_check::{RoomCheckClass, RoomCheckReason as R, RoomCheckReport, check_room};

type Guard = Option<Box<dyn std::any::Any>>;

struct Daemon {
    initial: Option<String>,
    relabelled: Option<String>,
    healthy_hits: usize,
}

fn run(setup: impl FnOnce(&Path) -> Guard) -> (Daemon, RoomCheckReport) {
    let mut f = Fixture::new(Some("apricots"));
    f.manager.shutdown().unwrap();
    let room = f.config.vaults[0].path.clone();
    let _guard = setup(&room);
    let data = f.dir.path().join("edge-data");
    build_index(&f.config, &data).unwrap();
    let runtime = SearchRuntime::new();
    let mut manager = IndexManager::open(f.config.clone(), &data, runtime.clone()).unwrap();
    manager.reconcile(f.config.clone()).unwrap();
    let label = |rt: &SearchRuntime| {
        let r = rt
            .search_authorized_with_generation(
                "apricots",
                10,
                &SearchFilters::default(),
                &f.resources,
            )
            .unwrap();
        (
            r.snapshot_ids[&f.resources[0]]
                .as_ref()
                .map(|s| s.as_str().to_owned()),
            r.hits.len(),
        )
    };
    let (initial, _) = label(&runtime);
    f.config.vaults[0].snapshot_id = Some("snapshot:B".parse().unwrap());
    manager.reconcile(f.config.clone()).unwrap();
    let (relabelled, healthy_hits) = label(&runtime);
    manager.shutdown().unwrap();
    let report = check_room(&f.config, 0, Some(&data)).unwrap();
    (
        Daemon {
            initial,
            relabelled,
            healthy_hits,
        },
        report,
    )
}

/// The daemon and the preflight agree, and both match the pinned expectation.
fn agree(name: &str, setup: impl FnOnce(&Path) -> Guard, labelled: bool, reasons: &[R]) {
    let (daemon, report) = run(setup);
    assert_eq!(
        daemon.initial.is_some(),
        daemon.relabelled.is_some(),
        "{name}: daemon passes disagree"
    );
    assert_eq!(
        report.would_label,
        daemon.relabelled.is_some(),
        "{name}: checkroom disagrees with the daemon: {report:?}"
    );
    assert_eq!(report.would_label, labelled, "{name}: {report:?}");
    assert_eq!(
        report.would_label,
        report.offenders.is_empty() && report.walk_defects.is_empty(),
        "{name}: verdict and findings disagree: {report:?}"
    );
    assert_eq!(daemon.healthy_hits, 1, "{name}: room stopped serving");
    let mut found: Vec<_> = report
        .offenders
        .iter()
        .chain(&report.walk_defects)
        .map(|f| f.reason)
        .collect();
    found.sort_by_key(|r| r.as_str());
    let mut expected = reasons.to_vec();
    expected.sort_by_key(|r| r.as_str());
    assert_eq!(found, expected, "{name}: {report:?}");
}

fn write(name: &'static str, bytes: Vec<u8>) -> impl FnOnce(&Path) -> Guard {
    move |room| {
        fs::write(room.join(name), bytes).unwrap();
        None
    }
}

fn sized(len: usize) -> Vec<u8> {
    let mut bytes = b"# Big\n".to_vec();
    bytes.resize(len, b'a');
    bytes
}

fn repeat(unit: &str, times: usize) -> Vec<u8> {
    unit.repeat(times).into_bytes()
}

#[test]
fn checkroom_matches_the_daemon_label_on_every_fixture_room() {
    let max = crate::model::MAX_FILE_BYTES as usize;
    agree("clean room", |_| None, true, &[]);

    // Every case pinned by `label_edges`.
    agree("zero-byte", write("edge.md", Vec::new()), true, &[]);
    agree(
        "whitespace-only",
        write("edge.md", b"  \n\t\n   \n".to_vec()),
        true,
        &[],
    );
    agree(
        "unterminated frontmatter",
        write("edge.md", b"---\ntitle: x\nbody text\n".to_vec()),
        true,
        &[],
    );
    agree(
        "frontmatter only",
        write("edge.md", b"---\ntitle: x\n---\n".to_vec()),
        true,
        &[],
    );
    agree(
        "invalid YAML frontmatter",
        write("edge.md", b"---\ntitle: [unclosed\n---\nbody\n".to_vec()),
        false,
        &[R::InvalidFrontmatter],
    );
    agree(
        "invalid UTF-8",
        write("edge.md", b"# Head\nbad \xff\xfe bytes\n".to_vec()),
        false,
        &[R::NonUtf8Content],
    );
    agree(
        "exactly the size budget",
        write("edge.md", sized(max)),
        true,
        &[],
    );
    agree(
        "one byte over the size budget",
        write("edge.md", sized(max + 1)),
        false,
        &[R::OverSizeBudget],
    );

    // Further withheld and kept cases.
    agree(
        "NUL byte",
        write("edge.md", b"# Head\nnul \0 byte\n".to_vec()),
        false,
        &[R::NulByte],
    );
    agree(
        "plain text source",
        write("edge.txt", b"plain".to_vec()),
        false,
        &[R::NonMarkdownFormat],
    );
    agree(
        "canvas source",
        write("board.canvas", br#"{"nodes":[],"edges":[]}"#.to_vec()),
        false,
        &[R::NonMarkdownFormat],
    );
    agree(
        "markdown not named .md",
        write("edge.markdown", b"# Head\n".to_vec()),
        false,
        &[R::NonMarkdownFormat],
    );
    agree(
        "upper-case .MD",
        write("edge.MD", b"# Head\n".to_vec()),
        true,
        &[],
    );
    agree(
        "non-source file is ignored",
        write("image.png", b"\x89PNG".to_vec()),
        true,
        &[],
    );
    agree(
        "dot-named entries are skipped",
        |room| {
            fs::create_dir(room.join(".obsidian")).unwrap();
            fs::write(room.join(".obsidian/bad.md"), b"\xff").unwrap();
            fs::write(room.join(".hidden.md"), b"\xff").unwrap();
            None
        },
        true,
        &[],
    );
    agree(
        "clean nested directory",
        |room| {
            fs::create_dir_all(room.join("a/b")).unwrap();
            fs::write(room.join("a/b/deep.md"), "# Deep\nbody\n").unwrap();
            None
        },
        true,
        &[],
    );
    agree(
        "backslash in a name",
        write("edge\\name.md", b"# Head\n".to_vec()),
        false,
        &[R::InvalidPath],
    );
    agree(
        "hard link",
        |room| {
            fs::write(room.join("one.md"), "# One\n").unwrap();
            fs::hard_link(room.join("one.md"), room.join("two.md")).unwrap();
            None
        },
        false,
        &[R::HardLink, R::HardLink],
    );
    agree(
        "symbolic link to a file",
        |room| {
            fs::write(room.join("target.md"), "# Target\n").unwrap();
            std::os::unix::fs::symlink(room.join("target.md"), room.join("link.md")).unwrap();
            None
        },
        false,
        &[R::BoundaryEscape],
    );
    agree(
        "symbolic link to a directory",
        |room| {
            fs::create_dir(room.join("real")).unwrap();
            std::os::unix::fs::symlink(room.join("real"), room.join("alias")).unwrap();
            None
        },
        false,
        &[R::BoundaryEscape],
    );
    agree(
        "special file",
        |room| {
            let listener = std::os::unix::net::UnixListener::bind(room.join("socket")).unwrap();
            Some(Box::new(listener))
        },
        false,
        &[R::SpecialFile],
    );
    agree(
        "non-UTF-8 name",
        |room| {
            let name = std::ffi::OsStr::from_bytes(b"bad\xff.md");
            fs::write(room.join(name), "# Head\n").unwrap();
            None
        },
        false,
        &[R::NonUtf8Name],
    );
    agree(
        "section limit",
        write(
            "edge.md",
            repeat(
                "# a\n",
                crate::extract::MAX_EXTRACTED_SECTIONS_PER_SOURCE + 1,
            ),
        ),
        false,
        &[R::SectionLimit],
    );
    agree(
        "heading-path limit",
        |room| {
            let mut bytes = format!("# {}\n", "h".repeat(200_000)).into_bytes();
            bytes.extend(repeat("## x\n", 60));
            fs::write(room.join("edge.md"), bytes).unwrap();
            None
        },
        false,
        &[R::HeadingPathLimit],
    );
    agree(
        "chunk inventory limit",
        |room| {
            let mut bytes = repeat("word ", 1_800_000);
            bytes.push(b'\n');
            bytes.extend(repeat("# a\nb\n", 99_990));
            fs::write(room.join("edge.md"), bytes).unwrap();
            None
        },
        false,
        &[R::ChunkInventoryLimit],
    );
}

#[test]
fn checkroom_matches_the_daemon_when_a_read_fails() {
    // The test hook fails every open of this source in this thread, in the
    // daemon's build and reconciles and in the preflight alike.
    crate::room_walk::with_unreadable_source("edge.md", || {
        agree(
            "unreadable source",
            write("edge.md", b"# Edge\nreadable bytes\n".to_vec()),
            false,
            &[R::IoUnavailable],
        );
    });
    let (_, report) = crate::room_walk::with_unreadable_source("edge.md", || {
        run(write("edge.md", b"# Edge\n".to_vec()))
    });
    assert_eq!(report.offenders[0].class, RoomCheckClass::Transient);
}

#[test]
fn every_reason_has_one_class() {
    use RoomCheckClass::{Content, Structural, Transient};
    for reason in R::ALL {
        let expected = match reason {
            R::IoUnavailable | R::ChangedDuringRead => Transient,
            R::NonMarkdownFormat
            | R::InvalidPath
            | R::HardLink
            | R::SpecialFile
            | R::BoundaryEscape
            | R::NonUtf8Name
            | R::RoomBoundary
            | R::Unclassified => Structural,
            _ => Content,
        };
        assert_eq!(reason.class(), expected, "{}", reason.as_str());
    }
}

#[test]
fn limit_findings_carry_section_usage() {
    let (_, report) = run(write(
        "edge.md",
        repeat(
            "# a\n",
            crate::extract::MAX_EXTRACTED_SECTIONS_PER_SOURCE + 1,
        ),
    ));
    let usage = report.offenders[0].usage.unwrap();
    assert_eq!(
        usage.sections,
        crate::extract::MAX_EXTRACTED_SECTIONS_PER_SOURCE + 1
    );
    assert_eq!(
        usage.heading_path_bytes,
        crate::extract::MAX_EXTRACTED_SECTIONS_PER_SOURCE + 1
    );
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["offenders"][0]["path"], "edge.md");
    assert_eq!(json["offenders"][0]["reason"], "section_limit");
    assert_eq!(json["offenders"][0]["class"], "content");
    assert!(!json.to_string().contains("snapshot:"));
}
