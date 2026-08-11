mod executor;
mod model;
mod planner;
mod prompt;
mod readiness;

pub use executor::{SetupExecutor, SetupServiceManager};
pub use model::{
    ActionDisposition, ReadinessExpectation, ResolvedSetupInput, ServiceInstallSpec, SetupAction,
    SetupActionKind, SetupActionOutcome, SetupActionReport, SetupConnectionReport,
    SetupDaemonSnapshot, SetupErrorCode, SetupIssue, SetupPlan, SetupReport, SetupRequest,
    SetupServiceState, SetupSnapshot,
};
pub use planner::plan_setup;
pub use prompt::{
    NoPrompt, Prompt, SEMANTIC_DISCLOSURE, ScriptedPrompt, StdioPrompt, resolve_setup_input,
    suggest_vault_id,
};
pub use readiness::{HttpReadinessProbe, ReadinessProbe};

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    use kwiry_core::{
        CHUNKING_VERSION, Config, ConnectionDescriptor, DaemonState, DaemonStatus, ModelStatus,
        Paths, SourceFormatCounts, VaultRegistration, VaultStatus, write_connection_descriptor,
    };
    use tempfile::tempdir;

    use super::readiness::{validate_descriptor, validate_status};
    use super::*;

    fn paths() -> Paths {
        Paths::resolve(
            Some(PathBuf::from("/workspace/config/config.toml")),
            Some(PathBuf::from("/workspace/state")),
        )
        .unwrap()
    }

    fn input(path: impl Into<PathBuf>, semantic_enabled: bool) -> ResolvedSetupInput {
        ResolvedSetupInput {
            vault: VaultRegistration {
                id: "project-notes".into(),
                path: path.into(),
                room: None,
            },
            semantic_enabled,
            dry_run: false,
        }
    }

    fn ready_snapshot(registration: VaultRegistration, semantic_enabled: bool) -> SetupSnapshot {
        SetupSnapshot {
            config_exists: true,
            config: Config {
                semantic: kwiry_core::SemanticConfig {
                    enabled: semantic_enabled,
                },
                vaults: vec![registration],
                ..Config::default()
            },
            bootstrap_ready: true,
            index_ready: true,
            service: SetupServiceState::Running,
            service_definition_current: true,
            daemon: SetupDaemonSnapshot {
                reachable: true,
                state: Some(DaemonState::Ready),
                semantic_ready: semantic_enabled,
            },
        }
    }

    #[test]
    fn stable_id_suggestion_is_lowercase_and_separator_normalized() {
        assert_eq!(
            suggest_vault_id(Path::new("/vaults/My Project__Notes!!")),
            "my-project-notes"
        );
        assert_eq!(suggest_vault_id(Path::new("/vaults/---")), "vault");
        assert_eq!(suggest_vault_id(Path::new("/")), "vault");
    }

    #[test]
    fn fresh_prompt_defaults_semantic_to_yes_and_discloses_exact_costs() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("Project Notes");
        fs::create_dir(&vault).unwrap();
        let request = SetupRequest {
            vault_path: Some(vault.clone()),
            vault_id: Some("notes".into()),
            ..SetupRequest::default()
        };
        let mut prompt = ScriptedPrompt::new([""]);

        let resolved =
            resolve_setup_input(&request, &SetupSnapshot::default(), &mut prompt).unwrap();

        assert!(resolved.semantic_enabled);
        assert_eq!(resolved.vault.path, fs::canonicalize(vault).unwrap());
        assert_eq!(prompt.transcript()[0], SEMANTIC_DISCLOSURE);
        assert_eq!(
            SEMANTIC_DISCLOSURE,
            "Semantic search downloads 133 MB on first use and may use up to 784 MiB of memory while indexing."
        );
        assert_eq!(prompt.transcript()[1], "Enable semantic search? [yes]");
    }

    #[test]
    fn rerun_prompt_uses_persisted_semantic_choice_and_registration() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("notes");
        fs::create_dir(&vault).unwrap();
        let vault = fs::canonicalize(vault).unwrap();
        let snapshot = ready_snapshot(
            VaultRegistration {
                id: "persisted-id".into(),
                path: vault.clone(),
                room: Some("work".into()),
            },
            false,
        );
        let mut prompt = ScriptedPrompt::new(["", "", ""]);

        let resolved =
            resolve_setup_input(&SetupRequest::default(), &snapshot, &mut prompt).unwrap();

        assert_eq!(resolved.vault.id, "persisted-id");
        assert_eq!(resolved.vault.path, vault);
        assert_eq!(resolved.vault.room.as_deref(), Some("work"));
        assert!(!resolved.semantic_enabled);
        assert_eq!(
            prompt.transcript().last().unwrap(),
            "Enable semantic search? [no]"
        );
    }

    #[test]
    fn no_prompt_accepts_complete_input_and_rejects_missing_answers() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("notes");
        fs::create_dir(&vault).unwrap();
        let complete = SetupRequest {
            vault_path: Some(vault),
            vault_id: Some("notes".into()),
            semantic: Some(false),
            ..SetupRequest::default()
        };
        resolve_setup_input(&complete, &SetupSnapshot::default(), &mut NoPrompt).unwrap();

        assert_eq!(
            resolve_setup_input(
                &SetupRequest::default(),
                &SetupSnapshot::default(),
                &mut NoPrompt,
            ),
            Err(SetupErrorCode::PromptRequired)
        );
    }

    #[test]
    fn fresh_plan_registers_indexes_installs_starts_and_checks_readiness() {
        let plan = plan_setup(
            input(PathBuf::from("/vaults/project-notes"), true),
            &SetupSnapshot::default(),
            &paths(),
        )
        .unwrap();
        let applied: Vec<_> = plan
            .actions
            .iter()
            .filter(|action| action.disposition == ActionDisposition::Apply)
            .map(|action| action.kind)
            .collect();

        assert_eq!(
            applied,
            vec![
                SetupActionKind::EnsureVaultRegistration,
                SetupActionKind::SetSemanticPreference,
                SetupActionKind::BootstrapDesktop,
                SetupActionKind::BuildIndex,
                SetupActionKind::InstallService,
                SetupActionKind::StartService,
                SetupActionKind::CheckReadiness,
            ]
        );
        assert!(plan.changes_system());
    }

    #[test]
    fn exact_rerun_is_idempotent_but_still_verifies_readiness() {
        let registration = input("/vaults/project-notes", false).vault;
        let plan = plan_setup(
            ResolvedSetupInput {
                vault: registration.clone(),
                semantic_enabled: false,
                dry_run: false,
            },
            &ready_snapshot(registration, false),
            &paths(),
        )
        .unwrap();

        assert!(plan.actions.iter().all(|action| {
            action.kind == SetupActionKind::CheckReadiness
                || action.disposition == ActionDisposition::AlreadySatisfied
        }));
        assert_eq!(
            plan.actions.last().unwrap().disposition,
            ActionDisposition::Apply
        );
        assert!(!plan.changes_system());
    }

    #[test]
    fn planner_rejects_id_and_path_conflicts() {
        let configured = VaultRegistration {
            id: "project-notes".into(),
            path: PathBuf::from("/vaults/old"),
            room: None,
        };
        let snapshot = ready_snapshot(configured, false);
        assert_eq!(
            plan_setup(input("/vaults/new", false), &snapshot, &paths()),
            Err(SetupErrorCode::VaultIdConflict)
        );

        let different_id = ResolvedSetupInput {
            vault: VaultRegistration {
                id: "other".into(),
                path: PathBuf::from("/vaults/old"),
                room: None,
            },
            semantic_enabled: false,
            dry_run: false,
        };
        assert_eq!(
            plan_setup(different_id, &snapshot, &paths()),
            Err(SetupErrorCode::VaultPathConflict)
        );
    }

    #[test]
    fn semantic_drift_skips_lexical_rebuild_and_restarts_running_service() {
        let registration = input("/vaults/project-notes", false).vault;
        let plan = plan_setup(
            ResolvedSetupInput {
                vault: registration.clone(),
                semantic_enabled: true,
                dry_run: false,
            },
            &ready_snapshot(registration, false),
            &paths(),
        )
        .unwrap();

        let index = plan
            .actions
            .iter()
            .find(|action| action.kind == SetupActionKind::BuildIndex)
            .unwrap();
        assert_eq!(index.disposition, ActionDisposition::Skipped);
        assert!(plan.actions.iter().any(|action| {
            action.kind == SetupActionKind::RestartService
                && action.disposition == ActionDisposition::Apply
        }));
    }

    #[test]
    fn service_definition_drift_updates_and_restarts_the_running_service() {
        let registration = input("/vaults/project-notes", false).vault;
        let mut snapshot = ready_snapshot(registration.clone(), false);
        snapshot.service_definition_current = false;

        let plan = plan_setup(
            ResolvedSetupInput {
                vault: registration,
                semantic_enabled: false,
                dry_run: false,
            },
            &snapshot,
            &paths(),
        )
        .unwrap();

        assert!(plan.actions.iter().any(|action| {
            action.kind == SetupActionKind::InstallService
                && action.disposition == ActionDisposition::Apply
        }));
        assert!(plan.actions.iter().any(|action| {
            action.kind == SetupActionKind::RestartService
                && action.disposition == ActionDisposition::Apply
        }));
    }

    #[test]
    fn pure_planner_has_no_wsl_or_host_path_assumptions() {
        let plan = plan_setup(
            input("/portable/vault", false),
            &SetupSnapshot::default(),
            &paths(),
        )
        .unwrap();
        let encoded = serde_json::to_string(&plan).unwrap();
        assert!(encoded.contains("/portable/vault"));
        assert!(!encoded.contains("/mnt/c"));
        assert!(!encoded.contains("\\\\wsl$"));
    }

    #[derive(Default)]
    struct CountingService {
        calls: Arc<AtomicUsize>,
    }

    impl SetupServiceManager for CountingService {
        fn install(&mut self, _spec: &ServiceInstallSpec) -> Result<(), SetupErrorCode> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn start(&mut self) -> Result<(), SetupErrorCode> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn restart(&mut self) -> Result<(), SetupErrorCode> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Default)]
    struct CountingProbe {
        calls: Arc<AtomicUsize>,
    }

    impl ReadinessProbe for CountingProbe {
        fn wait_until_ready(
            &mut self,
            _expectation: &ReadinessExpectation,
        ) -> Result<SetupConnectionReport, SetupErrorCode> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(SetupErrorCode::ReadinessTimedOut)
        }
    }

    #[test]
    fn dry_run_reports_plan_without_files_service_or_probe_side_effects() {
        let temporary = tempdir().unwrap();
        let paths = Paths::resolve(
            Some(temporary.path().join("config/config.toml")),
            Some(temporary.path().join("state")),
        )
        .unwrap();
        let mut resolved = input(temporary.path().join("vault"), false);
        resolved.dry_run = true;
        let plan = plan_setup(resolved, &SetupSnapshot::default(), &paths).unwrap();
        let mut service = CountingService::default();
        let service_calls = service.calls.clone();
        let mut probe = CountingProbe::default();
        let probe_calls = probe.calls.clone();

        let report = SetupExecutor::new(&paths, &mut service, &mut probe).execute(&plan);

        assert!(report.ok);
        assert!(report.dry_run);
        assert_eq!(service_calls.load(Ordering::SeqCst), 0);
        assert_eq!(probe_calls.load(Ordering::SeqCst), 0);
        assert!(!paths.config.exists());
        assert!(!paths.setup_lock_path().exists());
    }

    fn status(version: &str, semantic: bool) -> DaemonStatus {
        let mut source_format_counts = SourceFormatCounts::default();
        source_format_counts.record(
            kwiry_core::SourceFormat::Markdown,
            kwiry_core::ExtractionCoverage::IndexedComplete,
        );
        DaemonStatus {
            state: DaemonState::Ready,
            version: version.into(),
            generation: Some("generation-1".into()),
            chunking_version: CHUNKING_VERSION,
            extraction_policy_fingerprint: kwiry_core::extraction_policy_fingerprint().to_owned(),
            extraction_policy: kwiry_core::active_extraction_policy(),
            format_identities: kwiry_core::owned_format_identities(),
            documents: 1,
            chunks: 1,
            source_format_counts,
            last_sync: None,
            dirty: false,
            rebuilding: false,
            model: semantic.then(|| ModelStatus {
                name: "model".into(),
                version: "fingerprint".into(),
            }),
            vaults: vec![VaultStatus {
                vault_id: "project-notes".into(),
                room: None,
                documents: 1,
                chunks: 1,
                last_sync: None,
                dirty: false,
                warning_count: 0,
                last_error: None,
            }],
        }
    }

    #[test]
    fn descriptor_and_status_validation_are_loopback_and_expectation_bound() {
        let state_dir = std::env::current_dir().unwrap().join("state");
        let descriptor = ConnectionDescriptor::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 32189),
            state_dir.join("token"),
            "1.2.3",
        );
        assert_eq!(
            validate_descriptor(&descriptor).unwrap(),
            "127.0.0.1:32189".parse().unwrap()
        );
        let expectation = ReadinessExpectation {
            connection_path: state_dir.join("connection.json"),
            vault_id: "project-notes".into(),
            semantic_enabled: true,
        };
        validate_status(&descriptor, &status("1.2.3", true), &expectation).unwrap();
        assert_eq!(
            validate_status(&descriptor, &status("1.2.3", false), &expectation),
            Err(SetupErrorCode::SemanticNotReady)
        );
        let mut not_published = status("1.2.3", true);
        not_published.generation = None;
        assert_eq!(
            validate_status(&descriptor, &not_published, &expectation),
            Err(SetupErrorCode::StatusCheckFailed)
        );
        let mut vault_failed = status("1.2.3", true);
        vault_failed.vaults[0].last_error = Some("fixture failure".into());
        assert_eq!(
            validate_status(&descriptor, &vault_failed, &expectation),
            Err(SetupErrorCode::VaultNotReady)
        );

        let remote = ConnectionDescriptor::new(
            "192.0.2.10:32189".parse().unwrap(),
            state_dir.join("remote-token"),
            "1.2.3",
        );
        assert_eq!(
            validate_descriptor(&remote),
            Err(SetupErrorCode::NonLoopbackConnection)
        );
    }

    #[test]
    fn http_probe_uses_token_but_report_never_contains_it() {
        const SENTINEL: &str = "sentinel-token-must-not-leak";
        let temporary = tempdir().unwrap();
        let token_path = temporary.path().join("token");
        let descriptor_path = temporary.path().join("connection.json");
        fs::write(&token_path, format!("{SENTINEL}\n")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        write_connection_descriptor(
            &descriptor_path,
            &ConnectionDescriptor::new(address, token_path, "1.2.3"),
        )
        .unwrap();
        let status_body = serde_json::to_vec(&status("1.2.3", false)).unwrap();
        let server = thread::spawn(move || {
            for (index, stream) in listener.incoming().take(2).enumerate() {
                let mut stream = stream.unwrap();
                let mut request = [0_u8; 4096];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                let body = if index == 0 {
                    assert!(request.starts_with("GET /v0/health "));
                    br#"{"status":"ok"}"#.to_vec()
                } else {
                    assert!(request.starts_with("GET /v0/status "));
                    assert!(request.contains(&format!("Authorization: Bearer {SENTINEL}")));
                    status_body.clone()
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        let expectation = ReadinessExpectation {
            connection_path: descriptor_path,
            vault_id: "project-notes".into(),
            semantic_enabled: false,
        };
        let mut probe = HttpReadinessProbe::new(
            Duration::from_secs(2),
            Duration::from_millis(10),
            Duration::from_secs(1),
        );

        let connection = probe.wait_until_ready(&expectation).unwrap();
        server.join().unwrap();
        let encoded = serde_json::to_string(&connection).unwrap();
        assert!(!encoded.contains(SENTINEL));
    }

    #[test]
    fn json_plan_and_failure_report_do_not_have_a_token_value_field() {
        const SENTINEL: &str = "sentinel-token-must-not-appear";
        let mut resolved = input("/vaults/project-notes", false);
        resolved.dry_run = true;
        let plan = plan_setup(resolved, &SetupSnapshot::default(), &paths()).unwrap();
        let mut report = SetupReport::from_plan(&plan);
        report.ok = false;
        report.issues.push(SetupErrorCode::TokenUnavailable.into());

        for encoded in [
            serde_json::to_string(&plan).unwrap(),
            serde_json::to_string(&report).unwrap(),
        ] {
            assert!(!encoded.contains(SENTINEL));
            assert!(!encoded.contains("bearer_token"));
            assert!(!encoded.contains("token_value"));
        }
    }
}
