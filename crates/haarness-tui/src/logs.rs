//! `/logs`: this computer's Haarness logs from the terminal. `/logs export
//! [path]` packs them into a zip with known secret shapes removed and says
//! where it wrote it; `/logs detail [on|off]` has every Haarness process here
//! log in detail (debug) for 30 minutes, after which they go back by
//! themselves. Both run the `haarness logs` command the CLI owns, with the
//! same data folder, as Desktop's Diagnostics rows do, so the three agree.
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use serde::Deserialize;

use crate::state::App;

/// How long an export of up to 200 MB of logs may take.
const TIMEOUT: Duration = Duration::from_secs(180);
/// How long detailed logging lasts, as Desktop's switch keeps it.
const DETAIL_WINDOW: &str = "30m";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Request {
    Status,
    /// Where to write the zip, as typed; the Downloads folder without one.
    Export(Option<String>),
    /// On, off, or (none) the other way round from now.
    Detail(Option<bool>),
}

/// `/logs`, `/logs export [path]`, `/logs detail [on|off]`.
pub(crate) fn parse(arguments: &str) -> Option<Request> {
    let arguments = arguments.trim();
    let (action, rest) = arguments
        .split_once(char::is_whitespace)
        .map_or((arguments, ""), |(action, rest)| (action, rest.trim()));
    match (action.to_ascii_lowercase().as_str(), rest) {
        ("" | "status", "") => Some(Request::Status),
        ("export", "") => Some(Request::Export(None)),
        ("export", path) => Some(Request::Export(Some(path.to_owned()))),
        ("detail" | "detailed", rest) => match rest.to_ascii_lowercase().as_str() {
            "" | "toggle" => Some(Request::Detail(None)),
            "on" => Some(Request::Detail(Some(true))),
            "off" | "stop" => Some(Request::Detail(Some(false))),
            _ => None,
        },
        _ => None,
    }
}

/// The raised level, as `haarness logs level --json` reports it.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Raised {
    pub level: Option<String>,
    pub until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub until_stopped: bool,
}

/// What `haarness logs export --json` wrote.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Exported {
    pub path: PathBuf,
    #[serde(default)]
    pub skipped: usize,
}

#[derive(Debug)]
pub(crate) enum Outcome {
    Level(Raised),
    Exported(Exported),
}

#[derive(Default)]
pub(crate) struct Logs {
    pending: Option<(
        bool,
        tokio::sync::oneshot::Receiver<Result<Outcome, String>>,
    )>,
    /// The `haarness` executable to run; the running one unless a test says otherwise.
    pub program: Option<PathBuf>,
}

impl Logs {
    fn program(&self) -> Option<PathBuf> {
        if self.program.is_some() {
            return self.program.clone();
        }
        // A test binary is not `haarness`: running it would run the tests.
        if cfg!(test) {
            return None;
        }
        std::env::current_exe().ok()
    }
}

/// The folder a zip goes to when no path is typed: Downloads, else home.
fn downloads() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| {
            dirs.download_dir()
                .map(Path::to_path_buf)
                .or_else(|| Some(dirs.home_dir().to_path_buf()))
        })
        .unwrap_or_else(std::env::temp_dir)
}

/// A path as typed in a shell: surrounding quotes go, and off Windows a
/// backslash keeps the character after it (`bug\ report`). Windows paths keep
/// their backslashes, which are separators there.
fn unquote(typed: &str) -> String {
    let typed = typed.trim();
    let quoted = typed.len() >= 2
        && ((typed.starts_with('"') && typed.ends_with('"'))
            || (typed.starts_with('\'') && typed.ends_with('\'')));
    let typed = if quoted {
        &typed[1..typed.len() - 1]
    } else {
        typed
    };
    if cfg!(windows) || quoted {
        return typed.to_owned();
    }
    let mut plain = String::with_capacity(typed.len());
    let mut characters = typed.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => plain.extend(characters.next()),
            character => plain.push(character),
        }
    }
    plain
}

/// Where `/logs export [path]` writes: a typed file (`.zip` added when it
/// has no extension), a typed folder (or one ending in a separator) with the
/// default name inside, or the Downloads folder. `~` is the home folder; a
/// relative path starts where the TUI started.
pub(crate) fn export_path(
    typed: Option<&str>,
    now: DateTime<Local>,
    home: Option<&Path>,
) -> PathBuf {
    let name = format!("haarness-logs-{}.zip", now.format("%Y-%m-%d-%H%M%S"));
    let typed = typed.map(unquote).filter(|typed| !typed.is_empty());
    let Some(typed) = typed else {
        return downloads().join(name);
    };
    let expanded = match (typed.strip_prefix('~'), home) {
        (Some(rest), Some(home))
            if rest.is_empty() || rest.starts_with(std::path::is_separator) =>
        {
            home.join(rest.trim_start_matches(std::path::is_separator))
        }
        _ => PathBuf::from(&typed),
    };
    let mut path = std::path::absolute(&expanded).unwrap_or(expanded);
    if typed.ends_with(std::path::is_separator) || path.is_dir() {
        return path.join(name);
    }
    if path.extension().is_none() {
        path.set_extension("zip");
    }
    path
}

/// Whether writing the zip at `path` would replace a file that is not one:
/// an export never overwrites someone's notes or a log they typed by mistake.
pub(crate) fn replaces_other_file(path: &Path) -> bool {
    use std::io::Read as _;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    if !path.is_file() {
        return true;
    }
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic).is_err() || magic != *b"PK\x03\x04"
}

/// Runs `haarness logs <args> --json` and returns what it printed.
async fn run(program: PathBuf, args: Vec<String>) -> Result<String, String> {
    let mut command = tokio::process::Command::new(&program);
    command
        .arg("logs")
        .args(&args)
        .arg("--json")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(TIMEOUT, command.output())
        .await
        .map_err(|_| "Haarness took too long to answer".to_owned())?
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.trim_start_matches("Error: ").to_owned())
        .unwrap_or_else(|| output.status.to_string()))
}

async fn level(program: PathBuf, args: Vec<String>) -> Result<Outcome, String> {
    let text = run(program, args).await?;
    serde_json::from_str(&text)
        .map(Outcome::Level)
        .map_err(|error| error.to_string())
}

/// Detailed logging on, off, or the other way round from what is in force.
async fn detail(program: PathBuf, on: Option<bool>) -> Result<Outcome, String> {
    let on = match on {
        Some(on) => on,
        None => match level(program.clone(), vec!["level".into()]).await? {
            Outcome::Level(level) => level.level.is_none(),
            Outcome::Exported(_) => unreachable!("a level read reports a level"),
        },
    };
    let args = if on {
        vec![
            "level".into(),
            "debug".into(),
            "--for".into(),
            DETAIL_WINDOW.into(),
        ]
    } else {
        vec!["level".into(), "stop".into()]
    };
    level(program, args).await
}

async fn export(program: PathBuf, path: PathBuf) -> Result<Outcome, String> {
    let text = run(
        program,
        vec![
            "export".into(),
            "--output".into(),
            path.display().to_string(),
        ],
    )
    .await?;
    serde_json::from_str(&text)
        .map(Outcome::Exported)
        .map_err(|error| error.to_string())
}

impl App {
    pub(crate) fn logs_command(&mut self, arguments: &str) {
        let Some(request) = parse(arguments) else {
            self.notice = self
                .language
                .text("Use /logs export [path] or /logs detail [on|off]")
                .into();
            return;
        };
        if let Some((exporting, _)) = &self.logs.pending {
            self.notice = self
                .language
                .text(if *exporting {
                    "Still exporting logs…"
                } else {
                    "Still changing detailed logging…"
                })
                .into();
            return;
        }
        let Some(program) = self.logs.program() else {
            self.notice = self
                .language
                .text("Haarness could not find its own program to read its logs")
                .into();
            return;
        };
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let exporting = matches!(request, Request::Export(_));
        match request {
            Request::Status => {
                tokio::spawn(async move {
                    let _ = sender.send(level(program, vec!["level".into()]).await);
                });
            }
            Request::Detail(on) => {
                tokio::spawn(async move {
                    let _ = sender.send(detail(program, on).await);
                });
            }
            Request::Export(typed) => {
                let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
                let path = export_path(typed.as_deref(), Local::now(), home.as_deref());
                if replaces_other_file(&path) {
                    self.notice = self.language.format(
                        "{path} is not a zip of logs; choose another name",
                        &[("path", &path.display().to_string())],
                    );
                    return;
                }
                self.notice = self.language.text("Exporting this computer's logs…").into();
                tokio::spawn(async move {
                    let _ = sender.send(export(program, path).await);
                });
            }
        }
        self.logs.pending = Some((exporting, receiver));
    }

    /// Frame-tick hook: say how a finished `/logs` command went.
    pub(crate) fn poll_logs(&mut self) {
        let Some((exporting, receiver)) = &mut self.logs.pending else {
            return;
        };
        let exporting = *exporting;
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                Err("the command stopped without answering".into())
            }
        };
        self.logs.pending = None;
        self.notice = match result {
            Ok(Outcome::Exported(exported)) => {
                let path = exported.path.display().to_string();
                crate::terminal::copy_to_clipboard(&path);
                // Short enough for the status line: the home folder as `~`.
                let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
                let shown = home
                    .and_then(|home| {
                        exported
                            .path
                            .strip_prefix(home)
                            .ok()
                            .map(|rest| format!("~/{}", rest.display()))
                    })
                    .unwrap_or_else(|| path.clone());
                let mut text = self
                    .language
                    .format("Logs saved to {path} · path copied", &[("path", &shown)]);
                if exported.skipped > 0 {
                    text.push_str(" · ");
                    text.push_str(&self.language.format(
                        "{count} older files left out past 200 MB",
                        &[("count", &exported.skipped.to_string())],
                    ));
                }
                text
            }
            Ok(Outcome::Level(level)) => self.detail_notice(&level),
            Err(error) if exporting => self
                .language
                .format("Could not export logs: {error}", &[("error", &error)]),
            Err(error) => self.language.format(
                "Could not change detailed logging: {error}",
                &[("error", &error)],
            ),
        };
    }

    pub(crate) fn detail_notice(&self, level: &Raised) -> String {
        match (&level.level, level.until) {
            (None, _) => self
                .language
                .text("Detailed logging off · /logs detail on · /logs export [path]")
                .to_owned(),
            (Some(_), Some(until)) => self.language.format(
                "Detailed logging on until {time} · /logs detail off stops it",
                &[(
                    "time",
                    &until.with_timezone(&Local).format("%H:%M").to_string(),
                )],
            ),
            (Some(_), None) => self
                .language
                .text("Detailed logging on until stopped · /logs detail off stops it")
                .to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_arguments_read_as_people_type_them() {
        assert_eq!(parse(""), Some(Request::Status));
        assert_eq!(parse(" export "), Some(Request::Export(None)));
        assert_eq!(
            parse("export ~/Desktop/run 2.zip"),
            Some(Request::Export(Some("~/Desktop/run 2.zip".into())))
        );
        assert_eq!(parse("detail"), Some(Request::Detail(None)));
        assert_eq!(parse("detail ON"), Some(Request::Detail(Some(true))));
        assert_eq!(parse("detail off"), Some(Request::Detail(Some(false))));
        assert_eq!(parse("detail loud"), None);
        assert_eq!(parse("tail"), None);
    }

    #[test]
    fn an_export_goes_where_it_was_typed_or_to_downloads() {
        let now = Local::now();
        let name = format!("haarness-logs-{}.zip", now.format("%Y-%m-%d-%H%M%S"));
        let home =
            std::env::temp_dir().join(format!("haarness-logs-home-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(home.join("Desktop")).unwrap();
        assert_eq!(
            export_path(Some("~/Desktop"), now, Some(&home)),
            home.join("Desktop").join(&name)
        );
        assert_eq!(
            export_path(Some("~/run.zip"), now, Some(&home)),
            home.join("run.zip")
        );
        assert_eq!(
            export_path(Some("~/new/"), now, Some(&home)),
            home.join("new").join(&name)
        );
        let relative = export_path(Some("logs.zip"), now, Some(&home));
        assert!(
            relative.is_absolute() && relative.ends_with("logs.zip"),
            "{relative:?}"
        );
        let default = export_path(None, now, Some(&home));
        assert!(
            default.is_absolute() && default.ends_with(&name),
            "{default:?}"
        );
        // As a shell takes it: quotes, an escaped space, no extension.
        assert_eq!(
            export_path(Some("\"~/bug report/\""), now, Some(&home)),
            home.join("bug report").join(&name)
        );
        assert_eq!(
            export_path(Some("'~/My Logs'"), now, Some(&home)),
            home.join("My Logs.zip")
        );
        if !cfg!(windows) {
            assert_eq!(
                export_path(Some(r"~/bug\ report/run"), now, Some(&home)),
                home.join("bug report").join("run.zip")
            );
        }
        // A zip it wrote before may be replaced; someone's notes may not.
        let notes = home.join("notes.txt");
        std::fs::write(&notes, "keep me").unwrap();
        assert!(replaces_other_file(&notes));
        assert!(replaces_other_file(&home.join("Desktop")));
        let earlier = home.join("earlier.zip");
        std::fs::write(&earlier, b"PK\x03\x04rest").unwrap();
        assert!(!replaces_other_file(&earlier));
        assert!(!replaces_other_file(&home.join("new.zip")));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn the_status_reads_the_cli_report() {
        let level: Raised = serde_json::from_str(
            r#"{"level":"debug","until":"2026-10-22T10:30:00Z","untilStopped":false,"control":"/x","logs":"/y"}"#,
        )
        .unwrap();
        assert_eq!(level.level.as_deref(), Some("debug"));
        assert!(level.until.is_some());
        let off: Raised =
            serde_json::from_str(r#"{"level":null,"until":null,"untilStopped":false}"#).unwrap();
        assert_eq!(off, Raised::default());
        let exported: Exported =
            serde_json::from_str(r#"{"path":"/tmp/a.zip","files":2,"skipped":1,"scrubbed":true}"#)
                .unwrap();
        assert_eq!(
            (exported.path, exported.skipped),
            (PathBuf::from("/tmp/a.zip"), 1)
        );
    }
}
