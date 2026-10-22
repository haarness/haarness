//! Own the whole stdio MCP process tree, including failed handshakes.
use rmcp::{
    RoleClient,
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::{TokioChildProcess, Transport},
};
use std::{future::Future, io};

pub(super) struct Process {
    inner: TokioChildProcess,
    #[cfg(unix)]
    pid: Option<i32>,
    /// Stopped by the next server should this one die before it can.
    _recorded: haarness_core::process_groups::Recorded,
}

impl Process {
    pub fn new(mut command: tokio::process::Command) -> io::Result<Self> {
        #[cfg(unix)]
        command.process_group(0);
        command.kill_on_drop(true);
        let mut command = process_wrap::tokio::CommandWrap::from(command);
        command.wrap(process_wrap::tokio::KillOnDrop);
        #[cfg(windows)]
        command.wrap(process_wrap::tokio::JobObject);
        let inner = TokioChildProcess::new(command)?;
        Ok(Self {
            #[cfg(unix)]
            pid: inner.id().and_then(|pid| i32::try_from(pid).ok()),
            _recorded: haarness_core::process_groups::recorded(inner.id()),
            inner,
        })
    }

    fn stop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid.take().filter(|pid| *pid > 1) {
            use nix::{
                sys::signal::{Signal, kill, killpg},
                unistd::Pid,
            };
            // Snapshot before stopping the parent, since detached grandchildren
            // can be reparented when it exits. Never select processes by name.
            if let Ok(output) = std::process::Command::new("ps")
                .args(["-axo", "pid=,ppid="])
                .output()
                && output.status.success()
            {
                for child in descendants(&String::from_utf8_lossy(&output.stdout), pid)
                    .into_iter()
                    .rev()
                {
                    let _ = kill(Pid::from_raw(child), Signal::SIGKILL);
                }
            }
            // The dedicated group also catches children born during the snapshot
            // and children already reparented after the MCP leader exited.
            let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Transport<RoleClient> for Process {
    type Error = io::Error;
    fn send(
        &mut self,
        message: ClientJsonRpcMessage,
    ) -> impl Future<Output = io::Result<()>> + Send + 'static {
        self.inner.send(message)
    }
    fn receive(&mut self) -> impl Future<Output = Option<ServerJsonRpcMessage>> + Send {
        self.inner.receive()
    }
    async fn close(&mut self) -> io::Result<()> {
        self.stop();
        self.inner.close().await
    }
}

#[cfg(unix)]
fn descendants(snapshot: &str, root: i32) -> Vec<i32> {
    use std::collections::{HashMap, HashSet};
    let mut children = HashMap::<i32, Vec<i32>>::new();
    for line in snapshot.lines() {
        let mut fields = line.split_whitespace();
        if let (Some(pid), Some(parent)) = (
            fields.next().and_then(|s| s.parse::<i32>().ok()),
            fields.next().and_then(|s| s.parse::<i32>().ok()),
        ) && fields.next().is_none()
            && pid > 1
            && pid != std::process::id() as i32
        {
            children.entry(parent).or_default().push(pid);
        }
    }
    let mut seen = HashSet::from([root]);
    let mut queue = vec![root];
    let mut index = 0;
    while index < queue.len() {
        if let Some(children) = children.get(&queue[index]) {
            for child in children {
                if seen.insert(*child) {
                    queue.push(*child);
                }
            }
        }
        index += 1;
    }
    queue.into_iter().skip(1).collect()
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn traversal_is_complete_and_excludes_unrelated_processes_and_cycles() {
        let mut snapshot =
            String::from("1 7000\n7000 7002\n7001 7000\n7002 7001\n9000 1\ninvalid\n");
        for pid in 10000..10150 {
            snapshot.push_str(&format!("{pid} 7002\n"));
        }
        let children = super::descendants(&snapshot, 7000);
        assert_eq!(children.len(), 152);
        assert!(children.contains(&10149));
        assert!(!children.contains(&7000));
        assert!(!children.contains(&9000));
        assert!(!children.contains(&1));
    }
}
