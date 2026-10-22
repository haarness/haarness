//! The process groups a workspace server starts, recorded so the next server
//! for its folder can stop the ones a crash left running.
//!
//! A server stops what it started when it stops: a shell command's group, a
//! workflow's, an extension host's, an MCP server's. Killed or crashed, it
//! stops nothing, and a command that writes nothing (a dev server holding
//! its port, a `sleep`, a watcher) runs on under launchd or init. So each
//! group is written to a small file beside the folder's server lock while it
//! runs, and the next server, once it holds that lock (so the one that wrote
//! the file is gone), stops every group still running as it was started. A
//! group is stopped only while its leader is still the process that was
//! started, by pid and start time, so a pid the system reused is never hit.
//! Unix only: elsewhere nothing is recorded.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;

/// How far a leader's start time may be from the time recorded for it: `ps`
/// says it to the second.
const START_TOLERANCE: Duration = Duration::from_secs(2);

struct Ledger {
    path: PathBuf,
    /// Group id to the time its leader started, in seconds since the epoch.
    groups: Mutex<BTreeMap<i32, u64>>,
}

static LEDGER: OnceLock<Ledger> = OnceLock::new();

impl Ledger {
    fn write(&self, groups: &BTreeMap<i32, u64>) {
        let text = groups
            .iter()
            .fold(String::new(), |mut text, (group, started)| {
                use std::fmt::Write as _;
                let _ = writeln!(text, "{group} {started}");
                text
            });
        let partial = self.path.with_extension("partial");
        if std::fs::write(&partial, text).is_ok() {
            let _ = std::fs::rename(&partial, &self.path);
        }
    }
}

/// From now on, the groups this process starts are written to `path`. A
/// workspace server calls it once, holding its folder's lock, after
/// [`stop_left_running`]; nothing is recorded in any other process.
pub fn record_to(path: PathBuf) {
    if cfg!(unix) {
        let ledger = LEDGER.get_or_init(|| Ledger {
            path,
            groups: Mutex::new(BTreeMap::new()),
        });
        ledger.write(&ledger.groups.lock());
    }
}

/// A started process group, in the record until dropped.
#[must_use = "the group leaves the record when this is dropped"]
pub struct Recorded(Option<i32>);

/// Records the process group `leader` leads, just started.
pub fn recorded(leader: Option<u32>) -> Recorded {
    let (Some(ledger), Some(group)) =
        (LEDGER.get(), leader.and_then(|pid| i32::try_from(pid).ok()))
    else {
        return Recorded(None);
    };
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let mut groups = ledger.groups.lock();
    groups.insert(group, started);
    ledger.write(&groups);
    Recorded(Some(group))
}

impl Drop for Recorded {
    fn drop(&mut self) {
        if let (Some(group), Some(ledger)) = (self.0, LEDGER.get()) {
            let mut groups = ledger.groups.lock();
            groups.remove(&group);
            ledger.write(&groups);
        }
    }
}

/// Stops the groups the record at `path` names whose leader still runs as it
/// was started, and empties the record. Call only while holding the folder's
/// server lock, so the server that wrote it is gone. Returns how many groups
/// were stopped.
#[must_use]
pub fn stop_left_running(path: &Path) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let _ = std::fs::remove_file(path);
    let recorded = parse_record(&text);
    if recorded.is_empty() {
        return 0;
    }
    let Some(running) = running_leaders() else {
        tracing::warn!("could not list processes; commands an earlier server started are left");
        return 0;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let mut stopped = 0;
    for group in left_running(&recorded, &running, now) {
        if kill_group(group) {
            stopped += 1;
        }
    }
    stopped
}

fn parse_record(text: &str) -> Vec<(i32, u64)> {
    text.lines()
        .filter_map(|line| {
            let (group, started) = line.split_once(' ')?;
            Some((group.parse().ok()?, started.trim().parse().ok()?))
        })
        .filter(|(group, _)| *group > 1)
        .collect()
}

/// The recorded groups whose leader still runs: the process of that pid
/// leads that group and started when the group was recorded. `running` maps
/// a group leader's pid to how long ago it started.
fn left_running(recorded: &[(i32, u64)], running: &BTreeMap<i32, u64>, now: u64) -> Vec<i32> {
    recorded
        .iter()
        .filter(|(group, started)| {
            running.get(group).is_some_and(|elapsed| {
                now.saturating_sub(*elapsed).abs_diff(*started) <= START_TOLERANCE.as_secs()
            })
        })
        .map(|(group, _)| *group)
        .collect()
}

/// Every process that leads its own group, with how many seconds ago it
/// started; `None` when `ps` cannot say.
fn running_leaders() -> Option<BTreeMap<i32, u64>> {
    if !cfg!(unix) {
        return None;
    }
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid=,pgid=,etime="])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_leaders(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_leaders(listing: &str) -> BTreeMap<i32, u64> {
    listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid: i32 = fields.next()?.parse().ok()?;
            let group: i32 = fields.next()?.parse().ok()?;
            let elapsed = elapsed_seconds(fields.next()?)?;
            (pid == group).then_some((pid, elapsed))
        })
        .collect()
}

/// `ps`'s `etime`: `[[dd-]hh:]mm:ss`.
fn elapsed_seconds(text: &str) -> Option<u64> {
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, text),
    };
    let mut seconds = 0;
    for part in clock.split(':') {
        seconds = seconds * 60 + part.parse::<u64>().ok()?;
    }
    Some(days * 86_400 + seconds)
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
    fn reads_the_elapsed_time_ps_prints() {
        assert_eq!(elapsed_seconds("00:07"), Some(7));
        assert_eq!(elapsed_seconds("12:34"), Some(754));
        assert_eq!(elapsed_seconds("01:02:03"), Some(3723));
        assert_eq!(elapsed_seconds("2-01:02:03"), Some(2 * 86_400 + 3723));
        assert_eq!(elapsed_seconds("soon"), None);
    }

    #[test]
    fn only_a_leader_that_started_when_its_group_was_recorded_is_stopped() {
        let now = 1_000_000;
        let listing = "  100   100    00:10\n  101   100    00:09\n  200   200    05:00\n  300   300    00:30\n";
        let running = parse_leaders(listing);
        // 101 leads no group of its own.
        assert_eq!(running.keys().copied().collect::<Vec<_>>(), [100, 200, 300]);
        let recorded = parse_record(&format!(
            "100 {}\n200 {}\n300 {}\n400 {}\n1 {}\nnot a line\n",
            now - 10,
            // Recorded 10 s ago, but this process 200 started 5 minutes
            // ago: the pid now belongs to another program.
            now - 10,
            now - 31,
            now - 5,
            now,
        ));
        // 100 runs as it started (300 within the tolerance); 400 is gone;
        // pid 1 is never named.
        assert_eq!(left_running(&recorded, &running, now), [100, 300]);
    }

    #[cfg(unix)]
    #[test]
    fn a_group_a_dead_server_left_running_is_stopped_and_a_reused_pid_is_not() {
        use std::os::unix::process::CommandExt as _;
        let directory =
            std::env::temp_dir().join(format!("haarness-process-groups-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&directory).unwrap();
        let spawn = || {
            let mut command = std::process::Command::new("sleep");
            command.arg("30").process_group(0);
            command.spawn().unwrap()
        };
        let mut left = spawn();
        let mut other = spawn();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let record = directory.join("process-groups");
        // What a server killed before it could stop `left` wrote; `other`'s
        // pid was recorded for a program that started an hour ago.
        std::fs::write(
            &record,
            format!("{} {now}\n{} {}\n", left.id(), other.id(), now - 3600),
        )
        .unwrap();
        assert_eq!(stop_left_running(&record), 1);
        assert!(left.wait().unwrap().code().is_none(), "killed by a signal");
        assert!(other.try_wait().unwrap().is_none(), "left alone");
        assert!(!record.exists(), "the record is used once");
        other.kill().unwrap();
        other.wait().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
