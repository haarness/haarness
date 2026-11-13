//! Read-only cross-workspace analytics. No provider calls or transcript export.
mod render;
pub use render::{render_html, render_text, write_report};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
pub use haarness_core::FailureKind;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};
use utoipa::ToSchema;

#[derive(Clone, Debug, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    pub days: Option<u32>,
    pub project: Option<String>,
    #[serde(default)]
    pub current_project: bool,
    pub limit: Option<usize>,
}

impl Options {
    pub fn validate(&self) -> Result<()> {
        if self.days.is_some_and(|n| n == 0 || n > 36_500) {
            bail!("days must be between 1 and 36500");
        }
        if self.limit.is_some_and(|n| n == 0 || n > 1_000_000) {
            bail!("limit must be between 1 and 1000000");
        }
        if self.current_project && self.project.is_some() {
            bail!("choose --project or --here, not both");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub sessions: u64,
    pub turns: u64,
    pub completed: u64,
    pub failed: u64,
    pub interrupted: u64,
    pub in_progress: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub known_cost_usd: f64,
    pub priced_usage_records: u64,
    pub unpriced_usage_records: u64,
    pub turns_without_usage: u64,
    pub tool_calls: u64,
    pub tool_results: u64,
    pub tool_errors: u64,
    pub file_changes: u64,
    pub additions: u64,
    pub deletions: u64,
}

impl Metrics {
    pub fn tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
    fn add(&mut self, other: &Self) {
        macro_rules! sum { ($($field:ident),*) => { $(self.$field = self.$field.saturating_add(other.$field);)* }; }
        sum!(
            sessions,
            turns,
            completed,
            failed,
            interrupted,
            in_progress,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            priced_usage_records,
            unpriced_usage_records,
            turns_without_usage,
            tool_calls,
            tool_results,
            tool_errors,
            file_changes,
            additions,
            deletions
        );
        self.known_cost_usd += other.known_cost_usd;
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: String,
    pub name: String,
    pub metrics: Metrics,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub since: Option<DateTime<Utc>>,
    pub options: Options,
    pub total: Metrics,
    pub projects: Vec<Group>,
    pub models: Vec<Group>,
    pub tools: Vec<Group>,
    pub files: Vec<Group>,
    pub days: Vec<Group>,
    pub sessions: Vec<Group>,
    pub findings: Vec<String>,
    pub warnings: Vec<String>,
    pub databases_read: usize,
    pub matching_sessions: usize,
    pub duplicate_turns_skipped: usize,
    pub detached_items_skipped: u64,
}

/// The workspace registry, with the chat homes `chat-home.json` names
/// marked too (an older build may have rewritten the registry without the
/// mark).
fn read_registry(data: &Path) -> Result<Registry> {
    #[derive(Default, Deserialize)]
    struct Marks {
        #[serde(default)]
        paths: Vec<PathBuf>,
    }
    let registry_path = data.join("workspaces.json");
    let mut registry: Registry = if registry_path.exists() {
        serde_json::from_slice(&std::fs::read(&registry_path)?)
            .context("read workspace registry")?
    } else {
        Registry::default()
    };
    let marks = std::fs::read(data.join("chat-home.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Marks>(&bytes).ok())
        .unwrap_or_default();
    for workspace in &mut registry.workspaces {
        if marks
            .paths
            .iter()
            .any(|path| path == Path::new(&workspace.path))
        {
            workspace.chat_home = true;
        }
    }
    Ok(registry)
}

/// How reports name the chat home's chats.
const CHATS_WITHOUT_A_PROJECT: &str = "Chats without a project";

#[derive(Default, Deserialize)]
struct Registry {
    #[serde(default)]
    projects: Vec<Project>,
    #[serde(default)]
    workspaces: Vec<Workspace>,
}
#[derive(Deserialize)]
struct Project {
    id: String,
    name: String,
    root: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Workspace {
    id: String,
    project_id: String,
    path: String,
    /// The chat home holds the folders of chats without a project.
    #[serde(default)]
    chat_home: bool,
}
struct Source {
    path: PathBuf,
    project: String,
    name: String,
    workspace: Option<PathBuf>,
}
struct RecordedTurn {
    id: String,
    session: String,
    /// The chat the person sees: `session` itself, or for a subagent's
    /// thread the chat whose turn started it.
    chat: String,
    title: String,
    project: String,
    project_name: String,
    model: String,
    provider: String,
    model_name: String,
    /// Run by a subagent (a `task` call), not the chat's own agent.
    subagent: bool,
    /// Part of a scheduled chat.
    scheduled: bool,
    /// Why a failed turn of the chat's own agent failed, read from its
    /// stored error.
    failure: Option<FailureKind>,
    /// The words a failed turn stored, read only for the daily usage.
    error: Option<String>,
    /// The Haarness version that ran the turn, when the history recorded it.
    version: Option<String>,
    created: DateTime<Utc>,
    /// When the turn last changed: when it ended, for one that did.
    updated: DateTime<Utc>,
    /// The last tool the turn called.
    last_tool: Option<String>,
    /// The turn's tool calls that answered with an error, as
    /// `(tool, output)`, read only for the daily usage.
    tool_errors: Vec<(String, String)>,
    metrics: Metrics,
    tools: BTreeMap<String, Metrics>,
    files: BTreeMap<String, Metrics>,
}

/// Reads committed SQLite snapshots without opening/migrating a store or registering a workspace.
/// The time window selects turns by start time; all recorded events of those turns are counted.
pub fn collect(data: &Path, workspace: &Path, options: Options) -> Result<Report> {
    collect_at(data, workspace, options, Utc::now())
}

pub fn collect_at(
    data: &Path,
    workspace: &Path,
    options: Options,
    now: DateTime<Utc>,
) -> Result<Report> {
    options.validate()?;
    let registry = read_registry(data)?;
    let project_filter = if options.current_project {
        let path = dunce::canonicalize(workspace).unwrap_or_else(|_| workspace.to_owned());
        Some(
            registry
                .workspaces
                .iter()
                .filter(|w| path.starts_with(Path::new(&w.path)))
                .max_by_key(|w| Path::new(&w.path).components().count())
                .map(|w| w.project_id.clone())
                .context("current workspace is not registered; run a chat here first")?,
        )
    } else if let Some(reference) = &options.project {
        let matches: Vec<_> = registry
            .projects
            .iter()
            .filter(|p| p.id == *reference || p.name == *reference || p.root == *reference)
            .collect();
        match matches.as_slice() {
            [project] => Some(project.id.clone()),
            [] => bail!(
                "unknown project {reference:?}; use a registered project ID, name or root path"
            ),
            _ => bail!("ambiguous project {reference:?}; use its ID or root path"),
        }
    } else {
        None
    };
    let since = options
        .days
        .map(|days| now - chrono::Duration::days(i64::from(days)));
    let mut report = Report {
        schema_version: 1,
        generated_at: now,
        since,
        options,
        total: Metrics::default(),
        projects: vec![],
        models: vec![],
        tools: vec![],
        files: vec![],
        days: vec![],
        sessions: vec![],
        findings: vec![],
        warnings: vec![],
        databases_read: 0,
        matching_sessions: 0,
        duplicate_turns_skipped: 0,
        detached_items_skipped: 0,
    };
    let scan = scan(
        data,
        &registry,
        project_filter.as_deref(),
        since,
        now,
        false,
    )?;
    report.databases_read = scan.databases_read;
    report.detached_items_skipped = scan.detached_items;
    report.duplicate_turns_skipped = scan.duplicate_turns;
    report.warnings.extend(scan.warnings);
    let turns = scan.turns;
    let mut recent = BTreeMap::<String, DateTime<Utc>>::new();
    for turn in &turns {
        recent
            .entry(turn.session.clone())
            .and_modify(|date| *date = (*date).max(turn.created))
            .or_insert(turn.created);
    }
    let mut recent: Vec<_> = recent.into_iter().collect();
    recent.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    report.matching_sessions = recent.len();
    if let Some(limit) = report.options.limit {
        recent.truncate(limit);
    }
    let selected: BTreeSet<_> = recent.into_iter().map(|(id, _)| id).collect();
    let (mut projects, mut models, mut tools, mut files, mut days, mut sessions) = (
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    let mut group_sessions = BTreeSet::new();
    for turn in turns.into_iter().filter(|t| selected.contains(&t.session)) {
        report.total.add(&turn.metrics);
        let day = turn.created.format("%Y-%m-%d").to_string();
        for (kind, key, name, map) in [
            ("project", &turn.project, &turn.project_name, &mut projects),
            ("model", &turn.model, &turn.model, &mut models),
            ("day", &day, &day, &mut days),
            ("session", &turn.session, &turn.title, &mut sessions),
        ] {
            let metrics = group(map, key, name);
            metrics.add(&turn.metrics);
            if group_sessions.insert((kind.to_owned(), key.clone(), turn.session.clone())) {
                metrics.sessions += 1;
            }
        }
        for (name, metrics) in turn.tools {
            group(&mut tools, &name, &name).add(&metrics);
        }
        for (name, metrics) in turn.files {
            let id = format!("{}:{name}", turn.project);
            group(&mut files, &id, &format!("{} / {name}", turn.project_name)).add(&metrics);
        }
    }
    report.total.sessions = u64::try_from(selected.len())?;
    report.projects = ranked(projects);
    report.models = ranked(models);
    report.tools = ranked(tools);
    report.files = ranked(files);
    report.days = days.into_values().collect();
    report.sessions = ranked(sessions);
    report.findings = findings(&report);
    Ok(report)
}

/// One UTC day of activity across every workspace: the source of the usage
/// statistics Haarness shares unless they are turned off
/// (docs/USAGE-STATISTICS.md). Counts, model names and the failures of the
/// day; no titles, projects or file names. A failure's words are kept as the
/// history stored them: usage statistics scrub them before they leave.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsage {
    /// `YYYY-MM-DD`, by turn start in UTC.
    pub day: String,
    pub total: Metrics,
    /// Turns run by subagents (`task` calls).
    pub subagent_turns: u64,
    /// Turns in scheduled chats.
    pub scheduled_turns: u64,
    pub models: Vec<ModelUsage>,
    /// Keyed by tool name.
    pub tools: Vec<Group>,
    /// The failed turns counted in `total.failed`, oldest first.
    pub failures: Vec<FailedTurn>,
    /// Every tool call of the day that answered with an error, oldest first.
    pub tool_errors: Vec<ToolError>,
    /// The day's turns by the Haarness version that ran them.
    pub versions: Vec<VersionUsage>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub provider: String,
    pub model: String,
    pub metrics: Metrics,
}

/// One failed turn of a chat's own agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedTurn {
    pub id: String,
    /// When the turn started.
    pub at: DateTime<Utc>,
    pub kind: FailureKind,
    pub provider: String,
    pub model: String,
    /// The Haarness version that ran it; `None` for turns recorded before
    /// versions were.
    pub version: Option<String>,
    /// The words the turn stored, as the transcript shows them.
    pub error: String,
    /// From the turn's start to its failure.
    pub duration_ms: u64,
    pub tool_calls: u64,
    pub tool_errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The last tool the turn called before it failed.
    pub last_tool: Option<String>,
}

/// A tool call that answered with an error, on the model of the turn that
/// made it (a subagent's own turn for a subagent's call).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolError {
    pub tool: String,
    pub at: DateTime<Utc>,
    pub provider: String,
    pub model: String,
    pub version: Option<String>,
    /// The start of what the tool answered.
    pub output: String,
}

/// The turns one Haarness version ran on a day, counted as the day's totals
/// count them. `version` is `None` for turns recorded before versions were.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionUsage {
    pub version: Option<String>,
    pub turns: u64,
    pub completed: u64,
    pub failed: u64,
    pub interrupted: u64,
}

/// The most of a tool's error output read back for usage statistics, which
/// keep less still.
const TOOL_ERROR_READ: usize = 2_000;

/// The last `days` UTC days (today included) of every workspace, oldest first.
/// Days without turns are left out.
pub fn daily_usage(data: &Path, days: u32, now: DateTime<Utc>) -> Result<Vec<DailyUsage>> {
    anyhow::ensure!((1..=366).contains(&days), "days must be between 1 and 366");
    let registry = read_registry(data)?;
    let start = (now - chrono::Duration::days(i64::from(days) - 1))
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .context("start of day")?
        .and_utc();
    let scan = scan(data, &registry, None, Some(start), now, true)?;
    let mut by_day = BTreeMap::<String, (DailyUsage, BTreeSet<String>)>::new();
    for turn in scan.turns {
        let day = turn.created.format("%Y-%m-%d").to_string();
        let (usage, sessions) = by_day.entry(day.clone()).or_insert_with(|| {
            (
                DailyUsage {
                    day,
                    ..DailyUsage::default()
                },
                BTreeSet::new(),
            )
        });
        usage.total.add(&turn.metrics);
        // A subagent's thread is part of its chat, not a chat of its own.
        if sessions.insert(turn.chat.clone()) {
            usage.total.sessions += 1;
        }
        usage.subagent_turns += u64::from(turn.subagent);
        usage.scheduled_turns += u64::from(turn.scheduled);
        if let Some(kind) = turn.failure {
            usage.failures.push(FailedTurn {
                id: turn.id.clone(),
                at: turn.created,
                kind,
                provider: turn.provider.clone(),
                model: turn.model_name.clone(),
                version: turn.version.clone(),
                error: turn.error.clone().unwrap_or_default(),
                duration_ms: u64::try_from((turn.updated - turn.created).num_milliseconds())
                    .unwrap_or_default(),
                tool_calls: turn.metrics.tool_calls,
                tool_errors: turn.metrics.tool_errors,
                input_tokens: turn.metrics.input_tokens,
                output_tokens: turn.metrics.output_tokens,
                last_tool: turn.last_tool.clone(),
            });
        }
        for (tool, output) in &turn.tool_errors {
            usage.tool_errors.push(ToolError {
                tool: tool.clone(),
                at: turn.created,
                provider: turn.provider.clone(),
                model: turn.model_name.clone(),
                version: turn.version.clone(),
                output: output.clone(),
            });
        }
        let version = match usage
            .versions
            .iter_mut()
            .position(|row| row.version == turn.version)
        {
            Some(index) => &mut usage.versions[index],
            None => {
                usage.versions.push(VersionUsage {
                    version: turn.version.clone(),
                    ..VersionUsage::default()
                });
                usage.versions.last_mut().expect("just pushed")
            }
        };
        version.turns += turn.metrics.turns;
        version.completed += turn.metrics.completed;
        version.failed += turn.metrics.failed;
        version.interrupted += turn.metrics.interrupted;
        match usage
            .models
            .iter_mut()
            .find(|row| row.provider == turn.provider && row.model == turn.model_name)
        {
            Some(row) => row.metrics.add(&turn.metrics),
            None => usage.models.push(ModelUsage {
                provider: turn.provider.clone(),
                model: turn.model_name.clone(),
                metrics: turn.metrics.clone(),
            }),
        }
        for (name, metrics) in turn.tools {
            match usage.tools.iter_mut().find(|row| row.id == name) {
                Some(row) => row.metrics.add(&metrics),
                None => usage.tools.push(Group {
                    id: name.clone(),
                    name,
                    metrics,
                }),
            }
        }
    }
    Ok(by_day
        .into_values()
        .map(|(mut usage, _)| {
            // Turns are scanned across histories; the day reads in time order.
            usage
                .failures
                .sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
            usage.tool_errors.sort_by(|a, b| a.at.cmp(&b.at));
            usage
        })
        .collect())
}

/// Every recorded turn in the window, across workspace databases, once each.
struct Scan {
    turns: Vec<RecordedTurn>,
    databases_read: usize,
    detached_items: u64,
    duplicate_turns: usize,
    warnings: Vec<String>,
}

/// `details` also reads what failed turns and tool calls said, which only the
/// daily usage needs.
fn scan(
    data: &Path,
    registry: &Registry,
    project_filter: Option<&str>,
    since: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    details: bool,
) -> Result<Scan> {
    let mut result = Scan {
        turns: Vec::new(),
        databases_read: 0,
        detached_items: 0,
        duplicate_turns: 0,
        warnings: Vec::new(),
    };
    let mut sources = Vec::new();
    let directory = data.join("workspaces");
    if directory.exists() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            let registered = registry.workspaces.iter().find(|w| w.id == id);
            let project = registered.map_or(id.as_str(), |w| w.project_id.as_str());
            if project_filter.is_some_and(|p| p != project) {
                continue;
            }
            let name = if registered.is_some_and(|workspace| workspace.chat_home) {
                // Its folder's name is no project's.
                CHATS_WITHOUT_A_PROJECT.to_owned()
            } else {
                registry
                    .projects
                    .iter()
                    .find(|p| p.id == project)
                    .map_or_else(
                        || format!("Unregistered workspace {id}"),
                        |p| p.name.clone(),
                    )
            };
            let path = entry.path().join("haarness.db");
            if path.is_file() {
                sources.push(Source {
                    path,
                    project: project.to_owned(),
                    name,
                    workspace: registered.map(|workspace| PathBuf::from(&workspace.path)),
                });
            }
        }
    }
    if project_filter.is_none() && data.join("haarness.db").is_file() {
        sources.push(Source {
            path: data.join("haarness.db"),
            project: "legacy".into(),
            name: "Legacy history (unassigned)".into(),
            workspace: None,
        });
    }
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    let mut turns = Vec::new();
    for source in sources {
        match read_source(&source, since, now, details) {
            Ok((records, detached)) => {
                turns.extend(records);
                result.databases_read += 1;
                result.detached_items += detached;
            }
            Err(error) => result.warnings.push(format!(
                "Could not read {}: {error:#}",
                source.path.display()
            )),
        }
    }
    // Archive imports preserve turn IDs. Prefer the most complete copy, then stable source order.
    turns.sort_by(|a, b| {
        a.metrics
            .turns_without_usage
            .cmp(&b.metrics.turns_without_usage)
            .then_with(|| b.metrics.tokens().cmp(&a.metrics.tokens()))
            .then_with(|| b.metrics.tool_results.cmp(&a.metrics.tool_results))
            .then_with(|| {
                b.metrics
                    .priced_usage_records
                    .cmp(&a.metrics.priced_usage_records)
            })
    });
    let mut seen = BTreeSet::new();
    turns.retain(|turn| {
        let unique = seen.insert(turn.id.clone());
        if !unique {
            result.duplicate_turns += 1;
        }
        unique
    });
    result.turns = turns;
    Ok(result)
}

fn group<'a>(map: &'a mut BTreeMap<String, Group>, id: &str, name: &str) -> &'a mut Metrics {
    &mut map
        .entry(id.to_owned())
        .or_insert_with(|| Group {
            id: id.to_owned(),
            name: name.to_owned(),
            metrics: Metrics::default(),
        })
        .metrics
}
fn ranked(map: BTreeMap<String, Group>) -> Vec<Group> {
    let mut values: Vec<_> = map.into_values().collect();
    values.sort_by(|a, b| {
        b.metrics
            .tokens()
            .cmp(&a.metrics.tokens())
            .then_with(|| b.metrics.tool_calls.cmp(&a.metrics.tool_calls))
            .then_with(|| b.metrics.file_changes.cmp(&a.metrics.file_changes))
            .then_with(|| a.id.cmp(&b.id))
    });
    values
}

fn read_source(
    source: &Source,
    since: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    details: bool,
) -> Result<(Vec<RecordedTurn>, u64)> {
    let mut connection = history_connection(&source.path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    let tx = connection.transaction()?;
    // Older databases predate these columns; their turns count as the chat's own.
    let has = |table: &str, column: &str| -> Result<bool> {
        Ok(tx.query_row(
            "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name=?2",
            [table, column],
            |r| r.get::<_, i64>(0),
        )? > 0)
    };
    let has_column = |column: &str| has("threads", column);
    let version = if has("turns", "app_version")? {
        "t.app_version"
    } else {
        "NULL"
    };
    let subagent = if has_column("is_subagent")? {
        "h.is_subagent"
    } else {
        "0"
    };
    let scheduled = if has_column("schedule_json")? {
        "h.schedule_json IS NOT NULL"
    } else {
        "0"
    };
    // A subagent's thread belongs to the chat whose turn started it (through
    // any subagents between them); a fork, though it names its source as
    // parent too, is a chat of its own.
    let mut parents = HashMap::new();
    if subagent != "0" {
        let mut statement = tx.prepare(
            "SELECT id,parent_id FROM threads WHERE is_subagent<>0 AND parent_id IS NOT NULL",
        )?;
        let rows =
            statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (child, parent) = row?;
            parents.insert(child, parent);
        }
    }
    let chat_of = |thread: &str| {
        let mut chat = thread;
        // Bounded: a damaged history must not loop.
        for _ in 0..64 {
            match parents.get(chat) {
                Some(parent) => chat = parent,
                None => break,
            }
        }
        chat.to_owned()
    };
    let mut statement = tx.prepare(&format!("SELECT t.id,t.thread_id,h.title,t.provider,t.model,t.status,t.created_at,{subagent},{scheduled},t.error,t.updated_at,{version} FROM turns t JOIN threads h ON h.id=t.thread_id ORDER BY t.created_at,t.id"))?;
    let rows = statement.query_map([], |r| {
        Ok((
            (
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ),
            r.get::<_, bool>(7)?,
            r.get::<_, bool>(8)?,
            r.get::<_, Option<String>>(9)?,
            r.get::<_, String>(10)?,
            r.get::<_, Option<String>>(11)?,
        ))
    })?;
    let mut turns = BTreeMap::new();
    for row in rows {
        let (
            (id, session, title, provider, model, status, date),
            subagent,
            scheduled,
            error,
            updated,
            version,
        ) = row?;
        let created = DateTime::parse_from_rfc3339(&date)?.with_timezone(&Utc);
        let updated = DateTime::parse_from_rfc3339(&updated)
            .map_or(created, |updated| updated.with_timezone(&Utc));
        if created > now || since.is_some_and(|start| created < start) {
            continue;
        }
        let mut metrics = Metrics {
            turns: 1,
            ..Metrics::default()
        };
        // Outcomes are those of the turns people start. A subagent's turn
        // ends inside the turn that started it, which reads its result, an
        // error included, and goes on: a subagent that failed fails nothing
        // the person sees unless that turn fails too, and then it counts
        // once, there. Its work (tokens, tools, files) still counts.
        let mut failure = None;
        if !subagent {
            match status.as_str() {
                "completed" => metrics.completed = 1,
                "failed" => {
                    metrics.failed = 1;
                    failure = Some(FailureKind::of(error.as_deref().unwrap_or_default()));
                }
                "interrupted" => metrics.interrupted = 1,
                _ => metrics.in_progress = 1,
            }
        }
        turns.insert(
            id.clone(),
            RecordedTurn {
                id,
                chat: chat_of(&session),
                session,
                title,
                project: source.project.clone(),
                project_name: source.name.clone(),
                model: format!("{provider}/{model}"),
                provider,
                model_name: model,
                subagent,
                scheduled,
                failure,
                error: if details && failure.is_some() {
                    error
                } else {
                    None
                },
                version: version.filter(|version| !version.is_empty()),
                created,
                updated,
                last_tool: None,
                tool_errors: Vec::new(),
                metrics,
                tools: BTreeMap::new(),
                files: BTreeMap::new(),
            },
        );
    }
    // Include soft-deleted events: undo does not refund tokens or erase work actually performed.
    // Detached fork-context items have no turn and must never be counted as new activity.
    let detached: i64 = tx.query_row(
        "SELECT COUNT(*) FROM items WHERE turn_id IS NULL",
        [],
        |r| r.get(0),
    )?;
    let mut statement = tx.prepare("SELECT turn_id,data_json FROM items WHERE turn_id IS NOT NULL AND json_extract(data_json,'$.type') IN ('usage','tool_call','tool_result') ORDER BY thread_id,sequence")?;
    let rows = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (turn_id, data) = row?;
        let Some(turn) = turns.get_mut(&turn_id) else {
            continue;
        };
        let data: Value = serde_json::from_str(&data)?;
        match data["type"].as_str() {
            Some("usage") => {
                let m = &mut turn.metrics;
                m.input_tokens = m.input_tokens.saturating_add(
                    data["input_tokens"]
                        .as_u64()
                        .context("invalid input token count")?,
                );
                m.output_tokens = m.output_tokens.saturating_add(
                    data["output_tokens"]
                        .as_u64()
                        .context("invalid output token count")?,
                );
                m.cache_read_tokens = m
                    .cache_read_tokens
                    .saturating_add(data["cache_read_tokens"].as_u64().unwrap_or(0));
                m.cache_write_tokens = m
                    .cache_write_tokens
                    .saturating_add(data["cache_write_tokens"].as_u64().unwrap_or(0));
                if let Some(cost) = data["cost_usd"]
                    .as_f64()
                    .filter(|c| c.is_finite() && *c >= 0.0)
                {
                    m.known_cost_usd += cost;
                    m.priced_usage_records += 1;
                } else {
                    m.unpriced_usage_records += 1;
                }
            }
            Some("tool_call") => {
                let name = data["name"].as_str().context("tool name missing")?;
                turn.metrics.tool_calls += 1;
                turn.tools.entry(name.into()).or_default().tool_calls += 1;
                turn.last_tool = Some(name.to_owned());
            }
            Some("tool_result") => {
                let name = data["name"].as_str().context("tool name missing")?;
                let tool = turn.tools.entry(name.into()).or_default();
                tool.tool_results += 1;
                turn.metrics.tool_results += 1;
                if data["is_error"].as_bool() == Some(true) {
                    tool.tool_errors += 1;
                    turn.metrics.tool_errors += 1;
                    if details {
                        let output = data["output"].as_str().unwrap_or_default();
                        turn.tool_errors.push((
                            name.to_owned(),
                            output.chars().take(TOOL_ERROR_READ).collect(),
                        ));
                    }
                }
                if let Some(diffs) = data["metadata"]["diffs"].as_array() {
                    for diff in diffs {
                        if let Some(file) = diff["file"].as_str() {
                            let path = Path::new(file);
                            let relative = source
                                .workspace
                                .as_ref()
                                .and_then(|root| path.strip_prefix(root).ok())
                                .unwrap_or(path);
                            let file = turn
                                .files
                                .entry(relative.display().to_string())
                                .or_default();
                            let change = Metrics {
                                file_changes: 1,
                                additions: diff["additions"].as_u64().unwrap_or(0),
                                deletions: diff["deletions"].as_u64().unwrap_or(0),
                                ..Metrics::default()
                            };
                            file.add(&change);
                            turn.metrics.add(&change);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for turn in turns.values_mut() {
        turn.metrics.turns_without_usage =
            u64::from(turn.metrics.priced_usage_records + turn.metrics.unpriced_usage_records == 0);
    }
    Ok((turns.into_values().collect(), u64::try_from(detached)?))
}

fn history_connection(path: &Path) -> Result<Connection> {
    let readonly = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|connection| {
        connection.busy_timeout(Duration::from_secs(5))?;
        // Opening a closed WAL database can succeed, then fail on its first read
        // because SQLite needs to create/reconcile the journal sidecars.
        connection.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })?;
        Ok(connection)
    });
    match readonly {
        Ok(connection) => Ok(connection),
        Err(rusqlite::Error::SqliteFailure(error, _))
            if matches!(
                error.code,
                rusqlite::ErrorCode::CannotOpen | rusqlite::ErrorCode::ReadOnly
            ) =>
        {
            // No CREATE flag: a missing history file must never become a new database.
            // Allow only SQLite's normal WAL housekeeping; all application SQL stays query-only.
            let connection = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            connection.busy_timeout(Duration::from_secs(5))?;
            connection.pragma_update(None, "query_only", "ON")?;
            Ok(connection)
        }
        Err(error) => Err(error.into()),
    }
}

fn findings(report: &Report) -> Vec<String> {
    use render::{count, money, noun};
    let total = &report.total;
    if total.turns == 0 {
        return vec![
            "No recorded turns match this scope. Start a chat or widen the date/project filters."
                .into(),
        ];
    }
    let mut output = Vec::new();
    if let Some(project) = report.projects.first() {
        output.push(format!(
            "{} accounts for {} of {} recorded input/output tokens across {} ({}).",
            project.name,
            count(project.metrics.tokens()),
            count(total.tokens()),
            noun(
                report.projects.len() as u64,
                "active project",
                "active projects"
            ),
            noun(project.metrics.turns, "turn", "turns")
        ));
    }
    if let Some(model) = report.models.first() {
        output.push(format!("{} is the largest token consumer: {} across {}. Model attribution uses each turn's recorded model, including later model switches.", model.name, noun(model.metrics.tokens(), "token", "tokens"), noun(model.metrics.turns, "turn", "turns")));
    }
    if let Some(day) = report.days.iter().max_by_key(|day| day.metrics.tokens()) {
        output.push(format!(
            "{} was the busiest UTC day by recorded token volume: {} across {} and {}.",
            day.name,
            noun(day.metrics.tokens(), "token", "tokens"),
            noun(day.metrics.turns, "turn", "turns"),
            noun(day.metrics.sessions, "chat", "chats")
        ));
    }
    let errors = report.tools.iter().max_by_key(|t| t.metrics.tool_errors);
    if let Some(tool) = errors.filter(|t| t.metrics.tool_errors > 0) {
        output.push(format!("{} returned {} in {}. Inspect those chats for repeated friction; a tool error alone does not imply the task failed.", tool.name, noun(tool.metrics.tool_errors, "error", "errors"), noun(tool.metrics.tool_results, "result", "results")));
    }
    if total.unpriced_usage_records > 0 || total.turns_without_usage > 0 {
        output.push(format!("Cost coverage is incomplete: {} no price and {} no usage record. {} is the known subtotal, not the total bill.", if total.unpriced_usage_records == 1 { "1 usage record has".to_owned() } else { format!("{} usage records have", count(total.unpriced_usage_records)) }, if total.turns_without_usage == 1 { "1 turn has".to_owned() } else { format!("{} turns have", count(total.turns_without_usage)) }, money(total.known_cost_usd)));
    }
    if let Some(file) = report.files.first() {
        output.push(format!("{} has the most recorded file changes: {}, +{} / −{} lines. These are edit activity counts, not a net Git diff.", file.name, noun(file.metrics.file_changes, "change", "changes"), count(file.metrics.additions), count(file.metrics.deletions)));
    }
    output.push(format!("Turn outcomes: {} completed, {} failed, {} interrupted and {} still queued/running. They count the turns you started (a subagent's turn is part of the one that started it) and describe execution status, not task quality.", count(total.completed), count(total.failed), count(total.interrupted), count(total.in_progress)));
    output
}
