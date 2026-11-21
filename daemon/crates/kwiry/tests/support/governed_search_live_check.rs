//! Capture the shipped executable, not the component-only open room-format override.
use super::*;
use jsonschema::Validator;
use serde_json::{Value, json};
use std::cell::Cell;
use std::path::PathBuf;

const WIRE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../docs/wire/governed-search-live-check-v1"
);
const R: (&str, &str) = ("AAAAAAAAAAAAAAAAAAAAAQ", "AAAAAAAAAAAAAAAAAAAAAg");
const OTHER: (&str, &str) = ("BBBBBBBBBBBBBBBBBBBBBQ", "BBBBBBBBBBBBBBBBBBBBBg");
const LABEL_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const LABEL_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const NOTE: &[u8] =
    b"\xef\xbb\xbf# Topaz\r\nCaf\xc3\xa9 moonflower.\r\n# Quartz\r\nA moonflower.\r\n";
const HASH: &str = "88c947992efa50845a81c41612c14b0df9dad3107b5bafadcf2c34d0715c278a";
const PROFILE: &str = "Kwiry-Profile: live-check-v1\r\n";
const MEDIA: &str = "Content-Type: application/json\r\n";
const MARKER: &str = "not-for-output-927";
const SEARCH: &str = "/v0/search";

fn sealed_note(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("blossom.md");
    fs::write(&path, NOTE).unwrap();
    let file = fs::File::open(path).unwrap();
    file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(946684800))
        .unwrap();
    file.set_permissions(fs::Permissions::from_mode(0o444))
        .unwrap();
}
struct Lab {
    _root: tempfile::TempDir,
    path: PathBuf,
    data: PathBuf,
    config: Config,
    address: String,
    daemon: Option<Daemon>,
    at: u64,
    jti: Cell<usize>,
}
impl Lab {
    fn new() -> Self {
        let root = tempdir().unwrap();
        let path = isolated_config(root.path());
        let data = root.path().join("index-data");
        let room = root.path().join("edition-a");
        fs::create_dir(&room).unwrap();
        sealed_note(&room);
        let mut config = format_daemon_config(path.parent().unwrap());
        config.server.profile_required = true;
        config.vaults.push(VaultRegistration {
            id: R.0.into(),
            room: Some(R.1.into()),
            path: room,
            snapshot_id: Some(LABEL_A.parse().unwrap()),
        });
        let mut lab = Self {
            _root: root,
            path,
            data,
            config,
            address: String::new(),
            daemon: None,
            at: 0,
            jti: Cell::new(0),
        };
        lab.start();
        lab
    }
    fn start(&mut self) {
        save_config(&self.path, &self.config).unwrap();
        self.daemon = Some(Daemon::start_custom(
            &self.path,
            &self.data,
            "kwiry OpenClast sidecar listening on http://",
            |cmd| {
                cmd.env("RUST_LOG", "debug");
            },
        ));
        self.address = self.daemon.as_ref().unwrap().address.clone();
        self.at = format_release_token_time(&self.data);
    }
    fn stop(&mut self) {
        self.daemon.take().unwrap().stop();
    }
    fn signed(&self, resources: &[(&str, &str)], ceiling: Value, at: u64) -> String {
        self.jti.set(self.jti.get() + 1);
        sign_format_token_with_limit(
            resources,
            false,
            Some("kwiry-cap+jwt"),
            self.jti.get(),
            at,
            ceiling,
        )
    }
    fn auth(&self, resources: &[(&str, &str)], limit: Value) -> String {
        format!(
            "Authorization: Bearer {}\r\n",
            self.signed(
                resources,
                limit,
                std::time::UNIX_EPOCH.elapsed().unwrap().as_secs()
            )
        )
    }
    fn raw(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Value {
        request_at(&self.address, method, path, headers, body)
    }
    fn post(&self, resources: &[(&str, &str)], profile: &str, media: &str, body: &[u8]) -> Value {
        self.raw(
            "POST",
            SEARCH,
            &format!("{}{profile}{media}", self.auth(resources, json!(20))),
            body,
        )
    }
    fn query(&self, q: &str) -> Value {
        self.post(&[R], PROFILE, MEDIA, &page(q, 20))
    }
}
fn request_at(address: &str, method: &str, path: &str, headers: &str, body: &[u8]) -> Value {
    let request = described_request(method, path, headers, body);
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    write!(stream,"{method} {path} HTTP/1.1\r\nHost: {address}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
    stream.write_all(body).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let mut headers = serde_json::Map::new();
    for line in lines {
        let (name, value) = line.split_once(':').unwrap();
        assert!(
            headers
                .insert(name.to_ascii_lowercase(), json!(value.trim()))
                .is_none()
        );
    }
    if method != "HEAD" {
        assert_eq!(headers["content-length"], body.len().to_string());
    }
    let body: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(body).unwrap()
    };
    json!({"request":request,"status":status,"headers":headers,"body":body})
}

// Keep framing and raw JSON (including duplicates and numeric spelling), but
// never put a compact JWS, replay identity or clock value in a fixture.
fn described_request(method: &str, path: &str, headers: &str, bytes: &[u8]) -> Value {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let headers=headers.lines().map(|line| {
        let (name,value)=line.split_once(':').unwrap();let value=value.trim();
        if name.eq_ignore_ascii_case("authorization") {
            let descriptor=value.strip_prefix("Bearer ").filter(|s| !s.contains(',')).and_then(|s| s.split('.').nth(1)).and_then(|s| URL_SAFE_NO_PAD.decode(s).ok()).and_then(|b|serde_json::from_slice::<Value>(&b).ok()).map(|claims|json!({"synthetic_workload":true,"resources":claims["resources"],"ceiling":claims["constraints"]["max_limit"]})).unwrap_or(json!("<malformed synthetic bearer>"));
            json!(["authorization",descriptor])
        }else{json!([name.to_ascii_lowercase(),value])}
    }).collect::<Vec<_>>();
    let body = if bytes.len() > 2 * 1024 * 1024 {
        assert!(bytes.iter().all(|b| *b == b' '));
        json!({"repeat_byte":32,"length":bytes.len()})
    } else if let Ok(text) = std::str::from_utf8(bytes) {
        json!({"utf8":text})
    } else {
        json!({"base64url":URL_SAFE_NO_PAD.encode(bytes)})
    };
    json!({"method":method,"path":path,"headers":headers,"body":body})
}
fn page(q: &str, limit: u16) -> Vec<u8> {
    serde_json::to_vec(&json!({"q":q,"limit":limit,"mode":"lexical"})).unwrap()
}

struct Capture {
    success: Validator,
    error: Validator,
    health: Validator,
    request: Validator,
    records: Vec<Value>,
}
impl Capture {
    fn new() -> Self {
        let schema = |name| {
            jsonschema::draft202012::new(
                &serde_json::from_slice(
                    &fs::read(Path::new(WIRE).join(format!("{name}.schema.json"))).unwrap(),
                )
                .unwrap(),
            )
            .unwrap()
        };
        Self {
            success: schema("success"),
            error: schema("error"),
            health: schema("health"),
            request: schema("request"),
            records: vec![],
        }
    }
    fn check(&mut self, name: &str, mut reply: Value, status: u16, code: &str, echo: bool) {
        let request = reply.as_object_mut().unwrap().remove("request").unwrap();
        assert_eq!(reply["status"], status, "{name}: {reply}");
        assert_eq!(reply["headers"]["cache-control"], "no-store", "{name}");
        assert_eq!(
            reply["headers"]["kwiry-profile"].as_str(),
            echo.then_some("live-check-v1"),
            "{name}"
        );
        let body = &reply["body"];
        if status == 200 && body.get("hits").is_some() {
            self.success.validate(body).unwrap();
            let h = &reply["headers"];
            assert_eq!(h["x-kwiry-lexical-profile"], "lexical-v2");
            assert_eq!(h["x-kwiry-field-scope"], "none");
            assert_eq!(h["x-kwiry-field-emphasis"], "none");
            assert!(
                [
                    "current; basis=strict_hash",
                    "stale; basis=strict_hash",
                    "reconciling; basis=strict_hash"
                ]
                .contains(&h["x-kwiry-index-freshness"].as_str().unwrap())
            );
            assert!(
                ["standard_only", "mixed", "partial_only", "none"]
                    .contains(&h["x-kwiry-lexical-match-quality"].as_str().unwrap())
            );
            let digest = h["x-kwiry-generation"]
                .as_str()
                .unwrap()
                .strip_prefix("v1-")
                .unwrap();
            assert!(
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            );
            assert_eq!(
                body["resource_snapshots"][0]["resource"],
                json!({"tenant_id":"tenant_AAAAAAAAAAAAAAAAAAAA","vault_id":R.0,"room_id":R.1})
            );
            self.request
                .validate(&serde_json::from_str(request["body"]["utf8"].as_str().unwrap()).unwrap())
                .unwrap();
        } else if status == 200 {
            if !body.is_null() {
                self.health.validate(body).unwrap();
            }
        } else {
            if status != 404 {
                self.error.validate(body).unwrap();
            }
            assert_eq!(body["error"]["code"], code, "{name}");
            assert!(!reply.to_string().contains(MARKER));
            assert!(
                reply["headers"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .all(|name| !name.starts_with("x-kwiry-"))
            );
            if status == 401 {
                assert_eq!(reply["headers"]["www-authenticate"], "Bearer");
            }
        }
        if name.starts_with("health") || status == 404 {
            assert!(reply["headers"].get("kwiry-id-scheme").is_none());
        } else {
            assert_eq!(reply["headers"]["kwiry-id-scheme"], "room-v1");
            assert_eq!(reply["headers"]["kwiry-multi-room"], "closed");
        }
        assert!(reply["headers"]["date"].as_str().unwrap().ends_with(" GMT"));
        self.records
            .push(json!({"case":name,"request":request,"response":reply}));
    }
    fn save(&mut self, family: &str) {
        let file = Path::new(WIRE)
            .join("fixtures")
            .join(format!("{family}.json"));
        let mut actual = Value::Array(std::mem::take(&mut self.records));
        if std::env::var("KWIRY_RECORD_LIVE_CHECK").as_deref() == Ok("1") {
            fs::write(file, format!("{actual:#}\n")).unwrap();
        } else {
            let mut expected: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
            for records in [&mut actual, &mut expected] {
                for record in records.as_array_mut().unwrap() {
                    let reply = &mut record["response"];
                    reply["headers"]["date"] = json!("<date>");
                    if reply["body"].get("hits").is_some() {
                        self.success.validate(&reply["body"]).unwrap();
                        reply["headers"]["x-kwiry-generation"] = json!("<scoped-generation>");
                        reply["body"]["extraction_policy_fingerprint"] =
                            json!("<extraction-policy-fingerprint>");
                    }
                }
            }
            assert_eq!(actual, expected, "{family}");
        }
    }
}

#[test]
fn governed_search_live_check_admission_and_queries() {
    let mut lab = Lab::new();
    let mut c = Capture::new();
    let body = page("moonflower", 20);
    c.request
        .validate(&serde_json::from_slice(&body).unwrap())
        .unwrap();
    let hit = lab.query("moonflower");
    let hits = hit["body"]["hits"].as_array().unwrap();
    assert!(hits.len() >= 2);
    assert!(hits.iter().all(|h| h["content_sha256"] == HASH));
    assert_eq!(hit["body"]["resource_snapshots"][0]["snapshot_id"], LABEL_A);
    c.check("hits", hit, 200, "", true);
    let zero = lab.query("zzzzzzzz");
    assert_eq!(zero["body"]["hits"], json!([]));
    assert_eq!(
        zero["body"]["resource_snapshots"][0]["snapshot_id"],
        LABEL_A
    );
    c.check("no-hits", zero, 200, "", true);
    c.save("success");
    for (name, method, path, status, code) in [
        ("health", "GET", "/v0/health", 200, ""),
        ("health-head", "HEAD", "/v0/health", 200, ""),
        (
            "health-method",
            "POST",
            "/v0/health",
            405,
            "method_not_allowed",
        ),
        ("off-route", "GET", "/v0/status", 404, "not_found"),
    ] {
        c.check(
            name,
            lab.raw(method, path, PROFILE, &[]),
            status,
            code,
            false,
        );
    }
    let auth = lab.auth(&[R], json!(20));
    let multi = lab.auth(&[R, OTHER], json!(20));
    for (name, method, headers, status, code, echo) in [
        (
            "auth-before-method",
            "GET",
            format!("Kwiry-Profile: {MARKER}\r\n"),
            401,
            "unauthorized",
            false,
        ),
        (
            "auth-before-negotiation",
            "POST",
            format!("Authorization: Bearer {MARKER}\r\nKwiry-Profile: unknown\r\n"),
            401,
            "unauthorized",
            false,
        ),
        (
            "duplicate-auth",
            "POST",
            format!("{auth}{auth}{PROFILE}"),
            401,
            "unauthorized",
            false,
        ),
        (
            "mixed-auth",
            "POST",
            format!("{auth}Authorization: Bearer invalid\r\n{PROFILE}"),
            401,
            "unauthorized",
            false,
        ),
        (
            "comma-auth",
            "POST",
            format!("{}, Bearer invalid\r\n{PROFILE}", auth.trim_end()),
            401,
            "unauthorized",
            false,
        ),
        (
            "method-before-negotiation",
            "GET",
            format!("{auth}Kwiry-Profile: unknown\r\n"),
            405,
            "method_not_allowed",
            false,
        ),
        (
            "replay-after-method",
            "POST",
            format!("{auth}{PROFILE}{MEDIA}"),
            401,
            "unauthorized",
            false,
        ),
        (
            "multi-before-selection",
            "POST",
            format!("{multi}Kwiry-Profile: unknown\r\n"),
            406,
            "profile_unsupported",
            false,
        ),
        (
            "multi-selected",
            "POST",
            format!("{}{PROFILE}", lab.auth(&[R, OTHER], json!(20))),
            403,
            "forbidden",
            true,
        ),
        (
            "signed-101",
            "POST",
            lab.auth(&[R], json!(101)),
            403,
            "forbidden",
            false,
        ),
        (
            "signed-noninteger",
            "POST",
            lab.auth(&[R], json!(20.0)),
            401,
            "unauthorized",
            false,
        ),
    ] {
        c.check(
            name,
            lab.raw(method, SEARCH, &headers, b"{"),
            status,
            code,
            echo,
        );
    }
    let epoch = fs::read_to_string(lab.data.join("replay-epoch"))
        .unwrap()
        .parse()
        .unwrap();
    let startup = lab.signed(&[R], json!(20), epoch);
    c.check(
        "startup-second",
        lab.raw(
            "POST",
            SEARCH,
            &format!("Authorization: Bearer {startup}\r\n{PROFILE}{MEDIA}"),
            &body,
        ),
        401,
        "unauthorized",
        false,
    );
    for (name, profile) in [
        ("absent-v0", ""),
        ("unknown", "Kwiry-Profile: LIVE-CHECK-V1\r\n"),
        (
            "duplicate",
            "Kwiry-Profile: live-check-v1\r\nKwiry-Profile: live-check-v1\r\n",
        ),
        (
            "mixed",
            "Kwiry-Profile: live-check-v1\r\nKwiry-Profile: unknown\r\n",
        ),
        ("comma", "Kwiry-Profile: live-check-v1, live-check-v1\r\n"),
    ] {
        c.check(
            name,
            lab.post(&[R], profile, MEDIA, &body),
            406,
            "profile_unsupported",
            false,
        );
    }
    for (name, media) in [
        ("absent-media", ""),
        ("suffix-media", "Content-Type: application/test+json\r\n"),
        (
            "parameter-media",
            "Content-Type: application/json; charset=utf-8\r\n",
        ),
        (
            "comma-media",
            "Content-Type: application/json, application/json\r\n",
        ),
        (
            "duplicate-media",
            "Content-Type: application/json\r\nContent-Type: application/json\r\n",
        ),
        (
            "mixed-media",
            "Content-Type: application/json\r\nContent-Type: text/plain\r\n",
        ),
    ] {
        c.check(
            name,
            lab.post(&[R], PROFILE, media, &body),
            400,
            "invalid_request",
            true,
        );
    }
    c.check(
        "case-insensitive-media",
        lab.post(
            &[R],
            PROFILE,
            "Content-Type: Application/JSON\r\n",
            &page("zzzzzzzz", 20),
        ),
        200,
        "",
        true,
    );
    for (name, b) in [
        ("malformed-json", b"{".as_slice()),
        ("invalid-utf8", b"{\xff}"),
        (
            "invalid-unicode",
            br#"{"q":"\ud800","mode":"lexical","limit":20}"#,
        ),
        (
            "duplicate-key",
            br#"{"q":"x","q":"y","mode":"lexical","limit":20}"#,
        ),
        ("positional", br#"["x","lexical",20]"#),
        ("no-mode", br#"{"q":"x","limit":20}"#),
        ("wrong-mode", br#"{"q":"x","mode":"semantic","limit":20}"#),
        ("no-limit", br#"{"q":"x","mode":"lexical"}"#),
        (
            "decimal-limit",
            br#"{"q":"x","mode":"lexical","limit":20.0}"#,
        ),
        (
            "exponent-limit",
            br#"{"q":"x","mode":"lexical","limit":2e1}"#,
        ),
        (
            "filter",
            br#"{"q":"x","mode":"lexical","limit":20,"filters":{}}"#,
        ),
        (
            "cursor",
            br#"{"q":"x","mode":"lexical","limit":20,"cursor":null}"#,
        ),
        (
            "unknown-control",
            br#"{"q":"x","mode":"lexical","limit":20,"not-for-output-927":false}"#,
        ),
    ] {
        c.check(
            name,
            lab.post(&[OTHER], PROFILE, MEDIA, b),
            400,
            "invalid_request",
            true,
        );
    }
    c.check(
        "oversize",
        lab.post(&[R], PROFILE, MEDIA, &vec![b' '; 2 * 1024 * 1024 + 1]),
        400,
        "invalid_request",
        true,
    );
    for (name, limit, status, code) in [
        ("zero-limit", 0, 400, "invalid_request"),
        ("outside-range", 101, 400, "invalid_request"),
        ("below-ceiling", 19, 400, "invalid_request"),
        ("above-ceiling", 21, 403, "limit_exceeded"),
    ] {
        c.check(
            name,
            lab.post(&[OTHER], PROFILE, MEDIA, &page("in:body", limit)),
            status,
            code,
            true,
        );
    }
    c.check(
        "query-before-resource",
        lab.post(&[OTHER], PROFILE, MEDIA, &page(MARKER, 20)),
        400,
        "invalid_query",
        true,
    );
    let missing = lab.post(&[OTHER], PROFILE, MEDIA, &body);
    assert_eq!(missing["body"]["error"]["resource_indexes"], json!([0]));
    c.check(
        "missing-partition",
        missing,
        503,
        "resource_unavailable",
        true,
    );
    c.save("admission");
    for (name, q) in [
        ("accent", "Café  audit"),
        ("combining", "e\u{301} ٢"),
        ("ext-i", "\u{2ebf0}"),
        ("literal-operator-prefix", "ANDromeda"),
        ("digits", "42"),
    ] {
        c.check(name, lab.query(q), 200, "", true);
    }
    for (name, q) in [
        ("empty", ""),
        ("leading", " x"),
        ("trailing", "x "),
        ("leading-mark", "\u{301}a"),
        ("nbsp", "x\u{a0}y"),
        ("tab", "x\ty"),
        ("line-feed", "x\ny"),
        ("and", "aNd"),
        ("or", "x OR y"),
        ("quotes", "\"x\""),
        ("punctuation", "x-y"),
        ("field", "in:body"),
        ("post-15.1", "\u{1c89}"),
    ] {
        c.check(name, lab.query(q), 400, "invalid_query", true);
    }
    for (n, status) in [(4096, 200), (4097, 400)] {
        c.check(
            &format!("bytes-{n}"),
            lab.query(&"a".repeat(n)),
            status,
            "invalid_query",
            true,
        );
    }
    for (n, status) in [(128, 200), (129, 400)] {
        for (name, q) in [
            ("repeated", vec!["alpha"; n].join(" ")),
            (
                "distinct",
                (0..n)
                    .map(|i| format!("word{i:03}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        ] {
            c.check(
                &format!("{name}-{n}"),
                lab.query(&q),
                status,
                "invalid_query",
                true,
            );
        }
    }
    c.save("queries");
    // Rejected reload: invalid serving configuration wins over parser, not authority.
    fs::write(&lab.path, "[invalid").unwrap();
    wait_for(|| lab.post(&[R], PROFILE, MEDIA, b"{")["status"] == 503);
    c.check(
        "global-before-parser",
        lab.post(&[R], PROFILE, MEDIA, b"{"),
        503,
        "resource_unavailable",
        true,
    );
    c.check(
        "authority-before-global",
        lab.post(&[R, OTHER], PROFILE, MEDIA, b"{"),
        403,
        "forbidden",
        true,
    );
    c.check(
        "selection-before-global",
        lab.post(&[R], "", MEDIA, b"{"),
        406,
        "profile_unsupported",
        false,
    );
    c.save("global-gate");
    let watermark = lab.data.join("replay-iat-watermark");
    let high: u64 = fs::read_to_string(&watermark).unwrap().parse().unwrap();
    fs::remove_file(&watermark).unwrap();
    fs::create_dir(&watermark).unwrap();
    wait_for(|| std::time::UNIX_EPOCH.elapsed().unwrap().as_secs() > high);
    let now = std::time::UNIX_EPOCH.elapsed().unwrap().as_secs();
    let s = lab.signed(&[R], json!(20), now);
    c.check(
        "replay-store-before-method",
        lab.raw(
            "GET",
            SEARCH,
            &format!("Authorization: Bearer {s}\r\n{PROFILE}"),
            &[],
        ),
        503,
        "replay_state_unavailable",
        false,
    );
    c.save("replay-state");
    lab.stop();
    for file in fs::read_dir(lab.data.join("logs")).unwrap() {
        let log = fs::read_to_string(file.unwrap().path()).unwrap();
        for forbidden in [MARKER, HASH, LABEL_A, "moonflower", "synthetic-request-"] {
            assert!(!log.contains(forbidden));
        }
    }
}

#[path = "live_check_publication.rs"]
mod publication;

#[path = "live_check_barrier.rs"]
mod barrier;

#[test]
fn governed_search_live_check_jwks_restart_rotation() {
    let mut lab = Lab::new();
    let mut c = Capture::new();
    let file = lab
        .config
        .auth
        .openclast
        .as_ref()
        .unwrap()
        .jwks_file
        .clone();
    let old: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let mut next = old["keys"][0].clone();
    next["kid"] = json!("ed02");
    next["x"] = json!("IVL40Zt5HSRFMkLhXy6rbLfP-ntqXtMAl5YOBpiB2xI");
    // Deterministic, public synthetic PKCS#8 test seed, never a deployed key.
    let mut second_key = PRIVATE_DER.to_owned();
    second_key[16..].fill(0x42);
    let atomically = |jwks: Value| {
        let stage = file.with_extension("new");
        fs::write(&stage, serde_json::to_vec(&jwks).unwrap()).unwrap();
        fs::rename(stage, &file).unwrap();
    };
    lab.stop();
    atomically(json!({"keys":[old["keys"][0],next]}));
    lab.config
        .auth
        .openclast
        .as_mut()
        .unwrap()
        .max_token_ttl_seconds = 3;
    lab.start();
    let query = |lab: &Lab, kid: &str, key: &[u8], jti: usize| {
        let now = std::time::UNIX_EPOCH.elapsed().unwrap().as_secs();
        let mut claims = format_claims(&[R], false, jti, now, json!(20));
        claims["exp"] = json!(now + 3);
        let token = sign_fixture_claims(&claims, Some("kwiry-cap+jwt"), kid, key);
        (
            lab.raw(
                "POST",
                SEARCH,
                &format!("Authorization: Bearer {token}\r\n{PROFILE}{MEDIA}"),
                &page("zzzzzzzz", 20),
            ),
            now + 3,
        )
    };
    let (overlap_old, expires) = query(&lab, "ed01", PRIVATE_DER, 1);
    c.check("overlap-old", overlap_old, 200, "", true);
    c.check(
        "overlap-new",
        query(&lab, "ed02", &second_key, 2).0,
        200,
        "",
        true,
    );
    lab.stop();
    wait_for(|| std::time::UNIX_EPOCH.elapsed().unwrap().as_secs() >= expires);
    atomically(json!({"keys":[next]}));
    lab.start();
    c.check(
        "only-new",
        query(&lab, "ed02", &second_key, 3).0,
        200,
        "",
        true,
    );
    c.check(
        "retired-kid",
        query(&lab, "ed01", PRIVATE_DER, 4).0,
        401,
        "unauthorized",
        false,
    );
    c.save("key-rollover");
    lab.stop();
}
