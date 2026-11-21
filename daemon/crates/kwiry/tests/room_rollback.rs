#![cfg(target_os = "linux")]

use std::fs::{self, File};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kwiry_core::{Config, HostProfile, OpenClastAuthConfig, VaultRegistration, build_index};

const VAULT: &str = "VaultOne_0123456789abc";
const JWKS: &str = r#"{"keys":[{"kty":"OKP","crv":"Ed25519","x":"2-Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8","use":"sig","key_ops":["verify"],"alg":"EdDSA","kid":"ed01"}]}"#;

#[test]
fn pre_spec_binary_refuses_room_identity_generation_rb1_rb2() {
    let Some(binary) = std::env::var_os("KWIRY_ROLLBACK_BINARY") else {
        eprintln!("RB1/RB2 need KWIRY_ROLLBACK_BINARY; the dedicated CI job supplies it");
        return;
    };
    for (room_count, failure) in [(2, "duplicate vault ID"), (1, "unsupported data layout")] {
        let root = tempfile::tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let data = root.path().join("data");
        let jwks_file = root.path().join("jwks.json");
        fs::write(&jwks_file, JWKS).unwrap();
        let mut config = Config::default();
        config.server.profile = HostProfile::OpenClast;
        config.auth.openclast = Some(OpenClastAuthConfig {
            issuer: "synthetic-issuer".into(),
            audience: "kwiry-search".into(),
            tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
            jwks_file,
            max_token_ttl_seconds: 60,
            workload_subject_allowlist: vec![],
            legacy_identity_accept_until: Some(0),
        });
        for index in 0..room_count {
            let path = root.path().join(format!("room-{index}"));
            fs::create_dir(&path).unwrap();
            fs::write(path.join("index.md"), "synthetic rollback fixture").unwrap();
            config.vaults.push(VaultRegistration {
                snapshot_id: None,
                id: VAULT.into(),
                path,
                room: Some(format!("Room_{index:017}")),
            });
        }
        // Encode the admission config directly; the format config loader refuses it.
        fs::write(&config_path, toml::to_string_pretty(&config).unwrap()).unwrap();
        build_index(&config, &data).unwrap();
        let pointer = data.join("current.json");
        let original = fs::read(&pointer).unwrap();
        let output = root.path().join("old-output.txt");
        assert_refusal(Path::new(&binary), &config_path, &data, &output, failure);
        assert_eq!(
            fs::read(pointer).unwrap(),
            original,
            "old binary must not rebuild"
        );
    }
}

fn assert_refusal(binary: &Path, config: &Path, data: &Path, output: &Path, failure: &str) {
    let log = File::create(output).unwrap();
    let mut child = Command::new(binary)
        .arg("--config")
        .arg(config)
        .arg("--data-dir")
        .arg(data)
        .args(["serve", "--bind", "127.0.0.1:0"])
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "pre-spec binary did not refuse startup: {}",
                fs::read_to_string(output).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let log = fs::read_to_string(output).unwrap();
    assert!(
        !status.success() && log.contains(failure),
        "{status}: {log}"
    );
    assert!(!log.contains("sidecar listening"), "{log}");
}
