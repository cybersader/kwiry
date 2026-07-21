use std::fmt;
use std::path::PathBuf;

use kwiry_core::{Config, DaemonState, VaultRegistration};
use serde::{Deserialize, Serialize};

pub const SETUP_REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<bool>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResolvedSetupInput {
    pub vault: VaultRegistration,
    pub semantic_enabled: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SetupServiceState {
    #[default]
    NotInstalled,
    Stopped,
    Running,
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupDaemonSnapshot {
    pub reachable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<DaemonState>,
    pub semantic_ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupSnapshot {
    pub config_exists: bool,
    pub config: Config,
    pub bootstrap_ready: bool,
    pub index_ready: bool,
    pub service: SetupServiceState,
    pub service_definition_current: bool,
    #[serde(default)]
    pub daemon: SetupDaemonSnapshot,
}

impl Default for SetupSnapshot {
    fn default() -> Self {
        Self {
            config_exists: false,
            config: Config::default(),
            bootstrap_ready: false,
            index_ready: false,
            service: SetupServiceState::NotInstalled,
            service_definition_current: false,
            daemon: SetupDaemonSnapshot::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceInstallSpec {
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub semantic_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReadinessExpectation {
    pub connection_path: PathBuf,
    pub vault_id: String,
    pub semantic_enabled: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SetupActionKind {
    EnsureVaultRegistration,
    SetSemanticPreference,
    BootstrapDesktop,
    BuildIndex,
    InstallService,
    StartService,
    RestartService,
    CheckReadiness,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionDisposition {
    Apply,
    AlreadySatisfied,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupAction {
    pub kind: SetupActionKind,
    pub disposition: ActionDisposition,
    pub reason: String,
}

impl SetupAction {
    pub(crate) fn apply(kind: SetupActionKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            disposition: ActionDisposition::Apply,
            reason: reason.into(),
        }
    }

    pub(crate) fn satisfied(kind: SetupActionKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            disposition: ActionDisposition::AlreadySatisfied,
            reason: reason.into(),
        }
    }

    pub(crate) fn skipped(kind: SetupActionKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            disposition: ActionDisposition::Skipped,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupPlan {
    pub schema_version: u32,
    pub input: ResolvedSetupInput,
    pub service: ServiceInstallSpec,
    pub readiness: ReadinessExpectation,
    pub actions: Vec<SetupAction>,
}

impl SetupPlan {
    pub fn changes_system(&self) -> bool {
        self.actions.iter().any(|action| {
            action.disposition == ActionDisposition::Apply
                && action.kind != SetupActionKind::CheckReadiness
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SetupErrorCode {
    PromptRequired,
    PromptIo,
    UnsupportedEnvironment,
    InvalidVaultPath,
    InvalidVaultId,
    VaultIdConflict,
    VaultPathConflict,
    SetupLocked,
    ConfigUpdateFailed,
    IndexBuildFailed,
    ServiceInspectFailed,
    ServiceInstallFailed,
    ServiceStartFailed,
    ServiceRestartFailed,
    ConnectionDescriptorMissing,
    ConnectionDescriptorInvalid,
    NonLoopbackConnection,
    TokenUnavailable,
    HealthCheckFailed,
    StatusCheckFailed,
    DaemonVersionMismatch,
    VaultNotReady,
    SemanticNotReady,
    ReadinessTimedOut,
}

impl SetupErrorCode {
    pub const fn message(self) -> &'static str {
        match self {
            Self::PromptRequired => "setup needs an interactive answer or an explicit option",
            Self::PromptIo => "setup could not read or write an interactive prompt",
            Self::UnsupportedEnvironment => {
                "per-user setup is supported on native Windows and Linux, not WSL"
            }
            Self::InvalidVaultPath => "the vault path must be an absolute readable directory",
            Self::InvalidVaultId => "the vault ID must contain at least one letter or number",
            Self::VaultIdConflict => "the vault ID is already registered with different settings",
            Self::VaultPathConflict => "the vault path is already registered with a different ID",
            Self::SetupLocked => "another setup operation is already running",
            Self::ConfigUpdateFailed => "setup could not update the configuration",
            Self::IndexBuildFailed => "setup could not build the index",
            Self::ServiceInspectFailed => "setup could not inspect the per-user service",
            Self::ServiceInstallFailed => "setup could not install the per-user service",
            Self::ServiceStartFailed => "setup could not start the per-user service",
            Self::ServiceRestartFailed => "setup could not restart the per-user service",
            Self::ConnectionDescriptorMissing => {
                "the daemon connection descriptor is not available"
            }
            Self::ConnectionDescriptorInvalid => "the daemon connection descriptor is invalid",
            Self::NonLoopbackConnection => "the daemon connection descriptor is not loopback-only",
            Self::TokenUnavailable => "the daemon bearer token file is unavailable",
            Self::HealthCheckFailed => "the daemon health check did not succeed",
            Self::StatusCheckFailed => "the authenticated daemon status check did not succeed",
            Self::DaemonVersionMismatch => {
                "the daemon status does not match the connection descriptor"
            }
            Self::VaultNotReady => "the configured vault is not ready in the daemon",
            Self::SemanticNotReady => "semantic search is enabled but the model is not ready",
            Self::ReadinessTimedOut => "the daemon did not become ready before the setup deadline",
        }
    }
}

impl fmt::Display for SetupErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for SetupErrorCode {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupIssue {
    pub code: SetupErrorCode,
    pub message: String,
}

impl From<SetupErrorCode> for SetupIssue {
    fn from(code: SetupErrorCode) -> Self {
        Self {
            code,
            message: code.message().to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SetupActionOutcome {
    Planned,
    AlreadySatisfied,
    Skipped,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupActionReport {
    pub kind: SetupActionKind,
    pub outcome: SetupActionOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<SetupIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupConnectionReport {
    pub url: String,
    pub daemon_version: String,
    pub state: DaemonState,
    pub semantic_ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupReport {
    pub schema_version: u32,
    pub ok: bool,
    pub dry_run: bool,
    pub vault_id: String,
    pub vault_path: PathBuf,
    pub semantic_enabled: bool,
    pub actions: Vec<SetupActionReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<SetupIssue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<SetupConnectionReport>,
}

impl SetupReport {
    pub fn from_plan(plan: &SetupPlan) -> Self {
        let actions = plan
            .actions
            .iter()
            .map(|action| SetupActionReport {
                kind: action.kind,
                outcome: match action.disposition {
                    ActionDisposition::Apply => SetupActionOutcome::Planned,
                    ActionDisposition::AlreadySatisfied => SetupActionOutcome::AlreadySatisfied,
                    ActionDisposition::Skipped => SetupActionOutcome::Skipped,
                },
                issue: None,
            })
            .collect();
        Self {
            schema_version: SETUP_REPORT_SCHEMA_VERSION,
            ok: true,
            dry_run: plan.input.dry_run,
            vault_id: plan.input.vault.id.clone(),
            vault_path: plan.input.vault.path.clone(),
            semantic_enabled: plan.input.semantic_enabled,
            actions,
            issues: Vec::new(),
            connection: None,
        }
    }
}
