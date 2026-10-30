//! Own the whole stdio MCP process tree, including failed handshakes, and
//! bound what one message from it may hold.
use process_wrap::tokio::{ChildWrapper, CommandWrap};
use rmcp::{
    RoleClient,
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::{Transport, async_rw::AsyncRwTransport},
};
use std::{
    future::Future,
    io,
    pin::Pin,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    process::{ChildStdin, ChildStdout},
};

/// The most one message from a stdio server may hold, its newline not
/// counted. Real results of 20 MB and more pass; a server that never ends
/// its line is stopped before it can fill memory.
pub(super) const MESSAGE_LIMIT: usize = 64 * 1024 * 1024;
/// How long a closed server may take to exit before it is killed, where no
/// process group stops it at once (Windows).
const EXIT_WAIT: Duration = Duration::from_secs(3);

pub(super) struct Process {
    child: Box<dyn ChildWrapper>,
    transport: AsyncRwTransport<RoleClient, Capped<ChildStdout>, ChildStdin>,
    #[cfg(unix)]
    pid: Option<i32>,
    /// Stopped by the next server should this one die before it can.
    _recorded: haarness_core::process_groups::Recorded,
}

impl Process {
    /// Starts `command`. `oversized` is set once a message passes
    /// [`MESSAGE_LIMIT`]; the connection then ends and the server is stopped.
    pub fn new(
        mut command: tokio::process::Command,
        oversized: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        #[cfg(unix)]
        command.process_group(0);
        command
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut command = CommandWrap::from(command);
        command.wrap(process_wrap::tokio::KillOnDrop);
        #[cfg(windows)]
        command.wrap(process_wrap::tokio::JobObject);
        let mut child = command.spawn()?;
        let stdin = child
            .stdin()
            .take()
            .ok_or_else(|| io::Error::other("stdin was already taken"))?;
        let stdout = child
            .stdout()
            .take()
            .ok_or_else(|| io::Error::other("stdout was already taken"))?;
        let stdout = Capped::new(stdout, MESSAGE_LIMIT, oversized);
        Ok(Self {
            #[cfg(unix)]
            pid: child.id().and_then(|pid| i32::try_from(pid).ok()),
            _recorded: haarness_core::process_groups::recorded(child.id()),
            transport: AsyncRwTransport::new(stdout, stdin),
            child,
        })
    }

    #[cfg_attr(not(unix), allow(clippy::unused_self))]
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
        self.transport.send(message)
    }
    fn receive(&mut self) -> impl Future<Output = Option<ServerJsonRpcMessage>> + Send {
        self.transport.receive()
    }
    async fn close(&mut self) -> io::Result<()> {
        self.stop();
        self.transport.close().await?;
        // Closing its input ends a well-behaved server; one that keeps
        // running is killed.
        if tokio::time::timeout(EXIT_WAIT, self.child.wait())
            .await
            .is_err()
        {
            Box::into_pin(self.child.kill()).await?;
        }
        Ok(())
    }
}

/// A stdio server's output, read until one message passes its limit. rmcp
/// keeps every byte of a message until its newline arrives, so a server that
/// never writes one would grow that buffer without bound. Past the limit the
/// read fails as if the server had closed its output, without handing rmcp
/// the bytes that passed it: the connection ends, its buffer is freed with it
/// and the server is stopped.
struct Capped<R> {
    inner: R,
    /// Bytes of the message still open, read before its newline.
    open: usize,
    limit: usize,
    oversized: Arc<AtomicBool>,
}

impl<R> Capped<R> {
    fn new(inner: R, limit: usize, oversized: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            open: 0,
            limit,
            oversized,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for Capped<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.oversized.load(Ordering::Acquire) {
            return Poll::Ready(Err(oversized_error()));
        }
        let before = buf.filled().len();
        ready!(Pin::new(&mut self.inner).poll_read(cx, buf))?;
        match open_after(self.open, &buf.filled()[before..], self.limit) {
            Some(open) => {
                self.open = open;
                Poll::Ready(Ok(()))
            }
            None => {
                buf.set_filled(before);
                self.oversized.store(true, Ordering::Release);
                Poll::Ready(Err(oversized_error()))
            }
        }
    }
}

fn oversized_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "MCP server message exceeds the size limit",
    )
}

/// The bytes of the message still open after `chunk`, given `open` before
/// it, or `None` once any message passes `limit`: the open one, or one the
/// chunk holds or ends. A message of exactly `limit` bytes passes.
fn open_after(open: usize, chunk: &[u8], limit: usize) -> Option<usize> {
    let mut size = open;
    let mut rest = chunk;
    while let Some(index) = rest.iter().position(|byte| *byte == b'\n') {
        if size.saturating_add(index) > limit {
            return None;
        }
        size = 0;
        rest = &rest[index + 1..];
    }
    let size = size.saturating_add(rest.len());
    (size <= limit).then_some(size)
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
            && pid != std::process::id().cast_signed()
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

#[cfg(test)]
mod limit_tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn a_message_may_fill_the_limit_but_not_pass_it_wherever_it_ends() {
        assert_eq!(open_after(0, b"abc", 3), Some(3));
        assert_eq!(open_after(0, b"abcd", 3), None);
        // The newline is not counted, and ends the message.
        assert_eq!(open_after(0, b"abc\nab", 3), Some(2));
        assert_eq!(open_after(2, b"a\nabc\n", 3), Some(0));
        // A message the chunk ends counts what came before the chunk.
        assert_eq!(open_after(2, b"ab\n", 3), None);
        // One message too long in the middle of a chunk of short ones.
        assert_eq!(open_after(0, b"a\nabcd\nb", 3), None);
        assert_eq!(open_after(3, b"", 3), Some(3));
    }

    /// A peer that writes one endless message: the transport stops taking
    /// bytes at the limit, ends, and never holds more than the limit.
    #[tokio::test]
    async fn an_endless_message_ends_the_connection_at_the_limit() {
        const LIMIT: usize = 4 * 1024 * 1024;
        let (server, client) = tokio::io::duplex(64 * 1024);
        let (read, _) = tokio::io::split(client);
        let (_, mut write) = tokio::io::split(server);
        let oversized = Arc::<AtomicBool>::default();
        let mut transport = AsyncRwTransport::<RoleClient, _, _>::new(
            Capped::new(read, LIMIT, oversized.clone()),
            tokio::io::sink(),
        );
        let writer = tokio::spawn(async move {
            let chunk = vec![b'x'; 64 * 1024];
            let mut written = 0usize;
            write
                .write_all(br#"{"jsonrpc":"2.0","id":1,"result":{"text":""#)
                .await
                .unwrap();
            // Stops when the reader is gone, or once far past the limit.
            while written < 8 * LIMIT && write.write_all(&chunk).await.is_ok() {
                written += chunk.len();
            }
            written
        });
        let received = tokio::time::timeout(Duration::from_secs(10), transport.receive())
            .await
            .expect("the transport kept reading past the limit");
        assert!(received.is_none());
        assert!(oversized.load(Ordering::Acquire));
        drop(transport);
        let written = writer.await.unwrap();
        // What the peer could write is the limit plus the pipe's buffers.
        assert!(written <= LIMIT + 256 * 1024, "{written} bytes were taken");
    }

    #[tokio::test]
    async fn messages_up_to_the_limit_are_read_whole() {
        const LIMIT: usize = 1024 * 1024;
        let text = "y".repeat(LIMIT - 64);
        let line = format!(
            "{}\n",
            serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"text":text}})
        );
        assert!(line.len() - 1 <= LIMIT);
        let oversized = Arc::<AtomicBool>::default();
        let mut transport = AsyncRwTransport::<RoleClient, _, _>::new(
            Capped::new(
                std::io::Cursor::new(line.repeat(3).into_bytes()),
                LIMIT,
                oversized.clone(),
            ),
            tokio::io::sink(),
        );
        for _ in 0..3 {
            assert!(transport.receive().await.is_some());
        }
        assert!(transport.receive().await.is_none());
        assert!(!oversized.load(Ordering::Acquire));
    }
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn traversal_is_complete_and_excludes_unrelated_processes_and_cycles() {
        let mut snapshot =
            String::from("1 7000\n7000 7002\n7001 7000\n7002 7001\n9000 1\ninvalid\n");
        for pid in 10000..10150 {
            use std::fmt::Write as _;
            let _ = writeln!(snapshot, "{pid} 7002");
        }
        let children = super::descendants(&snapshot, 7000);
        assert_eq!(children.len(), 152);
        assert!(children.contains(&10149));
        assert!(!children.contains(&7000));
        assert!(!children.contains(&9000));
        assert!(!children.contains(&1));
    }
}
