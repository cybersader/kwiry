//! Pin which awkward Markdown inputs withhold a governed room's publication
//! label. Each case sits beside one healthy note so a hit proves the room
//! still serves; only the label decides whether a live check may answer (a
//! withheld label refuses the live check with `503 resource_unavailable`).
//! These pin current behaviour; a change here is a public-behaviour change.
use super::*;

struct Observed {
    /// Label after a cold build and boot reconcile with the edge note present.
    initial: Option<String>,
    /// Label after a label change forces a strict-hash walk of the whole room.
    relabelled: Option<String>,
    healthy_hits: usize,
    warnings: Vec<String>,
}

fn observe(edge: &[u8]) -> Observed {
    let mut f = Fixture::new(Some("apricots"));
    f.manager.shutdown().unwrap();
    let room = f.config.vaults[0].path.clone();
    fs::write(room.join("edge.md"), edge).unwrap();
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
    let report = manager.reconcile(f.config.clone()).unwrap();
    let (relabelled, healthy_hits) = label(&runtime);
    let warnings = report
        .warnings
        .iter()
        .filter(|w| w.path.ends_with("edge.md"))
        .map(|w| w.message.clone())
        .collect();
    Observed {
        initial,
        relabelled,
        healthy_hits,
        warnings,
    }
}

fn labelled(edge: &[u8]) {
    let o = observe(edge);
    assert_eq!(o.initial.as_deref(), Some("snapshot:A"));
    assert_eq!(o.relabelled.as_deref(), Some("snapshot:B"));
    assert_eq!(o.healthy_hits, 1);
    assert!(o.warnings.is_empty(), "{:?}", o.warnings);
}

fn withheld(edge: &[u8], warning: &str) {
    let o = observe(edge);
    assert_eq!(o.initial, None);
    assert_eq!(o.relabelled, None);
    // The room still serves ordinary search; only the label is withheld.
    assert_eq!(o.healthy_hits, 1);
    assert_eq!(o.warnings.len(), 1, "{:?}", o.warnings);
    assert!(o.warnings[0].starts_with(warning), "{:?}", o.warnings);
}

fn sized(len: usize) -> Vec<u8> {
    let mut bytes = b"# Big\n".to_vec();
    bytes.resize(len, b'a');
    bytes
}

#[test]
fn zero_byte_markdown_keeps_the_label() {
    labelled(b"");
}

#[test]
fn whitespace_only_markdown_keeps_the_label() {
    labelled(b"  \n\t\n   \n");
}

#[test]
fn unterminated_frontmatter_is_body_text_and_keeps_the_label() {
    labelled(b"---\ntitle: x\nbody text\n");
}

#[test]
fn frontmatter_only_markdown_keeps_the_label() {
    labelled(b"---\ntitle: x\n---\n");
}

#[test]
fn invalid_yaml_frontmatter_withholds_the_label() {
    withheld(
        b"---\ntitle: [unclosed\n---\nbody\n",
        "invalid YAML frontmatter",
    );
}

#[test]
fn invalid_utf8_markdown_withholds_the_label() {
    withheld(b"# Head\nbad \xff\xfe bytes\n", "skipped non-UTF-8 file");
}

#[test]
fn markdown_of_exactly_the_size_budget_keeps_the_label() {
    labelled(&sized(crate::model::MAX_FILE_BYTES as usize));
}

#[test]
fn markdown_one_byte_over_the_size_budget_withholds_the_label() {
    withheld(
        &sized(crate::model::MAX_FILE_BYTES as usize + 1),
        "file exceeds source size budget",
    );
}
