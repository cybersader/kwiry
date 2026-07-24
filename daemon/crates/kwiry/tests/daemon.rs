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
        let mut child = Command::new(cargo_bin("kwiry"))
            .args([
                "--config",
                config.to_str().unwrap(),
                "--data-dir",
                data.to_str().unwrap(),
                "serve",
                "--bind",
                "127.0.0.1:0",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
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
fn openclast_startup_creates_no_desktop_credentials_or_descriptor() {
    let temporary = tempdir().unwrap();
    let config_path = temporary.path().join("config.toml");
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
        tenant_id: "tenant-a".into(),
        issuer: "issuer".into(),
        audience: "kwiry-search".into(),
        jwks_file,
        max_token_ttl_seconds: 60,
    });
    config.vaults = vec![VaultRegistration {
        id: "fixture".into(),
        path: vault,
        room: Some("room-a".into()),
    }];
    save_config(&config_path, &config).unwrap();

    let daemon = Daemon::start_openclast(&config_path, &data);
    assert!(!config_path.with_extension("token").exists());
    assert!(!data.join("connection.json").exists());
    let (status, _) = request(&daemon.address, "desktop-token", "GET", "/v0/status", None);
    assert_eq!(status, 404);
    daemon.stop();

    let logs = fs::read_dir(data.join("logs"))
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(logs.contains("kwiry OpenClast sidecar ready"), "{logs}");
    assert!(!logs.contains("desktop-token"));
}

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
    let body = body.unwrap_or_default();
    let mut stream = TcpStream::connect(address).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
    (status, body.to_owned())
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
