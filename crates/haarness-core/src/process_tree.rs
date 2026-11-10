//! Whether a process runs inside Haarness: started, directly or further down, by a Haarness program (a server or
//! the terminal UI hosting one), the way an agent's shell command, a hook or a terminal pane is. Settings that give
//! agents more reach refuse changes from such a process, so an agent cannot widen what the person allowed by running
//! a command, whatever variables that command clears.
//!
//! A Haarness program is known by the name of the file the system ran for it, which neither a link nor the name it was
//! started under changes: `haarness`, a copy kept under a longer name (`haarness-test`, `haarness-0.1.19`), or the
//! program asking, whatever its file is called. So a server checking a request knows every program below itself, and
//! `haarness mac` run from the same copy as the server above it knows that server. Not known: a server run from a file
//! named otherwise, to a `haarness mac` run from yet another copy.
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
//! the rare request that would widen something. Unix only: elsewhere there is no `ps` table to read, and computer use,
//! the one thing this guards, runs on macOS only, so no process reads as inside.

#[cfg(unix)]
use parking_lot::Mutex;
#[cfg(unix)]
use std::collections::BTreeSet;
use std::{net::SocketAddr, path::Path};

/// The name of Haarness's command-line program, which runs every server.
#[cfg(unix)]
const PROGRAM: &str = "haarness";

/// Whether `program`, the name of the file the system ran for a process, is a Haarness program: `haarness`, a copy of
/// it under a longer name (`haarness-test`, `haarness_old`, `haarness.new`), or `own`, the program of the process
/// asking, which is Haarness whatever its file is called.
#[cfg(unix)]
fn is_haarness(program: &str, own: Option<&str>) -> bool {
    program
        .strip_prefix(PROGRAM)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '_', '.']))
        || own.is_some_and(|own| own == program)
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Process {
    pid: u32,
    parent: u32,
    group: i32,
    /// It leads its session, as a terminal's shell does (`s` in its state).
    session_leader: bool,
    program: String,
}

/// Whether `pid` runs inside Haarness: a Haarness program is among its ancestors (`pid` itself does not count), or
/// it or one of its ancestors runs in a process group Haarness started for a command. An unreadable process table
/// reads as no, and so does every process off Unix.
#[must_use]
pub fn inside_haarness(pid: u32) -> bool {
    #[cfg(unix)]
    {
        table().is_some_and(|rows| {
            let started = STARTED.lock().clone();
            let own = own_program(&rows);
            below_program(&rows, pid, own) || in_haarness_group(&rows, pid, &started, own)
        })
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Whether this very process runs inside Haarness: started for an agent's command, which names its chat, or as
/// [`inside_haarness`] tells, which also catches a command that cleared that variable and one put in the background.
#[must_use]
pub fn this_process_inside() -> bool {
    names_a_chat() || inside_haarness(std::process::id())
}

/// Whether this very process runs for an agent: started for an agent's command, which names its chat, or inside
/// Haarness ([`this_process_inside`]) other than in a terminal a person opened in Haarness (see
/// [`request_from_agent`]).
#[must_use]
pub fn this_process_for_agent() -> bool {
    if names_a_chat() {
        return true;
    }
    #[cfg(unix)]
    {
        table().is_some_and(|rows| {
            let started = STARTED.lock().clone();
            for_agent(&rows, std::process::id(), &started, own_program(&rows), 0)
        })
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// For a process a Haarness program started for itself, such as the server a launcher leaves running: whether the one
/// that started it runs for an agent, as [`this_process_for_agent`] would tell there. A process whose parent is no
/// Haarness program was not started that way, and counts as an agent's.
#[must_use]
pub fn parent_for_agent() -> bool {
    if names_a_chat() {
        return true;
    }
    #[cfg(unix)]
    {
        let parent = std::os::unix::process::parent_id();
        table().is_some_and(|rows| {
            let started = STARTED.lock().clone();
            launcher_for_agent(&rows, parent, &started, own_program(&rows))
        })
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Whether a request this process received from the local process `pid` comes from a program that runs for an agent:
/// inside Haarness, as [`request_from_inside`] tells, except in a terminal a person opened in Haarness. Such a
/// terminal is a session that a Haarness program running for a person started (as Desktop's terminal panel is): what
/// runs below its shell is what the person typed. An agent's command, a hook, an MCP server or a server's own helper
/// is no terminal, and neither is one an agent's own Haarness started. Nor can an agent type into a person's terminal:
/// the server opens and connects terminals only for requests that do not run for an agent.
#[must_use]
pub fn request_from_agent(pid: u32) -> bool {
    #[cfg(unix)]
    {
        table().is_some_and(|rows| {
            let started = STARTED.lock().clone();
            let helpers = HELPERS.lock().clone();
            let own = own_program(&rows);
            !helper(&rows, pid, std::process::id(), &helpers, &started)
                && for_agent(&rows, pid, &started, own, 0)
        })
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Whether a request this process received from the local process `pid` comes from a program Haarness started, as
/// [`inside_haarness`] tells, unless `pid` is a helper this process started to carry requests for people elsewhere
/// ([`mark_helper`]): what the connector for phones and Haarness Cloud sends comes from those people.
#[must_use]
pub fn request_from_inside(pid: u32) -> bool {
    #[cfg(unix)]
    {
        table().is_some_and(|rows| {
            let started = STARTED.lock().clone();
            let helpers = HELPERS.lock().clone();
            let own = own_program(&rows);
            !helper(&rows, pid, std::process::id(), &helpers, &started)
                && (below_program(&rows, pid, own) || in_haarness_group(&rows, pid, &started, own))
        })
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Whether `parent`, which started a process for itself, is no Haarness program (`own` as in [`is_haarness`]) or runs
/// for an agent.
#[cfg(unix)]
fn launcher_for_agent(
    rows: &[Process],
    parent: u32,
    started: &BTreeSet<i32>,
    own: Option<&str>,
) -> bool {
    let launcher = rows
        .iter()
        .find(|row| row.pid == parent && parent > 1)
        .is_some_and(|row| is_haarness(&row.program, own));
    !launcher || for_agent(rows, parent, started, own, 0)
}

/// How many Haarness programs deep a terminal is judged: one opened in a server a person started in a terminal of
/// another, and so on.
#[cfg(unix)]
const TERMINAL_DEPTH: usize = 8;

/// Whether `pid` runs inside Haarness ([`inside_haarness`], `own` as in [`is_haarness`]) other than in a terminal a
/// person opened ([`in_person_terminal`]).
#[cfg(unix)]
fn for_agent(
    rows: &[Process],
    pid: u32,
    started: &BTreeSet<i32>,
    own: Option<&str>,
    depth: usize,
) -> bool {
    (below_program(rows, pid, own) || in_haarness_group(rows, pid, started, own))
        && !in_person_terminal(rows, pid, started, own, depth)
}

/// Whether `pid` runs in a terminal a person opened in Haarness: the nearest process at or above it that a Haarness
/// program started leads a session, as a terminal's shell does, and that Haarness program runs for a person itself.
/// Whatever else a Haarness program starts directly (an agent's command, a hook, an MCP server, a helper) leads no
/// session; a program that starts a session of its own below one of those is not started by a Haarness program, and
/// one that leaves the tree is in no terminal.
#[cfg(unix)]
fn in_person_terminal(
    rows: &[Process],
    pid: u32,
    started: &BTreeSet<i32>,
    own: Option<&str>,
    depth: usize,
) -> bool {
    let row = |pid: u32| rows.iter().find(|row| row.pid == pid);
    let mut current = row(pid);
    // A table read while processes come and go could hold a loop.
    for _ in 0..128 {
        let Some(process) = current.filter(|process| process.pid > 1) else {
            return false;
        };
        let Some(parent) = row(process.parent).filter(|parent| parent.pid > 1) else {
            return false;
        };
        if is_haarness(&parent.program, own) {
            return process.session_leader
                && depth < TERMINAL_DEPTH
                && !for_agent(rows, parent.pid, started, own, depth + 1);
        }
        current = Some(parent);
    }
    false
}

/// Whether an agent's command runs here: the shell tool names the chat it runs for.
fn names_a_chat() -> bool {
    std::env::var_os(crate::SESSION_ID_ENV).is_some_and(|value| !value.is_empty())
}

/// The program this process runs, as the table names it.
#[cfg(unix)]
fn own_program(rows: &[Process]) -> Option<&str> {
    let own = std::process::id();
    rows.iter()
        .find(|row| row.pid == own)
        .map(|row| row.program.as_str())
}

/// The helpers this process started to carry requests for people elsewhere (see [`mark_helper`]).
#[cfg(unix)]
static HELPERS: std::sync::LazyLock<Mutex<BTreeSet<u32>>> =
    std::sync::LazyLock::new(Default::default);

/// Notes that this process just started `pid` to carry requests for people elsewhere, such as the connector that
/// brings requests from phones and Haarness Cloud, so [`request_from_inside`] takes what it sends as theirs.
/// [`forget_helper`] once it has ended, before its id can come back.
pub fn mark_helper(pid: u32) {
    #[cfg(unix)]
    HELPERS.lock().insert(pid);
    #[cfg(not(unix))]
    let _ = pid;
}

/// Forgets a helper [`mark_helper`] noted, once it has ended.
pub fn forget_helper(pid: u32) {
    #[cfg(unix)]
    HELPERS.lock().remove(&pid);
    #[cfg(not(unix))]
    let _ = pid;
}

/// Whether `pid` is one of `helpers` started by `own` (this process): still its child, and in no process group it
/// started for a command.
#[cfg(unix)]
fn helper(
    rows: &[Process],
    pid: u32,
    own: u32,
    helpers: &BTreeSet<u32>,
    started: &BTreeSet<i32>,
) -> bool {
    helpers.contains(&pid)
        && rows
            .iter()
            .find(|row| row.pid == pid)
            .is_some_and(|row| row.parent == own && !started.contains(&row.group))
}

/// The process groups this process started for commands. A server stops a command's group when the command ends, a
/// moment after its leader: until then a program in it is known here, with no leader to tell.
#[cfg(unix)]
static STARTED: std::sync::LazyLock<Mutex<BTreeSet<i32>>> =
    std::sync::LazyLock::new(Default::default);

/// Notes that this process just started the process group `leader` leads, for a command (see
/// [`crate::process_groups::recorded`]), and forgets the groups no process runs in any more.
///
/// Every host that records a group lands here (shell commands, workflows, extension and MCP hosts), but none of them
/// reads the note: only [`inside_haarness`] does, for the computer use settings, and a note counts only in the process
/// that started the group. That is the server; `haarness mac` starts no groups, so its own set stays empty.
#[cfg(unix)]
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

/// Off Unix nothing reads the note (see [`inside_haarness`]).
#[cfg(not(unix))]
pub(crate) fn mark_started(_leader: u32) {}

/// Whether some process still runs in `group`.
#[cfg(unix)]
fn group_runs(group: i32) -> bool {
    !matches!(
        nix::sys::signal::killpg(nix::unistd::Pid::from_raw(group), None),
        Err(nix::errno::Errno::ESRCH)
    )
}

/// Whether `pid`, or a process above it, runs in a process group Haarness started for a command: one whose leader
/// runs below a Haarness program (`own` as in [`is_haarness`]), or one of `started` (this process's own) whose leader
/// has ended. A leader that runs elsewhere leads a later group that happens to have the same id: an id is free again
/// only once its group is gone.
#[cfg(unix)]
fn in_haarness_group(
    rows: &[Process],
    pid: u32,
    started: &BTreeSet<i32>,
    own: Option<&str>,
) -> bool {
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
            Some(leader) => below_program(rows, leader, own),
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

/// Whether a Haarness program (`own` as in [`is_haarness`]) is among the ancestors of `pid`.
#[cfg(unix)]
fn below_program(rows: &[Process], pid: u32, own: Option<&str>) -> bool {
    let row = |pid: u32| rows.iter().find(|row| row.pid == pid);
    let mut current = row(pid).map(|row| row.parent);
    // A table read while processes come and go could hold a loop.
    for _ in 0..128 {
        let Some(parent) = current.filter(|pid| *pid > 1).and_then(row) else {
            return false;
        };
        if is_haarness(&parent.program, own) {
            return true;
        }
        current = Some(parent.parent);
    }
    false
}

/// The process table, each program named by the file the system ran for it (`ucomm`): `comm` on macOS is the name a
/// process was started under, which a link or its parent chooses.
#[cfg(unix)]
fn table() -> Option<Vec<Process>> {
    let output = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid=,stat=,ucomm="])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_table(&String::from_utf8_lossy(&output.stdout)))
}

/// `ps -o pid=,ppid=,pgid=,stat=,ucomm=` lines: the state's letters, then the program's file name, cut to 16 bytes on
/// macOS and 15 on Linux, which may hold spaces and is padded with them. A path, as `comm` gives on macOS, reads as its
/// file name.
#[cfg(unix)]
fn parse_table(text: &str) -> Vec<Process> {
    text.lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (parent, rest) = rest.trim_start().split_once(char::is_whitespace)?;
            let (group, rest) = rest.trim_start().split_once(char::is_whitespace)?;
            let (state, command) = rest.trim_start().split_once(char::is_whitespace)?;
            let program = Path::new(command.trim())
                .file_name()?
                .to_string_lossy()
                .into_owned();
            Some(Process {
                pid: pid.parse().ok()?,
                parent: parent.parse().ok()?,
                group: group.parse().ok()?,
                session_leader: state.contains('s'),
                program,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Desktop's server (602) runs an agent's command (603) in a group of its own; 610 is a program that command put in
    // the background, which the system handed to launchd, and 611 one that program started in a session of its own.
    // Desktop's terminal panel (640) is a shell leading a session of its own, where the person runs the terminal UI
    // (641) and curl (642). The table, and every test reading one, is Unix's: Windows has no `ps` table, and nothing
    // there reads as inside. Program names are as `ps` prints `ucomm`, cut to 16 bytes on macOS (where it pads them
    // with spaces, too); an `s` in the state marks a session's leader.
    #[cfg(unix)]
    const TABLE: &str = "    1     0     1 Ss   launchd
  500     1   500 S    Terminal
  501   500   501 Ss   zsh
  502   501   502 S+   haarness
  503   502   503 S    zsh
  504   503   503 S    curl
  600     1   600 S    Haarness
  601   600   600 S    Haarness Helper
  602   600   602 S    haarness
  603   602   603 S    sh
  604   603   603 S    python3
  610     1   603 S    curl
  611   610   611 Ss   sh
  620     1   620 S    curl
  640   602   640 Ss   zsh
  641   640   641 S+   haarness
  642   640   642 S+   curl
";

    /// [`TABLE`] with each `haarness` program run from a file called `program` instead.
    #[cfg(unix)]
    fn renamed(program: &str) -> Vec<Process> {
        parse_table(TABLE)
            .into_iter()
            .map(|mut row| {
                if row.program == PROGRAM {
                    program.clone_into(&mut row.program);
                }
                row
            })
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn a_process_started_by_haarness_is_inside_it_and_others_are_not() {
        let rows = parse_table(TABLE);
        assert_eq!(rows.len(), 17);
        assert!(rows[0].session_leader && !rows[1].session_leader);
        assert_eq!(rows[3].program, "haarness");
        assert_eq!(rows[6].program, "Haarness");
        assert_eq!(rows[7].program, "Haarness Helper");
        assert_eq!(rows[11].group, 603);
        // Padding is not part of a name, and a path, as `comm` gives on macOS, names its file.
        let padded = parse_table(
            "  602   600   602 S    haarness        \n  601   600   600 S    Haarness Helper \n",
        );
        assert_eq!(padded[0].program, "haarness");
        assert_eq!(padded[1].program, "Haarness Helper");
        let path = parse_table(
            "  602   600   602 S    /Applications/Haarness.app/Contents/Resources/bin/haarness\n",
        );
        assert_eq!(path[0].program, "haarness");
        for own in [None, Some("haarness")] {
            // An agent's command under the terminal UI, and one under the server Desktop runs.
            assert!(below_program(&rows, 504, own));
            assert!(below_program(&rows, 604, own));
            // The terminal UI itself, the shell it runs in, and Desktop's own windows.
            assert!(!below_program(&rows, 502, own));
            assert!(!below_program(&rows, 501, own));
            assert!(!below_program(&rows, 601, own));
            // A process the table does not know.
            assert!(!below_program(&rows, 999, own));
            // A program that left the tree is not below Haarness any more.
            assert!(!below_program(&rows, 610, own));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_haarness_run_from_a_renamed_file_is_still_haarness() {
        // A copy kept under a longer name, whoever asks.
        for program in [
            "haarness-test",
            "haarness-0.1.19",
            "haarness_old",
            "haarness.new",
        ] {
            let rows = renamed(program);
            assert!(below_program(&rows, 604, None), "{program}");
            assert!(below_program(&rows, 504, Some("haarness")), "{program}");
            assert!(
                in_haarness_group(&rows, 610, &BTreeSet::new(), None),
                "{program}"
            );
            assert!(!below_program(&rows, 501, None), "{program}");
            assert!(
                !in_haarness_group(&rows, 620, &BTreeSet::new(), None),
                "{program}"
            );
        }
        // Any other name, when the program asking runs from a file of that name: `haarness mac` run by a command of
        // a server both started from the same copy, or that server about a request.
        let rows = renamed("hn");
        assert!(!below_program(&rows, 604, None));
        assert!(!below_program(&rows, 604, Some("haarness")));
        assert!(!in_haarness_group(
            &rows,
            610,
            &BTreeSet::new(),
            Some("haarness")
        ));
        assert!(below_program(&rows, 604, Some("hn")));
        assert!(below_program(&rows, 504, Some("hn")));
        assert!(in_haarness_group(&rows, 610, &BTreeSet::new(), Some("hn")));
        assert!(in_haarness_group(&rows, 611, &BTreeSet::new(), Some("hn")));
        // The person's own programs stay outside, Desktop's windows included.
        for pid in [501, 502, 600, 601, 610, 620] {
            assert!(!below_program(&rows, pid, Some("hn")), "{pid}");
        }
        for pid in [501, 502, 600, 601, 620] {
            assert!(
                !in_haarness_group(&rows, pid, &BTreeSet::new(), Some("hn")),
                "{pid}"
            );
        }
        // Names that only start like Haarness's, or differ in case, are other programs.
        for program in ["haarnessd", "Haarness", "haarnes", "xhaarness"] {
            let rows = renamed(program);
            assert!(!below_program(&rows, 604, None), "{program}");
            assert!(
                !in_haarness_group(&rows, 610, &BTreeSet::new(), None),
                "{program}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_program_that_left_the_tree_is_known_by_its_group() {
        let rows = parse_table(TABLE);
        let none = BTreeSet::new();
        // Put in the background by the command while it runs, and a program it started in a group of its own.
        assert!(in_haarness_group(&rows, 610, &none, None));
        assert!(in_haarness_group(&rows, 611, &none, None));
        // Still attached: the group says so as well as the tree.
        assert!(in_haarness_group(&rows, 604, &none, None));
        // The person's own programs: Terminal's shells, the terminal UI, Desktop, another curl.
        for pid in [501, 502, 600, 601, 620] {
            assert!(!in_haarness_group(&rows, pid, &none, None), "{pid}");
        }
        // A group of the same id led by a program outside Haarness is a later one.
        assert!(!in_haarness_group(&rows, 620, &BTreeSet::from([620]), None));
    }

    #[cfg(unix)]
    #[test]
    fn a_group_whose_leader_just_ended_is_known_to_the_process_that_started_it() {
        // The command's shell (603) has ended and the server has not stopped the rest of its group yet.
        let rows: Vec<Process> = parse_table(TABLE)
            .into_iter()
            .filter(|row| row.pid != 603 && row.pid != 604)
            .collect();
        assert!(!in_haarness_group(&rows, 610, &BTreeSet::new(), None));
        let started = BTreeSet::from([603]);
        assert!(in_haarness_group(&rows, 610, &started, None));
        assert!(in_haarness_group(&rows, 611, &started, None));
        assert!(!in_haarness_group(&rows, 620, &started, None));
    }

    #[cfg(unix)]
    #[test]
    fn a_launcher_is_judged_by_where_it_runs() {
        let rows = parse_table(TABLE);
        let none = BTreeSet::new();
        // The terminal UI the person started, Desktop's server, and the terminal UI in Desktop's terminal panel start
        // their own servers.
        for parent in [502, 602, 641] {
            assert!(!launcher_for_agent(&rows, parent, &none, None), "{parent}");
        }
        // A command's shell or program is no Haarness program; neither is a process that is gone.
        for parent in [503, 603, 604, 999, 1] {
            assert!(launcher_for_agent(&rows, parent, &none, None), "{parent}");
        }
        // A Haarness program an agent's command runs is the agent's.
        let rows = parse_table(&format!("{TABLE}  605   603   603 S    haarness\n"));
        assert!(launcher_for_agent(&rows, 605, &none, None));
    }

    #[cfg(unix)]
    #[test]
    fn what_a_person_runs_in_a_terminal_haarness_hosts_is_theirs() {
        let rows = parse_table(TABLE);
        let none = BTreeSet::new();
        // Desktop's terminal panel, and the terminal UI and curl the person runs there: inside Haarness, and theirs.
        for pid in [640, 641, 642] {
            assert!(below_program(&rows, pid, None), "{pid}");
            assert!(in_person_terminal(&rows, pid, &none, None, 0), "{pid}");
            assert!(!for_agent(&rows, pid, &none, None, 0), "{pid}");
        }
        // An agent's command and what it runs, under Desktop's server and under the terminal UI, and what it left
        // running, in its group or in a session of its own.
        for pid in [503, 504, 603, 604, 610, 611] {
            assert!(!in_person_terminal(&rows, pid, &none, None, 0), "{pid}");
            assert!(for_agent(&rows, pid, &none, None, 0), "{pid}");
        }
        // The person's own programs outside Haarness are not inside to begin with.
        for pid in [501, 502, 600, 601, 620] {
            assert!(!for_agent(&rows, pid, &none, None, 0), "{pid}");
        }
        // A session an agent's command starts is no terminal of the person's.
        let rows = parse_table(&format!(
            "{TABLE}  650   604   650 Ss   sh\n  651   650   650 S    curl\n"
        ));
        assert!(for_agent(&rows, 651, &none, None, 0));
        // Nor is a terminal the Haarness an agent's command runs opened.
        let rows = parse_table(&format!(
            "{TABLE}  660   603   603 S    haarness\n  661   660   661 Ss   zsh\n  662   661   662 S    curl\n"
        ));
        assert!(for_agent(&rows, 662, &none, None, 0));
        // A terminal of a server the person started in Desktop's terminal panel is theirs.
        let rows = parse_table(&format!(
            "{TABLE}  670   640   670 S+   haarness\n  671   670   671 Ss   zsh\n  672   671   672 S    curl\n"
        ));
        assert!(!for_agent(&rows, 672, &none, None, 0));
    }

    #[cfg(unix)]
    #[test]
    fn only_a_helper_this_process_started_outside_any_command_speaks_for_others() {
        // Desktop's server (602) started a connector (630) in a group of its own, and runs a command (603).
        let rows = parse_table(&format!(
            "{TABLE}  630   602   630 S    haarness\n  631   603   603 S    haarness\n"
        ));
        let helpers = BTreeSet::from([630, 631]);
        let none = BTreeSet::new();
        assert!(helper(&rows, 630, 602, &helpers, &none));
        // Not one this process noted, not its child, or in a group it started for a command.
        assert!(!helper(&rows, 630, 602, &BTreeSet::new(), &none));
        assert!(!helper(&rows, 630, 502, &helpers, &none));
        assert!(!helper(&rows, 630, 602, &helpers, &BTreeSet::from([630])));
        assert!(!helper(&rows, 631, 602, &helpers, &none));
        assert!(!helper(&rows, 999, 602, &BTreeSet::from([999]), &none));
    }

    /// The real thing: of two programs this process (a Haarness program) started, the one it noted as a helper speaks
    /// for others until it is forgotten; the other is inside.
    #[cfg(unix)]
    #[test]
    fn a_helper_this_process_started_is_not_inside_while_noted() {
        let start = || {
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap()
        };
        let (mut noted, mut other) = (start(), start());
        mark_helper(noted.id());
        let while_noted = (
            request_from_inside(noted.id()),
            request_from_inside(other.id()),
        );
        forget_helper(noted.id());
        let forgotten = request_from_inside(noted.id());
        for child in [&mut noted, &mut other] {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert_eq!(while_noted, (false, true));
        assert!(forgotten);
    }

    /// The real thing: of two programs this process (a Haarness program, running for a person) started, the one that
    /// leads a session of its own, as a terminal's shell does, runs for the person; the other is an agent's.
    #[cfg(unix)]
    #[test]
    fn a_session_this_process_started_runs_for_the_person_who_runs_it() {
        let mut terminal = std::process::Command::new("python3")
            .args([
                "-c",
                "import os; os.setsid(); os.execvp('sleep', ['sleep', '30'])",
            ])
            .spawn()
            .unwrap();
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        // Until the session is set up the table may show the program as it started.
        let mut checked = (true, false);
        while started.elapsed() < std::time::Duration::from_secs(10) {
            checked = (
                request_from_agent(terminal.id()),
                request_from_agent(other.id()),
            );
            if checked == (false, true) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let own_for_agent = this_process_for_agent();
        for child in [&mut terminal, &mut other] {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert_eq!(checked, (false, true));
        assert!(!own_for_agent);
    }

    #[cfg(unix)]
    #[test]
    fn a_loop_in_the_table_ends() {
        let rows = parse_table("  10    11    10 S    a\n  11    10    11 S    b\n");
        assert!(!below_program(&rows, 10, None));
        assert!(!in_person_terminal(&rows, 10, &BTreeSet::new(), None, 0));
        assert!(!in_haarness_group(&rows, 10, &BTreeSet::from([99]), None));
    }

    /// The real thing: a command in a group of its own starts a program in the background and ends. Until its group is
    /// stopped the program runs under launchd (or init), outside this process's tree and with no leader, and is still
    /// known by its group; a program left the same way by a group this process did not note is not. A program this
    /// process started itself is inside whatever this test program's file is called: the process asking is Haarness.
    #[cfg(unix)]
    #[test]
    fn a_program_a_command_left_running_is_still_inside() {
        use std::os::unix::process::CommandExt as _;
        let folder =
            std::env::temp_dir().join(format!("haarness-process-tree-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&folder).unwrap();
        // Runs a command in a group of its own that leaves a program running and ends; the program's pid.
        let leave = |name: &str, note: bool| -> u32 {
            let pid_file = folder.join(name);
            let script = format!(
                "( sleep 30 > /dev/null 2>&1 & echo $! > '{}' ) ; exit 0",
                pid_file.display()
            );
            let mut command = std::process::Command::new("/bin/sh");
            command.args(["-c", &script]).process_group(0);
            let mut leader = command.spawn().unwrap();
            if note {
                mark_started(leader.id());
            }
            assert!(leader.wait().unwrap().success());
            std::fs::read_to_string(&pid_file)
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        };
        let left = leave("left.pid", true);
        let stray = leave("stray.pid", false);
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let checked = (
            inside_haarness(left),
            inside_haarness(stray),
            inside_haarness(child.id()),
        );
        for pid in [left, stray] {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&folder);
        assert_eq!(checked, (true, false, true));
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
