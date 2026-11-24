//! Drives the experimental provider-helper mode over its Unix socket with a
//! minimal HTTP/1.1 client, following the search provider contract (DRAFT
//! v1) wire form: hello, full feed, commit, query, a delta edit and delete,
//! status, partition sets, refusals, retire.
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
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
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
