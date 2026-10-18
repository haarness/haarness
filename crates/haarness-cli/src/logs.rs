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

/// Whether `level` logs more than a process whose own filter logs at most
/// `own`: a raised level never lowers a process started more verbose (with
/// `--log-level debug`, say).
fn louder(level: &str, own: Option<LevelFilter>) -> bool {
    level
        .parse::<LevelFilter>()
        .is_ok_and(|level| own.is_none_or(|own| level > own))
}

/// Swaps the log filters of this process whenever the control file asks for
/// a level louder than its own, and back to `base` (the level the process
/// started with) once no such window is in force. Runs for the life of the
/// process.
pub fn watch(
    data: PathBuf,
    base: EnvFilter,
    handles: Vec<reload::Handle<EnvFilter, Registry>>,
    raised: fn(&str) -> String,
) {
    if handles.is_empty() {
        return;
    }
    let own = base.max_level_hint();
    let apply = move |level: Option<&str>| {
        let filter = level.map_or_else(|| base.clone(), |level| EnvFilter::new(raised(level)));
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
            .filter(|level| louder(level, own))
            .map(str::to_owned);
        if wanted == current {
            return;
        }
        apply(wanted.as_deref());
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
    const LABEL: &str = "PRIVATE KEY-----";
    let mut shown = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("-----BEGIN ") {
        let block = &rest[start..];
        let Some(end) = block.find(LABEL).and_then(|header| {
            let footer = header + block[header..].find("-----END ")?;
            Some(footer + block[footer..].find(LABEL)? + LABEL.len())
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

/// Whether a line opens a private key block that goes on below it.
fn opens_private_key(line: &str) -> bool {
    line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") && !line.contains("-----END ")
}

/// Whether a line closes a private key block.
fn closes_private_key(line: &str) -> bool {
    line.contains("-----END ") && line.contains("PRIVATE KEY-----")
}

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
            // Inside a private key that spans lines: they are left out.
            let mut key = false;
            while reader.read_until(b'\n', &mut line)? > 0 {
                let text = String::from_utf8_lossy(&line);
                let ending = if text.ends_with('\n') { "\n" } else { "" };
                let text = text.trim_end_matches('\n');
                if key {
                    key = !closes_private_key(text);
                } else if opens_private_key(text) {
                    key = true;
                    let before = &text[..text.find("-----BEGIN ").unwrap_or(0)];
                    zip.write_all(scrub(before).as_bytes())?;
                    zip.write_all(b"[redacted private key]\n")?;
                } else {
                    zip.write_all(scrub(text).as_bytes())?;
                    zip.write_all(ending.as_bytes())?;
                }
                line.clear();
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
            vec![handle],
            str::to_owned,
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
            vec![handle],
            str::to_owned,
        );
        tracing::debug!("DEBUG_KEPT");
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|line| line.contains("DEBUG_KEPT"))
        );
        assert!(louder("trace", Some(LevelFilter::DEBUG)));
        assert!(!louder("info", Some(LevelFilter::DEBUG)));
        assert!(!louder("debug", Some(LevelFilter::DEBUG)));
        fs::remove_dir_all(data).unwrap();
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
             ERROR key read failed: \"-----BEGIN RSA PRIVATE KEY-----\\nMIIEINLINEKEY\\n-----END RSA PRIVATE KEY-----\" rest\n\
             DEBUG loaded -----BEGIN PRIVATE KEY-----\n\
             MIIBLOCKLINEONE\n\
             MIIBLOCKLINETWO\n\
             -----END PRIVATE KEY-----\n\
             INFO plain line\n",
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
}
