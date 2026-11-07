//! Whether a process runs inside Haarness: started, directly or further down, by a `haarness` program (a server or
//! the terminal UI hosting one), the way an agent's shell command, a hook or a terminal pane is. Settings that give
//! agents more reach refuse changes from such a process, so an agent cannot widen what the person allowed by running
//! a command, whatever variables that command clears.
//!
//! This reads the process table, so it costs a `ps` (and an `lsof` to place a local connection): callers ask only for
//! the rare request that would widen something. A process that detached itself from its parent on purpose is not
//! recognised; this guards the routes an agent would take, not a determined program.

use std::{net::SocketAddr, path::Path};

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
    program: String,
}

/// Whether a `haarness` program is among `pid`'s ancestors (`pid` itself does not count). An unreadable process
/// table reads as no.
#[must_use]
pub fn inside_haarness(pid: u32) -> bool {
    table().is_some_and(|rows| below_program(&rows, pid))
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
        .args(["-A", "-o", "pid=,ppid=,comm="])
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

/// `ps -o pid=,ppid=,comm=` lines. macOS prints the program's path, which may hold spaces; Linux its name.
fn parse_table(text: &str) -> Vec<Process> {
    text.lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (parent, command) = rest.trim_start().split_once(char::is_whitespace)?;
            let program = Path::new(command.trim())
                .file_name()?
                .to_string_lossy()
                .into_owned();
            Some(Process {
                pid: pid.parse().ok()?,
                parent: parent.parse().ok()?,
                program,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "    1     0 /sbin/launchd
  500     1 /System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal
  501   500 /bin/zsh
  502   501 /Users/sam/.local/bin/haarness
  503   502 /bin/zsh
  504   503 /usr/bin/curl
  600     1 /Applications/Haarness.app/Contents/MacOS/Haarness
  601   600 /Applications/Haarness.app/Contents/Frameworks/Haarness Helper (Renderer).app/Contents/MacOS/Haarness Helper (Renderer)
  602   600 /Applications/Haarness.app/Contents/Resources/bin/haarness
  603   602 /bin/sh
  604   603 python3
";

    #[test]
    fn a_process_started_by_haarness_is_inside_it_and_others_are_not() {
        let rows = parse_table(TABLE);
        assert_eq!(rows.len(), 11);
        assert_eq!(rows[6].program, "Haarness");
        assert_eq!(rows[7].program, "Haarness Helper (Renderer)");
        // An agent's command under the terminal UI, and one under the server Desktop runs.
        assert!(below_program(&rows, 504));
        assert!(below_program(&rows, 604));
        // The terminal UI itself, the shell it runs in, and Desktop's own windows.
        assert!(!below_program(&rows, 502));
        assert!(!below_program(&rows, 501));
        assert!(!below_program(&rows, 601));
        // A process the table does not know.
        assert!(!below_program(&rows, 999));
    }

    #[test]
    fn a_loop_in_the_table_ends() {
        let rows = parse_table("  10    11 /bin/a\n  11    10 /bin/b\n");
        assert!(!below_program(&rows, 10));
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
