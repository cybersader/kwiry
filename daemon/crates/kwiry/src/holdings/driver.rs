//! The daemon's list-only holder driver. A dedicated thread owns the holder
//! and lists whenever its relist scheduler says one is due (startup, the
//! periodic interval, backoff). It also probes the authority's clock while a
//! clock hold is raised. It does not run status, report, purge or release passes.
//!
//! Request handlers never call the holder; they share only its gate. The
//! thread holds no async lock and the holder's HTTP calls block only it.
//!
//! **Fail-stop (H.45).** The instant holder state becomes unwritable the
//! holder revokes every gate clone, the event sink trips [`FailStop`], and
//! the serving task stops the process with a non-zero exit.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::{Result, anyhow};
use kwiry_core::{Config, DiscardedGeneration, HolderSettings};
use kwiry_holdings::clock::Clock;
use kwiry_holdings::gate::{Refusal, ServingGate};
use kwiry_holdings::holder::{ListOutcome, RouteHealth};
use kwiry_holdings::state::HolderState;
use kwiry_holdings::store::ResourceStore;
use serde::{Deserialize, Serialize};

use super::adapters::LogSink;
use super::{start_holder, system_clock};

/// The operator message a fail-stopped daemon exits with.
pub(crate) const FAIL_STOP_MESSAGE: &str = "holder_state_unwritable: the lifecycle holder could not make its state durable and fail-stopped; serving has stopped. Check the holder state directory's storage, then restart the daemon";

/// The runtime snapshot a running daemon keeps in its data directory, read
/// by `kwiry holder status`. Never holds keys, ids of resources, nonces or
/// digests.
pub(crate) const SNAPSHOT_FILE: &str = "holder-status.json";
/// Temporary snapshots left by a crash are not index content.
pub(super) const SNAPSHOT_TEMP_PREFIX: &str = ".holder-status.json.";

const TICK: Duration = Duration::from_millis(500);
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);
const SNAPSHOT_REFRESH_MS: i64 = 30_000;

/// Trips once; the serving task waits on it.
#[derive(Default)]
pub(crate) struct FailStop {
    tripped: AtomicBool,
    notify: tokio::sync::Notify,
}

impl FailStop {
    pub(crate) fn trip(&self) {
        if !self.tripped.swap(true, Ordering::SeqCst) {
            tracing::error!("{FAIL_STOP_MESSAGE}");
        }
        self.notify.notify_one();
    }

    pub(crate) fn tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }

    pub(crate) async fn wait(&self) {
        while !self.tripped() {
            self.notify.notified().await;
        }
    }
}

/// Whether the holder that issued `gate` has fail-stopped or been dropped.
pub(crate) fn gate_revoked(gate: &ServingGate) -> bool {
    matches!(gate.check(""), Err(Refusal::Revoked))
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Tick {
    Continue,
    FailStopped,
}

/// One holder and its list scheduler.
struct Driver<S: ResourceStore, T: kwiry_holdings::client::Transport> {
    holder: kwiry_holdings::holder::Holder<S, T>,
    clock: Arc<dyn Clock>,
    /// The last list outcome logged, so a standing refusal (an auth halt, a
    /// pending reservation) is logged once rather than every tick.
    last_list_note: Option<String>,
}

impl<S, T> Driver<S, T>
where
    S: ResourceStore,
    T: kwiry_holdings::client::Transport,
{
    fn new(holder: kwiry_holdings::holder::Holder<S, T>, clock: Arc<dyn Clock>) -> Self {
        Self {
            holder,
            clock,
            last_list_note: None,
        }
    }

    pub(crate) fn holder(&self) -> &kwiry_holdings::holder::Holder<S, T> {
        &self.holder
    }

    /// Runs the list scheduler and clock probe, respecting the holder's refusals.
    fn tick(&mut self) -> Tick {
        if self.holder.fail_stopped() {
            return Tick::FailStopped;
        }
        let now = self.clock.mono_ms();
        if self.holder.scheduler().due(now) {
            let outcome = self.holder.list_once();
            let note = describe_list(&outcome);
            if self.last_list_note.as_deref() != Some(note.as_str()) {
                log_list(&outcome);
            }
            self.last_list_note = Some(note);
        }
        if let Some(Err(failure)) = self.holder.probe_clock() {
            tracing::warn!(failure = ?failure, "holder clock probe failed");
        }
        if self.holder.fail_stopped() {
            Tick::FailStopped
        } else {
            Tick::Continue
        }
    }
}

fn log_list(outcome: &ListOutcome) {
    match outcome {
        ListOutcome::Applied(report) => tracing::info!(
            seq = report.rev.seq,
            instructions = report.instructions,
            tombstone_conflicts = report.tombstone_conflicts,
            "holder list applied"
        ),
        ListOutcome::RateLimited { .. } => {}
        other => tracing::warn!(outcome = %describe_list(other), "holder list not applied"),
    }
}

/// A one-line, secret-free description of a list outcome.
pub(crate) fn describe_list(outcome: &ListOutcome) -> String {
    match outcome {
        ListOutcome::Applied(report) => format!(
            "applied: epoch {} seq {} ({} instruction(s), {} tombstone conflict(s))",
            report.rev.epoch, report.rev.seq, report.instructions, report.tombstone_conflicts
        ),
        ListOutcome::Failed(failure) => format!("failed: {failure:?}"),
        ListOutcome::ApplyFailed { step } => format!("apply failed at step {step}"),
        ListOutcome::AuthHalted => "refused: auth halt (operator restart required)".to_owned(),
        ListOutcome::RateLimited { .. } => "refused: list-start rate limit".to_owned(),
        ListOutcome::ReservationPending => {
            "refused: a release reservation is pending confirmation".to_owned()
        }
    }
}

/// What `kwiry holder status` shows: durable state plus, from a running
/// daemon, its live view. No key material, resource ids, nonces or digests.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HolderReport {
    pub holder_id: String,
    pub high_water_epoch: Option<String>,
    pub high_water_seq: Option<u64>,
    pub tombstones: usize,
    pub reset_pending: bool,
    pub list_request_invalid: bool,
    pub clock_hold: bool,
    pub status_halted: bool,
    pub status_pending: bool,
    pub status_uncertain: usize,
    pub report_channels: usize,
    pub report_channels_halted: usize,
    pub reports_pending: usize,
    pub reports_uncertain: usize,
    pub release_reservation_pending: bool,
    /// Present only from a running daemon (or right after `relist`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<LiveReport>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LiveReport {
    pub pid: u32,
    pub updated_at_ms: i64,
    /// `normal`, `auth_halt_invalid_proof` or `auth_halt_not_authorized`.
    pub route: String,
    pub list_applied_in_process: bool,
    pub lease_expires_at_ms: Option<i64>,
    pub alerts: Vec<String>,
    pub fail_stopped: bool,
}

impl HolderReport {
    pub(crate) fn from_state(state: &HolderState, clock_hold: bool) -> Self {
        let book = &state.obligations;
        Self {
            holder_id: state.holder_id.clone(),
            high_water_epoch: state.high_water.as_ref().map(|rev| rev.epoch.clone()),
            high_water_seq: state.high_water.as_ref().map(|rev| rev.seq),
            tombstones: state.tombstones.len(),
            reset_pending: state.reset_pending,
            list_request_invalid: state.list_request_invalid,
            clock_hold,
            status_halted: state.status.halted,
            status_pending: state.status.pending.is_some(),
            status_uncertain: state.status.uncertain.len(),
            report_channels: book.channels.len(),
            report_channels_halted: book.channels.values().filter(|c| c.halted).count(),
            reports_pending: book
                .channels
                .values()
                .filter(|c| c.pending.is_some())
                .count(),
            reports_uncertain: book.uncertain.len(),
            release_reservation_pending: state.status.reservation_pending(),
            live: None,
        }
    }

    pub(crate) fn from_holder<S: ResourceStore, T: kwiry_holdings::client::Transport>(
        holder: &kwiry_holdings::holder::Holder<S, T>,
        wall_ms: i64,
    ) -> Self {
        let clock_hold = holder.engine_gate().is_some_and(|gate| gate.clock_hold());
        let mut report = Self::from_state(holder.state(), clock_hold);
        report.live = Some(LiveReport {
            pid: std::process::id(),
            updated_at_ms: wall_ms,
            route: match holder.route_health() {
                RouteHealth::Normal => "normal",
                RouteHealth::AuthHalt(kwiry_holdings::holder::AuthHalt::InvalidProof) => {
                    "auth_halt_invalid_proof"
                }
                RouteHealth::AuthHalt(kwiry_holdings::holder::AuthHalt::NotAuthorized) => {
                    "auth_halt_not_authorized"
                }
            }
            .to_owned(),
            list_applied_in_process: holder.gate().list_applied_in_process(),
            lease_expires_at_ms: holder.applied().map(|a| a.lease_expires_wall_ms()),
            alerts: holder
                .raised_alerts()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            fail_stopped: holder.fail_stopped(),
        });
        report
    }
}

/// Rewrites the snapshot when it changed, and at least every
/// [`SNAPSHOT_REFRESH_MS`] so its age shows the daemon is alive.
fn write_snapshot(data_dir: &Path, report: &HolderReport, last: &mut Option<(HolderReport, i64)>) {
    let now = report.live.as_ref().map_or(0, |live| live.updated_at_ms);
    let mut unstamped = report.clone();
    if let Some(live) = unstamped.live.as_mut() {
        live.updated_at_ms = 0;
    }
    if let Some((previous, written_at)) = last
        && *previous == unstamped
        && now.saturating_sub(*written_at) < SNAPSHOT_REFRESH_MS
    {
        return;
    }
    if replace_snapshot(data_dir, report).is_ok() {
        *last = Some((unstamped, now));
    }
}

fn replace_snapshot(data_dir: &Path, report: &HolderReport) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(report)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(SNAPSHOT_TEMP_PREFIX)
        .tempfile_in(data_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(data_dir.join(SNAPSHOT_FILE))?;
    #[cfg(unix)]
    fs::File::open(data_dir)?.sync_all()?;
    Ok(())
}

fn remove_snapshot(data_dir: &Path) {
    if let Err(error) = fs::remove_file(data_dir.join(SNAPSHOT_FILE))
        && error.kind() != ErrorKind::NotFound
    {
        tracing::warn!(kind = ?error.kind(), "could not remove the holder status snapshot");
    }
}

/// The running holder: its driver thread, its gate and its fail-stop.
pub(crate) struct HolderService {
    gate: ServingGate,
    fail_stop: Arc<FailStop>,
    stop: mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HolderService {
    /// Starts the holder on its own thread and waits for its startup. A
    /// startup refusal (including a failed replay of owed transitions) is
    /// returned and nothing runs.
    pub(crate) async fn spawn(
        settings: HolderSettings,
        config: Config,
        data_dir: PathBuf,
    ) -> Result<(Self, Option<DiscardedGeneration>)> {
        let fail_stop = Arc::new(FailStop::default());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (stop, stop_rx) = mpsc::channel::<()>();
        let trip = fail_stop.clone();
        // The sink trips the fail-stop inside the failing write; the driver
        // also trips it if a poll finds the holder stopped first.
        let trip_poll = fail_stop.clone();
        let thread = std::thread::Builder::new()
            .name("kwiry-holder".into())
            .spawn(move || {
                let clock = system_clock();
                let sink = Arc::new(LogSink::new(Arc::new(move || trip.trip())));
                let startup =
                    start_holder(&settings, &config, &data_dir, clock.clone(), sink).and_then(
                        |(holder, data_lock)| Ok((holder, data_lock.prepare()?)),
                    );
                let (holder, discarded) = match startup {
                    Ok(started) => started,
                    Err(error) => {
                        let _ = started_tx.send(Err(error));
                        return;
                    }
                };
                let gate = holder.gate();
                let mut driver = Driver::new(holder, clock);
                // Do not expose a previous daemon's snapshot as ours.
                remove_snapshot(&data_dir);
                if started_tx.send(Ok((gate, discarded))).is_err() {
                    return;
                }
                tracing::info!("this build lists holdings only; it does not report status, report obligations, or delete index data");
                run(&trip_poll, &mut driver, &data_dir, &stop_rx);
            })
            .map_err(|error| anyhow!("could not start the holder thread: {error}"))?;
        let (gate, discarded) = started_rx
            .await
            .map_err(|_| anyhow!("the holder thread stopped during startup"))??;
        Ok((
            Self {
                gate,
                fail_stop,
                stop,
                thread: Some(thread),
            },
            discarded,
        ))
    }

    pub(crate) fn gate(&self) -> ServingGate {
        self.gate.clone()
    }

    pub(crate) fn fail_stop(&self) -> Arc<FailStop> {
        self.fail_stop.clone()
    }

    /// Stops the driver between passes. A pass blocked on the authority is
    /// given a short grace period; holder writes are crash-safe either way.
    pub(crate) async fn shutdown(mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let joined = tokio::time::timeout(
                SHUTDOWN_WAIT,
                tokio::task::spawn_blocking(move || thread.join()),
            )
            .await;
            if joined.is_err() {
                tracing::warn!("holder driver still in a pass at shutdown; abandoning it");
            }
        }
    }
}

fn run(
    fail_stop: &FailStop,
    driver: &mut Driver<super::adapters::ConfiguredRooms, super::transport::HttpTransport>,
    data_dir: &Path,
    stop: &mpsc::Receiver<()>,
) {
    let mut last_snapshot = None;
    loop {
        let tick = driver.tick();
        write_snapshot(
            data_dir,
            &HolderReport::from_holder(driver.holder(), driver.clock.wall_ms()),
            &mut last_snapshot,
        );
        if tick == Tick::FailStopped {
            fail_stop.trip();
            return;
        }
        match stop.recv_timeout(TICK) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            _ => {
                remove_snapshot(data_dir);
                return;
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::holdings::openclast_profile;
    use kwiry_holdings::state::STATE_FILE;
    use kwiry_holdings::testkit::{FakeAuthority, Fault, MemoryStore, Rig};

    const DIGEST_SALT: &str = "openclast-lifecycle/v1";

    fn rig() -> (tempfile::TempDir, Rig) {
        let dir = tempfile::tempdir().unwrap();
        let rig = Rig::with_profile(&dir.path().join("holder"), openclast_profile(), DIGEST_SALT);
        (dir, rig)
    }

    fn driver(rig: &mut Rig) -> Driver<MemoryStore, FakeAuthority> {
        Driver::new(rig.take_holder(), rig.clock.clone())
    }

    #[test]
    fn a_listed_release_and_a_tombstoned_obligation_delete_nothing_through_the_daemon_wiring() {
        let (dir, mut rig) = rig();
        let room = "room-test-1";
        rig.store.hold("room", room, true, 1);
        let holding = rig.authority.state().add_active("room", room);
        rig.authority.state().attest(&holding, "synthetic-root-1");
        rig.purger.add("room", room, 1);
        let unattributed = rig.purger.add_unattributable();
        let generation = dir.path().join("data/generations/g-1");
        fs::create_dir_all(&generation).unwrap();

        // Seed a matching accepted report in the testkit, not the driver.
        // The daemon must not act even on a release it could match.
        assert!(matches!(rig.holder().list_once(), ListOutcome::Applied(_)));
        assert_eq!(rig.status().unwrap().receipts, 1);
        let digest = rig.authority.state().accepted_digests[0].clone();
        let obligation = rig.authority.state().retire(&holding);
        let release = rig.authority.state().issue_release(&digest, 1);
        let mut driver = driver(&mut rig);
        for _ in 0..5 {
            rig.advance(60_000);
            assert_eq!(driver.tick(), Tick::Continue);
        }
        let applied = driver.holder().applied().unwrap();
        assert_eq!(applied.instructions.len(), 1);
        assert!(rig.authority.state().outstanding(&release));
        assert!(applied.items.iter().any(|item| {
            item.obligation
                .as_ref()
                .is_some_and(|listed| listed.obligation_id == obligation)
        }));
        let state = HolderState::parse(&fs::read(rig.dir.join(STATE_FILE)).unwrap()).unwrap();
        assert!(state.is_tombstoned(room));
        assert!(state.status.instructions.is_empty());
        assert!(!state.status.reservation_pending());
        assert!(!state.obligations.channels.contains_key(&obligation));
        assert!(rig.purger.deleted().is_empty());
        assert!(rig.purger.present(&unattributed));
        assert_eq!(rig.purger.present_of("room", room), 1);
        assert!(generation.is_dir());
    }

    #[test]
    fn an_unwritable_holder_stops_the_driver_and_revokes_the_gate() {
        let (_dir, mut rig) = rig();
        rig.store.hold("room", "room-test-1", true, 1);
        let holding = rig.authority.state().add_active("room", "room-test-1");
        rig.authority.state().attest(&holding, "synthetic-root-1");
        let mut driver = driver(&mut rig);
        assert_eq!(driver.tick(), Tick::Continue);
        let gate = driver.holder().gate();
        assert!(!gate_revoked(&gate));
        // Raise the clock hold, then make its clearing unpersistable: the
        // seal's fsync fails, the record cannot be written, nor the seal
        // removed (the slice-1 poison).
        rig.authority.state().offset_ms = 45_000;
        rig.advance(300_000);
        driver.tick();
        assert!(driver.holder().engine_gate().unwrap().clock_hold());
        rig.authority.state().offset_ms = 0;
        rig.io_faults
            .plan(vec![Fault::Pass, Fault::AfterRename, Fault::BeforeRename]);
        rig.io_faults.fail_removes();
        rig.advance(60_000);
        assert_eq!(driver.tick(), Tick::FailStopped);
        assert!(gate_revoked(&gate));
        let posts = rig.authority.state().posts();
        rig.advance(600_000);
        assert_eq!(driver.tick(), Tick::FailStopped);
        assert_eq!(
            rig.authority.state().posts(),
            posts,
            "no traffic after fail-stop"
        );
    }

    #[tokio::test]
    async fn fail_stop_wakes_a_waiter_tripped_before_or_after_it_waits() {
        let early = Arc::new(FailStop::default());
        early.trip();
        tokio::time::timeout(Duration::from_secs(1), early.wait())
            .await
            .unwrap();
        let late = Arc::new(FailStop::default());
        let tripper = late.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tripper.trip();
        });
        tokio::time::timeout(Duration::from_secs(1), late.wait())
            .await
            .unwrap();
        assert!(late.tripped());
    }

    #[test]
    fn the_report_never_carries_keys_or_resource_ids() {
        let (_dir, mut rig) = rig();
        rig.store.hold("room", "room-test-1", true, 1);
        let holding = rig.authority.state().add_active("room", "room-test-1");
        rig.authority.state().attest(&holding, "synthetic-root-1");
        let mut driver = driver(&mut rig);
        driver.tick();
        let report = HolderReport::from_holder(driver.holder(), 0);
        let text = serde_json::to_string(&report).unwrap();
        assert!(!text.contains("room-test-1"));
        assert!(!text.contains(&holding));
        assert!(!text.contains("\"d\""));
        let live = report.live.unwrap();
        assert_eq!(live.route, "normal");
        assert!(live.list_applied_in_process);
        assert!(live.lease_expires_at_ms.is_some());
    }
}
