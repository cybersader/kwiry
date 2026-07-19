use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use kwir_core::SearchHit;
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

    cargo_bin_cmd!("kwir")
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

    cargo_bin_cmd!("kwir")
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
    cargo_bin_cmd!("kwir")
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

fn search_json(config: &Path, data: &Path, query: &str, vault: Option<&str>) -> Vec<SearchHit> {
    let mut command = cargo_bin_cmd!("kwir");
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
