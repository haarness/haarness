//! Whether a process runs inside Haarness, and whether it runs for an agent. A process runs inside Haarness when a
//! Haarness program (a server, or the terminal UI hosting one) started it, directly or further down: an agent's shell
//! command, a hook, an MCP server, a language server, a terminal. It runs for an agent when it is inside other than in
//! a terminal a person opened in Haarness. Computer use refuses more reach to anything inside; a chat's approvals, a
//! turn that approves its own requests, an answer to an agent and a terminal are refused only to what runs for an agent.
//!
//! A Haarness program is known by the name of the file the system ran for it, which neither a link nor the name it was
//! started under changes: `haarness`, a copy kept under a longer name (`haarness-test`, `haarness-0.1.19`), or the
//! program asking, whatever its file is called. So a server checking a request knows every program below itself, and
//! `haarness mac` run from the same copy as the server above it knows that server. Not known: a server run from a file
//! named otherwise, to a `haarness mac` run from yet another copy.
//!
//! Three marks tell inside: the process's ancestry; the process group Haarness starts each command, hook and language
//! server in; and a process group a Haarness program leads. A program that left the tree (put in the background with
//! `nohup` or `( … & )`, or left by a process that exited) still runs in its group. What escapes them is a program
//! that moves to a group or session of its own once the process that started it is gone, or one launchd or another
//! app starts for it: this guards the routes an agent would take, not a determined program.
//!
//! A terminal a person opened is one this process noted when it started its shell ([`mark_terminal`]), or, started by
//! another Haarness program that runs for a person, a shell leading a session with a terminal of its own.
//!
//! This reads the process table (`/bin/ps`), and places a local connection with `netstat` on macOS and `lsof`
//! elsewhere. Unix only: elsewhere no process reads as inside, and on Linux without `lsof` a connection's program
//! cannot be told ([`Peer::Untold`]).

#[cfg(unix)]
use parking_lot::Mutex;
#[cfg(unix)]
use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
#[cfg(unix)]
use std::path::Path;

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
    /// It has a terminal of its own.
    tty: bool,
    program: String,
}

/// Who is at the other end of a connection this process accepted (see [`peer`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peer {
    /// Another computer.
    Elsewhere,
    /// This computer, where a connection's program cannot be told (Linux without `lsof`, Windows).
    Untold,
    /// This computer, but no program holds the connection any more, or the process table could not be read.
    Unknown,
    /// This very process, sending on behalf of someone it heard from (a request it forwards).
    Itself,
    /// A program on this computer: whether it runs inside Haarness, and whether for an agent.
    Program { inside: bool, agent: bool },
}

/// Who holds the other end of a connection from `address` that this process accepted.
#[must_use]
pub fn peer(address: SocketAddr) -> Peer {
    let address = SocketAddr::new(address.ip().to_canonical(), address.port());
    if !this_machine(address.ip()) {
        return Peer::Elsewhere;
    }
    #[cfg(unix)]
    {
        let pid = match local_client(address) {
            Client::Found(pid) => pid,
            Client::Itself => return Peer::Itself,
            Client::Missing => return Peer::Unknown,
            Client::NoTool => return Peer::Untold,
        };
        judge(pid)
    }
    #[cfg(not(unix))]
    {
        Peer::Untold
    }
}

/// Whether the local program `pid`, at the other end of a connection, runs inside Haarness and for an agent. A helper
/// this process started speaks for others ([`mark_helper`]), and a Haarness program another started for itself is
/// judged by that launcher.
#[cfg(unix)]
fn judge(pid: u32) -> Peer {
    let Some(rows) = table() else {
        return Peer::Unknown;
    };
    let view = View::new(&rows);
    if view.row(pid).is_none() {
        return Peer::Unknown;
    }
    if view.helper(pid, &HELPERS.lock()) {
        return Peer::Program {
            inside: false,
            agent: false,
        };
    }
    let agent = match view.launcher(pid) {
        Some(launcher) => view.launcher_for_agent(launcher),
        None => view.for_agent(pid, 0),
    };
    Peer::Program {
        inside: view.inside(pid),
        agent,
    }
}

/// Whether `pid` runs inside Haarness. An unreadable process table reads as no, and so does every process off Unix.
#[must_use]
pub fn inside_haarness(pid: u32) -> bool {
    #[cfg(unix)]
    {
        table().is_some_and(|rows| View::new(&rows).inside(pid))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Whether this very process runs inside Haarness: started for an agent's command, which names its chat, or as
/// [`inside_haarness`] tells. A process table that cannot be read counts as inside.
#[must_use]
pub fn this_process_inside() -> bool {
    if names_a_chat() {
        return true;
    }
    #[cfg(unix)]
    {
        table().is_none_or(|rows| View::new(&rows).inside(std::process::id()))
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Whether this very process runs for an agent: started for an agent's command, which names its chat, or inside
/// Haarness other than in a terminal a person opened. A process table that cannot be read counts as an agent's.
#[must_use]
pub fn this_process_for_agent() -> bool {
    if names_a_chat() {
        return true;
    }
    #[cfg(unix)]
    {
        table().is_none_or(|rows| View::new(&rows).for_agent(std::process::id(), 0))
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// For a process a Haarness program started for itself, such as the server a launcher leaves running: whether the one
/// that started it runs for an agent, as [`this_process_for_agent`] would tell there. A process whose parent is no
/// Haarness program was not started that way, and counts as an agent's, as does an unreadable process table.
#[must_use]
pub fn parent_for_agent() -> bool {
    if names_a_chat() {
        return true;
    }
    #[cfg(unix)]
    {
        let parent = std::os::unix::process::parent_id();
        table().is_none_or(|rows| View::new(&rows).launcher_for_agent(parent))
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Whether an agent's command runs here: the shell tool names the chat it runs for.
fn names_a_chat() -> bool {
    std::env::var_os(crate::SESSION_ID_ENV).is_some_and(|value| !value.is_empty())
}

/// Whether `ip` is one of this computer's own addresses.
fn this_machine(ip: IpAddr) -> bool {
    if ip.is_loopback() {
        return true;
    }
    #[cfg(unix)]
    {
        nix::ifaddrs::getifaddrs().is_ok_and(|mut addresses| {
            addresses.any(|interface| {
                interface.address.is_some_and(|address| {
                    address
                        .as_sockaddr_in()
                        .map(|address| IpAddr::V4(address.ip()))
                        .or_else(|| {
                            address
                                .as_sockaddr_in6()
                                .map(|address| IpAddr::V6(address.ip()))
                        })
                        == Some(ip)
                })
            })
        })
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// The helpers this process started to carry requests for people elsewhere (see [`mark_helper`]).
#[cfg(unix)]
static HELPERS: std::sync::LazyLock<Mutex<BTreeSet<u32>>> =
    std::sync::LazyLock::new(Default::default);

/// Notes that this process just started `pid` to carry requests for people elsewhere, such as the connector that
/// brings requests from phones and Haarness Cloud, or a scheduled run it was asked to start: what it sends counts as
/// theirs. [`forget_helper`] once it has ended, before its id can come back.
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

/// The shells of the terminals this process opened (see [`mark_terminal`]).
#[cfg(unix)]
static TERMINALS: std::sync::LazyLock<Mutex<BTreeSet<u32>>> =
    std::sync::LazyLock::new(Default::default);

/// Notes that this process just started `pid` as a terminal's shell, for a person; what runs below it is theirs.
/// [`forget_terminal`] once it has ended, before its id can come back.
pub fn mark_terminal(pid: u32) {
    #[cfg(unix)]
    TERMINALS.lock().insert(pid);
    #[cfg(not(unix))]
    let _ = pid;
}

/// Forgets a terminal's shell [`mark_terminal`] noted, once it has ended.
pub fn forget_terminal(pid: u32) {
    #[cfg(unix)]
    TERMINALS.lock().remove(&pid);
    #[cfg(not(unix))]
    let _ = pid;
}

/// How many Haarness programs deep a terminal is judged: one opened in a server a person started in a terminal of
/// another, and so on.
#[cfg(unix)]
const TERMINAL_DEPTH: usize = 8;

/// The process table as this process reads it: with the groups it started for commands, the terminals it opened, and
/// its own pid and program.
#[cfg(unix)]
struct View<'a> {
    rows: &'a [Process],
    started: BTreeSet<i32>,
    terminals: BTreeSet<u32>,
    own_pid: u32,
    own: Option<String>,
}

#[cfg(unix)]
impl<'a> View<'a> {
    fn new(rows: &'a [Process]) -> Self {
        let own_pid = std::process::id();
        Self {
            rows,
            started: STARTED.lock().clone(),
            terminals: TERMINALS.lock().clone(),
            own_pid,
            own: rows
                .iter()
                .find(|row| row.pid == own_pid)
                .map(|row| row.program.clone()),
        }
    }

    fn row(&self, pid: u32) -> Option<&'a Process> {
        self.rows.iter().find(|row| row.pid == pid)
    }

    fn haarness(&self, process: &Process) -> bool {
        is_haarness(&process.program, self.own.as_deref())
    }

    /// Whether `pid` runs inside Haarness: a Haarness program is among its ancestors (`pid` itself does not count),
    /// or it or one of its ancestors runs in a process group Haarness started or a Haarness program leads.
    fn inside(&self, pid: u32) -> bool {
        below_program(self.rows, pid, self.own.as_deref())
            || in_haarness_group(self.rows, pid, &self.started, self.own.as_deref())
    }

    /// Whether `pid` runs inside Haarness other than in a terminal a person opened.
    fn for_agent(&self, pid: u32, depth: usize) -> bool {
        self.inside(pid) && !self.in_person_terminal(pid, depth)
    }

    /// Whether `pid` runs in a terminal a person opened in Haarness. The nearest process at or above it that a
    /// Haarness program started is that terminal's shell: one this process noted ([`mark_terminal`]), or, for another
    /// Haarness program that runs for a person, one leading a session with a terminal of its own. Whatever else a
    /// Haarness program starts (an agent's command, a hook, an MCP or language server, a helper) is no terminal.
    fn in_person_terminal(&self, pid: u32, depth: usize) -> bool {
        let mut current = self.row(pid);
        // A table read while processes come and go could hold a loop.
        for _ in 0..128 {
            let Some(process) = current.filter(|process| process.pid > 1) else {
                return false;
            };
            let Some(parent) = self.row(process.parent).filter(|parent| parent.pid > 1) else {
                return false;
            };
            if self.haarness(parent) {
                return if parent.pid == self.own_pid {
                    self.terminals.contains(&process.pid) && !self.started.contains(&process.group)
                } else {
                    process.session_leader
                        && process.tty
                        && depth < TERMINAL_DEPTH
                        && !self.for_agent(parent.pid, depth + 1)
                };
            }
            current = Some(parent);
        }
        false
    }

    /// Whether `parent`, which started a process for itself, is no Haarness program or runs for an agent.
    fn launcher_for_agent(&self, parent: u32) -> bool {
        let launcher = parent > 1 && self.row(parent).is_some_and(|row| self.haarness(row));
        !launcher || self.for_agent(parent, 0)
    }

    /// The Haarness program, other than this process, that started the Haarness program `pid` for itself, in its own
    /// process group: `haarness schedule run` and the run it starts. Such a program is judged by its launcher.
    fn launcher(&self, pid: u32) -> Option<u32> {
        let process = self.row(pid)?;
        let parent = self
            .row(process.parent)
            .filter(|parent| parent.pid > 1 && parent.pid != self.own_pid)?;
        (self.haarness(process) && self.haarness(parent) && process.group == parent.group)
            .then_some(parent.pid)
    }

    /// Whether `pid` is one of `helpers`, started by this process: still its child, and in no process group it
    /// started for a command.
    fn helper(&self, pid: u32, helpers: &BTreeSet<u32>) -> bool {
        helpers.contains(&pid)
            && self
                .row(pid)
                .is_some_and(|row| row.parent == self.own_pid && !self.started.contains(&row.group))
    }
}

/// The process groups this process started for commands, hooks and language servers. A server stops a command's group
/// when the command ends, a moment after its leader: until then a program in it is known here, with no leader to tell.
#[cfg(unix)]
static STARTED: std::sync::LazyLock<Mutex<BTreeSet<i32>>> =
    std::sync::LazyLock::new(Default::default);

/// Notes that this process just started the process group `leader` leads (see [`crate::process_groups::recorded`]),
/// and forgets the groups no process runs in any more. A note counts only in the process that started the group.
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

/// Whether `pid`, or a process above it, runs in a process group Haarness started (`own` as in [`is_haarness`]): one
/// whose leader is a Haarness program other than `pid` or runs below one, or one of `started` (this process's own)
/// whose leader has ended. A leader that runs elsewhere leads a later group that happens to have the same id: an id is
/// free again only once its group is gone.
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
            .and_then(row);
        let ours = match leader {
            Some(leader) => {
                (leader.pid != pid && is_haarness(&leader.program, own))
                    || below_program(rows, leader.pid, own)
            }
            None => started.contains(&group),
        };
        if ours {
            return true;
        }
        current = row(process.parent);
    }
    false
}

/// What holds the client end of a connection to this process.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Client {
    Found(u32),
    Itself,
    Missing,
    NoTool,
}

/// The process on this computer that holds the client end of a connection from `address`, one of this computer's own.
#[cfg(unix)]
fn local_client(address: SocketAddr) -> Client {
    let own = std::process::id();
    #[cfg(target_os = "macos")]
    if let Some(client) = std::process::Command::new("/usr/sbin/netstat")
        .args(["-anv", "-p", "tcp"])
        .env("LC_ALL", "C")
        .output()
        .ok()
        .and_then(|output| {
            client_in_netstat(&String::from_utf8_lossy(&output.stdout), address, own)
        })
    {
        return client;
    }
    let Some(program) = ["/usr/sbin/lsof", "/usr/bin/lsof"]
        .into_iter()
        .find(|path| Path::new(path).exists())
    else {
        return Client::NoTool;
    };
    std::process::Command::new(program)
        .args(["-nP", "-a", &format!("-iTCP:{}", address.port()), "-Fpn"])
        .env("LC_ALL", "C")
        .output()
        .map_or(Client::Missing, |output| {
            client_in_listing(&String::from_utf8_lossy(&output.stdout), address, own)
        })
}

/// The holder, in `lsof -Fpn` output, of the socket that starts at `address`: the server's own end of the same
/// connection starts at the server's address instead.
#[cfg(unix)]
fn client_in_listing(listing: &str, address: SocketAddr, own: u32) -> Client {
    let start = format!("{address}->");
    let mut pid = None;
    for line in listing.lines() {
        if let Some(number) = line.strip_prefix('p') {
            pid = number.trim().parse::<u32>().ok();
        } else if let Some(name) = line.strip_prefix('n')
            && name.starts_with(&start)
            && let Some(pid) = pid
        {
            return if pid == own {
                Client::Itself
            } else {
                Client::Found(pid)
            };
        }
    }
    Client::Missing
}

/// The holder, in `netstat -anv` output on macOS, of the socket whose local address is `address` (written
/// `127.0.0.1.50123`), from its `process:pid` column. `None` when the listing has no such column (an older macOS).
#[cfg(unix)]
fn client_in_netstat(listing: &str, address: SocketAddr, own: u32) -> Option<Client> {
    let mut lines = listing.lines();
    lines
        .by_ref()
        .find(|line| line.contains("Local Address"))
        .filter(|header| header.contains("process:pid"))?;
    let local = format!("{}.{}", address.ip(), address.port());
    for line in lines {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.get(3) != Some(&local.as_str()) {
            continue;
        }
        // The process's name may hold spaces: its pid ends the first field after the counters that ends in `:<pid>`.
        let pid = fields.iter().skip(10).find_map(|field| {
            let (_, number) = field.rsplit_once(':')?;
            number.parse::<u32>().ok()
        });
        return Some(match pid {
            Some(pid) if pid == own => Client::Itself,
            Some(pid) => Client::Found(pid),
            None => Client::Missing,
        });
    }
    Some(Client::Missing)
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
/// process was started under, which a link or its parent chooses. `ps` is the system's own, whatever the PATH says.
#[cfg(unix)]
fn table() -> Option<Vec<Process>> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-A", "-o", "pid=,ppid=,pgid=,stat=,tty=,ucomm="])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_table(&String::from_utf8_lossy(&output.stdout)))
        .filter(|rows| !rows.is_empty())
}

/// `ps -o pid=,ppid=,pgid=,stat=,tty=,ucomm=` lines: the state's letters, the terminal (`??` or `?` for none), then
/// the program's file name, cut to 16 bytes on macOS and 15 on Linux, which may hold spaces and is padded with them.
/// A path, as `comm` gives on macOS, reads as its file name.
#[cfg(unix)]
fn parse_table(text: &str) -> Vec<Process> {
    text.lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (parent, rest) = rest.trim_start().split_once(char::is_whitespace)?;
            let (group, rest) = rest.trim_start().split_once(char::is_whitespace)?;
            let (state, rest) = rest.trim_start().split_once(char::is_whitespace)?;
            let (tty, command) = rest.trim_start().split_once(char::is_whitespace)?;
            let program = Path::new(command.trim())
                .file_name()?
                .to_string_lossy()
                .into_owned();
            Some(Process {
                pid: pid.parse().ok()?,
                parent: parent.parse().ok()?,
                group: group.parse().ok()?,
                session_leader: state.contains('s'),
                tty: !matches!(tty, "?" | "??" | "-"),
                program,
            })
        })
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    // Terminal's zsh (501) runs the terminal UI (502), which runs an agent's command (503). Desktop's server (602), a
    // daemon leading its own group, runs an agent's command (603) in a group of its own; 610 is a program that command
    // put in the background, which the system handed to launchd, and 611 one that program started in a session of its
    // own. Desktop's terminal panel (640) is a shell the server started in a session with a terminal of its own, where
    // the person runs the terminal UI (641) and curl (642). A hook (650) runs in a group of its own; 612 is what a
    // program the server started without a group of its own left in the server's group. The table, and every test
    // reading one, is Unix's: Windows has no `ps` table, and nothing there reads as inside. Program names are as `ps`
    // prints `ucomm`, cut to 16 bytes on macOS (where it pads them with spaces, too); an `s` in the state marks a
    // session's leader, and `??` a process with no terminal.
    const TABLE: &str = "    1     0     1 Ss   ??       launchd
  500     1   500 S    ??       Terminal
  501   500   501 Ss   ttys000  zsh
  502   501   502 S+   ttys000  haarness
  503   502   503 S    ttys000  zsh
  504   503   503 S    ttys000  curl
  600     1   600 S    ??       Haarness
  601   600   600 S    ??       Haarness Helper
  602     1   602 Ss   ??       haarness
  603   602   603 S    ??       sh
  604   603   603 S    ??       python3
  610     1   603 S    ??       curl
  611   610   611 Ss   ??       sh
  612     1   602 S    ??       python3
  620     1   620 S    ??       curl
  640   602   640 Ss   ttys003  zsh
  641   640   641 S+   ttys003  haarness
  642   640   642 S+   ttys003  curl
  650   602   650 S    ??       sh
";

    /// [`TABLE`] with each `haarness` program run from a file called `program` instead.
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

    /// The table as `own_pid` reads it, having started the groups `started` and the terminals `terminals`.
    fn view<'a>(rows: &'a [Process], own_pid: u32, started: &[i32], terminals: &[u32]) -> View<'a> {
        View {
            rows,
            started: started.iter().copied().collect(),
            terminals: terminals.iter().copied().collect(),
            own_pid,
            own: None,
        }
    }

    #[test]
    fn the_table_reads_names_sessions_and_terminals() {
        let rows = parse_table(TABLE);
        assert_eq!(rows.len(), 19);
        assert_eq!(rows[3].program, "haarness");
        assert_eq!(rows[6].program, "Haarness");
        assert_eq!(rows[7].program, "Haarness Helper");
        assert_eq!(rows[11].group, 603);
        assert!(rows[0].session_leader && !rows[1].session_leader);
        assert!(rows[2].tty && !rows[1].tty);
        // Padding is not part of a name, and a path, as `comm` gives on macOS, names its file. Linux writes `?` for
        // no terminal.
        let padded = parse_table(
            "  602   600   602 S    ??       haarness        \n  601   600   600 S    pts/1    Haarness Helper \n  9 1 9 S ? x\n",
        );
        assert_eq!(padded[0].program, "haarness");
        assert_eq!(padded[1].program, "Haarness Helper");
        assert!(!padded[0].tty && padded[1].tty && !padded[2].tty);
        let path = parse_table(
            "  602   600   602 S    ??       /Applications/Haarness.app/Contents/Resources/bin/haarness\n",
        );
        assert_eq!(path[0].program, "haarness");
    }

    #[test]
    fn a_process_started_by_haarness_is_inside_it_and_others_are_not() {
        let rows = parse_table(TABLE);
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

    #[test]
    fn a_haarness_run_from_a_renamed_file_is_still_haarness() {
        let none = BTreeSet::new();
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
            assert!(in_haarness_group(&rows, 610, &none, None), "{program}");
            assert!(!below_program(&rows, 501, None), "{program}");
            assert!(!in_haarness_group(&rows, 620, &none, None), "{program}");
        }
        // Any other name, when the program asking runs from a file of that name: `haarness mac` run by a command of
        // a server both started from the same copy, or that server about a request.
        let rows = renamed("hn");
        assert!(!below_program(&rows, 604, None));
        assert!(!below_program(&rows, 604, Some("haarness")));
        assert!(!in_haarness_group(&rows, 610, &none, Some("haarness")));
        assert!(below_program(&rows, 604, Some("hn")));
        assert!(below_program(&rows, 504, Some("hn")));
        assert!(in_haarness_group(&rows, 610, &none, Some("hn")));
        assert!(in_haarness_group(&rows, 611, &none, Some("hn")));
        // The person's own programs stay outside, Desktop's windows included.
        for pid in [501, 502, 600, 601, 610, 620] {
            assert!(!below_program(&rows, pid, Some("hn")), "{pid}");
        }
        for pid in [501, 502, 600, 601, 620] {
            assert!(!in_haarness_group(&rows, pid, &none, Some("hn")), "{pid}");
        }
        // Names that only start like Haarness's, or differ in case, are other programs.
        for program in ["haarnessd", "Haarness", "haarnes", "xhaarness"] {
            let rows = renamed(program);
            assert!(!below_program(&rows, 604, None), "{program}");
            assert!(!in_haarness_group(&rows, 610, &none, None), "{program}");
        }
    }

    #[test]
    fn a_program_that_left_the_tree_is_known_by_its_group() {
        let rows = parse_table(TABLE);
        let none = BTreeSet::new();
        // Put in the background by the command while it runs, and a program it started in a session of its own.
        assert!(in_haarness_group(&rows, 610, &none, None));
        assert!(in_haarness_group(&rows, 611, &none, None));
        // Still attached: the group says so as well as the tree.
        assert!(in_haarness_group(&rows, 604, &none, None));
        // Left in the group Desktop's server leads.
        assert!(in_haarness_group(&rows, 612, &none, None));
        // The person's own programs: Terminal's shells, the terminal UI (which leads its own group), Desktop, another
        // curl; and the server, which leads its own.
        for pid in [501, 502, 600, 601, 602, 620] {
            assert!(!in_haarness_group(&rows, pid, &none, None), "{pid}");
        }
        // A group of the same id led by a program outside Haarness is a later one.
        assert!(!in_haarness_group(&rows, 620, &BTreeSet::from([620]), None));
    }

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

    #[test]
    fn a_terminal_is_a_persons_only_when_its_server_opened_it_for_them() {
        let rows = parse_table(TABLE);
        // Desktop's server, which noted the shell it started for its terminal panel.
        let server = view(&rows, 602, &[603, 650], &[640]);
        for pid in [640, 641, 642] {
            assert!(server.inside(pid), "{pid}");
            assert!(server.in_person_terminal(pid, 0), "{pid}");
            assert!(!server.for_agent(pid, 0), "{pid}");
        }
        // Not noted, the same shell is not a terminal: a hook or a program that made itself lead a session.
        let unnoted = view(&rows, 602, &[603, 650], &[]);
        assert!(unnoted.for_agent(641, 0));
        // An agent's command and what it runs or leaves running, a hook, and what was left in the server's group.
        for pid in [503, 504, 603, 604, 610, 611, 612, 650] {
            assert!(!server.in_person_terminal(pid, 0), "{pid}");
            assert!(server.for_agent(pid, 0), "{pid}");
        }
        // The person's own programs outside Haarness are not inside to begin with.
        for pid in [501, 502, 600, 620] {
            assert!(!server.for_agent(pid, 0), "{pid}");
        }
        // Another process (a command line run in the terminal) knows the terminal by its shell: it leads a session
        // with a terminal of its own, and the server that started it runs for a person.
        let other = view(&rows, 642, &[], &[]);
        assert!(!other.for_agent(642, 0));
        assert!(!other.for_agent(641, 0));
        // A session with no terminal is none, nor is a shell's session below an agent's command.
        let rows = parse_table(&format!(
            "{TABLE}  651   602   651 Ss   ??       sh\n  652   651   651 S    ??       curl\n  653   604   653 Ss   ttys009  sh\n  654   653   653 S    ttys009  curl\n"
        ));
        let other = view(&rows, 999, &[], &[]);
        assert!(other.for_agent(652, 0));
        assert!(other.for_agent(654, 0));
        // A terminal of a server the person started in Desktop's terminal panel is theirs; one the Haarness an agent's
        // command runs opened is not.
        let rows = parse_table(&format!(
            "{TABLE}  670   640   670 S+   ttys003  haarness\n  671   670   671 Ss   ttys005  zsh\n  672   671   672 S    ttys005  curl\n  680   603   603 S    ??       haarness\n  681   680   681 Ss   ttys006  zsh\n  682   681   682 S    ttys006  curl\n"
        ));
        let server = view(&rows, 602, &[603], &[640]);
        assert!(!server.for_agent(672, 0));
        assert!(server.for_agent(682, 0));
    }

    #[test]
    fn a_launcher_is_judged_by_where_it_runs() {
        let rows = parse_table(TABLE);
        let server = view(&rows, 602, &[603], &[640]);
        // The terminal UI the person started, Desktop's server, and the terminal UI in Desktop's terminal panel start
        // their own servers.
        for parent in [502, 602, 641] {
            assert!(!server.launcher_for_agent(parent), "{parent}");
        }
        // A command's shell or program is no Haarness program; neither is a process that is gone.
        for parent in [503, 603, 604, 999, 1] {
            assert!(server.launcher_for_agent(parent), "{parent}");
        }
        // A Haarness program an agent's command runs is the agent's.
        let rows = parse_table(&format!(
            "{TABLE}  605   603   603 S    ??       haarness\n"
        ));
        assert!(view(&rows, 602, &[603], &[]).launcher_for_agent(605));
    }

    #[test]
    fn a_haarness_program_another_started_for_itself_is_judged_by_it() {
        // `haarness schedule run` in Terminal (659) and the run it started (660), in its group; the server's own run
        // (661); and a run below an agent's command (662, 663).
        let rows = parse_table(&format!(
            "{TABLE}  659   501   659 S+   ttys000  haarness\n  660   659   659 S+   ttys000  haarness\n  661   602   602 S    ??       haarness\n  662   603   603 S    ??       haarness\n  663   662   603 S    ??       haarness\n"
        ));
        let server = view(&rows, 602, &[603], &[640]);
        assert_eq!(server.launcher(660), Some(659));
        assert!(!server.launcher_for_agent(659));
        // The server's own run is judged as a helper it noted, not by the server.
        assert_eq!(server.launcher(661), None);
        assert_eq!(server.launcher(663), Some(662));
        assert!(server.launcher_for_agent(662));
        // A program in a group of its own is judged by itself.
        assert_eq!(server.launcher(641), None);
    }

    #[test]
    fn only_a_helper_this_process_started_outside_any_command_speaks_for_others() {
        // Desktop's server (602) started a connector (630) in a group of its own, and runs a command (603).
        let rows = parse_table(&format!(
            "{TABLE}  630   602   630 S    ??       haarness\n  631   603   603 S    ??       haarness\n"
        ));
        let helpers = BTreeSet::from([630, 631]);
        let server = view(&rows, 602, &[603], &[]);
        assert!(server.helper(630, &helpers));
        // Not one this process noted, not its child, or in a group it started for a command.
        assert!(!server.helper(630, &BTreeSet::new()));
        assert!(!view(&rows, 502, &[603], &[]).helper(630, &helpers));
        assert!(!view(&rows, 602, &[603, 630], &[]).helper(630, &helpers));
        assert!(!server.helper(631, &helpers));
        assert!(!server.helper(999, &BTreeSet::from([999])));
    }

    #[test]
    fn a_loop_in_the_table_ends() {
        let rows = parse_table("  10    11    10 S ?? a\n  11    10    11 S ?? b\n");
        assert!(!below_program(&rows, 10, None));
        assert!(!in_haarness_group(&rows, 10, &BTreeSet::from([99]), None));
        assert!(!view(&rows, 10, &[], &[]).in_person_terminal(10, 0));
    }

    #[test]
    fn the_client_end_of_a_connection_is_the_one_starting_at_its_address() {
        let peer: SocketAddr = "127.0.0.1:50123".parse().unwrap();
        let listing = "p4242\nf9\nn127.0.0.1:4096->127.0.0.1:50123\np777\nf3\nn127.0.0.1:50123->127.0.0.1:4096\n";
        assert_eq!(client_in_listing(listing, peer, 4242), Client::Found(777));
        assert_eq!(client_in_listing(listing, peer, 777), Client::Itself);
        assert_eq!(
            client_in_listing("p777\nf3\nn127.0.0.1:4096->127.0.0.1:50124\n", peer, 1),
            Client::Missing
        );
        let six: SocketAddr = "[::1]:50123".parse().unwrap();
        assert_eq!(
            client_in_listing("p88\nf4\nn[::1]:50123->[::1]:4096\n", six, 1),
            Client::Found(88)
        );
        // macOS's netstat names the holder in a column of its own, and a program's name may hold spaces.
        let netstat = "Active Internet connections (including servers)
Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)          rxbytes      txbytes  rhiwat  shiwat          process:pid    state  options
tcp4       0      0  127.0.0.1.4096         127.0.0.1.50123        ESTABLISHED            0            0  408300  146988       haarness:4242  00002 00000000
tcp4       0      0  127.0.0.1.50123        127.0.0.1.4096         ESTABLISHED            0            0  408300  146988  Google Chrome H:777  00002 00000000
tcp6       0      0  ::1.50123              ::1.4096               ESTABLISHED            0            0  408300  146988           curl:88  00002 00000000
";
        assert_eq!(
            client_in_netstat(netstat, peer, 4242),
            Some(Client::Found(777))
        );
        assert_eq!(client_in_netstat(netstat, peer, 777), Some(Client::Itself));
        assert_eq!(client_in_netstat(netstat, six, 1), Some(Client::Found(88)));
        let other: SocketAddr = "127.0.0.1:50999".parse().unwrap();
        assert_eq!(client_in_netstat(netstat, other, 1), Some(Client::Missing));
        // Without the column, netstat cannot tell.
        assert_eq!(
            client_in_netstat(
                "Proto Recv-Q Send-Q  Local Address  Foreign Address  (state)\n",
                peer,
                1
            ),
            None
        );
    }

    #[test]
    fn this_computer_is_known_by_its_own_addresses() {
        assert!(this_machine("127.0.0.1".parse().unwrap()));
        assert!(this_machine("::1".parse().unwrap()));
        assert!(this_machine(
            "::ffff:127.0.0.1".parse::<IpAddr>().unwrap().to_canonical()
        ));
        assert!(!this_machine("192.0.2.1".parse().unwrap()));
        // Every address of this computer's own interfaces.
        for interface in nix::ifaddrs::getifaddrs().unwrap() {
            if let Some(address) = interface.address.and_then(|address| {
                address
                    .as_sockaddr_in()
                    .map(nix::sys::socket::SockaddrIn::ip)
            }) {
                assert!(this_machine(IpAddr::V4(address)), "{address}");
            }
        }
    }

    /// The real thing: a command in a group of its own starts a program in the background and ends. Until its group is
    /// stopped the program runs under launchd (or init), outside this process's tree and with no leader, and is still
    /// known by its group; a program left the same way by a group this process did not note is not. A program this
    /// process started itself is inside whatever this test program's file is called: the process asking is Haarness.
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

    /// The real thing: of programs this process (a Haarness program, running for a person) started, a helper it noted
    /// speaks for others and a terminal's shell it noted is the person's, until forgotten; others are an agent's,
    /// one that made itself lead a session included.
    #[test]
    fn only_what_this_process_noted_speaks_for_a_person() {
        let mut helper = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let mut session = std::process::Command::new("python3")
            .args([
                "-c",
                "import os; os.setsid(); os.execvp('sleep', ['sleep', '30'])",
            ])
            .spawn()
            .unwrap();
        let agent = |verdict: Peer| matches!(verdict, Peer::Program { agent: true, .. });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        // Until the session is set up the table may show the program as it started.
        while table()
            .and_then(|rows| rows.into_iter().find(|row| row.pid == session.id()))
            .is_none_or(|row| !row.session_leader)
        {
            assert!(std::time::Instant::now() < deadline, "no session");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let before = (agent(judge(helper.id())), agent(judge(session.id())));
        mark_helper(helper.id());
        mark_terminal(session.id());
        let noted = (judge(helper.id()), agent(judge(session.id())));
        forget_helper(helper.id());
        forget_terminal(session.id());
        let forgotten = (agent(judge(helper.id())), agent(judge(session.id())));
        let own = this_process_for_agent();
        for child in [&mut helper, &mut session] {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert_eq!(before, (true, true));
        assert_eq!(
            noted,
            (
                Peer::Program {
                    inside: false,
                    agent: false
                },
                false
            )
        );
        assert_eq!(forgotten, (true, true));
        assert!(!own);
    }

    /// The real thing: a connection from a program this process started is placed, by its IPv4 address or the same
    /// address written as IPv6, and one from this process itself reads as its own.
    #[test]
    fn a_connection_is_placed_by_the_program_holding_it() {
        use std::io::Read as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut child = std::process::Command::new("python3")
            .args([
                "-c",
                &format!(
                    "import socket,time; s=socket.create_connection(('127.0.0.1',{port})); time.sleep(30)"
                ),
            ])
            .spawn()
            .unwrap();
        let (mut accepted, address) = listener.accept().unwrap();
        let mapped = SocketAddr::new(
            match address.ip() {
                IpAddr::V4(ip) => IpAddr::V6(ip.to_ipv6_mapped()),
                ip @ IpAddr::V6(_) => ip,
            },
            address.port(),
        );
        let placed = (peer(address), peer(mapped));
        let own = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_mine, own_address) = listener.accept().unwrap();
        let itself = peer(own_address);
        let _ = child.kill();
        let _ = child.wait();
        let mut rest = Vec::new();
        let _ = accepted.set_read_timeout(Some(std::time::Duration::from_millis(10)));
        let _ = accepted.read_to_end(&mut rest);
        drop(own);
        let program = Peer::Program {
            inside: true,
            agent: true,
        };
        assert_eq!(placed, (program, program));
        assert_eq!(itself, Peer::Itself);
        assert_eq!(peer("192.0.2.1:50123".parse().unwrap()), Peer::Elsewhere);
    }
}
