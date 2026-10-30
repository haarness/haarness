//! The process groups a workspace server starts, recorded so the next server
//! for its folder can stop the ones a crash left running.
//!
//! A server stops what it started when it stops: a shell command's group, a
//! workflow's, an extension host's, an MCP server's. Killed or crashed, it
//! stops nothing, and a command that writes nothing (a dev server holding
//! its port, a `sleep`, a watcher) runs on under launchd or init. So each
//! group is written to a small file beside the folder's server lock while it
//! runs, under the recording server's own pid and start time. The next
//! server, once it holds that lock, stops each group whose leader is still
//! the process that was started (same pid, same start time, read the same
//! way), and only when the recording server itself is gone; a pid the system
//! reused is never hit. A server that stops normally gives the record up
//! before it lets go of the lock, so the processes it is still stopping are
//! left to it. Unix only: elsewhere nothing is recorded.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;

/// The record of one server's groups.
struct Ledger {
    path: PathBuf,
    /// The recording server: its pid and start time.
    server: Option<(u32, String)>,
    /// Group id to its leader's start time, once read (see [`record_in`]).
    groups: Mutex<BTreeMap<i32, Option<String>>>,
    /// Given up (see [`Recording`]): nothing more is written.
    released: AtomicBool,
}

static LEDGER: OnceLock<Arc<Ledger>> = OnceLock::new();

impl Ledger {
    fn new(path: PathBuf, carried: BTreeMap<i32, String>) -> Self {
        let pid = std::process::id();
        Self {
            path,
            server: start_of(pid).map(|start| (pid, start)),
            groups: Mutex::new(
                carried
                    .into_iter()
                    .map(|(group, start)| (group, Some(start)))
                    .collect(),
            ),
            released: AtomicBool::new(false),
        }
    }

    fn write(&self, groups: &BTreeMap<i32, Option<String>>) {
        use std::fmt::Write as _;
        if self.released.load(Ordering::Acquire) {
            return;
        }
        let mut text = String::new();
        if let Some((pid, start)) = &self.server {
            let _ = writeln!(text, "server {pid} {start}");
        }
        for (group, start) in groups {
            if let Some(start) = start {
                let _ = writeln!(text, "{group} {start}");
            }
        }
        let partial = self
            .path
            .with_extension(format!("{}.partial", std::process::id()));
        if std::fs::write(&partial, text).is_ok() {
            let _ = std::fs::rename(&partial, &self.path);
        }
    }

    fn remove(&self, group: i32) {
        let mut groups = self.groups.lock();
        groups.remove(&group);
        self.write(&groups);
    }

    fn release(&self) {
        let _groups = self.groups.lock();
        if !self.released.swap(true, Ordering::AcqRel) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Held by a workspace server for as long as it holds its folder's lock;
/// dropped just before the lock goes, it gives the record up: what this
/// server is still stopping is left to it, never to the next server.
#[must_use = "the record is given up when this is dropped"]
pub struct Recording(bool);

impl Drop for Recording {
    fn drop(&mut self) {
        if self.0
            && let Some(ledger) = LEDGER.get()
        {
            ledger.release();
        }
    }
}

/// Run by a workspace server holding its folder's lock: stops the groups the
/// record at `path` names that a dead server left running, then records this
/// server's own groups there. Returns how many groups were stopped, and the
/// guard to drop just before the lock is released.
pub fn take_over(path: PathBuf) -> (usize, Recording) {
    if !cfg!(unix) {
        return (0, Recording(false));
    }
    let (stopped, carried) = match sweep(&path, probe) {
        Sweep::Done { stopped, carried } => (stopped, carried),
        // Its record stays its own: this server records nothing over it.
        Sweep::WriterRunning => return (0, Recording(false)),
    };
    let installed = LEDGER.set(Arc::new(Ledger::new(path, carried))).is_ok();
    if let Some(ledger) = LEDGER.get().filter(|_| installed) {
        ledger.write(&ledger.groups.lock());
    }
    (stopped, Recording(installed))
}

/// A started process group, in the record until dropped.
#[must_use = "the group leaves the record when this is dropped"]
pub struct Recorded(Option<(Arc<Ledger>, i32)>);

/// Records the process group `leader` leads, just started.
pub fn recorded(leader: Option<u32>) -> Recorded {
    let (Some(ledger), Some(pid)) = (LEDGER.get(), leader) else {
        return Recorded(None);
    };
    record_in(ledger, pid)
}

/// The group is in the record at once; its leader's start time is read on a
/// thread of its own (on macOS that is a `ps`, a few milliseconds), and the
/// entry is written once it is known. A leader gone already, or one this
/// system cannot date, is nothing to stop later and leaves the record again.
fn record_in(ledger: &Arc<Ledger>, pid: u32) -> Recorded {
    let Ok(group) = i32::try_from(pid) else {
        return Recorded(None);
    };
    ledger.groups.lock().insert(group, None);
    let reader = ledger.clone();
    let dated = std::thread::Builder::new()
        .name("haarness-process-start".into())
        .spawn(move || {
            let start = match probe(group) {
                Probe::Running { group: led, start } if led == group => Some(start),
                _ => None,
            };
            let mut groups = reader.groups.lock();
            // Gone from the record meanwhile: the group ended already.
            let Some(slot @ None) = groups.get_mut(&group) else {
                return;
            };
            match start {
                Some(start) => {
                    *slot = Some(start);
                    reader.write(&groups);
                }
                None => {
                    groups.remove(&group);
                }
            }
        });
    if dated.is_err() {
        ledger.groups.lock().remove(&group);
        return Recorded(None);
    }
    Recorded(Some((ledger.clone(), group)))
}

impl Drop for Recorded {
    fn drop(&mut self) {
        if let Some((ledger, group)) = &self.0 {
            ledger.remove(*group);
        }
    }
}

/// What a pid is now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Probe {
    Running {
        group: i32,
        start: String,
    },
    #[cfg_attr(not(unix), allow(dead_code))]
    Gone,
    /// This system could not say.
    Unknown,
}

/// What a sweep found.
#[derive(Debug, PartialEq, Eq)]
enum Sweep {
    /// The groups stopped, and those of a gone server left unchecked,
    /// carried into the next record.
    Done {
        stopped: usize,
        carried: BTreeMap<i32, String>,
    },
    /// The server that wrote the record still runs (its lock was bypassed,
    /// or its folder's cache removed): its groups are its own.
    WriterRunning,
}

/// Stops the record's groups a gone server left running. Nothing is lost
/// when the check cannot be made: the record is removed only once every
/// entry is settled. Groups of a writer that runs, or cannot be told to be
/// gone, are never taken over: a later crash would then stop its commands.
fn sweep(path: &Path, probe: impl Fn(i32) -> Probe) -> Sweep {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Sweep::Done {
            stopped: 0,
            carried: BTreeMap::new(),
        };
    };
    let (server, groups) = parse_record(&text);
    if let Some((pid, start)) = server
        && let Ok(pid) = i32::try_from(pid)
    {
        match probe(pid) {
            Probe::Running { start: now, .. } if now == start => {
                tracing::warn!(
                    pid,
                    "another Haarness server for this folder is still running; leaving its commands to it"
                );
                return Sweep::WriterRunning;
            }
            Probe::Unknown => {
                tracing::warn!(
                    pid,
                    "could not tell whether the server that recorded these commands is gone; leaving them"
                );
                let _ = std::fs::remove_file(path);
                return Sweep::Done {
                    stopped: 0,
                    carried: BTreeMap::new(),
                };
            }
            _ => {}
        }
    }
    let mut stopped = 0;
    let mut unchecked = BTreeMap::new();
    for (group, start) in groups {
        match probe(group) {
            Probe::Running {
                group: led,
                start: now,
            } if led == group && now == start => {
                if kill_group(group) {
                    stopped += 1;
                }
            }
            Probe::Unknown => {
                unchecked.insert(group, start);
            }
            _ => {}
        }
    }
    if unchecked.is_empty() {
        let _ = std::fs::remove_file(path);
    } else {
        tracing::warn!(
            groups = unchecked.len(),
            "could not tell whether commands an earlier server started still run; kept them on record"
        );
    }
    Sweep::Done {
        stopped,
        carried: unchecked,
    }
}

#[allow(clippy::type_complexity)]
fn parse_record(text: &str) -> (Option<(u32, String)>, BTreeMap<i32, String>) {
    let mut server = None;
    let mut groups = BTreeMap::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("server ") {
            if let Some((pid, start)) = rest.split_once(' ')
                && let Ok(pid) = pid.parse()
            {
                server = Some((pid, start.trim().to_owned()));
            }
        } else if let Some((group, start)) = line.split_once(' ')
            && let Ok(group) = group.parse::<i32>()
            && group > 1
            && !start.trim().is_empty()
        {
            groups.insert(group, start.trim().to_owned());
        }
    }
    (server, groups)
}

/// A process's start time as this system says it, the same way every time.
fn start_of(pid: u32) -> Option<String> {
    match probe(i32::try_from(pid).ok()?) {
        Probe::Running { start, .. } => Some(start),
        _ => None,
    }
}

/// Linux: `/proc/<pid>/stat` (its group and its start in clock ticks since
/// boot), with the boot's id, so a pid and tick count from before a restart
/// never match.
#[cfg(target_os = "linux")]
fn probe(pid: i32) -> Probe {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return if std::path::Path::new("/proc/self/stat").exists() {
            Probe::Gone
        } else {
            Probe::Unknown
        };
    };
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
    parse_proc_stat(&stat, boot.trim()).unwrap_or(Probe::Unknown)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_stat(stat: &str, boot: &str) -> Option<Probe> {
    // The command name is in parentheses and may hold spaces or `)`.
    let fields: Vec<&str> = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .collect();
    let group = fields.get(2)?.parse().ok()?;
    let ticks = fields.get(19)?;
    Some(Probe::Running {
        group,
        start: format!("{boot}:{ticks}"),
    })
}

/// Elsewhere on Unix: `ps`, which macOS always has: the group and the start
/// time it prints (to the second, in the C locale).
#[cfg(all(unix, not(target_os = "linux")))]
fn probe(pid: i32) -> Probe {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-o", "pgid=,lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
    else {
        return Probe::Unknown;
    };
    // `ps` exits 1 when no such process exists.
    let listing = String::from_utf8_lossy(&output.stdout);
    if listing.trim().is_empty() {
        return if output.status.code() == Some(1) {
            Probe::Gone
        } else {
            Probe::Unknown
        };
    }
    parse_ps_line(&listing).unwrap_or(Probe::Unknown)
}

#[cfg_attr(not(all(unix, not(target_os = "linux"))), allow(dead_code))]
fn parse_ps_line(line: &str) -> Option<Probe> {
    let mut fields = line.split_whitespace();
    let group = fields.next()?.parse().ok()?;
    let start = fields.collect::<Vec<_>>().join(" ");
    (!start.is_empty()).then_some(Probe::Running { group, start })
}

#[cfg(not(unix))]
fn probe(_pid: i32) -> Probe {
    Probe::Unknown
}

#[cfg(unix)]
fn kill_group(group: i32) -> bool {
    nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(group),
        nix::sys::signal::Signal::SIGKILL,
    )
    .is_ok()
}

#[cfg(not(unix))]
fn kill_group(_group: i32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_start_time_linux_and_ps_give() {
        let stat = "4242 (my (odd) cmd) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 1000 10";
        assert_eq!(
            parse_proc_stat(stat, "boot-1"),
            Some(Probe::Running {
                group: 4242,
                start: "boot-1:987654".into()
            })
        );
        assert_eq!(
            parse_ps_line(" 4242 Thu Oct 22 11:46:03 2026\n"),
            Some(Probe::Running {
                group: 4242,
                start: "Thu Oct 22 11:46:03 2026".into()
            })
        );
        assert_eq!(parse_ps_line("   \n"), None);
    }

    fn temporary_record(text: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("haarness-process-groups-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("process-groups");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn only_a_leader_that_started_when_it_was_recorded_is_stopped_once_its_server_is_gone() {
        let running = |group: i32, start: &str| Probe::Running {
            group,
            start: start.into(),
        };
        // 100 runs as recorded; pid 200 now belongs to another program; 300
        // now leads no group of its own; 400 is gone. Nothing real is
        // stopped: the pids are all above any this test could own.
        let record = "server 9 server-start\n100 a\n200 b\n300 c\n400 d\n1 e\nnot a line\n";
        let probe = |pid: i32| match pid {
            100 => running(100, "a"),
            200 => running(200, "other"),
            300 => running(299, "c"),
            _ => Probe::Gone,
        };
        let (_, groups) = parse_record(record);
        assert_eq!(
            groups.keys().copied().collect::<Vec<_>>(),
            [100, 200, 300, 400]
        );
        let stoppable = |probe: &dyn Fn(i32) -> Probe| {
            groups
                .iter()
                .filter(|(group, start)| {
                    matches!(probe(**group), Probe::Running { group: led, start: now } if led == **group && &now == *start)
                })
                .map(|(group, _)| *group)
                .collect::<Vec<_>>()
        };
        assert_eq!(stoppable(&probe), [100]);

        // The server that wrote it still runs (its lock was removed): none
        // of its groups are taken over, and its record stays its own.
        let path = temporary_record(record);
        let alive = |pid: i32| match pid {
            9 => running(9, "server-start"),
            other => probe(other),
        };
        assert_eq!(sweep(&path, alive), Sweep::WriterRunning);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), record);
        // Nor when it cannot be told whether that server is gone.
        let unsure = |pid: i32| match pid {
            9 => Probe::Unknown,
            other => probe(other),
        };
        assert_eq!(
            sweep(&path, unsure),
            Sweep::Done {
                stopped: 0,
                carried: BTreeMap::new()
            }
        );
        std::fs::write(&path, record).unwrap();

        // A check this system cannot make keeps the entry for the next
        // record instead of losing it.
        let unknown = |pid: i32| {
            if pid == 400 {
                Probe::Unknown
            } else {
                Probe::Gone
            }
        };
        assert_eq!(
            sweep(&path, unknown),
            Sweep::Done {
                stopped: 0,
                carried: BTreeMap::from([(400, "d".into())])
            }
        );
        assert!(
            path.exists(),
            "the record stays until every entry is settled"
        );
        assert_eq!(
            sweep(&path, |_| Probe::Gone),
            Sweep::Done {
                stopped: 0,
                carried: BTreeMap::new()
            }
        );
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Waits for the record to name `pid` with its start time, read on a
    /// thread of its own.
    fn dated(path: &Path, pid: u32) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .any(|line| line.starts_with(&format!("{pid} ")))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "{pid} was never dated"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// The start time is read off the caller's thread (on macOS a `ps`); a
    /// group that ends before it is known never reaches the record.
    #[cfg(unix)]
    #[test]
    fn a_group_is_dated_off_the_callers_thread_and_one_that_ended_first_is_not_written() {
        use std::os::unix::process::CommandExt as _;
        let path = temporary_record("");
        let ledger = Arc::new(Ledger::new(path.clone(), BTreeMap::new()));
        let mut child = std::process::Command::new("sleep");
        let mut child = child.arg("30").process_group(0).spawn().unwrap();
        let recorded = record_in(&ledger, child.id());
        let group = i32::try_from(child.id()).unwrap();
        // Recorded at once, written once dated.
        assert!(ledger.groups.lock().contains_key(&group));
        dated(&path, child.id());
        drop(recorded);
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains(&format!("{} ", child.id()))
        );
        // Ended before its start time was read: never written.
        let ended = record_in(&ledger, child.id());
        drop(ended);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains(&format!("{} ", child.id()))
        );
        assert!(ledger.groups.lock().is_empty());
        child.kill().unwrap();
        child.wait().unwrap();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_group_a_dead_server_left_running_is_stopped_and_one_given_up_is_not() {
        use std::os::unix::process::CommandExt as _;
        let spawn = || {
            let mut command = std::process::Command::new("sleep");
            command.arg("30").process_group(0);
            command.spawn().unwrap()
        };
        let path = temporary_record("");
        let mut left = spawn();
        let mut other = spawn();
        // What a server that was killed wrote: `left` as it started; and
        // `other`'s pid for a program that started at another time.
        let ledger = Arc::new(Ledger::new(path.clone(), BTreeMap::new()));
        let _left = record_in(&ledger, left.id());
        dated(&path, left.id());
        std::fs::write(
            &path,
            std::fs::read_to_string(&path).unwrap()
                + &format!("{} Mon Jan  1 00:00:00 2001\n", other.id()),
        )
        .unwrap();
        // The pid recorded as the server's is not this test's process any
        // more: mark it gone.
        let text = std::fs::read_to_string(&path).unwrap().replace(
            &format!("server {} ", std::process::id()),
            "server 2147483646 ",
        );
        std::fs::write(&path, text).unwrap();
        assert_eq!(
            sweep(&path, probe),
            Sweep::Done {
                stopped: 1,
                carried: BTreeMap::new()
            }
        );
        assert!(left.wait().unwrap().code().is_none(), "killed by a signal");
        assert!(other.try_wait().unwrap().is_none(), "left alone");
        assert!(!path.exists(), "the record is used once");

        // A server that stops normally gives its record up before its lock:
        // what it is still stopping is not the next server's to kill.
        let mut exiting = spawn();
        let ledger = Arc::new(Ledger::new(path.clone(), BTreeMap::new()));
        let recorded = record_in(&ledger, exiting.id());
        dated(&path, exiting.id());
        ledger.release();
        drop(recorded);
        assert!(
            !path.exists(),
            "nothing is written once the record is given up"
        );
        assert_eq!(
            sweep(&path, probe),
            Sweep::Done {
                stopped: 0,
                carried: BTreeMap::new()
            }
        );
        assert!(exiting.try_wait().unwrap().is_none());
        for mut child in [other, exiting] {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
