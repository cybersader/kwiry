use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(any(windows, test))]
pub(crate) mod windows;

#[cfg(target_os = "linux")]
pub(crate) const SERVICE_ID: &str = "kwiry";
pub(crate) type ServiceResult<T> = Result<T>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceSpec {
    pub(crate) executable: PathBuf,
    pub(crate) config: PathBuf,
    pub(crate) data_dir: PathBuf,
}

impl ServiceSpec {
    pub(crate) fn new(
        executable: PathBuf,
        config: PathBuf,
        data_dir: PathBuf,
    ) -> ServiceResult<Self> {
        let spec = Self {
            executable,
            config,
            data_dir,
        };
        spec.validate()?;
        Ok(spec)
    }

    pub(crate) fn validate(&self) -> ServiceResult<()> {
        for (label, path) in [
            ("executable", &self.executable),
            ("configuration", &self.config),
            ("data directory", &self.data_dir),
        ] {
            if !path.is_absolute() {
                bail!(
                    "invalid service specification: {label} path must be absolute: {}",
                    path.display()
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceManagerKind {
    #[cfg(any(target_os = "linux", test))]
    #[cfg_attr(all(test, windows), allow(dead_code))]
    SystemdUser,
    #[cfg(any(windows, test))]
    TaskScheduler,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceStatus {
    pub(crate) manager: ServiceManagerKind,
    pub(crate) installed: bool,
    pub(crate) enabled: bool,
    pub(crate) running: bool,
    pub(crate) detail: Option<String>,
}

impl ServiceStatus {
    pub(crate) fn not_installed(manager: ServiceManagerKind) -> Self {
        Self {
            manager,
            installed: false,
            enabled: false,
            running: false,
            detail: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandOutput {
    pub(crate) code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl CommandOutput {
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn success(&self) -> bool {
        self.code == Some(0)
    }
}

pub(crate) trait CommandRunner: Send + Sync {
    fn run(&self, program: &OsStr, args: &[OsString]) -> ServiceResult<CommandOutput>;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(&self, program: &OsStr, args: &[OsString]) -> ServiceResult<CommandOutput> {
        let output = Command::new(program).args(args).output().with_context(|| {
            format!(
                "failed to execute {} without a shell",
                Path::new(program).display()
            )
        })?;
        Ok(CommandOutput {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

pub(crate) trait ServiceManager: Send + Sync {
    fn kind(&self) -> ServiceManagerKind;
    fn definition_matches(&self, spec: &ServiceSpec) -> ServiceResult<bool>;
    fn install(&self, spec: &ServiceSpec) -> ServiceResult<()>;
    fn start(&self) -> ServiceResult<()>;
    fn stop(&self) -> ServiceResult<()>;
    fn restart(&self) -> ServiceResult<()>;
    fn status(&self) -> ServiceResult<ServiceStatus>;
    fn uninstall(&self) -> ServiceResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_spec_requires_absolute_paths() {
        let spec = ServiceSpec::new(
            PathBuf::from("/opt/Kwiry App/kwiry"),
            PathBuf::from("/home/alice/.config/kwiry/config.toml"),
            PathBuf::from("/home/alice/.local/share/kwiry/index"),
        )
        .unwrap();

        assert_eq!(spec.executable, Path::new("/opt/Kwiry App/kwiry"));
        assert!(
            ServiceSpec::new(
                PathBuf::from("kwiry"),
                PathBuf::from("/config.toml"),
                PathBuf::from("/data"),
            )
            .is_err()
        );
    }

    #[test]
    fn captured_command_runner_never_interprets_shell_syntax() {
        let output = ProcessCommandRunner
            .run(
                OsStr::new("printf"),
                &[OsString::from("%s"), OsString::from("literal; false")],
            )
            .unwrap();

        assert!(output.success());
        assert_eq!(output.stdout, "literal; false");
    }
}
