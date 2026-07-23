mod auth;
mod capability;
mod logging;
mod runtime;
mod server;
mod service;
pub mod setup;
mod watcher;

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use kwiry_core::{
    DaemonState, DataRoot, HostProfile, Paths, SearchRequest, acquire_setup_lock, add_vault,
    build_index, load_config, search_index, update_config,
};
use serde::Serialize;

#[cfg(windows)]
use crate::service::ProcessCommandRunner;
use crate::service::{ServiceManager, ServiceManagerKind, ServiceSpec, ServiceStatus};
use crate::setup::{
    ActionDisposition, HttpReadinessProbe, NoPrompt, Prompt, ServiceInstallSpec,
    SetupDaemonSnapshot, SetupErrorCode, SetupExecutor, SetupPlan, SetupRequest,
    SetupServiceManager, SetupServiceState, SetupSnapshot, StdioPrompt, plan_setup,
    resolve_setup_input,
};

#[derive(Debug, Parser)]
#[command(name = "kwiry", version, about = "Knowledge workspace search")]
struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[arg(long, global = true, value_name = "DIRECTORY")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Configure a tree and install the per-user daemon service.
    Setup(SetupArgs),
    /// Manage the per-user daemon service.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Manage registered trees.
    Vault {
        #[command(subcommand)]
        command: VaultCommand,
    },
    /// Rebuild the disposable lexical index from registered files.
    Index,
    /// Run the long-lived HTTP search daemon and filesystem reconciler.
    Serve {
        #[arg(long, value_name = "ADDRESS")]
        bind: Option<String>,
        /// Load the local embedding model and serve semantic/hybrid modes.
        /// Downloads the model on first use; embeddings backfill at boot.
        #[arg(long)]
        semantic: bool,
    },
    /// Search the lexical index.
    Search {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, value_name = "VAULT_ID")]
        vault: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct SetupArgs {
    #[arg(value_name = "TREE")]
    tree: Option<PathBuf>,
    #[arg(long, value_name = "VAULT_ID")]
    id: Option<String>,
    #[arg(long, conflicts_with = "no_semantic")]
    semantic: bool,
    #[arg(long, conflicts_with = "semantic")]
    no_semantic: bool,
    /// Accept the final setup plan without prompting.
    #[arg(long)]
    yes: bool,
    /// Print the setup plan without changing files or services.
    #[arg(long)]
    dry_run: bool,
    /// Emit one versioned JSON document on stdout.
    #[arg(long)]
    json: bool,
    #[arg(long, value_name = "SECONDS")]
    timeout: Option<u64>,
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    Install(ServiceMutationArgs),
    Start(ServiceMutationArgs),
    Stop(ServiceMutationArgs),
    Restart(ServiceMutationArgs),
    Status {
        #[arg(long)]
        json: bool,
    },
    Uninstall(ServiceMutationArgs),
}

#[derive(Debug, Args)]
struct ServiceMutationArgs {
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum VaultCommand {
    /// Register a Markdown/text tree.
    Add {
        #[arg(long)]
        id: String,
        #[arg(long, value_name = "DIRECTORY")]
        path: PathBuf,
        #[arg(long)]
        room: Option<String>,
    },
}

#[derive(Debug, Serialize)]
struct ServiceReport {
    schema_version: u32,
    command: &'static str,
    ok: bool,
    dry_run: bool,
    manager: &'static str,
    status: ServiceStatusReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ServiceStatusReport {
    installed: bool,
    enabled: bool,
    running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl From<ServiceStatus> for ServiceStatusReport {
    fn from(status: ServiceStatus) -> Self {
        Self {
            installed: status.installed,
            enabled: status.enabled,
            running: status.running,
            detail: status.detail,
        }
    }
}

struct SetupServiceAdapter<'a> {
    manager: &'a dyn ServiceManager,
    executable: PathBuf,
}

impl SetupServiceManager for SetupServiceAdapter<'_> {
    fn install(&mut self, spec: &ServiceInstallSpec) -> Result<(), SetupErrorCode> {
        let spec = ServiceSpec::new(
            self.executable.clone(),
            spec.config_path.clone(),
            spec.data_dir.clone(),
        )
        .map_err(|_| SetupErrorCode::ServiceInstallFailed)?;
        self.manager
            .install(&spec)
            .map_err(|_| SetupErrorCode::ServiceInstallFailed)
    }

    fn start(&mut self) -> Result<(), SetupErrorCode> {
        self.manager
            .start()
            .map_err(|_| SetupErrorCode::ServiceStartFailed)
    }

    fn restart(&mut self) -> Result<(), SetupErrorCode> {
        self.manager
            .restart()
            .map_err(|_| SetupErrorCode::ServiceRestartFailed)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::resolve(cli.config, cli.data_dir)?;

    match cli.command {
        Command::Setup(args) => {
            require_desktop_profile(&paths)?;
            run_setup(&paths, args)?;
        }
        Command::Service { command } => {
            require_desktop_profile(&paths)?;
            run_service(&paths, command)?;
        }
        Command::Vault {
            command: VaultCommand::Add { id, path, room },
        } => {
            let registration =
                update_config(&paths.config, |config| add_vault(config, id, path, room))?;
            println!(
                "Registered vault '{}' at {}",
                registration.id,
                registration.path.display()
            );
        }
        Command::Index => {
            let config = load_config(&paths.config).with_context(|| {
                format!(
                    "failed to load configuration from {}",
                    paths.config.display()
                )
            })?;
            let stats = build_index(&config, &paths.data_dir)?;
            for warning in &stats.warnings {
                eprintln!("warning: {}: {}", warning.path.display(), warning.message);
            }
            println!(
                "Indexed {} documents into {} chunks at {} ({} warnings)",
                stats.documents,
                stats.chunks,
                paths.data_dir.display(),
                stats.warnings.len()
            );
        }
        Command::Serve { bind, semantic } => server::serve(paths, bind, semantic).await?,
        Command::Search {
            query,
            limit,
            vault,
            json,
        } => {
            let hits = search_index(
                &paths.data_dir,
                &SearchRequest {
                    query,
                    limit,
                    vault_id: vault,
                },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
            } else if hits.is_empty() {
                println!("No results.");
            } else {
                for (index, hit) in hits.iter().enumerate() {
                    let heading = if hit.heading_path.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", hit.heading_path.join(" > "))
                    };
                    println!(
                        "{}. [{:.4}] {}:{}{}\n   {}",
                        index + 1,
                        hit.score,
                        hit.vault_id,
                        hit.path,
                        heading,
                        hit.excerpt.replace('\n', " ")
                    );
                }
            }
        }
    }

    Ok(())
}

fn require_desktop_profile(paths: &Paths) -> Result<()> {
    if load_config(&paths.config)?.server.profile != HostProfile::Desktop {
        bail!("setup and per-user service commands are unavailable for the openclast profile");
    }
    Ok(())
}

fn run_setup(paths: &Paths, args: SetupArgs) -> Result<()> {
    if !args.dry_run && ensure_native_lifecycle_supported().is_err() {
        return emit_setup_error(
            SetupErrorCode::UnsupportedEnvironment,
            args.json,
            args.dry_run,
        );
    }

    let manager = create_service_manager()?;
    let executable = std::env::current_exe().context("could not resolve the kwiry executable")?;
    let service_spec = ServiceSpec::new(
        executable.clone(),
        paths.config.clone(),
        paths.data_dir.clone(),
    )?;
    let snapshot = setup_snapshot(paths, manager.as_ref(), &service_spec, args.dry_run)
        .map_err(|code| anyhow!(code))?;
    let interactive = !args.json && io::stdin().is_terminal() && io::stderr().is_terminal();
    if !interactive && !args.dry_run && !args.yes {
        return emit_setup_error(SetupErrorCode::PromptRequired, args.json, args.dry_run);
    }

    let semantic = if args.semantic {
        Some(true)
    } else if args.no_semantic {
        Some(false)
    } else if interactive {
        None
    } else if snapshot.config_exists {
        Some(snapshot.config.semantic.enabled)
    } else {
        Some(true)
    };
    let request = SetupRequest {
        vault_path: args.tree,
        vault_id: args.id,
        room: None,
        semantic,
        dry_run: args.dry_run,
    };

    let resolved = if interactive {
        let mut prompt = StdioPrompt;
        resolve_setup_input(&request, &snapshot, &mut prompt)
    } else {
        resolve_setup_input(&request, &snapshot, &mut NoPrompt)
    };
    let resolved = match resolved {
        Ok(resolved) => resolved,
        Err(code) => return emit_setup_error(code, args.json, args.dry_run),
    };
    let plan = match plan_setup(resolved, &snapshot, paths) {
        Ok(plan) => plan,
        Err(code) => return emit_setup_error(code, args.json, args.dry_run),
    };

    if interactive && !args.json {
        print_setup_plan(&plan);
        if !args.yes && !args.dry_run {
            let mut prompt = StdioPrompt;
            if !prompt
                .confirm("Apply this setup plan?", true)
                .map_err(|code| anyhow!(code))?
            {
                println!("Setup cancelled; no changes were made.");
                return Ok(());
            }
        }
    }

    if !args.json && !args.dry_run {
        for action in plan
            .actions
            .iter()
            .filter(|action| action.disposition == ActionDisposition::Apply)
        {
            eprintln!("setup: {}", action.reason);
        }
    }

    let mut service = SetupServiceAdapter {
        manager: manager.as_ref(),
        executable,
    };
    let timeout = Duration::from_secs(args.timeout.unwrap_or(if plan.input.semantic_enabled {
        900
    } else {
        60
    }));
    let mut readiness =
        HttpReadinessProbe::new(timeout, Duration::from_millis(250), Duration::from_secs(1));
    let report = SetupExecutor::new(paths, &mut service, &mut readiness).execute(&plan);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_setup_report(paths, &report);
    }
    if !report.ok {
        bail!("Kwiry setup did not complete");
    }
    Ok(())
}

fn setup_snapshot(
    paths: &Paths,
    manager: &dyn ServiceManager,
    spec: &ServiceSpec,
    allow_unknown_service: bool,
) -> Result<SetupSnapshot, SetupErrorCode> {
    let config_exists = paths.config.is_file();
    let config = load_config(&paths.config).map_err(|_| SetupErrorCode::ConfigUpdateFailed)?;
    let bootstrap_ready = config.auth.token_file.is_some() && paths.token_path(&config).is_file();
    let index_ready = DataRoot::new(&paths.data_dir)
        .active()
        .map(|active| active.is_some())
        .unwrap_or(false);
    let native = manager.status();
    let service = match &native {
        Ok(status) if !status.installed => SetupServiceState::NotInstalled,
        Ok(status) if status.running => SetupServiceState::Running,
        Ok(_) => SetupServiceState::Stopped,
        Err(_) if allow_unknown_service => SetupServiceState::Unknown,
        Err(_) => return Err(SetupErrorCode::ServiceInspectFailed),
    };
    let running = native.as_ref().is_ok_and(|status| status.running);
    let service_definition_current = if native.as_ref().is_ok_and(|status| status.installed) {
        match manager.definition_matches(spec) {
            Ok(matches) => matches,
            Err(_) if allow_unknown_service => false,
            Err(_) => return Err(SetupErrorCode::ServiceInspectFailed),
        }
    } else {
        false
    };
    Ok(SetupSnapshot {
        config_exists,
        daemon: SetupDaemonSnapshot {
            reachable: running,
            state: running.then_some(DaemonState::Ready),
            semantic_ready: running && config.semantic.enabled,
        },
        config,
        bootstrap_ready,
        index_ready,
        service,
        service_definition_current,
    })
}

fn emit_setup_error(code: SetupErrorCode, json: bool, dry_run: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "ok": false,
                "dry_run": dry_run,
                "issues": [{ "code": code, "message": code.message() }]
            }))?
        );
    }
    Err(anyhow!(code))
}

fn print_setup_plan(plan: &SetupPlan) {
    println!("\nSetup plan for '{}':", plan.input.vault.id);
    println!("  Tree: {}", plan.input.vault.path.display());
    println!(
        "  Semantic search: {}",
        if plan.input.semantic_enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    for action in &plan.actions {
        let marker = match action.disposition {
            ActionDisposition::Apply => "will",
            ActionDisposition::AlreadySatisfied => "ready",
            ActionDisposition::Skipped => "skip",
        };
        println!("  - {marker}: {}", action.reason);
    }
    println!();
}

fn print_setup_report(paths: &Paths, report: &crate::setup::SetupReport) {
    if report.dry_run {
        println!("Dry run complete; no changes were made.");
    } else if report.ok {
        println!("Kwiry setup complete.");
    } else {
        println!("Kwiry setup stopped before readiness.");
    }
    println!(
        "Tree: {} ({})",
        report.vault_path.display(),
        report.vault_id
    );
    println!(
        "Semantic search: {}",
        if report.semantic_enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    if let Some(connection) = &report.connection {
        println!("Daemon: {}", connection.url);
    }
    println!(
        "Connection descriptor: {}",
        paths.connection_path().display()
    );
    println!("Logs: {}", paths.logs_dir().display());
    for issue in &report.issues {
        eprintln!("setup error: {}", issue.message);
    }
}

fn run_service(paths: &Paths, command: ServiceCommand) -> Result<()> {
    let manager = create_service_manager()?;
    match command {
        ServiceCommand::Status { json } => {
            let status = manager.status()?;
            emit_service_report("status", false, json, manager.kind(), status, None)
        }
        ServiceCommand::Install(args) => {
            run_service_mutation(paths, manager.as_ref(), "install", args, |manager, spec| {
                manager.install(spec)
            })
        }
        ServiceCommand::Start(args) => {
            run_service_mutation(paths, manager.as_ref(), "start", args, |manager, _| {
                manager.start()
            })
        }
        ServiceCommand::Stop(args) => {
            run_service_mutation(paths, manager.as_ref(), "stop", args, |manager, _| {
                manager.stop()
            })
        }
        ServiceCommand::Restart(args) => {
            run_service_mutation(paths, manager.as_ref(), "restart", args, |manager, _| {
                manager.restart()
            })
        }
        ServiceCommand::Uninstall(args) => {
            run_service_mutation(paths, manager.as_ref(), "uninstall", args, |manager, _| {
                manager.uninstall()
            })
        }
    }
}

fn run_service_mutation(
    paths: &Paths,
    manager: &dyn ServiceManager,
    command: &'static str,
    args: ServiceMutationArgs,
    operation: impl FnOnce(&dyn ServiceManager, &ServiceSpec) -> Result<()>,
) -> Result<()> {
    let executable = std::env::current_exe().context("could not resolve the kwiry executable")?;
    let spec = ServiceSpec::new(executable, paths.config.clone(), paths.data_dir.clone())?;
    if args.dry_run {
        let status = manager
            .status()
            .unwrap_or_else(|_| ServiceStatus::not_installed(manager.kind()));
        return emit_service_report(command, true, args.json, manager.kind(), status, None);
    }
    if let Err(error) = ensure_native_lifecycle_supported() {
        let status = manager
            .status()
            .unwrap_or_else(|_| ServiceStatus::not_installed(manager.kind()));
        emit_service_report(
            command,
            false,
            args.json,
            manager.kind(),
            status,
            Some(error.to_string()),
        )?;
        return Err(error);
    }
    let _setup_lock =
        acquire_setup_lock(paths).context("another setup or service operation is running")?;
    if let Err(error) = operation(manager, &spec) {
        let status = manager
            .status()
            .unwrap_or_else(|_| ServiceStatus::not_installed(manager.kind()));
        emit_service_report(
            command,
            false,
            args.json,
            manager.kind(),
            status,
            Some(error.to_string()),
        )?;
        return Err(error);
    }
    let status = manager.status()?;
    emit_service_report(command, false, args.json, manager.kind(), status, None)
}

fn emit_service_report(
    command: &'static str,
    dry_run: bool,
    json: bool,
    manager: ServiceManagerKind,
    status: ServiceStatus,
    error: Option<String>,
) -> Result<()> {
    let report = ServiceReport {
        schema_version: 1,
        command,
        ok: error.is_none(),
        dry_run,
        manager: service_manager_name(manager),
        status: status.into(),
        error,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if report.dry_run {
        println!("Would {command} the {} service.", report.manager);
    } else {
        println!(
            "Kwiry service: installed={}, enabled={}, running={} ({})",
            report.status.installed, report.status.enabled, report.status.running, report.manager
        );
        if let Some(detail) = &report.status.detail {
            println!("{detail}");
        }
        if let Some(error) = &report.error {
            eprintln!("service error: {error}");
        }
    }
    Ok(())
}

fn service_manager_name(kind: ServiceManagerKind) -> &'static str {
    match kind {
        #[cfg(any(target_os = "linux", test))]
        ServiceManagerKind::SystemdUser => "systemd_user",
        #[cfg(any(windows, test))]
        ServiceManagerKind::TaskScheduler => "task_scheduler",
    }
}

#[cfg(target_os = "linux")]
fn create_service_manager() -> Result<Box<dyn ServiceManager>> {
    Ok(Box::new(
        service::linux::LinuxSystemdManager::for_current_user()?,
    ))
}

#[cfg(windows)]
fn create_service_manager() -> Result<Box<dyn ServiceManager>> {
    Ok(Box::new(service::windows::WindowsTaskScheduler::new(
        ProcessCommandRunner,
    )))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn create_service_manager() -> Result<Box<dyn ServiceManager>> {
    bail!("per-user service management is supported on native Windows and Linux only")
}

#[cfg(target_os = "linux")]
fn ensure_native_lifecycle_supported() -> Result<()> {
    match service::linux::LinuxEnvironment::detect() {
        service::linux::LinuxEnvironment::Native => Ok(()),
        service::linux::LinuxEnvironment::Wsl { marker } => bail!(
            "unsupported_environment: setup and service changes are disabled under WSL ({marker}); use the native Windows binary or run `kwiry serve` manually for development"
        ),
    }
}

#[cfg(windows)]
fn ensure_native_lifecycle_supported() -> Result<()> {
    Ok(())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn ensure_native_lifecycle_supported() -> Result<()> {
    bail!("per-user service management is supported on native Windows and Linux only")
}
