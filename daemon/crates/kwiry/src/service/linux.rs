use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow, bail};

use super::{
    CommandOutput, CommandRunner, ProcessCommandRunner, SERVICE_ID, ServiceManager,
    ServiceManagerKind, ServiceSpec, ServiceStatus,
};

const GENERATED_HEADER: &str = "# Managed by kwiry (service unit schema 1).\n";
const UNIT_DESCRIPTION: &str = "Kwiry knowledge workspace search daemon";
const SYSTEMCTL: &str = "systemctl";
const UNIT_NAME: &str = "kwiry.service";
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinuxEnvironment {
    Native,
    Wsl { marker: String },
}

impl LinuxEnvironment {
    pub(crate) fn detect() -> Self {
        let kernel_release = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        let kernel_version = fs::read_to_string("/proc/version").unwrap_or_default();
        Self::from_markers(
            &kernel_release,
            &kernel_version,
            env::var_os("WSL_INTEROP").as_deref(),
            env::var_os("WSL_DISTRO_NAME").as_deref(),
        )
    }

    fn from_markers(
        kernel_release: &str,
        kernel_version: &str,
        wsl_interop: Option<&OsStr>,
        wsl_distro_name: Option<&OsStr>,
    ) -> Self {
        if wsl_interop.is_some_and(|value| !value.is_empty()) {
            return Self::Wsl {
                marker: "WSL_INTEROP is set".to_owned(),
            };
        }
        if wsl_distro_name.is_some_and(|value| !value.is_empty()) {
            return Self::Wsl {
                marker: "WSL_DISTRO_NAME is set".to_owned(),
            };
        }

        for (source, value) in [
            ("kernel release", kernel_release),
            ("kernel version", kernel_version),
        ] {
            let lowercase = value.to_ascii_lowercase();
            if lowercase.contains("microsoft") || lowercase.contains("wsl") {
                return Self::Wsl {
                    marker: format!("{source} identifies WSL"),
                };
            }
        }

        Self::Native
    }
}

pub(crate) struct LinuxSystemdManager<R = ProcessCommandRunner> {
    runner: R,
    unit_dir: PathBuf,
    systemctl: PathBuf,
    environment: LinuxEnvironment,
}

impl LinuxSystemdManager<ProcessCommandRunner> {
    pub(crate) fn for_current_user() -> Result<Self> {
        Ok(Self::new(
            ProcessCommandRunner,
            user_unit_dir()?,
            LinuxEnvironment::detect(),
        ))
    }
}

impl<R> LinuxSystemdManager<R> {
    pub(crate) fn new(runner: R, unit_dir: PathBuf, environment: LinuxEnvironment) -> Self {
        Self {
            runner,
            unit_dir,
            systemctl: PathBuf::from(SYSTEMCTL),
            environment,
        }
    }

    pub(crate) fn unit_path(&self) -> PathBuf {
        self.unit_dir.join(UNIT_NAME)
    }

    fn ensure_mutation_supported(&self) -> Result<()> {
        match &self.environment {
            LinuxEnvironment::Native => Ok(()),
            LinuxEnvironment::Wsl { marker } => bail!(
                "unsupported_environment: native systemd user-service changes are disabled under WSL ({marker}); run Kwiry manually or install the native Windows service"
            ),
        }
    }
}

impl<R: CommandRunner> LinuxSystemdManager<R> {
    fn run_systemctl(&self, operation: &str, args: &[OsString]) -> Result<CommandOutput> {
        self.runner
            .run(self.systemctl.as_os_str(), args)
            .with_context(|| format!("could not execute systemctl to {operation}"))
    }

    fn run_required(&self, operation: &str, args: &[OsString]) -> Result<CommandOutput> {
        let output = self.run_systemctl(operation, args)?;
        if output.success() {
            Ok(output)
        } else {
            Err(native_error(operation, &output))
        }
    }

    fn run_idempotent(
        &self,
        operation: &str,
        args: &[OsString],
        accepted_failure: impl FnOnce(&str) -> bool,
    ) -> Result<CommandOutput> {
        let output = self.run_systemctl(operation, args)?;
        if output.success() || accepted_failure(&command_message(&output)) {
            Ok(output)
        } else {
            Err(native_error(operation, &output))
        }
    }

    fn daemon_reload(&self) -> Result<()> {
        self.run_required(
            "reload the user service manager",
            &systemctl_args(&["daemon-reload"]),
        )?;
        Ok(())
    }

    fn query_status(&self) -> Result<ServiceStatus> {
        let output = self.run_systemctl("query the Kwiry service", &show_args())?;
        if !output.success() {
            if is_missing(&command_message(&output)) {
                return Ok(ServiceStatus::not_installed(
                    ServiceManagerKind::SystemdUser,
                ));
            }
            return Err(native_error("query the Kwiry service", &output));
        }
        parse_show_properties(&output.stdout)
    }
}

impl<R: CommandRunner> ServiceManager for LinuxSystemdManager<R> {
    fn kind(&self) -> ServiceManagerKind {
        ServiceManagerKind::SystemdUser
    }

    fn definition_matches(&self, spec: &ServiceSpec) -> Result<bool> {
        spec.validate()?;
        let unit_path = self.unit_path();
        match inspect_unit(&unit_path)? {
            ExistingUnit::Missing => Ok(false),
            ExistingUnit::Foreign => bail!(
                "refusing to inspect service unit not generated by Kwiry: {}",
                unit_path.display()
            ),
            ExistingUnit::Generated => {
                let current = fs::read_to_string(&unit_path)
                    .with_context(|| format!("could not read {}", unit_path.display()))?;
                Ok(current == render_unit(spec)?)
            }
        }
    }

    fn install(&self, spec: &ServiceSpec) -> Result<()> {
        self.ensure_mutation_supported()?;
        spec.validate()?;
        let unit_path = self.unit_path();
        let previous = match inspect_unit(&unit_path)? {
            ExistingUnit::Missing => None,
            ExistingUnit::Generated => Some(
                fs::read(&unit_path)
                    .with_context(|| format!("could not read {}", unit_path.display()))?,
            ),
            ExistingUnit::Foreign => bail!(
                "refusing to replace service unit not generated by Kwiry: {}",
                unit_path.display()
            ),
        };

        let unit = render_unit(spec)?;
        write_unit_atomic(&unit_path, unit.as_bytes())?;
        if let Err(error) = self.daemon_reload() {
            restore_unit(&unit_path, previous.as_deref())?;
            return Err(error);
        }
        self.run_idempotent(
            "enable the Kwiry service",
            &systemctl_args(&["enable", UNIT_NAME]),
            is_already_enabled,
        )?;
        Ok(())
    }

    fn start(&self) -> Result<()> {
        self.ensure_mutation_supported()?;
        self.run_idempotent(
            "start the Kwiry service",
            &systemctl_args(&["start", UNIT_NAME]),
            is_already_active,
        )?;
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        self.ensure_mutation_supported()?;
        self.run_idempotent(
            "stop the Kwiry service",
            &systemctl_args(&["stop", UNIT_NAME]),
            |message| is_missing(message) || is_already_inactive(message),
        )?;
        Ok(())
    }

    fn restart(&self) -> Result<()> {
        self.ensure_mutation_supported()?;
        self.run_required(
            "restart the Kwiry service",
            &systemctl_args(&["restart", UNIT_NAME]),
        )?;
        Ok(())
    }

    fn status(&self) -> Result<ServiceStatus> {
        self.query_status()
    }

    fn uninstall(&self) -> Result<()> {
        self.ensure_mutation_supported()?;
        let unit_path = self.unit_path();
        match inspect_unit(&unit_path)? {
            ExistingUnit::Missing => return Ok(()),
            ExistingUnit::Foreign => bail!(
                "refusing to remove service unit not generated by Kwiry: {}",
                unit_path.display()
            ),
            ExistingUnit::Generated => {}
        }
        let generated_unit = fs::read(&unit_path)
            .with_context(|| format!("could not read {}", unit_path.display()))?;

        self.run_idempotent(
            "stop the Kwiry service",
            &systemctl_args(&["stop", UNIT_NAME]),
            |message| is_missing(message) || is_already_inactive(message),
        )?;
        self.run_idempotent(
            "disable the Kwiry service",
            &systemctl_args(&["disable", UNIT_NAME]),
            |message| is_missing(message) || is_already_disabled(message),
        )?;
        fs::remove_file(&unit_path)
            .with_context(|| format!("could not remove {}", unit_path.display()))?;
        sync_directory(&self.unit_dir)?;
        if let Err(error) = self.daemon_reload() {
            write_unit_atomic(&unit_path, &generated_unit)?;
            return Err(error);
        }
        Ok(())
    }
}

pub(crate) fn render_unit(spec: &ServiceSpec) -> Result<String> {
    spec.validate()?;
    let executable = quote_systemd_argument(&spec.executable, "executable")?;
    let config = quote_systemd_argument(&spec.config, "configuration file")?;
    let data_dir = quote_systemd_argument(&spec.data_dir, "data directory")?;
    Ok(format!(
        "{GENERATED_HEADER}[Unit]\nDescription={UNIT_DESCRIPTION}\n\n[Service]\nType=simple\nExecStart={executable} \"--config\" {config} \"--data-dir\" {data_dir} \"serve\"\nRestart=on-failure\nRestartSec=5s\nUMask=0077\nNoNewPrivileges=true\n\n[Install]\nWantedBy=default.target\n"
    ))
}

fn quote_systemd_argument(path: &Path, label: &str) -> Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow!("Kwiry service {label} path is not valid Unicode"))?;
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '%' => escaped.push_str("%%"),
            '$' => escaped.push_str("$$"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write as _;

                let mut encoded = [0_u8; 4];
                for byte in character.encode_utf8(&mut encoded).as_bytes() {
                    write!(escaped, "\\x{byte:02x}").expect("writing to String cannot fail");
                }
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    Ok(escaped)
}

fn user_unit_dir() -> Result<PathBuf> {
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME") {
        let config_home = PathBuf::from(config_home);
        if !config_home.is_absolute() {
            bail!(
                "XDG_CONFIG_HOME must be absolute: {}",
                config_home.display()
            );
        }
        return Ok(config_home.join("systemd/user"));
    }

    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set"))?;
    if !home.is_absolute() {
        bail!("HOME must be absolute: {}", home.display());
    }
    Ok(home.join(".config/systemd/user"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExistingUnit {
    Missing,
    Generated,
    Foreign,
}

fn inspect_unit(path: &Path) -> Result<ExistingUnit> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(ExistingUnit::Missing);
        }
        Err(source) => {
            return Err(source).with_context(|| format!("could not inspect {}", path.display()));
        }
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Ok(ExistingUnit::Foreign);
    }
    let content = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    if content.starts_with(GENERATED_HEADER.as_bytes()) {
        Ok(ExistingUnit::Generated)
    } else {
        Ok(ExistingUnit::Foreign)
    }
}

fn write_unit_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("service unit path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "could not create service unit directory {}",
            parent.display()
        )
    })?;
    let metadata = fs::symlink_metadata(parent).with_context(|| {
        format!(
            "could not inspect service unit directory {}",
            parent.display()
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "service unit directory must be a real directory: {}",
            parent.display()
        );
    }
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "could not secure service unit directory {}",
            parent.display()
        )
    })?;

    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary_path = parent.join(format!(
        ".{SERVICE_ID}.service.tmp.{}.{}",
        std::process::id(),
        sequence
    ));
    let write_result = (|| {
        let mut temporary = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary_path)
            .with_context(|| format!("could not create {}", temporary_path.display()))?;
        temporary
            .write_all(content)
            .with_context(|| format!("could not write {}", temporary_path.display()))?;
        temporary
            .sync_all()
            .with_context(|| format!("could not sync {}", temporary_path.display()))?;
        fs::set_permissions(&temporary_path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("could not secure {}", temporary_path.display()))?;
        fs::rename(&temporary_path, path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        sync_directory(parent)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    write_result
}

fn restore_unit(path: &Path, previous: Option<&[u8]>) -> Result<()> {
    match previous {
        Some(content) => write_unit_atomic(path, content),
        None => {
            match fs::remove_file(path) {
                Ok(()) => sync_directory(path.parent().ok_or_else(|| {
                    anyhow!("service unit path has no parent: {}", path.display())
                })?),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => {
                    Err(error).with_context(|| format!("could not remove {}", path.display()))
                }
            }
        }
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("could not sync directory {}", path.display()))
}

fn parse_show_properties(source: &str) -> Result<ServiceStatus> {
    let mut properties = BTreeMap::new();
    for line in source.lines().filter(|line| !line.trim().is_empty()) {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| anyhow!("systemctl show returned malformed line: {line:?}"))?;
        properties.insert(key.trim(), value.trim());
    }

    let load_state = properties
        .get("LoadState")
        .copied()
        .ok_or_else(|| anyhow!("systemctl show omitted LoadState"))?;
    if load_state == "not-found" {
        return Ok(ServiceStatus::not_installed(
            ServiceManagerKind::SystemdUser,
        ));
    }

    let unit_file_state = properties.get("UnitFileState").copied().unwrap_or_default();
    let active_state = properties.get("ActiveState").copied().unwrap_or("unknown");
    let sub_state = properties.get("SubState").copied().unwrap_or("unknown");
    let main_pid = properties.get("MainPID").copied().unwrap_or("0");
    if main_pid != "0" {
        main_pid
            .parse::<u32>()
            .with_context(|| format!("systemctl show returned invalid MainPID {main_pid:?}"))?;
    }

    Ok(ServiceStatus {
        manager: ServiceManagerKind::SystemdUser,
        installed: true,
        enabled: matches!(unit_file_state, "enabled" | "enabled-runtime"),
        running: active_state == "active",
        detail: Some(format!(
            "load={load_state}; unit_file={unit_file_state}; active={active_state}; sub={sub_state}; pid={main_pid}"
        )),
    })
}

fn systemctl_args(arguments: &[&str]) -> Vec<OsString> {
    std::iter::once(OsString::from("--user"))
        .chain(arguments.iter().map(OsString::from))
        .collect()
}

fn show_args() -> Vec<OsString> {
    systemctl_args(&[
        "show",
        UNIT_NAME,
        "--property=LoadState",
        "--property=UnitFileState",
        "--property=ActiveState",
        "--property=SubState",
        "--property=MainPID",
        "--no-pager",
    ])
}

fn command_message(output: &CommandOutput) -> String {
    format!("{}\n{}", output.stdout, output.stderr).to_ascii_lowercase()
}

fn is_missing(message: &str) -> bool {
    [
        "not loaded",
        "does not exist",
        "not-found",
        "not found",
        "could not be found",
        "no files found",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

fn is_already_enabled(message: &str) -> bool {
    message.contains("already enabled")
}

fn is_already_disabled(message: &str) -> bool {
    message.contains("already disabled") || message.contains("not enabled")
}

fn is_already_active(message: &str) -> bool {
    message.contains("already active") || message.contains("already running")
}

fn is_already_inactive(message: &str) -> bool {
    message.contains("already inactive")
        || message.contains("not active")
        || message.contains("not running")
}

fn native_error(operation: &str, output: &CommandOutput) -> anyhow::Error {
    let detail = sanitized_native_detail(output);
    let code = output.code.map_or_else(
        || "terminated without an exit code".to_owned(),
        |code| format!("exit code {code}"),
    );
    anyhow!("systemctl could not {operation} ({code}): {detail}")
}

fn sanitized_native_detail(output: &CommandOutput) -> String {
    let raw = if output.stderr.trim().is_empty() {
        output.stdout.as_str()
    } else {
        output.stderr.as_str()
    };
    let single_line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.is_empty() {
        return "no diagnostic was provided".to_owned();
    }

    const MAX_CHARS: usize = 240;
    let mut characters = single_line.chars();
    let mut sanitized: String = characters.by_ref().take(MAX_CHARS).collect();
    if characters.next().is_some() {
        sanitized.push('…');
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use tempfile::tempdir;

    use super::*;

    type CommandCall = (OsString, Vec<OsString>);

    #[derive(Clone, Default)]
    struct FakeCommandRunner {
        calls: Arc<Mutex<Vec<CommandCall>>>,
        outputs: Arc<Mutex<VecDeque<CommandOutput>>>,
    }

    impl FakeCommandRunner {
        fn with_outputs(outputs: impl IntoIterator<Item = CommandOutput>) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                outputs: Arc::new(Mutex::new(outputs.into_iter().collect())),
            }
        }

        fn calls(&self) -> Vec<(OsString, Vec<OsString>)> {
            self.calls.lock().unwrap().clone()
        }

        fn clear_calls(&self) {
            self.calls.lock().unwrap().clear();
        }
    }

    impl CommandRunner for FakeCommandRunner {
        fn run(&self, program: &OsStr, args: &[OsString]) -> Result<CommandOutput> {
            self.calls
                .lock()
                .unwrap()
                .push((program.to_owned(), args.to_vec()));
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow!("unexpected command"))
        }
    }

    fn spec(root: &Path) -> ServiceSpec {
        ServiceSpec::new(
            root.join("bin/kwiry"),
            root.join("config/config.toml"),
            root.join("data/index"),
        )
        .unwrap()
    }

    fn output(code: i32, stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            code: Some(code),
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        }
    }

    fn call_words(call: &(OsString, Vec<OsString>)) -> Vec<String> {
        std::iter::once(call.0.to_string_lossy().into_owned())
            .chain(
                call.1
                    .iter()
                    .map(|argument| argument.to_string_lossy().into_owned()),
            )
            .collect()
    }

    #[test]
    fn unit_rendering_is_exact_and_escapes_systemd_syntax() {
        let spec = ServiceSpec::new(
            PathBuf::from(r#"/opt/Kwiry app/kwiry "β"\tool%name$"#),
            PathBuf::from(r#"/home/Zoë/Kwiry %n/"quoted"\config.toml"#),
            PathBuf::from(r#"/home/Zoë/data\set %i"#),
        )
        .unwrap();

        let rendered = render_unit(&spec).unwrap();

        assert_eq!(
            rendered,
            concat!(
                "# Managed by kwiry (service unit schema 1).\n",
                "[Unit]\n",
                "Description=Kwiry knowledge workspace search daemon\n",
                "\n",
                "[Service]\n",
                "Type=simple\n",
                "ExecStart=\"/opt/Kwiry app/kwiry \\\"β\\\"\\\\tool%%name$$\" ",
                "\"--config\" \"/home/Zoë/Kwiry %%n/\\\"quoted\\\"\\\\config.toml\" ",
                "\"--data-dir\" \"/home/Zoë/data\\\\set %%i\" \"serve\"\n",
                "Restart=on-failure\n",
                "RestartSec=5s\n",
                "UMask=0077\n",
                "NoNewPrivileges=true\n",
                "\n",
                "[Install]\n",
                "WantedBy=default.target\n",
            )
        );
    }

    #[test]
    fn rendered_unit_has_no_semantic_flag_or_token_material() {
        let temporary = tempdir().unwrap();
        let sentinel = "KWIRY_SECRET_SENTINEL_7fa962";
        fs::write(temporary.path().join("config.token"), sentinel).unwrap();
        let rendered = render_unit(&spec(temporary.path())).unwrap();

        assert!(!rendered.contains(sentinel));
        assert!(!rendered.contains("--token"));
        assert!(!rendered.contains("--semantic"));
        assert!(rendered.ends_with("WantedBy=default.target\n"));
    }

    #[test]
    fn lifecycle_actions_use_exact_systemctl_argv_without_a_shell() {
        let temporary = tempdir().unwrap();
        let runner = FakeCommandRunner::with_outputs([
            output(0, "", ""),
            output(1, "", "Unit file is already enabled"),
            output(1, "", "Unit is already active"),
            output(1, "", "Unit is not running"),
            output(0, "", ""),
        ]);
        let manager = LinuxSystemdManager::new(
            runner.clone(),
            temporary.path().join("units"),
            LinuxEnvironment::Native,
        );

        manager.install(&spec(temporary.path())).unwrap();
        manager.start().unwrap();
        manager.stop().unwrap();
        manager.restart().unwrap();

        let calls: Vec<Vec<String>> = runner.calls().iter().map(call_words).collect();
        assert_eq!(
            calls,
            vec![
                vec!["systemctl", "--user", "daemon-reload"],
                vec!["systemctl", "--user", "enable", UNIT_NAME],
                vec!["systemctl", "--user", "start", UNIT_NAME],
                vec!["systemctl", "--user", "stop", UNIT_NAME],
                vec!["systemctl", "--user", "restart", UNIT_NAME],
            ]
        );
        assert!(
            calls
                .iter()
                .all(|call| call[0] != "sh" && !call.contains(&"-c".to_owned()))
        );
    }

    #[test]
    fn status_parsing_is_order_independent_and_available_under_wsl() {
        let temporary = tempdir().unwrap();
        let runner = FakeCommandRunner::with_outputs([output(
            0,
            "SubState=running\nMainPID=4242\nIgnored=field\nActiveState=active\nLoadState=loaded\nUnitFileState=enabled\n",
            "",
        )]);
        let manager = LinuxSystemdManager::new(
            runner.clone(),
            temporary.path().join("units"),
            LinuxEnvironment::Wsl {
                marker: "test marker".to_owned(),
            },
        );

        let status = manager.status().unwrap();

        assert_eq!(status.manager, ServiceManagerKind::SystemdUser);
        assert!(status.installed);
        assert!(status.enabled);
        assert!(status.running);
        assert_eq!(
            status.detail.as_deref(),
            Some("load=loaded; unit_file=enabled; active=active; sub=running; pid=4242")
        );
        assert_eq!(call_words(&runner.calls()[0]), {
            let mut expected = vec!["systemctl".to_owned()];
            expected.extend(
                show_args()
                    .iter()
                    .map(|value| value.to_string_lossy().into_owned()),
            );
            expected
        });
    }

    #[test]
    fn missing_status_and_stop_are_idempotent() {
        let temporary = tempdir().unwrap();
        let runner = FakeCommandRunner::with_outputs([
            output(4, "", "Unit kwiry.service could not be found."),
            output(5, "", "Unit kwiry.service not loaded."),
        ]);
        let manager = LinuxSystemdManager::new(
            runner,
            temporary.path().join("units"),
            LinuxEnvironment::Native,
        );

        assert_eq!(
            manager.status().unwrap(),
            ServiceStatus::not_installed(ServiceManagerKind::SystemdUser)
        );
        manager.stop().unwrap();
    }

    #[test]
    fn wsl_detection_uses_kernel_and_environment_markers() {
        assert!(matches!(
            LinuxEnvironment::from_markers("6.6.87.2-microsoft-standard-WSL2", "", None, None),
            LinuxEnvironment::Wsl { .. }
        ));
        assert!(matches!(
            LinuxEnvironment::from_markers(
                "6.8.0",
                "Linux version 6.8.0",
                Some(OsStr::new("/run/WSL/1")),
                None
            ),
            LinuxEnvironment::Wsl { .. }
        ));
        assert!(matches!(
            LinuxEnvironment::from_markers(
                "6.8.0",
                "Linux version 6.8.0",
                None,
                Some(OsStr::new("Ubuntu"))
            ),
            LinuxEnvironment::Wsl { .. }
        ));
        assert_eq!(
            LinuxEnvironment::from_markers("6.8.0", "Linux version 6.8.0", None, None),
            LinuxEnvironment::Native
        );
    }

    #[test]
    fn wsl_blocks_mutations_before_filesystem_or_command_changes() {
        let temporary = tempdir().unwrap();
        let runner = FakeCommandRunner::default();
        let unit_dir = temporary.path().join("units");
        let manager = LinuxSystemdManager::new(
            runner.clone(),
            unit_dir.clone(),
            LinuxEnvironment::Wsl {
                marker: "kernel release identifies WSL".to_owned(),
            },
        );

        for result in [
            manager.install(&spec(temporary.path())),
            manager.start(),
            manager.stop(),
            manager.restart(),
            manager.uninstall(),
        ] {
            let error = result.unwrap_err().to_string();
            assert!(error.starts_with("unsupported_environment:"), "{error}");
        }
        assert!(!unit_dir.exists());
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn install_is_atomic_owner_only_and_refuses_foreign_units() {
        let temporary = tempdir().unwrap();
        let runner = FakeCommandRunner::with_outputs([output(0, "", ""), output(0, "", "")]);
        let unit_dir = temporary.path().join("units");
        let manager =
            LinuxSystemdManager::new(runner.clone(), unit_dir.clone(), LinuxEnvironment::Native);
        let spec = spec(temporary.path());

        manager.install(&spec).unwrap();
        let unit_path = manager.unit_path();
        assert_eq!(
            fs::metadata(&unit_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&unit_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::read_to_string(&unit_path).unwrap(),
            render_unit(&spec).unwrap()
        );
        assert_eq!(fs::read_dir(&unit_dir).unwrap().count(), 1);
        assert!(manager.definition_matches(&spec).unwrap());
        let mut moved = spec.clone();
        moved.executable = temporary.path().join("other/kwiry");
        assert!(!manager.definition_matches(&moved).unwrap());

        fs::write(&unit_path, "[Unit]\nDescription=Owner managed\n").unwrap();
        runner.clear_calls();
        let error = manager.install(&spec).unwrap_err().to_string();
        assert!(error.contains("not generated by Kwiry"));
        assert_eq!(
            fs::read_to_string(&unit_path).unwrap(),
            "[Unit]\nDescription=Owner managed\n"
        );
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn uninstall_only_removes_generated_unit_and_preserves_foreign_state() {
        let temporary = tempdir().unwrap();
        let runner = FakeCommandRunner::default();
        let unit_dir = temporary.path().join("units");
        fs::create_dir_all(&unit_dir).unwrap();
        let manager = LinuxSystemdManager::new(runner.clone(), unit_dir, LinuxEnvironment::Native);
        let unit_path = manager.unit_path();
        let foreign = "[Unit]\nDescription=Do not touch\n";
        fs::write(&unit_path, foreign).unwrap();

        let error = manager.uninstall().unwrap_err().to_string();
        assert!(error.contains("not generated by Kwiry"));
        assert_eq!(fs::read_to_string(&unit_path).unwrap(), foreign);
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn generated_uninstall_stops_disables_removes_and_reloads() {
        let temporary = tempdir().unwrap();
        let runner = FakeCommandRunner::with_outputs([
            output(0, "", ""),
            output(0, "", ""),
            output(1, "", "Unit is not running"),
            output(1, "", "Unit is already disabled"),
            output(0, "", ""),
        ]);
        let manager = LinuxSystemdManager::new(
            runner.clone(),
            temporary.path().join("units"),
            LinuxEnvironment::Native,
        );

        manager.install(&spec(temporary.path())).unwrap();
        runner.clear_calls();
        manager.uninstall().unwrap();

        assert!(!manager.unit_path().exists());
        let calls: Vec<Vec<String>> = runner.calls().iter().map(call_words).collect();
        assert_eq!(
            calls,
            vec![
                vec!["systemctl", "--user", "stop", UNIT_NAME],
                vec!["systemctl", "--user", "disable", UNIT_NAME],
                vec!["systemctl", "--user", "daemon-reload"],
            ]
        );

        runner.clear_calls();
        manager.uninstall().unwrap();
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn empty_wsl_environment_markers_do_not_report_wsl() {
        assert_eq!(
            LinuxEnvironment::from_markers(
                "6.8.0",
                "Linux version 6.8.0",
                Some(OsStr::new("")),
                Some(OsStr::new("")),
            ),
            LinuxEnvironment::Native
        );
    }

    #[test]
    fn failed_install_reload_restores_previous_generated_unit() {
        let temporary = tempdir().unwrap();
        let runner = FakeCommandRunner::with_outputs([output(1, "", "reload failed")]);
        let unit_dir = temporary.path().join("units");
        let manager = LinuxSystemdManager::new(runner, unit_dir.clone(), LinuxEnvironment::Native);
        fs::create_dir_all(&unit_dir).unwrap();
        let previous = format!("{GENERATED_HEADER}[Unit]\nDescription=previous\n");
        fs::write(manager.unit_path(), &previous).unwrap();

        assert!(manager.install(&spec(temporary.path())).is_err());
        assert_eq!(fs::read_to_string(manager.unit_path()).unwrap(), previous);
    }

    #[test]
    fn failed_uninstall_reload_restores_generated_unit() {
        let temporary = tempdir().unwrap();
        let runner = FakeCommandRunner::with_outputs([
            output(0, "", ""),
            output(0, "", ""),
            output(1, "", "reload failed"),
        ]);
        let unit_dir = temporary.path().join("units");
        let manager = LinuxSystemdManager::new(runner, unit_dir, LinuxEnvironment::Native);
        let rendered = render_unit(&spec(temporary.path())).unwrap();
        write_unit_atomic(&manager.unit_path(), rendered.as_bytes()).unwrap();

        assert!(manager.uninstall().is_err());
        assert_eq!(fs::read_to_string(manager.unit_path()).unwrap(), rendered);
    }

    #[test]
    fn malformed_status_is_rejected_instead_of_guessed() {
        let error = parse_show_properties("ActiveState=active\nMainPID=not-a-pid\n").unwrap_err();
        assert!(error.to_string().contains("omitted LoadState"));
    }
}
