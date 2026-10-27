//! Durable native schedules backed by launchd, systemd user timers, or
//! Windows Task Scheduler. This crate owns definitions and OS registration;
//! the CLI owns actual agent execution through the normal server API.

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike as _, Local, SecondsFormat, Timelike as _, Utc};
use directories::{BaseDirs, ProjectDirs};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};
use uuid::Uuid;

const MIN_INTERVAL_SECONDS: u64 = 60;
const MAX_EXPANSIONS: usize = 512;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    Launchd,
    SystemdUser,
    WindowsTask,
}

impl Backend {
    pub fn current() -> Result<Self> {
        match std::env::consts::OS {
            "macos" => Ok(Self::Launchd),
            "linux" => Ok(Self::SystemdUser),
            "windows" => Ok(Self::WindowsTask),
            os => anyhow::bail!("scheduling is not supported on {os}"),
        }
    }

    #[must_use]
    pub const fn source(self) -> RunSource {
        match self {
            Self::Launchd => RunSource::Launchd,
            Self::SystemdUser => RunSource::SystemdUser,
            Self::WindowsTask => RunSource::WindowsTask,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Trigger {
    Interval { seconds: u64 },
    Cron { expression: String },
    Once { at: DateTime<Utc> },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub id: Uuid,
    pub backend: Backend,
    pub name: String,
    pub workspace: PathBuf,
    /// The chat folder inside `workspace` its runs work in, shared with the
    /// chat the schedule was made in (one without a project). Absent, each
    /// run is a chat of the workspace (or, in the chat home, a chat with a
    /// new folder of its own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    pub trust_project: bool,
    pub enabled: bool,
    pub trigger: Trigger,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Default)]
pub struct CreateSchedule {
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub request: Option<String>,
    pub workspace: PathBuf,
    /// See [`Schedule::directory`]; it must be a folder inside `workspace`.
    pub directory: Option<PathBuf>,
    pub model: Option<String>,
    pub agent: Option<String>,
    pub variant: Option<String>,
    pub trust_project: bool,
    pub enabled: bool,
    pub trigger: Option<Trigger>,
    pub timezone: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct UpdateSchedule {
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub request: Option<String>,
    pub workspace: Option<PathBuf>,
    pub model: Option<Option<String>>,
    pub agent: Option<Option<String>>,
    pub variant: Option<Option<String>>,
    pub trust_project: Option<bool>,
    pub enabled: Option<bool>,
    pub trigger: Option<Trigger>,
    pub timezone: Option<Option<String>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunSource {
    Manual,
    Launchd,
    SystemdUser,
    WindowsTask,
}

impl std::fmt::Display for RunSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manual => formatter.write_str("manual"),
            Self::Launchd => formatter.write_str("launchd"),
            Self::SystemdUser => formatter.write_str("systemd-user"),
            Self::WindowsTask => formatter.write_str("windows-task"),
        }
    }
}

impl std::str::FromStr for RunSource {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "manual" => Ok(Self::Manual),
            "launchd" => Ok(Self::Launchd),
            "systemd-user" => Ok(Self::SystemdUser),
            "windows-task" => Ok(Self::WindowsTask),
            _ => anyhow::bail!("unknown schedule run source: {value}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Success,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRun {
    pub id: Uuid,
    pub schedule_id: Uuid,
    pub status: RunStatus,
    pub source: RunSource,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SchedulerPaths {
    pub config: PathBuf,
    pub state: PathBuf,
    pub launchd: PathBuf,
    pub systemd: PathBuf,
    pub windows: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Scheduler {
    paths: SchedulerPaths,
    executable: PathBuf,
    data_root: Option<PathBuf>,
    backend: Backend,
    install: bool,
    /// Storage variables a native runner must see; see `storage_environment`.
    environment: Vec<(&'static str, String)>,
}

impl Scheduler {
    pub fn discover(executable: PathBuf, data_root: Option<PathBuf>) -> Result<Self> {
        let project = ProjectDirs::from("dev", "haarness", "haarness")
            .context("OS data directories are unavailable")?;
        let base = BaseDirs::new().context("OS home directory is unavailable")?;
        // HAARNESS_HOME keeps everything under one folder, schedules too, so
        // an isolated install never shares the person's schedule list.
        let home = std::env::var_os("HAARNESS_HOME")
            .map(PathBuf::from)
            .filter(|root| root.is_absolute());
        let (config, state) = match (&data_root, home) {
            (Some(root), _) => (root.join("config"), root.join("data")),
            (None, Some(home)) => (home.join("config"), home.join("state")),
            (None, None) => (
                project.config_dir().to_path_buf(),
                project.data_local_dir().to_path_buf(),
            ),
        };
        let systemd = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| base.home_dir().join(".config"))
            .join("systemd/user");
        Ok(Self {
            paths: SchedulerPaths {
                windows: config.join("scheduler/tasks"),
                config,
                state,
                launchd: base.home_dir().join("Library/LaunchAgents"),
                systemd,
            },
            executable,
            data_root,
            backend: Backend::current()?,
            install: std::env::var_os("HAARNESS_SCHEDULE_SKIP_INSTALL").is_none(),
            environment: storage_environment(),
        })
    }

    #[must_use]
    pub fn with_paths(
        paths: SchedulerPaths,
        executable: PathBuf,
        data_root: Option<PathBuf>,
        backend: Backend,
        install: bool,
    ) -> Self {
        Self {
            paths,
            executable,
            data_root,
            backend,
            install,
            environment: Vec::new(),
        }
    }

    #[must_use]
    pub const fn backend(&self) -> Backend {
        self.backend
    }

    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.paths.state.join("logs/scheduler")
    }

    #[must_use]
    pub fn log_path(&self, id: Uuid) -> PathBuf {
        self.logs_dir().join(format!("{id}.jsonl"))
    }

    #[must_use]
    pub fn job_path(&self, id: Uuid) -> PathBuf {
        self.paths
            .config
            .join("scheduler/jobs")
            .join(format!("{id}.json"))
    }

    #[must_use]
    pub fn run_path(&self, id: Uuid) -> PathBuf {
        self.paths
            .state
            .join("scheduler/runs")
            .join(format!("{id}.jsonl"))
    }

    fn lock_path(&self, id: Uuid) -> PathBuf {
        self.paths
            .state
            .join("scheduler/locks")
            .join(format!("{id}.json"))
    }

    #[must_use]
    pub fn label(&self, schedule: &Schedule) -> String {
        match schedule.backend {
            Backend::Launchd => format!("dev.haarness.schedule.{}", schedule.id),
            Backend::SystemdUser => format!("haarness-schedule-{}", schedule.id),
            Backend::WindowsTask => format!("Haarness\\Schedule\\{}", schedule.id),
        }
    }

    #[must_use]
    pub fn launchd_path(&self, schedule: &Schedule) -> PathBuf {
        self.paths
            .launchd
            .join(format!("{}.plist", self.label(schedule)))
    }

    #[must_use]
    pub fn systemd_service_path(&self, schedule: &Schedule) -> PathBuf {
        self.paths
            .systemd
            .join(format!("{}.service", self.label(schedule)))
    }

    #[must_use]
    pub fn systemd_timer_path(&self, schedule: &Schedule) -> PathBuf {
        self.paths
            .systemd
            .join(format!("{}.timer", self.label(schedule)))
    }

    #[must_use]
    pub fn windows_xml_path(&self, schedule: &Schedule) -> PathBuf {
        self.paths.windows.join(format!("{}.xml", schedule.id))
    }

    pub fn create(&self, input: CreateSchedule) -> Result<Schedule> {
        let parsed = input.request.as_deref().map(parse_request).transpose()?;
        let trigger = input
            .trigger
            .or_else(|| parsed.as_ref().map(|value| value.trigger.clone()))
            .context("a schedule needs a time to run")?;
        let prompt = input
            .prompt
            .or_else(|| parsed.as_ref().map(|value| value.prompt.clone()))
            .filter(|value| !value.trim().is_empty())
            .context("a schedule needs a prompt")?;
        validate_trigger(&trigger)?;
        validate_model(input.model.as_deref())?;
        let workspace = canonical_directory(&input.workspace)?;
        let directory = input
            .directory
            .as_deref()
            .map(canonical_directory)
            .transpose()?;
        if let Some(directory) = &directory {
            anyhow::ensure!(
                directory.starts_with(&workspace) && directory != &workspace,
                "schedule folder {} is not inside {}",
                directory.display(),
                workspace.display()
            );
        }
        let name = match input
            .name
            .or_else(|| parsed.and_then(|value| value.name))
            .filter(|value| !value.trim().is_empty())
        {
            Some(name) => name,
            None => self.unused_name(&title_from_prompt(&prompt))?,
        };
        let timestamp = Utc::now();
        let schedule = Schedule {
            id: Uuid::now_v7(),
            backend: self.backend,
            name,
            workspace,
            directory,
            prompt,
            model: clean(input.model),
            agent: clean(input.agent),
            variant: clean(input.variant),
            trust_project: input.trust_project,
            enabled: input.enabled,
            trigger,
            timezone: clean(input.timezone),
            created_at: timestamp,
            updated_at: timestamp,
        };
        self.ensure_unique_name(&schedule)?;
        self.persist(&schedule)?;
        if let Err(error) = self.install(&schedule) {
            // A unit the system refused to start is still written and
            // enabled; take it away with the schedule it belonged to.
            let _ = self.uninstall(&schedule);
            let _ = fs::remove_file(self.job_path(schedule.id));
            return Err(error);
        }
        Ok(schedule)
    }

    pub fn update(&self, reference: &str, input: UpdateSchedule) -> Result<Schedule> {
        let current = self.require(reference)?;
        let parsed = input.request.as_deref().map(parse_request).transpose()?;
        let workspace = input
            .workspace
            .as_deref()
            .map(canonical_directory)
            .transpose()?
            .unwrap_or_else(|| current.workspace.clone());
        let model = input.model.map(|value| value.and_then(clean_string));
        validate_model(model.as_ref().unwrap_or(&current.model).as_deref())?;
        let schedule = Schedule {
            name: input
                .name
                .or_else(|| parsed.as_ref().and_then(|value| value.name.clone()))
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| current.name.clone()),
            // A schedule moved to another workspace leaves its chat's folder.
            directory: if input.workspace.is_some() {
                None
            } else {
                current.directory.clone()
            },
            workspace,
            prompt: input
                .prompt
                .or_else(|| parsed.as_ref().map(|value| value.prompt.clone()))
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| current.prompt.clone()),
            model: model.unwrap_or_else(|| current.model.clone()),
            agent: input
                .agent
                .map(|value| value.and_then(clean_string))
                .unwrap_or_else(|| current.agent.clone()),
            variant: input
                .variant
                .map(|value| value.and_then(clean_string))
                .unwrap_or_else(|| current.variant.clone()),
            trust_project: input.trust_project.unwrap_or(current.trust_project),
            enabled: input.enabled.unwrap_or(current.enabled),
            trigger: input
                .trigger
                .or_else(|| parsed.map(|value| value.trigger))
                .unwrap_or_else(|| current.trigger.clone()),
            timezone: input
                .timezone
                .map(|value| value.and_then(clean_string))
                .unwrap_or_else(|| current.timezone.clone()),
            updated_at: Utc::now(),
            ..current.clone()
        };
        if schedule.enabled || schedule.trigger != current.trigger {
            validate_trigger(&schedule.trigger)?;
        }
        // Duplicates saved before names were unique stay editable.
        if schedule.name != current.name {
            self.ensure_unique_name(&schedule)?;
        }
        self.uninstall(&current)?;
        self.persist(&schedule)?;
        self.install(&schedule)?;
        Ok(schedule)
    }

    pub fn remove(&self, reference: &str) -> Result<Schedule> {
        let schedule = self.require(reference)?;
        self.uninstall(&schedule)?;
        remove_if_exists(&self.job_path(schedule.id))?;
        remove_if_exists(&self.lock_path(schedule.id))?;
        Ok(schedule)
    }

    pub fn toggle(&self, reference: &str, enabled: bool) -> Result<Schedule> {
        self.update(
            reference,
            UpdateSchedule {
                enabled: Some(enabled),
                ..UpdateSchedule::default()
            },
        )
    }

    pub fn list(&self) -> Result<Vec<Schedule>> {
        let root = self.paths.config.join("scheduler/jobs");
        let mut schedules = fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().extension() == Some(OsStr::new("json")))
            .filter_map(|entry| read_json::<Schedule>(&entry.path()).ok())
            .collect::<Vec<_>>();
        schedules.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(schedules)
    }

    pub fn find(&self, reference: &str) -> Result<Option<Schedule>> {
        let reference = reference.trim();
        Ok(self
            .list()?
            .into_iter()
            .find(|schedule| schedule.id.to_string() == reference || schedule.name == reference))
    }

    /// A name made from the prompt never blocks a schedule: the same prompt
    /// again, here or in another project, becomes "check deps 2".
    fn unused_name(&self, base: &str) -> Result<String> {
        let taken: Vec<String> = self.list()?.into_iter().map(|other| other.name).collect();
        // One of the first len + 1 names is free.
        Ok((1..=taken.len() + 1)
            .map(|count| {
                if count == 1 {
                    base.to_owned()
                } else {
                    format!("{base} {count}")
                }
            })
            .find(|name| !taken.contains(name))
            .expect("one of them is free"))
    }

    /// Names double as references (`haarness schedule run <name>`), so two
    /// schedules may not share one.
    fn ensure_unique_name(&self, schedule: &Schedule) -> Result<()> {
        if self
            .list()?
            .iter()
            .any(|other| other.name == schedule.name && other.id != schedule.id)
        {
            anyhow::bail!(
                "a schedule named \"{}\" already exists; choose another name",
                schedule.name
            );
        }
        Ok(())
    }

    pub fn require(&self, reference: &str) -> Result<Schedule> {
        self.find(reference)?
            .with_context(|| format!("schedule not found: {reference}"))
    }

    pub fn history(&self, reference: &str, limit: usize) -> Result<Vec<ScheduleRun>> {
        let schedule = self.require(reference)?;
        let text = match fs::read_to_string(self.run_path(schedule.id)) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut runs = text
            .lines()
            .rev()
            .take(limit)
            .map(serde_json::from_str)
            .collect::<std::result::Result<Vec<ScheduleRun>, _>>()?;
        runs.shrink_to_fit();
        Ok(runs)
    }

    pub fn record(&self, schedule: &Schedule, run: &ScheduleRun) -> Result<()> {
        append_json_line(&self.run_path(schedule.id), run)?;
        append_json_line(&self.log_path(schedule.id), run)
    }

    /// Save the result before unloading a native one-shot job: launchd's
    /// bootout can terminate the runner that is calling it.
    pub fn finish(
        &self,
        schedule: &Schedule,
        mut run: ScheduleRun,
        guard: RunGuard,
    ) -> Result<ScheduleRun> {
        let completed = (|| -> Result<Option<Schedule>> {
            if !matches!(schedule.trigger, Trigger::Once { .. }) {
                return Ok(None);
            }
            let Some(mut current) = self.find(&schedule.id.to_string())? else {
                return Ok(None);
            };
            // A user may have rescheduled this job while its prompt was running.
            if current.trigger != schedule.trigger {
                return Ok(None);
            }
            current.enabled = false;
            current.updated_at = Utc::now();
            self.persist(&current)?;
            Ok(Some(current))
        })();
        if let Err(error) = &completed {
            run.status = RunStatus::Failed;
            run.error = Some(format!(
                "failed to disable completed one-shot schedule: {error:#}"
            ));
        }
        self.record(schedule, &run)?;
        // Release overlap protection before bootout can terminate this process.
        drop(guard);
        if let Ok(Some(current)) = completed {
            self.uninstall(&current)?;
        }
        Ok(run)
    }

    pub fn acquire(&self, schedule: &Schedule) -> Result<Option<RunGuard>> {
        let path = self.lock_path(schedule.id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        for _ in 0..3 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let contents = serde_json::to_vec(
                        &serde_json::json!({"pid": std::process::id(), "createdAt": Utc::now()}),
                    )?;
                    file.write_all(&contents)?;
                    file.flush()?;
                    return Ok(Some(RunGuard { path, contents }));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let Some(stale) = abandoned_lock(&path) else {
                        return Ok(None);
                    };
                    // Remove only the lock judged abandoned, not one another
                    // runner wrote since.
                    if fs::read(&path).is_ok_and(|contents| contents == stale) {
                        remove_if_exists(&path)?;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(None)
    }

    pub fn run_now(&self, reference: &str) -> Result<ScheduleRun> {
        let schedule = self.require(reference)?;
        let before = Utc::now();
        let mut command = self.runner_command(&schedule, RunSource::Manual);
        // The trigger is an internal child process. Its machine-readable JSON
        // belongs in native-scheduler logs, while the caller owns the public
        // `schedule run` representation. Capture it here so an interactive run
        // does not print the same result twice.
        let output = command
            .output()
            .with_context(|| format!("failed to launch schedule {}", schedule.name))?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let missing_run = if stderr.trim().is_empty() {
            "schedule runner did not record a result".to_string()
        } else {
            format!("schedule runner did not record a result: {}", stderr.trim())
        };
        let run = self
            .history(&schedule.id.to_string(), 1)?
            .into_iter()
            .next()
            .filter(|run| run.started_at >= before)
            .with_context(|| missing_run)?;
        if !output.status.success() && run.status != RunStatus::Failed {
            anyhow::bail!("schedule runner exited with {}", output.status);
        }
        Ok(run)
    }

    /// Start a manual run without blocking an interactive configuration request.
    /// The canonical runner owns execution, locking, and durable run history.
    pub fn start_now(&self, reference: &str) -> Result<u32> {
        let schedule = self.require(reference)?;
        let mut child = self
            .runner_command(&schedule, RunSource::Manual)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("Could not start schedule runner")?;
        let pid = child.id();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(pid)
    }

    #[must_use]
    pub fn summary(schedule: &Schedule) -> String {
        format!(
            "{} | {} | {} | model: {} | {}",
            schedule.name,
            describe_trigger(&schedule.trigger),
            if schedule.enabled {
                "enabled"
            } else {
                "disabled"
            },
            schedule.model.as_deref().unwrap_or("default"),
            schedule.workspace.display()
        )
    }

    pub fn render_launchd(&self, schedule: &Schedule) -> Result<String> {
        let args = self.runner_args(schedule, RunSource::Launchd);
        let trigger = match &schedule.trigger {
            Trigger::Interval { seconds } => {
                format!("<key>StartInterval</key>\n  <integer>{seconds}</integer>")
            }
            Trigger::Once { at } => {
                let local = at.with_timezone(&Local);
                format!(
                    "<key>StartCalendarInterval</key>\n  <dict>\n    <key>Month</key><integer>{}</integer>\n    <key>Day</key><integer>{}</integer>\n    <key>Hour</key><integer>{}</integer>\n    <key>Minute</key><integer>{}</integer>\n  </dict>",
                    local.month(),
                    local.day(),
                    local.hour(),
                    local.minute()
                )
            }
            Trigger::Cron { expression } => format!(
                "<key>StartCalendarInterval</key>\n  <array>\n{}\n  </array>",
                expand_cron(expression)?
                    .into_iter()
                    .map(|entry| {
                        let body = entry
                            .into_iter()
                            .map(|(key, value)| {
                                format!("      <key>{key}</key><integer>{value}</integer>")
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        format!("    <dict>\n{body}\n    </dict>")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        };
        let mut environment = BTreeMap::new();
        for key in ["HOME", "PATH", "SHELL", "HAARNESS_TEST_PROVIDER"] {
            if let Some(value) = std::env::var_os(key).and_then(|value| value.into_string().ok()) {
                environment.insert(key, value);
            }
        }
        environment.extend(
            self.environment
                .iter()
                .map(|(key, value)| (*key, value.clone())),
        );
        let env = environment
            .into_iter()
            .map(|(key, value)| {
                format!(
                    "    <key>{}</key><string>{}</string>",
                    xml_escape(key),
                    xml_escape(value)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{}</string>\n  <key>ProgramArguments</key>\n  <array>\n{}\n  </array>\n  <key>WorkingDirectory</key>\n  <string>{}</string>\n  <key>EnvironmentVariables</key>\n  <dict>\n{}\n  </dict>\n  <key>RunAtLoad</key>\n  <false/>\n  <key>StandardOutPath</key>\n  <string>{}</string>\n  <key>StandardErrorPath</key>\n  <string>{}</string>\n  {}\n</dict>\n</plist>\n",
            xml_escape(self.label(schedule)),
            args.iter()
                .map(|value| format!(
                    "    <string>{}</string>",
                    xml_escape(value.to_string_lossy())
                ))
                .collect::<Vec<_>>()
                .join("\n"),
            xml_escape(schedule.workspace.display()),
            env,
            xml_escape(self.log_path(schedule.id).display()),
            xml_escape(self.log_path(schedule.id).display()),
            trigger
        ))
    }

    pub fn render_systemd(&self, schedule: &Schedule) -> Result<(String, String)> {
        let args = self.runner_args(schedule, RunSource::SystemdUser);
        let timer_body = match &schedule.trigger {
            // OnUnitActiveSec= alone never fires: it counts from the
            // service's last activation, and a new timer has none.
            // OnActiveSec= schedules the first run from the timer's start.
            Trigger::Interval { seconds } => {
                format!("OnActiveSec={seconds}s\nOnUnitActiveSec={seconds}s\nAccuracySec=1s")
            }
            Trigger::Once { at } => format!("OnCalendar={}", at.format("%Y-%m-%d %H:%M:%S UTC")),
            Trigger::Cron { expression } => expand_cron(expression)?
                .into_iter()
                .map(|entry| {
                    let month = entry.get("Month").map_or("*".into(), u32::to_string);
                    let day = entry.get("Day").map_or("*".into(), u32::to_string);
                    let hour = entry.get("Hour").map_or("*".into(), u32::to_string);
                    let minute = entry
                        .get("Minute")
                        .map_or("*".into(), |value| format!("{value:02}"));
                    let weekday = entry
                        .get("Weekday")
                        .map(|value| format!("{} ", weekday_name(*value)))
                        .unwrap_or_default();
                    let timezone = schedule
                        .timezone
                        .as_ref()
                        .map(|value| format!(" {value}"))
                        .unwrap_or_default();
                    format!("OnCalendar={weekday}*-{month}-{day} {hour}:{minute}:00{timezone}")
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let mut environment = String::new();
        for (key, value) in &self.environment {
            let _ = writeln!(
                environment,
                "Environment={}",
                systemd_argument(OsStr::new(&format!("{key}={value}")))
            );
        }
        let service = format!(
            "[Unit]\nDescription=Haarness scheduled run {}\n\n[Service]\nType=oneshot\n{environment}WorkingDirectory={}\nExecStart={}\nStandardOutput=append:{}\nStandardError=append:{}\n",
            systemd_escape(&schedule.name),
            systemd_path(&schedule.workspace),
            args.iter()
                .map(|value| systemd_argument(value))
                .collect::<Vec<_>>()
                .join(" "),
            systemd_path(&self.log_path(schedule.id)),
            systemd_path(&self.log_path(schedule.id))
        );
        let timer = format!(
            "[Unit]\nDescription=Haarness schedule timer {}\n\n[Timer]\n{}\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n",
            systemd_escape(&schedule.name),
            timer_body
        );
        Ok((service, timer))
    }

    pub fn render_windows(&self, schedule: &Schedule) -> Result<String> {
        let triggers = match &schedule.trigger {
            Trigger::Interval { seconds } => format!(
                "    <TimeTrigger>\n      <StartBoundary>{}</StartBoundary>\n      <Enabled>true</Enabled>\n      <Repetition><Interval>PT{seconds}S</Interval><StopAtDurationEnd>false</StopAtDurationEnd></Repetition>\n    </TimeTrigger>",
                (Local::now() + chrono::Duration::minutes(1))
                    .to_rfc3339_opts(SecondsFormat::Secs, false)
            ),
            Trigger::Once { at } => format!(
                "    <TimeTrigger><StartBoundary>{}</StartBoundary><Enabled>true</Enabled></TimeTrigger>",
                at.with_timezone(&Local)
                    .to_rfc3339_opts(SecondsFormat::Secs, false)
            ),
            Trigger::Cron { expression } => windows_cron_triggers(expression)?,
        };
        let args = self.runner_args(schedule, RunSource::WindowsTask);
        let mut command = args
            .first()
            .context("schedule runner command is empty")?
            .to_string_lossy()
            .into_owned();
        let mut arguments = args
            .iter()
            .skip(1)
            .map(|value| windows_argument(value))
            .collect::<Vec<_>>()
            .join(" ");
        if !self.environment.is_empty() {
            // A task has no environment of its own: cmd sets it, then starts
            // the runner. `/s` runs the quoted line exactly as written.
            let mut assignments = String::new();
            for (key, value) in &self.environment {
                let _ = write!(assignments, "set \"{key}={value}\" && ");
            }
            arguments = format!(
                "/d /s /c \"{assignments}{} {arguments}\"",
                windows_argument(OsStr::new(&command))
            );
            command = "cmd.exe".into();
        }
        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Task version=\"1.4\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n  <RegistrationInfo><Description>Haarness scheduled run {}</Description></RegistrationInfo>\n  <Triggers>\n{}\n  </Triggers>\n  <Principals><Principal id=\"Author\"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\n  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><Enabled>true</Enabled></Settings>\n  <Actions Context=\"Author\"><Exec><Command>{}</Command><Arguments>{}</Arguments><WorkingDirectory>{}</WorkingDirectory></Exec></Actions>\n</Task>\n",
            xml_escape(&schedule.name),
            triggers,
            xml_escape(command),
            xml_escape(arguments),
            xml_escape(schedule.workspace.display())
        ))
    }

    fn persist(&self, schedule: &Schedule) -> Result<()> {
        write_private_json(&self.job_path(schedule.id), schedule)
    }

    fn install(&self, schedule: &Schedule) -> Result<()> {
        if !schedule.enabled {
            return self.uninstall(schedule);
        }
        fs::create_dir_all(self.logs_dir())?;
        match schedule.backend {
            Backend::Launchd => self.install_launchd(schedule),
            Backend::SystemdUser => self.install_systemd(schedule),
            Backend::WindowsTask => self.install_windows(schedule),
        }
    }

    fn uninstall(&self, schedule: &Schedule) -> Result<()> {
        match schedule.backend {
            Backend::Launchd => self.uninstall_launchd(schedule),
            Backend::SystemdUser => self.uninstall_systemd(schedule),
            Backend::WindowsTask => self.uninstall_windows(schedule),
        }
    }

    fn install_launchd(&self, schedule: &Schedule) -> Result<()> {
        let path = self.launchd_path(schedule);
        write_text(&path, &self.render_launchd(schedule)?)?;
        if !self.install {
            return Ok(());
        }
        let domain = launchd_domain()?;
        let _ = Command::new("/bin/launchctl")
            .args(["bootout", &domain])
            .arg(&path)
            .output();
        successful(
            Command::new("/bin/launchctl")
                .args(["bootstrap", &domain])
                .arg(&path),
            "launchctl bootstrap",
        )
    }

    fn uninstall_launchd(&self, schedule: &Schedule) -> Result<()> {
        let domain = self.install.then(launchd_domain).transpose()?;
        // Unlink first: bootout may terminate this very runner. Address the
        // loaded service by label so removal does not depend on the plist.
        remove_if_exists(&self.launchd_path(schedule))?;
        if let Some(domain) = domain {
            let _ = Command::new("/bin/launchctl")
                .args(["bootout", &format!("{domain}/{}", self.label(schedule))])
                .output();
        }
        Ok(())
    }

    fn install_systemd(&self, schedule: &Schedule) -> Result<()> {
        let (service, timer) = self.render_systemd(schedule)?;
        write_text(&self.systemd_service_path(schedule), &service)?;
        write_text(&self.systemd_timer_path(schedule), &timer)?;
        if !self.install {
            return Ok(());
        }
        successful(
            Command::new("systemctl").args(["--user", "daemon-reload"]),
            "systemctl --user daemon-reload",
        )?;
        successful(
            Command::new("systemctl")
                .args(["--user", "enable", "--now"])
                .arg(
                    self.systemd_timer_path(schedule)
                        .file_name()
                        .context("invalid systemd timer path")?,
                ),
            "systemctl --user enable --now",
        )
    }

    fn uninstall_systemd(&self, schedule: &Schedule) -> Result<()> {
        let timer = self.systemd_timer_path(schedule);
        if self.install && timer.exists() {
            let _ = Command::new("systemctl")
                .args(["--user", "disable", "--now"])
                .arg(timer.file_name().context("invalid systemd timer path")?)
                .output();
        }
        remove_if_exists(&self.systemd_service_path(schedule))?;
        remove_if_exists(&timer)?;
        if self.install {
            let _ = Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .output();
        }
        Ok(())
    }

    fn install_windows(&self, schedule: &Schedule) -> Result<()> {
        let path = self.windows_xml_path(schedule);
        write_text(&path, &self.render_windows(schedule)?)?;
        if !self.install {
            return Ok(());
        }
        successful(
            Command::new("schtasks.exe")
                .args(["/Create", "/TN"])
                .arg(self.label(schedule))
                .args(["/XML"])
                .arg(&path)
                .arg("/F"),
            "schtasks /Create",
        )
    }

    fn uninstall_windows(&self, schedule: &Schedule) -> Result<()> {
        if self.install {
            // output(), not status(): schtasks prints SUCCESS/ERROR lines that
            // would land in the terminal UI.
            let _ = Command::new("schtasks.exe")
                .args(["/Delete", "/TN"])
                .arg(self.label(schedule))
                .arg("/F")
                .output();
        }
        remove_if_exists(&self.windows_xml_path(schedule))
    }

    fn runner_args(&self, schedule: &Schedule, source: RunSource) -> Vec<OsString> {
        let mut args = vec![self.executable.clone().into_os_string()];
        if let Some(root) = &self.data_root {
            args.push("--data-dir".into());
            args.push(root.as_os_str().to_owned());
        }
        args.push("--workspace".into());
        args.push(schedule.workspace.as_os_str().to_owned());
        if !schedule.trust_project {
            args.push("--disable-project-config".into());
        }
        args.extend([
            OsString::from("schedule"),
            OsString::from("trigger"),
            schedule.id.to_string().into(),
            OsString::from("--source"),
            source.to_string().into(),
        ]);
        args
    }

    fn runner_command(&self, schedule: &Schedule, source: RunSource) -> Command {
        let args = self.runner_args(schedule, source);
        let mut command = Command::new(&args[0]);
        command.args(&args[1..]).current_dir(&schedule.workspace);
        command
    }
}

pub struct RunGuard {
    path: PathBuf,
    /// The lock as this run wrote it.
    contents: Vec<u8>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        // Another runner may have taken over a lock it judged abandoned; that
        // lock is its own, not this run's to remove.
        if fs::read(&self.path).is_ok_and(|contents| contents == self.contents) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// How old a run lock must be before it counts as abandoned when nothing can
/// tell whether its run is still going: it names no readable process, or
/// this system cannot check the process it names.
const UNVERIFIED_LOCK_AGE: Duration = Duration::from_secs(60 * 60);

/// A process that started this long after a lock was written did not write it.
#[cfg(unix)]
const PID_REUSE_MARGIN: chrono::TimeDelta = chrono::TimeDelta::seconds(60);

/// What a run lock records about the run holding it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockHolder {
    pid: u32,
    // Only Unix can tell when a process started (Windows locks stay Unknown).
    #[cfg_attr(not(unix), allow(dead_code))]
    created_at: DateTime<Utc>,
}

/// Whether the process a lock names still runs the run that wrote it.
enum Holder {
    Running,
    #[cfg_attr(not(unix), allow(dead_code))]
    Gone,
    Unknown,
}

/// The lock at `path` as read, when it no longer belongs to a running run:
/// its process has exited, or its pid now names a process that started after
/// the lock was written (the pid was reused, as after a restart). A run that
/// is still going keeps its lock however long it takes.
fn abandoned_lock(path: &Path) -> Option<Vec<u8>> {
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        // The run finished meanwhile: nothing is left to wait for.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
    };
    let holder = serde_json::from_slice::<LockHolder>(&contents).ok();
    let abandoned = match holder.as_ref().map_or(Holder::Unknown, lock_holder) {
        Holder::Running => false,
        Holder::Gone => true,
        // An empty lock is one being written, or one its runner never finished.
        Holder::Unknown => fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > UNVERIFIED_LOCK_AGE),
    };
    abandoned.then_some(contents)
}

fn lock_holder(holder: &LockHolder) -> Holder {
    // A process does not wait for itself; at worst its pid is a crashed
    // run's reused, and the next run, under another pid, clears that lock.
    if holder.pid == std::process::id() {
        return Holder::Running;
    }
    #[cfg(unix)]
    {
        use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
        let Some(pid) = i32::try_from(holder.pid).ok().filter(|pid| *pid > 0) else {
            return Holder::Unknown;
        };
        if kill(Pid::from_raw(pid), None) == Err(Errno::ESRCH) {
            return Holder::Gone;
        }
        match process_started(holder.pid) {
            Some(started) if started > holder.created_at + PID_REUSE_MARGIN => Holder::Gone,
            Some(_) => Holder::Running,
            None => Holder::Unknown,
        }
    }
    #[cfg(not(unix))]
    {
        Holder::Unknown
    }
}

/// When process `pid` started, from the elapsed time `ps` reports.
#[cfg(unix)]
fn process_started(pid: u32) -> Option<DateTime<Utc>> {
    let output = Command::new("ps")
        .args(["-o", "etime=", "-p", &pid.to_string()])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let elapsed = elapsed_seconds(String::from_utf8_lossy(&output.stdout).trim())?;
    Some(Utc::now() - chrono::TimeDelta::seconds(elapsed))
}

/// `ps`'s POSIX elapsed time, `[[dd-]hh:]mm:ss`, in seconds.
#[cfg_attr(not(unix), allow(dead_code))]
fn elapsed_seconds(text: &str) -> Option<i64> {
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<i64>().ok()?, clock),
        None => (0, text),
    };
    let parts = clock.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut seconds = 0;
    for part in parts {
        seconds = seconds * 60 + part.parse::<i64>().ok()?;
    }
    Some(days * 86_400 + seconds)
}

#[derive(Clone, Debug)]
struct ParsedRequest {
    name: Option<String>,
    prompt: String,
    trigger: Trigger,
}

/// `text` without a leading `keyword`, matched ignoring ASCII case so
/// "Every day at 8am" reads like "every day at 8am". The rest keeps its case.
fn strip_keyword<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    text.get(..keyword.len())
        .filter(|head| head.eq_ignore_ascii_case(keyword))
        .map(|_| &text[keyword.len()..])
}

/// The byte offset of the lowercase `needle` in `text`, ignoring ASCII case.
/// ASCII lowercasing keeps every byte offset, so the offset fits `text`.
fn find_keyword(text: &str, needle: &str) -> Option<usize> {
    text.to_ascii_lowercase().find(needle)
}

fn parse_request(request: &str) -> Result<ParsedRequest> {
    let mut text = request.trim().to_owned();
    let mut name = None;
    if let Some(start) = find_keyword(&text, "named \"") {
        let value = &text[start + 7..];
        if let Some(end) = value.find('"') {
            name = clean(Some(value[..end].to_owned()));
            text.replace_range(start..start + 8 + end, "");
        }
    }
    for prefix in [
        "please schedule ",
        "schedule a job ",
        "schedule an job ",
        "schedule ",
    ] {
        if let Some(rest) = strip_keyword(&text, prefix) {
            text = rest.trim().to_owned();
            break;
        }
    }
    if let Some(rest) = strip_keyword(&text, "in ") {
        let (amount, unit, prompt) = interval_parts(rest, false)?;
        let seconds = interval_seconds(amount, unit)?;
        let at = Utc::now()
            + chrono::Duration::seconds(i64::try_from(seconds.max(MIN_INTERVAL_SECONDS))?)
            + chrono::Duration::seconds(5);
        return Ok(ParsedRequest {
            name,
            prompt,
            trigger: Trigger::Once { at },
        });
    }
    if let Some(rest) = strip_keyword(&text, "every ") {
        if let Some(rest) = strip_keyword(rest, "day at ") {
            let (clock, prompt) = split_prompt(rest)?;
            let (hour, minute) = parse_clock(clock)?;
            return Ok(ParsedRequest {
                name,
                prompt,
                trigger: Trigger::Cron {
                    expression: format!("{minute} {hour} * * *"),
                },
            });
        }
        for (index, weekday) in [
            "sunday",
            "monday",
            "tuesday",
            "wednesday",
            "thursday",
            "friday",
            "saturday",
        ]
        .iter()
        .enumerate()
        {
            if let Some(rest) = strip_keyword(rest, &format!("{weekday} at ")) {
                let (clock, prompt) = split_prompt(rest)?;
                let (hour, minute) = parse_clock(clock)?;
                return Ok(ParsedRequest {
                    name,
                    prompt,
                    trigger: Trigger::Cron {
                        expression: format!("{minute} {hour} * * {index}"),
                    },
                });
            }
        }
        let (amount, unit, prompt) = interval_parts(rest, true)?;
        return Ok(ParsedRequest {
            name,
            prompt,
            trigger: Trigger::Interval {
                seconds: interval_seconds(amount, unit)?,
            },
        });
    }
    if let Some(rest) = strip_keyword(&text, "on ") {
        let (date, prompt) = split_prompt(rest)?;
        let at = DateTime::parse_from_rfc3339(date)
            .map(|value| value.with_timezone(&Utc))
            .or_else(|_| {
                chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").map(|value| {
                    value
                        .and_hms_opt(0, 0, 0)
                        .expect("midnight is valid")
                        .and_local_timezone(Local)
                        .single()
                        .expect("local midnight is unambiguous")
                        .with_timezone(&Utc)
                })
            })
            .with_context(|| format!("invalid schedule date: {date}"))?;
        return Ok(ParsedRequest {
            name,
            prompt,
            trigger: Trigger::Once { at },
        });
    }
    anyhow::bail!(
        "could not parse schedule request; use 'in 10 minutes to ...', 'every 5 minutes to ...', 'every day at 8am to ...', 'every Monday at 8am to ...', or 'on <RFC3339> to ...'"
    )
}

fn interval_parts(rest: &str, require_to: bool) -> Result<(u64, &str, String)> {
    let mut words = rest.splitn(3, char::is_whitespace);
    let amount = words
        .next()
        .context("schedule interval amount is required")?
        .parse::<u64>()
        .context("schedule interval must start with an integer")?;
    let unit = words.next().context("schedule interval unit is required")?;
    let suffix = words.next().context("a schedule needs a prompt")?.trim();
    let prompt = strip_keyword(suffix, "to ")
        .or_else(|| (!require_to).then_some(suffix))
        .context("recurring schedule prompt must follow 'to'")?
        .trim_matches(|character: char| character == ',' || character.is_whitespace())
        .to_owned();
    if prompt.is_empty() {
        anyhow::bail!("a schedule needs a prompt");
    }
    Ok((amount, unit, prompt))
}

fn interval_seconds(amount: u64, unit: &str) -> Result<u64> {
    let factor = match unit.to_lowercase().as_str() {
        "second" | "seconds" | "s" => 1,
        "minute" | "minutes" | "m" => 60,
        "hour" | "hours" | "h" => 60 * 60,
        "day" | "days" | "d" => 60 * 60 * 24,
        _ => anyhow::bail!("invalid schedule interval unit: {unit}"),
    };
    amount
        .checked_mul(factor)
        .context("schedule interval is too large")
}

fn split_prompt(value: &str) -> Result<(&str, String)> {
    let split = find_keyword(value, " to ").context("a schedule needs a prompt after 'to'")?;
    let (left, right) = (&value[..split], &value[split + 4..]);
    if right.trim().is_empty() {
        anyhow::bail!("a schedule needs a prompt");
    }
    Ok((left.trim(), right.trim().to_owned()))
}

fn parse_clock(value: &str) -> Result<(u32, u32)> {
    let lower = value.trim().to_lowercase();
    let (clock, meridiem) = if let Some(clock) = lower.strip_suffix("am") {
        (clock.trim(), Some("am"))
    } else if let Some(clock) = lower.strip_suffix("pm") {
        (clock.trim(), Some("pm"))
    } else {
        (lower.as_str(), None)
    };
    let (hour, minute) = clock.split_once(':').unwrap_or((clock, "0"));
    let mut hour = hour
        .parse::<u32>()
        .with_context(|| format!("invalid schedule time: {value}"))?;
    let minute = minute
        .parse::<u32>()
        .with_context(|| format!("invalid schedule time: {value}"))?;
    if minute > 59 {
        anyhow::bail!("invalid schedule time: {value}");
    }
    if let Some(meridiem) = meridiem {
        if !(1..=12).contains(&hour) {
            anyhow::bail!("invalid schedule time: {value}");
        }
        if hour == 12 {
            hour = 0;
        }
        if meridiem == "pm" {
            hour += 12;
        }
    } else if hour > 23 {
        anyhow::bail!("invalid schedule time: {value}");
    }
    Ok((hour, minute))
}

fn validate_trigger(trigger: &Trigger) -> Result<()> {
    match trigger {
        Trigger::Interval { seconds } if *seconds < MIN_INTERVAL_SECONDS => {
            anyhow::bail!("a schedule can repeat at most once a minute")
        }
        Trigger::Cron { expression } => {
            parse_cron(expression)?;
        }
        Trigger::Once { at } if *at < Utc::now() + chrono::Duration::minutes(1) => {
            anyhow::bail!("a one-time schedule must start at least a minute from now")
        }
        _ => {}
    }
    Ok(())
}

fn validate_model(model: Option<&str>) -> Result<()> {
    if model.is_some_and(|value| !value.trim().is_empty() && !value.contains('/')) {
        anyhow::bail!("schedule model must use provider/model form")
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct CronField {
    wildcard: bool,
    values: Vec<u32>,
}

#[derive(Clone, Debug)]
struct Cron {
    minute: CronField,
    hour: CronField,
    day: CronField,
    month: CronField,
    weekday: CronField,
}

fn parse_cron(expression: &str) -> Result<Cron> {
    let fields = expression.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 {
        anyhow::bail!("cron expression must contain exactly 5 fields")
    }
    Ok(Cron {
        minute: parse_cron_field(fields[0], 0, 59)?,
        hour: parse_cron_field(fields[1], 0, 23)?,
        day: parse_cron_field(fields[2], 1, 31)?,
        month: parse_cron_field(fields[3], 1, 12)?,
        weekday: parse_cron_field(fields[4], 0, 7)?,
    })
}

fn parse_cron_field(field: &str, min: u32, max: u32) -> Result<CronField> {
    if field == "*" {
        return Ok(CronField {
            wildcard: true,
            values: Vec::new(),
        });
    }
    let mut values = Vec::new();
    let number = |text: &str| {
        text.parse::<u32>().map_err(|_| {
            anyhow::anyhow!(
                "cron field \"{field}\": use numbers from {min} to {max}; names such as MON or JAN are not supported"
            )
        })
    };
    for chunk in field.split(',') {
        let (base, step) = chunk.split_once('/').map_or((chunk, 1), |(base, step)| {
            (base, step.parse::<u32>().unwrap_or(0))
        });
        if step == 0 {
            anyhow::bail!("invalid cron step: {chunk}")
        }
        let (start, end) = if base == "*" {
            (min, max)
        } else if let Some((start, end)) = base.split_once('-') {
            (number(start)?, number(end)?)
        } else {
            let value = number(base)?;
            (value, value)
        };
        if start < min || end > max || start > end {
            anyhow::bail!("invalid cron range: {chunk}")
        }
        values.extend((start..=end).step_by(usize::try_from(step)?));
    }
    values.sort_unstable();
    values.dedup();
    if values.is_empty() {
        anyhow::bail!("invalid cron field: {field}")
    }
    Ok(CronField {
        wildcard: false,
        values,
    })
}

fn expand_cron(expression: &str) -> Result<Vec<BTreeMap<&'static str, u32>>> {
    let cron = parse_cron(expression)?;
    let minutes = expanded_values(&cron.minute);
    let hours = expanded_values(&cron.hour);
    let days = expanded_values(&cron.day);
    let months = expanded_values(&cron.month);
    let weekdays = expanded_values(&cron.weekday)
        .into_iter()
        .map(|value| value.map(|value| if value == 7 { 0 } else { value }))
        .collect::<Vec<_>>();
    let mut result = Vec::new();
    for minute in &minutes {
        for hour in &hours {
            for day in &days {
                for month in &months {
                    for weekday in &weekdays {
                        let mut entry = BTreeMap::new();
                        for (key, value) in [
                            ("Minute", minute),
                            ("Hour", hour),
                            ("Day", day),
                            ("Month", month),
                            ("Weekday", weekday),
                        ] {
                            if let Some(value) = value {
                                entry.insert(key, *value);
                            }
                        }
                        result.push(entry);
                        if result.len() > MAX_EXPANSIONS {
                            anyhow::bail!("cron expression is too complex for native conversion")
                        }
                    }
                }
            }
        }
    }
    Ok(result)
}

fn expanded_values(field: &CronField) -> Vec<Option<u32>> {
    if field.wildcard {
        vec![None]
    } else {
        field.values.iter().copied().map(Some).collect()
    }
}

fn windows_cron_triggers(expression: &str) -> Result<String> {
    let cron = parse_cron(expression)?;
    if !cron.day.wildcard || !cron.month.wildcard {
        anyhow::bail!(
            "Windows Task Scheduler cron support is limited to daily and weekly schedules"
        )
    }
    let minutes = if cron.minute.wildcard {
        (0..60).collect()
    } else {
        cron.minute.values.clone()
    };
    let hours = if cron.hour.wildcard {
        (0..24).collect()
    } else {
        cron.hour.values.clone()
    };
    let days = if cron.weekday.wildcard {
        None
    } else {
        Some(
            cron.weekday
                .values
                .iter()
                .map(|value| weekday_xml(if *value == 7 { 0 } else { *value }))
                .collect::<Vec<_>>()
                .join(""),
        )
    };
    let date = Local::now().date_naive();
    let mut triggers = Vec::new();
    for hour in hours {
        for minute in &minutes {
            let boundary = format!("{}T{hour:02}:{minute:02}:00", date.format("%Y-%m-%d"));
            let schedule = days.as_ref().map_or_else(
                || "<ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay>".into(),
                |days| {
                    format!("<ScheduleByWeek><WeeksInterval>1</WeeksInterval><DaysOfWeek>{days}</DaysOfWeek></ScheduleByWeek>")
                },
            );
            triggers.push(format!(
                "    <CalendarTrigger><StartBoundary>{boundary}</StartBoundary><Enabled>true</Enabled>{schedule}</CalendarTrigger>"
            ));
            if triggers.len() > MAX_EXPANSIONS {
                anyhow::bail!("cron expression is too complex for Windows Task Scheduler")
            }
        }
    }
    Ok(triggers.join("\n"))
}

fn title_from_prompt(prompt: &str) -> String {
    let text = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= 48 {
        return text;
    }
    format!("{}…", text.chars().take(48).collect::<String>())
}

fn clean(value: Option<String>) -> Option<String> {
    value.and_then(clean_string)
}

fn clean_string(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    // The plain form on Windows: Task Scheduler and cmd.exe do not start in a
    // \\?\ folder.
    let path = dunce::canonicalize(path)
        .with_context(|| format!("schedule workspace does not exist: {}", path.display()))?;
    if !path.is_dir() {
        anyhow::bail!("schedule workspace is not a directory: {}", path.display())
    }
    Ok(path)
}

/// Where Haarness keeps its files, as this process sees it. A scheduled run is
/// started by launchd, systemd or Task Scheduler with none of the creating
/// shell's environment, so it gets these to use the same install: the same
/// schedules, chats, credentials and Cloud sign-in.
fn storage_environment() -> Vec<(&'static str, String)> {
    [
        "HAARNESS_HOME",
        "HAARNESS_DATA_DIR",
        "HAARNESS_CLOUD_CONFIG_DIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
    ]
    .into_iter()
    .filter_map(|key| {
        let value = std::env::var_os(key)?.into_string().ok()?;
        (!value.is_empty()).then_some((key, value))
    })
    .collect()
}

fn describe_trigger(trigger: &Trigger) -> String {
    let every = |count: u64, unit: &str| {
        if count == 1 {
            format!("Every {unit}")
        } else {
            format!("Every {count} {unit}s")
        }
    };
    match trigger {
        Trigger::Interval { seconds } if seconds % 86_400 == 0 => every(seconds / 86_400, "day"),
        Trigger::Interval { seconds } if seconds % 3_600 == 0 => every(seconds / 3_600, "hour"),
        Trigger::Interval { seconds } if seconds % 60 == 0 => every(seconds / 60, "minute"),
        Trigger::Interval { seconds } => every(*seconds, "second"),
        Trigger::Cron { expression } => format!("Cron {expression}"),
        Trigger::Once { at } => format!(
            "Once at {}",
            at.with_timezone(&Local).format("%Y-%m-%d %H:%M")
        ),
    }
}

fn weekday_name(value: u32) -> &'static str {
    ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
        .get(value as usize)
        .copied()
        .unwrap_or("Sun")
}

fn weekday_xml(value: u32) -> &'static str {
    [
        "<Sunday/>",
        "<Monday/>",
        "<Tuesday/>",
        "<Wednesday/>",
        "<Thursday/>",
        "<Friday/>",
        "<Saturday/>",
    ]
    .get(value as usize)
    .copied()
    .unwrap_or("<Sunday/>")
}

fn launchd_domain() -> Result<String> {
    let output = Command::new("id").arg("-u").output()?;
    if !output.status.success() {
        anyhow::bail!("id -u failed with {}", output.status)
    }
    Ok(format!("gui/{}", String::from_utf8(output.stdout)?.trim()))
}

fn successful(command: &mut Command, label: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("failed to execute {label}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!("{label} failed ({}): {}", output.status, stderr.trim())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut body = serde_json::to_vec_pretty(value)?;
    body.push(b'\n');
    write_private(path, &body)
}

fn append_json_line<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

fn write_text(path: &Path, value: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, value)?;
    Ok(())
}

fn write_private(path: &Path, body: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", Uuid::now_v7()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(body)?;
    file.flush()?;
    drop(file);
    if let Err(error) = fs::rename(&temporary, path) {
        if !path.exists() {
            return Err(error.into());
        }
        fs::remove_file(path)?;
        fs::rename(&temporary, path)?;
    }
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn xml_escape(value: impl std::fmt::Display) -> String {
    value
        .to_string()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn systemd_escape(value: impl std::fmt::Display) -> String {
    value.to_string().replace('%', "%%").replace('\n', " ")
}

fn systemd_argument(value: &OsStr) -> String {
    format!(
        "\"{}\"",
        value
            .to_string_lossy()
            .replace('%', "%%")
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}

/// A path setting (WorkingDirectory=, StandardOutput=append:) takes the path
/// as written: systemd neither strips quotes nor decodes escapes there, so a
/// quoted path is "not absolute" and the unit never loads. Only specifiers and
/// line breaks need care.
fn systemd_path(value: &Path) -> String {
    value
        .to_string_lossy()
        .replace('%', "%%")
        .replace(['\n', '\r'], "")
}

fn windows_argument(value: &OsStr) -> String {
    let value = value.to_string_lossy();
    if !value.contains([' ', '\t', '"']) {
        return value.into_owned();
    }
    format!("\"{}\"", value.replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cron_name_is_refused_with_the_numbers_to_use() {
        let error = parse_cron("0 9 * * MON").err().unwrap().to_string();
        assert_eq!(
            error,
            "cron field \"MON\": use numbers from 0 to 7; names such as MON or JAN are not supported"
        );
        assert!(parse_cron("0 9 * * 1-5").is_ok());
    }

    fn fixture(backend: Backend) -> (PathBuf, Scheduler) {
        let root = std::env::temp_dir().join(format!("haarness-scheduler-{}", Uuid::now_v7()));
        fs::create_dir_all(root.join("workspace")).unwrap();
        let paths = SchedulerPaths {
            config: root.join("config"),
            state: root.join("state"),
            launchd: root.join("launchd"),
            systemd: root.join("systemd"),
            windows: root.join("windows"),
        };
        (
            root,
            Scheduler::with_paths(
                paths,
                PathBuf::from("/opt/bin/haarness"),
                None,
                backend,
                false,
            ),
        )
    }

    #[test]
    fn completed_once_records_and_disables_past_triggers_without_disabling_rescheduled_jobs() {
        let (root, scheduler) = fixture(Backend::Launchd);
        let mut schedule = create(
            &scheduler,
            &root,
            Trigger::Once {
                at: Utc::now() + chrono::Duration::hours(1),
            },
        );
        schedule.trigger = Trigger::Once {
            at: Utc::now() - chrono::Duration::minutes(1),
        };
        scheduler.persist(&schedule).unwrap();
        let run = ScheduleRun {
            id: Uuid::now_v7(),
            schedule_id: schedule.id,
            status: RunStatus::Success,
            source: RunSource::Launchd,
            started_at: Utc::now(),
            finished_at: Utc::now(),
            thread_id: Some(Uuid::now_v7()),
            turn_id: None,
            response: Some("done".into()),
            error: None,
        };
        let finished = scheduler
            .finish(
                &schedule,
                run.clone(),
                scheduler.acquire(&schedule).unwrap().unwrap(),
            )
            .unwrap();
        assert_eq!(finished.status, RunStatus::Success);
        assert!(!scheduler.require(&schedule.id.to_string()).unwrap().enabled);
        assert_eq!(
            scheduler
                .history(&schedule.id.to_string(), 10)
                .unwrap()
                .len(),
            1
        );
        assert!(!scheduler.launchd_path(&schedule).exists());
        scheduler
            .update(
                &schedule.id.to_string(),
                UpdateSchedule {
                    name: Some("Renamed completed job".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(scheduler.toggle(&schedule.id.to_string(), true).is_err());
        let updated = scheduler
            .update(
                &schedule.id.to_string(),
                UpdateSchedule {
                    trigger: Some(Trigger::Interval { seconds: 3600 }),
                    enabled: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        scheduler
            .finish(
                &schedule,
                run,
                scheduler.acquire(&schedule).unwrap().unwrap(),
            )
            .unwrap();
        assert!(scheduler.require(&updated.id.to_string()).unwrap().enabled);
        assert!(scheduler.launchd_path(&updated).exists());
        fs::remove_dir_all(root).unwrap();
    }

    fn create(scheduler: &Scheduler, root: &Path, trigger: Trigger) -> Schedule {
        scheduler
            .create(CreateSchedule {
                name: Some("repo summary".into()),
                prompt: Some("summarize this repository".into()),
                workspace: root.join("workspace"),
                model: Some("echo/echo-1".into()),
                enabled: true,
                trigger: Some(trigger),
                ..CreateSchedule::default()
            })
            .unwrap()
    }

    #[test]
    fn stored_project_policy_survives_updates_and_reaches_runner() {
        let (root, scheduler) = fixture(Backend::Launchd);
        let schedule = create(&scheduler, &root, Trigger::Interval { seconds: 3600 });
        assert!(!schedule.trust_project);
        let updated = scheduler
            .update(
                &schedule.id.to_string(),
                UpdateSchedule {
                    name: Some("renamed".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!updated.trust_project);
        let args = scheduler.runner_args(&updated, RunSource::Launchd);
        assert!(args.iter().any(|arg| arg == "--disable-project-config"));

        let enabled = scheduler
            .update(
                &schedule.id.to_string(),
                UpdateSchedule {
                    trust_project: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(enabled.trust_project);
        let args = scheduler.runner_args(&enabled, RunSource::Launchd);
        assert!(!args.iter().any(|arg| arg == "--disable-project-config"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn triggers_read_in_plain_words() {
        assert_eq!(
            describe_trigger(&Trigger::Interval { seconds: 60 }),
            "Every minute"
        );
        assert_eq!(
            describe_trigger(&Trigger::Interval { seconds: 1_800 }),
            "Every 30 minutes"
        );
        assert_eq!(
            describe_trigger(&Trigger::Interval { seconds: 7_200 }),
            "Every 2 hours"
        );
        assert_eq!(
            describe_trigger(&Trigger::Interval { seconds: 86_400 }),
            "Every day"
        );
        assert_eq!(
            describe_trigger(&Trigger::Interval { seconds: 90 }),
            "Every 90 seconds"
        );
        let at = chrono::TimeZone::with_ymd_and_hms(&Local, 2026, 10, 10, 16, 50, 0).unwrap();
        assert_eq!(
            describe_trigger(&Trigger::Once {
                at: at.with_timezone(&Utc)
            }),
            "Once at 2026-10-10 16:50"
        );
    }

    #[test]
    fn natural_language_matches_common_haarness_forms() {
        let parsed = parse_request("every Monday at 8am to summarize notifications").unwrap();
        assert_eq!(
            parsed.trigger,
            Trigger::Cron {
                expression: "0 8 * * 1".into()
            }
        );
        let parsed =
            parse_request("schedule named \"repo-summary\" every 2 minutes to summarize the repo")
                .unwrap();
        assert_eq!(parsed.name.as_deref(), Some("repo-summary"));
        assert_eq!(parsed.prompt, "summarize the repo");
        assert_eq!(parsed.trigger, Trigger::Interval { seconds: 120 });
    }

    #[test]
    fn natural_language_keywords_ignore_case_and_keep_the_prompt_as_typed() {
        let parsed = parse_request("Every Day at 8AM To Summarize PRs").unwrap();
        assert_eq!(
            parsed.trigger,
            Trigger::Cron {
                expression: "0 8 * * *".into()
            }
        );
        assert_eq!(parsed.prompt, "Summarize PRs");
        let parsed = parse_request("EVERY friday AT 5pm to Write the Weekly note").unwrap();
        assert_eq!(
            parsed.trigger,
            Trigger::Cron {
                expression: "0 17 * * 5".into()
            }
        );
        assert_eq!(parsed.prompt, "Write the Weekly note");
        let parsed = parse_request("In 10 minutes to Check CI").unwrap();
        assert!(matches!(parsed.trigger, Trigger::Once { .. }));
        assert_eq!(parsed.prompt, "Check CI");
        let parsed = parse_request("Every 30 Minutes TO Ping").unwrap();
        assert_eq!(parsed.trigger, Trigger::Interval { seconds: 1800 });
        assert_eq!(parsed.prompt, "Ping");
        let parsed = parse_request("On 2030-01-02 to Renew the Domain").unwrap();
        assert!(matches!(parsed.trigger, Trigger::Once { .. }));
        assert_eq!(parsed.prompt, "Renew the Domain");
        let parsed = parse_request("Schedule Named \"Ünïcode\" every day at 7am to Grüß").unwrap();
        assert_eq!(parsed.name.as_deref(), Some("Ünïcode"));
        assert_eq!(parsed.prompt, "Grüß");
    }

    #[test]
    fn lifecycle_is_file_backed_and_disabled_jobs_are_unregistered() {
        let (root, scheduler) = fixture(Backend::Launchd);
        let schedule = create(&scheduler, &root, Trigger::Interval { seconds: 120 });
        assert!(scheduler.job_path(schedule.id).is_file());
        assert!(scheduler.launchd_path(&schedule).is_file());
        assert_eq!(scheduler.list().unwrap(), vec![schedule.clone()]);
        let disabled = scheduler.toggle(&schedule.id.to_string(), false).unwrap();
        assert!(!disabled.enabled);
        assert!(!scheduler.launchd_path(&schedule).exists());
        scheduler.remove(&schedule.name).unwrap();
        assert!(scheduler.list().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn launchd_render_escapes_values_and_expands_cron() {
        let (root, scheduler) = fixture(Backend::Launchd);
        let mut schedule = create(
            &scheduler,
            &root,
            Trigger::Cron {
                expression: "0,30 8 * * 1-5".into(),
            },
        );
        schedule.name = "A&B".into();
        let xml = scheduler.render_launchd(&schedule).unwrap();
        assert!(xml.contains("dev.haarness.schedule"));
        assert!(xml.contains("<key>Weekday</key><integer>1</integer>"));
        assert!(xml.contains("<key>Minute</key><integer>30</integer>"));
        assert!(xml.contains("schedule</string>"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_runners_use_the_install_that_created_the_schedule() {
        for backend in [Backend::Launchd, Backend::SystemdUser, Backend::WindowsTask] {
            let (root, mut scheduler) = fixture(backend);
            let schedule = create(&scheduler, &root, Trigger::Interval { seconds: 3600 });
            scheduler.environment = vec![
                ("HAARNESS_HOME", "/isolated home".into()),
                ("XDG_CACHE_HOME", "/xdg/cache".into()),
            ];
            match backend {
                Backend::Launchd => {
                    let xml = scheduler.render_launchd(&schedule).unwrap();
                    assert!(
                        xml.contains("<key>HAARNESS_HOME</key><string>/isolated home</string>")
                    );
                    assert!(xml.contains("<key>XDG_CACHE_HOME</key><string>/xdg/cache</string>"));
                }
                Backend::SystemdUser => {
                    let (service, _) = scheduler.render_systemd(&schedule).unwrap();
                    assert!(service.contains("Environment=\"HAARNESS_HOME=/isolated home\"\n"));
                }
                Backend::WindowsTask => {
                    let xml = scheduler.render_windows(&schedule).unwrap();
                    assert!(xml.contains("<Command>cmd.exe</Command>"), "{xml}");
                    assert!(xml.contains(
                        "<Arguments>/d /s /c &quot;set &quot;HAARNESS_HOME=/isolated home&quot; &amp;&amp; set &quot;XDG_CACHE_HOME=/xdg/cache&quot; &amp;&amp; /opt/bin/haarness --workspace"
                    ), "{xml}");
                }
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn systemd_paths_are_written_as_systemd_reads_them() {
        let (root, scheduler) = fixture(Backend::SystemdUser);
        let mut schedule = create(&scheduler, &root, Trigger::Interval { seconds: 3600 });
        schedule.workspace = root.join("my 100% project");
        let (service, _) = scheduler.render_systemd(&schedule).unwrap();
        // systemd neither strips quotes nor decodes escapes in a path setting:
        // a quoted WorkingDirectory is "not absolute", the unit never loads and
        // its timer refuses to start (systemd 249, Ubuntu 22.04).
        let written = root
            .join("my 100% project")
            .display()
            .to_string()
            .replace('%', "%%");
        let directory = format!("WorkingDirectory={written}\n");
        assert!(service.contains(&directory), "{service}");
        let log = format!(
            "StandardOutput=append:{}\n",
            systemd_path(&scheduler.log_path(schedule.id))
        );
        assert!(service.contains(&log) && !log.contains('"'), "{service}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn systemd_render_uses_persistent_user_timer() {
        let (root, scheduler) = fixture(Backend::SystemdUser);
        let schedule = create(
            &scheduler,
            &root,
            Trigger::Cron {
                expression: "15 9 * * 1".into(),
            },
        );
        let (service, timer) = scheduler.render_systemd(&schedule).unwrap();
        assert!(service.contains("Type=oneshot"));
        assert!(service.contains("schedule\" \"trigger"));
        assert!(timer.contains("OnCalendar=Mon *-*-* 9:15:00"));
        assert!(timer.contains("Persistent=true"));
        // An interval timer needs a first run from its own start; repeating
        // from the last run alone never begins.
        let interval = Schedule {
            trigger: Trigger::Interval { seconds: 300 },
            ..schedule
        };
        let (_, timer) = scheduler.render_systemd(&interval).unwrap();
        assert!(
            timer.contains("[Timer]\nOnActiveSec=300s\nOnUnitActiveSec=300s\nAccuracySec=1s\n"),
            "{timer}"
        );
        assert!(!timer.contains("OnCalendar="));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn windows_render_supports_interval_daily_and_weekly() {
        let (root, scheduler) = fixture(Backend::WindowsTask);
        let interval = create(&scheduler, &root, Trigger::Interval { seconds: 300 });
        assert!(
            scheduler
                .render_windows(&interval)
                .unwrap()
                .contains("<Interval>PT300S</Interval>")
        );
        let weekly = Schedule {
            id: Uuid::now_v7(),
            trigger: Trigger::Cron {
                expression: "0 8 * * 1".into(),
            },
            ..interval.clone()
        };
        assert!(
            scheduler
                .render_windows(&weekly)
                .unwrap()
                .contains("<Monday/>")
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn write_lock(scheduler: &Scheduler, schedule: &Schedule, pid: u32, age: Duration) {
        let path = scheduler.lock_path(schedule.id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let created = Utc::now() - chrono::TimeDelta::from_std(age).unwrap();
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"pid": pid, "createdAt": created})).unwrap(),
        )
        .unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - age)
            .unwrap();
    }

    #[test]
    fn a_live_run_keeps_its_lock_however_long_it_runs() {
        let (root, scheduler) = fixture(Backend::Launchd);
        let schedule = create(&scheduler, &root, Trigger::Interval { seconds: 60 });
        write_lock(
            &scheduler,
            &schedule,
            std::process::id(),
            Duration::from_secs(2 * 60 * 60),
        );
        assert!(scheduler.acquire(&schedule).unwrap().is_none());
        assert!(scheduler.lock_path(schedule.id).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_lock_whose_run_is_gone_is_taken_at_once() {
        let (root, scheduler) = fixture(Backend::Launchd);
        let schedule = create(&scheduler, &root, Trigger::Interval { seconds: 60 });
        // A run that exited a moment ago.
        let mut exited = Command::new("true").spawn().unwrap();
        exited.wait().unwrap();
        write_lock(&scheduler, &schedule, exited.id(), Duration::from_secs(5));
        assert!(scheduler.acquire(&schedule).unwrap().is_some());
        // A live process that started after the lock was written reuses a
        // crashed run's pid; it does not hold the lock.
        let mut reused = Command::new("sleep").arg("30").spawn().unwrap();
        write_lock(
            &scheduler,
            &schedule,
            reused.id(),
            Duration::from_secs(2 * 60 * 60),
        );
        assert!(scheduler.acquire(&schedule).unwrap().is_some());
        // The same process, holding a lock it wrote before this check, keeps it.
        write_lock(&scheduler, &schedule, reused.id(), Duration::ZERO);
        assert!(scheduler.acquire(&schedule).unwrap().is_none());
        reused.kill().unwrap();
        reused.wait().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_finished_run_removes_only_its_own_lock() {
        let (root, scheduler) = fixture(Backend::Launchd);
        let schedule = create(&scheduler, &root, Trigger::Interval { seconds: 60 });
        let guard = scheduler.acquire(&schedule).unwrap().unwrap();
        // Another runner took the path over meanwhile.
        write_lock(&scheduler, &schedule, 1, Duration::ZERO);
        let other = fs::read(scheduler.lock_path(schedule.id)).unwrap();
        drop(guard);
        assert_eq!(fs::read(scheduler.lock_path(schedule.id)).unwrap(), other);
        assert_eq!(elapsed_seconds("05:09"), Some(309));
        assert_eq!(elapsed_seconds("2:05:09"), Some(7509));
        assert_eq!(elapsed_seconds("3-02:05:09"), Some(3 * 86_400 + 7509));
        assert_eq!(elapsed_seconds("12"), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lock_skips_overlap_and_history_is_newest_first() {
        let (root, scheduler) = fixture(Backend::Launchd);
        let schedule = create(&scheduler, &root, Trigger::Interval { seconds: 60 });
        let guard = scheduler.acquire(&schedule).unwrap().unwrap();
        assert!(scheduler.acquire(&schedule).unwrap().is_none());
        drop(guard);
        assert!(scheduler.acquire(&schedule).unwrap().is_some());
        for status in [RunStatus::Success, RunStatus::Failed] {
            scheduler
                .record(
                    &schedule,
                    &ScheduleRun {
                        id: Uuid::now_v7(),
                        schedule_id: schedule.id,
                        status,
                        source: RunSource::Manual,
                        started_at: Utc::now(),
                        finished_at: Utc::now(),
                        thread_id: None,
                        turn_id: None,
                        response: None,
                        error: None,
                    },
                )
                .unwrap();
        }
        assert_eq!(
            scheduler.history(&schedule.name, 1).unwrap()[0].status,
            RunStatus::Failed
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_unsafe_cadence_and_ambiguous_model() {
        let (root, scheduler) = fixture(Backend::Launchd);
        let error = scheduler
            .create(CreateSchedule {
                prompt: Some("test".into()),
                workspace: root.join("workspace"),
                trigger: Some(Trigger::Interval { seconds: 59 }),
                enabled: true,
                ..CreateSchedule::default()
            })
            .unwrap_err();
        assert!(error.to_string().contains("minimum schedule interval"));
        let error = scheduler
            .create(CreateSchedule {
                prompt: Some("test".into()),
                workspace: root.join("workspace"),
                model: Some("echo-1".into()),
                trigger: Some(Trigger::Interval { seconds: 60 }),
                enabled: true,
                ..CreateSchedule::default()
            })
            .unwrap_err();
        assert!(error.to_string().contains("provider/model"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn schedule_names_are_unique_because_they_are_references() {
        let (root, scheduler) = fixture(Backend::Launchd);
        let first = create(&scheduler, &root, Trigger::Interval { seconds: 3600 });
        // A name someone chose is refused when taken.
        let error = scheduler
            .create(CreateSchedule {
                request: Some("schedule named \"repo summary\" every day at 8am to x".into()),
                workspace: root.join("workspace"),
                enabled: true,
                ..CreateSchedule::default()
            })
            .unwrap_err();
        assert!(error.to_string().contains("already exists"), "{error}");
        // A name made from the prompt is numbered instead.
        for expected in ["repo summary 2", "repo summary 3"] {
            let numbered = scheduler
                .create(CreateSchedule {
                    request: Some("every day at 8am to repo summary".into()),
                    workspace: root.join("workspace"),
                    enabled: true,
                    ..CreateSchedule::default()
                })
                .unwrap();
            assert_eq!(numbered.name, expected);
        }
        let second = scheduler
            .create(CreateSchedule {
                name: Some("other".into()),
                prompt: Some("x".into()),
                workspace: root.join("workspace"),
                trigger: Some(Trigger::Interval { seconds: 3600 }),
                enabled: true,
                ..CreateSchedule::default()
            })
            .unwrap();
        let error = scheduler
            .update(
                "other",
                UpdateSchedule {
                    name: Some(first.name.clone()),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("already exists"), "{error}");
        // Keeping its own name is not a clash.
        scheduler
            .update(
                &second.id.to_string(),
                UpdateSchedule {
                    name: Some("other".into()),
                    prompt: Some("y".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
