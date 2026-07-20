use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{RwLock, mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};

use kwiry_core::{Config, DaemonState, DaemonStatus, Paths, load_config};

use crate::runtime::ManagerHandle;
use crate::server::status_from_manifest;

const DEBOUNCE: Duration = Duration::from_millis(300);
const MAX_DEBOUNCE: Duration = Duration::from_secs(2);
const SAFETY_RECONCILE: Duration = Duration::from_secs(60);

pub(crate) struct WatcherHandle {
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl WatcherHandle {
    pub(crate) async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        let _ = self.task.await;
    }
}

struct WatcherContext {
    paths: Paths,
    config: Config,
    manager: ManagerHandle,
    status: Arc<RwLock<DaemonStatus>>,
}

pub(crate) fn spawn_watcher(
    paths: Paths,
    initial_config: Config,
    manager: ManagerHandle,
    status: Arc<RwLock<DaemonStatus>>,
) -> Result<WatcherHandle> {
    let (events, receiver) = mpsc::channel(1_024);
    let overflow = Arc::new(AtomicBool::new(false));
    let callback_overflow = overflow.clone();
    let watcher = notify::recommended_watcher(move |event| {
        if events.try_send(event).is_err() {
            callback_overflow.store(true, Ordering::Release);
        }
    })
    .context("failed to create filesystem watcher")?;
    let (shutdown, shutdown_receiver) = watch::channel(false);
    let task = tokio::spawn(run_watcher(
        watcher,
        receiver,
        overflow,
        shutdown_receiver,
        WatcherContext {
            paths,
            config: initial_config,
            manager,
            status,
        },
    ));
    Ok(WatcherHandle { shutdown, task })
}

async fn run_watcher(
    mut watcher: RecommendedWatcher,
    mut events: mpsc::Receiver<notify::Result<Event>>,
    overflow: Arc<AtomicBool>,
    mut shutdown: watch::Receiver<bool>,
    context: WatcherContext,
) {
    let WatcherContext {
        paths,
        mut config,
        manager,
        status,
    } = context;
    let config_parent = paths
        .config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    if watcher
        .watch(&config_parent, RecursiveMode::NonRecursive)
        .is_err()
    {
        mark_degraded(&status).await;
    }
    let mut watched_roots = BTreeSet::new();
    add_roots(&mut watcher, &config, &mut watched_roots);

    let mut dirty = false;
    let mut config_dirty = false;
    let mut config_invalid = false;
    let mut first_dirty: Option<Instant> = None;
    let mut last_dirty: Option<Instant> = None;
    let mut debounce = tokio::time::interval(Duration::from_millis(100));
    debounce.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut safety = tokio::time::interval(SAFETY_RECONCILE);
    safety.set_missed_tick_behavior(MissedTickBehavior::Skip);
    safety.tick().await;

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            Some(event) = events.recv() => {
                let now = Instant::now();
                dirty = true;
                first_dirty.get_or_insert(now);
                last_dirty = Some(now);
                if let Ok(event) = event
                    && event.paths.iter().any(|path| path == &paths.config)
                {
                    config_dirty = true;
                }
                status.write().await.dirty = true;
            }
            _ = debounce.tick() => {
                let now = Instant::now();
                if overflow.swap(false, Ordering::AcqRel) {
                    dirty = true;
                    config_dirty = true;
                    first_dirty.get_or_insert(now);
                    last_dirty = Some(now);
                    status.write().await.dirty = true;
                }
                if !dirty {
                    continue;
                }
                let quiet = last_dirty.is_some_and(|last| now.duration_since(last) >= DEBOUNCE);
                let overdue = first_dirty.is_some_and(|first| now.duration_since(first) >= MAX_DEBOUNCE);
                if quiet || overdue {
                    if config_dirty {
                        match load_config(&paths.config) {
                            Ok(next) => {
                                config_invalid = false;
                                if next != config {
                                    reconfigure_roots(&mut watcher, &next, &mut watched_roots);
                                    config = next;
                                }
                            }
                            Err(_) => {
                                config_invalid = true;
                                mark_degraded(&status).await;
                                dirty = false;
                                config_dirty = false;
                                first_dirty = None;
                                last_dirty = None;
                                continue;
                            }
                        }
                    }
                    add_roots(&mut watcher, &config, &mut watched_roots);
                    reconcile_and_update(&manager, &config, &status, config_invalid).await;
                    dirty = false;
                    config_dirty = false;
                    first_dirty = None;
                    last_dirty = None;
                }
            }
            _ = safety.tick() => {
                add_roots(&mut watcher, &config, &mut watched_roots);
                reconcile_and_update(&manager, &config, &status, config_invalid).await;
            }
        }
    }
}

fn add_roots(
    watcher: &mut RecommendedWatcher,
    config: &Config,
    watched_roots: &mut BTreeSet<PathBuf>,
) {
    for vault in &config.vaults {
        if vault.path.is_dir()
            && watched_roots.insert(vault.path.clone())
            && watcher
                .watch(&vault.path, RecursiveMode::Recursive)
                .is_err()
        {
            watched_roots.remove(&vault.path);
        }
    }
}

fn reconfigure_roots(
    watcher: &mut RecommendedWatcher,
    config: &Config,
    watched_roots: &mut BTreeSet<PathBuf>,
) {
    let desired: BTreeSet<_> = config
        .vaults
        .iter()
        .filter(|vault| vault.path.is_dir())
        .map(|vault| vault.path.clone())
        .collect();
    let added: Vec<_> = desired.difference(watched_roots).cloned().collect();
    for root in added {
        if watcher.watch(&root, RecursiveMode::Recursive).is_ok() {
            watched_roots.insert(root);
        }
    }
    let removed: Vec<_> = watched_roots.difference(&desired).cloned().collect();
    for root in removed {
        let _ = watcher.unwatch(&root);
        watched_roots.remove(&root);
    }
}

async fn reconcile_and_update(
    manager: &ManagerHandle,
    config: &Config,
    status: &Arc<RwLock<DaemonStatus>>,
    force_degraded: bool,
) {
    match manager.reconcile(config.clone()).await {
        Ok(report) => {
            let mut next = status_from_manifest(
                config,
                &report.manifest,
                &report.unavailable_vaults,
                report.generation,
            );
            if force_degraded {
                next.state = DaemonState::Degraded;
                next.dirty = true;
            }
            let mut current = status.write().await;
            // The model identity never changes while the daemon runs.
            next.model = current.model.clone();
            *current = next;
        }
        Err(_) => mark_degraded(status).await,
    }
}

async fn mark_degraded(status: &Arc<RwLock<DaemonStatus>>) {
    let mut status = status.write().await;
    status.state = DaemonState::Degraded;
    status.dirty = true;
}
