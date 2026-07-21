use kwiry_core::Paths;

use super::model::{
    ReadinessExpectation, ResolvedSetupInput, SETUP_REPORT_SCHEMA_VERSION, ServiceInstallSpec,
    SetupAction, SetupActionKind, SetupErrorCode, SetupPlan, SetupServiceState, SetupSnapshot,
};

pub fn plan_setup(
    input: ResolvedSetupInput,
    snapshot: &SetupSnapshot,
    paths: &Paths,
) -> Result<SetupPlan, SetupErrorCode> {
    let existing_by_id = snapshot
        .config
        .vaults
        .iter()
        .find(|vault| vault.id == input.vault.id);
    let existing_by_path = snapshot
        .config
        .vaults
        .iter()
        .find(|vault| vault.path == input.vault.path);

    if let Some(existing) = existing_by_id {
        if existing.path != input.vault.path || existing.room != input.vault.room {
            return Err(SetupErrorCode::VaultIdConflict);
        }
    } else if existing_by_path.is_some() {
        return Err(SetupErrorCode::VaultPathConflict);
    }

    let registration_changed = existing_by_id.is_none();
    let semantic_changed = snapshot.config.semantic.enabled != input.semantic_enabled;
    let config_changed = registration_changed || semantic_changed;
    let mut actions = Vec::with_capacity(8);

    if registration_changed {
        actions.push(SetupAction::apply(
            SetupActionKind::EnsureVaultRegistration,
            "register the selected vault",
        ));
    } else {
        actions.push(SetupAction::satisfied(
            SetupActionKind::EnsureVaultRegistration,
            "the selected vault registration already matches",
        ));
    }

    if semantic_changed {
        actions.push(SetupAction::apply(
            SetupActionKind::SetSemanticPreference,
            "persist the selected semantic-search preference",
        ));
    } else {
        actions.push(SetupAction::satisfied(
            SetupActionKind::SetSemanticPreference,
            "the semantic-search preference already matches",
        ));
    }

    if snapshot.bootstrap_ready {
        actions.push(SetupAction::satisfied(
            SetupActionKind::BootstrapDesktop,
            "the local authentication token is already available",
        ));
    } else {
        actions.push(SetupAction::apply(
            SetupActionKind::BootstrapDesktop,
            "create or validate the local authentication token",
        ));
    }

    if registration_changed && snapshot.service == SetupServiceState::Running {
        actions.push(SetupAction::skipped(
            SetupActionKind::BuildIndex,
            "the running daemon will reconcile the registration change",
        ));
    } else if registration_changed || !snapshot.index_ready {
        actions.push(SetupAction::apply(
            SetupActionKind::BuildIndex,
            if registration_changed {
                "build the index after registration changes"
            } else {
                "build the missing index"
            },
        ));
    } else if semantic_changed {
        actions.push(SetupAction::skipped(
            SetupActionKind::BuildIndex,
            "semantic vectors are reconciled by the daemon; the lexical index is current",
        ));
    } else {
        actions.push(SetupAction::satisfied(
            SetupActionKind::BuildIndex,
            "the index is already present",
        ));
    }

    match snapshot.service {
        SetupServiceState::NotInstalled => actions.push(SetupAction::apply(
            SetupActionKind::InstallService,
            "install the per-user daemon service",
        )),
        SetupServiceState::Stopped | SetupServiceState::Running
            if snapshot.service_definition_current =>
        {
            actions.push(SetupAction::satisfied(
                SetupActionKind::InstallService,
                "the per-user daemon service definition already matches",
            ));
        }
        SetupServiceState::Stopped | SetupServiceState::Running => {
            actions.push(SetupAction::apply(
                SetupActionKind::InstallService,
                "update the per-user daemon service definition",
            ));
        }
        SetupServiceState::Unknown => actions.push(SetupAction::apply(
            SetupActionKind::InstallService,
            "reconcile the per-user daemon service installation",
        )),
    }

    match snapshot.service {
        SetupServiceState::Running
            if config_changed
                || !snapshot.service_definition_current
                || !snapshot.daemon.reachable
                || (input.semantic_enabled && !snapshot.daemon.semantic_ready) =>
        {
            actions.push(SetupAction::apply(
                SetupActionKind::RestartService,
                "restart the daemon to apply configuration or recover readiness",
            ));
        }
        SetupServiceState::Running => actions.push(SetupAction::satisfied(
            SetupActionKind::StartService,
            "the daemon service is already running",
        )),
        SetupServiceState::NotInstalled
        | SetupServiceState::Stopped
        | SetupServiceState::Unknown => actions.push(SetupAction::apply(
            SetupActionKind::StartService,
            "start the per-user daemon service",
        )),
    }

    actions.push(SetupAction::apply(
        SetupActionKind::CheckReadiness,
        "verify the loopback health and authenticated status endpoints",
    ));

    Ok(SetupPlan {
        schema_version: SETUP_REPORT_SCHEMA_VERSION,
        service: ServiceInstallSpec {
            config_path: paths.config.clone(),
            data_dir: paths.data_dir.clone(),
            semantic_enabled: input.semantic_enabled,
        },
        readiness: ReadinessExpectation {
            connection_path: paths.connection_path(),
            vault_id: input.vault.id.clone(),
            semantic_enabled: input.semantic_enabled,
        },
        input,
        actions,
    })
}
