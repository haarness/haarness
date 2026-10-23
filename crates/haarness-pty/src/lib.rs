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

mod modes;

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
    /// The terminal modes in effect at `start`: fed every byte that leaves.
    modes: modes::Modes,
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
        // Checked under the same lock a create holds, so concurrent requests
        // for "a terminal" start one.
        if request.if_none
            && let Some(oldest) = sessions
                .values()
                .map(|session| session.info.lock().clone())
                .filter(|info| info.status == PtyStatus::Running)
                .min_by_key(|info| info.id.0)
        {
            return Ok(oldest);
        }
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
        // cmd.exe treats a \\?\ working directory as a UNC path and opens in
        // C:\Windows; give children the plain form.
        builder.cwd(dunce::simplified(&cwd));
        for name in haarness_core::private_env_names() {
            builder.env_remove(name);
        }
        // A terminal is the person's, also when this server holds Haarness's
        // own agent markers (a `haarness serve` an agent's command started).
        for name in haarness_core::own_agent_markers(std::env::var_os("AI_AGENT").as_deref()) {
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
        if let Some(locale) = missing_locale() {
            builder.env("LANG", locale);
        }
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
            created: false,
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
        Ok(PtyInfo {
            created: true,
            ..info
        })
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
        let (replay, cursor) = buffer.replay(after);
        Ok(PtySubscription {
            replay,
            cursor,
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
        let excess = self.bytes.len().saturating_sub(BUFFER_LIMIT);
        if excess > 0 {
            self.modes.feed(self.bytes.drain(..excess));
            self.start = self
                .start
                .saturating_add(u64::try_from(excess).unwrap_or(u64::MAX));
        }
    }

    /// The output after `requested` (none: a client that has nothing yet),
    /// with the control frame that announces it. Output older than the
    /// buffer is gone: the replay then starts at the first whole line (or
    /// character) outside an escape sequence, so a client never draws half a
    /// sequence or half a character, and carries the modes the dropped output
    /// left on.
    fn replay(&self, requested: Option<u64>) -> (Vec<u8>, PtyCursor) {
        let requested = requested.unwrap_or(0);
        let truncated = requested < self.start;
        let (begin, modes) = if truncated {
            let (skip, modes) = self.modes.clean_start(self.bytes.iter().copied());
            let begin = self
                .start
                .saturating_add(u64::try_from(skip).unwrap_or(u64::MAX));
            (
                begin,
                Some(modes.preamble()).filter(|modes| !modes.is_empty()),
            )
        } else {
            (requested.min(self.end), None)
        };
        let offset = usize::try_from(begin.saturating_sub(self.start)).unwrap_or(usize::MAX);
        let replay = self.bytes.iter().skip(offset).copied().collect();
        let cursor = PtyCursor {
            start: if truncated { begin } else { self.start },
            end: self.end,
            truncated,
            modes,
        };
        (replay, cursor)
    }
}

fn default_shell() -> String {
    // Git Bash exports SHELL=/usr/bin/bash, a path Windows cannot start;
    // there the terminal opens the system's command processor instead.
    if cfg!(windows) {
        return std::env::var("SHELL")
            .ok()
            .filter(|shell| Path::new(shell).is_absolute() && Path::new(shell).is_file())
            .or_else(|| {
                std::env::var("COMSPEC")
                    .ok()
                    .filter(|shell| !shell.is_empty())
            })
            .unwrap_or_else(|| "cmd.exe".into());
    }
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
}

/// The `LANG` a terminal or an agent's shell command gets when Haarness
/// itself has no locale. Desktop opened from Finder inherits no `LANG`,
/// `LC_ALL` or `LC_CTYPE`, and a shell started without them reads each byte of
/// 日本 as a character of its own, so typing it prints escapes. Like
/// Terminal.app, Haarness then sets `LANG` from the system's region settings.
/// `None` when a locale is set in any of the three: that one is left alone.
#[must_use]
pub fn missing_locale() -> Option<&'static str> {
    static LOCALE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if locale_set(|name| std::env::var_os(name)) {
        return None;
    }
    Some(LOCALE.get_or_init(|| {
        utf8_locale(system_locale().as_deref(), |name| {
            Path::new("/usr/share/locale").join(name).is_dir()
        })
    }))
}

/// Whether the environment names a locale for character handling. An empty
/// value counts as unset, as it does for the C library.
fn locale_set(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .any(|name| env(name).is_some_and(|value| !value.is_empty()))
}

/// The region setting macOS keeps for the user (`en_US`, `ja_JP`,
/// `zh-Hans_CN`, `en_GB@rg=…`). Other systems have none to offer.
fn system_locale() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLocale"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// A UTF-8 locale for `preferred` (a macOS region setting) when the system
/// has one by that name, else the platform's default: `en_US.UTF-8` on
/// macOS, which has no `C.UTF-8`, and `C.UTF-8` elsewhere.
fn utf8_locale(preferred: Option<&str>, installed: impl Fn(&str) -> bool) -> String {
    let fallback = if cfg!(target_os = "macos") {
        "en_US.UTF-8"
    } else {
        "C.UTF-8"
    };
    preferred
        .and_then(|value| {
            let value = value.split('@').next()?;
            let (language, region) = value.split_once('_')?;
            let language = language.split('-').next()?;
            let letters = |text: &str, lengths: std::ops::RangeInclusive<usize>| {
                lengths.contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_alphabetic())
            };
            (letters(language, 2..=3) && letters(region, 2..=2))
                .then(|| format!("{language}_{region}.UTF-8"))
        })
        .filter(|name| installed(name))
        .unwrap_or_else(|| fallback.to_owned())
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

    #[test]
    fn a_truncated_replay_starts_on_a_whole_line_with_the_modes_left_on() {
        let mut buffer = OutputBuffer::default();
        // A full-screen program starts, then redraws well past the buffer.
        buffer.append(b"\x1b[?1049h\x1b[?1h\x1b[?1000;1006h\x1b[?2004h");
        let mut line = 0;
        while buffer.end < u64::try_from(BUFFER_LIMIT * 2).unwrap() {
            buffer.append(format!("\x1b[31mline {line:07}\x1b[0m\r\n").as_bytes());
            line += 1;
        }
        let (replay, cursor) = buffer.replay(Some(0));
        assert!(cursor.truncated);
        assert!(cursor.start > buffer.start && cursor.start - buffer.start < 64);
        assert_eq!(
            cursor.end - cursor.start,
            u64::try_from(replay.len()).unwrap()
        );
        assert!(replay.starts_with(b"\x1b[31mline "), "{:?}", &replay[..20]);
        assert_eq!(
            cursor.modes.as_deref(),
            Some("\x1b[?1049h\x1b[?1h\x1b[?1000h\x1b[?1006h\x1b[?2004h")
        );

        // A client that has everything still buffered gets the rest as is.
        let (replay, cursor) = buffer.replay(Some(buffer.end - 5));
        assert_eq!(
            (replay.len(), cursor.start, cursor.truncated),
            (5, buffer.start, false)
        );
        assert_eq!(cursor.modes, None);
        // Modes the program turned off before the cut are not restored.
        buffer.append(b"\x1b[?1049l\x1b[?1l\x1b[?1000l\x1b[?2004l");
        buffer.append(&vec![b'x'; BUFFER_LIMIT]);
        buffer.append(b"\n");
        buffer.append(&vec![b'y'; BUFFER_LIMIT - 100]);
        let (replay, cursor) = buffer.replay(Some(0));
        assert_eq!(cursor.start, buffer.start + 100);
        assert!(replay.len() == BUFFER_LIMIT - 100 && replay.iter().all(|byte| *byte == b'y'));
        assert_eq!(cursor.modes.as_deref(), Some("\x1b[?1006h"));
        // A first connection, which names no cursor, is told the same.
        assert_eq!(buffer.replay(None), buffer.replay(Some(0)));
    }

    #[test]
    fn a_truncated_replay_cut_just_after_a_line_break_keeps_the_next_line() {
        let mut buffer = OutputBuffer::default();
        buffer.append(&[vec![b'a'; 99], b"\n".to_vec()].concat());
        buffer.append(b"first line\n");
        buffer.append(&vec![b'c'; BUFFER_LIMIT - 11]);
        assert_eq!(buffer.start, 100);
        let (replay, cursor) = buffer.replay(None);
        assert!(cursor.truncated);
        assert_eq!(cursor.start, 100);
        assert!(replay.starts_with(b"first line\n"));
    }

    #[test]
    fn a_truncated_replay_without_line_breaks_starts_on_a_whole_character() {
        let mut buffer = OutputBuffer::default();
        // 日本 is six bytes, so the cut lands inside a character.
        buffer.append(b"x");
        while buffer.bytes.len() < BUFFER_LIMIT {
            buffer.append("日本".as_bytes());
        }
        assert!(buffer.bytes.front().is_some_and(|byte| byte & 0xc0 == 0x80));
        let (replay, cursor) = buffer.replay(Some(0));
        assert!(cursor.truncated);
        assert!(std::str::from_utf8(&replay).is_ok());
        assert_eq!(
            cursor.end - cursor.start,
            u64::try_from(replay.len()).unwrap()
        );
        assert_eq!(cursor.modes, None);
    }

    #[test]
    fn a_terminal_gets_a_utf8_locale_only_when_none_is_set() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| std::ffi::OsString::from(value))
            }
        };
        assert!(!locale_set(env(&[])));
        assert!(!locale_set(env(&[
            ("LANG", ""),
            ("LC_MESSAGES", "fr_FR.UTF-8")
        ])));
        assert!(locale_set(env(&[("LANG", "C")])));
        assert!(locale_set(env(&[("LC_CTYPE", "UTF-8")])));
        assert!(locale_set(env(&[("LC_ALL", "de_DE.UTF-8")])));

        let installed = |name: &str| ["ja_JP.UTF-8", "zh_CN.UTF-8", "en_GB.UTF-8"].contains(&name);
        let fallback = if cfg!(target_os = "macos") {
            "en_US.UTF-8"
        } else {
            "C.UTF-8"
        };
        assert_eq!(utf8_locale(Some("ja_JP"), installed), "ja_JP.UTF-8");
        assert_eq!(utf8_locale(Some("zh-Hans_CN"), installed), "zh_CN.UTF-8");
        assert_eq!(
            utf8_locale(Some("en_GB@rg=gbzzzz"), installed),
            "en_GB.UTF-8"
        );
        // English in Japan has no locale of its own.
        assert_eq!(utf8_locale(Some("en_JP"), installed), fallback);
        assert_eq!(utf8_locale(Some("es_419"), installed), fallback);
        assert_eq!(utf8_locale(Some("en"), installed), fallback);
        assert_eq!(utf8_locale(None, installed), fallback);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_terminal_reads_multibyte_input_as_characters() {
        if locale_set(|name| std::env::var_os(name)) {
            // A locale is already set here; terminals simply inherit it.
            return;
        }
        let manager = PtyManager::new(std::env::current_dir().unwrap(), EventBus::new(64));
        // The shell reports the locale it got and how many characters it read.
        let info = manager
            .create(PtyCreateRequest {
                command: Some("/bin/sh".into()),
                args: vec![
                    "-c".into(),
                    "read line; printf 'chars=%s ctype=%s\\n' \"${#line}\" \"$(locale | sed -n 's/^LC_CTYPE=//p')\"".into(),
                ],
                ..PtyCreateRequest::default()
            })
            .unwrap();
        let mut subscription = manager.subscribe(info.id, None).unwrap();
        manager.write(info.id, "日本\r".as_bytes()).unwrap();
        let mut output = String::from_utf8_lossy(&subscription.replay).into_owned();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !output.contains("ctype=") || !output.ends_with('\n') {
            let chunk = tokio::time::timeout_at(deadline, subscription.receiver.recv())
                .await
                .expect("the shell answers")
                .unwrap();
            output.push_str(&String::from_utf8_lossy(&chunk));
        }
        assert!(output.contains("chars=2 "), "{output:?}");
        assert!(output.contains("UTF-8"), "{output:?}");
    }

    #[cfg(unix)]
    #[test]
    fn windows_opening_a_terminal_at_once_share_one_shell() {
        let manager = PtyManager::new(std::env::current_dir().unwrap(), EventBus::new(64));
        // A shell that already exited is no terminal to share.
        let ended = manager
            .create(PtyCreateRequest {
                command: Some("/bin/sh".into()),
                args: vec!["-c".into(), "true".into()],
                ..PtyCreateRequest::default()
            })
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while manager.get(ended.id).unwrap().status != PtyStatus::Exited {
            assert!(std::time::Instant::now() < deadline, "the shell exits");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let request = PtyCreateRequest {
            command: Some("/bin/sh".into()),
            args: vec!["-c".into(), "sleep 30".into()],
            if_none: true,
            ..PtyCreateRequest::default()
        };
        let opened = std::thread::scope(|scope| {
            let windows: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        let info = manager.create(request.clone()).unwrap();
                        (info.id, info.created)
                    })
                })
                .collect();
            windows
                .into_iter()
                .map(|window| window.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(manager.list().len(), 2, "one shell for every window");
        assert!(opened.iter().all(|(id, _)| *id == opened[0].0));
        assert_ne!(opened[0].0, ended.id);
        // Only the window whose create started it is told so: for the others
        // its output is a shell's they did not start.
        assert_eq!(opened.iter().filter(|(_, created)| *created).count(), 1);
        assert!(manager.list().iter().all(|info| !info.created));
        // An explicit New terminal still opens another; "a terminal" then
        // keeps returning the oldest.
        let second = manager
            .create(PtyCreateRequest {
                if_none: false,
                ..request.clone()
            })
            .unwrap();
        assert_ne!(second.id, opened[0].0);
        assert!(second.created);
        let again = manager.create(request).unwrap();
        assert_eq!((again.id, again.created), (opened[0].0, false));
        assert_eq!(manager.list().len(), 3);
        for info in manager.list() {
            manager.delete(info.id).unwrap();
        }
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

    /// A server started in an agent's shell command (a `haarness serve`, say)
    /// holds that command's markers: `AGENT=1`, `HAARNESS=1`,
    /// `AI_AGENT=haarness` and its chat id, which names none of this server's
    /// chats. Its terminals are a person's, so they get none of them; an outer
    /// agent's own `AI_AGENT` is kept. Each case runs again in a process whose
    /// own environment holds them.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_terminal_never_inherits_a_chat_id_or_gets_the_agent_markers() {
        const NAME: &str = "tests::a_terminal_never_inherits_a_chat_id_or_gets_the_agent_markers";
        let Some(expected) = std::env::var_os("HAARNESS_PTY_EXPECTED") else {
            for (ai_agent, expected) in [
                ("haarness", "seen=unset|unset|unset|unset"),
                ("outer-agent", "seen=unset|unset|unset|outer-agent"),
            ] {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", NAME, "--nocapture"])
                    .env("HAARNESS_PTY_EXPECTED", expected)
                    .env("HAARNESS_SESSION_ID", "outer-chat")
                    .env("AGENT", "1")
                    .env("HAARNESS", "1")
                    .env("AI_AGENT", ai_agent)
                    .output()
                    .unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    output.status.success() && stdout.contains(&format!("terminal {expected}")),
                    "{ai_agent}: {stdout}{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        };
        let expected = expected.to_string_lossy().into_owned();
        let manager = PtyManager::new(std::env::current_dir().unwrap(), EventBus::new(64));
        let info = manager
            .create(PtyCreateRequest {
                command: Some("/bin/sh".into()),
                args: vec![
                    "-c".into(),
                    r#"printf 'seen=%s|%s|%s|%s\n' "${HAARNESS_SESSION_ID-unset}" "${AGENT-unset}" "${HAARNESS-unset}" "${AI_AGENT-unset}"; sleep 5"#.into(),
                ],
                ..PtyCreateRequest::default()
            })
            .unwrap();
        let mut subscription = manager.subscribe(info.id, Some(0)).unwrap();
        let mut output = String::from_utf8_lossy(&subscription.replay).into_owned();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !output.contains('\n') {
            let chunk = tokio::time::timeout_at(deadline, subscription.receiver.recv())
                .await
                .expect("the shell answers")
                .unwrap();
            output.push_str(&String::from_utf8_lossy(&chunk));
        }
        assert!(output.contains(&expected), "{output:?}");
        manager.delete(info.id).unwrap();
        println!("terminal {expected}");
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
