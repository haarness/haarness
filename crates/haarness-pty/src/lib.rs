//! Server-owned, workspace-scoped pseudo-terminal sessions.

use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use haarness_core::{CoreError, CoreResult, EventBus};
use haarness_protocol::{PtyCreateRequest, PtyCursor, PtyId, PtyInfo, PtyStatus, PtyUpdateRequest};
use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde_json::json;
use tokio::sync::broadcast;

const BUFFER_LIMIT: usize = 2 * 1024 * 1024;
const OUTPUT_CHANNEL_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct PtyManager {
    inner: Arc<Inner>,
}

struct Inner {
    workspace: PathBuf,
    events: EventBus,
    sessions: Mutex<HashMap<PtyId, Arc<Session>>>,
}

struct Session {
    info: Mutex<PtyInfo>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    output: broadcast::Sender<Vec<u8>>,
    buffer: Mutex<OutputBuffer>,
    deleted: AtomicBool,
}

#[derive(Default)]
struct OutputBuffer {
    start: u64,
    end: u64,
    bytes: VecDeque<u8>,
}

pub struct PtySubscription {
    pub replay: Vec<u8>,
    pub cursor: PtyCursor,
    pub receiver: broadcast::Receiver<Vec<u8>>,
}

impl PtyManager {
    #[must_use]
    pub fn new(workspace: PathBuf, events: EventBus) -> Self {
        Self {
            inner: Arc::new(Inner {
                workspace,
                events,
                sessions: Mutex::new(HashMap::new()),
            }),
        }
    }

    #[must_use]
    pub fn list(&self) -> Vec<PtyInfo> {
        let mut result = self
            .inner
            .sessions
            .lock()
            .values()
            .map(|session| session.info.lock().clone())
            .collect::<Vec<_>>();
        result.sort_by_key(|info| info.id.0);
        result
    }

    pub fn get(&self, id: PtyId) -> CoreResult<PtyInfo> {
        Ok(self.session(id)?.info.lock().clone())
    }

    pub fn create(&self, request: PtyCreateRequest) -> CoreResult<PtyInfo> {
        let mut sessions = self.inner.sessions.lock();
        if request.cols == 0 || request.rows == 0 {
            return Err(CoreError::Invalid("PTY dimensions must be positive".into()));
        }
        let cols = request.cols;
        let rows = request.rows;
        let cwd = self.resolve_cwd(request.cwd.as_deref())?;
        let requested_title = request.title.map(|title| title.trim().to_owned());
        if requested_title
            .as_ref()
            .is_some_and(|title| title.is_empty() || title.chars().count() > 256)
        {
            return Err(CoreError::Invalid(
                "PTY title must contain 1 to 256 characters".into(),
            ));
        }
        let command = request.command.unwrap_or_else(default_shell);
        if command.trim().is_empty() {
            return Err(CoreError::Invalid("PTY command cannot be empty".into()));
        }

        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(pty_error)?;
        let mut builder = CommandBuilder::new(&command);
        builder.args(&request.args);
        builder.cwd(&cwd);
        for name in haarness_core::private_env_names() {
            builder.env_remove(name);
        }
        builder.env(
            "PATH",
            haarness_ripgrep::prepend_path(std::env::var_os("PATH").as_deref())
                .map_err(pty_error)?,
        );
        builder.env("TERM", "xterm-256color");
        builder.env("COLORTERM", "truecolor");
        builder.env("HAARNESS_TERMINAL", "1");
        let child = pair.slave.spawn_command(builder).map_err(pty_error)?;
        let pid = child.process_id();
        let killer = child.clone_killer();
        let reader = pair.master.try_clone_reader().map_err(pty_error)?;
        let writer = pair.master.take_writer().map_err(pty_error)?;
        drop(pair.slave);

        let id = PtyId::new();
        let title = requested_title.unwrap_or_else(|| {
            Path::new(&command)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("terminal")
                .to_owned()
        });
        let info = PtyInfo {
            id,
            title,
            command,
            args: request.args,
            cwd: cwd.display().to_string(),
            status: PtyStatus::Running,
            pid,
            cols,
            rows,
        };
        let (output, _) = broadcast::channel(OUTPUT_CHANNEL_CAPACITY);
        let session = Arc::new(Session {
            info: Mutex::new(info.clone()),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            output,
            buffer: Mutex::new(OutputBuffer::default()),
            deleted: AtomicBool::new(false),
        });
        sessions.insert(id, Arc::clone(&session));
        drop(sessions);
        self.publish("pty.created", &info);
        spawn_reader(Arc::clone(&session), reader);
        spawn_waiter(Arc::downgrade(&self.inner), session, child);
        Ok(info)
    }

    pub fn update(&self, id: PtyId, request: PtyUpdateRequest) -> CoreResult<PtyInfo> {
        let session = self.session(id)?;
        let title = request.title.map(|title| title.trim().to_owned());
        if let Some(title) = &title {
            if title.is_empty() || title.chars().count() > 256 {
                return Err(CoreError::Invalid(
                    "PTY title must contain 1 to 256 characters".into(),
                ));
            }
        }
        match (request.cols, request.rows) {
            (None, None) => {}
            (Some(cols), Some(rows)) => Self::resize_session(&session, cols, rows)?,
            _ => {
                return Err(CoreError::Invalid(
                    "PTY resize requires both cols and rows".into(),
                ));
            }
        }
        if let Some(title) = title {
            session.info.lock().title = title;
        }
        let info = session.info.lock().clone();
        self.publish("pty.updated", &info);
        Ok(info)
    }

    pub fn write(&self, id: PtyId, bytes: &[u8]) -> CoreResult<()> {
        let session = self.session(id)?;
        if session.info.lock().status != PtyStatus::Running {
            return Err(CoreError::Conflict("PTY process has exited".into()));
        }
        let mut writer = session.writer.lock();
        writer.write_all(bytes).map_err(pty_error)?;
        writer.flush().map_err(pty_error)
    }

    pub fn subscribe(&self, id: PtyId, after: Option<u64>) -> CoreResult<PtySubscription> {
        let session = self.session(id)?;
        let buffer = session.buffer.lock();
        // Subscribe while holding the same lock used by append+publish. This
        // makes the replay/live boundary exact: a byte can appear in one side
        // or the other, never both and never neither.
        let receiver = session.output.subscribe();
        let requested = after.unwrap_or(buffer.start);
        let effective = requested.clamp(buffer.start, buffer.end);
        let offset = usize::try_from(effective.saturating_sub(buffer.start)).unwrap_or(usize::MAX);
        let replay = buffer.bytes.iter().skip(offset).copied().collect();
        Ok(PtySubscription {
            replay,
            cursor: PtyCursor {
                start: buffer.start,
                end: buffer.end,
                truncated: requested < buffer.start,
            },
            receiver,
        })
    }

    pub fn delete(&self, id: PtyId) -> CoreResult<()> {
        let session = self
            .inner
            .sessions
            .lock()
            .remove(&id)
            .ok_or_else(|| CoreError::NotFound(format!("PTY {id}")))?;
        session.deleted.store(true, Ordering::Release);
        if session.info.lock().status == PtyStatus::Running {
            if let Err(error) = session.killer.lock().kill() {
                // Exit and delete can race. The session has already been
                // detached from the manager, so an absent process is still a
                // successful, idempotent terminal close from the user's view.
                tracing::debug!(error = %error, %id, "PTY was already exiting during delete");
            }
        }
        self.inner
            .events
            .publish("pty.deleted", None, None, None, json!({"id": id}));
        Ok(())
    }

    pub fn shutdown_all(&self) {
        let sessions = self
            .inner
            .sessions
            .lock()
            .drain()
            .map(|(_, session)| session)
            .collect::<Vec<_>>();
        for session in sessions {
            session.deleted.store(true, Ordering::Release);
            if session.info.lock().status == PtyStatus::Running
                && let Err(error) = session.killer.lock().kill()
            {
                tracing::warn!(error = %error, "failed to stop PTY during shutdown");
            }
        }
    }

    fn session(&self, id: PtyId) -> CoreResult<Arc<Session>> {
        self.inner
            .sessions
            .lock()
            .get(&id)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(format!("PTY {id}")))
    }

    fn resolve_cwd(&self, requested: Option<&str>) -> CoreResult<PathBuf> {
        let workspace = self.inner.workspace.canonicalize().map_err(pty_error)?;
        let candidate = requested.map_or_else(
            || workspace.clone(),
            |cwd| {
                let path = PathBuf::from(cwd);
                if path.is_absolute() {
                    path
                } else {
                    workspace.join(path)
                }
            },
        );
        let candidate = candidate.canonicalize().map_err(pty_error)?;
        if !candidate.starts_with(&workspace) {
            return Err(CoreError::PermissionDenied(
                "PTY working directory must be inside the active workspace".into(),
            ));
        }
        if !candidate.is_dir() {
            return Err(CoreError::Invalid(
                "PTY working directory is not a directory".into(),
            ));
        }
        Ok(candidate)
    }

    fn resize_session(session: &Session, cols: u16, rows: u16) -> CoreResult<()> {
        if cols == 0 || rows == 0 {
            return Err(CoreError::Invalid("PTY dimensions must be positive".into()));
        }
        session
            .master
            .lock()
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(pty_error)?;
        let mut info = session.info.lock();
        info.cols = cols;
        info.rows = rows;
        Ok(())
    }

    fn publish(&self, event_type: &str, info: &PtyInfo) {
        self.inner.events.publish(
            event_type,
            None,
            None,
            None,
            serde_json::to_value(info).expect("PTY info serializes"),
        );
    }
}

fn spawn_reader(session: Arc<Session>, mut reader: Box<dyn Read + Send>) {
    thread::Builder::new()
        .name(format!("haarness-pty-read-{}", session.info.lock().id))
        .spawn(move || {
            let mut chunk = vec![0; 64 * 1024];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(length) => {
                        let bytes = chunk[..length].to_vec();
                        let mut buffer = session.buffer.lock();
                        buffer.append(&bytes);
                        let _ = session.output.send(bytes);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        })
        .expect("spawn PTY reader");
}

fn spawn_waiter(
    manager: Weak<Inner>,
    session: Arc<Session>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
) {
    thread::Builder::new()
        .name(format!("haarness-pty-wait-{}", session.info.lock().id))
        .spawn(move || {
            let _ = child.wait();
            let info = {
                let mut info = session.info.lock();
                info.status = PtyStatus::Exited;
                info.clone()
            };
            let _ = session.output.send(Vec::new());
            if !session.deleted.load(Ordering::Acquire)
                && let Some(manager) = manager.upgrade()
            {
                manager.events.publish(
                    "pty.exited",
                    None,
                    None,
                    None,
                    serde_json::to_value(info).expect("PTY info serializes"),
                );
            }
        })
        .expect("spawn PTY waiter");
}

impl OutputBuffer {
    fn append(&mut self, bytes: &[u8]) {
        self.end = self
            .end
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        self.bytes.extend(bytes);
        while self.bytes.len() > BUFFER_LIMIT {
            self.bytes.pop_front();
            self.start = self.start.saturating_add(1);
        }
    }
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| {
        if cfg!(windows) {
            "cmd.exe".into()
        } else {
            "/bin/sh".into()
        }
    })
}

fn pty_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::Tool(format!("PTY: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_buffer_keeps_a_monotonic_cursor_when_trimming() {
        let mut buffer = OutputBuffer::default();
        buffer.append(&vec![b'a'; BUFFER_LIMIT]);
        buffer.append(b"tail");
        assert_eq!(buffer.start, 4);
        assert_eq!(buffer.end, u64::try_from(BUFFER_LIMIT + 4).unwrap());
        assert_eq!(buffer.bytes.len(), BUFFER_LIMIT);
        assert_eq!(
            buffer
                .bytes
                .iter()
                .rev()
                .take(4)
                .copied()
                .collect::<Vec<_>>(),
            b"liat"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminals_are_not_rejected_after_twenty_sessions() {
        let manager = PtyManager::new(std::env::current_dir().unwrap(), EventBus::new(64));
        let mut ids = Vec::new();
        for _ in 0..21 {
            let info = manager
                .create(PtyCreateRequest {
                    command: Some("/bin/sh".into()),
                    args: vec!["-c".into(), "read line; printf '%s\\n' \"$line\"".into()],
                    ..PtyCreateRequest::default()
                })
                .unwrap();
            ids.push(info.id);
        }
        assert_eq!(manager.list().len(), 21);
        let last = *ids.last().unwrap();
        let mut subscriber = manager.subscribe(last, Some(0)).unwrap();
        manager.write(last, b"last-terminal-proof\n").unwrap();
        let chunk = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            subscriber.receiver.recv(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!chunk.is_empty());
        for id in ids {
            manager.delete(id).unwrap();
        }
        assert!(manager.list().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pty_survives_subscriber_detach_replays_and_resizes() {
        let manager = PtyManager::new(std::env::current_dir().unwrap(), EventBus::new(64));
        let info = manager
            .create(PtyCreateRequest {
                command: Some("/bin/sh".into()),
                args: vec![
                    "-c".into(),
                    "printf 'before\\n'; read line; printf 'after:%s\\n' \"$line\"; sleep 30"
                        .into(),
                ],
                cols: 80,
                rows: 24,
                ..PtyCreateRequest::default()
            })
            .unwrap();
        let mut first = manager.subscribe(info.id, Some(0)).unwrap();
        let mut output = first.replay.clone();
        while !String::from_utf8_lossy(&output).contains("before") {
            let chunk =
                tokio::time::timeout(std::time::Duration::from_secs(3), first.receiver.recv())
                    .await
                    .unwrap()
                    .unwrap();
            output.extend(chunk);
        }
        manager.write(info.id, b"proof\n").unwrap();
        while !String::from_utf8_lossy(&output).contains("after:proof") {
            let chunk =
                tokio::time::timeout(std::time::Duration::from_secs(3), first.receiver.recv())
                    .await
                    .unwrap()
                    .unwrap();
            output.extend(chunk);
        }
        drop(first);

        let replayed = manager.subscribe(info.id, Some(0)).unwrap();
        let replay = String::from_utf8_lossy(&replayed.replay);
        assert!(replay.contains("before"));
        assert!(replay.contains("after:proof"));
        let resized = manager
            .update(
                info.id,
                PtyUpdateRequest {
                    title: Some("proof tab".into()),
                    cols: Some(113),
                    rows: Some(37),
                },
            )
            .unwrap();
        assert_eq!((resized.cols, resized.rows), (113, 37));
        assert_eq!(resized.title, "proof tab");

        manager.delete(info.id).unwrap();
        assert!(manager.list().is_empty());
    }
}
