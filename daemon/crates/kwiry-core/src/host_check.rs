//! Checks that a host and config can run the governed profile, without
//! serving. Config, room-root, storage, ingestion and mount checks call the
//! same functions startup uses; `governed_profile`, `loopback_bind` and
//! `snapshot_ids` are rules of this module, deliberately stricter than
//! startup. This module only sequences the checks and records every result
//! instead of stopping at the first failure. Nothing here opens a network
//! connection. The only writes are startup's own storage probe, inside a
//! `.storage-probe-*` directory that it removes.

use std::path::Path;

use serde::Serialize;

use crate::config::{Paths, staged_config_for_paths};
use crate::generation::DataRoot;
use crate::model::{Config, HostProfile};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCheckStatus {
    Pass,
    Fail,
    /// Not run because a check it depends on failed; counts as not passing.
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostCheck {
    pub name: &'static str,
    pub status: HostCheckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Check names, in report order. The caller appends composition-owned checks
/// (such as the signing key set) after these.
pub const HOST_CHECKS: [&str; 9] = [
    "config",
    "governed_profile",
    "loopback_bind",
    "snapshot_ids",
    "room_roots",
    "data_dir_storage",
    "data_dir_unlocked",
    "openat2_probe",
    "mount_separation",
];

pub struct GovernedHostReport {
    /// The parsed document, when it parsed, for composition-owned checks.
    pub config: Option<Config>,
    pub checks: Vec<HostCheck>,
}

impl GovernedHostReport {
    pub fn push(&mut self, name: &'static str, result: Result<(), String>) {
        let (status, detail) = match result {
            Ok(()) => (HostCheckStatus::Pass, None),
            Err(detail) => (HostCheckStatus::Fail, Some(detail)),
        };
        self.checks.push(HostCheck {
            name,
            status,
            detail,
        });
    }

    pub fn skip(&mut self, name: &'static str, reason: &str) {
        self.checks.push(HostCheck {
            name,
            status: HostCheckStatus::Skipped,
            detail: Some(reason.to_owned()),
        });
    }

    pub fn passed(&self) -> bool {
        self.checks
            .iter()
            .all(|check| check.status == HostCheckStatus::Pass)
    }
}

fn text(error: crate::Error) -> String {
    error.to_string()
}

pub fn governed_host_checks(paths: &Paths) -> GovernedHostReport {
    let stages = staged_config_for_paths(paths);
    let mut report = GovernedHostReport {
        config: None,
        checks: Vec::with_capacity(HOST_CHECKS.len() + 1),
    };
    let config = match stages.parsed {
        Ok(config) => config,
        Err(error) => {
            report.push("config", Err(text(error)));
            for name in &HOST_CHECKS[1..] {
                report.skip(name, "config did not parse");
            }
            redact_paths(&mut report, None, paths);
            return report;
        }
    };
    report.push("config", stages.validated.map_err(text));
    report.push(
        "governed_profile",
        if config.server.profile == HostProfile::OpenClast && config.server.profile_required {
            Ok(())
        } else {
            Err("server.profile must be the governed profile with profile_required = true".into())
        },
    );
    report.push("loopback_bind", loopback(&config.server.bind));
    report.push("snapshot_ids", snapshot_ids(&config));
    let roots = room_roots(&config);
    let roots_ok = roots.is_ok();
    report.push("room_roots", roots);
    let data_root = DataRoot::new(&paths.data_dir);
    let data = data_root.check_startup_storage().map_err(text);
    let data_ok = data.is_ok();
    report.push("data_dir_storage", data);
    if data_ok {
        report.push(
            "data_dir_unlocked",
            match data_root.writer_lock_held() {
                Ok(false) => Ok(()),
                Ok(true) => Err("another process holds the data directory writer lock".into()),
                Err(error) => Err(text(error)),
            },
        );
    } else {
        report.skip("data_dir_unlocked", "data directory storage failed");
    }
    if roots_ok && data_ok {
        report.push(
            "openat2_probe",
            crate::room_walk::probe_openclast_ingestion(&paths.data_dir, &config.vaults)
                .map_err(text),
        );
        report.push(
            "mount_separation",
            mount_separation(&config, &paths.data_dir),
        );
    } else {
        // Both probes need existing canonical roots; the ingestion probe would
        // otherwise create a missing data directory.
        for name in ["openat2_probe", "mount_separation"] {
            report.skip(name, "room roots or data directory failed");
        }
    }
    redact_paths(&mut report, Some(&config), paths);
    report.config = Some(config);
    report
}

/// Details name configured locations by role, never by path, matching the
/// private CLI's no-diagnostic-paths rule. Labels are never echoed at all.
pub fn redact_paths(report: &mut GovernedHostReport, config: Option<&Config>, paths: &Paths) {
    let mut named: Vec<(String, String)> = Vec::new();
    let mut add = |path: &Path, role: String| {
        for form in [Some(path.to_path_buf()), std::fs::canonicalize(path).ok()]
            .into_iter()
            .flatten()
        {
            let text = form.to_string_lossy().into_owned();
            if !text.is_empty() {
                named.push((text, format!("<{role}>")));
            }
        }
    };
    if let Some(config) = config {
        for (index, vault) in config.vaults.iter().enumerate() {
            add(&vault.path, format!("vaults[{index}]"));
        }
        if let Some(auth) = &config.auth.openclast {
            add(&auth.jwks_file, "jwks_file".into());
        }
    }
    add(&paths.data_dir, "data_dir".into());
    add(&paths.config, "config".into());
    // Longest first, so a room inside a named parent keeps its own role.
    named.sort_by_key(|(text, _)| std::cmp::Reverse(text.len()));
    for check in &mut report.checks {
        if let Some(detail) = &mut check.detail {
            for (text, role) in &named {
                *detail = detail.replace(text.as_str(), role);
            }
        }
    }
}

fn loopback(bind: &str) -> Result<(), String> {
    match bind.parse::<std::net::SocketAddr>() {
        Ok(address) if address.ip().is_loopback() => Ok(()),
        Ok(_) => Err("server.bind is not a loopback address".into()),
        Err(_) => Err("server.bind is not an IP socket address".into()),
    }
}

/// Labels are reported by registration position, never by value.
fn snapshot_ids(config: &Config) -> Result<(), String> {
    if config.vaults.is_empty() {
        return Err("no room is registered".into());
    }
    let defects: Vec<String> = config
        .vaults
        .iter()
        .enumerate()
        .filter_map(|(index, vault)| match &vault.snapshot_id {
            None => Some(format!("vaults[{index}] has no snapshot_id")),
            Some(id) if !id.is_live_check_label() => Some(format!(
                "vaults[{index}] snapshot_id is not 64 lowercase hex characters"
            )),
            Some(_) => None,
        })
        .collect();
    if defects.is_empty() {
        Ok(())
    } else {
        Err(defects.join("; "))
    }
}

fn room_roots(config: &Config) -> Result<(), String> {
    if config.vaults.is_empty() {
        return Err("no room is registered".into());
    }
    let defects: Vec<String> = config
        .vaults
        .iter()
        .enumerate()
        .filter_map(|(index, vault)| {
            match crate::room_boundary::require_canonical_room_root(&vault.path) {
                Ok(root) if root.is_dir() => None,
                Ok(_) => Some(format!("vaults[{index}] room root is not a directory")),
                Err(error) => Some(format!("vaults[{index}]: {error}")),
            }
        })
        .collect();
    if defects.is_empty() {
        Ok(())
    } else {
        Err(defects.join("; "))
    }
}

fn mount_separation(config: &Config, data_dir: &Path) -> Result<(), String> {
    crate::room_boundary::validate(config, Some(data_dir)).map_err(text)?;
    crate::room_boundary::validate_data_root(config, data_dir).map_err(text)
}
