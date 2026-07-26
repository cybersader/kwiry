use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{RwLock, mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};

use kwiry_core::{Config, DaemonState, DaemonStatus, Paths, ReconcileScope, load_config};

use crate::runtime::ManagerHandle;
use crate::server::status_from_manifest;

const DEBOUNCE: Duration = Duration::from_millis(300);
const MAX_DEBOUNCE: Duration = Duration::from_secs(2);
const SAFETY_RECONCILE: Duration = Duration::from_secs(60);
/// Distinct pending watcher paths before escalating to one full pass.
const MAX_PENDING_PATHS: usize = 4_096;

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

/// Why a batch must escalate to one full authoritative pass instead of a
/// path-scoped one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RescanCause {
    ChannelOverflow,
    BackendRescan,
    WatcherError,
    WatchRootFailed,
    PathCapacity,
    ConfigChanged,
    FailedPass,
}

/// One event's contribution to the pending batch.
#[derive(Debug, PartialEq, Eq)]
enum EventEffect {
    ConfigChanged,
    Rescan(RescanCause),
    Touched(Vec<(String, String)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchRoot {
    vault_id: String,
    root: PathBuf,
}

/// Bounded accumulator of watcher evidence between passes. The rescan
/// latch is one-way for a batch: once any signal is untrustworthy, the
/// whole batch escalates to a full pass and path accumulation stops.
#[derive(Debug, Default)]
struct PendingPaths {
    touched: BTreeSet<(String, String)>,
    rescan: Option<RescanCause>,
}

impl PendingPaths {
    fn note_rescan(&mut self, cause: RescanCause) {
        if self.rescan.is_none() {
            self.rescan = Some(cause);
        }
        self.touched.clear();
    }

    fn insert(&mut self, key: (String, String)) {
        if self.rescan.is_some() {
            return;
        }
        self.touched.insert(key);
        if self.touched.len() > MAX_PENDING_PATHS {
            self.note_rescan(RescanCause::PathCapacity);
        }
    }

    fn is_empty(&self) -> bool {
        self.touched.is_empty() && self.rescan.is_none()
    }

    fn take_scope(&mut self) -> ReconcileScope {
        if self.rescan.take().is_some() {
            self.touched.clear();
            ReconcileScope::Full
        } else {
            ReconcileScope::Paths(std::mem::take(&mut self.touched))
        }
    }

    /// Requeues the batch a failed pass consumed so retry happens at the
    /// next debounce window rather than the next safety tick.
    fn merge_back(&mut self, scope: ReconcileScope) {
        match scope {
            ReconcileScope::Full => self.note_rescan(RescanCause::FailedPass),
            ReconcileScope::Paths(paths) => {
                for path in paths {
                    self.insert(path);
                }
            }
        }
    }
}

/// Maps one raw watcher delivery onto the pending batch. Any signal that
/// cannot be classified trustworthily escalates; nothing is silently
/// dropped in the unsafe direction.
fn classify_event(
    event: &notify::Result<Event>,
    roots: &[WatchRoot],
    config_path: &Path,
) -> EventEffect {
    let event = match event {
        Ok(event) => event,
        Err(_) => return EventEffect::Rescan(RescanCause::WatcherError),
    };
    if event.need_rescan() {
        return EventEffect::Rescan(RescanCause::BackendRescan);
    }
    if event.paths.iter().any(|path| path == config_path) {
        return EventEffect::ConfigChanged;
    }
    let mut touched = Vec::new();
    for path in &event.paths {
        // A path may fall under several nested registered roots; each
        // registered vault enumerates independently, so record all.
        for root in roots {
            let Ok(relative) = path.strip_prefix(&root.root) else {
                continue;
            };
            match normalize_relative(relative) {
                Some(relative) => touched.push((root.vault_id.clone(), relative)),
                // Byte-identical to walk.rs normalization: a component
                // that is not valid UTF-8 cannot be matched to a source
                // key, so the batch must escalate.
                None => return EventEffect::Rescan(RescanCause::WatcherError),
            }
        }
    }
    EventEffect::Touched(touched)
}

/// Must match `walk.rs` exactly: forward-slash join of UTF-8 components.
fn normalize_relative(relative: &Path) -> Option<String> {
    let components: Option<Vec<_>> = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect();
    Some(components?.join("/"))
}

fn watch_roots(config: &Config) -> Vec<WatchRoot> {
    config
        .vaults
        .iter()
        .map(|vault| WatchRoot {
            vault_id: vault.id.clone(),
            root: vault.path.clone(),
        })
        .collect()
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
    let startup_config = config.clone();
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
    let mut pending = PendingPaths::default();
    let mut watched_roots = BTreeSet::new();
    let mut unwatched_roots = add_roots(&mut watcher, &config, &mut watched_roots);
    if unwatched_roots {
        pending.note_rescan(RescanCause::WatchRootFailed);
        mark_degraded(&status).await;
    }
    let mut roots = watch_roots(&config);

    let mut config_dirty = false;
    let mut config_invalid = false;
    let mut restart_required = false;
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
                let effect = classify_event(&event, &roots, &paths.config);
                if matches!(effect, EventEffect::Touched(ref touched) if touched.is_empty()) {
                    // Outside every registered root and not the config:
                    // nothing this daemon indexes can have changed.
                    continue;
                }
                let now = Instant::now();
                first_dirty.get_or_insert(now);
                last_dirty = Some(now);
                match effect {
                    EventEffect::ConfigChanged => {
                        config_dirty = true;
                        pending.note_rescan(RescanCause::ConfigChanged);
                    }
                    EventEffect::Rescan(cause) => pending.note_rescan(cause),
                    EventEffect::Touched(touched) => {
                        for key in touched {
                            pending.insert(key);
                        }
                    }
                }
                status.write().await.dirty = true;
            }
            _ = debounce.tick() => {
                let now = Instant::now();
                if overflow.swap(false, Ordering::AcqRel) {
                    // A dropped delivery may have been anything, including
                    // a config write.
                    config_dirty = true;
                    pending.note_rescan(RescanCause::ChannelOverflow);
                    first_dirty.get_or_insert(now);
                    last_dirty = Some(now);
                    status.write().await.dirty = true;
                }
                if pending.is_empty() && !config_dirty {
                    continue;
                }
                let quiet = last_dirty.is_some_and(|last| now.duration_since(last) >= DEBOUNCE);
                let overdue = first_dirty.is_some_and(|first| now.duration_since(first) >= MAX_DEBOUNCE);
                if quiet || overdue {
                    if config_dirty {
                        match load_config(&paths.config) {
                            Ok(next) => {
                                config_invalid = false;
                                if startup_config.requires_restart_for(&next) {
                                    restart_required = true;
                                    mark_degraded(&status).await;
                                } else {
                                    restart_required = false;
                                    if next != config {
                                        reconfigure_roots(&mut watcher, &next, &mut watched_roots);
                                        config = next;
                                        roots = watch_roots(&config);
                                    }
                                }
                            }
                            Err(_) => {
                                config_invalid = true;
                                mark_degraded(&status).await;
                                pending = PendingPaths::default();
                                config_dirty = false;
                                first_dirty = None;
                                last_dirty = None;
                                continue;
                            }
                        }
                    }
                    unwatched_roots = add_roots(&mut watcher, &config, &mut watched_roots);
                    if unwatched_roots {
                        // Events for an unwatched root never arrive, so a
                        // scoped pass would miss its changes.
                        pending.note_rescan(RescanCause::WatchRootFailed);
                    }
                    let scope = pending.take_scope();
                    config_dirty = false;
                    first_dirty = None;
                    last_dirty = None;
                    let succeeded = reconcile_and_update(
                        &manager,
                        &config,
                        &status,
                        scope.clone(),
                        config_invalid || restart_required || unwatched_roots,
                    )
                    .await;
                    if !succeeded {
                        // Requeue so the retry happens at the next quiet
                        // debounce window, not the next safety tick.
                        pending.merge_back(scope);
                        let retry = Instant::now();
                        first_dirty = Some(retry);
                        last_dirty = Some(retry);
                        status.write().await.dirty = true;
                    }
                }
            }
            _ = safety.tick() => {
                unwatched_roots = add_roots(&mut watcher, &config, &mut watched_roots);
                reconcile_and_update(
                    &manager,
                    &config,
                    &status,
                    ReconcileScope::Full,
                    config_invalid || restart_required || unwatched_roots,
                )
                .await;
            }
        }
    }
}

/// Returns whether any existing vault root could not be watched.
fn add_roots(
    watcher: &mut RecommendedWatcher,
    config: &Config,
    watched_roots: &mut BTreeSet<PathBuf>,
) -> bool {
    let mut failed = false;
    for vault in &config.vaults {
        if !vault.path.is_dir() || watched_roots.contains(&vault.path) {
            continue;
        }
        if watcher.watch(&vault.path, RecursiveMode::Recursive).is_ok() {
            watched_roots.insert(vault.path.clone());
        } else {
            failed = true;
        }
    }
    failed
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
    scope: ReconcileScope,
    force_degraded: bool,
) -> bool {
    {
        let mut current = status.write().await;
        current.rebuilding = true;
        current.dirty = true;
    }
    match manager.reconcile_scoped(config.clone(), scope).await {
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
            true
        }
        Err(_) => {
            mark_degraded(status).await;
            false
        }
    }
}

async fn mark_degraded(status: &Arc<RwLock<DaemonStatus>>) {
    let mut status = status.write().await;
    status.state = DaemonState::Degraded;
    status.dirty = true;
    status.rebuilding = false;
}

#[cfg(test)]
mod tests {
    use notify::EventKind;
    use notify::event::{CreateKind, Flag};

    use super::*;

    fn roots() -> Vec<WatchRoot> {
        vec![
            WatchRoot {
                vault_id: "outer".into(),
                root: PathBuf::from("/vaults/outer"),
            },
            WatchRoot {
                vault_id: "inner".into(),
                root: PathBuf::from("/vaults/outer/inner"),
            },
        ]
    }

    fn config_path() -> PathBuf {
        PathBuf::from("/state/config.toml")
    }

    #[test]
    fn classify_event_maps_err_to_rescan() {
        let error: notify::Result<Event> = Err(notify::Error::generic("backend failure"));
        assert_eq!(
            classify_event(&error, &roots(), &config_path()),
            EventEffect::Rescan(RescanCause::WatcherError)
        );
    }

    #[test]
    fn classify_event_maps_backend_rescan_flag_to_rescan() {
        let mut event = Event::new(EventKind::Create(CreateKind::File));
        event = event.set_flag(Flag::Rescan);
        assert_eq!(
            classify_event(&Ok(event), &roots(), &config_path()),
            EventEffect::Rescan(RescanCause::BackendRescan)
        );
    }

    #[test]
    fn classify_event_detects_the_config_file() {
        let event = Event::new(EventKind::Create(CreateKind::File)).add_path(config_path());
        assert_eq!(
            classify_event(&Ok(event), &roots(), &config_path()),
            EventEffect::ConfigChanged
        );
    }

    #[test]
    fn classify_event_normalizes_rename_pairs_and_nested_roots() {
        let event = Event::new(EventKind::Create(CreateKind::File))
            .add_path(PathBuf::from("/vaults/outer/a/from.md"))
            .add_path(PathBuf::from("/vaults/outer/inner/to.md"));
        let EventEffect::Touched(mut touched) =
            classify_event(&Ok(event), &roots(), &config_path())
        else {
            panic!("expected touched paths");
        };
        touched.sort();
        assert_eq!(
            touched,
            vec![
                ("inner".to_owned(), "to.md".to_owned()),
                ("outer".to_owned(), "a/from.md".to_owned()),
                ("outer".to_owned(), "inner/to.md".to_owned()),
            ]
        );
    }

    #[test]
    fn classify_event_outside_every_root_touches_nothing() {
        let event = Event::new(EventKind::Create(CreateKind::File))
            .add_path(PathBuf::from("/elsewhere/note.md"));
        assert_eq!(
            classify_event(&Ok(event), &roots(), &config_path()),
            EventEffect::Touched(Vec::new())
        );
    }

    #[test]
    fn capacity_overflow_latches_rescan_and_stops_accumulating() {
        let mut pending = PendingPaths::default();
        for index in 0..=MAX_PENDING_PATHS {
            pending.insert(("vault".to_owned(), format!("note-{index}.md")));
        }
        assert_eq!(pending.rescan, Some(RescanCause::PathCapacity));
        assert!(pending.touched.is_empty());
        assert_eq!(pending.take_scope(), ReconcileScope::Full);
        assert!(pending.is_empty());
    }

    #[test]
    fn take_scope_drains_paths_and_merge_back_requeues_them() {
        let mut pending = PendingPaths::default();
        pending.insert(("vault".to_owned(), "note.md".to_owned()));
        let scope = pending.take_scope();
        assert_eq!(
            scope,
            ReconcileScope::Paths(BTreeSet::from([("vault".to_owned(), "note.md".to_owned())]))
        );
        assert!(pending.is_empty());

        pending.merge_back(scope);
        assert!(!pending.is_empty());
        assert_eq!(
            pending.take_scope(),
            ReconcileScope::Paths(BTreeSet::from([("vault".to_owned(), "note.md".to_owned())]))
        );

        pending.merge_back(ReconcileScope::Full);
        assert_eq!(pending.rescan, Some(RescanCause::FailedPass));
        assert_eq!(pending.take_scope(), ReconcileScope::Full);
    }
}
