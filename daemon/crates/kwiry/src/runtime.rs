use tokio::sync::{mpsc, oneshot};

use kwiry_core::{Config, IndexManager, ReconcileReport, ReconcileScope, Result, SearchRuntime};

#[derive(Clone)]
pub(crate) struct ManagerHandle {
    sender: mpsc::Sender<ManagerCommand>,
    runtime: SearchRuntime,
}

impl ManagerHandle {
    pub(crate) fn mark_resources_dirty(&self, config: &Config, scope: &ReconcileScope) {
        if let Err(error) = self.runtime.mark_resources_dirty(config, scope) {
            tracing::error!(%error, "could not record resource freshness");
        }
    }

    pub(crate) fn update_openclast_registrations(&self, config: &Config) {
        if config.server.profile == kwiry_core::HostProfile::OpenClast {
            self.runtime.set_openclast_registrations(config);
        }
    }

    pub(crate) async fn reconcile_scoped(
        &self,
        config: Config,
        scope: ReconcileScope,
    ) -> Result<ReconcileReport> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(ManagerCommand::Reconcile {
                config: Box::new(config),
                scope,
                reply,
            })
            .await
            .map_err(|_| kwiry_core::Error::State("index manager stopped".to_owned()))?;
        response
            .await
            .map_err(|_| kwiry_core::Error::State("index manager dropped response".to_owned()))?
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(ManagerCommand::Shutdown { reply })
            .await
            .map_err(|_| kwiry_core::Error::State("index manager stopped".to_owned()))?;
        response
            .await
            .map_err(|_| kwiry_core::Error::State("index manager dropped response".to_owned()))?
    }
}

enum ManagerCommand {
    Reconcile {
        config: Box<Config>,
        scope: ReconcileScope,
        reply: oneshot::Sender<Result<ReconcileReport>>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<()>>,
    },
}

pub(crate) fn spawn_manager(
    mut manager: IndexManager,
    runtime: SearchRuntime,
) -> (ManagerHandle, tokio::task::JoinHandle<()>) {
    let (sender, mut receiver) = mpsc::channel(16);
    let task = tokio::task::spawn_blocking(move || {
        while let Some(command) = receiver.blocking_recv() {
            match command {
                ManagerCommand::Reconcile {
                    config,
                    scope,
                    reply,
                } => {
                    let _ = reply.send(manager.reconcile_scoped(*config, &scope));
                }
                ManagerCommand::Shutdown { reply } => {
                    let _ = reply.send(manager.shutdown());
                    break;
                }
            }
        }
    });
    (ManagerHandle { sender, runtime }, task)
}
