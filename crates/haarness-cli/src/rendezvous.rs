//! The private files a local workspace daemon leaves for the processes that
//! look for it: its own clients, and Chrome's bridge (`browser native-host`),
//! which must find every daemon without starting or locking any of them.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RuntimeDiscovery {
    pub(crate) url: String,
    pub(crate) token: String,
    pub(crate) pid: u32,
    /// Added after the first discovery format shipped. The live `/v1/info`
    /// response remains authoritative for legacy files.
    #[serde(default)]
    pub(crate) workspace: Option<String>,
    /// Identity of the executable that owns this local daemon. Package
    /// versions do not change between development rebuilds, so version/schema
    /// checks alone can otherwise attach a new client to stale server code.
    #[serde(default)]
    pub(crate) executable_fingerprint: Option<String>,
    #[serde(default)]
    pub(crate) access_fingerprint: Option<String>,
    /// The OTLP traces endpoint the daemon exports to, if any. A client reusing
    /// the daemon with different OTEL settings is told its spans go elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) trace_endpoint: Option<String>,
}

pub(crate) fn read_runtime_file(path: &Path) -> Result<RuntimeDiscovery> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

pub(crate) fn comparable_workspace(workspace: &Path) -> PathBuf {
    workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
}

/// `<path>:<dev>:<inode>:<length>:<mtime ns>` of an executable (no device or
/// inode outside Unix). A rebuild or an in-place upgrade changes it even when
/// the version does not.
pub(crate) fn executable_fingerprint(path: &Path) -> Result<String> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let metadata = fs::metadata(&path)
        .with_context(|| format!("cannot inspect Haarness executable {}", path.display()))?;
    let modified = metadata
        .modified()
        .context("Haarness executable modification time is unavailable")?
        .duration_since(UNIX_EPOCH)
        .context("Haarness executable modification time predates the Unix epoch")?
        .as_nanos();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(format!(
            "{}:{}:{}:{}:{}",
            path.display(),
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            modified
        ))
    }
    #[cfg(not(unix))]
    {
        Ok(format!(
            "{}:{}:{}",
            path.display(),
            metadata.len(),
            modified
        ))
    }
}

pub(crate) fn write_private_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    let mut file = options.open(&temporary)?;
    set_private(&file)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}
#[cfg(unix)]
pub(crate) fn set_private(file: &fs::File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}
#[cfg(not(unix))]
pub(crate) fn set_private(_file: &fs::File) -> Result<()> {
    Ok(())
}
