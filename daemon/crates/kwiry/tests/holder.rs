//! The lifecycle holder wired into the shipped daemon, against a local fake
//! authority served over HTTP. Synthetic data only: every key, id and seed
//! here is a published test value.
#![cfg(target_os = "linux")]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use assert_cmd::cargo::cargo_bin;
use kwiry_core::{
    Config, HolderProfile, HolderSettings, HostProfile, OpenClastAuthConfig, VaultRegistration,
    save_config,
};
use kwiry_holdings::client::{Timeouts, Transport};
use kwiry_holdings::keys::HolderKey;
use kwiry_holdings::profile::{ROUTE_LIST, ROUTE_REPORT, ROUTE_STATUS, WireProfile};
use kwiry_holdings::state::{FsIo, HolderDir, STATE_FILE};
use kwiry_holdings::testkit::{AuthorityConfig, FakeAuthority, FakeClock, TEST_HOLDER_KEY_SEED};
use tempfile::tempdir;

const ROOM: &str = "RoomAaaa_0123456789abc";
const DIGEST_SALT: &str = "openclast-lifecycle/v1";
const BASE_PATH: &str = "/api/lifecycle/v1";

/// The OpenClast wire profile, as the daemon's `profile = "openclast"`
/// selects it. Mirrored here because the composition module is private to
/// the binary; `the_mirrored_profile_matches_the_daemon` guards the copy.
fn openclast_profile() -> WireProfile {
    let strings = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect();
    WireProfile {
        typ_list_request: "openclast-holdings-listreq+jwt".into(),
        typ_list_page: "openclast-holdings-list+jwt".into(),
        typ_keyset: "openclast-lifecycle-keyset+jwt".into(),
        typ_report_request: "openclast-obligation-report+jwt".into(),
        typ_report_ack: "openclast-obligation-ack+jwt".into(),
        typ_status_request: "openclast-holder-status+jwt".into(),
        typ_status_ack: "openclast-holder-status-ack+jwt".into(),
        outcome_header: "OpenClast-Lifecycle-Outcome".into(),
        audience_prefix: "openclast-lifecycle:".into(),
        authority_key_id_prefix: "lc-".into(),
        resource_kinds: strings(&["room", "vault", "workspace", "file", "share", "backup"]),
        accepted_resource_kinds: strings(&["room"]),
        obligation_reasons: strings(&["retire", "holder-decommission"]),
        engine: Some(kwiry_holdings::engine::EngineProfile {
            holder_kind: "engine".into(),
            pinned_resource_kind: "room".into(),
        }),
    }
}

fn wall_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// The testkit authority behind a loopback HTTP listener. Its fake clock is
/// pulled forward to real time before every request.
struct AuthorityServer {
    authority: FakeAuthority,
    issuer: String,
    hits: Arc<AtomicUsize>,
    post_paths: Arc<Mutex<Vec<String>>>,
}

impl AuthorityServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let issuer = format!("{origin}{BASE_PATH}");
        let clock = Arc::new(FakeClock::new(wall_ms(), 1_000_000));
        let authority = FakeAuthority::new(
            AuthorityConfig {
                profile: openclast_profile(),
                issuer: issuer.clone(),
                digest_salt: DIGEST_SALT.into(),
                holder_kind: "engine".into(),
                serve_seconds: 900,
                relist_seconds: 5,
            },
            clock.clone(),
        );
        let hits = Arc::new(AtomicUsize::new(0));
        let post_paths = Arc::new(Mutex::new(Vec::new()));
        let served = authority.clone();
        let counter = hits.clone();
        let recorded_posts = post_paths.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                counter.fetch_add(1, Ordering::SeqCst);
                let lag = wall_ms() - kwiry_holdings::clock::Clock::wall_ms(clock.as_ref());
                if lag > 0 {
                    clock.advance(u64::try_from(lag).unwrap());
                }
                serve_one(stream, &served, &origin, &recorded_posts);
            }
        });
        Self {
            authority,
            issuer,
            hits,
            post_paths,
        }
    }

    fn posts_to(&self, route: &str) -> usize {
        let path = format!("{BASE_PATH}{route}");
        self.post_paths
            .lock()
            .unwrap()
            .iter()
            .filter(|&seen| seen == &path)
            .count()
    }
}

fn serve_one(
    mut stream: TcpStream,
    authority: &FakeAuthority,
    origin: &str,
    post_paths: &Mutex<Vec<String>>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    if method == "POST" {
        post_paths.lock().unwrap().push(path.to_owned());
    }
    let mut content_type = String::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            match name.to_ascii_lowercase().as_str() {
                "content-type" => value.trim().clone_into(&mut content_type),
                "content-length" => length = value.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let url = format!("{origin}{path}");
    let timeouts = Timeouts {
        connect_ms: 1_000,
        response_ms: 1_000,
    };
    let response = if method == "POST" {
        authority.post(&url, &content_type, &body, timeouts)
    } else {
        authority.get(&url, timeouts)
    };
    let Ok(response) = response else {
        // A forced transport failure: drop the connection.
        return;
    };
    let mut head = format!(
        "HTTP/1.1 {} Status\r\ncontent-length: {}\r\nconnection: close\r\n",
        response.status,
        response.body.len()
    );
    if let Some(content_type) = &response.content_type {
        head.push_str(&format!("content-type: {content_type}\r\n"));
    }
    if let Some(retry) = response.retry_after_s {
        head.push_str(&format!("retry-after: {retry}\r\n"));
    }
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&response.body);
}

struct Fixture {
    _temporary: tempfile::TempDir,
    config: PathBuf,
    data: PathBuf,
    state_dir: PathBuf,
}

/// An OpenClast-profile daemon with one room, and (when `issuer` is given)
/// a `[holder]` section pointing at it.
fn fixture(issuer: Option<&str>) -> Fixture {
    let temporary = tempdir().unwrap();
    let root = temporary.path();
    let control = root.join("control");
    fs::create_dir_all(&control).unwrap();
    let config_path = control.join("config.toml");
    let vault = root.join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("note.md"), "# Synthetic\nholderprobe").unwrap();
    let jwks_file = root.join("search.jwks.json");
    fs::write(
        &jwks_file,
        r#"{"keys":[{"kty":"OKP","crv":"Ed25519","x":"2-Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8","use":"sig","key_ops":["verify"],"alg":"EdDSA","kid":"ed01"}]}"#,
    )
    .unwrap();
    let state_dir = root.join("holder");
    let mut config = Config::default();
    config.server.profile = HostProfile::OpenClast;
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
        snapshot_id: None,
        id: "VaultOne_0123456789abc".into(),
        path: fs::canonicalize(&vault).unwrap(),
        room: Some(ROOM.into()),
    }];
    config.holder = issuer.map(|issuer| HolderSettings {
        profile: HolderProfile::OpenClast,
        issuer: issuer.to_owned(),
        state_dir: state_dir.clone(),
        page_size: None,
        clock_skew_allowance_seconds: None,
        max_stale_serve_seconds: None,
    });
    save_config(&config_path, &config).unwrap();
    Fixture {
        config: config_path,
        data: root.join("data"),
        state_dir,
        _temporary: temporary,
    }
}

/// Writes the test holder key, then enrolls through `kwiry holder enroll`.
fn enroll(fixture: &Fixture, server: &AuthorityServer) {
    let key = HolderKey::from_seed(TEST_HOLDER_KEY_SEED);
    {
        let dir = HolderDir::open(&fixture.state_dir, &[]).unwrap();
        kwiry_holdings::startup::write_key(&dir, &FsIo, &key).unwrap();
    }
    let (holder_id, jwks) = server.authority.enroll(&key);
    let jwks_path = fixture.state_dir.with_extension("jwks.json");
    fs::write(&jwks_path, serde_json::to_vec(&jwks).unwrap()).unwrap();
    let output = kwiry(
        fixture,
        &["holder", "enroll", "--holder-id", &holder_id, "--jwks"],
    )
    .arg(&jwks_path)
    .output()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
}

fn kwiry(fixture: &Fixture, args: &[&str]) -> Command {
    let mut command = Command::new(cargo_bin("kwiry"));
    command
        .arg("--config")
        .arg(&fixture.config)
        .arg("--data-dir")
        .arg(&fixture.data)
        .args(args);
    command
}

struct Daemon {
    child: Child,
}

impl Daemon {
    fn start(fixture: &Fixture) -> Self {
        let mut child = kwiry(fixture, &["serve", "--bind", "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut line = String::new();
            let mut reader = BufReader::new(output);
            let _ = reader.read_line(&mut line);
            let _ = send.send(line);
            // Keep draining so the daemon never blocks on stdout.
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
        });
        let line = receive
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_default();
        let mut daemon = Self { child };
        if !line.starts_with("kwiry OpenClast sidecar listening on http://") {
            let _ = daemon.child.kill();
            panic!("daemon did not start: {line:?}");
        }
        daemon
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

fn wait_for(seconds: u64, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

fn holder_status(fixture: &Fixture) -> serde_json::Value {
    let output = kwiry(fixture, &["holder", "status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn a_configured_daemon_applies_a_list_and_never_posts_status_or_obligation_reports() {
    let server = AuthorityServer::start();
    let fixture = fixture(Some(&server.issuer));
    enroll(&fixture, &server);
    let (seq, epoch) = {
        let mut authority = server.authority.state();
        let holding = authority.add_active("room", ROOM);
        authority.attest(&holding, "synthetic-root-1");
        authority.retire(&holding); // A live obligation, not just an empty report queue.
        (authority.seq, authority.epoch.clone())
    };
    let daemon = Daemon::start(&fixture);

    // The running daemon applied the signed list and exposed a local snapshot.
    assert!(wait_for(20, || {
        let report = holder_status(&fixture);
        report["live"]["list_applied_in_process"] == true && report["high_water_seq"] == seq
    }));
    let report = holder_status(&fixture);
    assert_eq!(report["high_water_epoch"], epoch.as_str());
    assert_eq!(report["live"]["route"], "normal");
    assert_eq!(report["tombstones"], 1);
    let text = report.to_string();
    assert!(!text.contains(ROOM), "status never names a resource");
    assert!(!text.contains("\"d\""), "status never carries key material");

    // A second scheduled list start proves the observation spans more than
    // two 500 ms driver ticks (list starts are spaced at least 5 s apart).
    assert!(wait_for(15, || server.posts_to(ROUTE_LIST) >= 2));
    let snapshot_path = fixture.data.join("holder-status.json");
    let mode = fs::metadata(&snapshot_path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let snapshot = fs::read(&snapshot_path).unwrap();

    // State-changing commands refuse while the daemon holds the lock.
    let output = kwiry(&fixture, &["holder", "relist"]).output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("is the daemon running"),
        "{output:?}"
    );
    daemon.stop();
    assert_eq!(server.posts_to(ROUTE_STATUS), 0);
    assert_eq!(server.posts_to(ROUTE_REPORT), 0);
    assert!(!snapshot_path.exists(), "clean shutdown left a snapshot");

    // Simulate an old snapshot left by an unclean exit. With the lock free,
    // a read-only status ignores it rather than printing it as live.
    fs::write(&snapshot_path, &snapshot).unwrap();
    assert!(holder_status(&fixture).get("live").is_none());
    assert_eq!(fs::read(&snapshot_path).unwrap(), snapshot);

    // With the daemon stopped, relist runs one pass and prints its outcome
    // (after the authority's 5 s list-start spacing).
    thread::sleep(Duration::from_millis(5_500));
    let output = kwiry(&fixture, &["holder", "relist"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("list applied: epoch "), "{text}");
    assert_eq!(server.posts_to(ROUTE_STATUS), 0);
    assert_eq!(server.posts_to(ROUTE_REPORT), 0);
}

#[test]
fn without_holder_configuration_the_daemon_sends_no_holder_traffic() {
    let server = AuthorityServer::start();
    // An enrolled holder directory exists, but the config has no [holder].
    let fixture = fixture(Some(&server.issuer));
    enroll(&fixture, &server);
    let mut config = kwiry_core::load_config(&fixture.config).unwrap();
    config.holder = None;
    save_config(&fixture.config, &config).unwrap();
    let daemon = Daemon::start(&fixture);
    thread::sleep(Duration::from_secs(3));
    daemon.stop();
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
    assert!(!fixture.data.join("holder-marker.json").exists());
    assert!(!fixture.data.join("holder-status.json").exists());
    let output = kwiry(&fixture, &["holder", "status"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no [holder] section"));
}

/// Startup fails closed: unreadable holder state stops the daemon before it
/// serves, with a non-zero exit and the refusal code.
#[test]
fn a_holder_that_cannot_start_stops_the_daemon_before_it_serves() {
    let server = AuthorityServer::start();
    let fixture = fixture(Some(&server.issuer));
    enroll(&fixture, &server);
    fs::write(fixture.state_dir.join("holder-state.json"), b"{not json").unwrap();
    let output = kwiry(&fixture, &["serve", "--bind", "127.0.0.1:0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains("listening"), "{stdout}");
    assert!(stderr.contains("holder_state_corrupt"), "{stderr}");
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
}

#[test]
fn an_unmarked_staging_generation_is_refused_and_preserved() {
    assert_unmarked_index_is_refused_and_preserved("generations/.staging-test/segment", false);
}

#[test]
fn an_unmarked_semantic_only_root_is_refused_and_preserved() {
    assert_unmarked_index_is_refused_and_preserved("semantic/semantic.db", true);
}

fn assert_unmarked_index_is_refused_and_preserved(index_file: &str, lose_holder_state: bool) {
    let server = AuthorityServer::start();
    let fixture = fixture(Some(&server.issuer));
    enroll(&fixture, &server);
    let state = fixture.state_dir.join(STATE_FILE);
    if lose_holder_state {
        fs::remove_file(&state).unwrap();
    }
    let index_file = fixture.data.join(index_file);
    fs::create_dir_all(index_file.parent().unwrap()).unwrap();
    fs::write(&index_file, b"synthetic index data").unwrap();

    for args in [
        &["serve", "--bind", "127.0.0.1:0"][..],
        &["holder", "relist"],
    ] {
        let output = kwiry(&fixture, args).output().unwrap();
        assert!(!output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stdout.contains("listening"), "{stdout}");
        for expected in [
            "root_holder_mismatch",
            "remove only `current.json`, `generations`, `meta.json`, `semantic`",
            "the index rebuilds from the vault",
            "Keep everything else in the data directory",
            "`replay-epoch` and `replay-iat-watermark`",
        ] {
            assert!(stderr.contains(expected), "{stderr}");
        }
        assert_eq!(fs::read(&index_file).unwrap(), b"synthetic index data");
        assert!(!fixture.data.join("holder-marker.json").exists());
        assert_eq!(state.exists(), !lose_holder_state);
    }
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
}

#[test]
fn relist_binds_an_unmarked_root_with_only_replay_state() {
    let server = AuthorityServer::start();
    let fixture = fixture(Some(&server.issuer));
    enroll(&fixture, &server);
    fs::create_dir(&fixture.data).unwrap();
    let replay_entries = [("replay-epoch", b"7"), ("replay-iat-watermark", b"9")];
    for (file, bytes) in replay_entries {
        fs::write(fixture.data.join(file), bytes).unwrap();
    }
    let output = kwiry(&fixture, &["holder", "relist"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(fixture.data.join("holder-marker.json").is_file());
    for (file, bytes) in replay_entries {
        assert_eq!(fs::read(fixture.data.join(file)).unwrap(), bytes);
    }
}

#[test]
fn holder_key_creates_a_private_state_directory_on_first_run() {
    let fixture = fixture(Some("https://authority.example.test/api/lifecycle/v1"));
    assert!(!fixture.state_dir.exists());
    let output = kwiry(&fixture, &["holder", "key"]).output().unwrap();
    assert!(output.status.success());
    let mode = fs::metadata(&fixture.state_dir)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn daemon_and_holder_commands_refuse_writable_state_directories() {
    let server = AuthorityServer::start();
    let fixture = fixture(Some(&server.issuer));
    enroll(&fixture, &server);
    let jwks = fixture.state_dir.with_extension("jwks.json");
    for mode in [0o720, 0o702] {
        fs::set_permissions(&fixture.state_dir, fs::Permissions::from_mode(mode)).unwrap();
        for args in [
            &["serve", "--bind", "127.0.0.1:0"][..],
            &["holder", "key"],
            &[
                "holder",
                "enroll",
                "--holder-id",
                "synthetic",
                "--jwks",
                jwks.to_str().unwrap(),
            ],
            &["holder", "relist"],
            &["holder", "status"],
        ] {
            let output = kwiry(&fixture, args).output().unwrap();
            assert!(!output.status.success(), "{args:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("unsafe holder directory permissions"),
                "{stderr}"
            );
        }
    }
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
}

#[test]
fn holder_help_lists_only_the_supported_commands() {
    let fixture = fixture(None);
    let output = kwiry(&fixture, &["holder", "--help"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    let commands: Vec<_> = text
        .lines()
        .skip_while(|line| *line != "Commands:")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(commands, ["key", "enroll", "relist", "status", "help"]);
}

#[test]
fn the_mirrored_profile_matches_the_daemon() {
    let provenance: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/holdings/lifecycle-vectors.v1.provenance.json"
    ))
    .unwrap();
    let wire = &provenance["wire_profile"];
    let profile = openclast_profile();
    let engine = profile.engine.clone().unwrap();
    let mirrored = serde_json::json!({
        "typ_list_request": profile.typ_list_request,
        "typ_list_page": profile.typ_list_page,
        "typ_keyset": profile.typ_keyset,
        "typ_report_request": profile.typ_report_request,
        "typ_report_ack": profile.typ_report_ack,
        "typ_status_request": profile.typ_status_request,
        "typ_status_ack": profile.typ_status_ack,
        "outcome_header": profile.outcome_header,
        "audience_prefix": profile.audience_prefix,
        "authority_key_id_prefix": profile.authority_key_id_prefix,
        "resource_kinds": profile.resource_kinds,
        "accepted_resource_kinds": profile.accepted_resource_kinds,
        "obligation_reasons": profile.obligation_reasons,
        "engine_holder_kind": engine.holder_kind,
        "engine_pinned_resource_kind": engine.pinned_resource_kind,
    });
    for (name, value) in mirrored.as_object().unwrap() {
        assert_eq!(&wire[name], value, "{name}");
    }
}
