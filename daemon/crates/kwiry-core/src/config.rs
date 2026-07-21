use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use fs2::FileExt;
use tempfile::NamedTempFile;

use crate::error::{Error, Result, io_error};
use crate::model::{Config, VaultRegistration};

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
    let config = load_config_unlocked(path)?;
    validate_config(path, &config)?;
    Ok(config)
}

pub fn save_config(path: &Path, config: &Config) -> Result<()> {
    let lock_path = path.with_extension("lock");
    let _lock = acquire_config_lock(&lock_path)?;
    validate_config(path, config)?;
    save_config_unlocked(path, config)
}

pub fn update_config<T>(path: &Path, update: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    let lock_path = path.with_extension("lock");
    let _lock = acquire_config_lock(&lock_path)?;
    let mut config = load_config_unlocked(path)?;
    validate_config(path, &config)?;
    let result = update(&mut config)?;
    validate_config(path, &config)?;
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
    let parent = path.parent().ok_or_else(|| Error::InvalidConfig {
        path: path.to_path_buf(),
        message: "configuration path has no parent".to_owned(),
    })?;
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;

    let encoded = toml::to_string_pretty(config).map_err(|error| Error::InvalidConfig {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| io_error(parent, error))?;
    temporary
        .write_all(encoded.as_bytes())
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

fn validate_config(path: &Path, config: &Config) -> Result<()> {
    if config.server.bind.trim().is_empty() {
        return Err(Error::InvalidConfig {
            path: path.to_path_buf(),
            message: "server bind must not be empty".to_owned(),
        });
    }

    let mut ids = BTreeSet::new();
    for vault in &config.vaults {
        if vault.id.trim().is_empty() {
            return Err(Error::InvalidConfig {
                path: path.to_path_buf(),
                message: "vault IDs must not be empty".to_owned(),
            });
        }
        if !ids.insert(vault.id.as_str()) {
            return Err(Error::InvalidConfig {
                path: path.to_path_buf(),
                message: format!("duplicate vault ID: {}", vault.id),
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
    if config.vaults.iter().any(|vault| vault.id == id) {
        return Err(Error::DuplicateVaultId(id));
    }

    if id.trim().is_empty() || !path.is_absolute() || !path.is_dir() {
        return Err(Error::InvalidVaultPath(path));
    }
    if room.as_deref().is_some_and(|room| room.trim().is_empty()) {
        return Err(Error::InvalidConfig {
            path,
            message: "room must not be empty".to_owned(),
        });
    }

    let canonical_path = fs::canonicalize(&path).map_err(|_| Error::InvalidVaultPath(path))?;
    let registration = VaultRegistration {
        id,
        path: canonical_path,
        room,
    };
    config.vaults.push(registration.clone());
    config.vaults.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(registration)
}

pub fn ensure_vault_registration(
    config: &mut Config,
    id: String,
    path: PathBuf,
    room: Option<String>,
) -> Result<(VaultRegistration, VaultRegistrationDisposition)> {
    if id.trim().is_empty() || !path.is_absolute() || !path.is_dir() {
        return Err(Error::InvalidVaultPath(path));
    }
    if room.as_deref().is_some_and(|room| room.trim().is_empty()) {
        return Err(Error::InvalidConfig {
            path,
            message: "room must not be empty".to_owned(),
        });
    }

    let canonical_path = fs::canonicalize(&path).map_err(|_| Error::InvalidVaultPath(path))?;
    if let Some(existing) = config.vaults.iter().find(|vault| vault.id == id) {
        if existing.path == canonical_path && existing.room == room {
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
    config.vaults.push(registration.clone());
    config.vaults.sort_by(|left, right| left.id.cmp(&right.id));
    Ok((registration, VaultRegistrationDisposition::Added))
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
        assert_eq!(loaded.server.bind, crate::model::DEFAULT_BIND);
        assert_eq!(loaded.auth.token_file, None);
        assert!(!loaded.semantic.enabled);
        assert_eq!(loaded.vaults[0].id, "cynario");
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
}
