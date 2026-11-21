use super::*;

struct Fixture {
    dir: tempfile::TempDir,
    config: Config,
    runtime: SearchRuntime,
    manager: IndexManager,
    resources: [ResourceKey; 1],
}

impl Fixture {
    fn new(note: Option<&str>) -> Self {
        Self::with_label(note, Some("snapshot:A"))
    }

    fn with_label(note: Option<&str>, label: Option<&str>) -> Self {
        let dir = tempdir().unwrap();
        let room = dir.path().join("first");
        fs::create_dir(&room).unwrap();
        if let Some(text) = note {
            fs::write(room.join("note.md"), text).unwrap();
        }
        let mut config = openclast_config(vec![VaultRegistration {
            snapshot_id: label.map(|s| s.parse().unwrap()),
            id: "orchard".into(),
            path: room,
            room: Some("garden".into()),
        }]);
        config.indexing.max_stale_serve_seconds = 60;
        let data = dir.path().join("data");
        build_index(&config, &data).unwrap();
        let runtime = SearchRuntime::new();
        let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
        manager.reconcile(config.clone()).unwrap();
        let resources = [config.resource_key(&config.vaults[0]).unwrap()];
        Self {
            dir,
            config,
            runtime,
            manager,
            resources,
        }
    }

    fn replace_root(&mut self, note: Option<&str>) {
        let root = self.dir.path().join("second");
        fs::create_dir(&root).unwrap();
        if let Some(text) = note {
            fs::write(root.join("note.md"), text).unwrap();
        }
        self.config.vaults[0].path = root;
        self.config.vaults[0].snapshot_id = Some("snapshot:B".parse().unwrap());
        self.runtime.set_openclast_registrations(&self.config);
    }

    fn published(&self) -> crate::GenerationPaths {
        DataRoot::new(self.dir.path().join("data"))
            .active()
            .unwrap()
            .unwrap()
    }

    fn reconcile(&mut self) {
        self.manager.reconcile(self.config.clone()).unwrap();
    }

    fn search(&self, query: &str) -> GenerationSearchResult {
        self.runtime
            .search_authorized_with_generation(
                query,
                10,
                &SearchFilters::default(),
                &self.resources,
            )
            .unwrap()
    }

    fn check(&self, result: &GenerationSearchResult, label: Option<&str>, hits: usize) {
        assert_eq!(result.hits.len(), hits);
        assert_eq!(result.snapshot_ids.len(), 1);
        assert_eq!(
            result.snapshot_ids[&self.resources[0]]
                .as_ref()
                .map(SnapshotId::as_str),
            label
        );
        assert_eq!(result.require_snapshot_ids().is_ok(), label.is_some());
    }
}

#[test]
fn nonempty_root_switch_reports_old_label_until_new_reader_is_installed() {
    let mut f = Fixture::new(Some("apricots"));
    f.replace_root(Some("violets"));
    f.check(&f.search("apricots"), Some("snapshot:A"), 1);
    f.reconcile();
    f.check(&f.search("violets"), Some("snapshot:B"), 1);
    f.check(&f.search("apricots"), Some("snapshot:B"), 0);
}

#[test]
fn byte_identical_roots_publish_new_snapshot_label() {
    let mut f = Fixture::new(Some("apricots"));
    let old = f.published();
    f.replace_root(Some("apricots"));
    f.reconcile();
    assert_ne!(old.id, f.published().id);
    f.check(&f.search("apricots"), Some("snapshot:B"), 1);
}

#[test]
fn label_transition_hashes_new_bytes_even_when_metadata_is_identical() {
    let mut f = Fixture::new(Some("apricots"));
    f.config.indexing.basis = IndexFreshnessBasis::MetadataAudit;
    f.config.indexing.racy_window_millis = 0;
    let note = f.config.vaults[0].path.join("note.md");
    let old_time = FileTime::from_last_modification_time(&fs::metadata(&note).unwrap());
    fs::write(&note, "seedling").unwrap();
    set_file_mtime(&note, old_time).unwrap();
    f.config.vaults[0].snapshot_id = Some("snapshot:B".parse().unwrap());
    // No rolling audit on a scoped pass, and no changed-path hint for the note.
    f.manager
        .reconcile_scoped(
            f.config.clone(),
            &ReconcileScope::RoomPaths(Default::default()),
        )
        .unwrap();
    f.check(&f.search("seedling"), Some("snapshot:B"), 1);
    f.check(&f.search("apricots"), Some("snapshot:B"), 0);
}

#[test]
fn two_empty_roots_publish_new_snapshot_label() {
    let mut f = Fixture::new(None);
    let old = f.published();
    f.check(&f.search("apricots"), Some("snapshot:A"), 0);
    f.replace_root(None);
    f.reconcile();
    assert_ne!(old.id, f.published().id);
    f.check(&f.search("apricots"), Some("snapshot:B"), 0);
}

#[test]
fn stalled_reader_returns_its_own_snapshot_label_on_zero_hits_after_switch() {
    let mut f = Fixture::new(Some("apricots"));
    let resources = f.resources.clone();
    let filters = SearchFilters::default();
    let (reader, opened) = f
        .runtime
        .open_authorized_snapshot(&filters, &resources)
        .unwrap();
    f.replace_root(Some("violets"));
    f.reconcile();
    f.check(&f.search("violets"), Some("snapshot:B"), 1);
    let stalled = reader
        .search_opened(&opened, "violets", 10, &filters, &resources)
        .unwrap();
    f.check(&stalled, Some("snapshot:A"), 0);
}

#[test]
fn same_path_label_only_change_and_label_removal_each_publish() {
    let mut f = Fixture::new(Some("apricots"));
    let mut old = f.published();
    for label in [Some("snapshot:B"), None] {
        f.config.vaults[0].snapshot_id = label.map(|s| s.parse().unwrap());
        f.reconcile();
        let next = f.published();
        assert_ne!(old.id, next.id);
        old = next;
        f.check(&f.search("apricots"), label, 1);
    }
}

fn remove_label_sidecar(active: &crate::GenerationPaths) {
    fs::remove_file(active.root.join(crate::partition::LABEL_SIDECAR)).unwrap();
}

fn reopened_result(
    active: &crate::GenerationPaths,
    resources: &[ResourceKey],
    query: &str,
) -> GenerationSearchResult {
    let reader = PartitionedSearchIndex::open(active).unwrap();
    let filters = SearchFilters::default();
    let opened = reader
        .open_resources(&filters, resources, |_| true)
        .unwrap();
    reader
        .search_opened(&opened, query, 10, &filters, resources)
        .unwrap()
}

#[test]
fn legacy_unlabeled_deployment_serves_after_boot_without_rebuilding_or_republishing() {
    let f = Fixture::new(Some("apricots"));
    let active = f.published();
    f.manager.shutdown().unwrap();
    remove_label_sidecar(&active);
    let mut config = f.config;
    config.vaults[0].snapshot_id = None;
    let rt = SearchRuntime::new();
    let data = f.dir.path().join("data");
    let mut manager = IndexManager::open(config.clone(), &data, rt.clone()).unwrap();
    manager.reconcile(config).unwrap();
    assert_eq!(DataRoot::new(data).active().unwrap().unwrap().id, active.id);
    let served = rt
        .search_authorized_with_generation("apricots", 10, &SearchFilters::default(), &f.resources)
        .unwrap();
    assert_eq!(served.hits.len(), 1);
    assert!(served.hits[0].require_content_sha256().is_ok());
    assert!(served.require_snapshot_ids().is_err());
}

fn restart_with_different_config(legacy: bool) {
    let f = Fixture::new(Some("apricots"));
    let active = f.published();
    f.manager.shutdown().unwrap();
    if legacy {
        remove_label_sidecar(&active);
    }
    let mut config = f.config;
    config.vaults[0].snapshot_id = Some("snapshot:B".parse().unwrap());
    let runtime = SearchRuntime::new();
    let data = f.dir.path().join("data");
    let mut manager = IndexManager::open(config.clone(), &data, runtime.clone()).unwrap();
    let pinned = runtime.active.load_full().unwrap();
    let ActiveSearchIndex::OpenClast(reader) = pinned.as_ref() else {
        panic!()
    };
    assert_eq!(reader.generation, active.id);
    // Boot normally withholds service pending its room walk. Inspect that exact
    // reopened reader without granting the public search path any fresh evidence.
    let filters = SearchFilters::default();
    let opened = reader
        .open_resources(&filters, &f.resources, |_| true)
        .unwrap();
    let old = reader
        .search_opened(&opened, "apricots", 10, &filters, &f.resources)
        .unwrap();
    assert_eq!(old.hits.len(), 1);
    assert_eq!(
        old.snapshot_ids[&f.resources[0]]
            .as_ref()
            .map(SnapshotId::as_str),
        (!legacy).then_some("snapshot:A")
    );
    assert_eq!(old.require_snapshot_ids().is_err(), legacy);
    manager.reconcile(config).unwrap();
    assert_ne!(DataRoot::new(data).active().unwrap().unwrap().id, active.id);
    let live = runtime
        .search_authorized_with_generation("apricots", 10, &filters, &f.resources)
        .unwrap();
    assert_eq!(
        live.snapshot_ids[&f.resources[0]]
            .as_ref()
            .unwrap()
            .as_str(),
        "snapshot:B"
    );
}

#[test]
fn restart_uses_persisted_snapshot_label_until_boot_publication() {
    restart_with_different_config(false);
}

#[test]
fn legacy_unlabeled_reader_fails_closed_after_restart_until_publication() {
    restart_with_different_config(true);
}

#[test]
fn incomplete_walk_retaining_old_rows_cannot_acquire_new_label() {
    let mut f = Fixture::new(Some("apricots"));
    let room = f.config.vaults[0].path.clone();
    fs::remove_file(room.join("note.md")).unwrap();
    fs::write(room.join("new.md"), "violets").unwrap();
    f.config.vaults[0].snapshot_id = Some("snapshot:B".parse().unwrap());
    crate::room_walk::with_interrupted_listing(room, || f.reconcile());
    f.check(&f.search("apricots"), None, 1);
    f.reconcile();
    f.check(&f.search("apricots"), Some("snapshot:B"), 0);
}

#[test]
fn zero_hit_labels_cover_only_participating_resources_and_require_all_of_them() {
    let mut f = Fixture::new(Some("apricots"));
    let other = f.dir.path().join("other");
    fs::create_dir(&other).unwrap();
    f.config.vaults.push(VaultRegistration {
        id: "second".into(),
        path: other,
        room: Some("other".into()),
        snapshot_id: None,
    });
    let second = f.config.resource_key(&f.config.vaults[1]).unwrap();
    let both = [f.resources[0].clone(), second.clone()];
    f.runtime.set_openclast_registrations(&f.config);
    f.reconcile();
    let all = f
        .runtime
        .search_authorized_with_generation("violets", 10, &SearchFilters::default(), &both)
        .unwrap();
    assert!(all.hits.is_empty());
    assert_eq!(all.snapshot_ids.len(), 2);
    assert_eq!(all.snapshot_ids[&second], None);
    assert!(all.require_snapshot_ids().is_err());
    let filters = SearchFilters {
        vault_id: Some(f.resources[0].vault_id.clone()),
        ..SearchFilters::default()
    };
    let one = f
        .runtime
        .search_authorized_with_generation("violets", 10, &filters, &both)
        .unwrap();
    f.check(&one, Some("snapshot:A"), 0);
}

#[test]
fn partial_initial_build_cannot_claim_the_registered_label() {
    let f = Fixture::new(Some("apricots"));
    let data = f.dir.path().join("fresh-data");
    crate::room_walk::with_interrupted_listing(f.config.vaults[0].path.clone(), || {
        build_index(&f.config, &data)
    })
    .unwrap();
    let active = DataRoot::new(data).active().unwrap().unwrap();
    let result = reopened_result(&active, &f.resources, "apricots");
    f.check(&result, None, 1);
}

#[test]
fn failed_root_cannot_relabel_the_serving_reader() {
    let mut f = Fixture::new(Some("apricots"));
    fs::rename(&f.config.vaults[0].path, f.dir.path().join("offline")).unwrap();
    f.config.vaults[0].snapshot_id = Some("snapshot:B".parse().unwrap());
    f.reconcile();
    f.check(&f.search("apricots"), Some("snapshot:A"), 1);
}

#[test]
fn unreadable_note_under_unchanged_label_publishes_unlabeled() {
    let mut f = Fixture::new(Some("apricots"));
    let old = f.published();
    crate::room_walk::with_unreadable_source("note.md", || f.reconcile());
    assert_ne!(old.id, f.published().id);
    f.check(&f.search("apricots"), None, 0);
    f.reconcile();
    f.check(&f.search("apricots"), Some("snapshot:A"), 1);
}

#[test]
fn indexed_partial_source_disqualifies_initial_build_and_replacement() {
    let mut f = Fixture::new(Some("apricots"));
    fs::write(
        f.config.vaults[0].path.join("board.canvas"),
        include_bytes!("../../../tests/fixtures/canvas/partial.canvas"),
    )
    .unwrap();
    f.reconcile();
    f.check(&f.search("apricots"), None, 1);
    let data = f.dir.path().join("fresh");
    build_index(&f.config, &data).unwrap();
    let active = DataRoot::new(data).active().unwrap().unwrap();
    f.check(&reopened_result(&active, &f.resources, "apricots"), None, 1);
}

#[test]
fn boot_time_eviction_clears_label_until_a_clean_pass() {
    let f = Fixture::new(Some("apricots"));
    let data = f.dir.path().join("data");
    f.manager.shutdown().unwrap();
    assert_eq!(forge_foreign_identity(&data, SourceFormat::Markdown), 1);
    let runtime = SearchRuntime::new();
    let mut manager = IndexManager::open(f.config.clone(), &data, runtime.clone()).unwrap();
    assert_eq!(manager.open_evictions().total(), 1);
    let before = reopened_result(
        &DataRoot::new(&data).active().unwrap().unwrap(),
        &f.resources,
        "apricots",
    );
    assert!(before.hits.is_empty());
    assert!(before.require_snapshot_ids().is_err());
    manager.reconcile(f.config).unwrap();
    let after = runtime
        .search_authorized_with_generation("apricots", 10, &SearchFilters::default(), &f.resources)
        .unwrap();
    assert_eq!(after.hits.len(), 1);
    assert_eq!(
        after.snapshot_ids[&f.resources[0]]
            .as_ref()
            .unwrap()
            .as_str(),
        "snapshot:A"
    );
}

fn restore_withheld_root(previous: Option<&str>, next: Option<&str>) {
    let mut f = Fixture::with_label(Some("apricots"), previous);
    let original = f.config.vaults[0].path.clone();
    f.config.vaults[0].path = f.dir.path().join("offline");
    f.reconcile();
    assert!(!partition_index_dir(&f.published().partitions_dir, &f.resources[0]).exists());
    assert_eq!(f.manager.manifest().files.len(), 1);
    f.config.vaults[0].path = original;
    f.config.vaults[0].snapshot_id = next.map(|s| s.parse().unwrap());
    f.reconcile();
    f.check(&f.search("apricots"), next, 1);
}

#[test]
fn returning_to_withheld_root_with_new_label_returns_note_and_label() {
    restore_withheld_root(Some("snapshot:A"), Some("snapshot:C"));
}

#[test]
fn returning_to_withheld_root_without_label_returns_note() {
    restore_withheld_root(None, None);
}

#[test]
fn matching_manifest_with_one_missing_chunk_is_reingested_before_label_publication() {
    let mut f = Fixture::new(Some("# First\napricots\n# Second\napricots\n"));
    let both = f.search("apricots");
    f.check(&both, Some("snapshot:A"), 2);
    let index_dir = partition_index_dir(&f.published().partitions_dir, &f.resources[0]);
    let (index, fields) = open_index_dir(&index_dir).unwrap();
    let mut writer: IndexWriter<tantivy::TantivyDocument> =
        index.writer(WRITER_MEMORY_BYTES).unwrap();
    writer.delete_term(Term::from_field_text(
        fields.chunk_id,
        &both.hits[0].chunk_id,
    ));
    writer.commit().unwrap();
    writer.wait_merging_threads().unwrap();
    assert_eq!(
        partition_source_counts(&index_dir)
            .unwrap()
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [1]
    );
    f.config.vaults[0].snapshot_id = Some("snapshot:C".parse().unwrap());
    f.reconcile();
    f.check(&f.search("apricots"), Some("snapshot:C"), 2);
}

#[test]
fn physical_count_mismatch_with_clean_walk_is_unlabeled_and_repaired_on_retry() {
    let mut f = Fixture::new(Some("apricots"));
    let index_dir = partition_index_dir(&f.published().partitions_dir, &f.resources[0]);
    let (index, fields) = open_index_dir(&index_dir).unwrap();
    // Inherited metadata and physical state agree on two chunks; the clean
    // walk produces one. Checking only for the source key would certify it.
    let IndexManagerInner::OpenClast(manager) = &mut f.manager.inner else {
        unreachable!()
    };
    let (key, file) = manager.manifest.files.iter_mut().next().unwrap();
    file.chunk_count = 2;
    let mut extra = tantivy::TantivyDocument::default();
    extra.add_text(fields.source_key.unwrap(), key.as_str());
    let mut writer: IndexWriter<tantivy::TantivyDocument> =
        index.writer(WRITER_MEMORY_BYTES).unwrap();
    writer.add_document(extra).unwrap();
    writer.commit().unwrap();
    writer.wait_merging_threads().unwrap();
    let report = f.manager.reconcile(f.config.clone()).unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("incomplete partition"))
    );
    f.check(&f.search("apricots"), None, 1);
    f.reconcile();
    f.check(&f.search("apricots"), Some("snapshot:A"), 1);
    assert_eq!(
        partition_source_counts(&partition_index_dir(
            &f.published().partitions_dir,
            &f.resources[0]
        ))
        .unwrap()
        .values()
        .sum::<usize>(),
        1
    );
}

#[test]
fn layout_bytes_remain_v3_with_and_without_labels_on_rollback() {
    // Frozen v3 parser and field order. A rollback must not rely on an old
    // reader ignoring extension fields or accepting a new version number.
    #[derive(serde::Deserialize, serde::Serialize)]
    #[serde(deny_unknown_fields)]
    struct V3Layout {
        layout_version: u32,
        profile: HostProfile,
        partitions: Vec<V3Partition>,
    }
    #[derive(serde::Deserialize, serde::Serialize)]
    #[serde(deny_unknown_fields)]
    struct V3Partition {
        partition_id: String,
        resource: ResourceKey,
    }
    for label in [Some("snapshot:A"), None] {
        let f = Fixture::with_label(Some("apricots"), label);
        let bytes = fs::read(f.published().layout_path).unwrap();
        let old: V3Layout = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(old.layout_version, 3);
        assert_eq!(old.profile, HostProfile::OpenClast);
        assert_eq!(old.partitions.len(), 1);
        assert_eq!(old.partitions[0].resource, f.resources[0]);
        assert_eq!(
            old.partitions[0].partition_id,
            crate::partition::partition_id(&f.resources[0])
        );
        let mut v3_bytes = serde_json::to_vec_pretty(&old).unwrap();
        v3_bytes.push(b'\n');
        assert_eq!(bytes, v3_bytes);
    }
}

#[test]
fn missing_corrupt_wrong_version_or_mismatched_sidecar_serves_every_room_unlabeled() {
    let mut f = Fixture::new(Some("apricots"));
    let mut other = f.config.vaults[0].clone();
    other.id = "second".into();
    other.room = Some("second".into());
    other.path = f.dir.path().join("second");
    fs::create_dir(&other.path).unwrap();
    let both = [
        f.resources[0].clone(),
        f.config.resource_key(&other).unwrap(),
    ];
    f.config.vaults.push(other);
    f.reconcile();
    let active = f.published();
    let path = active.root.join(crate::partition::LABEL_SIDECAR);
    let valid: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for defect in [
        "missing",
        "json",
        "version",
        "resource",
        "partition",
        "generation",
        "duplicate",
    ] {
        let mut bad = valid.clone();
        match defect {
            "missing" => fs::remove_file(&path).unwrap(),
            "json" => fs::write(&path, b"{").unwrap(),
            _ => {
                match defect {
                    "version" => bad["version"] = 400.into(),
                    "resource" => bad["labels"][1]["resource"]["room_id"] = "unknown".into(),
                    "partition" => bad["labels"][1]["partition_id"] = "wrong".into(),
                    "generation" => bad["generation"] = "predecessor".into(),
                    "duplicate" => bad["labels"][1] = bad["labels"][0].clone(),
                    _ => unreachable!(),
                }
                fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
            }
        }
        let result = reopened_result(&active, &both, "apricots");
        assert_eq!(result.hits.len(), 1, "{defect}");
        assert_eq!(result.snapshot_ids.len(), 2);
        assert!(
            result.snapshot_ids.values().all(Option::is_none),
            "{defect}"
        );
        assert!(result.require_snapshot_ids().is_err());
    }
}

#[test]
fn empty_participating_set_does_not_satisfy_required_labels() {
    let mut result = Fixture::new(None).search("apricots");
    result.snapshot_ids.clear();
    assert!(result.require_snapshot_ids().is_err());
}
