//! Read-only cross-workspace analytics. No provider calls or transcript export.
mod render;
pub use render::{render_html, render_text, write_report};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
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
            bail!("choose project or current-project, not both");
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
    title: String,
    project: String,
    project_name: String,
    model: String,
    created: DateTime<Utc>,
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
    let registry_path = data.join("workspaces.json");
    let registry: Registry = if registry_path.exists() {
        serde_json::from_slice(&std::fs::read(&registry_path)?)
            .context("read workspace registry")?
    } else {
        Registry::default()
    };
    let project_filter = if options.current_project {
        let path = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_owned());
        Some(
            registry
                .workspaces
                .iter()
                .filter(|w| path.starts_with(Path::new(&w.path)))
                .max_by_key(|w| Path::new(&w.path).components().count())
                .map(|w| w.project_id.clone())
                .context("current workspace is not registered; run a session here first")?,
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
            if project_filter.as_ref().is_some_and(|p| p != project) {
                continue;
            }
            let name = registry
                .projects
                .iter()
                .find(|p| p.id == project)
                .map_or_else(
                    || format!("Unregistered workspace {id}"),
                    |p| p.name.clone(),
                );
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
        match read_source(&source, since, now) {
            Ok((records, detached)) => {
                turns.extend(records);
                report.databases_read += 1;
                report.detached_items_skipped += detached;
            }
            Err(error) => report.warnings.push(format!(
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
            report.duplicate_turns_skipped += 1;
        }
        unique
    });
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
) -> Result<(Vec<RecordedTurn>, u64)> {
    let mut connection = history_connection(&source.path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    let tx = connection.transaction()?;
    let mut statement = tx.prepare("SELECT t.id,t.thread_id,h.title,t.provider,t.model,t.status,t.created_at FROM turns t JOIN threads h ON h.id=t.thread_id ORDER BY t.created_at,t.id")?;
    let rows = statement.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
        ))
    })?;
    let mut turns = BTreeMap::new();
    for row in rows {
        let (id, session, title, provider, model, status, date) = row?;
        let created = DateTime::parse_from_rfc3339(&date)?.with_timezone(&Utc);
        if created > now || since.is_some_and(|start| created < start) {
            continue;
        }
        let mut metrics = Metrics {
            turns: 1,
            ..Metrics::default()
        };
        match status.as_str() {
            "completed" => metrics.completed = 1,
            "failed" => metrics.failed = 1,
            "interrupted" => metrics.interrupted = 1,
            _ => metrics.in_progress = 1,
        }
        turns.insert(
            id.clone(),
            RecordedTurn {
                id,
                session,
                title,
                project: source.project.clone(),
                project_name: source.name.clone(),
                model: format!("{provider}/{model}"),
                created,
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
            }
            Some("tool_result") => {
                let name = data["name"].as_str().context("tool name missing")?;
                let tool = turn.tools.entry(name.into()).or_default();
                tool.tool_results += 1;
                turn.metrics.tool_results += 1;
                if data["is_error"].as_bool() == Some(true) {
                    tool.tool_errors += 1;
                    turn.metrics.tool_errors += 1;
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
    let total = &report.total;
    if total.turns == 0 {
        return vec![
            "No recorded turns match this scope. Run sessions or widen the date/project filters."
                .into(),
        ];
    }
    let mut output = Vec::new();
    if let Some(project) = report.projects.first() {
        output.push(format!("{} accounts for {} of {} recorded input/output tokens across {} active projects ({} turns).", project.name, project.metrics.tokens(), total.tokens(), report.projects.len(), project.metrics.turns));
    }
    if let Some(model) = report.models.first() {
        output.push(format!("{} is the largest token consumer: {} tokens across {} turns. Model attribution uses each turn's recorded model, including later model switches.", model.name, model.metrics.tokens(), model.metrics.turns));
    }
    if let Some(day) = report.days.iter().max_by_key(|day| day.metrics.tokens()) {
        output.push(format!("{} was the busiest UTC day by recorded token volume: {} tokens across {} turns and {} sessions. Open the session breakdown to inspect the larger conversations.", day.name, day.metrics.tokens(), day.metrics.turns, day.metrics.sessions));
    }
    let errors = report.tools.iter().max_by_key(|t| t.metrics.tool_errors);
    if let Some(tool) = errors.filter(|t| t.metrics.tool_errors > 0) {
        output.push(format!("{} returned {} errors in {} results. Inspect those sessions for repeated friction; a tool error alone does not imply the task failed.", tool.name, tool.metrics.tool_errors, tool.metrics.tool_results));
    }
    if total.unpriced_usage_records > 0 || total.turns_without_usage > 0 {
        output.push(format!("Cost coverage is incomplete: {} usage records have no price and {} turns have no usage record. ${:.4} is the known subtotal, not the total bill.", total.unpriced_usage_records, total.turns_without_usage, total.known_cost_usd));
    }
    if let Some(file) = report.files.first() {
        output.push(format!("{} has the most recorded file changes: {} changes, +{} / −{} lines. These are edit activity counts, not a net Git diff.", file.name, file.metrics.file_changes, file.metrics.additions, file.metrics.deletions));
    }
    output.push(format!("Turn outcomes: {} completed, {} failed, {} interrupted and {} still queued/running. These describe execution status, not task quality.", total.completed, total.failed, total.interrupted, total.in_progress));
    output
}
