use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use fs2::FileExt;
use tempfile::NamedTempFile;

use crate::error::{Error, Result, io_error};
use crate::model::{Config, HostProfile, IndexFreshnessBasis, MAX_FILE_BYTES, VaultRegistration};

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
        message: error.to_string(),
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
        if !vault.path.is_absolute() {
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
    validate_room_registrations(path, config)
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
            id: "VaultOne_0123456789abc".into(),
            path: root("a"),
            room: Some("RoomAaaa_0123456789abc".into()),
        };
        let b = VaultRegistration {
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
}
