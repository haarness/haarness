//! Whether a process runs inside Haarness: started, directly or further down, by a `haarness` program (a server or
//! the terminal UI hosting one), the way an agent's shell command, a hook or a terminal pane is. Settings that give
//! agents more reach refuse changes from such a process, so an agent cannot widen what the person allowed by running
//! a command, whatever variables that command clears.
//!
//! Two marks tell: the process's ancestry, and the process group Haarness starts each command in. A command's shell
//! starts without job control, so a program that left the tree (put in the background with `nohup` or `( … & )` and
//! handed to launchd by the system) still runs in its command's group, whose leader, the command's own shell, is still
//! below Haarness; a server stops that group when the command ends, and until then knows it as one it started. What
//! escapes both is a program that moves to a group or session of its own (job control turned on with `set -m`,
//! `setsid`, a daemon detaching itself) once the process that started it is gone, which also outlives its command,
//! or one launchd or another app starts for it (`launchctl`, `at`, `open`): this guards the routes an agent would
//! take, not a determined program.
//!
//! This reads the process table, so it costs a `ps` (and an `lsof` to place a local connection): callers ask only for
//! the rare request that would widen something.

use parking_lot::Mutex;
use std::{collections::BTreeSet, net::SocketAddr, path::Path};

/// The name of Haarness's command-line program, which runs every server.
const PROGRAM: &str = if cfg!(windows) {
    "haarness.exe"
} else {
    "haarness"
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Process {
    pid: u32,
    parent: u32,
    group: i32,
    program: String,
}

/// Whether `pid` runs inside Haarness: a `haarness` program is among its ancestors (`pid` itself does not count), or
/// it or one of its ancestors runs in a process group Haarness started for a command. An unreadable process table
/// reads as no.
#[must_use]
pub fn inside_haarness(pid: u32) -> bool {
    table().is_some_and(|rows| {
        let started = STARTED.lock().clone();
        below_program(&rows, pid) || in_haarness_group(&rows, pid, &started)
    })
}

/// The process groups this process started for commands. A server stops a command's group when the command ends, a
/// moment after its leader: until then a program in it is known here, with no leader to tell.
static STARTED: std::sync::LazyLock<Mutex<BTreeSet<i32>>> =
    std::sync::LazyLock::new(Default::default);

/// Notes that this process just started the process group `leader` leads, for a command (see
/// [`crate::process_groups::recorded`]), and forgets the groups no process runs in any more.
///
/// Every host that records a group lands here (shell commands, workflows, extension and MCP hosts), but none of them
/// reads the note: only [`inside_haarness`] does, for the computer use settings, and a note counts only in the process
/// that started the group. That is the server; `haarness mac` starts no groups, so its own set stays empty.
pub(crate) fn mark_started(leader: u32) {
    let Ok(group) = i32::try_from(leader) else {
        return;
    };
    let mut started = STARTED.lock();
    started.retain(|group| group_runs(*group));
    if group > 1 {
        started.insert(group);
    }
}

/// Whether some process still runs in `group`.
#[cfg(unix)]
fn group_runs(group: i32) -> bool {
    !matches!(
        nix::sys::signal::killpg(nix::unistd::Pid::from_raw(group), None),
        Err(nix::errno::Errno::ESRCH)
    )
}

#[cfg(not(unix))]
fn group_runs(_group: i32) -> bool {
    false
}

/// Whether `pid`, or a process above it, runs in a process group Haarness started for a command: one whose leader
/// runs below a `haarness` program, or one of `started` (this process's own) whose leader has ended. A leader that runs
/// elsewhere leads a later group that happens to have the same id: an id is free again only once its group is gone.
fn in_haarness_group(rows: &[Process], pid: u32, started: &BTreeSet<i32>) -> bool {
    let row = |pid: u32| rows.iter().find(|row| row.pid == pid);
    let mut current = row(pid);
    for _ in 0..128 {
        let Some(process) = current.filter(|process| process.pid > 1) else {
            return false;
        };
        let group = process.group;
        let leader = u32::try_from(group)
            .ok()
            .filter(|leader| *leader > 1)
            .and_then(|leader| row(leader).map(|_| leader));
        let ours = match leader {
            Some(leader) => below_program(rows, leader),
            None => started.contains(&group),
        };
        if ours {
            return true;
        }
        current = row(process.parent);
    }
    false
}

/// The process on this machine that holds the client end of a connection from `peer`: the process behind a request
/// a server received. `None` for a peer on another machine, or when it cannot be told.
#[must_use]
pub fn local_client(peer: SocketAddr) -> Option<u32> {
    if !peer.ip().is_loopback() {
        return None;
    }
    let program = ["/usr/sbin/lsof", "/usr/bin/lsof"]
        .into_iter()
        .find(|path| Path::new(path).exists())?;
    let output = std::process::Command::new(program)
        .args([
            "-nP",
            "-a",
            &format!("-iTCP:{}", peer.port()),
            "-sTCP:ESTABLISHED",
            "-Fpn",
        ])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    client_in_listing(&String::from_utf8_lossy(&output.stdout), peer)
}

/// The pid in `lsof -Fpn` output whose socket starts at `peer`: the server's own end of the same connection starts
/// at the server's address instead.
fn client_in_listing(listing: &str, peer: SocketAddr) -> Option<u32> {
    let start = format!("{peer}->");
    let own = std::process::id();
    let mut pid = None;
    for line in listing.lines() {
        if let Some(number) = line.strip_prefix('p') {
            pid = number.trim().parse::<u32>().ok();
        } else if let Some(name) = line.strip_prefix('n')
            && name.starts_with(&start)
            && pid.is_some_and(|pid| pid != own)
        {
            return pid;
        }
    }
    None
}

fn below_program(rows: &[Process], pid: u32) -> bool {
    let row = |pid: u32| rows.iter().find(|row| row.pid == pid);
    let mut current = row(pid).map(|row| row.parent);
    // A table read while processes come and go could hold a loop.
    for _ in 0..128 {
        let Some(parent) = current.filter(|pid| *pid > 1).and_then(row) else {
            return false;
        };
        if parent.program == PROGRAM {
            return true;
        }
        current = Some(parent.parent);
    }
    false
}

#[cfg(unix)]
fn table() -> Option<Vec<Process>> {
    let output = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid=,comm="])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_table(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(not(unix))]
fn table() -> Option<Vec<Process>> {
    None
}

/// `ps -o pid=,ppid=,pgid=,comm=` lines. macOS prints the program's path, which may hold spaces; Linux its name.
fn parse_table(text: &str) -> Vec<Process> {
    text.lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (parent, rest) = rest.trim_start().split_once(char::is_whitespace)?;
            let (group, command) = rest.trim_start().split_once(char::is_whitespace)?;
            let program = Path::new(command.trim())
                .file_name()?
                .to_string_lossy()
                .into_owned();
            Some(Process {
                pid: pid.parse().ok()?,
                parent: parent.parse().ok()?,
                group: group.parse().ok()?,
                program,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Desktop's server (602) runs an agent's command (603) in a group of its own; 610 is a program that command put in
    // the background, which the system handed to launchd, and 611 one that program started in a group of its own.
    const TABLE: &str = "    1     0     1 /sbin/launchd
  500     1   500 /System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal
  501   500   501 /bin/zsh
  502   501   502 /Users/sam/.local/bin/haarness
  503   502   503 /bin/zsh
  504   503   503 /usr/bin/curl
  600     1   600 /Applications/Haarness.app/Contents/MacOS/Haarness
  601   600   600 /Applications/Haarness.app/Contents/Frameworks/Haarness Helper (Renderer).app/Contents/MacOS/Haarness Helper (Renderer)
  602   600   602 /Applications/Haarness.app/Contents/Resources/bin/haarness
  603   602   603 /bin/sh
  604   603   603 python3
  610     1   603 /usr/bin/curl
  611   610   611 /bin/sh
  620     1   620 /usr/bin/curl
";

    #[test]
    fn a_process_started_by_haarness_is_inside_it_and_others_are_not() {
        let rows = parse_table(TABLE);
        assert_eq!(rows.len(), 14);
        assert_eq!(rows[6].program, "Haarness");
        assert_eq!(rows[7].program, "Haarness Helper (Renderer)");
        assert_eq!(rows[11].group, 603);
        // An agent's command under the terminal UI, and one under the server Desktop runs.
        assert!(below_program(&rows, 504));
        assert!(below_program(&rows, 604));
        // The terminal UI itself, the shell it runs in, and Desktop's own windows.
        assert!(!below_program(&rows, 502));
        assert!(!below_program(&rows, 501));
        assert!(!below_program(&rows, 601));
        // A process the table does not know.
        assert!(!below_program(&rows, 999));
        // A program that left the tree is not below Haarness any more.
        assert!(!below_program(&rows, 610));
    }

    #[test]
    fn a_program_that_left_the_tree_is_known_by_its_group() {
        let rows = parse_table(TABLE);
        let none = BTreeSet::new();
        // Put in the background by the command while it runs, and a program it started in a group of its own.
        assert!(in_haarness_group(&rows, 610, &none));
        assert!(in_haarness_group(&rows, 611, &none));
        // Still attached: the group says so as well as the tree.
        assert!(in_haarness_group(&rows, 604, &none));
        // The person's own programs: Terminal's shells, the terminal UI, Desktop, another curl.
        for pid in [501, 502, 600, 601, 620] {
            assert!(!in_haarness_group(&rows, pid, &none), "{pid}");
        }
        // A group of the same id led by a program outside Haarness is a later one.
        assert!(!in_haarness_group(&rows, 620, &BTreeSet::from([620])));
    }

    #[test]
    fn a_group_whose_leader_just_ended_is_known_to_the_process_that_started_it() {
        // The command's shell (603) has ended and the server has not stopped the rest of its group yet.
        let rows: Vec<Process> = parse_table(TABLE)
            .into_iter()
            .filter(|row| row.pid != 603 && row.pid != 604)
            .collect();
        assert!(!in_haarness_group(&rows, 610, &BTreeSet::new()));
        let started = BTreeSet::from([603]);
        assert!(in_haarness_group(&rows, 610, &started));
        assert!(in_haarness_group(&rows, 611, &started));
        assert!(!in_haarness_group(&rows, 620, &started));
    }

    #[test]
    fn a_loop_in_the_table_ends() {
        let rows = parse_table("  10    11    10 /bin/a\n  11    10    11 /bin/b\n");
        assert!(!below_program(&rows, 10));
        assert!(!in_haarness_group(&rows, 10, &BTreeSet::from([99])));
    }

    /// The real thing: a command in a group of its own starts a program in the background and ends. Until its group is
    /// stopped the program runs under launchd (or init), outside this process's tree and with no leader, and is still
    /// known by its group; a program this process started some other way is not.
    #[cfg(unix)]
    #[test]
    fn a_program_a_command_left_running_is_still_inside() {
        use std::os::unix::process::CommandExt as _;
        let folder =
            std::env::temp_dir().join(format!("haarness-process-tree-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&folder).unwrap();
        let pid_file = folder.join("left.pid");
        let script = format!(
            "( sleep 30 > /dev/null 2>&1 & echo $! > '{}' ) ; exit 0",
            pid_file.display()
        );
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", &script]).process_group(0);
        let mut leader = command.spawn().unwrap();
        mark_started(leader.id());
        assert!(leader.wait().unwrap().success());
        let left: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let checked = (inside_haarness(left), inside_haarness(other.id()));
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(i32::try_from(left).unwrap()),
            nix::sys::signal::Signal::SIGKILL,
        );
        let _ = other.kill();
        let _ = other.wait();
        let _ = std::fs::remove_dir_all(&folder);
        assert_eq!(checked, (true, false));
    }

    #[test]
    fn the_client_end_of_a_connection_is_the_one_starting_at_the_peer() {
        let peer: SocketAddr = "127.0.0.1:50123".parse().unwrap();
        let listing = "p4242\nf9\nn127.0.0.1:4096->127.0.0.1:50123\np777\nf3\nn127.0.0.1:50123->127.0.0.1:4096\n";
        assert_eq!(client_in_listing(listing, peer), Some(777));
        assert_eq!(
            client_in_listing("p777\nf3\nn127.0.0.1:4096->127.0.0.1:50124\n", peer),
            None
        );
        let six: SocketAddr = "[::1]:50123".parse().unwrap();
        assert_eq!(
            client_in_listing("p88\nf4\nn[::1]:50123->[::1]:4096\n", six),
            Some(88)
        );
    }
}
