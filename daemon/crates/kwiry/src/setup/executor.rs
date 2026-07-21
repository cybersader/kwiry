use kwiry_core::{
    Error as CoreError, Paths, acquire_setup_lock, bootstrap_desktop, build_index,
    ensure_vault_registration, load_config, update_config,
};

use super::model::{
    ActionDisposition, ServiceInstallSpec, SetupActionKind, SetupActionOutcome, SetupErrorCode,
    SetupIssue, SetupPlan, SetupReport,
};
use super::readiness::ReadinessProbe;

pub trait SetupServiceManager {
    fn install(&mut self, spec: &ServiceInstallSpec) -> Result<(), SetupErrorCode>;
    fn start(&mut self) -> Result<(), SetupErrorCode>;
    fn restart(&mut self) -> Result<(), SetupErrorCode>;
}

pub struct SetupExecutor<'a, S, R> {
    paths: &'a Paths,
    service: &'a mut S,
    readiness: &'a mut R,
}

impl<'a, S, R> SetupExecutor<'a, S, R>
where
    S: SetupServiceManager,
    R: ReadinessProbe,
{
    pub fn new(paths: &'a Paths, service: &'a mut S, readiness: &'a mut R) -> Self {
        Self {
            paths,
            service,
            readiness,
        }
    }

    pub fn execute(&mut self, plan: &SetupPlan) -> SetupReport {
        let mut report = SetupReport::from_plan(plan);
        if plan.input.dry_run {
            return report;
        }

        let _setup_lock = match acquire_setup_lock(self.paths) {
            Ok(lock) => lock,
            Err(CoreError::LockHeld(_)) => {
                fail_report(&mut report, None, SetupErrorCode::SetupLocked);
                return report;
            }
            Err(_) => {
                fail_report(&mut report, None, SetupErrorCode::ConfigUpdateFailed);
                return report;
            }
        };

        for (index, action) in plan.actions.iter().enumerate() {
            if action.disposition != ActionDisposition::Apply {
                continue;
            }
            let result = match action.kind {
                SetupActionKind::EnsureVaultRegistration => {
                    update_config(&self.paths.config, |config| {
                        ensure_vault_registration(
                            config,
                            plan.input.vault.id.clone(),
                            plan.input.vault.path.clone(),
                            plan.input.vault.room.clone(),
                        )
                        .map(|_| ())
                    })
                    .map_err(|_| SetupErrorCode::ConfigUpdateFailed)
                }
                SetupActionKind::SetSemanticPreference => {
                    update_config(&self.paths.config, |config| {
                        config.semantic.enabled = plan.input.semantic_enabled;
                        Ok(())
                    })
                    .map_err(|_| SetupErrorCode::ConfigUpdateFailed)
                }
                SetupActionKind::BootstrapDesktop => bootstrap_desktop(self.paths)
                    .map(|_| ())
                    .map_err(|_| SetupErrorCode::TokenUnavailable),
                SetupActionKind::BuildIndex => load_config(&self.paths.config)
                    .and_then(|config| build_index(&config, &self.paths.data_dir).map(|_| ()))
                    .map_err(|_| SetupErrorCode::IndexBuildFailed),
                SetupActionKind::InstallService => self
                    .service
                    .install(&plan.service)
                    .map_err(|_| SetupErrorCode::ServiceInstallFailed),
                SetupActionKind::StartService => self
                    .service
                    .start()
                    .map_err(|_| SetupErrorCode::ServiceStartFailed),
                SetupActionKind::RestartService => self
                    .service
                    .restart()
                    .map_err(|_| SetupErrorCode::ServiceRestartFailed),
                SetupActionKind::CheckReadiness => self
                    .readiness
                    .wait_until_ready(&plan.readiness)
                    .map(|connection| {
                        report.connection = Some(connection);
                    }),
            };

            match result {
                Ok(()) => report.actions[index].outcome = SetupActionOutcome::Succeeded,
                Err(code) => {
                    fail_report(&mut report, Some(index), code);
                    for remaining in report.actions.iter_mut().skip(index + 1) {
                        if remaining.outcome == SetupActionOutcome::Planned {
                            remaining.outcome = SetupActionOutcome::Skipped;
                        }
                    }
                    return report;
                }
            }
        }

        report.ok = true;
        report
    }
}

fn fail_report(report: &mut SetupReport, action_index: Option<usize>, code: SetupErrorCode) {
    let issue = SetupIssue::from(code);
    report.ok = false;
    report.issues.push(issue.clone());
    if let Some(index) = action_index {
        report.actions[index].outcome = SetupActionOutcome::Failed;
        report.actions[index].issue = Some(issue);
    }
}
