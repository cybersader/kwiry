use super::{
    Daemon, VaultRegistration, format_daemon_config, format_release_token_time, fs,
    isolated_config, request_with_headers, save_config, sign_format_token, tempdir, wait_for,
};
use jsonschema::Validator;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const WIRE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../docs/wire/governed-search-v0"
);
const REGISTERED: (&str, &str) = ("VaultAaa_0123456789abc", "RoomAaaa_0123456789abc");
const MISSING: (&str, &str) = ("VaultBbb_0123456789abc", "RoomBbbb_0123456789abc");

fn validator(name: &str) -> (Value, Validator) {
    let schema: Value = serde_json::from_str(
        &fs::read_to_string(Path::new(WIRE).join(format!("{name}.schema.json"))).unwrap(),
    )
    .unwrap();
    let compiled = jsonschema::draft202012::new(&schema).unwrap();
    (schema, compiled)
}

fn validate(validator: &Validator, body: &Value, name: &str) {
    if let Err(error) = validator.validate(body) {
        panic!("{name}: {}: {error}\n{body:#}", error.instance_path);
    }
}

fn call(daemon: &Daemon, token: &str, method: &str, path: &str, body: &Value) -> Value {
    // A string fixture represents raw malformed JSON, not a JSON string on the wire.
    let raw = match body {
        Value::Null => None,
        Value::String(raw) => Some(raw.clone()),
        body => Some(body.to_string()),
    };
    let (status, response, headers) =
        request_with_headers(&daemon.address, token, method, path, raw.as_deref());
    let mut parsed_headers = serde_json::Map::new();
    for line in headers.lines().skip(1) {
        let (name, value) = line.split_once(':').unwrap();
        assert!(
            parsed_headers
                .insert(name.to_ascii_lowercase(), json!(value.trim()))
                .is_none()
        );
    }
    assert_eq!(parsed_headers["content-length"], response.len().to_string());
    let body: Value = serde_json::from_str(&response).unwrap_or_else(|error| {
        panic!("{method} {path} returned HTTP {status}, not a JSON body: {error}: {response}")
    });
    json!({"status": status, "headers": parsed_headers, "body": body})
}

fn normalize(fixture: &mut Value) {
    let headers = fixture["response"]["headers"].as_object_mut().unwrap();
    let date = headers.get_mut("date").unwrap();
    assert!(date.as_str().unwrap().ends_with(" GMT"));
    *date = json!("<date>");
    if let Some(generation) = headers.get_mut("x-kwiry-generation") {
        let digest = generation.as_str().unwrap().strip_prefix("v1-").unwrap();
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .bytes()
                .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch))
        );
        *generation = json!("<scoped-generation>");
    }
    let body = &mut fixture["response"]["body"];
    if let Some(fingerprint) = body.get_mut("extraction_policy_fingerprint") {
        // The schema checks the original string before this normalization.
        *fingerprint = json!("<extraction-policy-fingerprint>");
    }
}

#[test]
fn governed_search_wire_matches_deployed_fixtures() {
    let (_, request_schema) = validator("request");
    let (_, success_schema) = validator("success");
    let (errors, error_schema) = validator("error");
    let (_, health_schema) = validator("health");
    let fixture_root = tempdir().unwrap();
    let root = fixture_root.path();
    let config_path = isolated_config(root);
    let data = root.join("index-data");
    let room = root.join("room-notes");
    let grove = room.join("grove");
    fs::create_dir_all(&grove).unwrap();
    fs::write(
        grove.join("seedling.md"),
        "---\ntitle: Seedling ledger\ndescription: Synthetic botany notes\ntags: [botany]\nstatus: sample\ndate: '2026-01-02'\n---\n# Greenhouse\n## Trials\nMoonpetal survives in clay pots.\n",
    )
    .unwrap();
    fs::write(
        room.join("herbarium.base"),
        "views:\n  - type: table\n    name: Sunfern\n    order: [file.name]\n",
    )
    .unwrap();
    let mut config = format_daemon_config(config_path.parent().unwrap());
    config.indexing.max_stale_serve_seconds = 60;
    config.vaults.push(VaultRegistration {
        id: REGISTERED.0.into(),
        path: room,
        room: Some(REGISTERED.1.into()),
    });
    save_config(&config_path, &config).unwrap();
    let daemon = Daemon::start_openclast(&config_path, &data);
    let at = format_release_token_time(&data);
    let token =
        |jti, resources| sign_format_token(resources, false, Some("kwiry-cap+jwt"), jti, at);

    // Use the deployable binary, not the library's cfg(test) room-format override.
    let check = |name: &str,
                 method: &str,
                 path: &str,
                 auth: &str,
                 request: Value,
                 response: Value,
                 expected: (u16, Option<&str>)| {
        let (status, code) = expected;
        assert_eq!(response["status"], status, "{name}: {response:#}");
        let body = &response["body"];
        let body_schema = if let Some(code) = code {
            assert_eq!(body["error"]["code"], code, "{name}");
            let pairs = errors
                .pointer("/properties/error/properties/code/oneOf")
                .unwrap()
                .as_array()
                .unwrap();
            assert_eq!(
                pairs
                    .iter()
                    .filter(|pair| pair["x-http-status"] == status && pair["const"] == code)
                    .count(),
                1,
                "{name}: error pair not catalogued exactly once"
            );
            &error_schema
        } else if path == "/v0/health" {
            &health_schema
        } else {
            validate(&request_schema, &request, name);
            &success_schema
        };
        validate(body_schema, body, name);
        if path == "/v0/search" {
            assert_eq!(response["headers"]["kwiry-id-scheme"], "room-v1");
            assert_eq!(response["headers"]["kwiry-multi-room"], "closed");
        }
        let mut actual = json!({
            "request": {"method": method, "path": path, "authorization": auth, "body": request},
            "response": response,
        });
        let path = Path::new(WIRE)
            .join("fixtures")
            .join(format!("{name}.json"));
        if matches!(
            std::env::var("KWIRY_RECORD_GOVERNED_WIRE").as_deref(),
            Ok("1")
        ) {
            fs::write(path, format!("{actual:#}\n")).unwrap();
            return;
        }
        let mut expected: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        validate(body_schema, &expected["response"]["body"], name);
        normalize(&mut actual);
        normalize(&mut expected);
        assert_eq!(actual, expected, "{name}");
    };
    let post = |name, auth, signed: &str, body: Value, expected| {
        let response = call(&daemon, signed, "POST", "/v0/search", &body);
        check(name, "POST", "/v0/search", auth, body, response, expected);
    };
    let query = json!({"q": "moonpetal", "mode": "lexical"});
    check(
        "health",
        "GET",
        "/v0/health",
        "none",
        Value::Null,
        call(&daemon, "", "GET", "/v0/health", &Value::Null),
        (200, None),
    );
    post(
        "hits",
        "valid workload",
        &token(1, &[REGISTERED]),
        json!({"q": "in:body >body moonpetal", "mode": "lexical", "filters": {
             "vault_id": REGISTERED.0, "room": REGISTERED.1, "path_prefix": "grove/",
             "tags": ["botany"], "frontmatter_equals": {"status": "sample"}
         }, "limit": 1, "cursor": null}),
        (200, None),
    );
    post(
        "no-hits",
        "valid workload",
        &token(2, &[REGISTERED]),
        json!({"q": "rainshard", "mode": "lexical"}),
        (200, None),
    );
    post(
        "base-view",
        "valid workload",
        &token(16, &[REGISTERED]),
        json!({"q": "sunfern", "mode": "lexical"}),
        (200, None),
    );
    post(
        "empty-effective-set",
        "valid workload; signed partition not registered",
        &token(17, &[MISSING]),
        json!({"q": "moonpetal", "mode": "lexical", "filters": {"vault_id": REGISTERED.0}}),
        (200, None),
    );
    post(
        "positional-request",
        "valid workload",
        &token(18, &[REGISTERED]),
        json!(["rainshard", "lexical", [null, null, "grove/", ["botany"], {"status": "sample"}]]),
        (200, None),
    );

    let mut bad_signature = token(3, &[REGISTERED]);
    let signature_start = bad_signature.rfind('.').unwrap() + 1;
    let replacement = if bad_signature.as_bytes()[signature_start] == b'A' {
        "B"
    } else {
        "A"
    };
    bad_signature.replace_range(signature_start..signature_start + 1, replacement);
    post(
        "unauthorized",
        "bad signature",
        &bad_signature,
        query.clone(),
        (401, Some("unauthorized")),
    );
    post(
        "forbidden",
        "repeated signed vault ID",
        &token(4, &[REGISTERED, (REGISTERED.0, MISSING.1)]),
        query.clone(),
        (403, Some("forbidden")),
    );
    post(
        "resource-unavailable",
        "one of two signed resources has no partition",
        &token(5, &[REGISTERED, MISSING]),
        query.clone(),
        (503, Some("resource_unavailable")),
    );
    for (case, jti, body, status, code) in [
        (
            "mode-unavailable",
            6,
            json!({"q": "moonpetal", "mode": "semantic"}),
            501,
            "mode_unavailable",
        ),
        (
            "omitted-mode",
            7,
            json!({"q": "moonpetal"}),
            501,
            "mode_unavailable",
        ),
        ("malformed-body", 8, json!("{"), 400, "invalid_request"),
        (
            "invalid-query",
            9,
            json!({"q": " ", "mode": "lexical"}),
            400,
            "invalid_query",
        ),
        (
            "invalid-field-control",
            10,
            json!({"q": "in:unknown moonpetal", "mode": "lexical"}),
            400,
            "invalid_field_control",
        ),
        (
            "invalid-filter",
            11,
            json!({"q": "moonpetal", "mode": "lexical", "filters": {"tags": [""]}}),
            400,
            "invalid_filter",
        ),
        (
            "cursor-unavailable",
            12,
            json!({"q": "moonpetal", "mode": "lexical", "cursor": "example"}),
            501,
            "cursor_unavailable",
        ),
        (
            "limit-exceeded",
            13,
            json!({"q": "moonpetal", "mode": "lexical", "limit": 21}),
            403,
            "limit_exceeded",
        ),
    ] {
        post(
            case,
            "valid workload",
            &token(jti, &[REGISTERED]),
            body,
            (status, Some(code)),
        );
    }
    check(
        "method-not-allowed",
        "GET",
        "/v0/search",
        "valid workload",
        Value::Null,
        call(
            &daemon,
            &token(14, &[REGISTERED]),
            "GET",
            "/v0/search",
            &Value::Null,
        ),
        (405, Some("method_not_allowed")),
    );
    check(
        "search-method-unauthorized",
        "GET",
        "/v0/search",
        "none",
        Value::Null,
        call(&daemon, "", "GET", "/v0/search", &Value::Null),
        (401, Some("unauthorized")),
    );
    check(
        "health-method-not-allowed",
        "POST",
        "/v0/health",
        "none",
        Value::Null,
        call(&daemon, "", "POST", "/v0/health", &Value::Null),
        (405, Some("method_not_allowed")),
    );
    post(
        "replayed",
        "previously consumed workload token",
        &token(1, &[REGISTERED]),
        query.clone(),
        (401, Some("unauthorized")),
    );
    let epoch = fs::read_to_string(data.join("replay-epoch"))
        .unwrap()
        .parse()
        .unwrap();
    post(
        "startup-second",
        "otherwise valid workload minted in the startup epoch second",
        &sign_format_token(&[REGISTERED], false, Some("kwiry-cap+jwt"), 19, epoch),
        query.clone(),
        (401, Some("unauthorized")),
    );
    post(
        "invalid-query-syntax",
        "valid workload",
        &token(20, &[REGISTERED]),
        json!({"q": "\"moonpetal", "mode": "lexical"}),
        (400, Some("invalid_query")),
    );

    post(
        "explicit-boost",
        "valid workload",
        &token(21, &[REGISTERED]),
        json!({"q": "moonpetal^2", "mode": "lexical"}),
        (200, None),
    );
    post(
        "nonfinite-boost-score",
        "valid workload",
        &token(22, &[REGISTERED]),
        json!({"q": "moonpetal^1000000000000000000000000000000000000000", "mode": "lexical"}),
        (200, None),
    );

    // An unreadable subdirectory keeps the old complete result stale while the
    // room remains registered. This avoids depending on the length of a pass.
    let old_permissions = fs::metadata(&grove).unwrap().permissions();
    fs::set_permissions(&grove, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&grove).is_err() {
        let mut jti = 100;
        wait_for(|| {
            jti += 1;
            let response = call(
                &daemon,
                &token(jti, &[REGISTERED]),
                "POST",
                "/v0/search",
                &query,
            );
            if response["headers"]["x-kwiry-index-freshness"] != "stale; basis=strict_hash" {
                return false;
            }
            check(
                "stale",
                "POST",
                "/v0/search",
                "valid workload; unreadable note directory",
                query.clone(),
                response,
                (200, None),
            );
            true
        });
    } else {
        eprintln!("stale fixture skipped: this process can read a mode-000 directory");
    }
    fs::set_permissions(&grove, old_permissions).unwrap();

    // A new issued-at second needs a durable watermark write. A directory at
    // that destination gives a bounded failure wholly inside the temp data root.
    let watermark = data.join("replay-iat-watermark");
    assert_eq!(fs::read_to_string(&watermark).unwrap(), at.to_string());
    fs::remove_file(&watermark).unwrap();
    fs::create_dir(&watermark).unwrap();
    wait_for(|| std::time::UNIX_EPOCH.elapsed().unwrap().as_secs() > at);
    let now = std::time::UNIX_EPOCH.elapsed().unwrap().as_secs();
    let fresh = sign_format_token(&[REGISTERED], false, Some("kwiry-cap+jwt"), 15, now);
    post(
        "replay-state-unavailable",
        "valid workload; replay watermark cannot be written",
        &fresh,
        query,
        (503, Some("replay_state_unavailable")),
    );
    daemon.stop();
}
