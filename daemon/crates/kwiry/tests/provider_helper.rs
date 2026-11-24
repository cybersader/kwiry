//! Drives the experimental provider-helper mode over its Unix socket with a
//! minimal HTTP/1.1 client, following the search provider contract (DRAFT
//! v1) wire form: hello, full feed, commit, query, a delta edit and delete,
//! status, partition sets, refusals, retire; plus recovery from an abandoned
//! delta, data-directory permissions and the streamed-body cap.
#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const PROFILE: (&str, &str) = ("Search-Provider-Profile", "v1");

struct Helper {
    child: Child,
    socket: PathBuf,
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(root: &Path) -> Helper {
    let run = root.join("run");
    std::fs::create_dir(&run).unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = run.join("provider.sock");
    let child = Command::new(env!("CARGO_BIN_EXE_kwiry"))
        .arg("--data-dir")
        .arg(root.join("data"))
        .arg("provider-helper")
        .arg("--socket")
        .arg(&socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let helper = Helper { child, socket };
    let deadline = Instant::now() + Duration::from_secs(30);
    while UnixStream::connect(&helper.socket).is_err() {
        assert!(Instant::now() < deadline, "helper did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    helper
}

struct Reply {
    status: u16,
    profile: Option<String>,
    body: Value,
}

/// Sends raw head + body on a fresh connection and parses the reply.
fn exchange(helper: &Helper, head: &str, body: &[u8]) -> Reply {
    let mut stream = UnixStream::connect(&helper.socket).unwrap();
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    read_reply(stream)
}

/// Reads a whole reply (the request asked for `Connection: close`). When the
/// helper answers before reading the whole body and closes, Linux reports
/// `ECONNRESET` after the reply bytes instead of end of file.
fn read_reply(mut stream: UnixStream) -> Reply {
    let mut response = Vec::new();
    if let Err(error) = stream.read_to_end(&mut response) {
        assert!(
            error.kind() == std::io::ErrorKind::ConnectionReset && !response.is_empty(),
            "{error}"
        );
    }
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response has a header block");
    let head = std::str::from_utf8(&response[..split]).unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let profile = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(PROFILE.0)
            .then(|| value.trim().to_owned())
    });
    let body = &response[split + 4..];
    let body = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body).unwrap()
    };
    Reply {
        status,
        profile,
        body,
    }
}

fn request(
    helper: &Helper,
    method: &str,
    target: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> Reply {
    let mut head = format!(
        "{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    exchange(helper, &head, body)
}

fn call(helper: &Helper, method: &str, target: &str, body: Option<Value>) -> (u16, Value) {
    let mut headers = vec![(PROFILE.0, PROFILE.1.to_owned())];
    let bytes = body.map(|body| body.to_string()).unwrap_or_default();
    if !bytes.is_empty() {
        headers.push(("Content-Type", "application/json".to_owned()));
    }
    let reply = request(helper, method, target, &headers, bytes.as_bytes());
    assert_eq!(
        reply.profile.as_deref(),
        Some("v1"),
        "profile header on {target}"
    );
    (reply.status, reply.body)
}

fn code(reply: &(u16, Value)) -> (u16, &str) {
    (reply.0, reply.1["error"]["code"].as_str().unwrap_or(""))
}

fn sha256(bytes: &[u8]) -> String {
    kwiry_core::hex_sha256(bytes)
}

fn put_with(
    helper: &Helper,
    p: &str,
    id: &str,
    path: &str,
    body: &str,
    version: &str,
) -> (u16, Value) {
    let reply = request(
        helper,
        "PUT",
        &format!("/v1/partitions/{p}/items/{id}"),
        &[
            (PROFILE.0, PROFILE.1.to_owned()),
            ("Content-Type", "application/octet-stream".to_owned()),
            ("Provider-Item-Version", version.to_owned()),
            ("Provider-Item-Path", path.to_owned()),
        ],
        body.as_bytes(),
    );
    (reply.status, reply.body)
}

fn put(helper: &Helper, p: &str, id: &str, path: &str, body: &str) {
    let reply = put_with(helper, p, id, path, body, &sha256(body.as_bytes()));
    assert_eq!(reply, (200, json!({})));
}

fn post(helper: &Helper, target: &str, body: Value) -> (u16, Value) {
    call(helper, "POST", target, Some(body))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn query(helper: &Helper, partitions: &[&str], text: &str) -> Value {
    let reply = post(
        helper,
        "/v1/query",
        json!({"partitions": partitions, "q": {"text": text, "mode": "lexical"},
               "terms": [text], "limit": 200, "deadlineMs": now_ms() + 10_000}),
    );
    assert_eq!(reply.0, 200, "{}", reply.1);
    reply.1
}

/// Referenced `partition/id` pairs, sorted: membership, not rank.
fn refs(answer: &Value) -> Vec<String> {
    let mut refs: Vec<_> = answer["refs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            format!(
                "{}/{}",
                r["partition"].as_str().unwrap(),
                r["id"].as_str().unwrap()
            )
        })
        .collect();
    refs.sort_unstable();
    refs
}

#[test]
fn provider_helper_serves_the_contract_over_a_unix_socket() {
    let root = tempfile::tempdir().unwrap();
    let helper = start(root.path());
    let mode = std::fs::metadata(&helper.socket)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    // The profile header is required on requests and present on replies.
    let bare = request(&helper, "GET", "/v1/hello", &[], b"");
    assert_eq!((bare.status, bare.profile.as_deref()), (400, Some("v1")));
    assert_eq!(bare.body, json!({"error": {"code": "bad_request"}}));

    let (status, hello) = call(&helper, "GET", "/v1/hello", None);
    assert_eq!(status, 200);
    assert_eq!(hello["contract"], "search-provider/1");
    assert_eq!(hello["capabilities"]["retrieval"], json!(["lexical"]));
    assert_eq!(hello["capabilities"]["maxLimit"], 200);
    assert_eq!(hello["capabilities"]["maxPartitions"], 64);
    assert_eq!(hello["capabilities"]["maxNoteBytes"], 10_485_760);

    // Unknown, then building.
    let unknown = query(&helper, &["p1"], "quokka");
    assert_eq!(
        unknown["coverage"],
        json!([{"partition": "p1", "mode": "lexical", "state": "unavailable", "asOf": null}])
    );
    let early = post(&helper, "/v1/partitions/p1/checkpoint", json!({"n": 1}));
    assert_eq!(code(&early), (409, "no_open_feed"));
    let begun = post(
        &helper,
        "/v1/partitions/p1/begin",
        json!({"epoch": 1, "mode": "full"}),
    );
    assert_eq!(begun, (200, json!({})));
    assert_eq!(
        query(&helper, &["p1"], "quokka")["coverage"][0]["state"],
        "building"
    );

    // Full feed, with refusals.
    let alpha = "# Alpha\n\nThe quokka habitat survey.\n";
    put(&helper, "p1", "id-a", "Areas/alpha.md", alpha);
    put(&helper, "p1", "id-b", "beta.md", "# Beta\n\nUnrelated.\n");
    put(
        &helper,
        "p1",
        "id-c",
        "caf%C3%A9.md",
        "---\n: : [\n---\nquokka body\n",
    );
    put(&helper, "p1", "id-d", "blob.bin", "quokka");
    let mismatch = put_with(&helper, "p1", "id-x", "x.md", "body", &sha256(b"other"));
    assert_eq!(code(&mismatch), (422, "version_mismatch"));
    let malformed = put_with(&helper, "p1", "id-x", "x.md", "body", "nothex");
    assert_eq!(code(&malformed), (400, "bad_request"));
    // Declared over the cap: refused from the header, before any body byte,
    // and counted as excluded.
    let head = format!(
        "PUT /v1/partitions/p1/items/id-big HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Search-Provider-Profile: v1\r\nContent-Length: 10485761\r\n\
         Provider-Item-Version: {}\r\nProvider-Item-Path: big.md\r\n\r\n",
        sha256(b"")
    );
    let big = exchange(&helper, &head, b"");
    assert_eq!(
        (big.status, big.body),
        (413, json!({"error": {"code": "too_large"}}))
    );

    let checkpointed = post(&helper, "/v1/partitions/p1/checkpoint", json!({"n": 1}));
    assert_eq!(checkpointed, (200, json!({})));
    let committed = post(&helper, "/v1/partitions/p1/commit", json!({}));
    assert_eq!(committed, (200, json!({"asOf": {"epoch": 1, "n": 1}})));
    let first = query(&helper, &["p1"], "quokka");
    assert_eq!(first["mode"], "lexical");
    assert_eq!(
        first["coverage"],
        json!([{"partition": "p1", "mode": "lexical", "state": "ready", "asOf": {"epoch": 1, "n": 1}}])
    );
    assert_eq!(refs(&first), ["p1/id-a", "p1/id-c"]);
    let alpha_ref = first["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "id-a")
        .unwrap();
    assert_eq!(
        alpha_ref,
        &json!({"partition": "p1", "id": "id-a", "version": sha256(alpha.as_bytes())})
    );

    // Delta: edit + delete, visible only after commit.
    let wrong_epoch = post(
        &helper,
        "/v1/partitions/p1/begin",
        json!({"epoch": 2, "mode": "delta"}),
    );
    assert_eq!(code(&wrong_epoch), (409, "epoch_mismatch"));
    let delta = post(
        &helper,
        "/v1/partitions/p1/begin",
        json!({"epoch": 1, "mode": "delta"}),
    );
    assert_eq!(delta.0, 200);
    put(
        &helper,
        "p1",
        "id-b",
        "beta.md",
        "# Beta\n\nquokka quokka quokka.\n",
    );
    let deleted = call(&helper, "DELETE", "/v1/partitions/p1/items/id-a", None);
    assert_eq!(deleted, (200, json!({})));
    let repeat = post(&helper, "/v1/partitions/p1/checkpoint", json!({"n": 1}));
    assert_eq!(code(&repeat), (409, "checkpoint_not_increasing"));
    assert_eq!(
        post(&helper, "/v1/partitions/p1/checkpoint", json!({"n": 2})).0,
        200
    );
    assert_eq!(
        refs(&query(&helper, &["p1"], "quokka")),
        ["p1/id-a", "p1/id-c"]
    );
    let committed = post(&helper, "/v1/partitions/p1/commit", json!({}));
    assert_eq!(committed, (200, json!({"asOf": {"epoch": 1, "n": 2}})));
    assert_eq!(
        refs(&query(&helper, &["p1"], "quokka")),
        ["p1/id-b", "p1/id-c"]
    );

    // status: counts by reason, out of band.
    let (status, report) = call(&helper, "GET", "/v1/partitions/p1/status", None);
    assert_eq!(status, 200);
    assert_eq!(report["state"], "ready");
    assert_eq!(report["asOf"], json!({"epoch": 1, "n": 2}));
    assert_eq!(report["notes"], 2);
    assert_eq!(
        report["excluded"],
        json!({"over_transport_cap": 1, "unsupported_format": 1})
    );
    assert_eq!(report["partial"], json!({"frontmatter_not_extracted": 1}));
    assert!(report["indexBytes"].as_u64().unwrap() > 0);

    // A partition set: the same path in two partitions stays two refs, and
    // coverage has one entry per partition.
    let p2 = post(
        &helper,
        "/v1/partitions/p2/begin",
        json!({"epoch": 7, "mode": "full"}),
    );
    assert_eq!(p2.0, 200);
    put(&helper, "p2", "id-b", "beta.md", "# Beta\n\nquokka.\n");
    assert_eq!(
        post(&helper, "/v1/partitions/p2/checkpoint", json!({"n": 1})).0,
        200
    );
    assert_eq!(post(&helper, "/v1/partitions/p2/commit", json!({})).0, 200);
    let set = query(&helper, &["p1", "p2", "p3"], "quokka");
    let coverage = set["coverage"].as_array().unwrap();
    assert_eq!(coverage.len(), 3);
    assert_eq!(coverage[1]["asOf"], json!({"epoch": 7, "n": 1}));
    assert_eq!(coverage[2]["state"], "unavailable");
    assert_eq!(refs(&set), ["p1/id-b", "p1/id-c", "p2/id-b"]);
    assert_eq!(refs(&query(&helper, &["p2"], "quokka")), ["p2/id-b"]);

    // Question shape: undeclared modes, unknown keys, bad limits, deadlines.
    let ask = |body: Value| post(&helper, "/v1/query", body);
    let semantic = ask(
        json!({"partitions": ["p1"], "q": {"text": "quokka", "mode": "semantic"}, "terms": [], "limit": 5}),
    );
    assert_eq!(code(&semantic), (400, "mode_unavailable"));
    let unknown_key = ask(
        json!({"partitions": ["p1"], "q": {"text": "quokka", "boost": 2}, "terms": [], "limit": 5}),
    );
    assert_eq!(code(&unknown_key), (400, "bad_request"));
    let too_many = ask(json!({"partitions": ["p1"], "q": {"text": "quokka"}, "limit": 201}));
    assert_eq!(code(&too_many), (400, "bad_request"));
    let twice = ask(json!({"partitions": ["p1", "p1"], "q": {"text": "quokka"}, "limit": 5}));
    assert_eq!(code(&twice), (400, "bad_request"));
    let past =
        ask(json!({"partitions": ["p1"], "q": {"text": "quokka"}, "limit": 5, "deadlineMs": 1}));
    assert_eq!(code(&past), (503, "unavailable"));
    assert_eq!(
        code(&call(&helper, "GET", "/v1/nowhere", None)),
        (404, "not_found")
    );
    assert_eq!(
        code(&call(
            &helper,
            "PATCH",
            "/v1/partitions/p1/items/id-b",
            None
        )),
        (405, "method_not_allowed")
    );

    // A full resync keeps serving the committed epoch until its commit, and
    // drops every id it did not receive.
    let resync = post(
        &helper,
        "/v1/partitions/p1/begin",
        json!({"epoch": 2, "mode": "full"}),
    );
    assert_eq!(resync.0, 200);
    put(&helper, "p1", "id-z", "zeta.md", "# Zeta\n\nquokka\n");
    assert_eq!(
        post(&helper, "/v1/partitions/p1/checkpoint", json!({"n": 1})).0,
        200
    );
    assert_eq!(
        query(&helper, &["p1"], "quokka")["coverage"][0]["asOf"],
        json!({"epoch": 1, "n": 2})
    );
    let committed = post(&helper, "/v1/partitions/p1/commit", json!({}));
    assert_eq!(committed, (200, json!({"asOf": {"epoch": 2, "n": 1}})));
    assert_eq!(refs(&query(&helper, &["p1"], "quokka")), ["p1/id-z"]);
    let regressed = post(
        &helper,
        "/v1/partitions/p1/begin",
        json!({"epoch": 1, "mode": "full"}),
    );
    assert_eq!(code(&regressed), (409, "epoch_regressed"));

    // retire
    assert_eq!(
        call(&helper, "DELETE", "/v1/partitions/p1", None),
        (200, json!({}))
    );
    assert_eq!(
        query(&helper, &["p1"], "quokka")["coverage"][0]["state"],
        "unavailable"
    );
    let gone = call(&helper, "DELETE", "/v1/partitions/p1/items/id-z", None);
    assert_eq!(code(&gone), (409, "no_open_feed"));
}

fn begin(helper: &Helper, p: &str, epoch: u64, mode: &str) -> (u16, Value) {
    post(
        helper,
        &format!("/v1/partitions/{p}/begin"),
        json!({"epoch": epoch, "mode": mode}),
    )
}

fn checkpoint(helper: &Helper, p: &str, n: u64) -> (u16, Value) {
    post(
        helper,
        &format!("/v1/partitions/{p}/checkpoint"),
        json!({"n": n}),
    )
}

fn commit(helper: &Helper, p: &str) -> (u16, Value) {
    post(helper, &format!("/v1/partitions/{p}/commit"), json!({}))
}

/// Review B1: a delta abandoned after a checkpoint must not leave its writes
/// behind, and the host's recovery (§2.2 rule 2: a `full` with the same
/// epoch, resending from the committed `n`) must be accepted.
#[test]
fn provider_helper_recovers_from_an_abandoned_delta_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let helper = start(root.path());

    // 1. Full feed, commit at {1,1}.
    assert_eq!(begin(&helper, "p1", 1, "full"), (200, json!({})));
    put(&helper, "p1", "id-a", "alpha.md", "# Alpha\n\nquokka\n");
    assert_eq!(checkpoint(&helper, "p1", 1), (200, json!({})));
    assert_eq!(
        commit(&helper, "p1"),
        (200, json!({"asOf": {"epoch": 1, "n": 1}}))
    );

    // 2. begin delta, put ghost, checkpoint 2.
    assert_eq!(begin(&helper, "p1", 1, "delta"), (200, json!({})));
    put(&helper, "p1", "ghost", "ghost.md", "# Ghost\n\nquokka\n");
    assert_eq!(checkpoint(&helper, "p1", 2), (200, json!({})));

    // 3. begin delta again: the abandoned checkpoint cannot be taken back, so
    // a delta is refused until a full resync commits.
    assert_eq!(
        code(&begin(&helper, "p1", 1, "delta")),
        (409, "epoch_mismatch")
    );
    // 4. The abandoned feed is closed; neither its checkpoint nor a commit
    // can bring the ghost back.
    assert_eq!(code(&checkpoint(&helper, "p1", 2)), (409, "no_open_feed"));
    // 5. No commit makes the ghost visible, and coverage and status still
    // report the last commit.
    assert_eq!(code(&commit(&helper, "p1")), (409, "no_open_feed"));
    let answer = query(&helper, &["p1"], "quokka");
    assert_eq!(refs(&answer), ["p1/id-a"]);
    assert_eq!(
        answer["coverage"][0],
        json!({"partition": "p1", "mode": "lexical", "state": "ready", "asOf": {"epoch": 1, "n": 1}})
    );
    let (_, report) = call(&helper, "GET", "/v1/partitions/p1/status", None);
    assert_eq!(report["asOf"], json!({"epoch": 1, "n": 1}));
    assert_eq!(report["notes"], 1);

    // Recovery: a full with the same epoch, resending checkpoint 2.
    assert_eq!(begin(&helper, "p1", 1, "full"), (200, json!({})));
    put(&helper, "p1", "id-a", "alpha.md", "# Alpha\n\nquokka\n");
    assert_eq!(checkpoint(&helper, "p1", 2), (200, json!({})));
    assert_eq!(
        commit(&helper, "p1"),
        (200, json!({"asOf": {"epoch": 1, "n": 2}}))
    );
    assert_eq!(refs(&query(&helper, &["p1"], "quokka")), ["p1/id-a"]);
    // Deltas work again afterwards.
    assert_eq!(begin(&helper, "p1", 1, "delta"), (200, json!({})));
    assert_eq!(checkpoint(&helper, "p1", 3), (200, json!({})));
    assert_eq!(
        commit(&helper, "p1"),
        (200, json!({"asOf": {"epoch": 1, "n": 3}}))
    );
}

/// The data directory is 0700 and nothing under it is readable by others.
#[test]
fn provider_helper_keeps_its_data_private() {
    let root = tempfile::tempdir().unwrap();
    let helper = start(root.path());
    assert_eq!(begin(&helper, "p1", 1, "full"), (200, json!({})));
    put(&helper, "p1", "id-a", "alpha.md", "# Alpha\n\nquokka\n");
    assert_eq!(checkpoint(&helper, "p1", 1), (200, json!({})));
    assert_eq!(commit(&helper, "p1").0, 200);

    let data = root.path().join("data");
    let mode = |path: &Path| {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&data), 0o700);
    let mut pending = vec![data];
    let mut files = 0;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                assert_eq!(mode(&path), 0o700, "{}", path.display());
                pending.push(path);
            } else {
                assert_eq!(mode(&path) & 0o077, 0, "{}", path.display());
                files += 1;
            }
        }
    }
    assert!(files > 2, "the index wrote files under the data directory");
}

/// Streams `chunks` chunked-encoding chunks of `chunk` bytes each, reading
/// the reply concurrently: the helper may answer and close before the body
/// ends, so write errors are expected and ignored.
fn put_chunked(helper: &Helper, p: &str, id: &str, chunk: usize, chunks: usize) -> Reply {
    let head = format!(
        "PUT /v1/partitions/{p}/items/{id} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Search-Provider-Profile: v1\r\nTransfer-Encoding: chunked\r\n\
         Content-Type: application/octet-stream\r\n\
         Provider-Item-Version: {}\r\nProvider-Item-Path: big.md\r\n\r\n",
        sha256(b"")
    );
    let stream = UnixStream::connect(&helper.socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    let sender = std::thread::spawn(move || {
        let data = vec![b'q'; chunk];
        let _ = (|| -> std::io::Result<()> {
            writer.write_all(head.as_bytes())?;
            for _ in 0..chunks {
                writer.write_all(format!("{chunk:x}\r\n").as_bytes())?;
                writer.write_all(&data)?;
                writer.write_all(b"\r\n")?;
            }
            writer.write_all(b"0\r\n\r\n")
        })();
    });
    let reply = read_reply(stream);
    sender.join().unwrap();
    reply
}

/// A body sent without a length is read only up to the 10 MiB cap; past it
/// the id is counted as excluded. With no open feed every over-cap put is
/// `no_open_feed`, like any other put.
#[test]
fn provider_helper_caps_a_chunked_body() {
    let root = tempfile::tempdir().unwrap();
    let helper = start(root.path());

    // No feed open: declared and streamed over-cap puts are no_open_feed.
    let head = format!(
        "PUT /v1/partitions/p1/items/id-big HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Search-Provider-Profile: v1\r\nContent-Length: 10485761\r\n\
         Provider-Item-Version: {}\r\nProvider-Item-Path: big.md\r\n\r\n",
        sha256(b"")
    );
    let declared = exchange(&helper, &head, b"");
    assert_eq!(
        (declared.status, declared.body),
        (409, json!({"error": {"code": "no_open_feed"}}))
    );
    let streamed = put_chunked(&helper, "p1", "id-big", 1 << 20, 11);
    assert_eq!(
        (streamed.status, streamed.body),
        (409, json!({"error": {"code": "no_open_feed"}}))
    );

    assert_eq!(begin(&helper, "p1", 1, "full"), (200, json!({})));
    put(&helper, "p1", "id-a", "alpha.md", "# Alpha\n\nquokka\n");
    // 11 MiB in 1 MiB chunks: stopped at the cap (LengthLimitError).
    let big = put_chunked(&helper, "p1", "id-big", 1 << 20, 11);
    assert_eq!(
        (big.status, big.profile.as_deref(), big.body),
        (413, Some("v1"), json!({"error": {"code": "too_large"}}))
    );
    assert_eq!(checkpoint(&helper, "p1", 1), (200, json!({})));
    assert_eq!(commit(&helper, "p1").0, 200);
    let (_, report) = call(&helper, "GET", "/v1/partitions/p1/status", None);
    assert_eq!(report["notes"], 1);
    assert_eq!(report["excluded"], json!({"over_transport_cap": 1}));
    assert_eq!(refs(&query(&helper, &["p1"], "quokka")), ["p1/id-a"]);
}
