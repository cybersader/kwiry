use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use fs2::FileExt;
use tempfile::NamedTempFile;

use crate::error::{Error, Result, io_error};
use crate::model::{Config, HostProfile, IndexFreshnessBasis, MAX_FILE_BYTES, VaultRegistration};
use crate::partition::{MAX_LABELED_ROOMS, label_resource_fits};

const ROOM_FORMAT: crate::OpenClastRoomFormat = crate::openclast_room_format!();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf,
    pub data_dir: PathBuf,
    pub config_lock: PathBuf,
    local_state_dir: PathBuf,
    default_token_file: PathBuf,
}

impl Paths {
    pub fn resolve(config: Option<PathBuf>, data_dir: Option<PathBuf>) -> Result<Self> {
        let project = ProjectDirs::from("", "", "kwiry").ok_or_else(|| Error::InvalidConfig {
            path: PathBuf::from("<platform>"),
            message: "could not resolve platform config and data directories".to_owned(),
        })?;
        let custom_config = config.is_some();
        let custom_data_dir = data_dir.is_some();
        let config =
            absolutize(config.unwrap_or_else(|| project.config_dir().join("config.toml")))?;
        let platform_state_dir = absolutize(project.data_local_dir().to_path_buf())?;
        let data_dir = absolutize(data_dir.unwrap_or_else(|| platform_state_dir.join("index")))?;
        let local_state_dir = if custom_data_dir {
            data_dir.clone()
        } else {
            platform_state_dir
        };
        let default_token_file = default_token_file(&config, &local_state_dir, custom_config);

        Ok(Self {
            config_lock: config.with_extension("lock"),
            config,
            data_dir,
            local_state_dir,
            default_token_file,
        })
    }

    pub fn token_path(&self, config: &Config) -> PathBuf {
        match config.auth.token_file.as_deref() {
            Some(path) if path.is_absolute() => path.to_path_buf(),
            Some(path) => self
                .config
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(path),
            None => self.default_token_path().to_path_buf(),
        }
    }

    pub fn default_token_path(&self) -> &Path {
        &self.default_token_file
    }

    pub fn connection_path(&self) -> PathBuf {
        self.local_state_dir.join("connection.json")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.local_state_dir.join("logs")
    }

    pub fn setup_lock_path(&self) -> PathBuf {
        self.local_state_dir.join("setup.lock")
    }
}

fn absolutize(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    let current = std::env::current_dir().map_err(|error| io_error(".", error))?;
    Ok(current.join(path))
}

#[cfg(windows)]
fn default_token_file(config: &Path, local_state_dir: &Path, custom_config: bool) -> PathBuf {
    if custom_config {
        config.with_extension("token")
    } else {
        local_state_dir.join("config.token")
    }
}

#[cfg(not(windows))]
fn default_token_file(config: &Path, _local_state_dir: &Path, _custom_config: bool) -> PathBuf {
    config.with_extension("token")
}

#[derive(Debug)]
pub struct ConfigLock {
    file: File,
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub fn acquire_config_lock(path: &Path) -> Result<ConfigLock> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| io_error(path, error))?;
    file.try_lock_exclusive()
        .map_err(|_| Error::LockHeld(path.to_path_buf()))?;
    Ok(ConfigLock { file })
}

pub fn acquire_setup_lock(paths: &Paths) -> Result<ConfigLock> {
    acquire_config_lock(&paths.setup_lock_path())
}

pub fn load_config(path: &Path) -> Result<Config> {
    let mut config = load_config_unlocked(path)?;
    prepare_config(path, &mut config, None)?;
    Ok(config)
}

pub fn load_config_for_paths(paths: &Paths) -> Result<Config> {
    let mut config = load_config_unlocked(&paths.config)?;
    prepare_config(&paths.config, &mut config, Some(paths))?;
    Ok(config)
}

fn prepare_config(path: &Path, config: &mut Config, paths: Option<&Paths>) -> Result<()> {
    validate_config(path, config)?;
    if config.server.profile == HostProfile::OpenClast {
        let config_path = absolutize(path.to_path_buf())?;
        let config_dir = config_path
            .parent()
            .expect("absolute config path has a parent");
        let mut protected = vec![config_dir.to_path_buf(), config_path.clone()];
        if let Ok(target) = fs::canonicalize(&config_path)
            && let Some(parent) = target.parent()
        {
            protected.push(parent.to_path_buf());
        }
        if let Some(paths) = paths {
            protected.extend([
                paths.data_dir.clone(),
                paths.local_state_dir.clone(),
                paths.token_path(config),
                paths.config_lock.clone(),
                paths.connection_path(),
                paths.logs_dir(),
            ]);
        } else {
            let token = config.auth.token_file.as_ref().map_or_else(
                || config_path.with_extension("token"),
                |token| config_dir.join(token),
            );
            protected.push(token);
            protected.push(config_path.with_extension("lock"));
        }
        if let Some(holder) = &config.holder {
            protected.push(holder.state_dir.clone());
        }
        config.openclast_state_paths = protected;
        crate::room_boundary::validate(config, None)?;
    }
    Ok(())
}

pub fn register_vault(
    paths: &Paths,
    id: String,
    path: PathBuf,
    room: Option<String>,
) -> Result<VaultRegistration> {
    let candidate = || {
        let mut config = load_config_for_paths(paths)?;
        let registration = add_vault(&mut config, id.clone(), path.clone(), room.clone())?;
        Ok::<_, Error>((config, registration))
    };
    // Check the proposed boundary before acquiring a lock that can create
    // the config directory; re-read under the lock before publishing.
    candidate()?;
    let _lock = acquire_config_lock(&paths.config_lock)?;
    let (config, registration) = candidate()?;
    save_config_unlocked(&paths.config, &config)?;
    Ok(registration)
}

pub fn save_config(path: &Path, config: &Config) -> Result<()> {
    let mut config = config.clone();
    prepare_config(path, &mut config, None)?;
    let _lock = acquire_config_lock(&path.with_extension("lock"))?;
    save_config_unlocked(path, &config)
}

pub fn update_config<T>(path: &Path, update: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    load_config(path)?;
    let _lock = acquire_config_lock(&path.with_extension("lock"))?;
    let mut config = load_config(path)?;
    let result = update(&mut config)?;
    prepare_config(path, &mut config, None)?;
    save_config_unlocked(path, &config)?;
    Ok(result)
}

fn load_config_unlocked(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }

    let source = fs::read_to_string(path).map_err(|error| io_error(path, error))?;
    let config: Config = toml::from_str(&source).map_err(|error| Error::InvalidConfig {
        path: path.to_path_buf(),
        // Display would echo the source line, including producer labels.
        message: error.message().to_owned(),
    })?;

    if config.version != 1 {
        return Err(Error::InvalidConfig {
            path: path.to_path_buf(),
            message: format!("unsupported config version {}", config.version),
        });
    }

    Ok(config)
}

fn save_config_unlocked(path: &Path, config: &Config) -> Result<()> {
    let encoded = toml::to_string_pretty(config).map_err(|error| Error::InvalidConfig {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    persist_config_bytes(path, encoded.as_bytes())
}

fn persist_config_bytes(path: &Path, encoded: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| Error::InvalidConfig {
        path: path.to_path_buf(),
        message: "configuration path has no parent".to_owned(),
    })?;
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| io_error(parent, error))?;
    temporary
        .write_all(encoded)
        .map_err(|error| io_error(temporary.path(), error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| io_error(temporary.path(), error))?;
    temporary
        .persist(path)
        .map_err(|error| io_error(path, error.error))?;
    sync_parent(parent)?;
    Ok(())
}

pub fn valid_room_identifier(value: &str) -> bool {
    opaque_identifier(value, 22)
}

pub fn valid_tenant_identifier(value: &str) -> bool {
    value
        .strip_prefix("tenant_")
        .is_some_and(|suffix| opaque_identifier(suffix, 20))
}

fn opaque_identifier(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn allows_repeated_vault(config: &Config) -> bool {
    ROOM_FORMAT.admits_repeated_vault()
        && config.server.profile == HostProfile::OpenClast
        && config
            .auth
            .openclast
            .as_ref()
            .is_some_and(|auth| auth.legacy_identity_accept_until.is_some())
}

fn validate_room_registrations(path: &Path, config: &Config) -> Result<()> {
    let mut vaults = BTreeSet::new();
    let mut rooms = BTreeSet::new();
    let mut roots = BTreeSet::new();
    let invalid = |message: &str| Error::InvalidConfig {
        path: path.to_path_buf(),
        message: message.to_owned(),
    };
    for vault in &config.vaults {
        if !vaults.insert(&vault.id) && !allows_repeated_vault(config) {
            return Err(Error::DuplicateVaultId(vault.id.clone()));
        }
        if config.server.profile == HostProfile::OpenClast {
            let room = vault
                .room
                .as_deref()
                .ok_or_else(|| invalid("OpenClast registration requires a room"))?;
            if !rooms.insert(room) {
                return Err(invalid("a room may be registered only once"));
            }
            let root = crate::room_boundary::require_canonical_room_root(&vault.path)?;
            if !root.is_dir()
                || roots.iter().any(|previous: &PathBuf| {
                    root.starts_with(previous) || previous.starts_with(&root)
                })
            {
                return Err(invalid("OpenClast room roots must be disjoint directories"));
            }
            roots.insert(root);
        }
    }
    Ok(())
}

fn validate_config(path: &Path, config: &Config) -> Result<()> {
    if config.vaults.iter().any(|v| {
        v.snapshot_id.is_some()
            && (config.server.profile != HostProfile::OpenClast
                || v.room.is_none()
                || !config.server.profile_required)
    }) {
        return Err(Error::InvalidConfig {
            path: path.to_path_buf(),
            message: "snapshot_id requires a profile-required governed room registration".into(),
        });
    }
    if config
        .vaults
        .iter()
        .filter(|v| v.snapshot_id.is_some())
        .count()
        > MAX_LABELED_ROOMS
    {
        return Err(Error::InvalidConfig {
            path: path.to_path_buf(),
            message: format!("at most {MAX_LABELED_ROOMS} OpenClast rooms may have a snapshot_id"),
        });
    }
    if config.server.profile_required
        && !(config.server.profile == HostProfile::OpenClast
            && crate::ROOM_INCLUSIVE_IDS
            && config.indexing.basis == IndexFreshnessBasis::StrictHash
            && config
                .server
                .bind
                .parse::<std::net::SocketAddr>()
                .is_ok_and(|addr| addr.ip().is_loopback())
            && config.auth.openclast.as_ref().is_some_and(|auth| {
                auth.legacy_identity_accept_until == Some(0)
                    && !auth.workload_subject_allowlist.is_empty()
            }))
    {
        return Err(Error::InvalidConfig { path: path.to_path_buf(), message: "profile_required requires governed room identity, loopback, strict_hash and workload-only authentication".into() });
    }
    if config.server.bind.trim().is_empty() {
        return Err(Error::InvalidConfig {
            path: path.to_path_buf(),
            message: "server bind must not be empty".to_owned(),
        });
    }
    if config.server.profile == HostProfile::OpenClast
        && config.indexing.max_stale_serve_seconds > 86_400
    {
        return Err(Error::InvalidConfig {
            path: path.to_path_buf(),
            message: "indexing.max_stale_serve_seconds must be between 0 and 86400".to_owned(),
        });
    }
    if config.indexing.basis == IndexFreshnessBasis::ProducerManifest {
        return Err(Error::InvalidConfig {
            path: path.to_path_buf(),
            message: "indexing.basis producer_manifest is not available yet".to_owned(),
        });
    }
    if !(1..=256).contains(&config.indexing.audit_sources_per_pass) {
        return Err(Error::InvalidConfig {
            path: path.to_path_buf(),
            message: "indexing.audit_sources_per_pass must be between 1 and 256".to_owned(),
        });
    }
    if !(MAX_FILE_BYTES..=64 * 1024 * 1024).contains(&config.indexing.audit_bytes_per_pass) {
        return Err(Error::InvalidConfig {
            path: path.to_path_buf(),
            message: format!(
                "indexing.audit_bytes_per_pass must be between {MAX_FILE_BYTES} and {}",
                64 * 1024 * 1024
            ),
        });
    }
    if config.indexing.racy_window_millis > 60_000 {
        return Err(Error::InvalidConfig {
            path: path.to_path_buf(),
            message: "indexing.racy_window_millis must not exceed 60000".to_owned(),
        });
    }

    match config.server.profile {
        HostProfile::Desktop => {
            if config.auth.openclast.is_some() {
                return Err(Error::InvalidConfig {
                    path: path.to_path_buf(),
                    message: "desktop profile must not configure auth.openclast".to_owned(),
                });
            }
        }
        HostProfile::OpenClast => {
            if config.auth.token_file.is_some() {
                return Err(Error::InvalidConfig {
                    path: path.to_path_buf(),
                    message: "openclast profile must not configure a desktop token file".to_owned(),
                });
            }
            if config.semantic.enabled {
                return Err(Error::InvalidConfig {
                    path: path.to_path_buf(),
                    message: "openclast profile does not serve semantic or hybrid search in IG-1"
                        .to_owned(),
                });
            }
            let auth = config
                .auth
                .openclast
                .as_ref()
                .ok_or_else(|| Error::InvalidConfig {
                    path: path.to_path_buf(),
                    message: "openclast profile requires auth.openclast".to_owned(),
                })?;
            for (name, value) in [
                ("tenant_id", auth.tenant_id.as_str()),
                ("issuer", auth.issuer.as_str()),
                ("audience", auth.audience.as_str()),
            ] {
                if value.trim().is_empty() {
                    return Err(Error::InvalidConfig {
                        path: path.to_path_buf(),
                        message: format!("auth.openclast.{name} must not be empty"),
                    });
                }
            }
            if config.vaults.iter().any(|vault| {
                vault.snapshot_id.is_some()
                    && vault
                        .room
                        .as_deref()
                        .is_some_and(|room| !label_resource_fits(&auth.tenant_id, &vault.id, room))
            }) {
                return Err(Error::InvalidConfig {
                    path: path.to_path_buf(),
                    message:
                        "OpenClast resource identifiers are too long for snapshot label storage"
                            .into(),
                });
            }
            if crate::source::ROOM_INCLUSIVE_IDS && !valid_tenant_identifier(&auth.tenant_id) {
                return Err(Error::InvalidConfig {
                    path: path.to_path_buf(),
                    message: "auth.openclast.tenant_id must match tenant_[A-Za-z0-9_-]{20}"
                        .to_owned(),
                });
            }
            let mut workload_subjects = BTreeSet::new();
            if auth
                .workload_subject_allowlist
                .iter()
                .any(|subject| subject.trim().is_empty() || !workload_subjects.insert(subject))
            {
                return Err(Error::InvalidConfig {
                    path: path.to_path_buf(),
                    message: "auth.openclast.workload_subject_allowlist must contain distinct nonempty workload subjects, never user identities".to_owned(),
                });
            }
            if !auth.jwks_file.is_absolute() {
                return Err(Error::InvalidConfig {
                    path: path.to_path_buf(),
                    message: "auth.openclast.jwks_file must be absolute".to_owned(),
                });
            }
            if !(1..=60).contains(&auth.max_token_ttl_seconds) {
                return Err(Error::InvalidConfig {
                    path: path.to_path_buf(),
                    message: "auth.openclast.max_token_ttl_seconds must be between 1 and 60"
                        .to_owned(),
                });
            }
        }
    }

    for vault in &config.vaults {
        if vault.id.trim().is_empty() {
            return Err(Error::InvalidConfig {
                path: path.to_path_buf(),
                message: "vault IDs must not be empty".to_owned(),
            });
        }
        // OpenClast validates the exact raw spelling in require_canonical_room_root.
        if config.server.profile == HostProfile::Desktop && !vault.path.is_absolute() {
            return Err(Error::InvalidConfig {
                path: path.to_path_buf(),
                message: format!("vault path must be absolute: {}", vault.path.display()),
            });
        }
        if vault
            .room
            .as_deref()
            .is_some_and(|room| room.trim().is_empty())
        {
            return Err(Error::InvalidConfig {
                path: path.to_path_buf(),
                message: format!("vault room must not be empty: {}", vault.id),
            });
        }
        if config.server.profile == HostProfile::OpenClast && vault.room.is_none() {
            return Err(Error::InvalidConfig {
                path: path.to_path_buf(),
                message: format!(
                    "openclast profile requires a nonempty room for vault {}",
                    vault.id
                ),
            });
        }
    }
    validate_holder(path, config)?;
    validate_room_registrations(path, config)
}

fn validate_holder(path: &Path, config: &Config) -> Result<()> {
    let Some(holder) = &config.holder else {
        return Ok(());
    };
    let invalid = |message: String| Error::InvalidConfig {
        path: path.to_path_buf(),
        message,
    };
    if config.server.profile != HostProfile::OpenClast {
        return Err(invalid(
            "the [holder] section requires server.profile = \"openclast\"".to_owned(),
        ));
    }
    validate_holder_issuer(&holder.issuer)
        .map_err(|reason| invalid(format!("holder.issuer {reason}")))?;
    if !holder.state_dir.is_absolute() {
        return Err(invalid("holder.state_dir must be absolute".to_owned()));
    }
    for vault in &config.vaults {
        if holder.state_dir.starts_with(&vault.path) || vault.path.starts_with(&holder.state_dir) {
            return Err(invalid(format!(
                "holder.state_dir must not overlap registered vault {}",
                vault.id
            )));
        }
    }
    if let Some(page_size) = holder.page_size
        && !(1..=1000).contains(&page_size)
    {
        return Err(invalid(
            "holder.page_size must be between 1 and 1000".to_owned(),
        ));
    }
    if let Some(skew) = holder.clock_skew_allowance_seconds
        && skew > 30
    {
        return Err(invalid(
            "holder.clock_skew_allowance_seconds must be between 0 and 30".to_owned(),
        ));
    }
    if let Some(stale) = holder.max_stale_serve_seconds
        && stale > 86_400
    {
        return Err(invalid(
            "holder.max_stale_serve_seconds must be between 0 and 86400".to_owned(),
        ));
    }
    Ok(())
}

/// `https://host[:port]/path` with no user info, query, fragment or trailing
/// slash. Plain `http` is accepted only for a loopback IP literal, for local
/// authorities and tests.
fn validate_holder_issuer(issuer: &str) -> std::result::Result<(), &'static str> {
    if issuer.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("must not contain whitespace");
    }
    let (https, rest) = if let Some(rest) = issuer.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = issuer.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err("must be an https URL");
    };
    if issuer.contains(['?', '#']) {
        return Err("must not carry a query or fragment");
    }
    if issuer.ends_with('/') {
        return Err("must not end with a slash");
    }
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.is_empty() || authority.contains('@') {
        return Err("must name a host without user info");
    }
    if path.is_empty() {
        return Err("must include the lifecycle base path");
    }
    if !https {
        // SocketAddr accepts only IP literals, requires brackets for IPv6,
        // and validates any explicit port. Try a default port when omitted.
        let loopback = authority
            .parse::<std::net::SocketAddr>()
            .or_else(|_| format!("{authority}:80").parse())
            .is_ok_and(|address| address.ip().is_loopback());
        if !loopback || authority.contains('%') {
            return Err("must use https unless the host is a loopback IP literal");
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultRegistrationDisposition {
    Added,
    Unchanged,
}

pub fn add_vault(
    config: &mut Config,
    id: String,
    path: PathBuf,
    room: Option<String>,
) -> Result<VaultRegistration> {
    if !allows_repeated_vault(config) && config.vaults.iter().any(|vault| vault.id == id) {
        return Err(Error::DuplicateVaultId(id));
    }

    if id.trim().is_empty() {
        return Err(Error::InvalidVaultPath(path));
    }
    if room.as_deref().is_some_and(|room| room.trim().is_empty()) {
        return Err(Error::InvalidConfig {
            path,
            message: "room must not be empty".to_owned(),
        });
    }

    let canonical_path = registration_root(config.server.profile, path)?;
    let registration = VaultRegistration {
        snapshot_id: None,
        id,
        path: canonical_path,
        room,
    };
    insert_registration(config, &registration)?;
    Ok(registration)
}

pub fn ensure_vault_registration(
    config: &mut Config,
    id: String,
    path: PathBuf,
    room: Option<String>,
) -> Result<(VaultRegistration, VaultRegistrationDisposition)> {
    if id.trim().is_empty() {
        return Err(Error::InvalidVaultPath(path));
    }
    if room.as_deref().is_some_and(|room| room.trim().is_empty()) {
        return Err(Error::InvalidConfig {
            path,
            message: "room must not be empty".to_owned(),
        });
    }

    let canonical_path = registration_root(config.server.profile, path)?;
    if let Some(existing) = config
        .vaults
        .iter()
        .find(|vault| vault.id == id && (vault.room == room || !allows_repeated_vault(config)))
    {
        if existing.path == canonical_path && existing.room == room {
            if config.server.profile == HostProfile::OpenClast {
                validate_config(&canonical_path, config)?;
                crate::room_boundary::validate(config, None)?;
            }
            return Ok((existing.clone(), VaultRegistrationDisposition::Unchanged));
        }
        return Err(Error::InvalidConfig {
            path: canonical_path,
            message: format!("vault ID {id:?} is already registered with different settings"),
        });
    }
    if let Some(existing) = config
        .vaults
        .iter()
        .find(|vault| vault.path == canonical_path)
    {
        return Err(Error::InvalidConfig {
            path: canonical_path,
            message: format!("vault path is already registered with ID {:?}", existing.id),
        });
    }

    let registration = VaultRegistration {
        snapshot_id: None,
        id,
        path: canonical_path,
        room,
    };
    insert_registration(config, &registration)?;
    Ok((registration, VaultRegistrationDisposition::Added))
}

fn registration_root(profile: HostProfile, path: PathBuf) -> Result<PathBuf> {
    if profile == HostProfile::OpenClast {
        let canonical = crate::room_boundary::require_canonical_room_root(&path)?;
        if !canonical.is_dir() {
            return Err(Error::InvalidVaultPath(path));
        }
        return Ok(canonical);
    }
    if !path.is_absolute() || !path.is_dir() {
        return Err(Error::InvalidVaultPath(path));
    }
    fs::canonicalize(&path).map_err(|_| Error::InvalidVaultPath(path))
}

fn insert_registration(config: &mut Config, registration: &VaultRegistration) -> Result<()> {
    if config.server.profile == HostProfile::OpenClast {
        let mut candidate = config.clone();
        candidate.vaults.push(registration.clone());
        validate_config(&registration.path, &candidate)?;
        crate::room_boundary::validate(&candidate, None)?;
        *config = candidate;
    } else {
        config.vaults.push(registration.clone());
    }
    config
        .vaults
        .sort_by(|left, right| (&left.id, &left.room).cmp(&(&right.id, &right.room)));
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_error(path, error))
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn invalid_snapshot_grammar_is_rejected_at_load_without_echoing_the_value() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for value in [
            "".to_owned(),
            "peach/branch".into(),
            "a b".into(),
            "ä".into(),
            "x".repeat(129),
        ] {
            let encoded = serde_json::to_string(&value).unwrap();
            fs::write(&path,format!("[[vaults]]\nvault_id = 'garden'\npath = '/synthetic'\nsnapshot_id = {encoded}\n")).unwrap();
            let error = load_config(&path).unwrap_err().to_string();
            assert!(error.contains(crate::SnapshotId::INVALID), "{error}");
            if !value.is_empty() {
                assert!(!error.contains(&value));
            }
        }
        for valid in ["0".into(), "AZaz09._:-".into(), "x".repeat(128)] {
            assert!(valid.parse::<crate::SnapshotId>().is_ok());
        }
    }

    #[test]
    fn snapshot_id_rejected_in_desktop_profile_or_without_room() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for (profile, room) in [
            ("desktop", "room = 'room'"),
            ("openclast", ""),
            ("openclast", "room = 'room'"),
        ] {
            fs::write(&path,format!("[server]\nprofile = '{profile}'\n[[vaults]]\nvault_id = 'garden'\npath = '/synthetic'\nsnapshot_id = 'sample.label'\n{room}")).unwrap();
            let error = load_config(&path).unwrap_err().to_string();
            assert!(error.contains("snapshot_id requires"));
            assert!(!error.contains("sample.label"));
        }
    }

    #[test]
    fn config_profile_required_precludes_unsafe_serving_settings() {
        let path = Path::new("config.toml");
        let mut base = Config::default();
        base.server.profile = HostProfile::OpenClast;
        base.server.profile_required = true;
        base.auth.openclast = Some(crate::OpenClastAuthConfig {
            tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
            issuer: "issuer".into(),
            audience: "search".into(),
            jwks_file: std::env::temp_dir().join("synthetic-public-jwks"),
            workload_subject_allowlist: vec!["synthetic-workload".into()],
            max_token_ttl_seconds: 60,
            legacy_identity_accept_until: Some(0),
        });
        assert_eq!(
            validate_config(path, &base).is_ok(),
            crate::ROOM_INCLUSIVE_IDS
        );
        for kind in ["profile", "external", "basis", "legacy", "subject", "ttl"] {
            let mut candidate = base.clone();
            match kind {
                "profile" => candidate.server.profile = HostProfile::Desktop,
                "external" => candidate.server.bind = "0.0.0.0:0".into(),
                "basis" => candidate.indexing.basis = IndexFreshnessBasis::MetadataAudit,
                "legacy" => {
                    candidate
                        .auth
                        .openclast
                        .as_mut()
                        .unwrap()
                        .legacy_identity_accept_until = None
                }
                "subject" => candidate
                    .auth
                    .openclast
                    .as_mut()
                    .unwrap()
                    .workload_subject_allowlist
                    .clear(),
                "ttl" => {
                    candidate
                        .auth
                        .openclast
                        .as_mut()
                        .unwrap()
                        .max_token_ttl_seconds = 61
                }
                _ => unreachable!(),
            }
            assert!(validate_config(path, &candidate).is_err(), "{kind}");
        }
    }

    #[test]
    fn config_rejects_more_than_the_snapshot_labeled_room_bound() {
        use std::fmt::Write as _;

        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = String::from("[server]\nprofile = 'openclast'\nprofile_required = true\n");
        for i in 0..=MAX_LABELED_ROOMS {
            writeln!(
                config,
                "[[vaults]]\nvault_id = 'vault-{i}'\nroom = 'room-{i}'\npath = '/synthetic/{i}'\nsnapshot_id = 'sample.label'"
            )
            .unwrap();
        }
        fs::write(&path, config).unwrap();
        let error = load_config(&path).unwrap_err().to_string();
        assert!(error.contains(&format!("at most {MAX_LABELED_ROOMS} OpenClast rooms")));
        assert!(!error.contains("sample.label"));
    }

    #[test]
    fn config_round_trips_and_sorts_vaults() {
        let temporary = tempdir().unwrap();
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let config_path = temporary.path().join("config.toml");
        let mut config = Config::default();

        add_vault(&mut config, "zeta".into(), second, None).unwrap();
        add_vault(&mut config, "alpha".into(), first, Some("room-a".into())).unwrap();
        save_config(&config_path, &config).unwrap();

        let loaded = load_config(&config_path).unwrap();
        assert_eq!(loaded, config);
        assert_eq!(loaded.vaults[0].id, "alpha");
    }

    #[test]
    fn legacy_config_loads_with_server_and_auth_defaults() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("cynario");
        fs::create_dir(&vault).unwrap();
        let config_path = temporary.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                "version = 1\n\n[[vaults]]\nvault_id = \"cynario\"\npath = {:?}\n",
                vault.display().to_string()
            ),
        )
        .unwrap();

        let loaded = load_config(&config_path).unwrap();
        assert_eq!(loaded.server.profile, HostProfile::Desktop);
        assert_eq!(loaded.server.bind, crate::model::DEFAULT_BIND);
        assert_eq!(loaded.auth.token_file, None);
        assert_eq!(loaded.auth.openclast, None);
        assert!(!loaded.semantic.enabled);
        assert_eq!(loaded.vaults[0].id, "cynario");
    }

    #[test]
    fn openclast_profile_requires_exact_enterprise_configuration() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).unwrap();
        let config_path = temporary.path().join("control/config.toml");
        let mut config = Config::default();
        config.server.profile = HostProfile::OpenClast;
        config.vaults.push(VaultRegistration {
            snapshot_id: None,
            id: "notes".into(),
            path: fs::canonicalize(vault).unwrap(),
            room: None,
        });

        let error = save_config(&config_path, &config).unwrap_err();
        assert!(error.to_string().contains("requires auth.openclast"));

        config.auth.openclast = Some(crate::model::OpenClastAuthConfig {
            tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
            issuer: "openclast-search".into(),
            audience: "kwiry-search".into(),
            jwks_file: temporary.path().join("jwks.json"),
            max_token_ttl_seconds: 60,
            workload_subject_allowlist: Vec::new(),
            legacy_identity_accept_until: None,
        });
        let error = save_config(&config_path, &config).unwrap_err();
        assert!(error.to_string().contains("requires a nonempty room"));

        config.vaults[0].room = Some("room-a".into());
        save_config(&config_path, &config).unwrap();
        assert!(
            fs::read_to_string(&config_path)
                .unwrap()
                .contains("profile = \"openclast\"")
        );
        assert_eq!(
            load_config(&config_path).unwrap().server.profile,
            HostProfile::OpenClast
        );

        config.auth.token_file = Some(temporary.path().join("desktop.token"));
        let error = save_config(&config_path, &config).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must not configure a desktop token")
        );
    }

    #[cfg(target_os = "linux")]
    fn assert_pseudo_mount_refused(place_state: impl FnOnce(&mut Config, &mut Paths, &Path)) {
        use crate::room_boundary::with_mount_snapshot;

        let temporary = tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        let pseudo = root.join("pseudo");
        fs::create_dir(&pseudo).unwrap();
        let mut paths = Paths::resolve(
            Some(root.join("control/config.toml")),
            Some(root.join("local-state")),
        )
        .unwrap();
        let mut config = holder_config(&complete_holder(&root)).unwrap();
        config.auth.openclast = Some(crate::model::OpenClastAuthConfig {
            tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
            issuer: "issuer".into(),
            audience: "audience".into(),
            jwks_file: root.join("jwks.json"),
            max_token_ttl_seconds: 60,
            workload_subject_allowlist: vec![],
            legacy_identity_accept_until: None,
        });
        place_state(&mut config, &mut paths, &pseudo);
        save_config_unlocked(&paths.config, &config).unwrap();

        let mut snapshot = b"1 0 8:1 / / rw - ext4 fixture rw\n".to_vec();
        with_mount_snapshot(snapshot.clone(), || {
            load_config_for_paths(&paths).unwrap();
        });
        snapshot.extend_from_slice(b"2 1 0:42 net:[42] ");
        for &byte in pseudo.as_os_str().as_encoded_bytes() {
            match byte {
                b' ' => snapshot.extend_from_slice(b"\\040"),
                b'\t' => snapshot.extend_from_slice(b"\\011"),
                b'\n' => snapshot.extend_from_slice(b"\\012"),
                b'\\' => snapshot.extend_from_slice(b"\\134"),
                _ => snapshot.push(byte),
            }
        }
        snapshot.extend_from_slice(b" rw - nsfs fixture rw\n");
        with_mount_snapshot(snapshot, || {
            let error = load_config_for_paths(&paths).unwrap_err();
            assert!(matches!(
                error,
                Error::State(message)
                    if message == "openclast_room_boundary: path is on a mount with a non-absolute root"
            ));
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn openclast_config_refuses_data_directory_on_pseudo_mount() {
        assert_pseudo_mount_refused(|_, paths, pseudo| {
            // Leave the local state directory and its paths on the ordinary mount.
            paths.data_dir = pseudo.join("index");
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn openclast_config_refuses_holder_state_directory_on_pseudo_mount() {
        assert_pseudo_mount_refused(|config, _, pseudo| {
            config.holder.as_mut().unwrap().state_dir = pseudo.join("holder");
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn openclast_config_refuses_jwks_file_on_pseudo_mount() {
        assert_pseudo_mount_refused(|config, _, pseudo| {
            config.auth.openclast.as_mut().unwrap().jwks_file = pseudo.join("keys.json");
        });
    }

    #[test]
    fn relative_overrides_are_absolutized_without_existing() {
        let current = std::env::current_dir().unwrap();
        let paths = Paths::resolve(
            Some(PathBuf::from("nested/config.toml")),
            Some(PathBuf::from("state")),
        )
        .unwrap();

        assert_eq!(paths.config, current.join("nested/config.toml"));
        assert_eq!(paths.config_lock, current.join("nested/config.lock"));
        assert_eq!(paths.data_dir, current.join("state"));
        assert_eq!(
            paths.connection_path(),
            current.join("state/connection.json")
        );
        assert_eq!(paths.logs_dir(), current.join("state/logs"));
        assert_eq!(paths.setup_lock_path(), current.join("state/setup.lock"));
    }

    #[test]
    fn default_token_for_nested_config_is_not_double_prefixed() {
        let current = std::env::current_dir().unwrap();
        let paths = Paths::resolve(Some(PathBuf::from("nested/config.toml")), None).unwrap();
        let mut config = Config::default();

        assert_eq!(
            paths.default_token_path(),
            current.join("nested/config.token")
        );
        assert_eq!(
            paths.token_path(&config),
            current.join("nested/config.token")
        );

        config.auth.token_file = Some(PathBuf::from("config.token"));
        assert_eq!(
            paths.token_path(&config),
            current.join("nested/config.token")
        );
    }

    #[test]
    fn update_config_serializes_load_mutate_save() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).unwrap();
        let config_path = temporary.path().join("config.toml");

        let registration = update_config(&config_path, |config| {
            add_vault(config, "notes".into(), vault, None)
        })
        .unwrap();

        assert_eq!(registration.id, "notes");
        assert_eq!(load_config(&config_path).unwrap().vaults.len(), 1);
    }

    #[test]
    fn duplicate_vault_ids_are_rejected() {
        let temporary = tempdir().unwrap();
        let mut config = Config::default();
        add_vault(
            &mut config,
            "notes".into(),
            temporary.path().to_path_buf(),
            None,
        )
        .unwrap();

        let error = add_vault(
            &mut config,
            "notes".into(),
            temporary.path().to_path_buf(),
            None,
        )
        .unwrap_err();
        assert!(matches!(error, Error::DuplicateVaultId(id) if id == "notes"));
    }

    #[test]
    fn ensure_vault_registration_is_idempotent_and_detects_conflicts() {
        let temporary = tempdir().unwrap();
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let mut config = Config::default();

        let (registration, disposition) = ensure_vault_registration(
            &mut config,
            "notes".into(),
            first.clone(),
            Some("work".into()),
        )
        .unwrap();
        assert_eq!(disposition, VaultRegistrationDisposition::Added);
        assert_eq!(registration.path, fs::canonicalize(&first).unwrap());

        let (_, disposition) = ensure_vault_registration(
            &mut config,
            "notes".into(),
            first.clone(),
            Some("work".into()),
        )
        .unwrap();
        assert_eq!(disposition, VaultRegistrationDisposition::Unchanged);
        assert_eq!(config.vaults.len(), 1);

        assert!(matches!(
            ensure_vault_registration(&mut config, "notes".into(), second, Some("work".into())),
            Err(Error::InvalidConfig { .. })
        ));
        assert!(matches!(
            ensure_vault_registration(&mut config, "other".into(), first, Some("work".into())),
            Err(Error::InvalidConfig { .. })
        ));
    }

    #[test]
    fn setup_lock_uses_the_local_state_directory() {
        let temporary = tempdir().unwrap();
        let paths = Paths::resolve(
            Some(temporary.path().join("config/config.toml")),
            Some(temporary.path().join("state")),
        )
        .unwrap();

        let first = acquire_setup_lock(&paths).unwrap();
        assert!(matches!(
            acquire_setup_lock(&paths),
            Err(Error::LockHeld(_))
        ));
        drop(first);
        acquire_setup_lock(&paths).unwrap();
    }

    #[test]
    fn multi_room_registration_vectors_n19_n21_n26_n27_n29() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("control/config.toml");
        fs::create_dir(path.parent().unwrap()).unwrap();
        let root = |name| {
            let root = dir.path().join(name);
            fs::create_dir_all(&root).unwrap();
            fs::canonicalize(root).unwrap()
        };
        let a = VaultRegistration {
            snapshot_id: None,
            id: "VaultOne_0123456789abc".into(),
            path: root("a"),
            room: Some("RoomAaaa_0123456789abc".into()),
        };
        let b = VaultRegistration {
            snapshot_id: None,
            id: a.id.clone(),
            path: root("b"),
            room: Some("RoomBbbb_0123456789abc".into()),
        };
        let mut config = Config::default();
        config.server.profile = HostProfile::OpenClast;
        config.auth.openclast = Some(crate::model::OpenClastAuthConfig {
            tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
            issuer: "issuer".into(),
            audience: "audience".into(),
            jwks_file: dir.path().join("jwks.json"),
            max_token_ttl_seconds: 60,
            workload_subject_allowlist: vec![],
            legacy_identity_accept_until: Some(0),
        });
        let load = |candidate: &Config| {
            fs::write(&path, toml::to_string_pretty(candidate).unwrap()).unwrap();
            load_config(&path)
        };
        config.vaults = vec![a.clone(), b.clone()];
        if !crate::source::ROOM_INCLUSIVE_IDS {
            assert!(load(&config).is_err(), "N26: actual legacy-format build");
            return;
        }
        assert!(load(&config).is_ok());
        let mut refused = config.clone();
        refused.vaults[1].path = root("a/nested");
        assert!(load(&refused).is_err(), "N19 load");
        let mut one = config.clone();
        one.vaults.pop();
        assert!(
            add_vault(
                &mut one,
                b.id.clone(),
                refused.vaults[1].path.clone(),
                b.room.clone()
            )
            .is_err(),
            "N19 add"
        );
        assert!(
            ensure_vault_registration(
                &mut one,
                b.id.clone(),
                refused.vaults[1].path.clone(),
                b.room.clone()
            )
            .is_err(),
            "N19 ensure"
        );
        refused = config.clone();
        refused.vaults[1].id = "VaultTwo_0123456789abc".into();
        refused.vaults[1].room = a.room.clone();
        assert!(load(&refused).is_err(), "N21");
        refused = config.clone();
        refused.auth.openclast.as_mut().unwrap().tenant_id = "tenant_short".into();
        assert!(load(&refused).is_err(), "N27");
        config
            .auth
            .openclast
            .as_mut()
            .unwrap()
            .legacy_identity_accept_until = None;
        assert!(load(&config).is_err(), "N29 load");
        config.vaults.pop();
        assert!(load(&config).is_ok());
        assert!(
            add_vault(&mut config, b.id.clone(), b.path.clone(), b.room.clone()).is_err(),
            "N29 add"
        );
        assert!(
            ensure_vault_registration(&mut config, b.id, b.path, b.room).is_err(),
            "N29 ensure"
        );
        assert_eq!(config.vaults, [a]);
    }

    fn holder_config(extra: &str) -> std::result::Result<Config, String> {
        let source = format!("[server]\nprofile = \"openclast\"\n\n[holder]\n{extra}");
        toml::from_str::<Config>(&source).map_err(|error| error.to_string())
    }

    fn complete_holder(root: &Path) -> String {
        let state_dir = toml::Value::try_from(root.join("holder")).unwrap();
        format!(
            "profile = \"openclast\"\nissuer = \"https://login.example.test/api/lifecycle/v1\"\nstate_dir = {state_dir}\n"
        )
    }

    #[test]
    fn holder_section_is_optional_and_absent_by_default() {
        let config = toml::from_str::<Config>("version = 1\n").unwrap();
        assert_eq!(config.holder, None);
        let encoded = toml::to_string_pretty(&config).unwrap();
        assert!(!encoded.contains("holder"));
    }

    #[test]
    fn partial_or_unknown_holder_configuration_is_refused_at_parse() {
        let temporary = tempfile::tempdir().unwrap();
        let complete = complete_holder(temporary.path());
        for missing in ["profile", "issuer", "state_dir"] {
            let partial: String = complete
                .lines()
                .filter(|line| !line.starts_with(missing))
                .map(|line| format!("{line}\n"))
                .collect();
            let error = holder_config(&partial).unwrap_err();
            assert!(
                error.contains(&format!("missing field `{missing}`")),
                "{error}"
            );
        }
        for removed_or_unknown in [
            "key",
            "status_interval_seconds",
            "report_interval_seconds",
            "purge_interval_seconds",
        ] {
            let error =
                holder_config(&format!("{complete}{removed_or_unknown} = 1\n")).unwrap_err();
            assert!(
                error.contains(&format!("unknown field `{removed_or_unknown}`")),
                "{error}"
            );
        }
        let unknown_profile = complete.replace("profile = \"openclast\"", "profile = \"other\"");
        let error = holder_config(&unknown_profile).unwrap_err();
        assert!(error.contains("unknown variant"), "{error}");
        assert!(holder_config(&complete).is_ok());
    }

    #[test]
    fn holder_section_validation_names_the_offending_field() {
        let temporary = tempfile::tempdir().unwrap();
        let complete = complete_holder(temporary.path());
        let config_path = temporary.path().join("config.toml");
        let path = config_path.as_path();
        let check = |extra: &str, profile: HostProfile| {
            let mut config = holder_config(&format!("{complete}{extra}")).unwrap();
            config.server.profile = profile;
            validate_holder(path, &config).map_err(|error| error.to_string())
        };
        assert_eq!(check("", HostProfile::OpenClast), Ok(()));
        assert!(
            check("", HostProfile::Desktop)
                .unwrap_err()
                .contains("requires server.profile")
        );
        for (extra, field) in [
            ("page_size = 0\n", "holder.page_size"),
            ("page_size = 1001\n", "holder.page_size"),
            (
                "clock_skew_allowance_seconds = 31\n",
                "holder.clock_skew_allowance_seconds",
            ),
            (
                "max_stale_serve_seconds = 86401\n",
                "holder.max_stale_serve_seconds",
            ),
        ] {
            let error = check(extra, HostProfile::OpenClast).unwrap_err();
            assert!(error.contains(field), "{extra}: {error}");
        }
        let mut config = holder_config(&complete).unwrap();
        config.holder.as_mut().unwrap().state_dir = PathBuf::from("relative/holder");
        assert!(
            validate_holder(path, &config)
                .unwrap_err()
                .to_string()
                .contains("must be absolute")
        );
        let vault = temporary.path().join("vault");
        config.holder.as_mut().unwrap().state_dir = vault.join("holder");
        config.vaults.push(VaultRegistration {
            snapshot_id: None,
            id: "notes".into(),
            path: vault,
            room: Some("room".into()),
        });
        assert!(
            validate_holder(path, &config)
                .unwrap_err()
                .to_string()
                .contains("must not overlap")
        );
    }

    #[test]
    fn holder_issuer_requires_https_except_for_loopback_ip_literals() {
        for good in [
            "https://login.example.test/api/lifecycle/v1",
            "https://login.example.test:8443/api/lifecycle/v1",
            "http://127.0.0.1:4000/api/lifecycle/v1",
            "http://127.255.0.42/api/lifecycle/v1",
            "http://[::1]/api/lifecycle/v1",
            "http://[::1]:4000/api/lifecycle/v1",
        ] {
            assert_eq!(validate_holder_issuer(good), Ok(()), "{good}");
        }
        for bad in [
            "http://localhost/api/lifecycle/v1",
            "http://login.example.test/api/lifecycle/v1",
            "http://10.0.0.1/api/lifecycle/v1",
            "http://[::2]/api/lifecycle/v1",
            "http://::1/api/lifecycle/v1",
            "http://[::1]unexpected/api/lifecycle/v1",
            "http://127.0.0.1:65536/api/lifecycle/v1",
            "ftp://login.example.test/x",
            "https://login.example.test",
            "https://login.example.test/api/lifecycle/v1/",
            "https://user@login.example.test/api",
            "https://login.example.test/api?x=1",
            "https://login.example.test/api#frag",
            "https://login.example.test/api lifecycle",
        ] {
            assert!(validate_holder_issuer(bad).is_err(), "{bad}");
        }
    }
}
