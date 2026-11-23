use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use kwiry_core::SearchHit;
use tempfile::tempdir;

fn fixture_vault() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/vault")
        .canonicalize()
        .unwrap()
}

#[test]
fn lifecycle_indexes_and_searches_fixture() {
    let temporary = tempdir().unwrap();
    let config = temporary.path().join("config.toml");
    let data = temporary.path().join("data");
    let fixture = fixture_vault();

    cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "vault",
            "add",
            "--id",
            "fixture",
            "--path",
            fixture.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("Registered vault 'fixture'"));

    cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "index",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("Indexed"));

    let first = search_json(&config, &data, "phosphorescent", None);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].path, "welcome.md");
    assert!(
        first[0]
            .heading_path
            .ends_with(&["Vocabulary mismatch".into()])
    );

    let hidden = search_json(&config, &data, "must-not-be-indexed", None);
    assert!(hidden.is_empty());

    fs::remove_dir_all(&data).unwrap();
    cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "index",
        ])
        .assert()
        .success();
    let rebuilt = search_json(&config, &data, "phosphorescent", Some("fixture"));
    assert_eq!(first, rebuilt);
}

#[test]
fn setup_dry_run_json_is_versioned_and_mutation_free() {
    let temporary = tempdir().unwrap();
    let config = temporary.path().join("config.toml");
    let data = temporary.path().join("data");
    let vault = temporary.path().join("Project Notes Ω");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("welcome.md"), "# Welcome\n\nhello").unwrap();

    let output = cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "setup",
            vault.to_str().unwrap(),
            "--id",
            "project-notes",
            "--no-semantic",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["ok"], true);
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["vault_id"], "project-notes");
    assert_eq!(report["semantic_enabled"], false);
    assert!(!config.exists());
    assert!(!data.exists());
}

#[test]
fn service_install_dry_run_json_is_mutation_free() {
    let temporary = tempdir().unwrap();
    let config = temporary.path().join("config.toml");
    let data = temporary.path().join("data");

    let output = cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "service",
            "install",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["command"], "install");
    assert_eq!(report["ok"], true);
    assert_eq!(report["dry_run"], true);
    assert!(!config.exists());
    assert!(!data.exists());
}

#[test]
fn setup_json_missing_input_returns_stable_error_document() {
    let temporary = tempdir().unwrap();
    let config = temporary.path().join("config.toml");
    let data = temporary.path().join("data");

    let output = cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "setup",
            "--dry-run",
            "--json",
        ])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["ok"], false);
    assert_eq!(report["issues"][0]["code"], "prompt_required");
    assert!(!config.exists());
    assert!(!data.exists());
}

#[test]
fn config_set_indexing_round_trips_and_rejects_invalid_values() {
    let temporary = tempdir().unwrap();
    let config = temporary.path().join("config.toml");
    let data = temporary.path().join("data");

    cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "config",
            "set-indexing",
            "--basis",
            "metadata-audit",
            "--audit-sources",
            "8",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("basis=metadata_audit"));

    let shown = cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "config",
            "show",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let effective: serde_json::Value = serde_json::from_slice(&shown).unwrap();
    assert_eq!(effective["indexing"]["basis"], "metadata_audit");
    assert_eq!(effective["indexing"]["audit_sources_per_pass"], 8);

    cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "config",
            "set-indexing",
            "--audit-sources",
            "999",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "audit_sources_per_pass must be between 1 and 256",
        ));

    // The rejected update must not have modified the stored configuration.
    let unchanged = cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "config",
            "show",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let effective: serde_json::Value = serde_json::from_slice(&unchanged).unwrap();
    assert_eq!(effective["indexing"]["audit_sources_per_pass"], 8);

    cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "config",
            "set-indexing",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("nothing to change"));
}

fn search_json(config: &Path, data: &Path, query: &str, vault: Option<&str>) -> Vec<SearchHit> {
    let mut command = cargo_bin_cmd!("kwiry");
    command.args([
        "--config",
        config.to_str().unwrap(),
        "--data-dir",
        data.to_str().unwrap(),
        "search",
        query,
        "--json",
    ]);
    if let Some(vault) = vault {
        command.args(["--vault", vault]);
    }
    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).unwrap()
}

// checkroom refuses with exit 2 off Linux, as governed ingestion does.
#[cfg(target_os = "linux")]
#[test]
fn checkroom_reports_offenders_without_content_and_uses_stable_exit_codes() {
    use predicates::prelude::PredicateBooleanExt;

    let temporary = tempdir().unwrap();
    let room = temporary.path().join("room");
    fs::create_dir(&room).unwrap();
    fs::write(room.join("clean.md"), "# Clean\nsynthetic body\n").unwrap();
    let checkroom = |args: &[&str]| {
        let mut command = cargo_bin_cmd!("kwiry");
        command.arg("checkroom").args(args);
        command.assert()
    };
    let room_arg = room.to_str().unwrap();

    let clean = checkroom(&[room_arg, "--json"]).code(0);
    let json: serde_json::Value = serde_json::from_slice(&clean.get_output().stdout).unwrap();
    assert_eq!(json["would_label"], true);
    assert_eq!(json["files_checked"], 1);
    assert_eq!(json["offenders"], serde_json::json!([]));
    assert_eq!(json["walk_defects"], serde_json::json!([]));
    assert_eq!(json["limits"]["max_sections"], 100_000);
    assert_eq!(json["engine"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        json["engine"]["extraction_policy_fingerprint"],
        kwiry_core::extraction_policy_fingerprint()
    );
    checkroom(&[room_arg])
        .code(0)
        .stdout(predicates::str::contains("room would be labelled"));

    fs::write(
        room.join("broken.md"),
        "---\ntitle: [synthetic-marker-zebra\n---\nsynthetic-marker-zebra\n",
    )
    .unwrap();
    fs::write(room.join("notes.txt"), "synthetic-marker-zebra").unwrap();
    let withheld = checkroom(&[room_arg, "--json"]).code(1);
    let output = withheld.get_output();
    assert!(!String::from_utf8_lossy(&output.stdout).contains("zebra"));
    assert!(output.stderr.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["would_label"], false);
    assert_eq!(json["files_checked"], 3);
    assert_eq!(
        json["offenders"],
        serde_json::json!([
            {"path": "broken.md", "reason": "invalid_frontmatter", "class": "content", "detail": "frontmatter is not valid YAML"},
            {"path": "notes.txt", "reason": "non_markdown_format", "class": "structural", "detail": "only .md Markdown sources may be present in a labelled room"},
        ])
    );
    checkroom(&[room_arg])
        .code(1)
        .stdout(predicates::str::contains(
            "offender broken.md: invalid_frontmatter [content]",
        ))
        .stdout(predicates::str::contains("zebra").not());

    // Usage and setup errors.
    let missing = temporary.path().join("missing");
    checkroom(&[missing.to_str().unwrap()]).code(2);
    // A non-canonical spelling of a real room is a usage error, not a verdict.
    checkroom(&[&format!("{room_arg}/")])
        .code(2)
        .stderr(predicates::str::contains("room path must be canonical"));
    checkroom(&[&format!("{room_arg}/../room")])
        .code(2)
        .stderr(predicates::str::contains("room path must be canonical"));
    // A canonically spelled path through a symbolic link is the daemon's
    // refusal, reported as a structural verdict.
    let alias = temporary.path().join("alias");
    std::os::unix::fs::symlink(&room, &alias).unwrap();
    checkroom(&[alias.to_str().unwrap()])
        .code(1)
        .stdout(predicates::str::contains(
            "walk_defect .: room_boundary [structural]",
        ));
    checkroom(&[]).code(2);
    let config = temporary.path().join("config.toml");
    checkroom(&[
        "--config",
        config.to_str().unwrap(),
        "--vault",
        "nope",
        room_arg,
    ])
    .code(2);

    // With a config, the named registration is re-pointed at the room and the
    // other registrations are still checked for overlap.
    let registered = temporary.path().join("registered");
    fs::create_dir(&registered).unwrap();
    cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            temporary.path().join("data").to_str().unwrap(),
            "vault",
            "add",
            "--id",
            "notes",
            "--path",
            registered.to_str().unwrap(),
        ])
        .assert()
        .success();
    fs::remove_file(room.join("broken.md")).unwrap();
    fs::remove_file(room.join("notes.txt")).unwrap();
    let args = |vault: &str, path: &str| {
        let data = temporary.path().join("data");
        let mut command = cargo_bin_cmd!("kwiry");
        command.args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "checkroom",
            "--vault",
            vault,
            path,
        ]);
        command.assert()
    };
    args("notes", room_arg).code(0);
    assert!(!temporary.path().join("data").exists());
    let inside = registered.join("inside");
    fs::create_dir(&inside).unwrap();
    cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "vault",
            "add",
            "--id",
            "other",
            "--path",
            room_arg,
        ])
        .assert()
        .success();
    args("other", inside.to_str().unwrap())
        .code(1)
        .stdout(predicates::str::contains("walk_defect .: room_boundary"));
}

// checkroom refuses with exit 2 off Linux, as governed ingestion does.
#[cfg(target_os = "linux")]
#[test]
fn checkroom_and_selfcheck_report_the_same_engine() {
    let temporary = tempdir().unwrap();
    let config = temporary.path().join("config.toml");
    let data = temporary.path().join("data");
    let room = temporary.path().join("room");
    fs::create_dir(&room).unwrap();
    cargo_bin_cmd!("kwiry")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "vault",
            "add",
            "--id",
            "notes",
            "--path",
            room.to_str().unwrap(),
        ])
        .assert()
        .success();
    let engine = |args: &[&str]| {
        let output = cargo_bin_cmd!("kwiry")
            .args(["--config", config.to_str().unwrap()])
            .args(["--data-dir", data.to_str().unwrap()])
            .args(args)
            .output()
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        json["engine"].clone()
    };
    // Selfcheck fails on this desktop config but still reports its engine.
    let server = engine(&["selfcheck", "--json"]);
    let checker = engine(&[
        "checkroom",
        "--vault",
        "notes",
        room.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(server, checker);
    assert_eq!(server["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        server["extraction_policy_fingerprint"],
        kwiry_core::extraction_policy_fingerprint()
    );
    let build = server["build"].as_str().unwrap();
    assert!(
        build.ends_with("/debug") || build.ends_with("/release"),
        "{build}"
    );
    match &server["commit"] {
        serde_json::Value::Null => {}
        serde_json::Value::String(commit) => {
            assert_eq!(commit.len(), 40);
            assert!(commit.bytes().all(|b| b.is_ascii_hexdigit()));
        }
        other => panic!("unexpected commit {other}"),
    }
}
