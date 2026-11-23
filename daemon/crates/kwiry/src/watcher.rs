use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{RwLock, mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};

use kwiry_core::{Config, DaemonState, DaemonStatus, Paths, ReconcileScope, load_config_for_paths};

use crate::runtime::ManagerHandle;
use crate::server::status_from_manifest;

const DEBOUNCE: Duration = Duration::from_millis(300);
const MAX_DEBOUNCE: Duration = Duration::from_secs(2);
const SAFETY_RECONCILE: Duration = Duration::from_secs(60);
/// Distinct pending watcher paths before escalating to one full pass.
const MAX_PENDING_PATHS: usize = 4_096;

/// Even epochs are closed. An event received during a reload invalidates its
/// ticket, so an older read can never reopen the gate over a newer notification.
#[derive(Default)]
pub(crate) struct ConfigGate(AtomicU64);
impl ConfigGate {
    pub(crate) fn open(&self) -> bool {
        self.ticket() & 1 == 1
    }
    fn ticket(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn invalidations(&self) -> u64 {
        self.ticket() >> 1
    }
    fn invalidate(&self) {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                Some((n | 1).wrapping_add(1))
            })
            .unwrap();
    }
    pub(crate) fn validated(&self, ticket: u64) {
        let _ = self
            .0
            .compare_exchange(ticket, ticket | 1, Ordering::AcqRel, Ordering::Acquire);
    }
}

fn config_may_have_changed(event: &notify::Result<Event>, path: &Path) -> bool {
    matches!(
        classify_event(event, &[], path),
        EventEffect::ConfigChanged | EventEffect::Rescan(_)
    )
}

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
    live_gate: Option<Arc<ConfigGate>>,
    #[cfg(test)]
    safety_ticks: Option<mpsc::Receiver<()>>,
    initial_check: Option<(bool, bool)>, // config watch armed, bootstrap config still current
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
    TouchedRooms(Vec<(kwiry_core::ResourceKey, String)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchRoot {
    vault_id: String,
    resource: Option<kwiry_core::ResourceKey>,
    root: PathBuf,
}

/// Bounded accumulator of watcher evidence between passes. The rescan
/// latch is one-way for a batch: once any signal is untrustworthy, the
/// whole batch escalates to a full pass and path accumulation stops.
#[derive(Debug, Default)]
struct PendingPaths {
    touched: BTreeSet<(String, String)>,
    touched_rooms: BTreeSet<(kwiry_core::ResourceKey, String)>,
    rescan: Option<RescanCause>,
}

impl PendingPaths {
    fn note_rescan(&mut self, cause: RescanCause) {
        if self.rescan.is_none() {
            self.rescan = Some(cause);
        }
        self.touched.clear();
        self.touched_rooms.clear();
    }

    fn insert(&mut self, key: (String, String)) {
        if self.rescan.is_some() {
            return;
        }
        self.touched.insert(key);
        if self.touched.len() + self.touched_rooms.len() > MAX_PENDING_PATHS {
            self.note_rescan(RescanCause::PathCapacity);
        }
    }

    fn insert_room(&mut self, key: (kwiry_core::ResourceKey, String)) {
        if self.rescan.is_some() {
            return;
        }
        self.touched_rooms.insert(key);
        if self.touched.len() + self.touched_rooms.len() > MAX_PENDING_PATHS {
            self.note_rescan(RescanCause::PathCapacity);
        }
    }

    fn is_empty(&self) -> bool {
        self.touched.is_empty() && self.touched_rooms.is_empty() && self.rescan.is_none()
    }

    fn take_scope(&mut self) -> ReconcileScope {
        if self.rescan.take().is_some() {
            self.touched.clear();
            self.touched_rooms.clear();
            ReconcileScope::Full
        } else if !self.touched_rooms.is_empty() {
            debug_assert!(self.touched.is_empty(), "one host profile per watcher");
            ReconcileScope::RoomPaths(std::mem::take(&mut self.touched_rooms))
        } else {
            ReconcileScope::Paths(std::mem::take(&mut self.touched))
        }
    }

    /// Requeues the batch a failed pass consumed so retry happens at the
    /// next debounce window rather than the next safety tick.
    fn merge_back(&mut self, scope: ReconcileScope) {
        match scope {
            ReconcileScope::Full => self.note_rescan(RescanCause::FailedPass),
            ReconcileScope::RoomPaths(paths) => {
                for path in paths {
                    self.insert_room(path);
                }
            }
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
    if is_read_only_access(&event.kind) {
        // Opening, reading, or closing without writing cannot change bytes.
        // Treating them as changes made reconciliation's own source reads
        // re-dirty the corpus, so freshness never settled to current.
        return EventEffect::Touched(Vec::new());
    }
    if event
        .paths
        .iter()
        .any(|path| path == config_path || is_projected_config_swap(path, config_path))
    {
        return EventEffect::ConfigChanged;
    }
    let mut touched = Vec::new();
    let mut touched_rooms = Vec::new();
    for path in &event.paths {
        // A path may fall under several nested registered roots; each
        // registered vault enumerates independently, so record all.
        for root in roots {
            let Ok(relative) = path.strip_prefix(&root.root) else {
                continue;
            };
            match normalize_relative(relative) {
                Some(relative) => match &root.resource {
                    Some(resource) => touched_rooms.push((resource.clone(), relative)),
                    None => touched.push((root.vault_id.clone(), relative)),
                },
                // Byte-identical to walk.rs normalization: a component
                // that is not valid UTF-8 cannot be matched to a source
                // key, so the batch must escalate.
                None => return EventEffect::Rescan(RescanCause::WatcherError),
            }
        }
    }
    if touched_rooms.is_empty() {
        EventEffect::Touched(touched)
    } else {
        EventEffect::TouchedRooms(touched_rooms)
    }
}

/// A projected-volume config directory (for example a Kubernetes ConfigMap)
/// publishes a new revision by atomically replacing its `..data` symlink; no
/// event names the config file itself. Only that exact sibling counts, not the
/// timestamped revision directories created beside it.
fn is_projected_config_swap(path: &Path, config_path: &Path) -> bool {
    config_path
        .parent()
        .is_some_and(|dir| path == dir.join("..data"))
}

/// Access notifications that cannot have changed file contents.
///
/// Opening never changes bytes: inotify reports every `IN_OPEN` as
/// `Open(Any)`, and truncation or writes after an open arrive as their own
/// modify or close-after-write events. Read and close-without-write are
/// likewise inert. A close-after-write still counts as a change (some
/// backends report the completed write only that way), as does a close of
/// unknown mode or any unclassified access.
fn is_read_only_access(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Read | AccessKind::Open(_)) => true,
        EventKind::Access(AccessKind::Close(mode)) => {
            matches!(mode, AccessMode::Read | AccessMode::Execute)
        }
        _ => false,
    }
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
            resource: config.resource_key(vault),
            root: vault.path.clone(),
        })
        .collect()
}

fn deliver_event(
    events: mpsc::Sender<notify::Result<Event>>,
    overflow: Arc<AtomicBool>,
    gate: Option<Arc<ConfigGate>>,
    config_path: PathBuf,
) -> impl FnMut(notify::Result<Event>) {
    move |event| {
        // Read-only access cannot change bytes, and `classify_event` would
        // discard it anyway. Drop it before the bounded queue: a full pass
        // opens every file it reads (notify subscribes to `IN_OPEN` with no
        // opt-out), and while the pass runs nothing drains the queue, so those
        // self-inflicted opens overflowed it, closed the gate and scheduled
        // the next full pass, forever. Rescan-flagged events still escalate.
        if let Ok(inner) = &event
            && !inner.need_rescan()
            && is_read_only_access(&inner.kind)
        {
            return;
        }
        if let Some(gate) = &gate
            && config_may_have_changed(&event, &config_path)
        {
            gate.invalidate();
        }
        if events.try_send(event).is_err() {
            if let Some(gate) = &gate {
                gate.invalidate();
            }
            overflow.store(true, Ordering::Release);
        }
    }
}

fn watch_config_parent(watcher: &mut impl Watcher, config_path: &Path) -> bool {
    watcher
        .watch(
            config_path.parent().unwrap_or(Path::new(".")),
            RecursiveMode::NonRecursive,
        )
        .is_ok()
}

fn arm_config_watch(
    watcher: &mut impl Watcher,
    gate: &ConfigGate,
    paths: &Paths,
    boot: &Config,
) -> (bool, bool) {
    let armed = watch_config_parent(watcher, &paths.config);
    let ticket = gate.ticket();
    let unchanged = load_config_for_paths(paths).is_ok_and(|now| now == *boot);
    if armed && unchanged {
        gate.validated(ticket);
    }
    (armed, unchanged)
}

pub(crate) fn spawn_watcher(
    paths: Paths,
    initial_config: Config,
    manager: ManagerHandle,
    status: Arc<RwLock<DaemonStatus>>,
    live_gate: Option<Arc<ConfigGate>>,
) -> Result<WatcherHandle> {
    spawn_native_watcher(
        paths,
        initial_config,
        manager,
        status,
        live_gate,
        #[cfg(test)]
        None,
    )
}

/// Like [`spawn_watcher`], with native delivery, but safety ticks are driven by
/// the test instead of the 60 s interval.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn spawn_native_watcher_with_test_ticks(
    paths: Paths,
    config: Config,
    manager: ManagerHandle,
    status: Arc<RwLock<DaemonStatus>>,
    gate: Arc<ConfigGate>,
) -> (WatcherHandle, mpsc::Sender<()>) {
    let (ticks, safety_ticks) = mpsc::channel(1);
    let handle = spawn_native_watcher(
        paths,
        config,
        manager,
        status,
        Some(gate),
        Some(safety_ticks),
    )
    .unwrap();
    (handle, ticks)
}

fn spawn_native_watcher(
    paths: Paths,
    initial_config: Config,
    manager: ManagerHandle,
    status: Arc<RwLock<DaemonStatus>>,
    live_gate: Option<Arc<ConfigGate>>,
    #[cfg(test)] safety_ticks: Option<mpsc::Receiver<()>>,
) -> Result<WatcherHandle> {
    let (events, receiver) = mpsc::channel(1_024);
    let overflow = Arc::new(AtomicBool::new(false));
    let mut watcher = notify::recommended_watcher(deliver_event(
        events,
        overflow.clone(),
        live_gate.clone(),
        paths.config.clone(),
    ))
    .context("failed to create filesystem watcher")?;
    let initial_check = live_gate
        .as_ref()
        .map(|gate| arm_config_watch(&mut watcher, gate, &paths, &initial_config));
    let (shutdown, shutdown_receiver) = watch::channel(false);
    let task = tokio::spawn(run_watcher(
        watcher,
        receiver,
        overflow,
        shutdown_receiver,
        WatcherContext {
            live_gate,
            #[cfg(test)]
            safety_ticks,
            initial_check,
            paths,
            config: initial_config,
            manager,
            status,
        },
    ));
    Ok(WatcherHandle { shutdown, task })
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn spawn_watcher_with_test_ticks(
    paths: Paths,
    config: Config,
    manager: ManagerHandle,
    status: Arc<RwLock<DaemonStatus>>,
    gate: Arc<ConfigGate>,
) -> (WatcherHandle, mpsc::Sender<()>) {
    // Install real watches, but deliver neither config nor room notifications.
    let mut watcher = notify::recommended_watcher(|_| {}).unwrap();
    let initial_check = arm_config_watch(&mut watcher, &gate, &paths, &config);
    let (_events, receiver) = mpsc::channel(1);
    let (ticks, safety_ticks) = mpsc::channel(1);
    let (shutdown, receiver_shutdown) = watch::channel(false);
    let task = tokio::spawn(run_watcher(
        watcher,
        receiver,
        Arc::new(AtomicBool::new(false)),
        receiver_shutdown,
        WatcherContext {
            live_gate: Some(gate),
            safety_ticks: Some(safety_ticks),
            initial_check: Some(initial_check),
            paths,
            config,
            manager,
            status,
        },
    ));
    (WatcherHandle { shutdown, task }, ticks)
}

async fn run_watcher(
    mut watcher: RecommendedWatcher,
    mut events: mpsc::Receiver<notify::Result<Event>>,
    overflow: Arc<AtomicBool>,
    mut shutdown: watch::Receiver<bool>,
    context: WatcherContext,
) {
    let WatcherContext {
        live_gate,
        #[cfg(test)]
        mut safety_ticks,
        initial_check,
        paths,
        mut config,
        manager,
        status,
    } = context;
    let startup_config = config.clone();
    let mut config_watch_ok = initial_check.map_or_else(
        || watch_config_parent(&mut watcher, &paths.config),
        |(armed, _)| armed,
    );
    if !config_watch_ok {
        if let Some(gate) = &live_gate {
            gate.invalidate();
        }
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

    let mut config_dirty = initial_check.is_some_and(|(_, unchanged)| !unchanged);
    if config_dirty {
        pending.note_rescan(RescanCause::ConfigChanged);
    }
    let mut config_invalid = false;
    let mut restart_required = false;
    let mut first_dirty = config_dirty.then(Instant::now);
    let mut last_dirty = first_dirty;
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
                let dirty_scope = match &effect {
                    EventEffect::Touched(paths) => ReconcileScope::Paths(paths.iter().cloned().collect()),
                    EventEffect::TouchedRooms(paths) => ReconcileScope::RoomPaths(paths.iter().cloned().collect()),
                    _ => ReconcileScope::Full,
                };
                manager.mark_resources_dirty(&config, &dirty_scope);
                match effect {
                    EventEffect::ConfigChanged => {
                        config_dirty = true;
                        pending.note_rescan(RescanCause::ConfigChanged);
                    }
                    EventEffect::Rescan(cause) => {
                        config_dirty |= live_gate.is_some();
                        pending.note_rescan(cause);
                    }
                    EventEffect::TouchedRooms(touched) => {
                        for key in touched { pending.insert_room(key); }
                    }
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
                    manager.mark_resources_dirty(&config, &ReconcileScope::Full);
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
                    let ticket = live_gate.as_ref().map(|gate| gate.ticket());
                    if config_dirty {
                        match load_config_for_paths(&paths) {
                            Ok(next) => {
                                config_invalid = false;
                                if startup_config.requires_restart_for(&next) {
                                    if let Some(gate) = &live_gate { gate.invalidate(); }
                                    restart_required = true;
                                    mark_degraded(&status).await;
                                } else {
                                    restart_required = false;
                                    if next != config {
                                        manager.update_openclast_registrations(&next);
                                        reconfigure_roots(&mut watcher, &next, &mut watched_roots);
                                        config = next;
                                        roots = watch_roots(&config);
                                    }
                                }
                            }
                            Err(_) => {
                                if let Some(gate) = &live_gate { gate.invalidate(); }
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
                    let validating = config_dirty;
                    config_dirty = false;
                    first_dirty = None;
                    last_dirty = None;
                    let succeeded = reconcile_and_update(
                        &manager,
                        &config,
                        &status,
                        scope.clone(),
                        config_invalid || restart_required || unwatched_roots
                            || (live_gate.is_some() && !config_watch_ok),
                    )
                    .await;
                    if validating && succeeded && !restart_required && !config_invalid && config_watch_ok && let (Some(gate), Some(ticket)) = (&live_gate, ticket) { gate.validated(ticket); }
                    if !succeeded {
                        config_dirty |= validating && live_gate.is_some();
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
            _ = async {
                #[cfg(test)]
                if let Some(ticks) = &mut safety_ticks && ticks.recv().await.is_some() {
                    return;
                }
                safety.tick().await;
            } => {
                if let Some(gate) = &live_gate {
                    if !config_watch_ok {
                        config_watch_ok = watch_config_parent(&mut watcher, &paths.config);
                    }
                    if !load_config_for_paths(&paths).is_ok_and(|next| next == config) {
                        gate.invalidate();
                    }
                    if config_dirty || !gate.open() {
                        // Reopen only through the normal reload and reconcile path,
                        // including when an initially missing watch is now armed.
                        config_dirty = true;
                        pending.note_rescan(RescanCause::ConfigChanged);
                        let now = Instant::now();
                        first_dirty.get_or_insert(now);
                        last_dirty.get_or_insert(now);
                        continue;
                    }
                }
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
                resource: None,
                root: PathBuf::from("/vaults/outer"),
            },
            WatchRoot {
                vault_id: "inner".into(),
                resource: None,
                root: PathBuf::from("/vaults/outer/inner"),
            },
        ]
    }

    fn config_path() -> PathBuf {
        PathBuf::from("/state/config.toml")
    }

    #[tokio::test]
    async fn governed_search_live_check_config_watch_reloads_after_uncertain_delivery() {
        use kwiry_core::{IndexManager, SearchRuntime, build_index, save_config};
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::resolve(
            Some(tmp.path().join("config.toml")),
            Some(tmp.path().join("data")),
        )
        .unwrap();
        let mut boot = Config::default();
        let root = tmp.path().join("room");
        std::fs::create_dir(&root).unwrap();
        boot.vaults.push(kwiry_core::VaultRegistration {
            id: "test".into(),
            path: root,
            room: None,
            snapshot_id: None,
        });
        save_config(&paths.config, &boot).unwrap();
        let runtime = SearchRuntime::new();
        build_index(&boot, &paths.data_dir).unwrap();
        let (manager, actor) = crate::runtime::spawn_manager(
            IndexManager::open(boot.clone(), &paths.data_dir, runtime.clone()).unwrap(),
            runtime,
        );
        let gate = Arc::new(ConfigGate::default());
        let status = Arc::new(RwLock::new(DaemonStatus::starting("test")));
        // Boot saw a valid config. Change it before arming; this event will
        // never be delivered. Subsequent backend notifications are injected
        // through the same delivery function as the native watcher.
        std::fs::write(&paths.config, "[invalid").unwrap();
        let mut watcher = notify::recommended_watcher(|_| {}).unwrap();
        let check = arm_config_watch(&mut watcher, &gate, &paths, &boot);
        assert_eq!(check, (true, false));
        assert!(!gate.open());
        let (events, rx) = mpsc::channel(1);
        let overflow = Arc::new(AtomicBool::new(false));
        let mut deliver = deliver_event(
            events,
            overflow.clone(),
            Some(gate.clone()),
            paths.config.clone(),
        );
        let (stop, shutdown) = watch::channel(false);
        let task = tokio::spawn(run_watcher(
            watcher,
            rx,
            overflow,
            shutdown,
            WatcherContext {
                live_gate: Some(gate.clone()),
                safety_ticks: None,
                initial_check: Some(check),
                paths: paths.clone(),
                config: boot.clone(),
                manager: manager.clone(),
                status: status.clone(),
            },
        ));
        let wait = |open| {
            let gate = gate.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while gate.open() != open {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
            }
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while status.read().await.state != DaemonState::Degraded {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // No file notification accompanies the repair. Only backend error or
        // rescan evidence can cause the watcher to reread and reopen.
        for event in [
            Err(notify::Error::generic("synthetic")),
            Ok(Event::new(EventKind::Any).set_flag(Flag::Rescan)),
        ] {
            save_config(&paths.config, &boot).unwrap();
            deliver(event);
            wait(true).await;
            let old_ticket = gate.ticket();
            std::fs::write(&paths.config, "[invalid").unwrap();
            deliver(Ok(Event::new(EventKind::Any).add_path(paths.config.clone())));
            // Synchronous: the task has not even been scheduled for debounce.
            assert!(!gate.open());
            gate.validated(old_ticket);
            assert!(!gate.open());
            tokio::time::timeout(Duration::from_secs(5), async {
                while status.read().await.state != DaemonState::Degraded {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        // Drop a delivery with no config path: overflow alone must also reread.
        save_config(&paths.config, &boot).unwrap();
        let unrelated = || Ok(Event::new(EventKind::Any));
        deliver(unrelated());
        deliver(unrelated());
        wait(true).await;
        stop.send(true).unwrap();
        task.await.unwrap();
        manager.shutdown().await.unwrap();
        actor.await.unwrap();
    }

    #[test]
    fn delivery_drops_only_read_only_access_before_the_bounded_queue() {
        use notify::event::{DataChange, MetadataKind, ModifyKind, RemoveKind, RenameMode};
        let config = config_path();
        let note = PathBuf::from("/vaults/outer/note.md");
        let fresh = || {
            let gate = Arc::new(ConfigGate::default());
            gate.validated(gate.ticket());
            assert!(gate.open());
            let (events, receiver) = mpsc::channel(1);
            let overflow = Arc::new(AtomicBool::new(false));
            let deliver =
                deliver_event(events, overflow.clone(), Some(gate.clone()), config.clone());
            (gate, receiver, overflow, deliver)
        };
        let inert = [
            EventKind::Access(AccessKind::Open(AccessMode::Any)),
            EventKind::Access(AccessKind::Read),
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            EventKind::Access(AccessKind::Close(AccessMode::Execute)),
        ];
        let (gate, mut receiver, overflow, mut deliver) = fresh();
        for kind in inert {
            // Far more than the one-slot queue holds, and even on the config file.
            for _ in 0..2_000 {
                deliver(Ok(Event::new(kind).add_path(note.clone())));
            }
            deliver(Ok(Event::new(kind).add_path(config.clone())));
        }
        assert!(receiver.try_recv().is_err(), "nothing was queued");
        assert!(!overflow.load(Ordering::Acquire));
        assert!(gate.open());

        // Everything that could indicate a change still occupies the queue,
        // and a dropped one still closes the gate and is recorded.
        let changes: Vec<notify::Result<Event>> = vec![
            Ok(Event::new(EventKind::Access(AccessKind::Close(
                AccessMode::Write,
            )))),
            Ok(Event::new(EventKind::Access(AccessKind::Close(
                AccessMode::Any,
            )))),
            Ok(
                Event::new(EventKind::Access(AccessKind::Open(AccessMode::Any)))
                    .set_flag(Flag::Rescan),
            ),
            Ok(Event::new(EventKind::Access(AccessKind::Any))),
            Ok(Event::new(EventKind::Access(AccessKind::Other))),
            Ok(Event::new(EventKind::Create(CreateKind::File))),
            Ok(Event::new(EventKind::Modify(ModifyKind::Data(
                DataChange::Any,
            )))),
            Ok(Event::new(EventKind::Modify(ModifyKind::Metadata(
                MetadataKind::Any,
            )))),
            Ok(Event::new(EventKind::Modify(ModifyKind::Name(
                RenameMode::Both,
            )))),
            Ok(Event::new(EventKind::Remove(RemoveKind::File))),
            Ok(Event::new(EventKind::Any)),
            Ok(Event::new(EventKind::Other)),
            Err(notify::Error::generic("synthetic")),
        ];
        for change in changes {
            let (gate, mut receiver, overflow, mut deliver) = fresh();
            deliver(Ok(Event::new(EventKind::Any).add_path(note.clone())));
            assert!(gate.open());
            let described = format!("{change:?}");
            deliver(change.map(|event| event.add_path(note.clone())));
            assert!(
                overflow.load(Ordering::Acquire),
                "{described} was dropped silently"
            );
            assert!(!gate.open(), "{described} overflowed with the gate open");
            assert!(receiver.try_recv().is_ok());
        }
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
    fn classify_event_ignores_read_only_access_inside_roots_and_on_config() {
        let read_only = [
            AccessKind::Read,
            // inotify reports every IN_OPEN as Open(Any), including the
            // daemon's own reconciliation reads.
            AccessKind::Open(AccessMode::Any),
            AccessKind::Open(AccessMode::Read),
            AccessKind::Open(AccessMode::Write),
            AccessKind::Open(AccessMode::Execute),
            AccessKind::Close(AccessMode::Read),
            AccessKind::Close(AccessMode::Execute),
        ];
        for kind in read_only {
            let note = Event::new(EventKind::Access(kind))
                .add_path(PathBuf::from("/vaults/outer/note.md"));
            assert_eq!(
                classify_event(&Ok(note), &roots(), &config_path()),
                EventEffect::Touched(Vec::new()),
                "{kind:?} on a source must not dirty the corpus"
            );
            let config = Event::new(EventKind::Access(kind)).add_path(config_path());
            assert_eq!(
                classify_event(&Ok(config), &roots(), &config_path()),
                EventEffect::Touched(Vec::new()),
                "{kind:?} on the config must not trigger a reload"
            );
        }
    }

    #[test]
    fn classify_event_keeps_write_and_unknown_access_as_changes() {
        let changing = [
            AccessKind::Close(AccessMode::Write),
            AccessKind::Close(AccessMode::Any),
            AccessKind::Close(AccessMode::Other),
            AccessKind::Any,
            AccessKind::Other,
        ];
        for kind in changing {
            let event = Event::new(EventKind::Access(kind))
                .add_path(PathBuf::from("/vaults/outer/note.md"));
            assert_eq!(
                classify_event(&Ok(event), &roots(), &config_path()),
                EventEffect::Touched(vec![("outer".into(), "note.md".into())]),
                "{kind:?} must still be treated as a possible change"
            );
        }
    }

    #[test]
    fn classify_event_read_access_never_suppresses_backend_rescan() {
        let mut event = Event::new(EventKind::Access(AccessKind::Read));
        event = event.set_flag(Flag::Rescan);
        assert_eq!(
            classify_event(&Ok(event), &roots(), &config_path()),
            EventEffect::Rescan(RescanCause::BackendRescan)
        );
    }

    #[test]
    fn classify_event_treats_a_projected_config_swap_as_a_config_change() {
        // The atomic rename of `..data` onto the new revision's symlink.
        let event =
            Event::new(EventKind::Create(CreateKind::Any)).add_path(PathBuf::from("/state/..data"));
        assert_eq!(
            classify_event(&Ok(event.clone()), &roots(), &config_path()),
            EventEffect::ConfigChanged
        );
        // The same classification closes the private service's admission gate.
        assert!(config_may_have_changed(&Ok(event), &config_path()));
    }

    #[test]
    fn classify_event_ignores_projected_revision_siblings() {
        for sibling in [
            "/state/..2026_11_23_tmp",
            "/state/..data_tmp",
            "/elsewhere/..data",
        ] {
            let event =
                Event::new(EventKind::Create(CreateKind::Any)).add_path(PathBuf::from(sibling));
            assert_eq!(
                classify_event(&Ok(event.clone()), &roots(), &config_path()),
                EventEffect::Touched(Vec::new()),
                "{sibling}"
            );
            assert!(!config_may_have_changed(&Ok(event), &config_path()));
        }
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
