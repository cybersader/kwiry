//! `kwiry selfcheck` against the same governed lab the live check serves.
use super::*;

const ORDER: [&str; 9] = [
    "config",
    "governed_profile",
    "loopback_bind",
    "snapshot_ids",
    "room_roots",
    "data_dir_writable",
    "openat2_probe",
    "mount_separation",
    "jwks_readable",
];

fn selfcheck(lab: &Lab, config: &Path, data: &Path) -> (i32, Value, String) {
    let output = Command::new(cargo_bin("kwiry"))
        .arg("--config")
        .arg(config)
        .arg("--data-dir")
        .arg(data)
        .args(["selfcheck", "--json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let root = lab._root.path().to_str().unwrap();
    for text in [&stdout, &stderr] {
        assert!(!text.contains(root), "a path leaked: {text}");
        for label in [LABEL_A, LABEL_B] {
            assert!(!text.contains(label), "a label leaked: {text}");
        }
    }
    let summary: Value = serde_json::from_str(stdout.trim()).unwrap();
    let names: Vec<&str> = summary["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ORDER);
    assert_eq!(stderr.lines().count(), ORDER.len() + 1, "{stderr}");
    (output.status.code().unwrap(), summary, stderr)
}

fn not_passing(summary: &Value) -> Vec<(String, String)> {
    summary["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] != "pass")
        .map(|c| {
            (
                c["name"].as_str().unwrap().to_owned(),
                c["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(n, s)| ((*n).to_owned(), (*s).to_owned()))
        .collect()
}

#[test]
fn governed_selfcheck_reports_each_host_fact_without_serving() {
    let mut lab = Lab::new();
    lab.stop();
    let listing = |dir: &Path| {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before = listing(&lab.data);

    let (code, summary, stderr) = selfcheck(&lab, &lab.path, &lab.data);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["schema_version"], 1);
    assert!(not_passing(&summary).is_empty());
    assert!(stderr.ends_with("selfcheck: ok\n"));
    // Read-only: the write probe leaves nothing behind.
    assert_eq!(before, listing(&lab.data));

    // A generic label is valid config but cannot answer a live check.
    lab.config.vaults[0].snapshot_id = Some("generic-synthetic".parse().unwrap());
    save_config(&lab.path, &lab.config).unwrap();
    let (code, summary, _) = selfcheck(&lab, &lab.path, &lab.data);
    assert_eq!(code, 1);
    assert_eq!(summary["ok"], false);
    assert_eq!(not_passing(&summary), pairs(&[("snapshot_ids", "fail")]));
    lab.config.vaults[0].snapshot_id = Some(LABEL_A.parse().unwrap());
    save_config(&lab.path, &lab.config).unwrap();

    // Hand-edited non-loopback bind: the document parses but fails validation.
    let text = fs::read_to_string(&lab.path).unwrap();
    assert!(text.contains("bind = \"127.0.0.1:"), "{text}");
    let open = lab.path.with_file_name("open-bind.toml");
    fs::write(
        &open,
        text.replace("bind = \"127.0.0.1:", "bind = \"0.0.0.0:"),
    )
    .unwrap();
    let (code, summary, _) = selfcheck(&lab, &open, &lab.data);
    assert_eq!(code, 1);
    assert_eq!(
        not_passing(&summary),
        pairs(&[("config", "fail"), ("loopback_bind", "fail")])
    );

    // A key set the verifier would refuse at startup.
    let jwks = lab
        .config
        .auth
        .openclast
        .as_ref()
        .unwrap()
        .jwks_file
        .clone();
    let keys = fs::read(&jwks).unwrap();
    fs::write(&jwks, b"{\"keys\":[]}").unwrap();
    let (code, summary, _) = selfcheck(&lab, &lab.path, &lab.data);
    assert_eq!(code, 1);
    assert_eq!(not_passing(&summary), pairs(&[("jwks_readable", "fail")]));
    fs::write(&jwks, keys).unwrap();

    // A missing data directory is reported, never created.
    let missing = lab._root.path().join("absent-data");
    let (code, summary, _) = selfcheck(&lab, &lab.path, &missing);
    assert_eq!(code, 1);
    assert_eq!(
        not_passing(&summary),
        pairs(&[
            ("data_dir_writable", "fail"),
            ("openat2_probe", "skipped"),
            ("mount_separation", "skipped"),
        ])
    );
    assert!(!missing.exists());

    // No config file: nothing else can be judged.
    let (code, summary, _) = selfcheck(&lab, &lab.path.with_file_name("absent.toml"), &lab.data);
    assert_eq!(code, 1);
    assert_eq!(summary["checks"][0]["status"], "fail");
    assert!(
        summary["checks"].as_array().unwrap()[1..]
            .iter()
            .all(|c| c["status"] == "skipped")
    );
}
