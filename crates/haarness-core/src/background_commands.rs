//! Shell commands the agent runs, which may outlive their call.
//!
//! Every `bash` call of an agent's turn is registered here under an id, so a
//! client can follow it and a foreground one can move to the background while
//! it runs. A command in the background keeps running after its call returned;
//! when it ends its chat is told (see the runtime's `commands` module, which
//! records and delivers). Ended commands stay listed for a while so a client
//! that looks a little later still finds how they ended.
//!
//! The process itself is the server's: it starts it, pumps its output into a
//! [`CommandOutput`] and reports its end; this module only asks it to stop
//! through [`ProcessControl`].

use std::fmt::Write as _;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use haarness_protocol::{
    ShellCommandEndedBy, ShellCommandInfo, ShellCommandMovedBy, ShellCommandOutput,
    ShellCommandRef, ShellCommandState, ThreadId, TurnId,
};
use parking_lot::Mutex;
use serde_json::json;
use tokio::{
    io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _},
    sync::{Notify, watch},
};
use tokio_util::sync::CancellationToken;

use crate::{CoreError, CoreResult, ExecutionSettings, ToolOutput};

/// How long an ended command stays listed.
pub(crate) const ENDED_KEPT: Duration = Duration::from_secs(10 * 60);
/// What a command keeps of its output in memory: its end.
pub const OUTPUT_TAIL_KEPT: usize = 64 * 1024;
/// What a command in the background, or one without a deadline, saves of its
/// output, from the start. 64 MiB, as much as a request body.
pub const OUTPUT_SAVED_LIMIT: u64 = 64 * 1024 * 1024;
/// How much of the command line a command reports.
const COMMAND_TEXT_LIMIT: usize = 8 * 1024;
/// A page of output when the reader names no limit, and the most it may name.
pub(crate) const PAGE_DEFAULT: usize = 64 * 1024;
pub(crate) const PAGE_MAX: usize = 1024 * 1024;
/// What the agent is shown of a command's output at once: in `bash_output`,
/// and of a moved command's output so far.
const AGENT_READ_BYTES: usize = 30 * 1024;
/// The end of the output a notice or a moved result carries.
const NOTICE_LINES: usize = 40;
const NOTICE_BYTES: usize = 8 * 1024;
/// The end of the output a checkpoint keeps.
const CHECKPOINT_TAIL_BYTES: usize = 4 * 1024;
/// A foreground command is reported to clients once it has run this long.
const ANNOUNCE_AFTER: Duration = Duration::from_secs(2);
/// At most one `command.output` event per command in this time.
const OUTPUT_EVENT_EVERY: Duration = Duration::from_secs(1);
/// How much of an unfinished line a later search for a match reads again.
const MATCH_OVERLAP: usize = 4 * 1024;
/// How long a stopped command's end is awaited before the stop answers.
const STOP_SETTLE: Duration = Duration::from_secs(5);

/// The signals Haarness sends a command's processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    Interrupt,
    Terminate,
    Kill,
}

impl StopSignal {
    /// Its name as a person reads it (`SIGINT`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text
            .trim()
            .trim_start_matches("SIG")
            .to_ascii_uppercase()
            .as_str()
        {
            "INT" => Some(Self::Interrupt),
            "TERM" => Some(Self::Terminate),
            "KILL" => Some(Self::Kill),
            _ => None,
        }
    }
}

/// How a command's process ended, once it has and all its output is in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessEnd {
    pub exit_code: Option<i32>,
    /// The signal that ended it (`SIGTERM`).
    pub signal: Option<String>,
    /// Processes it started were still running after it ended (`server &`
    /// in a command), holding its output open, and were stopped with it.
    pub leftovers: bool,
    /// Some of them were out of reach (a process in a session of its own)
    /// and still held its output open after the stop: they run on, and
    /// their output is no longer read.
    pub leftovers_running: bool,
    /// Reading its output failed; it was stopped.
    pub error: Option<String>,
}

/// The server's handle on a running command's processes.
pub trait ProcessControl: Send + Sync {
    /// Send `signal` to the command and every process it started.
    fn signal(&self, signal: StopSignal);
}

/// A started command, as the server hands it over: where its output goes,
/// when it ends, and how to stop it.
pub struct CommandProcess {
    pub output: Arc<CommandOutput>,
    /// `None` while it runs.
    pub end: watch::Receiver<Option<ProcessEnd>>,
    pub control: Arc<dyn ProcessControl>,
}

/// What a command printed: stdout and stderr together, in the order the
/// server read them, characters never split between reads. The start is
/// saved to a file (all of it while the command has a deadline in the
/// foreground, else its first [`OUTPUT_SAVED_LIMIT`] bytes) and the end is
/// kept in memory ([`OUTPUT_TAIL_KEPT`]), so a long-running command's middle
/// can be dropped.
pub struct CommandOutput {
    path: PathBuf,
    state: Mutex<OutputState>,
    size: watch::Sender<u64>,
    file: tokio::sync::Mutex<Option<tokio::fs::File>>,
}

struct OutputState {
    size: u64,
    /// Bytes from the start written to the file.
    saved: u64,
    save_limit: u64,
    /// The last bytes, at most [`OUTPUT_TAIL_KEPT`].
    tail: Vec<u8>,
    /// The file is still there to read.
    file_kept: bool,
}

/// A page of output (see [`CommandOutput::read`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPage {
    pub start: u64,
    pub end: u64,
    pub bytes: Vec<u8>,
    pub size: u64,
}

impl OutputPage {
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

impl CommandOutput {
    #[must_use]
    pub fn new(path: PathBuf, file: tokio::fs::File, save_limit: u64) -> Self {
        Self {
            path,
            state: Mutex::new(OutputState {
                size: 0,
                saved: 0,
                save_limit,
                tail: Vec::new(),
                file_kept: true,
            }),
            size: watch::Sender::new(0),
            file: tokio::sync::Mutex::new(Some(file)),
        }
    }

    /// Add what the command printed. One writer at a time: the server's pump.
    pub async fn append(&self, bytes: &[u8]) -> std::io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let room = {
            let state = self.state.lock();
            let room = state.save_limit.saturating_sub(state.saved);
            usize::try_from(room).unwrap_or(usize::MAX).min(bytes.len())
        };
        if room > 0
            && let Some(file) = self.file.lock().await.as_mut()
        {
            // Flushed before it counts as saved: a reader opens the file
            // and reads up to that point.
            file.write_all(&bytes[..room]).await?;
            file.flush().await?;
        }
        let size = {
            let mut state = self.state.lock();
            state.saved += room as u64;
            state.size += bytes.len() as u64;
            state.tail.extend_from_slice(bytes);
            if state.tail.len() > OUTPUT_TAIL_KEPT {
                let cut = state.tail.len() - OUTPUT_TAIL_KEPT;
                state.tail.drain(..cut);
            }
            state.size
        };
        self.size.send_replace(size);
        Ok(())
    }

    /// The command printed its last: flush and close the file.
    pub async fn finish(&self) -> std::io::Result<()> {
        if let Some(mut file) = self.file.lock().await.take() {
            file.flush().await?;
        }
        Ok(())
    }

    /// Save at most `limit` bytes from the start from now on.
    pub fn limit_saving(&self, limit: u64) {
        let mut state = self.state.lock();
        state.save_limit = state.save_limit.min(limit);
    }

    #[must_use]
    pub fn size(&self) -> u64 {
        self.state.lock().size
    }

    /// Bytes from the start saved to [`Self::path`].
    #[must_use]
    pub fn saved(&self) -> u64 {
        let state = self.state.lock();
        if state.file_kept { state.saved } else { 0 }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the file is still there.
    #[must_use]
    pub fn file_kept(&self) -> bool {
        self.state.lock().file_kept
    }

    /// Follow the size as it grows.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.size.subscribe()
    }

    /// Remove the file once every byte of the output is in memory too, as a
    /// short command's output is. Answers whether it was removed.
    pub async fn discard_file(&self) -> bool {
        {
            let mut state = self.state.lock();
            if !state.file_kept || state.size > state.tail.len() as u64 {
                return false;
            }
            state.file_kept = false;
        }
        drop(self.file.lock().await.take());
        let _ = tokio::fs::remove_file(&self.path).await;
        true
    }

    /// The last `max` bytes of the output in memory, starting on a whole
    /// character; whether earlier output was left out.
    #[must_use]
    pub fn tail(&self, max: usize) -> (Vec<u8>, bool) {
        let state = self.state.lock();
        let kept = &state.tail[state.tail.len().saturating_sub(max)..];
        let cut = state.size > kept.len() as u64;
        let kept = if cut { whole_start(kept) } else { kept };
        (kept.to_vec(), cut)
    }

    /// Up to `limit` bytes from `cursor`, cut on character boundaries. When
    /// the output at `cursor` is no longer kept, the page starts at the
    /// earliest byte that is.
    pub async fn read(&self, cursor: u64, limit: usize) -> std::io::Result<OutputPage> {
        let page = self.read_bytes(cursor, limit.max(1) as u64).await?;
        // A limit smaller than the character at the cursor still gets it.
        if page.bytes.is_empty() && page.end < page.size {
            return self.read_bytes(page.start, 4).await;
        }
        Ok(page)
    }

    async fn read_bytes(&self, cursor: u64, limit: u64) -> std::io::Result<OutputPage> {
        let (size, saved, tail_start, file_kept) = {
            let state = self.state.lock();
            (
                state.size,
                state.saved,
                state.size - state.tail.len() as u64,
                state.file_kept,
            )
        };
        let mut start = cursor.min(size);
        let mut forced = false;
        if start < tail_start && (start >= saved || !file_kept) {
            start = tail_start;
            forced = true;
        }
        let end = start.saturating_add(limit).min(size);
        let mut bytes = if start >= tail_start {
            let state = self.state.lock();
            // The tail may have moved on since: read what is still there.
            let tail_start = state.size - state.tail.len() as u64;
            if start < tail_start {
                forced = true;
                start = tail_start;
            }
            let from = usize::try_from(start - tail_start).unwrap_or(usize::MAX);
            let to = usize::try_from(end.max(start) - tail_start)
                .unwrap_or(usize::MAX)
                .min(state.tail.len());
            state.tail[from.min(to)..to].to_vec()
        } else {
            let to = end.min(saved);
            let mut file = tokio::fs::File::open(&self.path).await?;
            file.seek(std::io::SeekFrom::Start(start)).await?;
            let mut bytes = vec![0; usize::try_from(to - start).unwrap_or(0)];
            file.read_exact(&mut bytes).await?;
            bytes
        };
        if forced {
            let skipped = bytes.len() - whole_start(&bytes).len();
            bytes.drain(..skipped);
            start += skipped as u64;
        }
        let mut end = start + bytes.len() as u64;
        if end < size {
            let whole = whole_end(&bytes);
            bytes.truncate(whole);
            end = start + bytes.len() as u64;
        }
        Ok(OutputPage {
            start,
            end,
            bytes,
            size,
        })
    }
}

/// `bytes` without the continuation bytes of a character that began before
/// it.
#[must_use]
pub fn whole_start(bytes: &[u8]) -> &[u8] {
    let partial = bytes
        .iter()
        .take(3)
        .take_while(|byte| **byte & 0xC0 == 0x80)
        .count();
    &bytes[partial..]
}

/// How much of `bytes` ends on a whole character: a character whose bytes go
/// on past the end is left out. Bytes that are no character at all are kept.
#[must_use]
pub fn whole_end(bytes: &[u8]) -> usize {
    let lead = bytes
        .iter()
        .enumerate()
        .rev()
        .take(4)
        .find(|(_, byte)| **byte & 0xC0 != 0x80);
    let Some((index, byte)) = lead else {
        return bytes.len();
    };
    let length = match byte {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return bytes.len(),
    };
    if bytes.len() - index < length {
        index
    } else {
        bytes.len()
    }
}

/// When a command's time limit counts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Limit {
    SinceStart(Duration),
    SinceBackground(Duration),
}

/// What a command may do, from the settings, its call and its chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandPolicy {
    /// In the foreground it is stopped this long after it started.
    foreground_deadline: Option<Duration>,
    /// In the foreground it moves to the background this long after it
    /// started.
    move_after: Option<Duration>,
    /// A person may send it to the background.
    movable: bool,
    /// In the background it is stopped after this.
    background_limit: Option<Limit>,
    stop_grace: Duration,
    /// Its end is told to its chat when it runs in the background.
    delivers_notice: bool,
    per_chat: usize,
}

impl CommandPolicy {
    /// `follows`: the turn's client follows background work up;
    /// `subagent`: the chat is a subagent's (or a workflow's);
    /// `unattended`: nobody watches the turn (a scheduled run), whose
    /// commands in the background never run unlimited;
    /// `timeout_ms`: the call's own timeout, when it gave one.
    pub(crate) fn new(
        settings: &ExecutionSettings,
        follows: bool,
        subagent: bool,
        unattended: bool,
        timeout_ms: Option<u64>,
    ) -> Self {
        let millis = Duration::from_millis;
        let explicit = timeout_ms.map(|ms| (ms > 0).then(|| millis(ms)));
        let deadline = (settings.shell_timeout_ms > 0).then(|| millis(settings.shell_timeout_ms));
        let interactive = follows && !subagent;
        let move_after = (interactive && explicit.is_none() && settings.shell_move_after_ms > 0)
            .then(|| millis(settings.shell_move_after_ms));
        let foreground_deadline = match explicit {
            Some(limit) => limit,
            // A command that moves is never stopped for running long in the
            // foreground.
            None if move_after.is_some() => None,
            None => deadline,
        };
        let background_limit = match explicit {
            Some(None) if unattended => deadline.map(Limit::SinceStart),
            Some(limit) => limit.map(Limit::SinceStart),
            None if interactive => (settings.shell_background_max_ms > 0)
                .then(|| Limit::SinceBackground(millis(settings.shell_background_max_ms))),
            None => deadline.map(Limit::SinceStart),
        };
        Self {
            foreground_deadline,
            move_after,
            movable: !subagent,
            background_limit,
            stop_grace: millis(settings.shell_stop_grace_ms),
            delivers_notice: !subagent,
            per_chat: settings.shell_background_per_chat,
        }
    }

    fn stops_at(
        &self,
        started: DateTime<Utc>,
        background_at: Option<DateTime<Utc>>,
    ) -> Option<DateTime<Utc>> {
        let after = |from: DateTime<Utc>, limit: Duration| {
            chrono::Duration::from_std(limit)
                .ok()
                .map(|limit| from + limit)
        };
        match background_at {
            None => self
                .foreground_deadline
                .and_then(|limit| after(started, limit)),
            Some(moved) => match self.background_limit? {
                Limit::SinceStart(limit) => after(started, limit),
                Limit::SinceBackground(limit) => after(moved, limit),
            },
        }
    }
}

/// What stops a command, once something asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StopCause {
    ended_by: ShellCommandEndedBy,
    state: ShellCommandState,
    /// One signal was sent, which the command may not heed.
    soft: bool,
}

/// How a stop goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopSequence {
    /// Interrupt, terminate, then kill, `grace` apart.
    Graceful(Duration),
    /// Kill at once.
    Now,
    /// This signal alone.
    Only(StopSignal),
}

/// What a command's chat does as the command lives: the runtime's side.
#[async_trait::async_trait]
pub(crate) trait CommandHooks: Send + Sync {
    /// `command.updated`.
    fn updated(&self, info: &ShellCommandInfo);
    /// `command.output`.
    fn printed(&self, info: &ShellCommandInfo, size: u64);
    /// It runs in the background now: started there, or moved by `moved`.
    async fn backgrounded(&self, entry: &CommandEntry, moved: Option<ShellCommandMovedBy>);
    /// It ended in the background; `notify`: its chat is told.
    async fn ended(&self, entry: &CommandEntry, notify: bool);
}

/// One command.
pub(crate) struct CommandEntry {
    info: watch::Sender<ShellCommandInfo>,
    process: CommandProcess,
    /// The folder it runs in.
    cwd: PathBuf,
    policy: CommandPolicy,
    hooks: Arc<dyn CommandHooks>,
    /// Where the agent last read its output to.
    read_cursor: Mutex<u64>,
    stopping: Mutex<Option<StopCause>>,
    /// The result of its call is recorded: a notice may follow it.
    released: watch::Sender<bool>,
    /// A deadline changed.
    wake: Notify,
    /// Clients were told of it (`command.updated`).
    announced: AtomicBool,
}

impl CommandEntry {
    pub(crate) fn info(&self) -> ShellCommandInfo {
        self.info.borrow().clone()
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The call's result is recorded (or its turn is over): a notice may
    /// follow it.
    pub(crate) fn release(&self) {
        self.released.send_replace(true);
    }

    pub(crate) async fn wait_released(&self) {
        let mut released = self.released.subscribe();
        let _ = released.wait_for(|released| *released).await;
    }

    fn publish(&self) {
        self.announced.store(true, Ordering::SeqCst);
        self.hooks.updated(&self.info());
    }

    /// Wait until it has ended and its end is recorded, at most `limit`.
    pub(crate) async fn settled(&self, limit: Duration) -> ShellCommandInfo {
        let mut info = self.info.subscribe();
        let _ = tokio::time::timeout(limit, info.wait_for(|info| !info.state.is_live())).await;
        self.info()
    }

    /// Send it to the background while it runs in the foreground. Answers
    /// whether it moved.
    pub(crate) fn move_to_background(&self, by: ShellCommandMovedBy) -> bool {
        if by == ShellCommandMovedBy::Person && !self.policy.movable {
            return false;
        }
        let now = Utc::now();
        let mut moved = false;
        self.info.send_if_modified(|info| {
            if info.background || info.state != ShellCommandState::Running {
                return false;
            }
            info.background = true;
            info.moved_by = Some(by);
            info.background_at = Some(now);
            info.moves_at = None;
            info.stops_at = self.policy.stops_at(info.started_at, Some(now));
            info.delivers_notice = self.policy.delivers_notice;
            moved = true;
            true
        });
        if moved {
            self.process.output.limit_saving(OUTPUT_SAVED_LIMIT);
            self.wake.notify_one();
        }
        moved
    }

    /// Send it to the background for `by`, telling clients and its chat.
    /// Answers whether it moved.
    pub(crate) async fn send_to_background(&self, by: ShellCommandMovedBy) -> bool {
        if !self.move_to_background(by) {
            return false;
        }
        self.publish();
        self.hooks.backgrounded(self, Some(by)).await;
        true
    }

    /// Stop it, by `ended_by`. Answers whether a stop began (false when it
    /// had already ended, or a stop is under way).
    pub(crate) fn stop(
        self: &Arc<Self>,
        ended_by: ShellCommandEndedBy,
        state: ShellCommandState,
        sequence: StopSequence,
    ) -> bool {
        if !self.info.borrow().state.is_live() {
            return false;
        }
        let only = matches!(sequence, StopSequence::Only(_));
        {
            let mut stopping = self.stopping.lock();
            match *stopping {
                // A stop is under way: a kill speeds it up, one signal is
                // sent, anything else waits for it.
                Some(cause) if !cause.soft => {
                    match sequence {
                        StopSequence::Now => self.process.control.signal(StopSignal::Kill),
                        StopSequence::Only(signal) => self.process.control.signal(signal),
                        StopSequence::Graceful(_) => {}
                    }
                    return false;
                }
                _ => {
                    *stopping = Some(StopCause {
                        ended_by,
                        state,
                        soft: only,
                    });
                }
            }
        }
        if !only {
            let changed = self.info.send_if_modified(|info| {
                if info.state == ShellCommandState::Running {
                    info.state = ShellCommandState::Stopping;
                    true
                } else {
                    false
                }
            });
            if changed && (self.info.borrow().background || self.announced.load(Ordering::SeqCst)) {
                self.publish();
            }
        }
        let grace = match sequence {
            StopSequence::Now => {
                self.process.control.signal(StopSignal::Kill);
                return true;
            }
            StopSequence::Only(signal) => {
                self.process.control.signal(signal);
                return true;
            }
            StopSequence::Graceful(grace) => grace,
        };
        if grace.is_zero() || tokio::runtime::Handle::try_current().is_err() {
            self.process.control.signal(StopSignal::Kill);
            return true;
        }
        let entry = Arc::clone(self);
        tokio::spawn(async move {
            let mut end = entry.process.end.clone();
            for signal in [StopSignal::Interrupt, StopSignal::Terminate] {
                entry.process.control.signal(signal);
                if let Ok(_ended) = tokio::time::timeout(grace, end.wait_for(Option::is_some)).await
                {
                    return;
                }
            }
            entry.process.control.signal(StopSignal::Kill);
        });
        true
    }

    /// Follow the command from start to end: its time limits, its move to
    /// the background, the events clients follow, and its end.
    async fn follow(self: Arc<Self>) {
        let mut end = self.process.end.clone();
        let mut size = self.process.output.subscribe();
        let started = tokio::time::Instant::now();
        let mut printed_at: Option<tokio::time::Instant> = None;
        let mut printed_pending = false;
        loop {
            if end.borrow().is_some() {
                break;
            }
            let info = self.info();
            let now = Utc::now();
            let until = |at: DateTime<Utc>| {
                tokio::time::Instant::now() + (at - now).to_std().unwrap_or(Duration::ZERO)
            };
            let mut next: Option<tokio::time::Instant> = None;
            let mut consider = |at: tokio::time::Instant| {
                next = Some(next.map_or(at, |next| next.min(at)));
            };
            if info.state == ShellCommandState::Running {
                if let Some(at) = info.stops_at {
                    consider(until(at));
                }
                if let Some(at) = info.moves_at {
                    consider(until(at));
                }
            }
            let announced = self.announced.load(Ordering::SeqCst);
            if !announced && !info.background {
                consider(started + ANNOUNCE_AFTER);
            }
            if printed_pending && announced {
                consider(
                    printed_at.map_or_else(tokio::time::Instant::now, |at| at + OUTPUT_EVENT_EVERY),
                );
            }
            let sleep = async {
                match next {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                changed = end.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                changed = size.changed(), if !printed_pending => {
                    printed_pending = changed.is_ok();
                }
                () = sleep => {}
                () = self.wake.notified() => {}
            }
            if end.borrow().is_some() {
                break;
            }
            let info = self.info();
            let now = Utc::now();
            if info.state == ShellCommandState::Running {
                if info.stops_at.is_some_and(|at| at <= now) {
                    let sequence = if info.background {
                        StopSequence::Graceful(self.policy.stop_grace)
                    } else {
                        // As before: a foreground deadline kills at once.
                        StopSequence::Now
                    };
                    self.stop(
                        ShellCommandEndedBy::TimeLimit,
                        ShellCommandState::TimedOut,
                        sequence,
                    );
                    continue;
                }
                if info.moves_at.is_some_and(|at| at <= now)
                    && self.send_to_background(ShellCommandMovedBy::Limit).await
                {
                    continue;
                }
            }
            if !self.announced.load(Ordering::SeqCst)
                && !info.background
                && started.elapsed() >= ANNOUNCE_AFTER
            {
                self.publish();
            }
            if printed_pending
                && self.announced.load(Ordering::SeqCst)
                && printed_at.is_none_or(|at| at.elapsed() >= OUTPUT_EVENT_EVERY)
            {
                printed_pending = false;
                printed_at = Some(tokio::time::Instant::now());
                let size = self.process.output.size();
                self.info.send_modify(|info| info.output_bytes = size);
                self.hooks.printed(&self.info(), size);
            }
        }
        self.finish().await;
    }

    /// Record how it ended.
    async fn finish(&self) {
        let end = self
            .process
            .end
            .borrow()
            .clone()
            .unwrap_or_else(|| ProcessEnd {
                error: Some("the process was lost".into()),
                ..ProcessEnd::default()
            });
        let cause = *self.stopping.lock();
        let size = self.process.output.size();
        let ended_by = cause.map(|cause| cause.ended_by);
        // Its chat is told of an end the agent did not cause itself and
        // that was not a stop of everything.
        let notify = self.policy.delivers_notice
            && self.info.borrow().background
            && !matches!(
                ended_by,
                Some(
                    ShellCommandEndedBy::Agent
                        | ShellCommandEndedBy::ChatStopped
                        | ShellCommandEndedBy::Shutdown
                        | ShellCommandEndedBy::TurnEnded
                )
            );
        self.info.send_modify(|info| {
            info.state = cause.map_or(ShellCommandState::Exited, |cause| cause.state);
            info.ended_by = ended_by;
            info.exit_code = end.exit_code;
            info.signal.clone_from(&end.signal);
            info.ended_at = Some(Utc::now());
            info.stops_at = None;
            info.moves_at = None;
            info.output_bytes = size;
            info.delivers_notice = notify;
        });
        let info = self.info();
        if info.background || self.announced.load(Ordering::SeqCst) {
            self.publish();
        }
        if info.background {
            self.hooks.ended(self, notify).await;
        }
    }

    /// The end of its output, at most `lines` lines and `bytes` bytes.
    pub(crate) fn tail_text(&self, lines: usize, bytes: usize) -> (String, bool) {
        let (tail, cut) = self.process.output.tail(bytes);
        let text = String::from_utf8_lossy(&tail);
        let all = text.lines().collect::<Vec<_>>();
        let kept = all.len().saturating_sub(lines);
        (all[kept..].join("\n"), cut || kept > 0)
    }

    /// The end of its output a checkpoint keeps.
    pub(crate) fn checkpoint_tail(&self) -> String {
        let (tail, _) = self.process.output.tail(CHECKPOINT_TAIL_BYTES);
        String::from_utf8_lossy(&tail).into_owned()
    }

    /// The file that keeps its output, when there is one.
    pub(crate) fn output_file(&self) -> Option<PathBuf> {
        self.process
            .output
            .file_kept()
            .then(|| self.process.output.path().to_path_buf())
    }

    /// The hidden message its chat gets when it ends in the background.
    pub(crate) fn notice(&self) -> String {
        let info = self.info();
        let (output, _) = self.tail_text(NOTICE_LINES, NOTICE_BYTES);
        let file = self
            .output_file()
            .map(|path| format!("\n<output_file>{}</output_file>", path.display()))
            .unwrap_or_default();
        format!(
            "{}\n<summary>{}</summary>\n<command_output>\n{output}\n</command_output>{file}\n</command>",
            notice_tag(&info),
            ended_summary(&info),
        )
    }
}

/// The opening tag of a command's notice.
pub(crate) fn notice_tag(info: &ShellCommandInfo) -> String {
    let state = match info.state {
        ShellCommandState::TimedOut => "timed_out",
        ShellCommandState::Stopped => "stopped",
        ShellCommandState::Interrupted => "interrupted",
        _ => "exited",
    };
    let mut tag = format!("<command id=\"{}\" state=\"{state}\"", info.id);
    if let Some(code) = info.exit_code {
        let _ = write!(tag, " exit=\"{code}\"");
    }
    if let Some(signal) = &info.signal {
        let _ = write!(tag, " signal=\"{signal}\"");
    }
    tag.push('>');
    tag
}

/// One sentence on how a command ended.
pub(crate) fn ended_summary(info: &ShellCommandInfo) -> String {
    let line = command_label(&info.command);
    let ran = info
        .ended_at
        .and_then(|ended| (ended - info.started_at).to_std().ok())
        .map(spoken_duration)
        .unwrap_or_default();
    let how = match (info.state, info.ended_by) {
        (ShellCommandState::TimedOut, _) => format!("ran out of time and was stopped after {ran}"),
        (ShellCommandState::Interrupted, _) => {
            "was cut off: Haarness stopped while it ran, and it was not started again".into()
        }
        (ShellCommandState::Stopped, Some(ShellCommandEndedBy::Person)) => {
            format!("was stopped by the person after {ran}")
        }
        (ShellCommandState::Stopped, _) => format!("was stopped after {ran}"),
        _ => match (info.exit_code, &info.signal) {
            (Some(0), _) => format!("completed after {ran}"),
            (Some(code), _) => format!("exited with code {code} after {ran}"),
            (None, Some(signal)) => format!("was ended by {signal} after {ran}"),
            (None, None) => format!("ended after {ran}"),
        },
    };
    format!("`{line}` {how}.")
}

/// The first line of a command, shortened for a sentence.
fn command_label(command: &str) -> String {
    let line = command.lines().next().unwrap_or_default().trim();
    let mut label = line.chars().take(120).collect::<String>();
    if label.len() < line.len() || command.trim().lines().nth(1).is_some() {
        label.push('…');
    }
    label
}

/// `4s`, `1m 12s`, `2h 5m`.
#[must_use]
pub fn spoken_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// The commands of every chat this runtime serves, by id.
#[derive(Default)]
pub(crate) struct ShellCommands {
    entries: DashMap<String, Arc<CommandEntry>>,
}

impl ShellCommands {
    /// The command `id`, while it is listed.
    pub(crate) fn get(&self, id: &str) -> Option<Arc<CommandEntry>> {
        self.forget_ended();
        self.entries.get(id).map(|entry| Arc::clone(entry.value()))
    }

    /// The commands of `threads` (every chat when `None`): live ones first,
    /// oldest first, then ended ones, the latest end first.
    pub(crate) fn list(&self, threads: Option<&HashSet<ThreadId>>) -> Vec<ShellCommandInfo> {
        self.forget_ended();
        let mut commands = self
            .entries
            .iter()
            .map(|entry| entry.value().info())
            .filter(|info| threads.is_none_or(|threads| threads.contains(&info.thread_id)))
            .collect::<Vec<_>>();
        commands.sort_by(|left, right| {
            right
                .state
                .is_live()
                .cmp(&left.state.is_live())
                .then_with(|| {
                    if left.state.is_live() {
                        left.started_at.cmp(&right.started_at)
                    } else {
                        right.ended_at.cmp(&left.ended_at)
                    }
                })
                .then_with(|| left.id.cmp(&right.id))
        });
        commands
    }

    /// The commands still listed matching `keep`.
    pub(crate) fn all(&self, keep: impl Fn(&CommandEntry) -> bool) -> Vec<Arc<CommandEntry>> {
        self.entries
            .iter()
            .filter(|entry| keep(entry.value()))
            .map(|entry| Arc::clone(entry.value()))
            .collect()
    }

    /// The live commands matching `keep`.
    pub(crate) fn live(&self, keep: impl Fn(&CommandEntry) -> bool) -> Vec<Arc<CommandEntry>> {
        self.entries
            .iter()
            .filter(|entry| entry.value().info.borrow().state.is_live() && keep(entry.value()))
            .map(|entry| Arc::clone(entry.value()))
            .collect()
    }

    /// Commands of `threads` running in the background now.
    pub(crate) fn running_in_background(&self, threads: &HashSet<ThreadId>) -> u32 {
        let count = self
            .live(|entry| {
                let info = entry.info.borrow();
                info.background && threads.contains(&info.thread_id)
            })
            .len();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// Drop ended commands listed longer than [`ENDED_KEPT`].
    fn forget_ended(&self) {
        let Ok(kept) = chrono::Duration::from_std(ENDED_KEPT) else {
            return;
        };
        let cutoff = Utc::now() - kept;
        self.entries.retain(|_, entry| {
            let info = entry.info.borrow();
            info.state.is_live() || info.ended_at.is_none_or(|ended| ended > cutoff)
        });
    }
}

/// A new command id: short, as the agent names it.
fn command_id() -> String {
    // The random end of a time-ordered id.
    let id = uuid::Uuid::now_v7().simple().to_string();
    format!("cmd-{}", &id[id.len() - 10..])
}

/// The command a `bash` call of an agent's turn runs, handed to the shell
/// tool with the call's settings ([`ExecutionSettings::shell_call`]).
#[derive(Clone)]
pub struct ShellCall(Arc<ShellCallInner>);

pub(crate) struct ShellCallInner {
    pub(crate) commands: Arc<ShellCommands>,
    pub(crate) hooks: Arc<dyn CommandHooks>,
    pub(crate) thread_id: ThreadId,
    pub(crate) root_thread_id: ThreadId,
    pub(crate) turn_id: TurnId,
    pub(crate) call_id: String,
    pub(crate) settings: ExecutionSettings,
    pub(crate) follows: bool,
    pub(crate) subagent: bool,
    pub(crate) unattended: bool,
}

impl std::fmt::Debug for ShellCall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShellCall")
            .field("thread_id", &self.0.thread_id)
            .field("call_id", &self.0.call_id)
            .field("follows", &self.0.follows)
            .field("subagent", &self.0.subagent)
            .finish_non_exhaustive()
    }
}

/// What the shell tool asks of a command.
#[derive(Debug, Clone)]
pub struct CommandRequest {
    /// The line it runs.
    pub command: String,
    /// The folder it runs in.
    pub cwd: PathBuf,
    /// The folder the call named, as given.
    pub workdir: Option<String>,
    /// The call's own timeout in milliseconds, when it gave one.
    pub timeout_ms: Option<u64>,
    /// `run_in_background`.
    pub background: bool,
}

/// How a call's command went.
pub enum CommandRun {
    /// It ended while the call waited: the shell tool reports its output.
    Ended {
        reference: ShellCommandRef,
        end: ProcessEnd,
    },
    /// It runs on in the background: the call's result.
    Background(ToolOutput),
}

/// Stops a foreground command its call no longer waits for: one whose call
/// was dropped without a stop.
struct Foreground {
    entry: Arc<CommandEntry>,
    armed: bool,
}

impl Drop for Foreground {
    fn drop(&mut self) {
        if self.armed && !self.entry.info.borrow().background {
            self.entry.stop(
                ShellCommandEndedBy::TurnEnded,
                ShellCommandState::Stopped,
                StopSequence::Now,
            );
        }
    }
}

impl ShellCall {
    pub(crate) fn new(inner: ShellCallInner) -> Self {
        Self(Arc::new(inner))
    }

    /// The rules for a command of this call that gave `timeout_ms`.
    fn policy(&self, timeout_ms: Option<u64>) -> CommandPolicy {
        CommandPolicy::new(
            &self.0.settings,
            self.0.follows,
            self.0.subagent,
            self.0.unattended,
            timeout_ms,
        )
    }

    /// Whether all of the command's output is saved, from the start: only
    /// while it runs in the foreground with a deadline. Otherwise (no
    /// deadline, or in the background) its first [`OUTPUT_SAVED_LIMIT`]
    /// bytes are.
    #[must_use]
    pub fn saves_whole_output(&self, request: &CommandRequest) -> bool {
        !request.background
            && self
                .policy(request.timeout_ms)
                .foreground_deadline
                .is_some()
    }

    /// Whether the command may start; a call past the chat's cap of
    /// commands in the background is refused before anything runs.
    pub fn check_start(&self, background: bool) -> CoreResult<()> {
        let cap = self.0.settings.shell_background_per_chat;
        if !background || cap == 0 {
            return Ok(());
        }
        let running = self
            .0
            .commands
            .live(|entry| {
                let info = entry.info.borrow();
                info.background && info.thread_id == self.0.thread_id
            })
            .len();
        if running >= cap {
            return Err(CoreError::Tool(format!(
                "This chat already runs {running} commands in the background, as many as execution.shell_background_per_chat allows. Stop one with bash_stop, or run this one in the foreground."
            )));
        }
        Ok(())
    }

    /// Run `process` as this call's command: register it, then wait for it
    /// in the foreground until it ends, moves to the background, runs out of
    /// time or the turn stops; a command started in the background answers
    /// at once.
    pub async fn run(
        &self,
        process: CommandProcess,
        request: CommandRequest,
        cancel: &CancellationToken,
    ) -> CoreResult<CommandRun> {
        let call = &self.0;
        let policy = self.policy(request.timeout_ms);
        let started_at = Utc::now();
        let background_at = request.background.then_some(started_at);
        let mut command = request.command.clone();
        if command.len() > COMMAND_TEXT_LIMIT {
            let cut = (0..=COMMAND_TEXT_LIMIT)
                .rev()
                .find(|index| command.is_char_boundary(*index))
                .unwrap_or(0);
            command.truncate(cut);
        }
        let info = ShellCommandInfo {
            id: command_id(),
            thread_id: call.thread_id,
            root_thread_id: call.root_thread_id,
            turn_id: call.turn_id,
            call_id: call.call_id.clone(),
            command,
            workdir: request.workdir.clone(),
            background: request.background,
            moved_by: request.background.then_some(ShellCommandMovedBy::Agent),
            state: ShellCommandState::Running,
            exit_code: None,
            signal: None,
            ended_by: None,
            started_at,
            background_at,
            ended_at: None,
            stops_at: policy.stops_at(started_at, background_at),
            moves_at: if request.background {
                None
            } else {
                policy.move_after.and_then(|after| {
                    chrono::Duration::from_std(after)
                        .ok()
                        .map(|after| started_at + after)
                })
            },
            output_bytes: 0,
            delivers_notice: request.background && policy.delivers_notice,
        };
        if request.background {
            process.output.limit_saving(OUTPUT_SAVED_LIMIT);
        }
        let entry = Arc::new(CommandEntry {
            info: watch::Sender::new(info.clone()),
            process,
            cwd: request.cwd.clone(),
            policy,
            hooks: Arc::clone(&call.hooks),
            read_cursor: Mutex::new(0),
            stopping: Mutex::new(None),
            released: watch::Sender::new(false),
            wake: Notify::new(),
            announced: AtomicBool::new(false),
        });
        call.commands.forget_ended();
        call.commands
            .entries
            .insert(info.id.clone(), Arc::clone(&entry));
        tokio::spawn(Arc::clone(&entry).follow());
        let reference = |info: &ShellCommandInfo| ShellCommandRef {
            id: info.id.clone(),
            background: info.background,
            started_at: info.started_at,
        };
        if request.background {
            entry.publish();
            call.hooks.backgrounded(&entry, None).await;
            return Ok(CommandRun::Background(started_output(&entry)));
        }
        let mut guard = Foreground {
            entry: Arc::clone(&entry),
            armed: true,
        };
        let mut watched = entry.info.subscribe();
        let outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            changed = watched.wait_for(|info| info.background || !info.state.is_live()) => {
                changed.ok().map(|info| info.clone())
            }
        };
        let Some(current) = outcome else {
            // Stopped with its turn: kill it, as before, and wait for it.
            entry.stop(
                ShellCommandEndedBy::TurnEnded,
                ShellCommandState::Stopped,
                StopSequence::Now,
            );
            entry.settled(STOP_SETTLE).await;
            return Err(CoreError::Cancelled);
        };
        guard.armed = false;
        if current.background {
            *entry.read_cursor.lock() = entry.process.output.size();
            return Ok(CommandRun::Background(moved_output(&entry)));
        }
        let end = entry.process.end.borrow().clone().unwrap_or_default();
        if current.state == ShellCommandState::TimedOut {
            return Err(CoreError::Tool(format!(
                "command timed out after {} ms; output saved to {}",
                request.timeout_ms.unwrap_or(call.settings.shell_timeout_ms),
                entry.process.output.path().display()
            )));
        }
        if let Some(error) = &end.error {
            return Err(CoreError::Tool(error.clone()));
        }
        Ok(CommandRun::Ended {
            reference: reference(&current),
            end,
        })
    }
}

/// When its time limit stops it, in words, for the agent.
fn limit_note(info: &ShellCommandInfo) -> String {
    info.stops_at
        .and_then(|at| (at - Utc::now()).to_std().ok())
        .map(|left| {
            format!(
                " It is stopped if it still runs {} from now.",
                spoken_duration(left)
            )
        })
        .unwrap_or_default()
}

/// What the agent is told when a command starts in the background.
fn started_output(entry: &CommandEntry) -> ToolOutput {
    let info = entry.info();
    let follow_up = if entry.policy.delivers_notice {
        "When it ends, a message gives its exit status and the last lines it printed. It keeps running after this turn."
    } else {
        "It stops when your turn ends, and no message tells you when it ends: wait for it or read it with bash_output."
    };
    ToolOutput {
        text: format!(
            "Command {} runs in the background. {follow_up}{} Read its output with bash_output and stop it with bash_stop, using this id.",
            info.id,
            limit_note(&info),
        ),
        metadata: json!({"command": ShellCommandRef {
            id: info.id.clone(),
            background: true,
            started_at: info.started_at,
        }}),
    }
}

/// What the agent is told when its foreground command moves to the
/// background.
fn moved_output(entry: &CommandEntry) -> ToolOutput {
    let info = entry.info();
    let ran = info
        .background_at
        .and_then(|at| (at - info.started_at).to_std().ok())
        .map(spoken_duration)
        .unwrap_or_default();
    let why = match info.moved_by {
        Some(ShellCommandMovedBy::Person) => "the person sent it to the background",
        _ => "it ran long, so it moved to the background",
    };
    let follow_up = if entry.policy.delivers_notice {
        "When it ends, a message gives its exit status and the last lines it printed."
    } else {
        "It stops when your turn ends; wait for it or read it with bash_output."
    };
    let (output, cut) = entry.tail_text(NOTICE_LINES, NOTICE_BYTES);
    let output = if output.is_empty() {
        "(no output yet)".to_owned()
    } else if cut {
        format!("(earlier output left out; read it with bash_output and cursor 0)\n{output}")
    } else {
        output
    };
    ToolOutput {
        text: format!(
            "Still running after {ran}: {why} as command {}. {follow_up}{} Read its output with bash_output and stop it with bash_stop, using this id.\nOutput so far:\n{output}",
            info.id,
            limit_note(&info),
        ),
        metadata: json!({"command": ShellCommandRef {
            id: info.id.clone(),
            background: true,
            started_at: info.started_at,
        }}),
    }
}

/// What a wait in `bash_output` waits for.
#[derive(Debug)]
enum Until {
    Exit,
    Quiet(Duration),
    Match(regex::Regex),
}

/// `bash_output` for the agent of `thread`: `commands` are those it may
/// read (its chat's and those under it).
pub(crate) async fn bash_output(
    registry: &ShellCommands,
    threads: &HashSet<ThreadId>,
    arguments: &serde_json::Value,
    cancel: &CancellationToken,
) -> CoreResult<ToolOutput> {
    let Some(id) = arguments.get("id").and_then(serde_json::Value::as_str) else {
        return Ok(list_output(&registry.list(Some(threads))));
    };
    let entry = registry
        .get(id)
        .filter(|entry| threads.contains(&entry.info().thread_id))
        .ok_or_else(|| {
            CoreError::Invalid(format!(
                "no command {id} in this chat; call bash_output without an id to list them"
            ))
        })?;
    let wait = arguments
        .get("wait_seconds")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .min(300);
    let until = match arguments
        .get("until")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("exit")
    {
        "quiet" => Until::Quiet(Duration::from_secs(
            arguments
                .get("quiet_seconds")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(5)
                .clamp(1, 300),
        )),
        "match" => {
            let pattern = arguments
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| CoreError::Invalid("until=match needs a pattern".into()))?;
            Until::Match(
                regex::RegexBuilder::new(pattern)
                    .size_limit(1 << 20)
                    .build()
                    .map_err(|error| {
                        CoreError::Invalid(format!(
                            "pattern is not a valid regular expression: {error}"
                        ))
                    })?,
            )
        }
        _ => Until::Exit,
    };
    let given = arguments.get("cursor").and_then(serde_json::Value::as_u64);
    let cursor = given.unwrap_or_else(|| *entry.read_cursor.lock());
    let mut found = None;
    let mut waited = false;
    if wait > 0 {
        waited = true;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
        found = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(CoreError::Cancelled),
            found = wait_for(&entry, &until, cursor, deadline) => found?,
        };
    } else if let Until::Match(pattern) = &until {
        found = search(&entry, pattern, cursor).await?.0;
    }
    let info = entry.info();
    let output = &entry.process.output;
    // From a cursor it was given: the part that starts there. Otherwise
    // what is new since its last read, the newest part when that is more
    // than it is shown at once.
    let from = match given {
        Some(_) => cursor,
        None => cursor.max(output.size().saturating_sub(AGENT_READ_BYTES as u64)),
    };
    let page = output
        .read(from, AGENT_READ_BYTES)
        .await
        .map_err(|error| CoreError::Tool(error.to_string()))?;
    let more_than_shown = page.end < page.size;
    let mut text = page.text();
    let mut lines_left_out = false;
    if let Some(lines) = arguments
        .get("tail_lines")
        .and_then(serde_json::Value::as_u64)
    {
        let all = text.lines().collect::<Vec<_>>();
        let keep = usize::try_from(lines).unwrap_or(usize::MAX);
        if all.len() > keep {
            lines_left_out = true;
            text = all[all.len() - keep..].join("\n");
        }
    }
    *entry.read_cursor.lock() = page.end;
    let mut report = format!("Command {}: {}", info.id, state_line(&info));
    if waited || matches!(until, Until::Match(_)) {
        let outcome = match (&until, &found) {
            (Until::Match(_), Some(line)) => format!("Matched: {line}"),
            (Until::Match(_), None) if info.state.is_live() => {
                format!("No line matched within {wait}s.")
            }
            (Until::Match(_), None) => "No line matched before it ended.".into(),
            (Until::Quiet(_), Some(_)) => "It printed nothing for the quiet time.".into(),
            (_, _) if info.state.is_live() => format!("Still running after waiting {wait}s."),
            _ => "It ended.".into(),
        };
        report.push('\n');
        report.push_str(&outcome);
    }
    if from == cursor && page.start > cursor {
        let _ = write!(
            report,
            "\n{} bytes of output before this are no longer kept.",
            page.start - cursor
        );
    }
    if from > cursor || lines_left_out {
        let _ = write!(
            report,
            "\nOnly the end of the new output is shown; read from cursor {cursor} for the rest."
        );
    }
    if text.is_empty() {
        report.push_str("\nNo new output.");
    } else {
        let _ = write!(report, "\nOutput:\n{text}");
    }
    let _ = write!(
        report,
        "\nCursor: {}{}",
        page.end,
        if more_than_shown {
            " (more output follows; read on from this cursor)"
        } else {
            ""
        }
    );
    if !info.state.is_live()
        && let Some(path) = entry.output_file()
        && page.size > page.end.saturating_sub(page.start)
    {
        let _ = write!(report, "\nAll of its output: {}", path.display());
    }
    Ok(ToolOutput {
        text: report,
        metadata: json!({"command": info}),
    })
}

/// Wait for what `until` names, from `cursor`, until `deadline`. A match
/// answers the matching line.
async fn wait_for(
    entry: &CommandEntry,
    until: &Until,
    cursor: u64,
    deadline: tokio::time::Instant,
) -> CoreResult<Option<String>> {
    let mut info = entry.info.subscribe();
    let mut size = entry.process.output.subscribe();
    let mut scanned = cursor;
    loop {
        if let Until::Match(pattern) = until {
            let (found, next) = search(entry, pattern, scanned).await?;
            if found.is_some() {
                return Ok(found);
            }
            scanned = next;
        }
        if !info.borrow().state.is_live() {
            return Ok(match until {
                Until::Quiet(_) => Some(String::new()),
                _ => None,
            });
        }
        let quiet = match until {
            Until::Quiet(quiet) => Some(tokio::time::Instant::now() + *quiet),
            _ => None,
        };
        let wake = quiet.map_or(deadline, |quiet| quiet.min(deadline));
        tokio::select! {
            () = tokio::time::sleep_until(wake) => {
                if quiet.is_some_and(|quiet| quiet <= tokio::time::Instant::now()) {
                    return Ok(Some(String::new()));
                }
                return Ok(None);
            }
            _ = info.changed() => {}
            _ = size.changed() => {}
        }
    }
}

/// The first line from `cursor` that matches `pattern`, and where a later
/// search goes on from. A line ends at a line break or a carriage return
/// (a progress bar redrawing its line). The last, unfinished line is read
/// again by the next search, at most its last [`MATCH_OVERLAP`] bytes, so a
/// line that never ends is not searched from its start each time.
async fn search(
    entry: &CommandEntry,
    pattern: &regex::Regex,
    cursor: u64,
) -> CoreResult<(Option<String>, u64)> {
    let mut at = cursor;
    loop {
        let page = entry
            .process
            .output
            .read(at, PAGE_MAX)
            .await
            .map_err(|error| CoreError::Tool(error.to_string()))?;
        let text = page.text();
        let mut consumed = 0;
        for line in text.split_inclusive(['\n', '\r']) {
            let complete = line.ends_with(['\n', '\r']);
            let line_text = line.trim_end_matches(['\n', '\r']);
            if pattern.is_match(line_text) {
                return Ok((Some(line_text.chars().take(500).collect()), page.end));
            }
            if complete {
                consumed += line.len();
            }
        }
        let unfinished = page.start + consumed as u64;
        let next = unfinished.max(page.end.saturating_sub(MATCH_OVERLAP as u64));
        if page.end >= page.size || page.end == page.start {
            return Ok((None, next));
        }
        at = next.max(page.start + 1);
    }
}

/// One line on where a command is.
fn state_line(info: &ShellCommandInfo) -> String {
    if info.state.is_live() {
        let ran = (Utc::now() - info.started_at)
            .to_std()
            .map(spoken_duration)
            .unwrap_or_default();
        let place = if info.background {
            "in the background"
        } else {
            "in the foreground"
        };
        let stopping = if info.state == ShellCommandState::Stopping {
            ", stopping"
        } else {
            ""
        };
        format!("running {place} for {ran}{stopping}.")
    } else {
        ended_summary(info)
    }
}

/// `bash_output` without an id.
fn list_output(commands: &[ShellCommandInfo]) -> ToolOutput {
    let text = if commands.is_empty() {
        "This chat has no commands running or recently ended.".to_owned()
    } else {
        commands
            .iter()
            .map(|info| format!("{} — {}", info.id, state_line(info)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    ToolOutput {
        text,
        metadata: json!({"commands": commands}),
    }
}

/// `bash_stop` for the agent of `thread`.
pub(crate) async fn bash_stop(
    registry: &ShellCommands,
    threads: &HashSet<ThreadId>,
    arguments: &serde_json::Value,
    cancel: &CancellationToken,
) -> CoreResult<ToolOutput> {
    let id = arguments
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| CoreError::Invalid("id is required".into()))?;
    let entry = registry
        .get(id)
        .filter(|entry| threads.contains(&entry.info().thread_id))
        .ok_or_else(|| {
            CoreError::Invalid(format!(
                "no command {id} in this chat; call bash_output without an id to list them"
            ))
        })?;
    let signal = match arguments.get("signal").and_then(serde_json::Value::as_str) {
        Some(text) => Some(StopSignal::parse(text).ok_or_else(|| {
            CoreError::Invalid(format!("signal must be INT, TERM or KILL, not {text}"))
        })?),
        None => None,
    };
    let before = entry.info();
    if !before.state.is_live() {
        return Ok(ToolOutput {
            text: format!(
                "Command {} had already ended: {}",
                before.id,
                ended_summary(&before)
            ),
            metadata: json!({"command": before}),
        });
    }
    let grace = entry.policy.stop_grace;
    let (sequence, settle) = match signal {
        Some(signal) => (
            StopSequence::Only(signal),
            grace.max(Duration::from_millis(500)),
        ),
        None => (
            StopSequence::Graceful(grace),
            grace * 2 + Duration::from_secs(2),
        ),
    };
    entry.stop(
        ShellCommandEndedBy::Agent,
        ShellCommandState::Stopped,
        sequence,
    );
    let info = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(CoreError::Cancelled),
        info = entry.settled(settle) => info,
    };
    let (output, _) = entry.tail_text(NOTICE_LINES, NOTICE_BYTES);
    let text = if info.state.is_live() {
        match signal {
            Some(signal) => format!(
                "Sent {} to command {}; it is still running.",
                signal.name(),
                info.id
            ),
            None => format!("Command {} is still stopping.", info.id),
        }
    } else {
        format!(
            "Command {}: {}\nEnd of its output:\n{}",
            info.id,
            ended_summary(&info),
            if output.is_empty() { "(none)" } else { &output }
        )
    };
    Ok(ToolOutput {
        text,
        metadata: json!({"command": info}),
    })
}

/// A page of a command's output for a client.
pub(crate) async fn client_page(
    entry: &CommandEntry,
    cursor: Option<u64>,
    limit: Option<u64>,
    tail: bool,
) -> CoreResult<ShellCommandOutput> {
    let limit = limit
        .map_or(PAGE_DEFAULT, |limit| {
            usize::try_from(limit).unwrap_or(PAGE_MAX)
        })
        .clamp(1, PAGE_MAX);
    let info = entry.info();
    let output = &entry.process.output;
    let cursor = if tail {
        output.size().saturating_sub(limit as u64)
    } else {
        cursor.unwrap_or(0)
    };
    let page = output
        .read(cursor, limit)
        .await
        .map_err(|error| CoreError::Tool(error.to_string()))?;
    let settled = !info.state.is_live();
    Ok(ShellCommandOutput {
        text: page.text(),
        start: page.start,
        cursor: page.end,
        size: page.size,
        more: page.end < page.size,
        settled,
        state: info.state,
        exit_code: info.exit_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hooks that do nothing.
    struct Quiet;
    #[async_trait::async_trait]
    impl CommandHooks for Quiet {
        fn updated(&self, _: &ShellCommandInfo) {}
        fn printed(&self, _: &ShellCommandInfo, _: u64) {}
        async fn backgrounded(&self, _: &CommandEntry, _: Option<ShellCommandMovedBy>) {}
        async fn ended(&self, _: &CommandEntry, _: bool) {}
    }

    /// A process nobody can signal.
    struct Silent;
    impl ProcessControl for Silent {
        fn signal(&self, _: StopSignal) {}
    }

    struct Signals(Mutex<Vec<StopSignal>>);
    impl ProcessControl for Signals {
        fn signal(&self, signal: StopSignal) {
            self.0.lock().push(signal);
        }
    }

    async fn output(dir: &Path, limit: u64) -> Arc<CommandOutput> {
        let path = dir.join(format!("out-{}", uuid::Uuid::now_v7()));
        let file = tokio::fs::File::create(&path).await.unwrap();
        Arc::new(CommandOutput::new(path, file, limit))
    }

    /// A folder of the test's own, removed with it.
    struct Fixture(PathBuf);
    impl Fixture {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> Fixture {
        let path =
            std::env::temp_dir().join(format!("haarness-command-output-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Fixture(path)
    }

    #[tokio::test]
    async fn a_page_never_splits_a_character_and_says_what_was_dropped() {
        let dir = fixture();
        let output = output(dir.path(), u64::MAX).await;
        output.append("aé".as_bytes()).await.unwrap();
        output.append("b".as_bytes()).await.unwrap();
        // A limit that ends inside é stops before it.
        let page = output.read(0, 2).await.unwrap();
        assert_eq!((page.text().as_str(), page.start, page.end), ("a", 0, 1));
        let page = output.read(page.end, 10).await.unwrap();
        assert_eq!((page.text().as_str(), page.end, page.size), ("éb", 4, 4));
        // A cursor inside a character that is no longer kept starts at the
        // next whole one.
        let dir2 = fixture();
        let small = self::output(dir2.path(), 3).await;
        small.append(&[b'x'; 4]).await.unwrap();
        small
            .append("é".repeat(OUTPUT_TAIL_KEPT / 2 + 1).as_bytes())
            .await
            .unwrap();
        let page = small.read(4, 16).await.unwrap();
        assert!(page.start > 4, "{page:?}");
        assert!(page.text().starts_with('é'), "{:?}", page.text());
        assert!(!page.text().contains('\u{fffd}'));
        // The saved start is still read from the file.
        let page = small.read(0, 3).await.unwrap();
        assert_eq!(page.text(), "xxx");
    }

    #[tokio::test]
    async fn what_was_saved_can_be_read_back_at_once() {
        let dir = fixture();
        let output = output(dir.path(), u64::MAX).await;
        let chunk = [b'x'; 1000];
        for written in 1..=200_u64 {
            output.append(&chunk).await.unwrap();
            // What counts as saved is on disk already: a reader opening the
            // file finds all of it.
            assert_eq!(
                std::fs::metadata(output.path()).unwrap().len(),
                output.saved(),
                "after {written} writes"
            );
            // Past the end kept in memory, a page from the start is read
            // from the file, up to all that was saved.
            let page = output.read(0, PAGE_MAX).await.unwrap();
            assert_eq!(page.end, written * 1000, "after {written} writes");
        }
    }

    #[test]
    fn whole_end_keeps_complete_and_invalid_bytes() {
        assert_eq!(whole_end("aé".as_bytes()), 3);
        assert_eq!(whole_end(&"aé".as_bytes()[..2]), 1);
        assert_eq!(whole_end(&[b'a', 0xFF]), 2);
        assert_eq!(whole_end(&"日".as_bytes()[..2]), 0);
        assert_eq!(whole_start(&"日x".as_bytes()[1..]), b"x");
    }

    #[test]
    fn the_policy_moves_only_where_a_client_follows_and_never_a_subagents() {
        let settings = ExecutionSettings::default();
        let interactive = CommandPolicy::new(&settings, true, false, false, None);
        assert_eq!(interactive.move_after, Some(Duration::from_secs(60)));
        assert_eq!(interactive.foreground_deadline, None);
        assert_eq!(interactive.background_limit, None);
        assert!(interactive.movable && interactive.delivers_notice);
        // A timeout the agent gives limits the whole run and nothing moves.
        let timed = CommandPolicy::new(&settings, true, false, false, Some(5_000));
        assert_eq!(timed.move_after, None);
        assert_eq!(timed.foreground_deadline, Some(Duration::from_secs(5)));
        assert_eq!(
            timed.background_limit,
            Some(Limit::SinceStart(Duration::from_secs(5)))
        );
        // Elsewhere, as before: the configured deadline, also in the
        // background.
        let script = CommandPolicy::new(&settings, false, false, false, None);
        assert_eq!(script.move_after, None);
        assert_eq!(script.foreground_deadline, Some(Duration::from_secs(120)));
        assert_eq!(
            script.background_limit,
            Some(Limit::SinceStart(Duration::from_secs(120)))
        );
        let subagent = CommandPolicy::new(&settings, true, true, false, None);
        assert_eq!(subagent.move_after, None);
        assert!(!subagent.movable && !subagent.delivers_notice);
        // Nobody watches a scheduled run: no timeout lifts its limit.
        let scheduled = CommandPolicy::new(&settings, false, false, true, Some(0));
        assert_eq!(
            scheduled.background_limit,
            Some(Limit::SinceStart(Duration::from_secs(120)))
        );
        // Zero moves nothing; a background limit counts from the move.
        let settings = ExecutionSettings {
            shell_move_after_ms: 0,
            shell_background_max_ms: 1_000,
            ..ExecutionSettings::default()
        };
        let never = CommandPolicy::new(&settings, true, false, false, None);
        assert_eq!(never.move_after, None);
        assert_eq!(never.foreground_deadline, Some(Duration::from_secs(120)));
        assert_eq!(
            never.background_limit,
            Some(Limit::SinceBackground(Duration::from_secs(1)))
        );
    }

    #[test]
    fn a_notice_names_how_the_command_ended() {
        let started = Utc::now() - chrono::Duration::seconds(72);
        let mut info = ShellCommandInfo {
            id: "cmd-1".into(),
            thread_id: ThreadId::new(),
            root_thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            call_id: "c".into(),
            command: "npm test\n--watch".into(),
            workdir: None,
            background: true,
            moved_by: Some(ShellCommandMovedBy::Agent),
            state: ShellCommandState::Exited,
            exit_code: Some(2),
            signal: None,
            ended_by: None,
            started_at: started,
            background_at: Some(started),
            ended_at: Some(started + chrono::Duration::seconds(72)),
            stops_at: None,
            moves_at: None,
            output_bytes: 0,
            delivers_notice: true,
        };
        assert_eq!(
            notice_tag(&info),
            r#"<command id="cmd-1" state="exited" exit="2">"#
        );
        assert_eq!(
            ended_summary(&info),
            "`npm test…` exited with code 2 after 1m 12s."
        );
        info.state = ShellCommandState::Stopped;
        info.exit_code = None;
        info.signal = Some("SIGINT".into());
        info.ended_by = Some(ShellCommandEndedBy::Person);
        assert_eq!(
            notice_tag(&info),
            r#"<command id="cmd-1" state="stopped" signal="SIGINT">"#
        );
        assert!(ended_summary(&info).contains("stopped by the person"));
    }

    #[test]
    fn durations_read_as_people_say_them() {
        assert_eq!(spoken_duration(Duration::from_secs(4)), "4s");
        assert_eq!(spoken_duration(Duration::from_secs(72)), "1m 12s");
        assert_eq!(spoken_duration(Duration::from_secs(7500)), "2h 5m");
    }

    /// A command started in the background whose process the test plays,
    /// in `registry` for chat `thread`.
    async fn started(
        dir: &Path,
        registry: &Arc<ShellCommands>,
        thread: ThreadId,
    ) -> (
        Arc<CommandEntry>,
        Arc<CommandOutput>,
        watch::Sender<Option<ProcessEnd>>,
    ) {
        let (end, ended) = watch::channel(None);
        let output = output(dir, u64::MAX).await;
        let call = ShellCall::new(ShellCallInner {
            commands: Arc::clone(registry),
            hooks: Arc::new(Quiet),
            thread_id: thread,
            root_thread_id: thread,
            turn_id: TurnId::new(),
            call_id: "call".into(),
            settings: ExecutionSettings::default(),
            follows: true,
            subagent: false,
            unattended: false,
        });
        call.run(
            CommandProcess {
                output: Arc::clone(&output),
                end: ended,
                control: Arc::new(Silent),
            },
            CommandRequest {
                command: "server".into(),
                cwd: dir.to_path_buf(),
                workdir: None,
                timeout_ms: None,
                background: true,
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let id = registry.list(None).pop().unwrap().id;
        (registry.get(&id).unwrap(), output, end)
    }

    #[tokio::test]
    async fn bash_output_pages_forward_from_a_cursor_it_was_given() {
        let dir = fixture();
        let registry = Arc::new(ShellCommands::default());
        let thread = ThreadId::new();
        let (entry, output, _end) = started(dir.path(), &registry, thread).await;
        let lines = (0..2000)
            .map(|line| format!("line-{line:04}-{}\n", "x".repeat(40)))
            .collect::<String>();
        output.append(lines.as_bytes()).await.unwrap();
        let threads = HashSet::from([thread]);
        let id = entry.info().id;
        let cancel = CancellationToken::new();
        let read = |arguments: serde_json::Value| {
            let registry = Arc::clone(&registry);
            let threads = threads.clone();
            let cancel = cancel.clone();
            async move {
                bash_output(&registry, &threads, &arguments, &cancel)
                    .await
                    .unwrap()
                    .text
            }
        };
        let cursor_of = |text: &str| -> u64 {
            text.split("Cursor: ")
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap()
        };
        // Without a cursor: the newest output, and where the rest starts.
        let latest = read(json!({"id": id})).await;
        assert!(latest.contains("line-1999"), "{latest:.300}");
        let from = latest
            .split("read from cursor ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .trim_end_matches(|c: char| !c.is_ascii_digit())
            .parse::<u64>()
            .unwrap();
        assert_eq!(from, 0, "{latest:.300}");
        // From that cursor: the first part, and a cursor where it ends.
        let first = read(json!({"id": id, "cursor": from})).await;
        assert!(first.contains("\nline-0000-"), "{first:.300}");
        assert!(!first.contains("line-1999"), "{first:.300}");
        let next = cursor_of(&first);
        assert!(next > 0 && next < lines.len() as u64, "{next}");
        // Following it goes on where the first part ended.
        let second = read(json!({"id": id, "cursor": next})).await;
        let resumed = &lines[usize::try_from(next).unwrap()..];
        let shown = second.split("Output:\n").nth(1).unwrap();
        assert!(resumed.starts_with(&shown[..40]), "{shown:.80}");
        assert!(cursor_of(&second) > next);
    }

    #[tokio::test]
    async fn a_match_reads_progress_lines_and_never_rescans_a_long_line_from_its_start() {
        let dir = fixture();
        let registry = Arc::new(ShellCommands::default());
        let (entry, output, _end) = started(dir.path(), &registry, ThreadId::new()).await;
        // A progress bar redraws its line with a carriage return.
        output
            .append(b"progress 10%\rprogress 100%\rdone in 3s\n")
            .await
            .unwrap();
        let pattern = regex::Regex::new("^done").unwrap();
        let (found, _) = search(&entry, &pattern, 0).await.unwrap();
        assert_eq!(found.as_deref(), Some("done in 3s"));
        // A line that never ends: the next search starts near its end,
        // not at its start again.
        let start = output.size();
        output.append(&vec![b'.'; 2 * 1024 * 1024]).await.unwrap();
        let missing = regex::Regex::new("never").unwrap();
        let (found, next) = search(&entry, &missing, start).await.unwrap();
        assert!(found.is_none());
        assert!(
            next >= output.size() - MATCH_OVERLAP as u64,
            "{next} of {}",
            output.size()
        );
    }

    #[tokio::test]
    async fn a_stop_signals_in_steps_until_the_process_ends() {
        let dir = fixture();
        let signals = Arc::new(Signals(Mutex::new(Vec::new())));
        let (end, ended) = watch::channel(None);
        let registry = Arc::new(ShellCommands::default());
        let call = ShellCall::new(ShellCallInner {
            commands: Arc::clone(&registry),
            hooks: Arc::new(Quiet),
            thread_id: ThreadId::new(),
            root_thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            call_id: "call".into(),
            settings: ExecutionSettings {
                shell_stop_grace_ms: 50,
                ..ExecutionSettings::default()
            },
            follows: true,
            subagent: false,
            unattended: false,
        });
        let run = call
            .run(
                CommandProcess {
                    output: output(dir.path(), u64::MAX).await,
                    end: ended,
                    control: signals.clone(),
                },
                CommandRequest {
                    command: "server".into(),
                    cwd: dir.path().to_path_buf(),
                    workdir: None,
                    timeout_ms: None,
                    background: true,
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let CommandRun::Background(started) = run else {
            panic!("a background start answers at once");
        };
        assert!(
            started.text.contains("runs in the background"),
            "{}",
            started.text
        );
        let entry = registry.list(None).pop().unwrap();
        let entry = registry.get(&entry.id).unwrap();
        assert!(entry.stop(
            ShellCommandEndedBy::Person,
            ShellCommandState::Stopped,
            StopSequence::Graceful(Duration::from_millis(50)),
        ));
        assert_eq!(entry.info().state, ShellCommandState::Stopping);
        tokio::time::sleep(Duration::from_millis(80)).await;
        // The process ignores the interrupt; the terminate ends it.
        end.send_replace(Some(ProcessEnd {
            signal: Some("SIGTERM".into()),
            ..ProcessEnd::default()
        }));
        let info = entry.settled(Duration::from_secs(2)).await;
        assert_eq!(info.state, ShellCommandState::Stopped);
        assert_eq!(info.ended_by, Some(ShellCommandEndedBy::Person));
        assert_eq!(info.signal.as_deref(), Some("SIGTERM"));
        assert_eq!(
            *signals.0.lock(),
            [StopSignal::Interrupt, StopSignal::Terminate]
        );
        // Stopping it again changes nothing.
        assert!(!entry.stop(
            ShellCommandEndedBy::Person,
            ShellCommandState::Stopped,
            StopSequence::Graceful(Duration::ZERO),
        ));
    }

    #[test]
    fn live_commands_list_first_and_ended_ones_are_forgotten_after_a_while() {
        let commands = ShellCommands::default();
        let chat = ThreadId::new();
        let other = ThreadId::new();
        let entry = |id: &str, thread: ThreadId, state: ShellCommandState, ended: i64| {
            let (_, end) = watch::channel(None);
            let path = std::env::temp_dir().join("haarness-unused-command-output");
            let file = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
            let now = Utc::now();
            Arc::new(CommandEntry {
                info: watch::Sender::new(ShellCommandInfo {
                    id: id.into(),
                    thread_id: thread,
                    root_thread_id: thread,
                    turn_id: TurnId::new(),
                    call_id: format!("call-{id}"),
                    command: "sleep 1".into(),
                    workdir: None,
                    background: true,
                    moved_by: Some(ShellCommandMovedBy::Agent),
                    state,
                    exit_code: None,
                    signal: None,
                    ended_by: None,
                    started_at: now,
                    background_at: Some(now),
                    ended_at: (!state.is_live()).then(|| now - chrono::Duration::minutes(ended)),
                    stops_at: None,
                    moves_at: None,
                    output_bytes: 0,
                    delivers_notice: true,
                }),
                process: CommandProcess {
                    output: Arc::new(CommandOutput::new(path, tokio::fs::File::from_std(file), 0)),
                    end,
                    control: Arc::new(Silent),
                },
                cwd: PathBuf::new(),
                policy: CommandPolicy::new(&ExecutionSettings::default(), true, false, false, None),
                hooks: Arc::new(Quiet),
                read_cursor: Mutex::new(0),
                stopping: Mutex::new(None),
                released: watch::Sender::new(false),
                wake: Notify::new(),
                announced: AtomicBool::new(false),
            })
        };
        for (id, thread, state, ended) in [
            ("old", chat, ShellCommandState::Exited, 11),
            ("ended", chat, ShellCommandState::Stopped, 1),
            ("live", chat, ShellCommandState::Running, 0),
            ("elsewhere", other, ShellCommandState::Running, 0),
        ] {
            commands
                .entries
                .insert(id.into(), entry(id, thread, state, ended));
        }
        let threads = HashSet::from([chat]);
        let listed = commands
            .list(Some(&threads))
            .into_iter()
            .map(|info| info.id)
            .collect::<Vec<_>>();
        assert_eq!(listed, ["live", "ended"]);
        assert!(commands.get("old").is_none());
        assert_eq!(commands.list(None).len(), 3);
        assert_eq!(commands.running_in_background(&threads), 1);
    }
}
