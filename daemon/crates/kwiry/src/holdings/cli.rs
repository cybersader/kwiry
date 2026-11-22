//! `kwiry holder …`: operator commands for a configured holder.
//!
//! `key`, `enroll` and `relist` take the holder directory's exclusive lock
//! and refuse while a running daemon holds it. `status` changes no files:
//! it reads durable state and shows the daemon's snapshot only while the
//! holder lock is held. Nothing printed carries key material, resource ids,
//! nonces or digests.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;
use kwiry_core::{Config, HolderSettings, Paths, load_config_for_paths};
use kwiry_holdings::engine::clock_hold::SEAL_FILE;
use kwiry_holdings::state::{FsIo, HolderDir, HolderState, LOCK_FILE, STATE_FILE};

use super::adapters::LogSink;
use super::driver::{HolderReport, LiveReport, SNAPSHOT_FILE, describe_list};
use super::{open_holder_dir, start_holder, system_clock, wire_profile};

#[derive(Debug, Subcommand)]
pub(crate) enum HolderCommand {
    /// Create the holder's Ed25519 key in holder.state_dir and print its
    /// public JWK for enrollment at the authority. Refuses if a key exists.
    Key,
    /// Record the authority's enrollment: the assigned holder id and the
    /// lifecycle JWKS file to pin. Refuses if already enrolled.
    Enroll {
        #[arg(long, value_name = "HOLDER_ID")]
        holder_id: String,
        #[arg(long, value_name = "PATH")]
        jwks: PathBuf,
    },
    /// Run one list pass now and print its outcome.
    Relist {
        #[arg(long)]
        json: bool,
    },
    /// Print holder state: route health, lease, halts and pending counts.
    Status {
        #[arg(long)]
        json: bool,
    },
}

fn holder_settings(config: &Config) -> Result<HolderSettings> {
    config
        .holder
        .clone()
        .ok_or_else(|| anyhow!("no [holder] section is configured"))
}

pub(crate) fn run(paths: &Paths, command: HolderCommand) -> Result<()> {
    let config = load_config_for_paths(paths).with_context(|| {
        format!(
            "failed to load configuration from {}",
            paths.config.display()
        )
    })?;
    let private = config.server.profile_required;
    if private {
        crate::private_panic_hook();
    }
    let result = run_configured(paths, command, &config, private);
    if private {
        result.map_err(|_| anyhow!("holder_failed"))
    } else {
        result
    }
}

fn run_configured(
    paths: &Paths,
    command: HolderCommand,
    config: &Config,
    private: bool,
) -> Result<()> {
    let settings = holder_settings(config)?;
    match command {
        HolderCommand::Key => {
            let dir = open_holder_dir(&settings, config, &paths.data_dir)?;
            let key = kwiry_holdings::startup::create_key(&dir, &FsIo)
                .map_err(|error| anyhow!("holder key: {error}"))?;
            if private {
                println!("holder key created");
            } else {
                println!("holder key created; thumbprint {}", key.thumbprint());
                println!("{}", serde_json::to_string_pretty(&key.public_jwk())?);
            }
        }
        HolderCommand::Enroll { holder_id, jwks } => {
            let bytes = std::fs::read(&jwks)
                .with_context(|| format!("failed to read {}", jwks.display()))?;
            let jwks: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| anyhow!("the JWKS file is not JSON"))?;
            let profile = wire_profile(settings.profile);
            let holder_kind = profile
                .engine
                .as_ref()
                .map(|engine| engine.holder_kind.clone())
                .ok_or_else(|| anyhow!("the configured profile has no engine holder kind"))?;
            let dir = open_holder_dir(&settings, config, &paths.data_dir)?;
            kwiry_holdings::startup::enroll(
                dir,
                Box::new(FsIo),
                system_clock(),
                &kwiry_holdings::startup::EnrollParams {
                    holder_id: &holder_id,
                    holder_kind: &holder_kind,
                    issuer: &settings.issuer,
                    jwks: &jwks,
                    authority_key_id_prefix: &profile.authority_key_id_prefix,
                },
            )
            .map_err(|error| anyhow!("enrollment refused ({}): {error}", error.code()))?;
            println!("holder enrolled");
        }
        HolderCommand::Relist { json } => {
            let (mut holder, fail_stop) = start(&settings, config, &paths.data_dir)?;
            let outcome = holder.list_once();
            let report = HolderReport::from_holder(&holder, now_ms());
            if private {
                if !matches!(outcome, kwiry_holdings::holder::ListOutcome::Applied(_)) {
                    bail!("holder_failed");
                }
                println!("list applied");
            } else if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "outcome": describe_list(&outcome),
                        "holder": report,
                    }))?
                );
            } else {
                println!("list {}", describe_list(&outcome));
                print_report(&report);
            }
            if fail_stop.load(Ordering::SeqCst) || holder.fail_stopped() {
                bail!("{}", super::driver::FAIL_STOP_MESSAGE);
            }
        }
        HolderCommand::Status { json } => {
            let report = read_status(&settings, &paths.data_dir)?;
            if private {
                println!("holder status read");
            } else if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_report(&report);
            }
        }
    }
    Ok(())
}

fn start(
    settings: &HolderSettings,
    config: &Config,
    data_dir: &Path,
) -> Result<(super::DaemonHolder, Arc<AtomicBool>)> {
    let tripped = Arc::new(AtomicBool::new(false));
    let flag = tripped.clone();
    let sink = Arc::new(LogSink::new(Arc::new(move || {
        flag.store(true, Ordering::SeqCst);
    })));
    let (holder, data_lock) = start_holder(settings, config, data_dir, system_clock(), sink)?;
    // Relist inspects and binds under the writer lock, but never repairs or
    // prepares the index and need not hold the lock during authority I/O.
    drop(data_lock);
    Ok((holder, tripped))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Durable state plus the daemon's snapshot if the state directory is locked.
fn read_status(settings: &HolderSettings, data_dir: &Path) -> Result<HolderReport> {
    let bytes_and_dir = HolderDir::checked_root(&settings.state_dir)
        .and_then(|dir| std::fs::read(dir.join(STATE_FILE)).map(|bytes| (bytes, dir)));
    let (bytes, dir) = match bytes_and_dir {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("the holder is not enrolled (no holder state in holder.state_dir)")
        }
        Err(error) => return Err(anyhow!("holder state unreadable: {error}")),
    };
    let state = HolderState::parse(&bytes).map_err(|_| anyhow!("holder state unreadable"))?;
    let seal = match std::fs::read(dir.join(SEAL_FILE)) {
        Ok(bytes) => Some(String::from_utf8(bytes).ok()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        // An unreadable seal reads as a raised hold.
        Err(_) => Some(None),
    };
    let clock_hold = match seal {
        Some(None) => true,
        Some(Some(seal)) => state.engine.clock_hold(Some(&seal)),
        None => state.engine.clock_hold(None),
    };
    let mut report = HolderReport::from_state(&state, clock_hold);
    report.live = std::fs::read(data_dir.join(SNAPSHOT_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<HolderReport>(&bytes).ok())
        .filter(|snapshot| snapshot.holder_id == state.holder_id)
        .and_then(|snapshot| snapshot.live)
        .filter(|_| holder_lock_is_held(&dir));
    Ok(report)
}

fn holder_lock_is_held(state_dir: &Path) -> bool {
    // Open only an existing lock file, without truncating it. The same lock
    // primitive as HolderDir distinguishes contention from an I/O failure.
    let Ok(lock) = OpenOptions::new()
        .write(true)
        .open(state_dir.join(LOCK_FILE))
    else {
        return false;
    };
    fs2::FileExt::try_lock_exclusive(&lock)
        .is_err_and(|error| error.raw_os_error() == fs2::lock_contended_error().raw_os_error())
}

fn utc(ms: i64) -> String {
    kwiry_holdings::clock::format_utc_seconds(ms)
}

fn print_report(report: &HolderReport) {
    println!("holder {}", report.holder_id);
    match (&report.high_water_epoch, report.high_water_seq) {
        (Some(epoch), Some(seq)) => println!("  high-water: epoch {epoch} seq {seq}"),
        _ => println!("  high-water: none (no list applied yet)"),
    }
    println!("  tombstones: {}", report.tombstones);
    println!(
        "  alerts: reset_pending={} list_request_invalid={} clock_hold={}",
        report.reset_pending, report.list_request_invalid, report.clock_hold
    );
    println!(
        "  status channel: halted={} pending={} uncertain={}",
        report.status_halted, report.status_pending, report.status_uncertain
    );
    println!(
        "  report channels: {} (halted {}, pending {}), uncertain {}",
        report.report_channels,
        report.report_channels_halted,
        report.reports_pending,
        report.reports_uncertain
    );
    println!(
        "  release reservation pending: {}",
        report.release_reservation_pending
    );
    match &report.live {
        Some(live) => print_live(live),
        None => println!("  live: no running daemon snapshot"),
    }
}

fn print_live(live: &LiveReport) {
    println!(
        "  live (pid {}, as of {}): route {}, fail_stopped={}",
        live.pid,
        utc(live.updated_at_ms),
        live.route,
        live.fail_stopped
    );
    match live.lease_expires_at_ms {
        Some(at) if live.list_applied_in_process => {
            println!("  list applied in this process; lease expires {}", utc(at));
        }
        _ => println!("  no list applied in this process; nothing is servable"),
    }
    if !live.alerts.is_empty() {
        println!("  raised: {}", live.alerts.join(", "));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use kwiry_core::{DataRoot, HolderProfile};
    use kwiry_holdings::testkit::{Rig, TEST_ISSUER};

    #[test]
    fn startup_and_relist_do_not_bind_or_reset_when_the_storage_probe_fails() {
        let temp = tempfile::tempdir().unwrap();
        let settings = HolderSettings {
            profile: HolderProfile::OpenClast,
            issuer: TEST_ISSUER.into(),
            state_dir: temp.path().join("holder"),
            page_size: None,
            clock_skew_allowance_seconds: None,
            max_stale_serve_seconds: None,
        };
        drop(Rig::with_profile(
            &settings.state_dir,
            wire_profile(settings.profile),
            "example-lifecycle/v1",
        ));
        let state = settings.state_dir.join(STATE_FILE);
        std::fs::remove_file(&state).unwrap();
        let config = Config::default();
        let data = temp.path().join("data");

        for relist in [false, true] {
            DataRoot::fail_next_storage_probe_for_test();
            let result = if relist {
                start(&settings, &config, &data).map(|_| ())
            } else {
                let sink = Arc::new(LogSink::new(Arc::new(|| {})));
                start_holder(&settings, &config, &data, system_clock(), sink).map(|_| ())
            };
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains("injected storage-probe checkpoint failure"),
                "{error}"
            );
            assert!(!state.exists());
            assert!(!data.join(super::super::adapters::MARKER_FILE).exists());
        }
    }
}
