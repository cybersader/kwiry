use super::*;

fn assert_label(reply: &Value, label: &str, empty: bool) {
    assert_eq!(reply["status"], 200, "{reply}");
    assert_eq!(reply["body"]["resource_snapshots"][0]["snapshot_id"], label);
    assert_eq!(reply["body"]["hits"].as_array().unwrap().is_empty(), empty);
}

#[test]
fn governed_search_live_check_publication_and_restart() {
    let mut lab = Lab::new();
    let mut c = Capture::new();
    // Non-required instances must reject even a valid tuple/label *at load*.
    let config_text = fs::read_to_string(&lab.path).unwrap();
    let rejected = lab.path.with_file_name("ordinary.toml");
    fs::write(
        &rejected,
        config_text.replace("profile_required = true", "profile_required = false"),
    )
    .unwrap();
    let output = Command::new(cargo_bin("kwiry"))
        .arg("--config")
        .arg(&rejected)
        .arg("--data-dir")
        .arg(lab._root.path().join("ordinary-data"))
        .args(["serve", "--bind", "127.0.0.1:0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("snapshot_id requires a profile-required governed room registration"));
    assert!(!stderr.contains(LABEL_A));
    let refusal = json!({"exit_code":output.status.code(),"stderr":stderr.replace(rejected.to_str().unwrap(),"<config>").trim()});
    config_fixture("config-refusal", refusal);
    let second = lab._root.path().join("edition-b");
    fs::create_dir(&second).unwrap();
    sealed_note(&second);
    lab.config.vaults[0].path = second;
    lab.config.vaults[0].snapshot_id = Some(LABEL_B.parse().unwrap());
    save_config(&lab.path, &lab.config).unwrap();
    wait_for(|| lab.query("moonflower")["body"]["resource_snapshots"][0]["snapshot_id"] == LABEL_B);
    for (q, empty) in [("moonflower", false), ("zzzzzzzz", true)] {
        let reply = lab.query(q);
        assert_label(&reply, LABEL_B, empty);
        c.check(
            if empty {
                "identical-b-empty-answer"
            } else {
                "identical-b-hits"
            },
            reply,
            200,
            "",
            true,
        );
    }
    lab.stop();
    lab.start();
    let reply = lab.query("moonflower");
    assert_label(&reply, LABEL_B, false);
    c.check("restarted-b", reply, 200, "", true);
    for (dir, label, case) in [
        ("empty-a", LABEL_A, "empty-a"),
        ("empty-b", LABEL_B, "empty-b"),
    ] {
        let root = lab._root.path().join(dir);
        fs::create_dir(&root).unwrap();
        lab.config.vaults[0].path = root;
        lab.config.vaults[0].snapshot_id = Some(label.parse().unwrap());
        save_config(&lab.path, &lab.config).unwrap();
        wait_for(|| {
            lab.query("moonflower")["body"]["resource_snapshots"][0]["snapshot_id"] == label
        });
        let reply = lab.query("moonflower");
        assert_label(&reply, label, true);
        c.check(case, reply, 200, "", true);
    }
    let active = kwiry_core::DataRoot::new(&lab.data)
        .active()
        .unwrap()
        .unwrap();
    lab.stop();
    let layout: Value = serde_json::from_slice(&fs::read(&active.layout_path).unwrap()).unwrap();
    assert_eq!(layout["layout_version"], 3);
    fs::remove_file(active.root.join("snapshot-labels.json")).unwrap();
    lab.config.vaults[0].snapshot_id = None;
    lab.start();
    let reply = lab.query("moonflower");
    assert_eq!(reply["body"]["error"]["resource_indexes"], json!([0]));
    c.check(
        "restarted-legacy-v3",
        reply,
        503,
        "resource_unavailable",
        true,
    );
    for (label, case) in [(Some("A".repeat(64)), "generic-label"), (None, "unlabeled")] {
        lab.stop();
        lab.config.vaults[0].snapshot_id = label.map(|s| s.parse().unwrap());
        lab.start();
        let reply = lab.query("zzzzzzzz");
        assert_eq!(reply["body"]["error"]["resource_indexes"], json!([0]));
        c.check(case, reply, 503, "resource_unavailable", true);
    }
    // Even an irrelevant known non-pilot format prevents a labeled empty answer.
    fs::write(lab.config.vaults[0].path.join("defect.txt"), "unrelated").unwrap();
    lab.stop();
    lab.config.vaults[0].snapshot_id = Some(LABEL_A.parse().unwrap());
    lab.start();
    c.check(
        "non-markdown-candidate",
        lab.query("zzzzzzzz"),
        503,
        "resource_unavailable",
        true,
    );
    lab.stop();
    c.save("publication");
}

#[test]
fn governed_search_live_check_stored_fields() {
    use tantivy::{Index, TantivyDocument, schema::Document};
    let mut lab = Lab::new();
    let mut c = Capture::new();
    let active = kwiry_core::DataRoot::new(&lab.data)
        .active()
        .unwrap()
        .unwrap();
    let path = fs::read_dir(active.partitions_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("index");
    let index = Index::open_in_dir(path).unwrap();
    let schema = index.schema();
    let reader = index.reader().unwrap();
    let searcher = reader.searcher();
    let source: TantivyDocument = searcher.doc(tantivy::DocAddress::new(0, 0)).unwrap();
    let original: Value = serde_json::from_str(&source.to_json(&schema)).unwrap();
    assert_eq!(original["content_hash"][0], HASH);
    // Independent synthetic storage-fault fixture. Let the daemon open its lazy
    // partition reader only after the edit; no signing, verifier or HTTP test hook.
    let mut writer = index
        .writer_with_num_threads::<TantivyDocument>(1, 20_000_000)
        .unwrap();
    for (ordinal, (q, key, value)) in [
        ("quince", "content_hash", None),
        ("kumquat", "content_hash", Some(json!(["malformed"]))),
        (
            "clover",
            "extraction_coverage",
            Some(json!(["\"indexed-partial\""])),
        ),
        ("sorrel", "source_format", Some(json!(["\"text\""]))),
        (
            "juniper",
            "source_locator",
            Some(json!(["{\"kind\":\"pdf_page\",\"page\":1}"])),
        ),
        ("heather", "content_hash", Some(json!([LABEL_B]))),
    ]
    .into_iter()
    .enumerate()
    {
        let mut doc = original.clone();
        doc["chunk_id"] = json!([format!("{ordinal:064x}")]);
        doc["content"] = json!([format!("{q} moonflower")]);
        match value {
            Some(v) => doc[key] = v,
            None => {
                doc.as_object_mut().unwrap().remove(key);
            }
        }
        writer
            .add_document(TantivyDocument::parse_json(&schema, &doc.to_string()).unwrap())
            .unwrap();
    }
    writer.commit().unwrap();
    writer.wait_merging_threads().unwrap();
    for (case, q) in [
        ("missing-hash", "quince"),
        ("malformed-hash", "kumquat"),
        ("impossible-coverage", "clover"),
        ("impossible-format", "sorrel"),
        ("impossible-locator", "juniper"),
        ("no-partial-page", "moonflower"),
    ] {
        c.check(case, lab.query(q), 500, "internal_error", true);
    }
    let wrong = lab.query("heather");
    assert_eq!(wrong["body"]["hits"][0]["content_sha256"], LABEL_B);
    c.check(
        "well-formed-wrong-hash-needs-platform-verification",
        wrong,
        200,
        "",
        true,
    );
    c.save("stored-fields");
    lab.stop();
}

fn config_fixture(name: &str, actual: Value) {
    let path = Path::new(WIRE)
        .join("fixtures")
        .join(format!("{name}.json"));
    if std::env::var("KWIRY_RECORD_LIVE_CHECK").as_deref() == Ok("1") {
        fs::write(path, format!("{actual:#}\n")).unwrap();
    } else {
        let expected: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(actual, expected);
    }
}
