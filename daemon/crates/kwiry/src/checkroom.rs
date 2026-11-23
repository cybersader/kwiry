//! `kwiry checkroom`: the governed labelling pass, run offline and read-only
//! over one room directory. No index, data directory, lock, network or write.
use std::path::{Component, PathBuf};

use anyhow::{Context, Result, bail};
use kwiry_core::{
    Config, Paths, ROOM_CHECK_LIMITS, RoomCheckClass, RoomCheckFinding, RoomCheckLimits,
    RoomCheckReport, VaultRegistration, check_room, load_config_for_paths,
};
use serde::Serialize;

use crate::CheckroomArgs;
use crate::engine::{Engine, engine};

const WOULD_LABEL: i32 = 0;
const WOULD_NOT_LABEL: i32 = 1;
const SETUP_ERROR: i32 = 2;

#[derive(Serialize)]
struct JsonReport<'a> {
    #[serde(flatten)]
    report: &'a RoomCheckReport,
    limits: RoomCheckLimits,
    /// Which checker decided, so a caller can record it beside the verdict.
    engine: Engine,
}

/// Returns the process exit code.
pub(crate) fn run(config: Option<PathBuf>, data_dir: Option<PathBuf>, args: &CheckroomArgs) -> i32 {
    // Dependency panic text could quote a source; print a fixed line instead.
    std::panic::set_hook(Box::new(|_| eprintln!("kwiry checkroom: internal failure")));
    let report = setup(config, data_dir, args)
        .and_then(|(config, data_root)| Ok(check_room(&config, 0, data_root.as_deref())?));
    let report = match report {
        Ok(report) => report,
        Err(error) => {
            eprintln!("checkroom: {error:#}");
            return SETUP_ERROR;
        }
    };
    if args.json {
        let json = JsonReport {
            report: &report,
            limits: ROOM_CHECK_LIMITS,
            engine: engine(),
        };
        println!(
            "{}",
            serde_json::to_string(&json).expect("checkroom report serializes")
        );
    } else {
        print!("{}", human(&report));
    }
    if report.would_label {
        WOULD_LABEL
    } else {
        WOULD_NOT_LABEL
    }
}

/// A one-room config holding only the room under check, plus the data
/// directory to compare against. With `--vault`, the named registration is
/// re-pointed at the room and every other registration and state path stays.
fn setup(
    config: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    args: &CheckroomArgs,
) -> Result<(Config, Option<PathBuf>)> {
    if !cfg!(target_os = "linux") {
        bail!("checkroom requires Linux, as governed room ingestion does");
    }
    let room = std::path::absolute(&args.room).context("room path cannot be made absolute")?;
    // The daemon only walks a root spelled exactly as its canonical path. A
    // trailing separator, `.`, `..` or a doubled separator is a spelling to
    // fix, not a property of the room, so refuse it as a usage error. A path
    // through a symbolic link is spelled canonically and is left to the walk,
    // which reports it as `room_boundary`, as the daemon would refuse it.
    let respelled: PathBuf = room.components().collect();
    if respelled.as_os_str() != room.as_os_str()
        || room
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        bail!(
            "room path must be canonical (no trailing separator, `.` or `..`): {}",
            room.display()
        );
    }
    if !room.is_dir() {
        bail!("room is not a directory: {}", room.display());
    }
    let Some(vault_id) = &args.vault else {
        let config = Config {
            vaults: vec![VaultRegistration {
                snapshot_id: None,
                id: "checkroom".into(),
                path: room,
                room: Some("checkroom".into()),
            }],
            ..Config::default()
        };
        return Ok((config, data_dir));
    };
    let paths = Paths::resolve(config, data_dir)?;
    if !paths.config.is_file() {
        bail!("config file not found: {}", paths.config.display());
    }
    let mut config = load_config_for_paths(&paths).context("config does not load")?;
    let Some(index) = config.vaults.iter().position(|v| &v.id == vault_id) else {
        bail!("no registration with vault ID {vault_id:?} in the config");
    };
    let mut vault = config.vaults.remove(index);
    vault.path = room;
    // `check_room` checks the first registration; order does not affect
    // overlap or nesting detection.
    config.vaults.insert(0, vault);
    Ok((config, Some(paths.data_dir)))
}

fn human(report: &RoomCheckReport) -> String {
    let line = |kind: &str, finding: &RoomCheckFinding| {
        let path = if finding.path.is_empty() {
            "."
        } else {
            &finding.path
        };
        let usage = finding.usage.map_or(String::new(), |usage| {
            format!(
                " [sections={}, heading_path_bytes={}]",
                usage.sections, usage.heading_path_bytes
            )
        });
        let class = match finding.class {
            RoomCheckClass::Content => "content",
            RoomCheckClass::Transient => "transient",
            RoomCheckClass::Structural => "structural",
        };
        format!(
            "{kind} {path}: {} [{class}] ({}){usage}\n",
            finding.reason.as_str(),
            finding.detail
        )
    };
    let mut out = String::new();
    for finding in &report.offenders {
        out.push_str(&line("offender", finding));
    }
    for finding in &report.walk_defects {
        out.push_str(&line("walk_defect", finding));
    }
    out.push_str(&format!(
        "checkroom: {} files checked; {}\n",
        report.files_checked,
        if report.would_label {
            "room would be labelled"
        } else {
            "room would NOT be labelled"
        }
    ));
    out
}
