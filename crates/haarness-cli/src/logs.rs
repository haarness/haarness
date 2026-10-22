//! A log level raised for a while, and logs packed to share.
//!
//! `haarness logs level debug --for 30m` writes a small control file in the
//! data folder; every Haarness process using that folder (the TUI, the
//! workspace servers behind it and Desktop, one-shot commands) reads it about
//! every two seconds and swaps its log filter, so a running server logs more
//! without a restart that would stop its turns. The level goes back by itself
//! when the time is up; `--until-stopped` keeps it until `haarness logs level
//! stop`. `haarness logs export` packs the log files into a zip with known
//! secret shapes removed.

use std::{
    fmt::Write as _,
    fs,
    io::{BufRead as _, BufReader, Write as _},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use tracing_subscriber::{EnvFilter, Registry, filter::LevelFilter, reload};

/// The levels a window can raise every Haarness crate to.
pub const LEVELS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];
/// How long a raised level lasts without `--for`.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(30 * 60);
/// The longest timed window; `--until-stopped` has no end.
pub const MAX_WINDOW: Duration = Duration::from_secs(4 * 60 * 60);
/// How often each process reads the control file.
const POLL: Duration = Duration::from_secs(2);
/// What an export packs at most, newest files first.
const EXPORT_LIMIT: u64 = 200 * 1024 * 1024;

/// The raised level every process follows while it lasts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Control {
    pub level: String,
    /// When the level goes back; none until it is stopped.
    pub until: Option<DateTime<Utc>>,
    pub started_at: DateTime<Utc>,
}

impl Control {
    /// The level in force at `now`, none once the window has ended.
    fn level_at(&self, now: DateTime<Utc>) -> Option<&str> {
        self.until
            .is_none_or(|until| until > now)
            .then_some(self.level.as_str())
    }
}

pub fn control_path(data: &Path) -> PathBuf {
    data.join("log-level.json")
}

/// The control file, if one is set and readable; a damaged one counts as none.
pub fn read_control(data: &Path) -> Option<Control> {
    let bytes = fs::read(control_path(data)).ok()?;
    serde_json::from_slice::<Control>(&bytes)
        .ok()
        .filter(|control| LEVELS.contains(&control.level.as_str()))
}

fn write_control(data: &Path, control: &Control) -> Result<()> {
    fs::create_dir_all(data)?;
    let path = control_path(data);
    let staged = path.with_extension("json.tmp");
    fs::write(&staged, serde_json::to_vec_pretty(control)?)?;
    // A reader sees the old file or the new one, never half of it.
    fs::rename(&staged, &path)?;
    Ok(())
}

/// The level `directives` (an `EnvFilter` string) give events of `target`:
/// the directive with the longest target that `target` starts with, else
/// the bare level, else `error` (`EnvFilter`'s default). Span filters are
/// not read.
fn level_for(directives: &[(Option<&str>, LevelFilter)], target: &str) -> LevelFilter {
    directives
        .iter()
        .filter_map(|(name, level)| {
            name.filter(|name| target.starts_with(name))
                .map(|name| (name.len(), *level))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, level)| level)
        .or_else(|| {
            directives
                .iter()
                .rev()
                .find_map(|(name, level)| name.is_none().then_some(*level))
        })
        .unwrap_or(LevelFilter::ERROR)
}

/// A process's filter raised to `level` for Haarness's own `targets`: its
/// own directives, with each target that logs less than `level` raised. A
/// target the process already logs at that level or more keeps its level,
/// as do directives for other crates (`hyper=trace`). None when no target
/// would log more: a raised level never quiets a process.
fn raised_directives(base: &str, level: &str, targets: &[&str]) -> Option<String> {
    use std::fmt::Write as _;
    let level = level.parse::<LevelFilter>().ok()?;
    let directives = base
        .split(',')
        .map(str::trim)
        .filter(|directive| !directive.is_empty() && !directive.contains('['))
        .filter_map(|directive| match directive.split_once('=') {
            Some((name, value)) => value
                .trim()
                .parse()
                .ok()
                .map(|value| (Some(name.trim()), value)),
            None => Some(
                directive
                    .parse()
                    .map_or((Some(directive), LevelFilter::TRACE), |value| (None, value)),
            ),
        })
        .collect::<Vec<_>>();
    let mut raised = base.trim().trim_end_matches(',').to_owned();
    let mut louder = false;
    for target in targets {
        if level_for(&directives, target) < level {
            louder = true;
            if !raised.is_empty() {
                raised.push(',');
            }
            let _ = write!(raised, "{target}={level}");
        }
    }
    louder.then_some(raised)
}

/// Swaps the log filters of this process whenever the control file asks for
/// a level louder than its own for Haarness's `targets`, and back to `base`
/// (the filter the process started with, from `directives`) once no such
/// window is in force. Runs for the life of the process.
pub fn watch(
    data: PathBuf,
    base: EnvFilter,
    directives: String,
    handles: Vec<reload::Handle<EnvFilter, Registry>>,
    targets: &'static [&'static str],
) {
    if handles.is_empty() {
        return;
    }
    let apply = move |raised: Option<&str>| {
        let filter = raised.map_or_else(|| base.clone(), EnvFilter::new);
        for handle in &handles {
            let _ = handle.reload(filter.clone());
        }
    };
    let mut current: Option<String> = None;
    let mut step = move || {
        let control = read_control(&data);
        let wanted = control
            .as_ref()
            .and_then(|control| control.level_at(Utc::now()))
            .filter(|level| raised_directives(&directives, level, targets).is_some())
            .map(str::to_owned);
        if wanted == current {
            return;
        }
        apply(
            wanted
                .as_deref()
                .and_then(|level| raised_directives(&directives, level, targets))
                .as_deref(),
        );
        match (&wanted, control.and_then(|control| control.until)) {
            (Some(level), Some(until)) => {
                tracing::info!(level, until = %until, "log level raised for a while");
            }
            (Some(level), None) => tracing::info!(level, "log level raised until stopped"),
            (None, _) => tracing::info!("log level back to this process's own"),
        }
        current = wanted;
    };
    // A window already in force applies from the first line.
    step();
    let spawned = std::thread::Builder::new()
        .name("haarness-log-level".into())
        .spawn(move || {
            loop {
                std::thread::sleep(POLL);
                step();
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "cannot follow log level changes");
    }
}

/// `30m`, `2h`, `90s`, `1h30m`: whole numbers of hours, minutes and seconds.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let mut total = 0_u64;
    let mut number = String::new();
    for character in text.chars() {
        if character.is_ascii_digit() {
            number.push(character);
            continue;
        }
        let unit = match character {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => {
                return Err(format!(
                    "{text}: use hours, minutes or seconds, like 30m or 2h"
                ));
            }
        };
        let value = number
            .parse::<u64>()
            .map_err(|_| format!("{text}: a number goes before each unit, like 1h30m"))?;
        total = total.saturating_add(value.saturating_mul(unit));
        number.clear();
    }
    if !number.is_empty() || total == 0 {
        return Err(format!(
            "{text}: give a length with its unit, like 30m or 2h"
        ));
    }
    Ok(Duration::from_secs(total))
}

fn minutes(duration: Duration) -> String {
    let minutes = duration.as_secs().div_ceil(60);
    if minutes == 1 {
        "1 minute".into()
    } else {
        format!("{minutes} minutes")
    }
}

/// `haarness logs level [LEVEL|stop] [--for D | --until-stopped]`.
pub fn level_command(
    data: &Path,
    level: Option<&str>,
    window: Option<Duration>,
    until_stopped: bool,
) -> Result<()> {
    let now = Utc::now();
    let Some(level) = level else {
        if window.is_some() || until_stopped {
            bail!("name a level to raise: {}", LEVELS.join(", "));
        }
        match read_control(data) {
            Some(control) => match (control.level_at(now), control.until) {
                (Some(level), Some(until)) => println!(
                    "{level} until {} ({} left)",
                    until.with_timezone(&Local).format("%H:%M:%S"),
                    minutes((until - now).to_std().unwrap_or_default())
                ),
                (Some(level), None) => println!("{level} until stopped"),
                (None, _) => println!("each process's own level (the last window has ended)"),
            },
            None => println!("each process's own level"),
        }
        println!("control  {}", control_path(data).display());
        println!("logs     {}", data.join("logs").display());
        return Ok(());
    };
    if level == "stop" {
        if window.is_some() || until_stopped {
            bail!("`stop` takes no length");
        }
        let removed = match fs::remove_file(control_path(data)) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error).context("cannot stop the raised log level"),
        };
        println!(
            "{}",
            if removed {
                "Stopped: each process goes back to its own level within about 2 seconds."
            } else {
                "No raised log level to stop."
            }
        );
        return Ok(());
    }
    let level = level.to_ascii_lowercase();
    if !LEVELS.contains(&level.as_str()) {
        bail!(
            "unknown log level {level}; use one of {} or stop",
            LEVELS.join(", ")
        );
    }
    let until = if until_stopped {
        None
    } else {
        let window = window.unwrap_or(DEFAULT_WINDOW);
        if window > MAX_WINDOW {
            bail!(
                "a raised level lasts at most {}; use --until-stopped to keep it longer",
                minutes(MAX_WINDOW)
            );
        }
        Some(now + chrono::Duration::from_std(window)?)
    };
    write_control(
        data,
        &Control {
            level: level.clone(),
            until,
            started_at: now,
        },
    )?;
    match until {
        Some(until) => println!(
            "{level} until {} (running Haarness processes follow within about 2 seconds)",
            until.with_timezone(&Local).format("%H:%M:%S")
        ),
        None => println!(
            "{level} until `haarness logs level stop` (running Haarness processes follow within about 2 seconds)"
        ),
    }
    println!("A process that already logs at {level} or more keeps its own level.");
    Ok(())
}

/// Known secret shapes removed from one log line, as Haarness's own log
/// lines and hook output get them.
fn scrub(line: &str) -> String {
    haarness_core::redact::text(&haarness_core::redact::urls_in(&without_private_keys(line)))
}

/// A line with every private key written on it (as a JSON string or an
/// escaped error, `-----BEGIN … PRIVATE KEY-----` to `-----END … PRIVATE
/// KEY-----`) replaced.
fn without_private_keys(line: &str) -> String {
    let mut shown = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("-----BEGIN ") {
        let block = &rest[start..];
        let Some(end) = key_label_end(block).and_then(|header| {
            let footer = header + block[header..].find("-----END ")?;
            Some(footer + key_label_end(&block[footer..])?)
        }) else {
            break;
        };
        shown.push_str(&rest[..start]);
        shown.push_str("[redacted private key]");
        rest = &block[end..];
    }
    shown.push_str(rest);
    shown
}

/// Where the first private key label in `text` ends: `PRIVATE KEY-----`
/// (PEM) or `PRIVATE KEY BLOCK-----` (PGP).
fn key_label_end(text: &str) -> Option<usize> {
    ["PRIVATE KEY-----", "PRIVATE KEY BLOCK-----"]
        .into_iter()
        .filter_map(|label| text.find(label).map(|index| index + label.len()))
        .min()
}

/// Whether a line opens a private key block that goes on below it.
fn opens_private_key(line: &str) -> bool {
    line.find("-----BEGIN ")
        .is_some_and(|start| key_label_end(&line[start..]).is_some())
        && !line.contains("-----END ")
}

/// Whether a line closes a private key block.
fn closes_private_key(line: &str) -> bool {
    line.find("-----END ")
        .is_some_and(|start| key_label_end(&line[start..]).is_some())
}

/// Whether a line starts a new log line (`2026-10-18T…`): a private key
/// block whose end was cut off never swallows the lines after it.
fn starts_log_line(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() > 11
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
        && bytes[10] == b'T'
}

/// The most lines a private key block whose end is missing swallows.
const KEY_LINES: usize = 100;

/// `haarness logs export [--output PATH] [--full]`.
pub fn export_command(data: &Path, output: Option<PathBuf>, full: bool) -> Result<()> {
    let logs = data.join("logs");
    let mut files = fs::read_dir(&logs)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| (entry.path(), metadata.len()))
        })
        .collect::<Vec<_>>();
    // Newest first: daily files sort by their date.
    files.sort_by(|a, b| b.0.file_name().cmp(&a.0.file_name()));
    let output = output.unwrap_or_else(|| {
        PathBuf::from(format!(
            "haarness-logs-{}.zip",
            Local::now().format("%Y%m%d-%H%M%S")
        ))
    });
    let (packed, skipped) = export(&files, &output, full)?;
    println!(
        "{} ({packed} file{}{})",
        output.display(),
        if packed == 1 { "" } else { "s" },
        if skipped > 0 {
            format!(", {skipped} older left out past the 200 MB limit")
        } else {
            String::new()
        }
    );
    if !full {
        println!(
            "Known secret shapes were removed; text you typed or a tool printed can still hold one. Check before sharing."
        );
    }
    Ok(())
}

fn export(files: &[(PathBuf, u64)], output: &Path, full: bool) -> Result<(usize, usize)> {
    let file =
        fs::File::create(output).with_context(|| format!("cannot write {}", output.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut total = 0_u64;
    let mut packed = Vec::new();
    let mut skipped = Vec::new();
    for (path, size) in files {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if total.saturating_add(*size) > EXPORT_LIMIT && !packed.is_empty() {
            skipped.push(name);
            continue;
        }
        total = total.saturating_add(*size);
        zip.start_file(format!("logs/{name}"), options)?;
        if full {
            std::io::copy(&mut fs::File::open(path)?, &mut zip)?;
        } else {
            let mut reader = BufReader::new(fs::File::open(path)?);
            let mut line = Vec::new();
            // Lines left out inside a private key that spans lines, if in one.
            let mut key: Option<usize> = None;
            while reader.read_until(b'\n', &mut line)? > 0 {
                let text = String::from_utf8_lossy(&line);
                let ending = if text.ends_with('\n') { "\n" } else { "" };
                let text = text.trim_end_matches('\n');
                if let Some(swallowed) = key {
                    if closes_private_key(text) {
                        key = None;
                        line.clear();
                        continue;
                    }
                    // A key cut off before its end stops at the next log line,
                    // or after `KEY_LINES`, and says so.
                    if starts_log_line(text) || swallowed >= KEY_LINES {
                        zip.write_all(b"[the redacted private key had no end line]\n")?;
                        key = None;
                    } else {
                        key = Some(swallowed + 1);
                        line.clear();
                        continue;
                    }
                }
                if opens_private_key(text) {
                    key = Some(0);
                    let before = &text[..text.find("-----BEGIN ").unwrap_or(0)];
                    zip.write_all(scrub(before).as_bytes())?;
                    zip.write_all(b"[redacted private key]\n")?;
                } else {
                    zip.write_all(scrub(text).as_bytes())?;
                    zip.write_all(ending.as_bytes())?;
                }
                line.clear();
            }
            if key.is_some() {
                zip.write_all(b"[the redacted private key had no end line]\n")?;
            }
        }
        packed.push(name);
    }
    zip.start_file("README.txt", options)?;
    let mut readme = format!(
        "Haarness logs exported {} by haarness {}.\n\n",
        Local::now().to_rfc3339(),
        env!("CARGO_PKG_VERSION")
    );
    readme.push_str(if full {
        "Mode: full. The files are as they were; nothing was removed.\n"
    } else {
        "Mode: scrubbed. Known secret shapes (tokens, keys, passwords, credentials in URLs,\n\
         Authorization and cookie values) were replaced with [redacted]. Text you typed or a\n\
         tool printed can still hold a secret; paths, host names and chat ids stay.\n"
    });
    readme.push_str("\nPacked:\n");
    for name in &packed {
        let _ = writeln!(readme, "  logs/{name}");
    }
    if !skipped.is_empty() {
        readme.push_str("\nLeft out (past the 200 MB limit, oldest first):\n");
        for name in &skipped {
            let _ = writeln!(readme, "  {name}");
        }
    }
    zip.write_all(readme.as_bytes())?;
    zip.finish()?;
    Ok((packed.len(), skipped.len()))
}

/// What a server's stderr log may hold while its reader is behind.
pub const STDERR_QUEUE_LIMIT: usize = 4 * 1024 * 1024;
/// How long a stopping server waits for its queued stderr lines to go out.
const STDERR_DRAIN: Duration = Duration::from_secs(1);

/// A server's stderr log, written out by a thread of its own. A write to a
/// pipe or terminal nobody reads (a paused terminal, a stalled SSH session,
/// `serve 2>&1 | …` whose reader stopped) waits until it is read, and every
/// request the server answered waited behind it: one stalled reader froze
/// the server for every client. Lines now wait here instead, up to `limit`
/// bytes; past it they are left out and counted, and the next line written
/// says how many, so the log file (which has them all) explains the gap.
pub struct StderrQueue {
    sender: std::sync::mpsc::Sender<Vec<u8>>,
    state: std::sync::Arc<QueueState>,
}

struct QueueState {
    /// Bytes waiting to be written.
    queued: std::sync::atomic::AtomicUsize,
    /// Lines left out since the last one taken.
    skipped: std::sync::atomic::AtomicUsize,
    limit: usize,
    /// Named in the line that reports left-out lines.
    logs: String,
}

/// Whether this process's stderr log goes through a [`StderrQueue`]: a
/// panic's message then reaches stderr through it (as a log line) and not
/// by a write that could wait for a reader that stopped.
pub static STDERR_QUEUED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The queue's state, for [`stderr_drained`].
static QUEUE: std::sync::OnceLock<std::sync::Arc<QueueState>> = std::sync::OnceLock::new();

/// Whether nothing waits in the stderr queue, waiting up to `within` for it
/// to go out. A reader that is there reads it within that; one that stopped
/// leaves it waiting.
pub fn stderr_drained(within: Duration) -> bool {
    let Some(state) = QUEUE.get() else {
        return true;
    };
    let deadline = std::time::Instant::now() + within;
    loop {
        if state.queued.load(std::sync::atomic::Ordering::Acquire) == 0 {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Held for the life of the process: dropped, it gives what is still
/// queued up to a second to go out.
pub struct StderrQueueGuard(std::sync::Arc<QueueState>);

impl Drop for StderrQueueGuard {
    fn drop(&mut self) {
        let deadline = std::time::Instant::now() + STDERR_DRAIN;
        while self.0.queued.load(std::sync::atomic::Ordering::Acquire) > 0
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// `writer` behind a queue of at most `limit` bytes; `None` when its
/// thread cannot start, and the caller writes in place. `logs` is named in
/// the line that reports left-out lines.
pub fn stderr_queue<W: std::io::Write + Send + 'static>(
    mut writer: W,
    limit: usize,
    logs: &Path,
) -> Option<(StderrQueue, StderrQueueGuard)> {
    use std::sync::atomic::Ordering;
    let state = std::sync::Arc::new(QueueState {
        queued: 0.into(),
        skipped: 0.into(),
        limit,
        logs: logs.display().to_string(),
    });
    let (sender, receiver) = std::sync::mpsc::channel::<Vec<u8>>();
    let _ = QUEUE.set(state.clone());
    let worker = state.clone();
    std::thread::Builder::new()
        .name("haarness-stderr-log".into())
        .spawn(move || {
            for line in receiver {
                // A reader that is gone for good fails every write; what is
                // queued still drains, so the server never waits on it.
                let _ = writer.write_all(&line);
                let _ = writer.flush();
                worker.queued.fetch_sub(line.len(), Ordering::AcqRel);
            }
        })
        .ok()?;
    Some((
        StderrQueue {
            sender,
            state: state.clone(),
        },
        StderrQueueGuard(state),
    ))
}

impl std::io::Write for &StderrQueue {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use std::sync::atomic::Ordering;
        let state = &self.state;
        if state.queued.fetch_add(buf.len(), Ordering::AcqRel) + buf.len() > state.limit {
            state.queued.fetch_sub(buf.len(), Ordering::AcqRel);
            state.skipped.fetch_add(1, Ordering::AcqRel);
            return Ok(buf.len());
        }
        // The first line taken after a gap says how many were left out, so
        // the notice stands where the gap is.
        let mut line = Vec::with_capacity(buf.len());
        let skipped = state.skipped.swap(0, Ordering::AcqRel);
        if skipped > 0 {
            let notice = format!(
                "haarness: {skipped} log lines were left out here while this output was not read; {} has them all\n",
                state.logs
            );
            state.queued.fetch_add(notice.len(), Ordering::AcqRel);
            line.extend_from_slice(notice.as_bytes());
        }
        line.extend_from_slice(buf);
        let length = line.len();
        if self.sender.send(line).is_err() {
            state.queued.fetch_sub(length, Ordering::AcqRel);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for StderrQueue {
    type Writer = &'a StderrQueue;
    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("haarness-logs-{name}-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn durations_read_hours_minutes_and_seconds() {
        assert_eq!(parse_duration("30m"), Ok(Duration::from_secs(1800)));
        assert_eq!(parse_duration("1h30m"), Ok(Duration::from_secs(5400)));
        assert_eq!(parse_duration("90s"), Ok(Duration::from_secs(90)));
        for bad in ["", "30", "m", "1.5h", "2d", "0m"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_raised_level_lasts_its_window_and_stop_ends_it() {
        let data = scratch("level");
        level_command(&data, Some("DEBUG"), Some(Duration::from_secs(600)), false).unwrap();
        let control = read_control(&data).unwrap();
        assert_eq!(control.level, "debug");
        let now = Utc::now();
        assert_eq!(control.level_at(now), Some("debug"));
        assert_eq!(control.level_at(now + chrono::Duration::minutes(11)), None);

        level_command(&data, Some("info"), None, true).unwrap();
        let control = read_control(&data).unwrap();
        assert_eq!(control.until, None);
        assert_eq!(
            control.level_at(now + chrono::Duration::days(30)),
            Some("info")
        );

        assert!(level_command(&data, Some("loud"), None, false).is_err());
        assert!(level_command(&data, Some("debug"), Some(MAX_WINDOW * 2), false).is_err());
        level_command(&data, Some("stop"), None, false).unwrap();
        assert!(read_control(&data).is_none());
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn a_running_process_follows_the_control_file() {
        use tracing_subscriber::layer::SubscriberExt as _;
        let data = scratch("watch");
        let (filter, handle) = reload::Layer::new(EnvFilter::new("warn"));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let writer = {
            let seen = seen.clone();
            move || Line(seen.clone())
        };
        let subscriber =
            tracing_subscriber::registry().with(tracing_subscriber::Layer::with_filter(
                tracing_subscriber::fmt::layer()
                    .with_writer(writer)
                    .with_ansi(false),
                filter,
            ));
        let _guard = tracing::subscriber::set_default(subscriber);
        level_command(&data, Some("debug"), None, true).unwrap();
        watch(
            data.clone(),
            EnvFilter::new("warn"),
            "warn".into(),
            vec![handle],
            TEST_TARGETS,
        );
        tracing::debug!("DEBUG_WHILE_RAISED");
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|line| line.contains("DEBUG_WHILE_RAISED"))
        );
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn a_raised_level_never_lowers_a_process_started_more_verbose() {
        use tracing_subscriber::layer::SubscriberExt as _;
        let data = scratch("lower");
        let (filter, handle) = reload::Layer::new(EnvFilter::new("debug"));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let writer = {
            let seen = seen.clone();
            move || Line(seen.clone())
        };
        let subscriber =
            tracing_subscriber::registry().with(tracing_subscriber::Layer::with_filter(
                tracing_subscriber::fmt::layer()
                    .with_writer(writer)
                    .with_ansi(false),
                filter,
            ));
        let _guard = tracing::subscriber::set_default(subscriber);
        level_command(&data, Some("info"), None, true).unwrap();
        watch(
            data.clone(),
            EnvFilter::new("debug"),
            "debug".into(),
            vec![handle],
            TEST_TARGETS,
        );
        tracing::debug!("DEBUG_KEPT");
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|line| line.contains("DEBUG_KEPT"))
        );
        fs::remove_dir_all(data).unwrap();
    }

    /// The targets these tests' events have.
    const TEST_TARGETS: &[&str] = &["haarness"];

    #[test]
    fn a_raised_level_reaches_haarness_past_a_louder_directive_for_another_crate() {
        use tracing_subscriber::layer::SubscriberExt as _;
        let data = scratch("other-crate");
        let base = "info,hyper=trace";
        let (filter, handle) = reload::Layer::new(EnvFilter::new(base));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let writer = {
            let seen = seen.clone();
            move || Line(seen.clone())
        };
        let subscriber =
            tracing_subscriber::registry().with(tracing_subscriber::Layer::with_filter(
                tracing_subscriber::fmt::layer()
                    .with_writer(writer)
                    .with_ansi(false),
                filter,
            ));
        let _guard = tracing::subscriber::set_default(subscriber);
        level_command(&data, Some("debug"), None, true).unwrap();
        watch(
            data.clone(),
            EnvFilter::new(base),
            base.into(),
            vec![handle],
            TEST_TARGETS,
        );
        tracing::debug!("DEBUG_RAISED");
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|line| line.contains("DEBUG_RAISED"))
        );
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn a_raised_filter_keeps_every_louder_directive_of_the_process() {
        let targets = &["haarness", "haarness_cli", "haarness_core"];
        assert_eq!(
            raised_directives("info,hyper=trace", "debug", targets).as_deref(),
            Some("info,hyper=trace,haarness=debug,haarness_cli=debug,haarness_core=debug")
        );
        assert_eq!(
            raised_directives("warn,haarness_core=trace", "debug", targets).as_deref(),
            Some("warn,haarness_core=trace,haarness=debug,haarness_cli=debug"),
            "a target already louder keeps its level"
        );
        let started = "warn,haarness=debug,haarness_cli=debug,haarness_core=debug";
        assert_eq!(raised_directives(started, "info", targets), None);
        assert_eq!(raised_directives(started, "debug", targets), None);
        assert!(raised_directives(started, "trace", targets).is_some());
        assert!(
            raised_directives("haarness", "debug", targets).is_none(),
            "a bare target logs everything"
        );
        assert_eq!(
            raised_directives("", "info", &["haarness"]).as_deref(),
            Some("haarness=info")
        );
    }

    struct Line(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
    impl std::io::Write for Line {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(bytes).into_owned());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn an_export_packs_scrubbed_logs_and_says_what_it_holds() {
        let data = scratch("export");
        fs::create_dir_all(data.join("logs")).unwrap();
        fs::write(
            data.join("logs/haarness.log.2026-10-18"),
            "INFO request failed Authorization: Bearer sk-live-1234567890abcdef\n\
             WARN mcp: cannot reach https://mcp.example.com/sse?X-Amz-Signature=QUERYSECRET42 (refused)\n\
             DEBUG discovery url: Url { scheme: \"https\", cannot_be_a_base: false, username: \"DEBUGUSER\", password: Some(\"DEBUGPASS\"), host: Some(Domain(\"mcp.example.com\")), port: None, path: \"/\", query: None, fragment: None }\n\
             ERROR key read failed: \"-----BEGIN RSA PRIVATE KEY-----\\nMIIEINLINEKEY\\n-----END RSA PRIVATE KEY-----\" rest\n\
             DEBUG loaded -----BEGIN PRIVATE KEY-----\n\
             MIIBLOCKLINEONE\n\
             MIIBLOCKLINETWO\n\
             -----END PRIVATE KEY-----\n\
             INFO plain line\n\
             WARN gpg said -----BEGIN PGP PRIVATE KEY BLOCK-----\n\
             lQPGBPGPLINE\n\
             -----END PGP PRIVATE KEY BLOCK-----\n\
             ERROR cut off -----BEGIN EC PRIVATE KEY-----\n\
             MHcCUTOFFLINE\n\
             2026-10-18T07:00:00Z 1 serve  INFO AFTER_THE_CUT\n",
        )
        .unwrap();
        let output = data.join("out.zip");
        export_command(&data, Some(output.clone()), false).unwrap();
        let mut archive = zip::ZipArchive::new(fs::File::open(&output).unwrap()).unwrap();
        let mut log = String::new();
        std::io::Read::read_to_string(
            &mut archive.by_name("logs/haarness.log.2026-10-18").unwrap(),
            &mut log,
        )
        .unwrap();
        for secret in [
            "sk-live-1234567890abcdef",
            "QUERYSECRET42",
            "MIIEINLINEKEY",
            "MIIBLOCKLINEONE",
            "MIIBLOCKLINETWO",
            "lQPGBPGPLINE",
            "MHcCUTOFFLINE",
            "DEBUGUSER",
            "DEBUGPASS",
        ] {
            assert!(!log.contains(secret), "{secret} in {log}");
        }
        assert!(
            log.contains("https://mcp.example.com/sse?X-Amz-Signature="),
            "{log}"
        );
        assert!(
            log.contains("\" rest\n"),
            "the text after a key stays: {log}"
        );
        assert!(
            log.contains("DEBUG loaded [redacted private key]\nINFO plain line"),
            "{log}"
        );
        assert!(
            log.contains("WARN gpg said [redacted private key]\nERROR cut off"),
            "{log}"
        );
        assert!(
            log.contains(
                "[the redacted private key had no end line]\n2026-10-18T07:00:00Z 1 serve  INFO AFTER_THE_CUT\n"
            ),
            "a key cut off stops at the next log line: {log}"
        );
        assert!(
            log.contains("[redacted]") && log.contains("INFO plain line\n"),
            "{log}"
        );
        let mut readme = String::new();
        std::io::Read::read_to_string(&mut archive.by_name("README.txt").unwrap(), &mut readme)
            .unwrap();
        assert!(
            readme.contains("Mode: scrubbed") && readme.contains("logs/haarness.log.2026-10-18")
        );
        fs::remove_dir_all(data).unwrap();
    }

    /// A writer that takes nothing until it is let go, like a pipe or a
    /// terminal nobody reads.
    struct Stalled {
        open: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl std::io::Write for Stalled {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let (open, changed) = &*self.open;
            let mut open = open.lock().unwrap();
            while !*open {
                open = changed.wait(open).unwrap();
            }
            self.written.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_stalled_stderr_reader_never_holds_a_log_line_and_hears_what_it_missed() {
        use std::io::Write as _;
        let open = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let written = std::sync::Arc::default();
        let stalled = Stalled {
            open: open.clone(),
            written: std::sync::Arc::clone(&written),
        };
        let (queue, guard) = stderr_queue(stalled, 64, Path::new("/data/logs")).unwrap();
        let started = std::time::Instant::now();
        // Far more than the queue holds, while nothing is read.
        for line in 0..10_000 {
            (&queue)
                .write_all(format!("line {line:05}\n").as_bytes())
                .unwrap();
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a log write waited for the reader: {:?}",
            started.elapsed()
        );
        // The reader catches up.
        *open.0.lock().unwrap() = true;
        open.1.notify_all();
        (&queue).write_all(b"after\n").unwrap();
        drop(guard);
        let text = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        let lines: Vec<_> = text.lines().collect();
        // The lines from before the gap, in order, then the notice, then
        // the line after it.
        let kept = lines
            .iter()
            .take_while(|line| line.starts_with("line "))
            .count();
        for (index, line) in lines[..kept].iter().enumerate() {
            assert_eq!(*line, format!("line {index:05}"), "{text}");
        }
        assert_eq!(lines.len(), kept + 2, "{text}");
        let notice = lines[kept];
        assert!(notice.ends_with("/data/logs has them all"), "{notice}");
        let skipped: usize = notice
            .trim_start_matches("haarness: ")
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(kept + skipped, 10_000, "{text}");
        assert_eq!(lines[kept + 1], "after");
        assert!(text.ends_with("after\n"), "{text}");
    }
}
