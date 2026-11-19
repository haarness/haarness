//! The private files a local workspace daemon leaves for the processes that
//! look for it: its own clients, and Chrome's bridge (`browser native-host`),
//! which must find every daemon without starting or locking any of them.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
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

/// What the process a record names is, as far as this system can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))] // only `Unknown` there
pub(crate) enum RecordedProcess {
    /// Still the daemon that wrote the record.
    Server,
    /// Gone, or a program the system gave the daemon's pid after it ended.
    Other,
    /// Running, but this system cannot tell what it is.
    Unknown,
}

/// How a process is read: [`SYSTEM_PROBE`], or a test's stand-ins.
#[cfg(unix)]
pub(crate) struct ProcessProbe {
    /// Whether no process has the pid.
    pub(crate) ended: fn(u32) -> bool,
    /// When it started, as a record keeps it.
    pub(crate) start: fn(u32) -> Option<String>,
    /// The file name of the executable it runs.
    pub(crate) name: fn(u32) -> Option<String>,
    /// When it started, in whole seconds since the Unix epoch.
    pub(crate) started_at: fn(u32) -> Option<i64>,
}

#[cfg(unix)]
pub(crate) const SYSTEM_PROBE: ProcessProbe = ProcessProbe {
    ended: haarness_core::process_groups::process_ended,
    start: haarness_core::process_groups::process_start,
    name: running_executable_name,
    started_at,
};

impl RuntimeDiscovery {
    /// Whether process `pid` is still the daemon that wrote this record, read
    /// the system's way; `written` is when the record file was last written.
    #[cfg(unix)]
    pub(crate) fn recorded_process(&self, written: Option<SystemTime>) -> RecordedProcess {
        self.recorded_process_with(written, &SYSTEM_PROBE)
    }

    /// Nothing here can tell a process apart from another that was given its
    /// pid, so nothing is ever taken for the daemon.
    #[cfg(not(unix))]
    #[allow(clippy::unused_self)] // same signature as on Unix
    pub(crate) fn recorded_process(&self, _written: Option<SystemTime>) -> RecordedProcess {
        RecordedProcess::Unknown
    }

    /// Whether process `pid` is still the daemon that wrote this record, and
    /// not a program given its pid after the daemon ended (a crash, a
    /// restart). A record that says when the daemon started is matched on
    /// that. One from before records said so (0.1.23 and older) is matched on
    /// the name of the executable it records (`haarness` when it records
    /// none), and on the process having started no later than the record was
    /// written: whatever was given the pid afterwards started later.
    #[cfg(unix)]
    pub(crate) fn recorded_process_with(
        &self,
        written: Option<SystemTime>,
        probe: &ProcessProbe,
    ) -> RecordedProcess {
        if (probe.ended)(self.pid) {
            return RecordedProcess::Other;
        }
        // It may have ended while it was being read.
        let unread = || {
            if (probe.ended)(self.pid) {
                RecordedProcess::Other
            } else {
                RecordedProcess::Unknown
            }
        };
        if let Some(started) = &self.started {
            return match (probe.start)(self.pid) {
                Some(start) if start == *started => RecordedProcess::Server,
                Some(_) => RecordedProcess::Other,
                None => unread(),
            };
        }
        let Some(name) = (probe.name)(self.pid) else {
            return unread();
        };
        if !same_executable_name(&name, &self.recorded_executable_name()) {
            return RecordedProcess::Other;
        }
        let written = written
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .and_then(|since| i64::try_from(since.as_secs()).ok());
        let (Some(written), Some(began)) = (written, (probe.started_at)(self.pid)) else {
            return unread();
        };
        if began <= written {
            RecordedProcess::Server
        } else {
            RecordedProcess::Other
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
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "ucomm=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !name.is_empty()).then_some(name)
}

/// When process `pid` started, in seconds since the Unix epoch. Linux: the
/// boot time (`btime` in `/proc/stat`) plus its start in clock ticks since
/// boot.
#[cfg(target_os = "linux")]
fn started_at(pid: u32) -> Option<i64> {
    use nix::unistd::{SysconfVar, sysconf};
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is in parentheses and may hold spaces or `)`.
    let ticks: i64 = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()?;
    let boot: i64 = fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    #[allow(clippy::useless_conversion)] // `c_long` is narrower on 32-bit Linux
    let per_second = i64::from(sysconf(SysconfVar::CLK_TCK).ok()??);
    (per_second > 0).then(|| boot + ticks / per_second)
}

/// Elsewhere on Unix: the start time `ps` gives (to the second, in the C
/// locale and UTC; see [`haarness_core::process_groups::process_start`]).
#[cfg(all(unix, not(target_os = "linux")))]
fn started_at(pid: u32) -> Option<i64> {
    parse_ps_start(&haarness_core::process_groups::process_start(pid)?)
}

/// `Thu Nov 19 03:27:47 2026`, in UTC, as seconds since the Unix epoch.
#[cfg(unix)]
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn parse_ps_start(text: &str) -> Option<i64> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    chrono::NaiveDateTime::parse_from_str(&text, "%a %b %e %H:%M:%S %Y")
        .ok()
        .map(|time| time.and_utc().timestamp())
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
    /// its start time, is itself, and is not with another start time. An
    /// older record names it by its executable, written after it started.
    #[test]
    fn a_record_names_its_process_only_when_its_start_time_matches() {
        use RecordedProcess::{Other, Server};
        let pid = std::process::id();
        let started = haarness_core::process_groups::process_start(pid);
        assert!(started.is_some(), "this system says when processes start");
        let now = Some(SystemTime::now());
        assert_eq!(
            record(pid, started.clone(), None).recorded_process(now),
            Server
        );
        let elsewhen = Some("Sun Jun  3 08:15:00 2012".to_owned());
        assert_eq!(record(pid, elsewhen, None).recorded_process(now), Other);
        // An older record: this test binary is not named `haarness`.
        assert_eq!(record(pid, None, None).recorded_process(now), Other);
        let this = current_executable_name();
        let fingerprint = format!("/somewhere/{this}:1:2:3:4");
        let older = record(pid, None, Some(&fingerprint));
        assert_eq!(older.recorded_process(now), Server);
        // Written before this process started: it is not the one written of.
        let long_ago = Some(UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000));
        assert_eq!(older.recorded_process(long_ago), Other);
    }

    /// A process that runs but cannot be read is never taken for the
    /// server, nor said to be another program; one that ended is gone.
    #[test]
    fn a_process_that_cannot_be_read_is_unknown() {
        use RecordedProcess::{Other, Server, Unknown};
        let running =
            |name: fn(u32) -> Option<String>, started_at: fn(u32) -> Option<i64>| ProcessProbe {
                ended: |_| false,
                start: |_| None,
                name,
                started_at,
            };
        let written = Some(UNIX_EPOCH + std::time::Duration::from_secs(2_000));
        let newer = record(77, Some("its start".into()), None);
        let older = record(77, None, None);
        let unread = running(|_| None, |_| None);
        assert_eq!(newer.recorded_process_with(written, &unread), Unknown);
        assert_eq!(older.recorded_process_with(written, &unread), Unknown);
        // Named right, but when it started cannot be read.
        let named = running(|_| Some("haarness".into()), |_| None);
        assert_eq!(older.recorded_process_with(written, &named), Unknown);
        assert_eq!(
            older.recorded_process_with(
                None,
                &running(|_| Some("haarness".into()), |_| Some(1_000))
            ),
            Unknown
        );
        // Started before the record was written, or after it.
        let before = running(|_| Some("haarness".into()), |_| Some(1_999));
        let after = running(|_| Some("haarness".into()), |_| Some(2_001));
        assert_eq!(older.recorded_process_with(written, &before), Server);
        assert_eq!(older.recorded_process_with(written, &after), Other);
        let ended = ProcessProbe {
            ended: |_| true,
            ..unread
        };
        assert_eq!(newer.recorded_process_with(written, &ended), Other);
    }

    #[test]
    fn the_start_time_ps_prints_is_read_as_utc() {
        assert_eq!(
            parse_ps_start("Thu Nov 19 03:27:47 2026"),
            Some(1_795_058_867)
        );
        // `ps` pads a one-digit day; a record keeps the words single-spaced.
        let padded = parse_ps_start("Sun Jun  3 08:15:00 2012");
        assert_eq!(padded, Some(1_338_711_300));
        assert_eq!(parse_ps_start("Sun Jun 3 08:15:00 2012"), padded);
        assert_eq!(parse_ps_start("not a date"), None);
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
