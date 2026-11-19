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
    /// The release build the daemon is (`HAARNESS_BUILD_ID`), whichever copy
    /// of it the daemon was started from; none for any other build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) release_build: Option<String>,
    #[serde(default)]
    pub(crate) access_fingerprint: Option<String>,
    /// The OTLP traces endpoint the daemon exports to, if any. A client reusing
    /// the daemon with different OTEL settings is told its spans go elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) trace_endpoint: Option<String>,
    /// The system configuration folder the daemon reads, if any. A client started
    /// with another `HAARNESS_SYSTEM_CONFIG_DIR` is told it is not applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) system_config_dir: Option<String>,
    /// When the daemon's process started, as
    /// [`haarness_core::process_groups::process_start`] reads it. With `pid`
    /// it names that one process, never a later program the system gives the
    /// same pid. None in records written before it was kept, and where the
    /// system cannot say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) started: Option<String>,
}

impl RuntimeDiscovery {
    /// Whether process `pid` is still the daemon that wrote this record, and
    /// not a program that was given its pid after the daemon ended (a crash,
    /// a restart). A record that says when the daemon started is matched on
    /// that; one from before records said so, on the name of the executable
    /// it records (`haarness` when it records none). A process that cannot
    /// be read is not taken for it.
    #[cfg(unix)]
    pub(crate) fn still_its_server(&self) -> bool {
        use haarness_core::process_groups::{process_ended, process_start};
        if process_ended(self.pid) {
            return false;
        }
        match &self.started {
            Some(started) => process_start(self.pid).as_ref() == Some(started),
            None => running_executable_name(self.pid)
                .is_some_and(|name| same_executable_name(&name, &self.recorded_executable_name())),
        }
    }

    /// The file name of the executable the daemon ran, from its fingerprint
    /// (`<path>:<dev>:<inode>:<length>:<mtime ns>`).
    #[cfg(unix)]
    fn recorded_executable_name(&self) -> String {
        self.executable_fingerprint
            .as_deref()
            .and_then(|fingerprint| fingerprint.rsplitn(5, ':').nth(4))
            .and_then(|path| Path::new(path).file_name())
            .map_or_else(
                || "haarness".to_owned(),
                |name| name.to_string_lossy().into_owned(),
            )
    }
}

/// The file name of the executable process `pid` runs, as the kernel keeps
/// it (not the `argv[0]` the process was given). Linux: `/proc/<pid>/exe`,
/// which an update that replaced the file marks ` (deleted)`.
#[cfg(target_os = "linux")]
fn running_executable_name(pid: u32) -> Option<String> {
    let target = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let target = target.to_string_lossy();
    let target = target.strip_suffix(" (deleted)").unwrap_or(&target);
    Some(
        Path::new(target)
            .file_name()?
            .to_string_lossy()
            .into_owned(),
    )
}

/// Elsewhere on Unix: `ps`'s `ucomm`, the name the kernel took from the
/// executable's file, cut to its first 16 bytes.
#[cfg(all(unix, not(target_os = "linux")))]
fn running_executable_name(pid: u32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-o", "ucomm=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !name.is_empty()).then_some(name)
}

/// Whether the kernel's `name` for a process is the executable name
/// `recorded`, which macOS keeps only the first 16 bytes of.
#[cfg(unix)]
fn same_executable_name(name: &str, recorded: &str) -> bool {
    const KEPT: usize = 16;
    name == recorded
        || (cfg!(not(target_os = "linux"))
            && name.len() == KEPT
            && recorded.len() > KEPT
            && recorded.starts_with(name))
}

pub(crate) fn read_runtime_file(path: &Path) -> Result<RuntimeDiscovery> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

pub(crate) fn comparable_workspace(workspace: &Path) -> PathBuf {
    dunce::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf())
}

/// `<path>:<dev>:<inode>:<length>:<mtime ns>` of an executable (no device or
/// inode outside Unix). A rebuild or an in-place upgrade changes it even when
/// the version does not.
pub(crate) fn executable_fingerprint(path: &Path) -> Result<String> {
    let path = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
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
#[allow(clippy::unnecessary_wraps)] // same signature as on Unix, where it can fail
pub(crate) fn set_private(_file: &fs::File) -> Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn record(pid: u32, started: Option<String>, fingerprint: Option<&str>) -> RuntimeDiscovery {
        RuntimeDiscovery {
            url: "http://127.0.0.1:9".into(),
            token: "token".into(),
            pid,
            workspace: None,
            executable_fingerprint: fingerprint.map(str::to_owned),
            release_build: None,
            access_fingerprint: None,
            trace_endpoint: None,
            system_config_dir: None,
            started,
        }
    }

    #[test]
    fn the_recorded_executable_is_named_from_its_fingerprint() {
        let named = |fingerprint| record(1, None, fingerprint).recorded_executable_name();
        assert_eq!(
            named(Some("/usr/local/bin/haarness:16777232:42:9:17")),
            "haarness"
        );
        // A colon in the folder's name stays in the path.
        assert_eq!(
            named(Some("/Users/a:b/target/debug/haarness:1:2:3:4")),
            "haarness"
        );
        assert_eq!(
            named(Some("/opt/tools/haarness-dev:1:2:3:4")),
            "haarness-dev"
        );
        // No fingerprint, or one that is not a path: the name Haarness ships as.
        assert_eq!(named(None), "haarness");
        assert_eq!(named(Some("other-build")), "haarness");
    }

    #[test]
    fn a_name_the_kernel_cut_short_still_matches_on_macos_only() {
        assert!(same_executable_name("haarness", "haarness"));
        assert!(!same_executable_name("haarness", "haarness-dev"));
        assert!(!same_executable_name("sleep", "haarness"));
        let long = "haarness-nightly-arm64";
        assert_eq!(
            same_executable_name(&long[..16], long),
            cfg!(not(target_os = "linux"))
        );
        assert!(!same_executable_name(&long[..10], long));
    }

    /// The identity is read the way a server records it: this process, by
    /// its start time, is itself, and is not with another start time.
    #[test]
    fn a_record_names_its_process_only_when_its_start_time_matches() {
        let pid = std::process::id();
        let started = haarness_core::process_groups::process_start(pid);
        assert!(started.is_some(), "this system says when processes start");
        assert!(record(pid, started.clone(), None).still_its_server());
        assert!(!record(pid, Some("another time".into()), None).still_its_server());
        // An older record: this test binary is not named `haarness`.
        assert!(!record(pid, None, None).still_its_server());
        let this = current_executable_name();
        let fingerprint = format!("/somewhere/{this}:1:2:3:4");
        assert!(record(pid, None, Some(&fingerprint)).still_its_server());
    }

    fn current_executable_name() -> String {
        std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }
}
