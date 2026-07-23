use std::path::PathBuf;

use crate::auth::load_or_create_token;
use crate::config::{Paths, load_config, update_config};
use crate::error::{Error, Result};
use crate::model::{Config, HostProfile};

pub struct DesktopBootstrap {
    pub config: Config,
    pub token_path: PathBuf,
    token: String,
}

impl DesktopBootstrap {
    pub fn token(&self) -> &str {
        &self.token
    }
}

pub fn bootstrap_desktop(paths: &Paths) -> Result<DesktopBootstrap> {
    let config = load_config(&paths.config)?;
    if config.server.profile != HostProfile::Desktop {
        return Err(Error::Auth(
            "desktop bootstrap is unavailable for the openclast profile".to_owned(),
        ));
    }
    let config = if config.auth.token_file.is_none() {
        let default_token_path = paths.default_token_path().to_path_buf();
        update_config(&paths.config, |config| {
            if config.auth.token_file.is_none() {
                config.auth.token_file = Some(default_token_path);
            }
            Ok(config.clone())
        })?
    } else {
        config
    };
    let token_path = paths.token_path(&config);
    let token = load_or_create_token(&token_path)?;

    Ok(DesktopBootstrap {
        config,
        token_path,
        token,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::config::load_config;

    #[test]
    fn fresh_bootstrap_persists_an_absolute_token_path() {
        let temporary = tempdir().unwrap();
        let paths = Paths::resolve(
            Some(temporary.path().join("nested/config.toml")),
            Some(temporary.path().join("data")),
        )
        .unwrap();

        let bootstrap = bootstrap_desktop(&paths).unwrap();
        let persisted = load_config(&paths.config).unwrap();

        assert!(bootstrap.token_path.is_absolute());
        assert_eq!(
            persisted.auth.token_file.as_deref(),
            Some(bootstrap.token_path.as_path())
        );
        assert_eq!(bootstrap.token_path, paths.default_token_path());
        assert!(!bootstrap.token().is_empty());
        assert_eq!(
            std::fs::read_to_string(&bootstrap.token_path)
                .unwrap()
                .trim(),
            bootstrap.token()
        );
    }

    #[test]
    fn bootstrap_reuses_an_existing_token_and_path() {
        let temporary = tempdir().unwrap();
        let paths = Paths::resolve(
            Some(temporary.path().join("config.toml")),
            Some(temporary.path().join("data")),
        )
        .unwrap();
        let custom_token = temporary.path().join("secrets/desktop.token");
        update_config(&paths.config, |config| {
            config.auth.token_file = Some(custom_token.clone());
            Ok(())
        })
        .unwrap();

        let first = bootstrap_desktop(&paths).unwrap();
        let second = bootstrap_desktop(&paths).unwrap();

        assert_eq!(first.token_path, custom_token);
        assert_eq!(second.token_path, custom_token);
        assert_eq!(first.token(), second.token());
    }
}
