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
        let dir = tempdir().unwrap();
        let room = dir.path().join("first");
        fs::create_dir(&room).unwrap();
        if let Some(text) = note {
            fs::write(room.join("note.md"), text).unwrap();
        }
        let mut config = openclast_config(vec![VaultRegistration {
            snapshot_id: Some("snapshot:A".parse().unwrap()),
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

fn write_legacy_layout(active: &crate::GenerationPaths) {
    let mut layout: serde_json::Value =
        serde_json::from_slice(&fs::read(&active.layout_path).unwrap()).unwrap();
    layout["layout_version"] = 3.into();
    layout["partitions"][0]
        .as_object_mut()
        .unwrap()
        .remove("snapshot_id");
    fs::write(&active.layout_path, serde_json::to_vec(&layout).unwrap()).unwrap();
}

#[test]
fn legacy_unlabeled_deployment_serves_after_boot_without_rebuilding_or_republishing() {
    let f = Fixture::new(Some("apricots"));
    let active = f.published();
    f.manager.shutdown().unwrap();
    write_legacy_layout(&active);
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
        write_legacy_layout(&active);
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
    let reader = PartitionedSearchIndex::open(&active).unwrap();
    assert!(reader.partitions[&f.resources[0]].snapshot_id.is_none());
}

#[test]
fn failed_root_cannot_relabel_the_serving_reader() {
    let mut f = Fixture::new(Some("apricots"));
    fs::rename(&f.config.vaults[0].path, f.dir.path().join("offline")).unwrap();
    f.config.vaults[0].snapshot_id = Some("snapshot:B".parse().unwrap());
    f.reconcile();
    f.check(&f.search("apricots"), Some("snapshot:A"), 1);
}
