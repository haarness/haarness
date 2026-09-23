//! The credentials file: `<config_dir>/credentials.json`, mode 0600 (DESIGN.md §6.3).
//!
//! Format (exactly four keys, `camelCase`): `{"apiUrl","accessToken","accessExpiresAt","refreshToken"}`.
//! Writes follow the convention the Haarness runtime already uses for `auth.json`
//! (`docs/reference/understanding/clients-and-sdk.md` §5.1): a private temp file in the same directory,
//! `sync_all`, then an atomic `rename` over the target. **No keychain.**

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Mode of `credentials.json` (owner read/write only).
pub const FILE_MODE: u32 = 0o600;
/// Mode of the config directory when this process creates it.
pub const DIR_MODE: u32 = 0o700;

/// `{"apiUrl","accessToken","accessExpiresAt","refreshToken"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    pub api_url: String,
    pub access_token: String,
    pub access_expires_at: DateTime<Utc>,
    pub refresh_token: String,
}

impl Credentials {
    /// True when the access token expires within `skew` (the caller should refresh first).
    pub fn access_expired(&self, now: DateTime<Utc>, skew: chrono::Duration) -> bool {
        self.access_expires_at <= now + skew
    }
}

/// Reads the file; `Ok(None)` when it does not exist.
pub fn load(path: &Path) -> Result<Option<Credentials>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).map_err(|error| {
            Error::Other(format!("{}: {error}", path.display()))
        })?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Io(error)),
    }
}

/// Writes the file atomically (temp file + rename) with mode 0600, creating the config dir (0700).
pub fn save(path: &Path, credentials: &Credentials) -> Result<()> {
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    if !dir.exists() {
        fs::create_dir_all(&dir)?;
        set_mode(&dir, DIR_MODE)?;
    }

    let temp = dir.join(format!(
        "{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("credentials.json"),
        uuid::Uuid::now_v7().simple()
    ));
    let body = serde_json::to_vec_pretty(credentials)?;
    let write = || -> std::io::Result<()> {
        let mut file = private_file(&temp)?;
        file.write_all(&body)?;
        file.write_all(b"\n")?;
        file.sync_all()
    };
    if let Err(error) = write() {
        let _ = fs::remove_file(&temp);
        return Err(Error::Io(error));
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(Error::Io(error));
    }
    set_mode(path, FILE_MODE)?;
    Ok(())
}

/// Removes the file; a missing file is not an error.
pub fn delete(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::Io(error)),
    }
}

/// Creates `path` for writing, owner-only from the first byte (never a world-readable window).
#[cfg(unix)]
fn private_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    let _ = (path, mode);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Credentials {
        Credentials {
            api_url: "http://127.0.0.1:18080/".to_owned(),
            access_token: "hca_one".to_owned(),
            access_expires_at: DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
            refresh_token: "hcrt_one".to_owned(),
        }
    }

    #[test]
    fn expiry_uses_the_skew() {
        let credentials = sample();
        let now = credentials.access_expires_at - chrono::Duration::seconds(60);
        assert!(!credentials.access_expired(now, chrono::Duration::seconds(30)));
        assert!(credentials.access_expired(now, chrono::Duration::seconds(60)));
    }

    #[test]
    fn missing_file_loads_as_none() {
        let path = std::env::temp_dir().join(format!("hc-cli-absent-{}", uuid::Uuid::now_v7()));
        assert_eq!(load(&path).unwrap(), None);
        delete(&path).unwrap();
    }
}
