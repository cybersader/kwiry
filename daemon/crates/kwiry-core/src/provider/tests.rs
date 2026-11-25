use std::time::{Duration, Instant};

use super::*;
use crate::semantic::test_support::HashEmbedder;

fn put(store: &ProviderStore, p: &str, id: &str, path: &str, body: &str) {
    store
        .put(p, id, &hex_sha256(body.as_bytes()), path, body.as_bytes())
        .unwrap();
}

fn names(partitions: &[&str]) -> Vec<String> {
    partitions.iter().map(|p| (*p).to_owned()).collect()
}

fn ids(store: &ProviderStore, partitions: &[&str], q: &str) -> Vec<String> {
    let mut ids = ordered(store, partitions, q, ProviderMode::Lexical, 10);
    ids.sort_unstable();
    ids
}

fn ordered(
    store: &ProviderStore,
    partitions: &[&str],
    q: &str,
    mode: ProviderMode,
    limit: usize,
) -> Vec<String> {
    ask(store, partitions, q, mode, None, limit)
        .refs
        .into_iter()
        .map(|r| format!("{}/{}", r.partition, r.id))
        .collect()
}

fn ask(
    store: &ProviderStore,
    partitions: &[&str],
    q: &str,
    mode: ProviderMode,
    terms: Option<&[String]>,
    limit: usize,
) -> ProviderQueryOutcome {
    store
        .ask(&ProviderQuery {
            partitions: &names(partitions),
            text: q,
            mode,
            terms,
            limit,
        })
        .unwrap()
}

fn state(store: &ProviderStore, p: &str) -> (ProviderState, Option<ProviderAsOf>) {
    let coverage = &store.query(&[p.to_owned()], "x", 5).unwrap().coverage[0];
    (coverage.state, coverage.as_of)
}

fn feed(store: &ProviderStore, p: &str, notes: &[(&str, &str, &str)]) -> ProviderAsOf {
    store.begin(p, 1, ProviderFeedMode::Full).unwrap();
    for (id, path, body) in notes {
        put(store, p, id, path, body);
    }
    store.checkpoint(p, 1).unwrap();
    store.commit(p).unwrap()
}

fn semantic_store(dir: &Path) -> ProviderStore {
    ProviderStore::open_with_semantic(
        dir,
        Box::new(HashEmbedder::new()),
        Box::new(HashEmbedder::new()),
    )
    .unwrap()
}

/// Waits until the semantic kind of `p` has caught up with its lexical kind.
fn caught_up(store: &ProviderStore, p: &str) -> ProviderKindStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = store.status(p).unwrap();
        let semantic = status.semantic.clone().unwrap();
        if semantic.state == ProviderState::Ready && semantic.as_of == status.as_of {
            assert_eq!(semantic.pending_notes, 0);
            return semantic;
        }
        assert!(
            Instant::now() < deadline,
            "embedding never caught up: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn provider_feeds_become_visible_only_at_commit() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProviderStore::open(dir.path()).unwrap();
    assert_eq!(state(&store, "p"), (ProviderState::Unavailable, None));
    store.begin("p", 1, ProviderFeedMode::Full).unwrap();
    assert_eq!(state(&store, "p"), (ProviderState::Building, None));
    put(
        &store,
        "p",
        "a",
        "Areas/alpha.md",
        "# Alpha\n\nquokka habitat\n",
    );
    put(&store, "p", "b", "beta.md", "# Beta\n\nunrelated\n");
    put(
        &store,
        "p",
        "c",
        "broken.md",
        "---\n: : [\n---\nbody quokka\n",
    );
    put(&store, "p", "d", "blob.bin", "quokka");
    store.checkpoint("p", 1).unwrap();
    put(
        &store,
        "p",
        "late",
        "late.md",
        "quokka after the checkpoint\n",
    );
    assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 1 });
    assert_eq!(ids(&store, &["p"], "quokka"), ["p/a", "p/c"]);

    store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
    put(&store, "p", "b", "beta.md", "# Beta\n\nquokka quokka\n");
    store.delete("p", "a").unwrap();
    store.checkpoint("p", 2).unwrap();
    assert_eq!(
        ids(&store, &["p"], "quokka"),
        ["p/a", "p/c"],
        "not before commit"
    );
    assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 2 });
    assert_eq!(ids(&store, &["p"], "quokka"), ["p/b", "p/c"]);

    store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
    let refused = store.checkpoint("p", 2).unwrap_err();
    assert_eq!(refused.code, ProviderErrorCode::CheckpointNotIncreasing);
    let mismatch = store.put("p", "e", &hex_sha256(b"other"), "e.md", b"body");
    assert_eq!(
        mismatch.unwrap_err().code,
        ProviderErrorCode::VersionMismatch
    );
    store.exclude_over_transport_cap("p", "huge").unwrap();
    store.checkpoint("p", 3).unwrap();
    store.commit("p").unwrap();

    let status = store.status("p").unwrap();
    assert_eq!(status.notes, 2);
    assert_eq!(status.excluded.get("unsupported_format"), Some(&1));
    assert_eq!(status.excluded.get(PROVIDER_OVER_TRANSPORT_CAP), Some(&1));
    assert_eq!(status.partial.get("frontmatter_not_extracted"), Some(&1));
    assert_eq!(status.semantic, None, "no model, no semantic kind");

    assert_eq!(
        store
            .begin("p", 2, ProviderFeedMode::Delta)
            .unwrap_err()
            .code,
        ProviderErrorCode::EpochMismatch
    );
    // A full resync keeps serving epoch 1 until it commits; ids it did
    // not receive are gone afterwards.
    store.begin("p", 2, ProviderFeedMode::Full).unwrap();
    put(&store, "p", "z", "zeta.md", "quokka\n");
    store.checkpoint("p", 1).unwrap();
    assert_eq!(state(&store, "p").1, Some(ProviderAsOf { epoch: 1, n: 3 }));
    store.commit("p").unwrap();
    assert_eq!(ids(&store, &["p"], "quokka"), ["p/z"]);
    assert_eq!(
        store
            .begin("p", 1, ProviderFeedMode::Full)
            .unwrap_err()
            .code,
        ProviderErrorCode::EpochRegressed
    );
    assert_eq!(
        store.delete("p", "z").unwrap_err().code,
        ProviderErrorCode::NoOpenFeed
    );

    store.retire("p").unwrap();
    assert_eq!(state(&store, "p").0, ProviderState::Unavailable);
}

#[test]
fn provider_keeps_same_paths_in_two_partitions_apart() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProviderStore::open(dir.path()).unwrap();
    for name in ["p", "q"] {
        feed(
            &store,
            name,
            &[("same", "shared.md", "# Shared\n\nwombat burrow\n")],
        );
    }
    assert_eq!(
        ids(&store, &["p", "q", "r"], "wombat"),
        ["p/same", "q/same"]
    );
    assert_eq!(ids(&store, &["q"], "wombat"), ["q/same"]);
    let names = ["p".to_owned(), "p".to_owned()];
    assert_eq!(
        store.query(&names, "wombat", 5).unwrap_err().code,
        ProviderErrorCode::BadRequest
    );
}

#[test]
fn provider_never_serves_an_abandoned_delta_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProviderStore::open(dir.path()).unwrap();
    store.begin("p", 1, ProviderFeedMode::Full).unwrap();
    put(&store, "p", "a", "alpha.md", "# Alpha\n\nquokka\n");
    store.checkpoint("p", 1).unwrap();
    assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 1 });

    // A delta that checkpointed and was then abandoned by a new begin.
    store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
    put(&store, "p", "ghost", "ghost.md", "# Ghost\n\nquokka\n");
    store.checkpoint("p", 2).unwrap();
    let code = |result: ProviderResult<()>| result.unwrap_err().code;
    assert_eq!(
        code(store.begin("p", 1, ProviderFeedMode::Delta)),
        ProviderErrorCode::EpochMismatch
    );
    assert_eq!(
        code(store.checkpoint("p", 2)),
        ProviderErrorCode::NoOpenFeed
    );
    assert_eq!(
        store.commit("p").unwrap_err().code,
        ProviderErrorCode::NoOpenFeed
    );
    assert_eq!(ids(&store, &["p"], "quokka"), ["p/a"]);
    assert_eq!(state(&store, "p").1, Some(ProviderAsOf { epoch: 1, n: 1 }));
    let status = store.status("p").unwrap();
    assert_eq!(status.notes, 1, "status counts the commit, not the ghost");

    // A full resync with the same epoch counts checkpoints from the
    // commit, so the host's resend of 2 is accepted.
    store.begin("p", 1, ProviderFeedMode::Full).unwrap();
    put(&store, "p", "a", "alpha.md", "# Alpha\n\nquokka\n");
    store.checkpoint("p", 2).unwrap();
    assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 2 });
    assert_eq!(ids(&store, &["p"], "quokka"), ["p/a"]);
    store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
    store.checkpoint("p", 3).unwrap();
    assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 3 });

    // A delta abandoned before any checkpoint leaves nothing behind.
    store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
    put(&store, "p", "b", "beta.md", "# Beta\n\nquokka\n");
    store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
    store.checkpoint("p", 4).unwrap();
    assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 4 });
    assert_eq!(ids(&store, &["p"], "quokka"), ["p/a"]);
}

#[test]
fn provider_remembers_an_abandoned_higher_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProviderStore::open(dir.path()).unwrap();
    store.begin("p", 1, ProviderFeedMode::Full).unwrap();
    store.checkpoint("p", 1).unwrap();
    store.commit("p").unwrap();
    // A full at epoch 3 is abandoned by a delta at the committed epoch.
    store.begin("p", 3, ProviderFeedMode::Full).unwrap();
    store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
    store.checkpoint("p", 2).unwrap();
    assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 1, n: 2 });
    assert_eq!(
        store
            .begin("p", 2, ProviderFeedMode::Full)
            .unwrap_err()
            .code,
        ProviderErrorCode::EpochRegressed,
        "the delta must not lower the epoch record"
    );
    store.begin("p", 3, ProviderFeedMode::Full).unwrap();
    store.checkpoint("p", 1).unwrap();
    assert_eq!(store.commit("p").unwrap(), ProviderAsOf { epoch: 3, n: 1 });
}

#[cfg(unix)]
#[test]
fn provider_data_dir_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let _store = ProviderStore::open(&data).unwrap();
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&data), 0o700);
    assert_eq!(mode(&data.join("partitions")), 0o700);
    assert_eq!(mode(&data.join(DATA_MARKER)), 0o600);
}

#[test]
fn provider_refuses_a_foreign_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("keep.txt"), b"x").unwrap();
    assert!(ProviderStore::open(dir.path()).is_err());
    assert!(dir.path().join("keep.txt").is_file());
}

/// The partner's `galka` case: many notes carry a longer `galka…` word in a
/// heading (which Kwiry's prefix and heading signals rank highly for
/// `galka`), fewer contain the word `galka` itself in the body. Every note
/// with the body word must be returned while they fit in the limit, and
/// when they do not fit, every returned note must be one of them.
#[test]
fn lexical_answers_meet_the_superset_rule_under_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProviderStore::open(dir.path()).unwrap();
    let mut notes = Vec::new();
    for i in 0..40 {
        notes.push((
            format!("h{i:02}"),
            format!("decoys/galkavor-{i:02}.md"),
            format!("# Galkavor field notes {i}\n\nThe galkavor ridge and other places.\n"),
        ));
    }
    for i in 0..6 {
        notes.push((
            format!("b{i:02}"),
            format!("plain/note-{i:02}.md"),
            format!("# Plain {i}\n\nWe saw a galka near the shore, entry {i}.\n"),
        ));
    }
    // In frontmatter only: not a body word, not owed.
    notes.push((
        "f00".to_owned(),
        "plain/front.md".to_owned(),
        "---\ntags: galka\n---\nNothing here.\n".to_owned(),
    ));
    let borrowed: Vec<_> = notes
        .iter()
        .map(|(id, path, body)| (id.as_str(), path.as_str(), body.as_str()))
        .collect();
    feed(&store, "p", &borrowed);

    let body: Vec<String> = (0..6).map(|i| format!("p/b{i:02}")).collect();
    // Under the limit: all six, first, then Kwiry's other candidates.
    let answer = ordered(&store, &["p"], "galka", ProviderMode::Lexical, 10);
    assert_eq!(answer.len(), 10);
    let mut head = answer[..6].to_vec();
    head.sort();
    assert_eq!(head, body, "{answer:?}");
    // Over the limit: every returned ref contains the word.
    let answer = ordered(&store, &["p"], "galka", ProviderMode::Lexical, 4);
    assert_eq!(answer.len(), 4);
    assert!(answer.iter().all(|id| body.contains(id)), "{answer:?}");
    // Deterministic.
    assert_eq!(
        answer,
        ordered(&store, &["p"], "galka", ProviderMode::Lexical, 4)
    );

    // `terms` are authoritative: the recall set follows them, not the text.
    let terms = vec!["ridge".to_owned()];
    let answer = ask(
        &store,
        &["p"],
        "galka",
        ProviderMode::Lexical,
        Some(&terms),
        200,
    );
    let returned: HashSet<_> = answer.refs.iter().map(|r| r.id.clone()).collect();
    for i in 0..40 {
        assert!(returned.contains(&format!("h{i:02}")));
    }

    // The obligation follows edits: a delta that adds the word to one decoy's
    // body makes it owed; deleting a body note removes it.
    store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
    put(
        &store,
        "p",
        "h39",
        "decoys/galkavor-39.md",
        "# Galkavor 39\n\nNow with a galka too.\n",
    );
    store.delete("p", "b00").unwrap();
    store.checkpoint("p", 2).unwrap();
    store.commit("p").unwrap();
    let mut owed: Vec<String> = (1..6).map(|i| format!("p/b{i:02}")).collect();
    owed.push("p/h39".to_owned());
    owed.sort();
    let answer = ordered(&store, &["p"], "galka", ProviderMode::Lexical, 6);
    let mut sorted = answer.clone();
    sorted.sort();
    assert_eq!(sorted, owed);
}

#[test]
fn blank_ranking_text_still_searches_authoritative_terms() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProviderStore::open(dir.path()).unwrap();
    feed(
        &store,
        "p",
        &[("a", "a.md", "quokka"), ("b", "b.md", "otter")],
    );
    let answer = ask(
        &store,
        &["p"],
        " \t\n",
        ProviderMode::Lexical,
        Some(&["quokka".to_owned()]),
        1,
    );
    assert_eq!(answer.refs.len(), 1);
    assert_eq!(answer.refs[0].id, "a");
}

#[test]
fn lexical_obligation_counts_only_the_asked_partitions() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProviderStore::open(dir.path()).unwrap();
    feed(
        &store,
        "p",
        &[
            ("p1", "a.md", "galka one\n"),
            ("p2", "b.md", "# Galkavor\n\nother\n"),
        ],
    );
    let many: Vec<(String, String, String)> = (0..30)
        .map(|i| {
            (
                format!("q{i:02}"),
                format!("q/{i:02}.md"),
                format!("galka galka {i}\n"),
            )
        })
        .collect();
    let borrowed: Vec<_> = many
        .iter()
        .map(|(id, path, body)| (id.as_str(), path.as_str(), body.as_str()))
        .collect();
    feed(&store, "q", &borrowed);
    let answer = ordered(&store, &["p"], "galka", ProviderMode::Lexical, 1);
    assert_eq!(answer, ["p/p1"]);
    let both = ordered(&store, &["p", "q"], "galka", ProviderMode::Lexical, 31);
    assert_eq!(both.len(), 31, "all 31 owed notes, nothing else: {both:?}");
    assert!(!both.contains(&"p/p2".to_owned()));
}

#[test]
fn superset_order_keeps_tiers_and_breaks_ties_by_id() {
    let r = |id: &str| {
        Arc::new(ProviderReference {
            partition: "p".to_owned(),
            id: id.to_owned(),
            version: "v".to_owned(),
        })
    };
    // Kwiry ranks x (no match), c, b (equal scores), z (no match).
    let ranked = vec![(r("x"), 9.0), (r("c"), 5.0), (r("b"), 5.0), (r("z"), 1.0)];
    // Matching: b, c, plus unranked a (BM25 2.0) and d (BM25 3.0).
    let all = vec![(r("a"), 2.0), (r("b"), 1.0), (r("c"), 1.0), (r("d"), 3.0)];
    let ids =
        |out: Vec<Arc<ProviderReference>>| out.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
    assert_eq!(
        ids(superset_order(ranked.clone(), all.clone(), 10)),
        ["b", "c", "d", "a", "x", "z"]
    );
    assert_eq!(
        ids(superset_order(ranked.clone(), all.clone(), 3)),
        ["b", "c", "d"]
    );
    assert_eq!(ids(superset_order(ranked.clone(), all.clone(), 1)), ["b"]);
    assert_eq!(
        ids(superset_order(ranked, Vec::new(), 10)),
        ["x", "b", "c", "z"]
    );
}

#[test]
fn equal_ranks_are_ordered_by_id_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProviderStore::open(dir.path()).unwrap();
    feed(
        &store,
        "p",
        &[
            ("zz", "n/one.md", "wombat\n"),
            ("mm", "n/two.md", "wombat\n"),
            ("aa", "n/six.md", "wombat\n"),
        ],
    );
    assert_eq!(
        ordered(&store, &["p"], "wombat", ProviderMode::Lexical, 10),
        ["p/aa", "p/mm", "p/zz"]
    );
}

#[test]
fn semantic_needs_a_model() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProviderStore::open(dir.path()).unwrap();
    for mode in [ProviderMode::Semantic, ProviderMode::Hybrid] {
        let error = store
            .ask(&ProviderQuery {
                partitions: &names(&["p"]),
                text: "x",
                mode,
                terms: None,
                limit: 5,
            })
            .unwrap_err();
        assert_eq!(error.code, ProviderErrorCode::ModeUnavailable);
    }
}

const PARTITION_A: [(&str, &str, &str); 4] = [
    (
        "a1",
        "a/garden.md",
        "Watering the garden tomatoes every morning.\n",
    ),
    ("a2", "a/river.md", "Sailing boats on the river at dusk.\n"),
    (
        "a3",
        "a/trail.md",
        "Hiking a mountain trail with garden snacks.\n",
    ),
    ("a4", "a/kitchen.md", "Cooking tomatoes in the kitchen.\n"),
];

/// B holds notes nearer to the query than anything in A.
const PARTITION_B: [(&str, &str, &str); 3] = [
    ("b1", "b/exact.md", "garden tomatoes\n"),
    ("b2", "b/exact2.md", "garden tomatoes garden tomatoes\n"),
    ("b3", "b/near.md", "tomatoes garden\n"),
];

/// Vector search touches only the asked partitions: vectors of another
/// partition never become candidates and never change the order, even when
/// they are nearer than every asked vector.
#[test]
fn semantic_search_reads_only_the_asked_partitions() {
    let alone = tempfile::tempdir().unwrap();
    let alone = semantic_store(alone.path());
    feed(&alone, "A", &PARTITION_A);
    caught_up(&alone, "A");
    let baseline = ordered(
        &alone,
        &["A"],
        "garden tomatoes",
        ProviderMode::Semantic,
        10,
    );
    assert_eq!(baseline.len(), 4);

    let dir = tempfile::tempdir().unwrap();
    let store = semantic_store(dir.path());
    feed(&store, "A", &PARTITION_A);
    feed(&store, "B", &PARTITION_B);
    caught_up(&store, "A");
    caught_up(&store, "B");
    let b_alone = ordered(
        &store,
        &["B"],
        "garden tomatoes",
        ProviderMode::Semantic,
        10,
    );
    assert_eq!(b_alone[0], "B/b1", "B is nearer: {b_alone:?}");

    for limit in [1, 2, 3, 4, 10] {
        let scoped = ordered(
            &store,
            &["A"],
            "garden tomatoes",
            ProviderMode::Semantic,
            limit,
        );
        assert_eq!(scoped, baseline[..limit.min(4)], "limit {limit}");
    }
    let answer = ask(
        &store,
        &["A"],
        "garden tomatoes",
        ProviderMode::Semantic,
        None,
        10,
    );
    assert_eq!(answer.coverage.len(), 1);
    assert_eq!(answer.coverage[0].kind, ProviderKind::Semantic);
    assert!(answer.refs.iter().all(|r| r.partition == "A"));

    // A set query is the merge of the per-partition queries: same members,
    // and each partition's own order is kept.
    let both = ordered(
        &store,
        &["A", "B"],
        "garden tomatoes",
        ProviderMode::Semantic,
        10,
    );
    let mut members = both.clone();
    members.sort();
    let mut expected: Vec<_> = baseline.iter().chain(&b_alone).cloned().collect();
    expected.sort();
    assert_eq!(members, expected);
    let a_in_both: Vec<_> = both
        .iter()
        .filter(|id| id.starts_with("A/"))
        .cloned()
        .collect();
    assert_eq!(a_in_both, baseline);
    let b_in_both: Vec<_> = both
        .iter()
        .filter(|id| id.starts_with("B/"))
        .cloned()
        .collect();
    assert_eq!(b_in_both, b_alone);
    // With a limit below the set size, the top of the merge is B's nearest.
    assert_eq!(
        ordered(
            &store,
            &["A", "B"],
            "garden tomatoes",
            ProviderMode::Semantic,
            1
        ),
        ["B/b1"]
    );

    // Retiring B changes nothing for A.
    store.retire("B").unwrap();
    assert_eq!(
        ordered(
            &store,
            &["A"],
            "garden tomatoes",
            ProviderMode::Semantic,
            10
        ),
        baseline
    );
    let answer = ask(
        &store,
        &["A", "B"],
        "garden tomatoes",
        ProviderMode::Semantic,
        None,
        10,
    );
    assert_eq!(answer.coverage[1].state, ProviderState::Unavailable);
}

#[test]
fn hybrid_retries_if_an_edit_and_delete_land_between_legs() {
    let dir = tempfile::tempdir().unwrap();
    let (entered, between_legs) = mpsc::channel();
    let (resume, released) = mpsc::channel();
    let store = ProviderStore::open_with_semantic(
        dir.path(),
        Box::new(PausedQuery {
            inner: HashEmbedder::new(),
            gate: Some((entered, released)),
        }),
        Box::new(HashEmbedder::new()),
    )
    .unwrap();
    feed(
        &store,
        "p",
        &[("a", "a.md", "quokka old"), ("b", "b.md", "quokka habitat")],
    );
    caught_up(&store, "p");
    let (answer, latest) = std::thread::scope(|scope| {
        let query = scope.spawn(|| ask(&store, &["p"], "quokka", ProviderMode::Hybrid, None, 5));
        between_legs.recv_timeout(Duration::from_secs(5)).unwrap();
        store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
        put(&store, "p", "a", "a.md", "quokka new");
        store.delete("p", "b").unwrap();
        store.checkpoint("p", 2).unwrap();
        let latest = store.commit("p").unwrap();
        caught_up(&store, "p");
        resume.send(()).unwrap();
        (query.join().unwrap(), latest)
    });
    assert_eq!(answer.refs.len(), 1, "deleted note leaked: {answer:?}");
    assert_eq!(answer.refs[0].id, "a");
    assert_eq!(answer.refs[0].version, hex_sha256(b"quokka new"));
    assert!(
        answer
            .coverage
            .iter()
            .all(|kind| kind.as_of == Some(latest))
    );
}

struct PausedQuery {
    inner: HashEmbedder,
    gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
}

impl Embedder for PausedQuery {
    fn profile(&self) -> &crate::semantic::EmbeddingProfile {
        self.inner.profile()
    }

    fn embed_passages(&mut self, texts: &[String]) -> crate::Result<Vec<Vec<f32>>> {
        self.inner.embed_passages(texts)
    }

    fn embed_query(&mut self, text: &str) -> crate::Result<Vec<f32>> {
        if let Some((entered, released)) = self.gate.take() {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(20)).unwrap();
        }
        self.inner.embed_query(text)
    }
}

#[test]
fn semantic_does_not_serve_a_prior_epoch_during_full_lag() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Mutex::new(()));
    let store = gated_store(dir.path(), &gate);
    let notes = [("a", "a.md", "quokka habitat")];
    feed(&store, "p", &notes);
    caught_up(&store, "p");

    let closed = gate.lock().unwrap();
    store.begin("p", 2, ProviderFeedMode::Full).unwrap();
    put(&store, "p", notes[0].0, notes[0].1, notes[0].2);
    store.checkpoint("p", 1).unwrap();
    let latest = store.commit("p").unwrap();
    let answer = ask(&store, &["p"], "quokka", ProviderMode::Semantic, None, 5);
    assert_eq!(answer.coverage[0].state, ProviderState::Building);
    assert_eq!(answer.coverage[0].as_of, None);
    assert!(answer.refs.is_empty());
    let status = store.status("p").unwrap();
    assert_eq!(status.as_of, Some(latest));
    assert_eq!(status.semantic.unwrap().as_of, None);
    drop(closed);
    assert_eq!(caught_up(&store, "p").as_of, Some(latest));
}

#[test]
fn ambiguous_words_checkpoint_requires_a_full_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProviderStore::open(dir.path()).unwrap();
    let first = feed(&store, "p", &[("a", "a.md", "quokka")]);
    store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
    put(&store, "p", "a", "a.md", "badger");
    let partition = store.feeding("p").unwrap();
    partition.lock().target().unwrap().words.fail_commit = true;
    assert_eq!(
        store.checkpoint("p", 2).unwrap_err().code,
        ProviderErrorCode::Internal
    );
    let publish_refused = store.commit("p").is_err();
    assert_eq!(store.status("p").unwrap().as_of, Some(first));
    assert_eq!(ids(&store, &["p"], "quokka"), ["p/a"]);
    assert!(ids(&store, &["p"], "badger").is_empty());
    assert_eq!(
        store
            .begin("p", 1, ProviderFeedMode::Delta)
            .unwrap_err()
            .code,
        ProviderErrorCode::EpochMismatch
    );
    assert!(publish_refused, "an ambiguous generation can still publish");

    store.begin("p", 1, ProviderFeedMode::Full).unwrap();
    put(&store, "p", "a", "a.md", "badger");
    store.checkpoint("p", 2).unwrap();
    store.commit("p").unwrap();
    assert_eq!(ids(&store, &["p"], "badger"), ["p/a"]);
}

#[test]
fn lexical_recalls_split_terms_after_a_full_candidate_window() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProviderStore::open(dir.path()).unwrap();
    store.begin("p", 1, ProviderFeedMode::Full).unwrap();
    // Disjoint filename and body lanes each have more than 256 decoys.
    for n in 0..260 {
        let path = format!("filename-{n}/quokka.md");
        put(&store, "p", &path, &path, "unrelated body");
        let path = format!("body-{n}.md");
        put(&store, "p", &path, &path, "quokka quokka quokka quokka");
    }
    let target = format!(
        "# first\n\nquokka\n\n# second\n\na{}\u{345}\u{897}b",
        "\u{301}".repeat(63)
    );
    put(&store, "p", "target", "target.md", &target);
    store.checkpoint("p", 1).unwrap();
    store.commit("p").unwrap();
    let p = store.known("p").unwrap().unwrap();
    let served = p.served().unwrap();
    assert_eq!(chunk_addresses(&served, "target.md").unwrap().len(), 2);
    let outcome = search_pinned_with_quality(
        &[PinnedSearcher {
            index: &served.index,
            fields: &served.fields,
            searcher: &served.searcher,
        }],
        "quokka",
        MAX_TOTAL_CANDIDATES,
        &SearchFilters::default(),
    )
    .unwrap();
    assert_eq!(outcome.hits.len(), MAX_TOTAL_CANDIDATES);
    assert!(outcome.hits.iter().all(|hit| hit.path != "target.md"));
    // The host's terms are authoritative even when Kwiry's ranking text
    // retrieves a completely different window of candidates.
    let terms = names(&["quokka", "aι"]);
    let answer = ask(
        &store,
        &["p"],
        "quokka",
        ProviderMode::Lexical,
        Some(&terms),
        1,
    );
    assert_eq!(answer.refs[0].id, "target");
}

#[test]
fn embedding_backlog_coalesces_commits_while_indexer_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Mutex::new(()));
    let store = gated_store(dir.path(), &gate);
    let closed = gate.lock().unwrap();
    feed(
        &store,
        "p",
        &[
            ("a", "a.md", "initial quokka"),
            ("b", "b.md", "to be deleted"),
        ],
    );
    let partition = store.known("p").unwrap().unwrap();
    let mut latest = None;
    for n in 2..=12 {
        store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
        put(&store, "p", "a", "a.md", &format!("quokka version {n}"));
        if n == 2 {
            store.delete("p", "b").unwrap();
            put(&store, "p", "c", "c.md", "keep across intervening commits");
        }
        store.checkpoint("p", n).unwrap();
        latest = Some(store.commit("p").unwrap());
    }
    // The map and test own one reference each. At most the running job and
    // one queued job can hold partition snapshots while the worker is gated.
    assert!(Arc::strong_count(&partition) <= 4);
    assert_eq!(store.status("p").unwrap().as_of, latest);
    drop(closed);
    assert_eq!(caught_up(&store, "p").as_of, latest);
    let answer = ask(&store, &["p"], "quokka", ProviderMode::Semantic, None, 5);
    assert_eq!(answer.refs.len(), 2);
    let a = answer.refs.iter().find(|r| r.id == "a").unwrap();
    assert_eq!(a.version, hex_sha256(b"quokka version 12"));
    assert!(answer.refs.iter().any(|r| r.id == "c"));
}

#[test]
fn lexical_commit_finishes_during_a_paused_semantic_scan() {
    let dir = tempfile::tempdir().unwrap();
    let store = semantic_store(dir.path());
    feed(&store, "p", &[("a", "a.md", "quokka")]);
    caught_up(&store, "p");
    store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
    store.delete("p", "a").unwrap();
    store.checkpoint("p", 2).unwrap();

    let partition = store.known("p").unwrap().unwrap();
    let snapshot = read(&partition.semantic).snapshot.clone().unwrap();
    std::thread::scope(|scope| {
        let (entered, scanning) = mpsc::sync_channel(0);
        let (resume, paused) = mpsc::channel();
        *lock(&snapshot.before_scan) = Some((entered, paused));
        let (committed, done) = mpsc::channel();
        let reader = scope.spawn(|| ordered(&store, &["p"], "quokka", ProviderMode::Semantic, 1));
        scanning.recv_timeout(Duration::from_secs(10)).unwrap();
        let store = &store;
        scope.spawn(move || committed.send(store.commit("p")).unwrap());
        let during_scan = done.recv_timeout(Duration::from_secs(10));
        resume.send(()).unwrap();
        assert_eq!(
            during_scan
                .expect("commit blocked behind semantic scan")
                .unwrap()
                .n,
            2
        );
        assert!(
            reader.join().unwrap().is_empty(),
            "returned the deleted note"
        );
    });
}

#[test]
fn pending_notes_counts_distinct_ids_across_running_and_queued_work() {
    let dir = tempfile::tempdir().unwrap();
    let (store, entered, resume) = paused_indexer(dir.path());
    feed(
        &store,
        "p",
        &[("a", "a.md", "quokka"), ("b", "b.md", "otter")],
    );
    entered.recv_timeout(Duration::from_secs(10)).unwrap();

    store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
    put(&store, "p", "a", "a.md", "quokka edit");
    store.delete("p", "b").unwrap();
    put(&store, "p", "c", "c.md", "wombat");
    store.checkpoint("p", 2).unwrap();
    let latest = store.commit("p").unwrap();
    assert_eq!(
        store.status("p").unwrap().semantic.unwrap().pending_notes,
        3
    );

    resume.send(true).unwrap();
    // caught_up also asserts that pendingNotes is zero.
    assert_eq!(caught_up(&store, "p").as_of, Some(latest));
}

#[test]
fn embedding_worker_panic_marks_running_and_queued_partitions_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let (store, entered, resume) = paused_indexer(dir.path());
    feed(&store, "running", &[("a", "a.md", "quokka")]);
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    for p in ["queued1", "queued2"] {
        feed(&store, p, &[("a", "a.md", "quokka")]);
        assert_eq!(store.status(p).unwrap().semantic.unwrap().pending_notes, 1);
    }
    resume.send(false).unwrap();
    let queue = &store.semantic.as_ref().unwrap().jobs;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !queue.failed.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "worker panic not recorded");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(lock(&queue.work).is_empty());
    // A new full feed cannot resurrect a dead worker.
    feed(&store, "later", &[("a", "a.md", "quokka")]);
    let partitions = ["running", "queued1", "queued2", "later"];
    for p in partitions {
        let status = store.status(p).unwrap();
        assert_eq!(status.state, ProviderState::Ready);
        let semantic = status.semantic.unwrap();
        assert_eq!(semantic.state, ProviderState::Unavailable);
        assert_eq!(semantic.as_of, None);
        assert_eq!(semantic.pending_notes, 0);
        let partition = store.known(p).unwrap().unwrap();
        assert!(lock(&partition.pending).by_id.is_empty());
    }
    let answer = ask(
        &store,
        &partitions,
        "quokka",
        ProviderMode::Semantic,
        None,
        5,
    );
    assert!(answer.refs.is_empty());
    assert!(
        answer
            .coverage
            .iter()
            .all(|c| c.state == ProviderState::Unavailable)
    );
    assert_eq!(ids(&store, &partitions, "quokka").len(), 4);
}

struct PausedPassages {
    inner: HashEmbedder,
    gate: Option<(mpsc::Sender<()>, mpsc::Receiver<bool>)>,
}

impl Embedder for PausedPassages {
    fn profile(&self) -> &crate::semantic::EmbeddingProfile {
        self.inner.profile()
    }

    fn embed_passages(&mut self, texts: &[String]) -> crate::Result<Vec<Vec<f32>>> {
        if let Some((entered, resume)) = self.gate.take() {
            entered.send(()).unwrap();
            assert!(resume.recv().unwrap(), "injected worker panic");
        }
        self.inner.embed_passages(texts)
    }

    fn embed_query(&mut self, text: &str) -> crate::Result<Vec<f32>> {
        self.inner.embed_query(text)
    }
}

fn paused_indexer(dir: &Path) -> (ProviderStore, mpsc::Receiver<()>, mpsc::Sender<bool>) {
    let (entered, running) = mpsc::channel();
    let (resume, paused) = mpsc::channel();
    let store = ProviderStore::open_with_semantic(
        dir,
        Box::new(HashEmbedder::new()),
        Box::new(PausedPassages {
            inner: HashEmbedder::new(),
            gate: Some((entered, paused)),
        }),
    )
    .unwrap();
    (store, running, resume)
}

#[test]
fn embedding_large_delta_limits_passages_per_call() {
    let dir = tempfile::tempdir().unwrap();
    let batches = Arc::new(Mutex::new(Vec::new()));
    let store = ProviderStore::open_with_semantic(
        dir.path(),
        Box::new(HashEmbedder::new()),
        Box::new(RecordingEmbedder {
            inner: HashEmbedder::new(),
            batches: Arc::clone(&batches),
        }),
    )
    .unwrap();
    feed(&store, "p", &[("a", "a.md", "quokka before the edit")]);
    caught_up(&store, "p");
    lock(&batches).clear();

    let large = (0..260)
        .map(|n| format!("## Section {n}\n\nquokka habitat\n\n"))
        .collect::<String>();
    store.begin("p", 1, ProviderFeedMode::Delta).unwrap();
    put(&store, "p", "a", "a.md", &large);
    store.checkpoint("p", 2).unwrap();
    store.commit("p").unwrap();
    caught_up(&store, "p");

    let batches = lock(&batches);
    assert_eq!(batches.iter().sum::<usize>(), 260);
    assert!(
        batches.iter().all(|&size| size <= 128),
        "unbounded passage batches: {batches:?}"
    );
    let answer = ask(&store, &["p"], "quokka", ProviderMode::Semantic, None, 5);
    assert_eq!(answer.refs[0].version, hex_sha256(large.as_bytes()));
}

struct RecordingEmbedder {
    inner: HashEmbedder,
    batches: Arc<Mutex<Vec<usize>>>,
}

impl Embedder for RecordingEmbedder {
    fn profile(&self) -> &crate::semantic::EmbeddingProfile {
        self.inner.profile()
    }

    fn embed_passages(&mut self, texts: &[String]) -> crate::Result<Vec<Vec<f32>>> {
        lock(&self.batches).push(texts.len());
        self.inner.embed_passages(texts)
    }

    fn embed_query(&mut self, text: &str) -> crate::Result<Vec<f32>> {
        self.inner.embed_query(text)
    }
}

fn gated_store(dir: &Path, gate: &Arc<Mutex<()>>) -> ProviderStore {
    ProviderStore::open_with_semantic(
        dir,
        Box::new(HashEmbedder::new()),
        Box::new(GatedEmbedder {
            inner: HashEmbedder::new(),
            gate: Arc::clone(gate),
        }),
    )
    .unwrap()
}

/// An embedder whose passage batches wait on a gate the test holds.
struct GatedEmbedder {
    inner: HashEmbedder,
    gate: Arc<Mutex<()>>,
}

impl Embedder for GatedEmbedder {
    fn profile(&self) -> &crate::semantic::EmbeddingProfile {
        self.inner.profile()
    }

    fn embed_passages(&mut self, texts: &[String]) -> crate::Result<Vec<Vec<f32>>> {
        let _open = self.gate.lock().unwrap();
        self.inner.embed_passages(texts)
    }

    fn embed_query(&mut self, text: &str) -> crate::Result<Vec<f32>> {
        self.inner.embed_query(text)
    }
}

/// While embedding lags, the semantic kind says `building` at the last
/// commit it holds completely, and never returns a note whose vectors
/// describe bytes the lexical commit no longer holds.
#[test]
fn semantic_lag_is_reported_and_never_serves_stale_versions() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Mutex::new(()));
    let store = ProviderStore::open_with_semantic(
        dir.path(),
        Box::new(HashEmbedder::new()),
        Box::new(GatedEmbedder {
            inner: HashEmbedder::new(),
            gate: Arc::clone(&gate),
        }),
    )
    .unwrap();
    let first = feed(&store, "A", &PARTITION_A);
    assert_eq!(caught_up(&store, "A").as_of, Some(first));

    let closed = gate.lock().unwrap();
    store.begin("A", 1, ProviderFeedMode::Delta).unwrap();
    put(
        &store,
        "A",
        "a1",
        "a/garden.md",
        "Nothing about plants any more.\n",
    );
    put(&store, "A", "a5", "a/new.md", "garden tomatoes in pots\n");
    store.checkpoint("A", 2).unwrap();
    let second = store.commit("A").unwrap();

    let answer = ask(
        &store,
        &["A"],
        "garden tomatoes",
        ProviderMode::Semantic,
        None,
        10,
    );
    assert_eq!(answer.coverage[0].state, ProviderState::Building);
    assert_eq!(answer.coverage[0].as_of, Some(first));
    let returned: Vec<_> = answer.refs.iter().map(|r| r.id.as_str()).collect();
    assert!(
        !returned.contains(&"a1"),
        "stale vectors of a1: {returned:?}"
    );
    assert!(!returned.contains(&"a5"), "a5 is not embedded yet");
    assert!(returned.contains(&"a4"));
    let status = store.status("A").unwrap();
    assert_eq!(
        status.semantic.as_ref().unwrap().state,
        ProviderState::Building
    );
    assert_eq!(status.semantic.as_ref().unwrap().pending_notes, 2);
    // Lexical is already at the new commit.
    assert_eq!(status.as_of, Some(second));

    // Hybrid reports both kinds per partition.
    let hybrid = ask(
        &store,
        &["A"],
        "garden tomatoes",
        ProviderMode::Hybrid,
        None,
        10,
    );
    assert_eq!(hybrid.mode, ProviderMode::Hybrid);
    let kinds: Vec<_> = hybrid
        .coverage
        .iter()
        .map(|c| (c.kind, c.state, c.as_of))
        .collect();
    assert_eq!(
        kinds,
        [
            (ProviderKind::Lexical, ProviderState::Ready, Some(second)),
            (ProviderKind::Semantic, ProviderState::Building, Some(first)),
        ]
    );
    assert!(
        hybrid.refs.iter().any(|r| r.id == "a5"),
        "lexical side sees a5"
    );
    let a1 = hybrid.refs.iter().find(|r| r.id == "a1");
    assert!(a1.is_none_or(|r| r.version == hex_sha256(b"Nothing about plants any more.\n")));

    drop(closed);
    assert_eq!(caught_up(&store, "A").as_of, Some(second));
    let answer = ask(
        &store,
        &["A"],
        "garden tomatoes",
        ProviderMode::Semantic,
        None,
        10,
    );
    assert_eq!(answer.coverage[0].state, ProviderState::Ready);
    let a5 = answer.refs.iter().find(|r| r.id == "a5").unwrap();
    assert_eq!(a5.version, hex_sha256(b"garden tomatoes in pots\n"));
    let a1 = answer.refs.iter().find(|r| r.id == "a1").unwrap();
    assert_eq!(a1.version, hex_sha256(b"Nothing about plants any more.\n"));

    // A delete leaves the vectors too.
    store.begin("A", 1, ProviderFeedMode::Delta).unwrap();
    store.delete("A", "a5").unwrap();
    store.checkpoint("A", 3).unwrap();
    store.commit("A").unwrap();
    caught_up(&store, "A");
    let answer = ordered(
        &store,
        &["A"],
        "garden tomatoes",
        ProviderMode::Semantic,
        10,
    );
    assert!(!answer.contains(&"A/a5".to_owned()));
    assert_eq!(answer.len(), 4);

    // A full resync replaces the vector file.
    let third = {
        store.begin("A", 2, ProviderFeedMode::Full).unwrap();
        put(&store, "A", "z1", "z.md", "garden tomatoes\n");
        store.checkpoint("A", 1).unwrap();
        store.commit("A").unwrap()
    };
    assert_eq!(caught_up(&store, "A").as_of, Some(third));
    assert_eq!(
        ordered(
            &store,
            &["A"],
            "garden tomatoes",
            ProviderMode::Semantic,
            10
        ),
        ["A/z1"]
    );
}
