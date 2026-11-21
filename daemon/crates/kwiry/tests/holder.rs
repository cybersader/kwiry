//! The lifecycle holder wired into the shipped daemon, against a local fake
//! authority served over HTTP. Synthetic data only: every key, id and seed
//! here is a published test value.
#![cfg(target_os = "linux")]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use assert_cmd::cargo::cargo_bin;
use kwiry_core::{
    Config, HolderProfile, HolderSettings, HostProfile, OpenClastAuthConfig, VaultRegistration,
    save_config,
};
use kwiry_holdings::client::{Timeouts, Transport};
use kwiry_holdings::keys::HolderKey;
use kwiry_holdings::profile::WireProfile;
use kwiry_holdings::state::{FsIo, HolderDir};
use kwiry_holdings::testkit::{AuthorityConfig, FakeAuthority, FakeClock, TEST_HOLDER_KEY_SEED};
use tempfile::tempdir;

const ROOM: &str = "RoomAaaa_0123456789abc";
const DIGEST_SALT: &str = "openclast-lifecycle/v1";

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
}

impl AuthorityServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let issuer = format!("{origin}/api/lifecycle/v1");
        let clock = Arc::new(FakeClock::new(wall_ms(), 1_000_000));
        let authority = FakeAuthority::new(
            AuthorityConfig {
                profile: openclast_profile(),
                issuer: issuer.clone(),
                digest_salt: DIGEST_SALT.into(),
                holder_kind: "engine".into(),
                serve_seconds: 900,
                relist_seconds: 300,
            },
            clock.clone(),
        );
        let hits = Arc::new(AtomicUsize::new(0));
        let served = authority.clone();
        let counter = hits.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                counter.fetch_add(1, Ordering::SeqCst);
                let lag = wall_ms() - kwiry_holdings::clock::Clock::wall_ms(clock.as_ref());
                if lag > 0 {
                    clock.advance(u64::try_from(lag).unwrap());
                }
                serve_one(stream, &served, &origin);
            }
        });
        Self {
            authority,
            issuer,
            hits,
        }
    }
}

fn serve_one(mut stream: TcpStream, authority: &FakeAuthority, origin: &str) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
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
        status_interval_seconds: Some(1),
        report_interval_seconds: Some(1),
        purge_interval_seconds: Some(1),
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

#[test]
fn a_configured_daemon_applies_a_list_and_gets_a_status_receipt_over_http() {
    let server = AuthorityServer::start();
    let fixture = fixture(Some(&server.issuer));
    enroll(&fixture, &server);
    {
        let mut authority = server.authority.state();
        let holding = authority.add_active("room", ROOM);
        authority.attest(&holding, "synthetic-root-1");
    }
    let daemon = Daemon::start(&fixture);
    let receipted = wait_for(20, || {
        server
            .authority
            .state()
            .statuses
            .iter()
            .any(|(_, status)| *status == 200)
    });
    assert!(receipted, "no status receipt");
    // The receipted status cites a list the authority issued in this epoch;
    // the holder's high-water (below) is that applied list.
    let (cited, epoch) = {
        let authority = server.authority.state();
        let claims = authority
            .statuses
            .iter()
            .find(|(_, status)| *status == 200)
            .map(|(claims, _)| claims.clone())
            .unwrap();
        let cited = claims["list_ref"]["rev"]["seq"].as_u64().unwrap();
        assert!(cited <= authority.seq);
        assert_eq!(claims["list_ref"]["rev"]["epoch"], authority.epoch.as_str());
        (cited, authority.epoch.clone())
    };

    // The running daemon's snapshot is visible to a read-only status.
    let output = kwiry(&fixture, &["holder", "status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["high_water_seq"], cited);
    assert_eq!(report["high_water_epoch"], epoch.as_str());
    assert_eq!(report["status_halted"], false);
    assert!(wait_for(5, || {
        let output = kwiry(&fixture, &["holder", "status", "--json"])
            .output()
            .unwrap();
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        report["live"]["list_applied_in_process"] == true && report["live"]["route"] == "normal"
    }));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains(ROOM), "status never names a resource");
    assert!(!text.contains("\"d\""), "status never carries key material");

    // State-changing commands refuse while the daemon holds the lock.
    let output = kwiry(&fixture, &["holder", "relist"]).output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("is the daemon running"),
        "{output:?}"
    );
    daemon.stop();

    // With the daemon stopped, relist runs one pass and prints its outcome
    // (after the authority's 5 s list-start spacing).
    thread::sleep(Duration::from_millis(5_500));
    let output = kwiry(&fixture, &["holder", "relist"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("list applied: epoch "), "{text}");
    // Nothing to resume: both commands refuse and change nothing.
    let output = kwiry(&fixture, &["holder", "resume-status"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let output = kwiry(
        &fixture,
        &["holder", "resume-report", "ob_TESTnone0000000000000001"],
    )
    .output()
    .unwrap();
    assert!(!output.status.success());
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
