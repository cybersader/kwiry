use std::fs;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use subtle::ConstantTimeEq;
use tempfile::NamedTempFile;

use crate::error::{Error, Result, io_error};

const TOKEN_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Search,
    Admin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub scopes: Vec<Scope>,
    pub pinned_vault_ids: Vec<String>,
    pub pinned_rooms: Vec<String>,
}

impl Principal {
    pub fn desktop() -> Self {
        Self {
            scopes: vec![Scope::Search, Scope::Admin],
            pinned_vault_ids: Vec::new(),
            pinned_rooms: Vec::new(),
        }
    }

    pub fn has_scope(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }
}

pub fn load_or_create_token(path: &Path) -> Result<String> {
    if path.exists() {
        return load_token(path);
    }

    let mut bytes = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|error| Error::Auth(format!("could not generate bearer token: {error}")))?;
    let token = URL_SAFE_NO_PAD.encode(bytes);
    persist_token(path, &token)?;
    Ok(token)
}

pub fn load_token(path: &Path) -> Result<String> {
    let token = fs::read_to_string(path).map_err(|error| io_error(path, error))?;
    let token = token.trim().to_owned();
    if token.is_empty() {
        return Err(Error::Auth(format!(
            "bearer token file is empty: {}",
            path.display()
        )));
    }
    Ok(token)
}

pub fn token_matches(expected: &str, supplied: &str) -> bool {
    expected.len() == supplied.len() && bool::from(expected.as_bytes().ct_eq(supplied.as_bytes()))
}

fn persist_token(path: &Path, token: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::Auth(format!(
            "bearer token path has no parent: {}",
            path.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;

    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| io_error(parent, error))?;
    use std::io::Write;
    temporary
        .write_all(token.as_bytes())
        .map_err(|error| io_error(temporary.path(), error))?;
    temporary
        .write_all(b"\n")
        .map_err(|error| io_error(temporary.path(), error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| io_error(temporary.path(), error))?;
    set_owner_only(temporary.path())?;
    temporary
        .persist(path)
        .map_err(|error| io_error(path, error.error))?;
    sync_parent(parent)?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| io_error(path, error))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    fs::File::open(path)
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
    fn token_is_generated_once_and_compares_in_constant_time() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("token");

        let first = load_or_create_token(&path).unwrap();
        let second = load_or_create_token(&path).unwrap();

        assert_eq!(first, second);
        assert!(token_matches(&first, &second));
        assert!(!token_matches(&first, "wrong"));
    }

    #[cfg(unix)]
    #[test]
    fn token_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempdir().unwrap();
        let path = temporary.path().join("token");
        load_or_create_token(&path).unwrap();

        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
