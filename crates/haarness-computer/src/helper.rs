//! Talks to the native helper in `macos/`, the process that captures the screen, reads Accessibility and posts
//! input events. It is launched on first use and spoken to in newline-delimited JSON, one request at a time; it exits
//! when its stdin closes or after an idle spell. macOS counts the helper's use of both permissions against
//! whichever app launched Haarness.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
    sync::{broadcast, oneshot},
};
use tokio_util::sync::CancellationToken;

/// The line protocol this server speaks with the helper.
pub const PROTOCOL: u64 = 1;

const EMBEDDED: &[u8] = include_bytes!(env!("HAARNESS_MAC_HELPER_PATH"));
const EMBEDDED_SHA: &str = env!("HAARNESS_MAC_HELPER_SHA");

/// Idle time after which the helper exits; the next request launches a new one.
pub const IDLE: Duration = Duration::from_secs(5 * 60);
/// While computer use is off, a helper started only to read permissions quits soon after.
pub const IDLE_WHEN_OFF: Duration = Duration::from_secs(10);
/// One answer line is at most this long (a screenshot is a few hundred kilobytes).
const LINE_LIMIT: usize = 32 * 1024 * 1024;

/// Whether this build carries the helper.
#[must_use]
pub fn embedded() -> bool {
    !EMBEDDED.is_empty()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperError {
    pub code: String,
    pub message: String,
}

impl HelperError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for HelperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HelperError {}

/// Unprompted messages from the helper: Escape was pressed, a button on its bar was used, or it exited (taking its
/// bar along).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Escape,
    Stop,
    Pause,
    Resume,
    Exit,
}

/// Where the helper comes from.
#[derive(Clone, Debug)]
pub enum Source {
    /// The copy built into this binary, unpacked into this cache folder and checked against its hash before each start.
    Embedded { cache: PathBuf },
    /// A helper at this path: a development build or a test stand-in.
    Path(PathBuf),
}

struct Running {
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: parking_lot::Mutex<HashMap<u64, oneshot::Sender<Result<Value, HelperError>>>>,
    next: AtomicU64,
    closed: CancellationToken,
    kill: CancellationToken,
}

impl Running {
    fn fail_all(&self, error: &HelperError) {
        for (_, sender) in self.pending.lock().drain() {
            let _ = sender.send(Err(error.clone()));
        }
    }
}

pub struct Helper {
    source: Source,
    queue: tokio::sync::Mutex<()>,
    running: parking_lot::Mutex<Option<Arc<Running>>>,
    events: broadcast::Sender<Event>,
    /// Set during a pause so the helper is not shut down for idleness: its bar is what holds the pause.
    kept: AtomicBool,
    idle_generation: AtomicU64,
}

impl Helper {
    #[must_use]
    pub fn new(source: Source) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            source,
            queue: tokio::sync::Mutex::new(()),
            running: parking_lot::Mutex::new(None),
            events,
            kept: AtomicBool::new(false),
            idle_generation: AtomicU64::new(0),
        }
    }

    /// Whether a helper can run here: macOS, with one in this build or at the configured path.
    #[must_use]
    pub fn available(&self) -> bool {
        cfg!(target_os = "macos")
            && match &self.source {
                Source::Embedded { .. } => embedded(),
                Source::Path(path) => path.is_file(),
            }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running
            .lock()
            .as_ref()
            .is_some_and(|running| !running.closed.is_cancelled())
    }

    fn binary(&self) -> Result<PathBuf, HelperError> {
        match &self.source {
            Source::Path(path) => Ok(path.clone()),
            Source::Embedded { cache } => {
                if !embedded() {
                    return Err(HelperError::new(
                        "missing",
                        "Computer use is not part of this Haarness build.",
                    ));
                }
                unpack(cache).map_err(|error| {
                    HelperError::new(
                        "missing",
                        format!("Writing the Mac helper to the cache failed: {error}."),
                    )
                })
            }
        }
    }

    async fn start(self: &Arc<Self>) -> Result<Arc<Running>, HelperError> {
        let binary = self.binary()?;
        let mut child = Command::new(&binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                HelperError::new(
                    "missing",
                    format!("Launching the Mac helper failed: {error}."),
                )
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HelperError::new("failed", "The helper has no input."))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HelperError::new("failed", "The helper has no output."))?;
        let stderr = child.stderr.take();
        let running = Arc::new(Running {
            stdin: tokio::sync::Mutex::new(stdin),
            pending: parking_lot::Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            closed: CancellationToken::new(),
            kill: CancellationToken::new(),
        });
        {
            let running = running.clone();
            let events = self.events.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                let mut line = Vec::new();
                loop {
                    line.clear();
                    match reader.read_until(b'\n', &mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line.len() > LINE_LIMIT => break,
                        Ok(_) => {}
                    }
                    let Ok(message) = serde_json::from_slice::<Value>(&line) else {
                        continue;
                    };
                    if let Some(event) = message["event"].as_str() {
                        let event = match event {
                            "escape" => Event::Escape,
                            "stop" => Event::Stop,
                            "pause" => Event::Pause,
                            "resume" => Event::Resume,
                            _ => continue,
                        };
                        let _ = events.send(event);
                        continue;
                    }
                    let Some(id) = message["id"].as_u64() else {
                        continue;
                    };
                    let Some(sender) = running.pending.lock().remove(&id) else {
                        continue;
                    };
                    let answer = if message["ok"].as_bool() == Some(true) {
                        Ok(message.get("data").cloned().unwrap_or_else(|| json!({})))
                    } else {
                        Err(HelperError::new(
                            message["code"].as_str().unwrap_or("failed"),
                            message["message"]
                                .as_str()
                                .unwrap_or("The Mac helper reported a failure without details.")
                                .to_owned(),
                        ))
                    };
                    let _ = sender.send(answer);
                }
            });
        }
        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::warn!(target: "haarness_computer::helper", "{}", line.chars().take(2000).collect::<String>());
                }
            });
        }
        {
            let running = running.clone();
            let helper = Arc::downgrade(self);
            tokio::spawn(async move {
                let status = tokio::select! {
                    status = child.wait() => status.ok(),
                    () = running.kill.cancelled() => {
                        let _ = child.kill().await;
                        child.wait().await.ok()
                    }
                };
                running.closed.cancel();
                running.fail_all(&HelperError::new(
                    "exited",
                    format!(
                        "The Mac helper exited{}. The last action may or may not have completed; check with mac_snapshot before repeating it.",
                        status.and_then(|status| status.code()).map(|code| format!(" (exit {code})")).unwrap_or_default()
                    ),
                ));
                if let Some(helper) = helper.upgrade() {
                    let mut current = helper.running.lock();
                    if current
                        .as_ref()
                        .is_some_and(|item| Arc::ptr_eq(item, &running))
                    {
                        *current = None;
                    }
                    drop(current);
                    let _ = helper.events.send(Event::Exit);
                }
            });
        }
        let hello = Self::send(&running, "hello", json!({}), Duration::from_secs(10), None).await?;
        if hello["protocol"].as_u64() != Some(PROTOCOL) {
            running.kill.cancel();
            return Err(HelperError::new(
                "protocol",
                format!(
                    "The Mac helper uses protocol {}, but this server expects {PROTOCOL}.",
                    hello["protocol"]
                ),
            ));
        }
        tracing::info!(build = %hello["build"], pid = %hello["pid"], "mac helper running");
        Ok(running)
    }

    async fn send(
        running: &Arc<Running>,
        op: &str,
        mut input: Value,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, HelperError> {
        if running.closed.is_cancelled() {
            return Err(HelperError::new("exited", "The Mac helper exited."));
        }
        let id = running.next.fetch_add(1, Ordering::Relaxed);
        if let Some(object) = input.as_object_mut() {
            object.insert("id".into(), json!(id));
            object.insert("op".into(), json!(op));
        }
        let (sender, receiver) = oneshot::channel();
        running.pending.lock().insert(id, sender);
        let mut line = serde_json::to_vec(&input)
            .map_err(|error| HelperError::new("invalid", error.to_string()))?;
        line.push(b'\n');
        {
            let mut stdin = running.stdin.lock().await;
            if let Err(error) = async {
                stdin.write_all(&line).await?;
                stdin.flush().await
            }
            .await
            {
                running.pending.lock().remove(&id);
                running.kill.cancel();
                return Err(HelperError::new(
                    "exited",
                    format!("The Mac helper exited: {error}."),
                ));
            }
        }
        let never = CancellationToken::new();
        let cancel = cancel.unwrap_or(&never);
        tokio::select! {
            answer = receiver => answer.unwrap_or_else(|_| Err(HelperError::new("exited", "The Mac helper exited."))),
            () = tokio::time::sleep(timeout) => {
                running.pending.lock().remove(&id);
                // No answer usually means a hung app; kill the helper so the next request starts fresh.
                running.kill.cancel();
                Err(HelperError::new("timeout", "No reply from the Mac in time, and the action may or may not have gone through. Check with mac_snapshot before trying again."))
            }
            () = cancel.cancelled() => {
                running.pending.lock().remove(&id);
                // A cancelled turn must not keep typing: killing the helper ends any action in progress.
                running.kill.cancel();
                Err(HelperError::new("cancelled", "Stopped."))
            }
        }
    }

    /// Queues one operation behind any running request. `gate` is checked when the request reaches the front of the
    /// queue, because settings may have changed meanwhile, and its error refuses the request. Cancelling kills the
    /// helper, so nothing continues after the person stops the turn.
    pub async fn request(
        self: &Arc<Self>,
        op: &str,
        input: Value,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
        gate: Option<&(dyn Fn() -> Result<(), HelperError> + Send + Sync)>,
        idle: Duration,
    ) -> Result<Value, HelperError> {
        let _turn = self.queue.lock().await;
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(HelperError::new("cancelled", "Stopped."));
        }
        if let Some(gate) = gate {
            gate()?;
        }
        let running = {
            let current = self.running.lock().clone();
            match current {
                Some(running) if !running.closed.is_cancelled() => running,
                _ => {
                    let started = self.start().await?;
                    *self.running.lock() = Some(started.clone());
                    started
                }
            }
        };
        // Launching takes a little while; honour a cancel that arrived during it.
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(HelperError::new("cancelled", "Stopped."));
        }
        let generation = self.idle_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let result = Self::send(&running, op, input, timeout, cancel).await;
        self.schedule_idle(generation, idle);
        result
    }

    fn schedule_idle(self: &Arc<Self>, generation: u64, after: Duration) {
        if self.kept.load(Ordering::SeqCst) {
            return;
        }
        let helper = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            let Some(helper) = helper.upgrade() else {
                return;
            };
            if helper.idle_generation.load(Ordering::SeqCst) == generation
                && !helper.kept.load(Ordering::SeqCst)
            {
                helper.shutdown(false);
            }
        });
    }

    /// Holds off the idle shutdown while `on` (during a pause), then rearms it with [`IDLE`].
    pub fn keep(self: &Arc<Self>, on: bool) {
        self.kept.store(on, Ordering::SeqCst);
        if !on {
            let generation = self.idle_generation.fetch_add(1, Ordering::SeqCst) + 1;
            self.schedule_idle(generation, IDLE);
        }
    }

    /// Asks a running helper for its `status` ahead of any request in flight (the helper answers it at once), or
    /// starts one in turn when none runs.
    pub async fn status(self: &Arc<Self>, idle: Duration) -> Result<Value, HelperError> {
        let running = self
            .running
            .lock()
            .clone()
            .filter(|running| !running.closed.is_cancelled());
        match running {
            Some(running) => {
                Self::send(&running, "status", json!({}), Duration::from_secs(10), None).await
            }
            None => {
                self.request(
                    "status",
                    json!({}),
                    Duration::from_secs(15),
                    None,
                    None,
                    idle,
                )
                .await
            }
        }
    }

    /// Tells a running helper the screen is free so its bar disappears, jumping the queue and never launching a
    /// helper.
    pub fn release(&self) {
        let Some(running) = self.running.lock().clone() else {
            return;
        };
        if running.closed.is_cancelled() {
            return;
        }
        tokio::spawn(async move {
            let _ = Self::send(&running, "release", json!({}), Duration::from_secs(5), None).await;
        });
    }

    /// Shuts the helper down if there is one: `now` kills it immediately, ending any action in progress; otherwise it
    /// is asked to finish the current request and exit.
    pub fn shutdown(&self, now: bool) {
        let Some(running) = self.running.lock().take() else {
            return;
        };
        if now {
            running.kill.cancel();
            return;
        }
        tokio::spawn(async move {
            let _ = running.stdin.lock().await.shutdown().await;
            tokio::time::sleep(Duration::from_secs(2)).await;
            running.kill.cancel();
        });
    }
}

/// Writes the embedded helper into `cache` and returns its path. An existing copy is hashed and replaced whenever
/// it does not match, so a tampered file never runs.
fn unpack(cache: &Path) -> io::Result<PathBuf> {
    let directory = cache
        .join("mac-helper")
        .join(env!("CARGO_PKG_VERSION"))
        .join(&EMBEDDED_SHA[..16.min(EMBEDDED_SHA.len())]);
    let target = directory.join("haarness-mac");
    if let Ok(bytes) = fs::read(&target)
        && hex::encode(Sha256::digest(&bytes)) == EMBEDDED_SHA
    {
        return Ok(target);
    }
    fs::create_dir_all(&directory)?;
    let temp = directory.join(format!(".haarness-mac.{}.tmp", uuid::Uuid::now_v7()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o755);
        let mut file = options.open(&temp)?;
        file.write_all(EMBEDDED)?;
        file.sync_all()?;
        fs::rename(&temp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    Ok(target)
}
