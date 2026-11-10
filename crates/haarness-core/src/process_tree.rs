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
//! named otherwise, to a `haarness mac` run from yet another copy. Desktop is the person's app, not a Haarness program,
//! on Linux too, where its file is `haarness-desktop`. Of the Haarness programs a person runs, a server is one whose
//! runtime record names it and the port it listens on ([`note_server_records`]).
//!
//! Three marks tell inside: the process's ancestry; the process group Haarness starts each command, hook and language
//! server in; and a process group a Haarness program leads. A program that left the tree (put in the background with
//! `nohup` or `( … & )`, or left by a process that exited) still runs in its group. What escapes them is a program
//! that moves to a group or session of its own once the process that started it is gone, one launchd or another app
//! starts for it, and what a relay a person runs (a proxy, a port forward, a tunnel) carries, which reaches a server as
//! the relay's own: this guards the routes an agent would take, not a determined program.
//!
//! A terminal a person opened is one this process noted when it started its shell ([`mark_terminal`]), or, started by
//! another Haarness program that runs for a person, a shell leading a session with a terminal of its own.
//!
//! This reads the process table (`/bin/ps`), and finds the processes holding a local connection with `lsof`, whose
//! process ids are exact: a program's name never enters it. Unix only: elsewhere no process reads as inside, and where
//! `lsof` is missing (some Linux systems) a connection's program cannot be told ([`Peer::Untold`]).

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

/// The file Desktop runs from on Linux: the person's app, as Desktop is elsewhere (`Haarness`).
#[cfg(unix)]
const DESKTOP: &str = "haarness-desktop";

/// Whether `program`, the name of the file the system ran for a process, is a Haarness program: `haarness`, a copy of
/// it under a longer name (`haarness-test`, `haarness_old`, `haarness.new`), or `own`, the program of the process
/// asking, which is Haarness whatever its file is called. Desktop's file is not one, nor the 15 bytes of it Linux
/// gives (`haarness-deskto`).
#[cfg(unix)]
fn is_haarness(program: &str, own: Option<&str>) -> bool {
    let desktop = program.len() >= 15 && DESKTOP.starts_with(program);
    (!desktop
        && program
            .strip_prefix(PROGRAM)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '_', '.'])))
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
    /// Another Haarness server on this computer, running for a person, that its runtime record names: what it sends,
    /// it sends for someone it heard from (a request it forwards).
    Server,
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
        let Some(holders) = holders(address) else {
            return Peer::Untold;
        };
        let own = std::process::id();
        match holders.as_slice() {
            [] => Peer::Unknown,
            [only] if *only == own => Peer::Itself,
            // Shared with this process, which cannot speak for the others.
            _ if holders.contains(&own) => Peer::Unknown,
            _ => judge(&holders),
        }
    }
    #[cfg(not(unix))]
    {
        Peer::Untold
    }
}

/// Whether the local programs `pids`, which hold the other end of a connection, run inside Haarness and for an agent,
/// judged by the strictest of them. A helper this process started speaks for others ([`mark_helper`]), and a Haarness
/// program another started for itself is judged by that launcher.
#[cfg(unix)]
fn judge(pids: &[u32]) -> Peer {
    let Some(rows) = table() else {
        return Peer::Unknown;
    };
    let view = View::new(&rows);
    let helpers = HELPERS.lock().clone();
    let (mut inside, mut agent, mut server) = (false, false, false);
    for &pid in pids {
        let Some(row) = view.row(pid) else {
            return Peer::Unknown;
        };
        if view.helper(pid, &helpers) {
            continue;
        }
        let within = view.inside(pid);
        inside |= within;
        agent |= match view.launcher(pid) {
            Some(launcher) => view.launcher_for_agent(launcher),
            None => view.for_agent(pid, 0),
        };
        // A server of a person's may be carrying someone else's request.
        server |= !within && view.haarness(row) && serves(pid);
    }
    if server && !agent {
        return Peer::Server;
    }
    Peer::Program { inside, agent }
}

/// Where the servers of this process's Haarness leave their runtime records (see [`note_server_records`]).
#[cfg(unix)]
static RECORDS: std::sync::LazyLock<Mutex<Option<std::path::PathBuf>>> =
    std::sync::LazyLock::new(Default::default);

/// Notes `cache`, the folder where each server of this process's Haarness leaves its runtime record (`runtime.json`,
/// there or in a project's folder in `workspaces`), which names the server's process and address. A Haarness program
/// of a person's is taken for a server ([`Peer::Server`]) only when a record names it and the port it listens on.
pub fn note_server_records(cache: &Path) {
    #[cfg(unix)]
    {
        *RECORDS.lock() = Some(cache.to_path_buf());
    }
    #[cfg(not(unix))]
    let _ = cache;
}

/// Whether `pid` is a server: a runtime record names it ([`note_server_records`]) and it listens on the record's port,
/// or, once a record names it, its listeners cannot be told in time.
#[cfg(unix)]
fn serves(pid: u32) -> bool {
    let Some(cache) = RECORDS.lock().clone() else {
        return false;
    };
    let ports = recorded_ports(&cache, pid);
    if ports.is_empty() {
        return false;
    }
    let Some(program) = lsof() else {
        return true;
    };
    let mut command = std::process::Command::new(program);
    command.args([
        "-nP",
        "-a",
        "-p",
        &pid.to_string(),
        "-iTCP",
        "-sTCP:LISTEN",
        "-Fn",
    ]);
    output_within(command, LOOKUP_TIME).is_none_or(|output| {
        listening_ports(&String::from_utf8_lossy(&output.stdout))
            .iter()
            .any(|port| ports.contains(port))
    })
}

/// The ports the runtime records in `cache` give for the server `pid`.
#[cfg(unix)]
fn recorded_ports(cache: &Path, pid: u32) -> Vec<u16> {
    let projects = std::fs::read_dir(cache.join("workspaces"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().join("runtime.json"));
    std::iter::once(cache.join("runtime.json"))
        .chain(projects)
        .filter_map(|file| {
            let record: serde_json::Value =
                serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
            if record["pid"].as_u64()? != u64::from(pid) {
                return None;
            }
            url::Url::parse(record["url"].as_str()?)
                .ok()?
                .port_or_known_default()
        })
        .collect()
}

/// The ports in `lsof -Fn` output of listening sockets (`n127.0.0.1:4096`, `n*:4096`, `n[::1]:4096`).
#[cfg(unix)]
fn listening_ports(listing: &str) -> Vec<u16> {
    listing
        .lines()
        .filter_map(|line| line.strip_prefix('n')?.rsplit_once(':')?.1.parse().ok())
        .collect()
}

/// Whether a Haarness program, this process included, listens on `address`'s port when `address` is one of this
/// computer's own: a connection there reaches a Haarness server. Also so when the listeners cannot be told.
#[must_use]
pub fn haarness_listens(address: SocketAddr) -> bool {
    let address = SocketAddr::new(address.ip().to_canonical(), address.port());
    if !this_machine(address.ip()) {
        return false;
    }
    #[cfg(unix)]
    {
        let Some(program) = lsof() else {
            return false;
        };
        let mut command = std::process::Command::new(program);
        command.args([
            "-nP",
            "-a",
            &format!("-iTCP:{}", address.port()),
            "-sTCP:LISTEN",
            "-Fp",
        ]);
        let Some(output) = output_within(command, LOOKUP_TIME) else {
            return true;
        };
        let listeners: Vec<u32> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix('p')?.trim().parse().ok())
            .collect();
        if listeners.is_empty() {
            return false;
        }
        let own = std::process::id();
        table().is_none_or(|rows| {
            let view = View::new(&rows);
            listeners
                .iter()
                .any(|pid| *pid == own || view.row(*pid).is_none_or(|row| view.haarness(row)))
        })
    }
    #[cfg(not(unix))]
    {
        false
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

/// How long `lsof` or `ps` may take before the answer counts as unknown (a stale mount can hang `lsof`).
#[cfg(unix)]
const LOOKUP_TIME: std::time::Duration = std::time::Duration::from_secs(5);

/// The system's `lsof`, if it has one.
#[cfg(unix)]
fn lsof() -> Option<&'static str> {
    ["/usr/sbin/lsof", "/usr/bin/lsof"]
        .into_iter()
        .find(|path| Path::new(path).exists())
}

/// `command`'s output, if it ends within `limit`; it is stopped otherwise.
#[cfg(unix)]
fn output_within(
    mut command: std::process::Command,
    limit: std::time::Duration,
) -> Option<std::process::Output> {
    use std::io::Read as _;
    command
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let deadline = std::time::Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Some(std::process::Output {
                    status,
                    stdout: reader.join().ok()?,
                    stderr: Vec::new(),
                });
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// The processes on this computer that hold the client end of a connection from `address`, one of this computer's
/// own; `None` when they cannot be told, for want of `lsof`. One that did not answer in time finds none.
#[cfg(unix)]
fn holders(address: SocketAddr) -> Option<Vec<u32>> {
    let mut command = std::process::Command::new(lsof()?);
    command.args(["-nP", "-a", &format!("-iTCP:{}", address.port()), "-Fpn"]);
    Some(
        output_within(command, LOOKUP_TIME).map_or_else(Vec::new, |output| {
            holders_in_listing(&String::from_utf8_lossy(&output.stdout), address)
        }),
    )
}

/// The processes, in `lsof -Fpn` output, that hold the socket starting at `address`: the server's own end of the same
/// connection starts at the server's address instead. A socket a program handed down to another is held by both.
#[cfg(unix)]
fn holders_in_listing(listing: &str, address: SocketAddr) -> Vec<u32> {
    let start = format!("{address}->");
    let mut pid = None;
    let mut holders = Vec::new();
    for line in listing.lines() {
        if let Some(number) = line.strip_prefix('p') {
            pid = number.trim().parse::<u32>().ok();
        } else if let Some(name) = line.strip_prefix('n')
            && name.starts_with(&start)
            && let Some(pid) = pid
            && !holders.contains(&pid)
        {
            holders.push(pid);
        }
    }
    holders
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
    let mut command = std::process::Command::new("/bin/ps");
    command.args(["-A", "-o", "pid=,ppid=,pgid=,stat=,tty=,ucomm="]);
    let output = output_within(command, LOOKUP_TIME)?;
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

    /// What `look` finds, looked for again while it reads as unknown: on a busy computer (a whole test run starting
    /// processes at once) `ps` or `lsof` may take longer than a lookup waits, which tells nothing, and a client sends
    /// such a request once more.
    fn settled(look: impl Fn() -> Peer) -> Peer {
        let mut found = look();
        for _ in 0..3 {
            if found != Peer::Unknown {
                break;
            }
            found = look();
        }
        found
    }

    /// [`inside_haarness`], from a process table that could be read.
    fn inside_once_read(pid: u32) -> bool {
        (0..4)
            .find_map(|_| table().map(|rows| View::new(&rows).inside(pid)))
            .unwrap_or(false)
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
        // Names that only start like Haarness's, or differ in case, are other programs, and so is Desktop's file on
        // Linux, whole or cut to 15 bytes.
        for program in [
            "haarnessd",
            "Haarness",
            "haarnes",
            "xhaarness",
            "haarness-desktop",
            "haarness-deskto",
        ] {
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
    fn the_holders_of_a_connection_are_those_of_the_socket_starting_at_its_address() {
        let peer: SocketAddr = "127.0.0.1:50123".parse().unwrap();
        let listing = "p4242\nf9\nn127.0.0.1:4096->127.0.0.1:50123\np777\nf3\nn127.0.0.1:50123->127.0.0.1:4096\n";
        assert_eq!(holders_in_listing(listing, peer), [777]);
        assert!(
            holders_in_listing("p777\nf3\nn127.0.0.1:4096->127.0.0.1:50124\n", peer).is_empty()
        );
        let six: SocketAddr = "[::1]:50123".parse().unwrap();
        assert_eq!(
            holders_in_listing("p88\nf4\nn[::1]:50123->[::1]:4096\n", six),
            [88]
        );
        // A socket a program handed down is held by both, each named once.
        let shared = "p10\nf3\nn127.0.0.1:50123->127.0.0.1:4096\nf4\nn127.0.0.1:50123->127.0.0.1:4096\np11\nf3\nn127.0.0.1:50123->127.0.0.1:4096\n";
        assert_eq!(holders_in_listing(shared, peer), [10, 11]);
    }

    #[test]
    fn a_server_is_known_by_its_runtime_record() {
        let folder =
            std::env::temp_dir().join(format!("haarness-process-tree-{}", uuid::Uuid::now_v7()));
        let project = folder.join("workspaces/01a1");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            folder.join("runtime.json"),
            r#"{"url":"http://127.0.0.1:4096","token":"t","pid":42}"#,
        )
        .unwrap();
        std::fs::write(
            project.join("runtime.json"),
            r#"{"url":"http://[::1]:50200","token":"t","pid":42}"#,
        )
        .unwrap();
        std::fs::create_dir_all(folder.join("workspaces/01a2")).unwrap();
        std::fs::write(folder.join("workspaces/01a2/runtime.json"), "{").unwrap();
        let mut found = recorded_ports(&folder, 42);
        found.sort_unstable();
        let other = recorded_ports(&folder, 43);
        let missing = recorded_ports(&folder.join("missing"), 42);
        let _ = std::fs::remove_dir_all(&folder);
        assert_eq!(found, [4096, 50200]);
        assert!(other.is_empty() && missing.is_empty());
        assert_eq!(
            listening_ports("p42\nf7\nn127.0.0.1:4096\nf8\nn*:50200\nf9\nn[::1]:50201\n"),
            [4096, 50200, 50201]
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
            inside_once_read(left),
            inside_once_read(stray),
            inside_once_read(child.id()),
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
        let judged = |pid: u32| settled(|| judge(&[pid]));
        let before = (agent(judged(helper.id())), agent(judged(session.id())));
        mark_helper(helper.id());
        mark_terminal(session.id());
        let noted = (judged(helper.id()), agent(judged(session.id())));
        forget_helper(helper.id());
        forget_terminal(session.id());
        let forgotten = (agent(judged(helper.id())), agent(judged(session.id())));
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
        let placed = (settled(|| peer(address)), settled(|| peer(mapped)));
        let own = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_mine, own_address) = listener.accept().unwrap();
        let itself = settled(|| peer(own_address));
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
        // A Haarness program (this test) listens there; nothing listens on a port just let go.
        assert!(haarness_listens(listener.local_addr().unwrap()));
        let free = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        assert!(!haarness_listens(free));
        assert!(!haarness_listens("192.0.2.1:80".parse().unwrap()));
    }

    /// What [`a_program_named_like_a_process_id_is_placed_by_its_own`] runs from a copy of this test program: it
    /// connects to the port it is given and hands the socket down to a `sleep`, which then holds it too.
    #[test]
    #[ignore = "run by another test"]
    fn connect_and_hand_down() {
        let Ok(port) = std::env::var("HAARNESS_TEST_CONNECT_PORT") else {
            return;
        };
        let stream =
            std::net::TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
        let handed = std::os::fd::OwnedFd::from(stream.try_clone().unwrap());
        let mut sleep = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::from(handed))
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
        drop(stream);
        let _ = sleep.kill();
        let _ = sleep.wait();
    }

    /// What [`a_person_program_is_taken_for_a_server_only_by_its_record`] runs from links to this test program: it
    /// listens, writes its pid and port to the file it is given, and connects to the port it is given.
    #[test]
    #[ignore = "run by another test"]
    fn listen_and_connect() {
        let (Ok(port), Ok(report)) = (
            std::env::var("HAARNESS_TEST_CONNECT_PORT"),
            std::env::var("HAARNESS_TEST_REPORT"),
        ) else {
            return;
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let written = format!(
            "{} {}",
            std::process::id(),
            listener.local_addr().unwrap().port()
        );
        std::fs::write(&report, written).unwrap();
        let _stream =
            std::net::TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    /// The real thing: a person's program outside Haarness that listens is a person's, Desktop's file on Linux
    /// included, until a runtime record names it and its port, and then only if it is a Haarness program.
    #[test]
    fn a_person_program_is_taken_for_a_server_only_by_its_record() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let folder =
            std::env::temp_dir().join(format!("haarness-process-tree-{}", uuid::Uuid::now_v7()));
        let cache = folder.join("cache");
        std::fs::create_dir_all(cache.join("workspaces/01a1")).unwrap();
        // Started from a file of `name`, outside this process's tree and in a session of its own, as a person's
        // program is; its pid, the port it listens on, the address it connected from, and the connection.
        let start = |name: &str| {
            let file = folder.join(name);
            let test = std::env::current_exe().unwrap();
            if std::fs::hard_link(&test, &file).is_err() {
                std::fs::copy(&test, &file).unwrap();
            }
            let report = folder.join(format!("{name}.report"));
            let detached = std::process::Command::new("python3")
                .args([
                    "-c",
                    "import os, sys\nif os.fork() == 0:\n    os.setsid()\n    if os.fork() == 0:\n        os.execv(sys.argv[1], sys.argv[1:])\n    os._exit(0)\nos.wait()",
                ])
                .arg(&file)
                .args([
                    "--ignored",
                    "--exact",
                    "process_tree::tests::listen_and_connect",
                ])
                .env("HAARNESS_TEST_CONNECT_PORT", port.to_string())
                .env("HAARNESS_TEST_REPORT", &report)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(detached.success());
            let (accepted, address) = listener.accept().unwrap();
            let report = std::fs::read_to_string(&report).unwrap();
            let (pid, listening) = report.split_once(' ').unwrap();
            (
                pid.parse::<u32>().unwrap(),
                listening.parse::<u16>().unwrap(),
                address,
                accepted,
            )
        };
        let desktop = start("haarness-desktop");
        let copy = start("haarness-relay");
        note_server_records(&cache);
        let record = |pid: u32, port: u16| {
            std::fs::write(
                cache.join("workspaces/01a1/runtime.json"),
                format!(r#"{{"url":"http://127.0.0.1:{port}","token":"t","pid":{pid}}}"#),
            )
            .unwrap();
        };
        let unrecorded = (settled(|| peer(desktop.2)), settled(|| peer(copy.2)));
        record(copy.0, copy.1.wrapping_add(1));
        let other_port = settled(|| peer(copy.2));
        record(copy.0, copy.1);
        let recorded = settled(|| peer(copy.2));
        record(desktop.0, desktop.1);
        let desktop_recorded = settled(|| peer(desktop.2));
        for pid in [desktop.0, copy.0] {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let _ = std::fs::remove_dir_all(&folder);
        let person = Peer::Program {
            inside: false,
            agent: false,
        };
        assert_eq!(unrecorded, (person, person));
        assert_eq!(other_port, person);
        assert_eq!(recorded, Peer::Server);
        assert_eq!(desktop_recorded, person);
    }

    /// The real thing: a program whose file is named like a process id (`x:1 y`), connected through a socket it hands
    /// down to another, is placed by the processes that hold the socket, whatever their names say.
    #[test]
    fn a_program_named_like_a_process_id_is_placed_by_its_own() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let folder =
            std::env::temp_dir().join(format!("haarness-process-tree-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&folder).unwrap();
        let named = folder.join("x:1 y");
        std::fs::copy(std::env::current_exe().unwrap(), &named).unwrap();
        let mut child = std::process::Command::new(&named)
            .args([
                "--ignored",
                "--exact",
                "process_tree::tests::connect_and_hand_down",
            ])
            .env("HAARNESS_TEST_CONNECT_PORT", port.to_string())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let (_accepted, address) = listener.accept().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        // Once `sleep` holds the socket too, both are found. A look that ran out of time finds none, so the most found
        // counts.
        let mut found = 0;
        while found < 2 && std::time::Instant::now() < deadline {
            found = found.max(holders(address).map_or(0, |holders| holders.len()));
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let placed = (found, settled(|| peer(address)));
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&folder);
        assert_eq!(
            placed,
            (
                2,
                Peer::Program {
                    inside: true,
                    agent: true
                }
            )
        );
    }
}
