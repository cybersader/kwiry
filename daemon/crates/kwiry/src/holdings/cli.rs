//! `kwiry holder …`: operator commands for a configured holder.
//!
//! Commands that change holder state (`key`, `enroll`, `relist`,
//! `resume-status`, `resume-report`) take the holder directory's exclusive
//! lock and **refuse while a running daemon holds it**. `status` only reads:
//! the durable holder state (written by atomic rename) and the running
//! daemon's snapshot, so it is safe at any time. Nothing printed carries key
//! material, resource ids, nonces or digests.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;
use kwiry_core::{Config, HolderSettings, Paths, load_config_for_paths};
use kwiry_holdings::engine::clock_hold::SEAL_FILE;
use kwiry_holdings::state::{FsIo, HolderState, STATE_FILE};

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
    /// Operator resume (N5) of a bug-halted status channel.
    ResumeStatus,
    /// Operator resume (N5) of a bug-halted obligation report channel.
    ResumeReport {
        #[arg(value_name = "OBLIGATION_ID")]
        obligation_id: String,
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
    let settings = holder_settings(&config)?;
    match command {
        HolderCommand::Key => {
            let dir = open_holder_dir(&settings, &config, &paths.data_dir)?;
            let key = kwiry_holdings::startup::create_key(&dir, &FsIo)
                .map_err(|error| anyhow!("holder key: {error}"))?;
            println!("holder key created; thumbprint {}", key.thumbprint());
            println!("{}", serde_json::to_string_pretty(&key.public_jwk())?);
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
            let dir = open_holder_dir(&settings, &config, &paths.data_dir)?;
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
            let (mut holder, fail_stop) = start(&settings, &config, &paths.data_dir)?;
            let outcome = holder.list_once();
            let report = HolderReport::from_holder(&holder, now_ms());
            if json {
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
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_report(&report);
            }
        }
        HolderCommand::ResumeStatus => {
            let (mut holder, _) = start(&settings, &config, &paths.data_dir)?;
            if holder.resume_status() {
                println!("status channel resumed: it will be re-derived under a new key");
            } else {
                bail!("the status channel is not halted; nothing was changed");
            }
        }
        HolderCommand::ResumeReport { obligation_id } => {
            let (mut holder, _) = start(&settings, &config, &paths.data_dir)?;
            if holder.resume_report(&obligation_id) {
                println!("report channel resumed: it will be re-derived under a new key");
            } else {
                bail!("no halted report channel has that obligation id; nothing was changed");
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
    let holder = start_holder(settings, config, data_dir, system_clock(), sink)?;
    Ok((holder, tripped))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Durable state plus the running daemon's snapshot, read without the lock.
fn read_status(settings: &HolderSettings, data_dir: &Path) -> Result<HolderReport> {
    let state_path = settings.state_dir.join(STATE_FILE);
    let bytes = match std::fs::read(&state_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("the holder is not enrolled (no holder state in holder.state_dir)")
        }
        Err(error) => return Err(anyhow!("holder state unreadable: {error}")),
    };
    let state = HolderState::parse(&bytes).map_err(|_| anyhow!("holder state unreadable"))?;
    let seal = match std::fs::read(settings.state_dir.join(SEAL_FILE)) {
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
        .and_then(|snapshot| snapshot.live);
    Ok(report)
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
