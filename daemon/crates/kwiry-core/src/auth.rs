use std::fs;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use subtle::ConstantTimeEq;
use tempfile::NamedTempFile;

use crate::error::{Error, Result, io_error};
use crate::model::{HostProfile, ResourceKey};

const TOKEN_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Search,
    Admin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub profile: HostProfile,
    pub subject: String,
    pub actor: String,
    pub scopes: Vec<Scope>,
    pub resources: Vec<ResourceKey>,
    pub jti: Option<String>,
    pub policy_revision: Option<String>,
    pub subject_revision: Option<String>,
    pub max_limit: usize,
}

impl Principal {
    pub fn desktop() -> Self {
        Self {
            profile: HostProfile::Desktop,
            subject: "desktop-user".to_owned(),
            actor: "kwiry-desktop".to_owned(),
            scopes: vec![Scope::Search, Scope::Admin],
            resources: Vec::new(),
            jti: None,
            policy_revision: None,
            subject_revision: None,
            max_limit: 100,
        }
    }

    pub fn has_scope(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }
}

pub fn load_or_create_token(path: &Path) -> Result<String> {
    if path.exists() {
        set_owner_only(path)?;
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

#[cfg(windows)]
fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SetFileSecurityW,
    };

    let descriptor_text: Vec<u16> = "D:P(A;;FA;;;OW)\0".encode_utf16().collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: The SDDL string is NUL-terminated and `descriptor` receives a LocalAlloc allocation.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_text.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io_error(path, std::io::Error::last_os_error()));
    }

    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: Both pointers remain valid for the duration of this call.
    let applied = unsafe {
        SetFileSecurityW(
            path_wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    let error = (applied == 0).then(std::io::Error::last_os_error);
    // SAFETY: `descriptor` was allocated by the conversion function above.
    unsafe {
        LocalFree(descriptor);
    }

    match error {
        Some(error) => Err(io_error(path, error)),
        None => Ok(()),
    }
}

#[cfg(all(not(unix), not(windows)))]
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

    #[cfg(unix)]
    #[test]
    fn existing_token_permissions_are_repaired() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempdir().unwrap();
        let path = temporary.path().join("token");
        fs::write(&path, "existing-token\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(load_or_create_token(&path).unwrap(), "existing-token");
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(windows)]
    #[test]
    fn token_file_has_a_protected_owner_only_dacl() {
        use std::os::windows::ffi::OsStrExt;
        use std::ptr;

        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, GetFileSecurityW, PSECURITY_DESCRIPTOR,
        };

        let temporary = tempdir().unwrap();
        let path = temporary.path().join("token");
        fs::write(&path, "existing-token\n").unwrap();
        assert_eq!(load_or_create_token(&path).unwrap(), "existing-token");
        let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();

        let mut needed = 0_u32;
        // SAFETY: This first call intentionally queries the required buffer size.
        unsafe {
            GetFileSecurityW(
                path_wide.as_ptr(),
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                0,
                &mut needed,
            );
        }
        assert!(needed > 0);
        let mut descriptor = vec![0_u8; needed as usize];
        // SAFETY: The buffer has the size returned by `GetFileSecurityW`.
        let loaded = unsafe {
            GetFileSecurityW(
                path_wide.as_ptr(),
                DACL_SECURITY_INFORMATION,
                descriptor.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        };
        assert_ne!(loaded, 0);

        let mut text = ptr::null_mut();
        let mut text_length = 0_u32;
        // SAFETY: The descriptor buffer is initialized by `GetFileSecurityW`.
        let converted = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor.as_mut_ptr().cast::<core::ffi::c_void>() as PSECURITY_DESCRIPTOR,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                &mut text_length,
            )
        };
        assert_ne!(converted, 0);
        // SAFETY: The conversion call returned a UTF-16 allocation of `text_length` units.
        let sddl = unsafe {
            String::from_utf16_lossy(std::slice::from_raw_parts(text, text_length as usize))
        };
        // SAFETY: `text` was allocated by the conversion function.
        unsafe {
            LocalFree(text.cast());
        }

        assert!(sddl.starts_with("D:P"), "unexpected DACL: {sddl}");
        assert!(sddl.contains("(A;;FA;;;OW)"), "unexpected DACL: {sddl}");
    }
}
