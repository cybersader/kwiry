#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin;
use kwiry_core::{
    ApiSearchResponse, Config, DaemonState, DaemonStatus, HostProfile, OpenClastAuthConfig,
    VaultRegistration, load_config, load_connection_descriptor, save_config,
};
use tempfile::tempdir;

#[cfg(target_os = "linux")]
#[path = "support/governed_search_wire.rs"]
mod governed_search_wire;

struct Daemon {
    child: Child,
    address: String,
    _stdout: BufReader<std::process::ChildStdout>,
}

impl Daemon {
    fn start(config: &Path, data: &Path) -> Self {
        Self::start_with_prefix(config, data, "kwiry listening on http://")
    }

    fn start_openclast(config: &Path, data: &Path) -> Self {
        Self::start_with_prefix(config, data, "kwiry OpenClast sidecar listening on http://")
    }

    fn start_with_prefix(config: &Path, data: &Path, prefix: &str) -> Self {
        Self::start_custom(config, data, prefix, |_| {})
    }

    fn start_custom(
        config: &Path,
        data: &Path,
        prefix: &str,
        customize: impl FnOnce(&mut Command),
    ) -> Self {
        Self::start_command(
            Command::new(cargo_bin("kwiry")),
            config,
            data,
            prefix,
            customize,
        )
    }

    fn start_command(
        mut command: Command,
        config: &Path,
        data: &Path,
        prefix: &str,
        customize: impl FnOnce(&mut Command),
    ) -> Self {
        command
            .arg("--config")
            .arg(config)
            .arg("--data-dir")
            .arg(data)
            .args(["serve", "--bind", "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        customize(&mut command);
        let mut child = command.spawn().unwrap();
        let output = child.stdout.take().unwrap();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let mut stdout = BufReader::new(output);
            let mut line = String::new();
            let result = stdout.read_line(&mut line).map(|_| (stdout, line));
            let _ = send.send(result);
        });
        let result = receive.recv_timeout(Duration::from_secs(30));
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        reader.join().unwrap();
        let (stdout, line) = result
            .expect("daemon did not print its startup result")
            .unwrap();
        assert!(line.starts_with(prefix), "{line}");
        let address = line
            .split("http://")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .trim()
            .to_owned();
        Self {
            child,
            address,
            _stdout: stdout,
        }
    }

    fn stop(mut self) {
        Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        let status = self.child.wait().unwrap();
        assert!(status.success(), "daemon exited with {status}");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn daemon_watches_files_reloads_config_and_reconciles_offline_changes() {
    let temporary = tempdir().unwrap();
    let config = temporary.path().join("config.toml");
    let data = temporary.path().join("data");
    let first_vault = temporary.path().join("first");
    let second_vault = temporary.path().join("second");
    fs::create_dir(&first_vault).unwrap();
    fs::create_dir(&second_vault).unwrap();
    let first_note = first_vault.join("note.md");
    fs::write(&first_note, "# First\ninitialterm").unwrap();

    vault_add(&config, &data, "first", &first_vault);
    let daemon = Daemon::start(&config, &data);
    let token_path = config.with_extension("token");
    let token = fs::read_to_string(&token_path).unwrap().trim().to_owned();
    let descriptor = load_connection_descriptor(&data.join("connection.json")).unwrap();
    assert_eq!(descriptor.url, format!("http://{}", daemon.address));
    assert_eq!(descriptor.token_file, token_path);
    assert_eq!(search(&daemon.address, &token, "initialterm").len(), 1);

    let valid_config = fs::read_to_string(&config).unwrap();
    fs::write(&config, "version = [\n").unwrap();
    wait_for(|| status(&daemon.address, &token).state == DaemonState::Degraded);
    fs::write(&config, valid_config).unwrap();
    wait_for(|| status(&daemon.address, &token).state == DaemonState::Ready);

    let mut restart_config = load_config(&config).unwrap();
    let original_bind = restart_config.server.bind.clone();
    restart_config.server.bind = "127.0.0.1:40000".into();
    save_config(&config, &restart_config).unwrap();
    wait_for(|| status(&daemon.address, &token).state == DaemonState::Degraded);
    assert_eq!(search(&daemon.address, &token, "initialterm").len(), 1);
    restart_config.server.bind = original_bind;
    save_config(&config, &restart_config).unwrap();
    wait_for(|| status(&daemon.address, &token).state == DaemonState::Ready);

    fs::write(&first_note, "# First\nliveupdatedterm").unwrap();
    wait_for(|| {
        let hits = search(&daemon.address, &token, "liveupdatedterm");
        hits.len() == 1 && hits[0].path == "note.md"
    });
    assert!(search(&daemon.address, &token, "initialterm").is_empty());

    fs::write(second_vault.join("second.md"), "# Second\naddedvaultterm").unwrap();
    vault_add(&config, &data, "second", &second_vault);
    wait_for(|| search(&daemon.address, &token, "addedvaultterm").len() == 1);

    let moved = first_vault.join("moved.md");
    fs::rename(&first_note, &moved).unwrap();
    wait_for(|| {
        let hits = search(&daemon.address, &token, "liveupdatedterm");
        hits.len() == 1 && hits[0].path == "moved.md"
    });
    fs::remove_file(&moved).unwrap();
    wait_for(|| search(&daemon.address, &token, "liveupdatedterm").is_empty());

    daemon.stop();
    let logs = fs::read_dir(data.join("logs"))
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(logs.contains("\"kwiry daemon ready\""), "{logs}");
    assert!(!logs.contains(&token));

    fs::write(second_vault.join("second.md"), "# Second\nofflineterm").unwrap();
    let restarted = Daemon::start(&config, &data);
    wait_for(|| search(&restarted.address, &token, "offlineterm").len() == 1);
    assert!(search(&restarted.address, &token, "addedvaultterm").is_empty());
    restarted.stop();
}

#[test]
fn desktop_search_errors_keep_detailed_diagnostics() {
    let temporary = tempdir().unwrap();
    let config = temporary.path().join("config.toml");
    let data = temporary.path().join("data");
    let vault = temporary.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("note.md"), "# Orchard\nSample notes.").unwrap();
    vault_add(&config, &data, "orchard", &vault);
    let daemon = Daemon::start(&config, &data);
    let token = fs::read_to_string(config.with_extension("token")).unwrap();
    let token = token.trim();
    wait_for(|| status(&daemon.address, token).state == DaemonState::Ready);

    for (body, code, diagnostic) in [
        (
            r#"{"q":"\"orchard","mode":"lexical"}"#,
            "invalid_query",
            "Syntax Error",
        ),
        (
            r#"{"q":"orchard","mode":"lexical","synthetic_option":true}"#,
            "invalid_request",
            "unknown field `synthetic_option`",
        ),
        (
            r#"{"q":"orchard","mode":"lexical","filters":{"frontmatter_equals":{"synthetic_key":"sample"}}}"#,
            "invalid_filter",
            "unsupported frontmatter filter: synthetic_key",
        ),
    ] {
        let (status, response) = request(&daemon.address, token, "POST", "/v0/search", Some(body));
        assert_eq!(status, 400, "{response}");
        let error: kwiry_core::ApiErrorEnvelope = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, code);
        assert!(error.error.message.contains(diagnostic), "{response}");
    }
    daemon.stop();
}

#[test]
fn daemon_watches_populated_subtree_lifecycle() {
    let temporary = tempdir().unwrap();
    let config = temporary.path().join("config.toml");
    let data = temporary.path().join("data");
    let vault = temporary.path().join("vault");
    let incoming = temporary.path().join("incoming");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("anchor.md"), "# Anchor\nanchorreadytoken").unwrap();
    vault_add(&config, &data, "orchard", &vault);
    let daemon = Daemon::start(&config, &data);
    let token = fs::read_to_string(config.with_extension("token"))
        .unwrap()
        .trim()
        .to_owned();
    wait_for(|| status(&daemon.address, &token).state == DaemonState::Ready);
    assert_eq!(search(&daemon.address, &token, "anchorreadytoken").len(), 1);

    // Populate outside the watched root, then deliver the subtree in one move.
    // Each eight-second assertion must converge before the 60-second safety pass.
    fs::create_dir_all(incoming.join("nested/deeper")).unwrap();
    fs::write(incoming.join("first.md"), "# First\norchardimporttoken").unwrap();
    fs::write(
        incoming.join("nested/deeper/second.md"),
        "# Second\norchardimporttoken",
    )
    .unwrap();
    fs::rename(&incoming, vault.join("grove")).unwrap();
    let paths = |query: &str| {
        let mut paths = search(&daemon.address, &token, query)
            .into_iter()
            .map(|hit| hit.path)
            .collect::<Vec<_>>();
        paths.sort();
        paths
    };
    wait_for(|| paths("orchardimporttoken") == ["grove/first.md", "grove/nested/deeper/second.md"]);
    fs::rename(vault.join("grove"), vault.join("orchard")).unwrap();
    wait_for(|| {
        paths("orchardimporttoken") == ["orchard/first.md", "orchard/nested/deeper/second.md"]
    });
    fs::remove_dir_all(vault.join("orchard")).unwrap();
    wait_for(|| paths("orchardimporttoken").is_empty());
    fs::create_dir_all(vault.join("orchard/nested/deeper")).unwrap();
    fs::write(
        vault.join("orchard/first.md"),
        "# Fresh first\norchardrebuilttoken",
    )
    .unwrap();
    fs::write(
        vault.join("orchard/nested/deeper/second.md"),
        "# Fresh second\norchardrebuilttoken",
    )
    .unwrap();
    wait_for(|| {
        paths("orchardrebuilttoken") == ["orchard/first.md", "orchard/nested/deeper/second.md"]
            && paths("orchardimporttoken").is_empty()
    });
    daemon.stop();
}

#[cfg_attr(
    not(target_os = "linux"),
    ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
)]
#[test]
fn openclast_startup_creates_no_desktop_credentials_or_descriptor() {
    let temporary = tempdir().unwrap();
    let config_path = isolated_config(temporary.path());
    let data = temporary.path().join("data");
    let vault = temporary.path().join("vault");
    let jwks_file = temporary.path().join("search.jwks.json");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("note.md"), "# Search\nopenclastprobe").unwrap();
    fs::write(
        &jwks_file,
        r#"{"keys":[{"kty":"OKP","crv":"Ed25519","x":"2-Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8","use":"sig","key_ops":["verify"],"alg":"EdDSA","kid":"ed01"}]}"#,
    )
    .unwrap();

    let mut config = Config::default();
    config.server.profile = HostProfile::OpenClast;
    config.semantic.enabled = false;
    config.auth.token_file = None;
    config.auth.openclast = Some(OpenClastAuthConfig {
        tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
        issuer: "issuer".into(),
        audience: "kwiry-search".into(),
        jwks_file,
        max_token_ttl_seconds: 60,
        workload_subject_allowlist: Vec::new(),
        legacy_identity_accept_until: None,
    });
    config.vaults = vec![VaultRegistration {
        id: "VaultOne_0123456789abc".into(),
        path: vault,
        room: Some("RoomAaaa_0123456789abc".into()),
    }];
    save_config(&config_path, &config).unwrap();

    let daemon = Daemon::start_openclast(&config_path, &data);
    assert!(!config_path.with_extension("token").exists());
    assert!(!data.join("connection.json").exists());
    assert_eq!(
        request(&daemon.address, "", "GET", "/v0/health", None),
        (200, FORMAT_HEALTH.to_owned()), // P17: built from an empty data root
    );
    let (status, _) = request(&daemon.address, "desktop-token", "GET", "/v0/status", None);
    assert_eq!(status, 404);
    daemon.stop();
    let daemon = Daemon::start_openclast(&config_path, &data);
    assert_eq!(
        request(&daemon.address, "", "GET", "/v0/health", None),
        (200, FORMAT_HEALTH.to_owned())
    ); // P17: pre-populated data root
    daemon.stop();

    let logs = fs::read_dir(data.join("logs"))
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(logs.contains("kwiry OpenClast sidecar ready"), "{logs}");
    assert!(!logs.contains("desktop-token"));
}

#[test]
fn vaultless_daemon_serves_health_and_typed_index_building_instead_of_exiting() {
    let temporary = tempdir().unwrap();
    let config = temporary.path().join("config.toml");
    let data = temporary.path().join("data");

    // No vaults registered: the index bootstrap fails, but the desktop
    // surface must come up degraded rather than the process exiting.
    let daemon = Daemon::start_with_prefix(&config, &data, "kwiry listening on http://");
    let token = fs::read_to_string(config.with_extension("token"))
        .unwrap()
        .trim()
        .to_owned();

    let (health_status, health_body) = request(&daemon.address, &token, "GET", "/v0/health", None);
    assert_eq!(health_status, 200, "{health_body}");

    let body = serde_json::json!({"q": "anything", "mode": "lexical", "limit": 20}).to_string();
    let (search_status, search_body) =
        request(&daemon.address, &token, "POST", "/v0/search", Some(&body));
    assert_eq!(search_status, 503, "{search_body}");
    assert!(search_body.contains("index_building"), "{search_body}");

    assert_eq!(status(&daemon.address, &token).state, DaemonState::Degraded);
    daemon.stop();
}

#[cfg_attr(
    not(target_os = "linux"),
    ignore = "secure OpenClast ingestion is Linux-only pending platform guidance"
)]
#[test]
fn format_release_p17_p19_n30_n32_n35_to_n39_and_governed_watcher() {
    let temp = tempdir().unwrap();
    let config_path = isolated_config(temp.path());
    let data = temp.path().join("data");
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    fs::write(a.join("note.md"), "# Visible\nvisibleprobe commonterm").unwrap();
    fs::write(b.join("note.md"), "# Hidden\noldhiddenprobe commonterm").unwrap();
    // N35–N39: powerless ignore controls, including a dot FIFO with no
    // writer and a dot symlink to another room. Both coverage and hits prove
    // the exclusions did not pass through the desktop walker.
    for path in [
        ".git/info",
        ".hidden",
        "target",
        "node_modules",
        "fifo",
        "link",
    ] {
        fs::create_dir_all(a.join(path)).unwrap();
    }
    for (path, bytes) in [
        (".gitignore", "*"),
        (".ignore", "note.md"),
        (".rgignore", "*.md"),
        (".git/info/exclude", "*"),
        (".hidden/note.md", "visibleprobe"),
        (".note.md", "visibleprobe"),
        ("target/t.md", "visibleprobe"),
        ("node_modules/n.md", "visibleprobe"),
        ("fifo/f.md", "visibleprobe"),
        ("link/l.md", "visibleprobe"),
    ] {
        fs::write(a.join(path), bytes).unwrap();
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let fifo =
            std::ffi::CString::new(a.join("fifo/.gitignore").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    }
    std::os::unix::fs::symlink(b.join("note.md"), a.join("link/.gitignore")).unwrap();
    fs::write(temp.path().join(".gitignore"), "*.md").unwrap();
    let home = temp.path().join("home");
    let xdg = home.join("config");
    fs::create_dir_all(xdg.join("git")).unwrap();
    let global_ignore = xdg.join("git/ignore");
    fs::write(&global_ignore, "*.md").unwrap();
    fs::write(
        home.join(".gitconfig"),
        format!("[core]\nexcludesFile = {}\n", global_ignore.display()),
    )
    .unwrap();
    let jwks_file = temp.path().join("jwks.json");
    fs::write(&jwks_file, r#"{"keys":[{"kty":"OKP","crv":"Ed25519","x":"2-Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8","use":"sig","key_ops":["verify"],"alg":"EdDSA","kid":"ed01"}]}"#).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut config = Config::default();
    config.server.profile = HostProfile::OpenClast;
    config.auth.openclast = Some(OpenClastAuthConfig {
        tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
        issuer: "issuer".into(),
        audience: "kwiry-search".into(),
        jwks_file,
        max_token_ttl_seconds: 60,
        workload_subject_allowlist: vec!["workload:synthetic-search".into()],
        legacy_identity_accept_until: Some(now + 300),
    });
    config.vaults = vec![
        VaultRegistration {
            id: "VaultOne_0123456789abc".into(),
            path: a,
            room: Some("RoomAaaa_0123456789abc".into()),
        },
        VaultRegistration {
            id: "VaultTwo_0123456789abc".into(),
            path: b.clone(),
            room: Some("RoomBbbb_0123456789abc".into()),
        },
    ];
    save_config(&config_path, &config).unwrap();
    let daemon = Daemon::start_custom(
        &config_path,
        &data,
        "kwiry OpenClast sidecar listening on http://",
        |process| {
            process
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", &xdg)
                .env_remove("GIT_CONFIG_GLOBAL");
        },
    );
    let epoch: u64 = fs::read_to_string(data.join("replay-epoch"))
        .unwrap()
        .parse()
        .unwrap();
    let clock = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    wait_for(|| clock() > epoch);
    let nonce = std::cell::Cell::new(0);
    let a_key = ("VaultOne_0123456789abc", "RoomAaaa_0123456789abc");
    let b_key = ("VaultTwo_0123456789abc", "RoomBbbb_0123456789abc");
    let sign = |keys: &[(&str, &str)], legacy: bool, typ: Option<&str>, jti: usize| {
        sign_format_token(keys, legacy, typ, jti, clock())
    };
    let token = |vault: &str, legacy: bool, typ: Option<&str>| {
        nonce.set(nonce.get() + 1);
        sign(
            &[if vault == "a" { a_key } else { b_key }],
            legacy,
            typ,
            nonce.get(),
        )
    };
    let workload = Some("kwiry-cap+jwt");
    let wire_search = |keys: &[(&str, &str)], jti, query, expected| {
        let body = serde_json::json!({"q": query, "mode": "lexical", "limit": 20}).to_string();
        let token = sign(keys, false, workload, jti);
        let (code, body, headers) =
            request_with_headers(&daemon.address, &token, "POST", "/v0/search", Some(&body));
        assert_eq!(code, expected, "{body}");
        assert_closed_room_headers(&headers);
        if expected == 200 {
            assert!(
                headers.contains("x-kwiry-index-freshness: current; basis=strict_hash"),
                "{headers}"
            );
        }
        serde_json::from_str::<serde_json::Value>(&body).unwrap()
    };
    assert_eq!(
        request(&daemon.address, "", "GET", "/v0/health", None),
        (200, FORMAT_HEALTH.to_owned())
    ); // P17: different config with two rooms
    // P19/P1: one room, then two vaults with one room each.
    assert_eq!(
        wire_search(&[a_key], 101, "visibleprobe", 200)["hits"][0]["room_id"],
        a_key.1
    );
    let both = wire_search(&[a_key, b_key], 102, "commonterm", 200);
    let rooms: std::collections::BTreeSet<_> = both["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["room_id"].as_str().unwrap())
        .collect();
    assert_eq!(rooms, [a_key.1, b_key.1].into());
    let visible = wire_search(&[a_key], 108, "visibleprobe", 200);
    let paths: std::collections::BTreeSet<_> = visible["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "note.md",
            "target/t.md",
            "node_modules/n.md",
            "fifo/f.md",
            "link/l.md"
        ]
        .into()
    );
    assert_eq!(
        wire_search(&[a_key], 109, "oldhiddenprobe", 200)["hits"],
        serde_json::json!([])
    );
    // N30: F1 wins over R17 for an otherwise valid token. Reuse the rejected
    // jti with a newly signed valid set to prove the refusal did not record it.
    let wrong_vault = (a_key.0, b_key.1);
    let refused = wire_search(&[a_key, wrong_vault], 103, "visibleprobe", 403);
    assert_eq!(refused["error"]["code"], "forbidden");
    assert!(refused.get("hits").is_none());
    assert_eq!(
        wire_search(&[a_key], 103, "visibleprobe", 200)["hits"][0]["room_id"],
        a_key.1
    );
    assert_eq!(
        wire_search(&[a_key], 104, "visibleprobe", 200)["hits"][0]["room_id"],
        a_key.1
    );
    // R17/N14: known room under the wrong vault is indistinguishable from
    // an unknown room. Refuse all hits and list every failing signed index.
    let unknown = (a_key.0, "RoomZzzz_0123456789abc");
    let wrong = wire_search(&[wrong_vault], 105, "visibleprobe", 503);
    assert_eq!(wrong, wire_search(&[unknown], 106, "visibleprobe", 503));
    assert_eq!(wrong["error"]["resource_indexes"], serde_json::json!([0]));
    assert!(wrong.get("hits").is_none());
    let partial = wire_search(
        &[unknown, b_key, ("VaultZzz_0123456789abc", a_key.1)],
        107,
        "oldhiddenprobe",
        503,
    );
    assert_eq!(
        partial["error"]["resource_indexes"],
        serde_json::json!([0, 2])
    );
    assert!(partial.get("hits").is_none());
    let legacy = token("a", true, None);
    let body = r#"{"q":"visibleprobe","mode":"lexical","limit":20}"#;
    for (legacy, typ) in [(false, None), (true, Some("openclast-holder-status+jwt"))] {
        let rejected = token("a", legacy, typ);
        let (code, _, headers) =
            request_with_headers(&daemon.address, &rejected, "POST", "/v0/search", Some(body));
        assert_eq!(code, 401);
        assert_closed_room_headers(&headers); // N32
    }
    let snapshot = |token: &str| {
        let (code, body, headers) =
            request_with_headers(&daemon.address, token, "POST", "/v0/search", Some(body));
        assert_eq!(code, 200, "{body}");
        let header = |name: &str| {
            headers
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .unwrap()
                .trim()
                .to_owned()
        };
        (
            header("x-kwiry-generation:"),
            header("x-kwiry-index-freshness:"),
            body,
        )
    };
    let baseline = snapshot(&legacy);
    assert!(baseline.0.starts_with("v1-"));
    assert_eq!(baseline.1, "current; basis=strict_hash");
    assert_eq!(
        snapshot(&token("a", false, Some("kwiry-cap+jwt"))),
        baseline
    );
    fs::write(b.join("note.md"), "# Hidden changed\nnewhiddenprobe").unwrap();
    wait_for(|| {
        // Sample A while the actual notify watcher dirties and reconciles B.
        assert_eq!(snapshot(&legacy), baseline);
        !search(
            &daemon.address,
            &token("b", false, Some("kwiry-cap+jwt")),
            "newhiddenprobe",
        )
        .is_empty()
    });
    assert_eq!(
        snapshot(&token("a", false, Some("kwiry-cap+jwt"))),
        baseline
    );
    // A failed bearer also must not reach diagnostics with its token contents.
    let (code, _) = request(
        &daemon.address,
        "synthetic-rejected-token",
        "POST",
        "/v0/search",
        Some(body),
    );
    assert_eq!(code, 401);
    daemon.stop();
    let logs = fs::read_dir(data.join("logs"))
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(
        logs.contains("OpenClast search enforced"),
        "expected captured audit output"
    );
    for forbidden in [
        "synthetic-user-sentinel",
        "synthetic-actor-sentinel",
        "synthetic-identity-revision",
        "workload:synthetic-search",
        "synthetic-rejected-token",
        "\"subject\"",
        "\"actor\"",
        "subject_revision",
    ] {
        assert!(
            !logs.contains(forbidden),
            "captured logs retained {forbidden}"
        );
    }
}

const PRIVATE_DER: &[u8] = &[
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
    0x6a, 0xc3, 0xfd, 0xee, 0xee, 0x29, 0x8a, 0x92, 0x63, 0x8b, 0x70, 0x0c, 0x4b, 0x11, 0x7c, 0xc3,
    0x2e, 0x2d, 0x2a, 0xce, 0x0d, 0xfd, 0x78, 0x76, 0x94, 0xe2, 0x4c, 0xae, 0x8a, 0xd5, 0x82, 0x34,
];
fn sign_format_token(
    keys: &[(&str, &str)],
    legacy: bool,
    typ: Option<&str>,
    jti: usize,
    now: u64,
) -> String {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    let resources: Vec<_> = keys
        .iter()
        .map(|(vault, room)| {
            serde_json::json!({
                "tenant_id": "tenant_AAAAAAAAAAAAAAAAAAAA", "vault_id": vault, "room_id": room,
            })
        })
        .collect();
    let mut claims = serde_json::json!({
        "iss": "issuer", "aud": "kwiry-search", "sub": "workload:synthetic-search",
        "jti": format!("synthetic-request-{jti}"), "iat": now, "nbf": now, "exp": now + 60,
        "tenant": "tenant_AAAAAAAAAAAAAAAAAAAA", "actions": ["search:lexical"],
        "resources": resources,
        "constraints": {"max_limit": 20}
    });
    if legacy {
        claims["sub"] = serde_json::json!("synthetic-user-sentinel");
        claims["actor"] = serde_json::json!("synthetic-actor-sentinel");
        claims["subject_revision"] = serde_json::json!("synthetic-identity-revision");
    }
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some("ed01".into());
    header.typ = typ.map(str::to_owned);
    encode(&header, &claims, &EncodingKey::from_ed_der(PRIVATE_DER)).unwrap()
}

fn format_daemon_config(root: &Path) -> Config {
    let mut config = Config::default();
    config.server.profile = HostProfile::OpenClast;
    let jwks_file = root.join("jwks.json");
    fs::write(&jwks_file, r#"{"keys":[{"kty":"OKP","crv":"Ed25519","x":"2-Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8","use":"sig","key_ops":["verify"],"alg":"EdDSA","kid":"ed01"}]}"#).unwrap();
    config.auth.openclast = Some(OpenClastAuthConfig {
        tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
        issuer: "issuer".into(),
        audience: "kwiry-search".into(),
        jwks_file,
        max_token_ttl_seconds: 60,
        workload_subject_allowlist: vec!["workload:synthetic-search".into()],
        legacy_identity_accept_until: Some(0),
    });
    config
}

fn isolated_config(root: &Path) -> std::path::PathBuf {
    let control = root.join("control");
    fs::create_dir_all(&control).unwrap();
    control.join("config.toml")
}

#[cfg(target_os = "linux")]
#[test]
fn format_release_fr1_fr2_ignores_legacy_file_and_never_writes_it() {
    let fixture = tempdir().unwrap();
    let root = fixture.path();
    let path = isolated_config(root);
    let data = root.join("data");
    let room = root.join("room");
    fs::create_dir(&room).unwrap();
    fs::write(room.join("index.md"), "synthetic pinecone").unwrap();
    save_config(&path, &format_daemon_config(root)).unwrap();
    let vault_id = "VaultOne_0123456789abc";
    let room_id = "RoomAaaa_0123456789abc";
    let output = Command::new(cargo_bin("kwiry"))
        .arg("--config")
        .arg(&path)
        .arg("--data-dir")
        .arg(&data)
        .args([
            "vault", "add", "--id", vault_id, "--room", room_id, "--path",
        ])
        .arg(&room)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let legacy = path.with_extension("room-roots.json");
    assert!(!legacy.exists());
    assert!(!data.exists());
    let wrong = br#"{"RoomAaaa_0123456789abc":"0:0"}"#;
    fs::write(&legacy, wrong).unwrap();
    let trace = root.join("syscalls.log");
    let trace_enabled = std::env::var_os("KWIRY_FR1_STRACE").is_some();
    let command = if trace_enabled {
        let mut command = Command::new("strace");
        // Keep the tracee a direct child so the normal daemon cleanup owns it.
        command
            .args(["-D", "-f", "-e", "trace=%file", "-o"])
            .arg(&trace)
            .arg(cargo_bin("kwiry"));
        command
    } else {
        Command::new(cargo_bin("kwiry"))
    };
    let daemon = Daemon::start_command(
        command,
        &path,
        &data,
        "kwiry OpenClast sidecar listening on http://",
        |_| {},
    );
    let clock = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    let epoch: u64 = fs::read_to_string(data.join("replay-epoch"))
        .unwrap()
        .parse()
        .unwrap();
    wait_for(|| clock() > epoch);
    let token = sign_format_token(
        &[(vault_id, room_id)],
        false,
        Some("kwiry-cap+jwt"),
        1,
        clock(),
    );
    assert_eq!(search(&daemon.address, &token, "pinecone").len(), 1);
    daemon.stop();
    if trace_enabled {
        let calls = fs::read_to_string(trace).unwrap();
        assert!(
            calls.contains("config.toml"),
            "trace must observe actual config access"
        );
        assert!(
            !calls.contains("room-roots.json"),
            "legacy sidecar was accessed"
        );
    }
    assert_eq!(fs::read(&legacy).unwrap(), wrong);
    fs::remove_file(&legacy).unwrap();
    Daemon::start_openclast(&path, &data).stop();
    assert!(!legacy.exists());
    let mut entries: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort_unstable();
    assert_eq!(entries, ["config.lock", "config.toml"]);
}

#[cfg(target_os = "linux")]
#[test]
fn format_release_fr3_refuses_state_overlap_in_both_directions() {
    use std::os::unix::fs::symlink;
    for case in [
        "data-inside",
        "data-containing",
        "config-inside",
        "config-containing",
        "token-inside",
        "token-containing",
    ] {
        let fixture = tempdir().unwrap();
        let root = fixture.path();
        let mut path = isolated_config(root);
        let mut data = root.join("data");
        let mut room = root.join("room");
        match case {
            "data-inside" => data = room.join("data"),
            "data-containing" => room = data.join("room"),
            "config-inside" => path = room.join("control/config.toml"),
            "config-containing" => room = path.parent().unwrap().join("room"),
            "token-containing" => room = root.join("token-dir/room"),
            _ => (),
        }
        fs::create_dir_all(&room).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        match case {
            "token-inside" => {
                fs::write(room.join("token"), "synthetic").unwrap();
                symlink(room.join("token"), path.with_extension("token")).unwrap();
            }
            "token-containing" => {
                symlink(root.join("token-dir"), path.with_extension("token")).unwrap()
            }
            _ => (),
        }
        let mut config = format_daemon_config(root);
        config.vaults.push(VaultRegistration {
            id: "VaultOne_0123456789abc".into(),
            path: room,
            room: Some("RoomAaaa_0123456789abc".into()),
        });
        fs::write(&path, toml::to_string_pretty(&config).unwrap()).unwrap();
        let before = fs::read(&path).unwrap();
        let paths = kwiry_core::Paths::resolve(Some(path.clone()), Some(data.clone())).unwrap();
        assert!(
            kwiry_core::load_config_for_paths(&paths)
                .unwrap_err()
                .to_string()
                .contains("openclast_room_boundary"),
            "{case}"
        );
        assert_startup_refused(&path, &data, "openclast_room_boundary", |_| {});
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!data.join("daemon.lock").exists());
        assert!(!path.with_extension("lock").exists());
        if case != "data-containing" {
            assert!(!data.exists());
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn format_release_fr7_refuses_stale_cap_beyond_one_day() {
    let fixture = tempdir().unwrap();
    let root = fixture.path();
    let path = isolated_config(root);
    let data = root.join("data");
    let mut config = format_daemon_config(root);
    for seconds in [0, 86_400] {
        config.indexing.max_stale_serve_seconds = seconds;
        save_config(&path, &config).unwrap();
        assert_eq!(
            load_config(&path).unwrap().indexing.max_stale_serve_seconds,
            seconds
        );
    }
    config.indexing.max_stale_serve_seconds = 86_401;
    fs::write(&path, toml::to_string_pretty(&config).unwrap()).unwrap();
    assert_startup_refused(
        &path,
        &data,
        "indexing.max_stale_serve_seconds must be between 0 and 86400",
        |_| {},
    );
    assert!(!data.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn format_release_fr11_http_refuses_ingestion_and_boot_uses_guarded_walker() {
    let fixture = tempdir().unwrap();
    let root = fixture.path();
    let path = isolated_config(root);
    let data = root.join("data");
    let room = root.join("room");
    fs::create_dir(&room).unwrap();
    fs::write(room.join("ordinary.md"), "ordinary fixture").unwrap();
    fs::write(room.join("linked.md"), "synthetic restricted fixture").unwrap();
    fs::hard_link(room.join("linked.md"), room.join("alias.md")).unwrap();
    let vault_id = "VaultAaa_0123456789abc";
    let room_id = "RoomAaaa_0123456789abc";
    let mut config = format_daemon_config(root);
    config.vaults.push(VaultRegistration {
        id: vault_id.into(),
        path: room,
        room: Some(room_id.into()),
    });
    save_config(&path, &config).unwrap();
    let daemon = Daemon::start_openclast(&path, &data);
    let at = format_release_token_time(&data);
    let token = |jti| {
        sign_format_token(
            &[(vault_id, room_id)],
            false,
            Some("kwiry-cap+jwt"),
            jti,
            at,
        )
    };
    assert_eq!(
        request(&daemon.address, &token(1), "POST", "/v0/index", Some("{}")).0,
        404
    );
    assert_eq!(search(&daemon.address, &token(2), "ordinary").len(), 1);
    assert!(search(&daemon.address, &token(3), "restricted").is_empty());
    daemon.stop();
}

#[cfg(target_os = "linux")]
#[test]
fn format_release_fr12_http_reports_all_missing_signed_indexes() {
    let fixture = tempdir().unwrap();
    let root = fixture.path();
    let path = isolated_config(root);
    let data = root.join("data");
    let room = root.join("room");
    fs::create_dir(&room).unwrap();
    fs::write(room.join("index.md"), "synthetic middle witness").unwrap();
    let signed = [
        ("VaultAaa_0123456789abc", "RoomAaaa_0123456789abc"),
        ("VaultBbb_0123456789abc", "RoomBbbb_0123456789abc"),
        ("VaultCcc_0123456789abc", "RoomCccc_0123456789abc"),
    ];
    let mut config = format_daemon_config(root);
    config.vaults.push(VaultRegistration {
        id: signed[1].0.into(),
        path: room,
        room: Some(signed[1].1.into()),
    });
    save_config(&path, &config).unwrap();
    let daemon = Daemon::start_openclast(&path, &data);
    let at = format_release_token_time(&data);
    let middle = sign_format_token(&signed[1..2], false, Some("kwiry-cap+jwt"), 1, at);
    assert_eq!(search(&daemon.address, &middle, "witness").len(), 1);
    let all = sign_format_token(&signed, false, Some("kwiry-cap+jwt"), 2, at);
    let (status, body) = request(
        &daemon.address,
        &all,
        "POST",
        "/v0/search",
        Some(r#"{"q":"witness","mode":"lexical","limit":20}"#),
    );
    assert_eq!(status, 503, "{body}");
    assert_eq!(
        body,
        r#"{"error":{"code":"resource_unavailable","message":"an authorized resource is not available for search","resource_indexes":[0,2]}}"#
    );
    daemon.stop();
}

#[cfg(target_os = "linux")]
fn format_release_token_time(data: &Path) -> u64 {
    let clock = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    let epoch: u64 = fs::read_to_string(data.join("replay-epoch"))
        .unwrap()
        .parse()
        .unwrap();
    wait_for(|| clock() > epoch);
    clock()
}

fn assert_startup_refused(
    config: &Path,
    data: &Path,
    expected: &str,
    customize: impl FnOnce(&mut Command),
) {
    let file = tempfile::tempfile().unwrap();
    let mut command = Command::new(cargo_bin("kwiry"));
    command
        .arg("--config")
        .arg(config)
        .arg("--data-dir")
        .arg(data)
        .args(["serve", "--bind", "127.0.0.1:0"])
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .stderr(Stdio::from(file.try_clone().unwrap()));
    customize(&mut command);
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("daemon did not refuse startup: {expected}");
        }
        thread::sleep(Duration::from_millis(20));
    };
    use std::io::{Seek, SeekFrom};
    let mut file = file;
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut output = String::new();
    file.read_to_string(&mut output).unwrap();
    assert!(
        !status.success() && output.contains(expected),
        "{status}: {output}"
    );
    assert!(!output.contains("listening"), "{output}");
    assert!(
        !data.join("logs").exists(),
        "refusal reached daemon logging: {output}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn format_release_n31_rb3_refuses_admission_registration() {
    let fixture = tempdir().unwrap();
    let root = fixture.path();
    let path = isolated_config(root);
    let data = root.join("data");
    let mut config = format_daemon_config(root);
    for (name, room) in [
        ("a", "RoomAaaa_0123456789abc"),
        ("b", "RoomBbbb_0123456789abc"),
    ] {
        let room_root = root.join(name);
        fs::create_dir(&room_root).unwrap();
        fs::write(room_root.join("index.md"), "synthetic rollback fixture").unwrap();
        config.vaults.push(VaultRegistration {
            id: "VaultOne_0123456789abc".into(),
            path: room_root,
            room: Some(room.into()),
        });
    }
    fs::write(&path, toml::to_string_pretty(&config).unwrap()).unwrap();
    let expected_error = kwiry_core::Error::DuplicateVaultId(config.vaults[0].id.clone());
    assert!(matches!(
        load_config(&path),
        Err(kwiry_core::Error::DuplicateVaultId(_))
    ));
    assert_startup_refused(&path, &data, &expected_error.to_string(), |_| {}); // N31, RB3
    let b = config.vaults.pop().unwrap();
    save_config(&path, &config).unwrap();
    config = kwiry_core::load_config(&path).unwrap();
    let before = config.clone();
    assert!(matches!(
        kwiry_core::add_vault(&mut config, b.id.clone(), b.path.clone(), b.room.clone()),
        Err(kwiry_core::Error::DuplicateVaultId(_))
    ));
    assert!(kwiry_core::ensure_vault_registration(&mut config, b.id, b.path, b.room).is_err());
    assert_eq!(before, config);
    let daemon = Daemon::start_openclast(&path, &data);
    assert_eq!(
        request(&daemon.address, "", "GET", "/v0/health", None),
        (200, FORMAT_HEALTH.to_owned())
    );
    let clock = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    let epoch: u64 = fs::read_to_string(data.join("replay-epoch"))
        .unwrap()
        .parse()
        .unwrap();
    wait_for(|| clock() > epoch);
    let token = sign_format_token(
        &[("VaultOne_0123456789abc", "RoomAaaa_0123456789abc")],
        false,
        Some("kwiry-cap+jwt"),
        1,
        clock(),
    );
    let hits = search(&daemon.address, &token, "synthetic");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].room_id.as_deref(), Some("RoomAaaa_0123456789abc"));
    daemon.stop();
}

#[cfg(not(target_os = "linux"))]
#[test]
fn n33_non_linux_rejects_openclast_before_data_root_but_serves_desktop() {
    let fixture = tempdir().unwrap();
    let config = fixture.path().join("kwiry.toml");
    let data = fixture.path().join("data");
    save_config(&config, &format_daemon_config(fixture.path())).unwrap();
    assert_startup_refused(
        &config,
        &data,
        "openclast_ingestion_platform_unsupported",
        |_| {},
    );
    assert!(!data.exists());
    save_config(&config, &Config::default()).unwrap();
    let daemon = Daemon::start_with_prefix(&config, &data, "kwiry listening on http://");
    assert_eq!(
        request(&daemon.address, "", "GET", "/v0/health", None),
        (200, r#"{"status":"ok"}"#.to_owned())
    );
    daemon.stop();
}

#[cfg(target_os = "linux")]
#[test]
fn n34_openat2_probe_refuses_all_errors_even_with_no_rooms() {
    use std::os::unix::process::CommandExt;
    let fixture = tempdir().unwrap();
    let root = fixture.path();
    let config = isolated_config(root);
    let data = root.join("data");
    for (room_count, errno) in [
        (1, libc::ENOSYS),
        (1, libc::EPERM),
        (0, libc::EPERM),
        (0, libc::EACCES),
        (0, libc::EIO),
    ] {
        let mut settings = format_daemon_config(root);
        if room_count == 1 {
            let room = root.join("room");
            fs::create_dir_all(&room).unwrap();
            settings.vaults.push(VaultRegistration {
                id: "VaultOne_0123456789abc".into(),
                path: room,
                room: Some("RoomAaaa_0123456789abc".into()),
            });
        }
        save_config(&config, &settings).unwrap();
        assert_startup_refused(
            &config,
            &data,
            &format!("openclast_openat2_probe_failed: errno {errno} "),
            |command| {
                // Affect only this test child. Return the test errno from openat2;
                // do not block any other syscall needed by the loader or runtime.
                unsafe {
                    command.pre_exec(move || {
                        let filter = [
                            libc::sock_filter {
                                code: 0x20,
                                jt: 0,
                                jf: 0,
                                k: 0,
                            }, // seccomp_data.nr
                            libc::sock_filter {
                                code: 0x15,
                                jt: 0,
                                jf: 1,
                                k: libc::SYS_openat2 as u32,
                            },
                            libc::sock_filter {
                                code: 0x06,
                                jt: 0,
                                jf: 0,
                                k: libc::SECCOMP_RET_ERRNO | errno as u32,
                            },
                            libc::sock_filter {
                                code: 0x06,
                                jt: 0,
                                jf: 0,
                                k: libc::SECCOMP_RET_ALLOW,
                            },
                        ];
                        let program = libc::sock_fprog {
                            len: filter.len() as u16,
                            filter: filter.as_ptr().cast_mut(),
                        };
                        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                            || libc::prctl(
                                libc::PR_SET_SECCOMP,
                                libc::SECCOMP_MODE_FILTER,
                                &program,
                            ) != 0
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            },
        );
    }
}

fn assert_closed_room_headers(headers: &str) {
    let values = |name: &str| {
        headers
            .lines()
            .filter_map(|line| line.strip_prefix(name).map(str::trim))
            .collect::<Vec<_>>()
    };
    assert_eq!(values("kwiry-id-scheme:"), ["room-v1"]);
    assert_eq!(values("kwiry-multi-room:"), ["closed"]);
}

const FORMAT_HEALTH: &str = r#"{"status":"ok","id_scheme":"room-v1","multi_room":"closed"}"#;

fn vault_add(config: &Path, data: &Path, id: &str, vault: &Path) {
    let status = Command::new(cargo_bin("kwiry"))
        .args([
            "--config",
            config.to_str().unwrap(),
            "--data-dir",
            data.to_str().unwrap(),
            "vault",
            "add",
            "--id",
            id,
            "--path",
            vault.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());
}

fn search(address: &str, token: &str, query: &str) -> Vec<kwiry_core::SearchHit> {
    let body = serde_json::json!({
        "q": query,
        "mode": "lexical",
        "limit": 20
    })
    .to_string();
    let (status, response) = request(address, token, "POST", "/v0/search", Some(&body));
    assert_eq!(status, 200, "{response}");
    serde_json::from_str::<ApiSearchResponse>(&response)
        .unwrap()
        .hits
}

fn status(address: &str, token: &str) -> DaemonStatus {
    let (status, response) = request(address, token, "GET", "/v0/status", None);
    assert_eq!(status, 200, "{response}");
    serde_json::from_str(&response).unwrap()
}

fn request(
    address: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, String) {
    let (status, body, _) = request_with_headers(address, token, method, path, body);
    (status, body)
}

fn request_with_headers(
    address: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, String, String) {
    let body = body.unwrap_or_default();
    let timeout = Duration::from_secs(2);
    let mut stream = TcpStream::connect_timeout(&address.parse().unwrap(), timeout).unwrap();
    stream.set_read_timeout(Some(timeout)).unwrap();
    stream.set_write_timeout(Some(timeout)).unwrap();
    let authorization = if token.is_empty() {
        String::new()
    } else {
        format!("Authorization: Bearer {token}\r\n")
    };
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\n{authorization}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    let status = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, body.to_owned(), headers.to_owned())
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("condition was not satisfied before timeout");
}
