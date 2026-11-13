//! Usage statistics (docs/USAGE-STATISTICS.md): daily counts of chats, turns,
//! tokens, models and tool calls, and what failed and why, shared with
//! Haarness Cloud unless turned off.
//!
//! What is sent is computed from the local history by `haarness_insights`,
//! one snapshot per UTC day for the last week. Cloud replaces a day's snapshot
//! instead of adding to it, so retries, several servers and restarts never
//! count anything twice.
use anyhow::{Context as _, Result, bail};
use chrono::{DateTime, Duration, Utc};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

mod scrub;

/// `0`, `false`, `off`, `no` or `disabled` turns usage statistics off;
/// `1`, `true`, `on` or `yes` turns them on whatever the settings say.
pub(crate) const ENV: &str = "HAARNESS_TELEMETRY";
const ENDPOINT: &str = "api/v1/telemetry/usage";
const WINDOW_DAYS: u32 = 7;
const UPLOAD_EVERY: Duration = Duration::hours(6);
const FIRST_CHECK_AFTER: std::time::Duration = std::time::Duration::from_secs(60);
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// The most failed turns a day reports (its latest), the most tool error rows
/// and versions, and how many characters of words each keeps.
const MAX_FAILURES_PER_DAY: usize = 50;
const MAX_TOOL_ERRORS_PER_DAY: usize = 40;
const MAX_VERSIONS_PER_DAY: usize = 10;
const FAILURE_WORDS: usize = 500;
const TOOL_ERROR_WORDS: usize = 300;

/// Tools whose errors are Haarness's own words about the call (with the path,
/// address or name it was about), reported scrubbed. Any other tool's error
/// can be a program's output, a page, a screen or someone else's server's
/// words, and only counts.
const TOOL_ERROR_WORDS_OF: &[&str] = &[
    "agent_switch",
    "apply_patch",
    "batch",
    "edit",
    "glob",
    "grep",
    "image_generate",
    "list",
    "load_tool",
    "lsp",
    "multiedit",
    "preview",
    "question",
    "read",
    "schedule",
    "session_wait",
    "skill",
    "StructuredOutput",
    "task",
    "task_abort",
    "todo",
    "todoread",
    "todowrite",
    "video_generate",
    "webfetch",
    "websearch",
    "workflow",
    "write",
];

/// Built-in tools reported by name. Any other tool (plugins, extensions, MCP
/// servers) is counted as `mcp` or `other`, so custom names never leave; a
/// name the model made up counts as `other` too.
const BUILTIN_TOOLS: &[&str] = &[
    "agent_manage",
    "agent_switch",
    "apply_patch",
    "bash",
    "bash_output",
    "bash_stop",
    "batch",
    "capability_manage",
    "cloud_agent_manage",
    "cloud_chat",
    "cloud_schedule",
    "cloud_workspace",
    "command_manage",
    "edit",
    "execute",
    "get_goal",
    "glob",
    "grep",
    "image_generate",
    "list",
    "load_tool",
    "lsp",
    "marketplace_manage",
    "mcp_manage",
    "mcp_resource_list",
    "mcp_resource_read",
    "memory_delete",
    "memory_list",
    "memory_read",
    "memory_save",
    "memory_search",
    "multiedit",
    "plugin_manage",
    "preview",
    "propose_goal_criteria",
    "question",
    "read",
    "schedule",
    "session_manage",
    "session_message",
    "session_rename",
    "session_wait",
    "skill",
    "skill_manage",
    "StructuredOutput",
    "task",
    "task_abort",
    "todo",
    "todoread",
    "todowrite",
    "update_goal",
    "video_generate",
    "webfetch",
    "websearch",
    "workflow",
    "write",
];

/// Providers Haarness ships, reported by name; a model only when Haarness
/// ships it too (`other` for fine-tunes and anything else). Other provider
/// ids are names people chose for their own gateways: reported as `custom`,
/// without the model. Local runtimes report `local` models, since a local
/// model's name can be private.
const NATIVE_PROVIDERS: &[&str] = &[
    "haarness",
    "openai",
    "anthropic",
    "gemini",
    "github-copilot",
    "xai",
    "azure",
    "google-vertex",
    "amazon-bedrock",
];
const LOCAL_PROVIDERS: &[&str] = &["ollama", "lmstudio"];
/// Native providers whose model ids are the account's own deployment names,
/// ARNs or endpoints: reported as `custom` models.
const DEPLOYMENT_PROVIDERS: &[&str] = &["azure", "google-vertex", "amazon-bedrock"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Source {
    Default,
    Settings,
    Environment,
    DoNotTrack,
}

/// Whether usage statistics are on, and what decided it.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Setting {
    pub(crate) enabled: bool,
    pub(crate) source: Source,
    /// False when an environment variable decides; the settings toggle cannot.
    pub(crate) editable: bool,
}

fn global_files(config_dir: &Path) -> [PathBuf; 2] {
    [
        config_dir.join("haarness.jsonc"),
        config_dir.join("haarness.json"),
    ]
}

/// Global configuration only: a project's configuration can never turn
/// usage statistics on (or off) for everyone using the machine.
fn configured(config_dir: &Path) -> Option<bool> {
    global_files(config_dir).iter().rev().find_map(|path| {
        crate::jsonconfig::read_json(path)
            .ok()
            .flatten()?
            .get("telemetry")?
            .get("usage")?
            .as_bool()
    })
}

pub(crate) fn setting(config_dir: &Path) -> Setting {
    let environment = std::env::var(ENV).ok().and_then(|value| {
        match value.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "off" | "no" | "disabled" => Some(false),
            "1" | "true" | "on" | "yes" | "enabled" => Some(true),
            _ => None,
        }
    });
    if let Some(enabled) = environment {
        return Setting {
            enabled,
            source: Source::Environment,
            editable: false,
        };
    }
    if std::env::var("DO_NOT_TRACK").is_ok_and(|value| matches!(value.trim(), "1" | "true")) {
        return Setting {
            enabled: false,
            source: Source::DoNotTrack,
            editable: false,
        };
    }
    configured(config_dir).map_or(
        Setting {
            enabled: true,
            source: Source::Default,
            editable: true,
        },
        |enabled| Setting {
            enabled,
            source: Source::Settings,
            editable: true,
        },
    )
}

pub(crate) fn set_enabled(config_dir: &Path, enabled: bool) -> Result<()> {
    fs::create_dir_all(config_dir)?;
    let target = global_files(config_dir)
        .into_iter()
        .find(|path| path.exists())
        .unwrap_or_else(|| config_dir.join("haarness.json"));
    crate::jsonconfig::write_entry(
        &target,
        &crate::jsonconfig::SaveScope::Global,
        "telemetry",
        "usage",
        Some(json!(enabled)),
    )
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    install_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_upload_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

fn directory(data: &Path) -> PathBuf {
    data.join("telemetry")
}

fn read_state(data: &Path) -> State {
    fs::read(directory(data).join("state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_state(data: &Path, state: &State) -> Result<()> {
    let directory = directory(data);
    fs::create_dir_all(&directory)?;
    let path = directory.join("state.json");
    let temporary = directory.join(format!("state.{}.tmp", std::process::id()));
    fs::write(&temporary, serde_json::to_vec_pretty(state)?)?;
    fs::rename(&temporary, &path)?;
    Ok(())
}

/// Serializes read-modify-write of `state.json` across processes.
fn with_state<T>(data: &Path, change: impl FnOnce(&mut State) -> T) -> Result<T> {
    let directory = directory(data);
    fs::create_dir_all(&directory)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("state.lock"))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let mut state = read_state(data);
    let value = change(&mut state);
    write_state(data, &state)?;
    Ok(value)
}

/// A random id for this installation, created the first time it is needed.
/// It identifies the computer's reports, never the person.
fn install_id(data: &Path) -> Result<String> {
    with_state(data, |state| {
        state
            .install_id
            .get_or_insert_with(|| uuid::Uuid::new_v4().to_string())
            .clone()
    })
}

fn os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        "windows" => "windows",
        _ => "other",
    }
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        _ => "other",
    }
}

/// Keeps a reported name to the characters and length Cloud accepts.
fn clean(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "._:/@+-".contains(*c))
        .take(100)
        .collect();
    if cleaned.is_empty() {
        "other".to_owned()
    } else {
        cleaned
    }
}

fn reported_model(provider: &str, model: &str) -> (String, String) {
    if DEPLOYMENT_PROVIDERS.contains(&provider) {
        (clean(provider), "custom".to_owned())
    } else if NATIVE_PROVIDERS.contains(&provider)
        || haarness_providers::compatible_preset(provider).is_some()
    {
        let model = if haarness_providers::ships_model(provider, model) {
            clean(model)
        } else {
            "other".to_owned()
        };
        (clean(provider), model)
    } else if LOCAL_PROVIDERS.contains(&provider) {
        (clean(provider), "local".to_owned())
    } else {
        ("custom".to_owned(), "custom".to_owned())
    }
}

/// A tool's reported name: Haarness's own by name (the browser's and
/// computer use's as their groups), every MCP tool as `mcp`, and anything
/// else (an extension's tool, a name a model made up) as `other`.
fn reported_tool(name: &str) -> &str {
    if BUILTIN_TOOLS.contains(&name) {
        name
    } else if name.starts_with("mcp__") {
        "mcp"
    } else if name.starts_with("browser_") {
        "browser"
    } else if name.starts_with("mac_") {
        "computer"
    } else {
        "other"
    }
}

/// The names a person chose that `provider` and `model` stand for when they
/// are reported as `custom`, `other` or `local`: taken out of an error's words.
fn private_names<'a>(
    provider: &'a str,
    model: &'a str,
    reported: &(String, String),
) -> Vec<&'a str> {
    let mut names = Vec::new();
    if reported.0 != provider {
        names.push(provider);
    }
    if reported.1 != model {
        names.push(model);
        // A model id with its owner (`acme/private-7b`) can be quoted by its
        // last part.
        if let Some((_, last)) = model.rsplit_once('/') {
            names.push(last);
        }
    }
    names
}

/// A version as Cloud takes it; `unknown` for turns recorded before versions
/// were.
fn reported_version(version: Option<&str>) -> String {
    let version: String = version
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "._+-".contains(*c))
        .take(64)
        .collect();
    if version.is_empty() {
        "unknown".to_owned()
    } else {
        version
    }
}

/// The day's latest failed turns: when, why (a kind from a closed list), on
/// which provider and model (named as [`reported_model`] names them) and
/// Haarness version, the turn's words scrubbed (see `scrub`), and how far it
/// got. A hook's words are the person's own script's and stay here.
fn reported_failures(rows: &[haarness_insights::FailedTurn]) -> Vec<Value> {
    let skip = rows.len().saturating_sub(MAX_FAILURES_PER_DAY);
    rows.iter()
        .skip(skip)
        .map(|row| {
            let reported = reported_model(&row.provider, &row.model);
            let message = if row.kind == haarness_insights::FailureKind::Hook {
                String::new()
            } else {
                scrub::words(
                    &row.error,
                    &private_names(&row.provider, &row.model, &reported),
                    FAILURE_WORDS,
                )
            };
            let (provider, model) = reported;
            json!({
                "turnId": row.id,
                "at": row.at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "kind": row.kind.as_str(),
                "provider": provider,
                "model": model,
                "version": reported_version(row.version.as_deref()),
                "message": (!message.is_empty()).then_some(message),
                "durationMs": row.duration_ms,
                "toolCalls": row.tool_calls,
                "toolErrors": row.tool_errors,
                "inputTokens": row.input_tokens,
                "outputTokens": row.output_tokens,
                "lastTool": row.last_tool.as_deref().map(reported_tool),
            })
        })
        .collect()
}

/// The day's tool errors in words: one row per tool, provider, model,
/// version and scrubbed words, with how many calls answered so, the most
/// common first. Tools outside [`TOOL_ERROR_WORDS_OF`] only count, in `tools`.
fn reported_tool_errors(rows: &[haarness_insights::ToolError]) -> Vec<Value> {
    // (tool, provider, model, version, words) and how many calls said them.
    type Key = (String, String, String, String, String);
    let mut grouped: Vec<(Key, u64)> = Vec::new();
    for row in rows {
        if !TOOL_ERROR_WORDS_OF.contains(&row.tool.as_str()) {
            continue;
        }
        let reported = reported_model(&row.provider, &row.model);
        let message = scrub::words(
            &row.output,
            &private_names(&row.provider, &row.model, &reported),
            TOOL_ERROR_WORDS,
        );
        if message.is_empty() {
            continue;
        }
        let key = (
            row.tool.clone(),
            reported.0,
            reported.1,
            reported_version(row.version.as_deref()),
            message,
        );
        match grouped.iter_mut().find(|(k, _)| *k == key) {
            Some((_, errors)) => *errors += 1,
            None => grouped.push((key, 1)),
        }
    }
    // Most common first; the same count keeps the day's order.
    grouped.sort_by(|a, b| b.1.cmp(&a.1));
    grouped
        .into_iter()
        .take(MAX_TOOL_ERRORS_PER_DAY)
        .map(|((tool, provider, model, version, message), errors)| {
            json!({
                "tool": tool,
                "provider": provider,
                "model": model,
                "version": version,
                "message": message,
                "errors": errors,
            })
        })
        .collect()
}

/// The day's turns by the Haarness version that ran them, the most turns
/// first.
fn reported_versions(rows: &[haarness_insights::VersionUsage]) -> Vec<Value> {
    let mut versions: Vec<(String, [u64; 4])> = Vec::new();
    for row in rows {
        let version = reported_version(row.version.as_deref());
        let counts = [row.turns, row.completed, row.failed, row.interrupted];
        match versions.iter_mut().find(|(v, _)| *v == version) {
            Some((_, total)) => {
                for (total, count) in total.iter_mut().zip(counts) {
                    *total += count;
                }
            }
            None => versions.push((version, counts)),
        }
    }
    versions.sort_by(|a, b| b.1[0].cmp(&a.1[0]).then_with(|| a.0.cmp(&b.0)));
    versions
        .into_iter()
        .take(MAX_VERSIONS_PER_DAY)
        .map(|(version, [turns, completed, failed, interrupted])| {
            json!({
                "version": version,
                "turns": turns,
                "completedTurns": completed,
                "failedTurns": failed,
                "interruptedTurns": interrupted,
            })
        })
        .collect()
}

/// A day's fields that a Haarness Cloud older than them refuses: it knows
/// every field it takes and refuses the rest.
const DETAIL_FIELDS: [&str; 3] = ["failures", "toolErrorMessages", "versions"];

/// Whether Haarness Cloud refused a report for its failure details, as one
/// older than them does: by a field's name, or by its size.
fn refuses_details(status: reqwest::StatusCode, refusal: &str) -> bool {
    status == reqwest::StatusCode::PAYLOAD_TOO_LARGE
        || (status == reqwest::StatusCode::BAD_REQUEST
            && DETAIL_FIELDS
                .iter()
                .any(|field| refusal.contains(&format!("unknown field `{field}`"))))
}

/// `report` without failure details, for a Haarness Cloud that doesn't take
/// them yet: everything else still counts.
fn without_details(report: &Value) -> Value {
    let mut report = report.clone();
    if let Some(days) = report["days"].as_array_mut() {
        for day in days {
            if let Some(day) = day.as_object_mut() {
                for field in DETAIL_FIELDS {
                    day.remove(field);
                }
            }
        }
    }
    report
}

fn has_details(report: &Value) -> bool {
    report["days"].as_array().is_some_and(|days| {
        days.iter()
            .any(|day| DETAIL_FIELDS.iter().any(|field| day.get(field).is_some()))
    })
}

fn cost(value: f64, priced: u64) -> Value {
    // No priced usage at all means the cost is unknown, not zero.
    if priced == 0 {
        Value::Null
    } else {
        json!((value * 1_000_000.0).round() / 1_000_000.0)
    }
}

/// The report exactly as it would be sent now.
pub(crate) fn report(data: &Path, now: DateTime<Utc>) -> Result<Value> {
    let days = haarness_insights::daily_usage(data, WINDOW_DAYS, now)?;
    let days: Vec<Value> = days
        .into_iter()
        .map(|day| {
            let total = &day.total;
            let mut models: Vec<(String, String, haarness_insights::Metrics)> = Vec::new();
            for row in &day.models {
                let (provider, model) = reported_model(&row.provider, &row.model);
                match models
                    .iter_mut()
                    .find(|(p, m, _)| *p == provider && *m == model)
                {
                    Some((_, _, metrics)) => add(metrics, &row.metrics),
                    None => models.push((provider, model, row.metrics.clone())),
                }
            }
            let mut tools: Vec<(String, u64, u64)> = Vec::new();
            for row in &day.tools {
                let name = reported_tool(&row.id).to_owned();
                match tools.iter_mut().find(|(n, _, _)| *n == name) {
                    Some((_, calls, errors)) => {
                        *calls += row.metrics.tool_calls;
                        *errors += row.metrics.tool_errors;
                    }
                    None => tools.push((name, row.metrics.tool_calls, row.metrics.tool_errors)),
                }
            }
            models.sort_by(|a, b| b.2.tokens().cmp(&a.2.tokens()).then_with(|| a.0.cmp(&b.0)));
            tools.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let failures = reported_failures(&day.failures);
            let tool_errors = reported_tool_errors(&day.tool_errors);
            let versions = reported_versions(&day.versions);
            let mut day = json!({
                "day": day.day,
                "chats": total.sessions,
                "turns": total.turns,
                "completedTurns": total.completed,
                "failedTurns": total.failed,
                "interruptedTurns": total.interrupted,
                "subagentTurns": day.subagent_turns,
                "scheduledTurns": day.scheduled_turns,
                "inputTokens": total.input_tokens,
                "outputTokens": total.output_tokens,
                "cacheReadTokens": total.cache_read_tokens,
                "cacheWriteTokens": total.cache_write_tokens,
                "costUsd": cost(total.known_cost_usd, total.priced_usage_records),
                "toolCalls": total.tool_calls,
                "toolErrors": total.tool_errors,
                "fileChanges": total.file_changes,
                "linesAdded": total.additions,
                "linesRemoved": total.deletions,
                "models": models.iter().take(50).map(|(provider, model, metrics)| json!({
                    "provider": provider,
                    "model": model,
                    "turns": metrics.turns,
                    "inputTokens": metrics.input_tokens,
                    "outputTokens": metrics.output_tokens,
                    "cacheReadTokens": metrics.cache_read_tokens,
                    "cacheWriteTokens": metrics.cache_write_tokens,
                    "costUsd": cost(metrics.known_cost_usd, metrics.priced_usage_records),
                })).collect::<Vec<_>>(),
                "tools": tools.iter().take(64).map(|(name, calls, errors)| json!({
                    "name": name, "calls": calls, "errors": errors,
                })).collect::<Vec<_>>(),
            });
            // Each is left out when it has nothing to say.
            for (field, rows) in [
                ("failures", failures),
                ("toolErrorMessages", tool_errors),
                ("versions", versions),
            ] {
                if !rows.is_empty() {
                    day[field] = Value::Array(rows);
                }
            }
            day
        })
        .collect();
    Ok(json!({
        "schemaVersion": 1,
        "installId": install_id(data)?,
        "client": {
            "version": env!("CARGO_PKG_VERSION"),
            "os": os(),
            "arch": arch(),
            "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        },
        "days": days,
    }))
}

fn add(target: &mut haarness_insights::Metrics, other: &haarness_insights::Metrics) {
    target.turns += other.turns;
    target.input_tokens += other.input_tokens;
    target.output_tokens += other.output_tokens;
    target.cache_read_tokens += other.cache_read_tokens;
    target.cache_write_tokens += other.cache_write_tokens;
    target.known_cost_usd += other.known_cost_usd;
    target.priced_usage_records += other.priced_usage_records;
}

fn endpoint() -> Result<reqwest::Url> {
    let config = haarness_account::Config::from_env()?;
    Ok(config.api_url.join(ENDPOINT)?)
}

/// The signed-in person's access token, refreshed when needed. Reports are
/// still sent without one.
async fn bearer() -> Option<String> {
    let config = haarness_account::Config::from_env().ok()?;
    if !config.signed_in() {
        return None;
    }
    let client = haarness_account::Client::new(config).ok()?;
    tokio::time::timeout(std::time::Duration::from_secs(10), client.bearer(None))
        .await
        .ok()?
        .ok()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Sent { days: usize, linked: bool },
    Off,
    NotDue,
    Busy,
    Nothing,
}

/// Sends the report when usage statistics are on and the last report is old
/// enough (`force` skips the age check).
pub(crate) async fn upload(data: &Path, config_dir: &Path, force: bool) -> Result<Outcome> {
    if !setting(config_dir).enabled {
        return Ok(Outcome::Off);
    }
    let state = read_state(data);
    let now = Utc::now();
    if !force
        && state
            .last_upload_at
            .is_some_and(|last| now - last < UPLOAD_EVERY)
    {
        return Ok(Outcome::NotDue);
    }
    fs::create_dir_all(directory(data))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory(data).join("upload.lock"))?;
    // Another server or command is sending right now.
    if fs2::FileExt::try_lock_exclusive(&lock).is_err() {
        return Ok(Outcome::Busy);
    }
    let report = report(data, now)?;
    let days = report["days"].as_array().map_or(0, Vec::len);
    if days == 0 {
        with_state(data, |state| state.last_upload_at = Some(now))?;
        return Ok(Outcome::Nothing);
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(concat!("haarness/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let url = endpoint()?;
    let token = bearer().await;
    let send = |report: &Value| {
        let mut request = client.post(url.clone()).json(report);
        if let Some(token) = &token {
            request = request.bearer_auth(token);
        }
        request.send()
    };
    let result = async {
        let mut response = send(&report).await.context("send usage statistics")?;
        let status = response.status();
        if !status.is_success() && has_details(&report) {
            if !refuses_details(status, &response.text().await.unwrap_or_default()) {
                bail!("Haarness Cloud answered {status}");
            }
            response = send(&without_details(&report))
                .await
                .context("send usage statistics")?;
        }
        let status = response.status();
        if !status.is_success() {
            bail!("Haarness Cloud answered {status}");
        }
        let body: Value = response.json().await.unwrap_or(Value::Null);
        Ok(body["linked"].as_bool().unwrap_or(false))
    }
    .await;
    match result {
        Ok(linked) => {
            with_state(data, |state| {
                state.last_upload_at = Some(now);
                state.last_error = None;
            })?;
            Ok(Outcome::Sent { days, linked })
        }
        Err(error) => {
            let message = format!("{error:#}");
            with_state(data, |state| state.last_error = Some(message.clone()))?;
            Err(error)
        }
    }
}

/// Reports from a long-lived server: a minute after start, then every half
/// hour (each attempt sends only when six hours have passed). Several servers
/// can run at once; the upload lock and the snapshot model keep them from
/// sending or counting twice.
pub(crate) fn spawn_uploader(data: PathBuf, config_dir: PathBuf) {
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_AFTER).await;
        loop {
            match upload(&data, &config_dir, false).await {
                Ok(Outcome::Sent { days, linked }) => {
                    tracing::info!(days, linked, "usage statistics sent");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::debug!(error = %format!("{error:#}"), "usage statistics not sent")
                }
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

/// Status for `haarness telemetry status`, Desktop and the TUI.
pub(crate) fn status(data: &Path, config_dir: &Path) -> Value {
    let state = read_state(data);
    json!({
        "setting": setting(config_dir),
        "lastSentAt": state.last_upload_at,
        "lastError": state.last_error,
        "endpoint": endpoint().map(|url| url.to_string()).ok(),
        "environmentVariable": ENV,
    })
}

/// `manageResource("usage-statistics", …)` for Desktop and the TUI view.
pub(crate) fn request(data: &Path, config_dir: &Path, input: &Value) -> Result<Value> {
    match input["action"].as_str().unwrap_or("get") {
        "get" | "view" => {}
        "set" => {
            let enabled = input["enabled"]
                .as_bool()
                .context("enabled must be true or false")?;
            set_enabled(config_dir, enabled)?;
        }
        "preview" => {
            let mut value = status(data, config_dir);
            value["report"] = report(data, Utc::now())?;
            return Ok(value);
        }
        other => bail!("Unknown usage statistics action {other:?}"),
    }
    Ok(status(data, config_dir))
}

#[derive(Args, Debug)]
pub struct TelemetryArgs {
    #[command(subcommand)]
    command: Option<TelemetryCommand>,
}

#[derive(Subcommand, Debug)]
enum TelemetryCommand {
    /// Show whether usage statistics are shared and when they were last sent
    Status,
    /// Share usage statistics
    On,
    /// Stop sharing usage statistics
    Off,
    /// Print exactly what would be sent now
    Preview,
    /// Send now instead of waiting for the next scheduled report
    Send,
}

pub(crate) async fn run(data: &Path, config_dir: &Path, args: &TelemetryArgs) -> Result<()> {
    let describe = |setting: Setting| match (setting.enabled, setting.source) {
        (true, Source::Environment) => format!("on ({ENV} turns them on)"),
        (false, Source::Environment) => format!("off ({ENV} turns them off)"),
        (false, Source::DoNotTrack) => "off (DO_NOT_TRACK is set)".to_owned(),
        (true, _) => "on".to_owned(),
        (false, _) => "off".to_owned(),
    };
    match args.command.as_ref().unwrap_or(&TelemetryCommand::Status) {
        TelemetryCommand::Status => {
            let state = read_state(data);
            println!("Usage statistics: {}", describe(setting(config_dir)));
            match (state.last_upload_at, &state.last_error) {
                (_, Some(error)) => println!("Last attempt failed: {error}"),
                (Some(at), None) => println!("Last sent: {}", at.format("%Y-%m-%d %H:%M UTC")),
                (None, None) => println!("Nothing sent yet."),
            }
            println!("See what is shared: haarness telemetry preview");
        }
        TelemetryCommand::On | TelemetryCommand::Off => {
            let enabled = matches!(args.command, Some(TelemetryCommand::On));
            set_enabled(config_dir, enabled)?;
            let now = setting(config_dir);
            if now.enabled == enabled {
                println!(
                    "Usage statistics are {}.",
                    if enabled { "on" } else { "off" }
                );
            } else {
                println!(
                    "Saved. Usage statistics stay {} in this shell: {}.",
                    describe(now),
                    if now.source == Source::DoNotTrack {
                        "DO_NOT_TRACK"
                    } else {
                        ENV
                    }
                );
            }
        }
        TelemetryCommand::Preview => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report(data, Utc::now())?)?
            );
        }
        TelemetryCommand::Send => match upload(data, config_dir, true).await? {
            Outcome::Sent { days, linked } => println!(
                "Sent {days} day{} of usage statistics{}.",
                if days == 1 { "" } else { "s" },
                if linked {
                    ", linked to your Haarness account"
                } else {
                    ""
                }
            ),
            Outcome::Off => println!("Usage statistics are off; nothing was sent."),
            Outcome::Busy => println!("Another Haarness process is sending right now."),
            Outcome::Nothing => {
                println!("No activity in the last {WINDOW_DAYS} days; nothing to send.")
            }
            Outcome::NotDue => {}
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_reported_only_for_what_haarness_ships() {
        assert_eq!(
            reported_model("anthropic", "claude-sonnet-5-5"),
            ("anthropic".into(), "claude-sonnet-5-5".into())
        );
        assert_eq!(
            reported_model("openrouter", "anthropic/claude-sonnet-4.5"),
            ("openrouter".into(), "anthropic/claude-sonnet-4.5".into())
        );
        assert_eq!(
            reported_model("xai", "grok-4.7"),
            ("xai".into(), "grok-4.7".into())
        );
        assert_eq!(
            reported_model("ollama", "acme-private-7b"),
            ("ollama".into(), "local".into())
        );
        assert_eq!(
            reported_model("acme-gateway", "secret"),
            ("custom".into(), "custom".into())
        );
        // Only models Haarness ships: fine-tunes, router models it does not
        // list and look-alikes are `other`; deployments and ARNs `custom`.
        for (provider, model) in [
            ("openai", "ft:gpt-4o-mini:acme-corp:project-falcon:AbC123"),
            ("openai", "gpt-5.4-acme-tuned"),
            ("openrouter", "a/b"),
            ("openrouter", "acme-secret-org/claude-opus-5-5"),
            ("fireworks", "accounts/acme/models/llama-finetune"),
        ] {
            assert_eq!(
                reported_model(provider, model),
                (provider.into(), "other".into()),
                "{provider}/{model}"
            );
        }
        for (provider, model) in [
            (
                "amazon-bedrock",
                "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/acmeprod",
            ),
            ("azure", "acme-internal-deployment"),
            (
                "google-vertex",
                "projects/acme/locations/us-central1/endpoints/42",
            ),
        ] {
            assert_eq!(
                reported_model(provider, model),
                (provider.into(), "custom".into()),
                "{provider}/{model}"
            );
        }
        assert_eq!(reported_tool("bash"), "bash");
        // Built-in tools outside the first list were counted as `other`,
        // multiedit's errors among them.
        for builtin in ["multiedit", "batch", "execute", "lsp", "workflow"] {
            assert_eq!(reported_tool(builtin), builtin);
        }
        // Haarness's own tools that only some turns are offered (Cloud,
        // memory, sessions, managers) were `other`, among made-up names.
        for builtin in [
            "cloud_schedule",
            "cloud_workspace",
            "memory_save",
            "session_wait",
            "skill_manage",
            "marketplace_manage",
            "mcp_resource_read",
        ] {
            assert_eq!(reported_tool(builtin), builtin);
        }
        assert_eq!(reported_tool("browser_navigate"), "browser");
        assert_eq!(reported_tool("mac_click"), "computer");
        assert_eq!(reported_tool("mcp__acme__deploy"), "mcp");
        assert_eq!(reported_tool("acme_release"), "other");
        assert_eq!(reported_tool("read_file"), "other");
        assert_eq!(clean("claude sonnet<script>"), "claudesonnetscript");
        assert_eq!(clean("   "), "other");
    }

    fn failed(
        kind: haarness_insights::FailureKind,
        provider: &str,
        model: &str,
        error: &str,
    ) -> haarness_insights::FailedTurn {
        haarness_insights::FailedTurn {
            id: "019a7c4e-0000-7000-8000-000000000001".into(),
            at: "2026-11-12T10:20:30.456Z".parse().unwrap(),
            kind,
            provider: provider.into(),
            model: model.into(),
            version: Some("0.1.22".into()),
            error: error.into(),
            duration_ms: 4_200,
            tool_calls: 3,
            tool_errors: 1,
            input_tokens: 12_000,
            output_tokens: 300,
            last_tool: Some("mcp__acme__deploy".into()),
        }
    }

    #[test]
    fn a_failed_turn_says_when_why_where_and_on_which_version() {
        use haarness_insights::FailureKind;
        let failures = reported_failures(&[failed(
            FailureKind::Quota,
            "gemini",
            "gemini-2.5-flash",
            "This API key has used up today's free allowance for Gemini 2.5 Flash on Gemini, which comes back at midnight Pacific time, in about 9 hours.",
        )]);
        assert_eq!(
            failures,
            [json!({
                "turnId": "019a7c4e-0000-7000-8000-000000000001",
                "at": "2026-11-12T10:20:30Z",
                "kind": "quota",
                "provider": "gemini",
                "model": "gemini-2.5-flash",
                "version": "0.1.22",
                "message": "This API key has used up today's free allowance for Gemini 2.5 Flash on Gemini, which comes back at midnight Pacific time, in about 9 hours.",
                "durationMs": 4_200,
                "toolCalls": 3,
                "toolErrors": 1,
                "inputTokens": 12_000,
                "outputTokens": 300,
                "lastTool": "mcp",
            })]
        );
    }

    #[test]
    fn a_failed_turn_s_words_lose_the_person_s_names_paths_and_hooks() {
        use haarness_insights::FailureKind;
        let mut older = failed(
            FailureKind::ProviderError,
            "acme-gateway",
            "acme/private-7b",
            "Could not reach acme-gateway: private-7b broke at /Users/sam/acme/main.rs on https://llm.acme.example.com/v1",
        );
        older.version = None;
        let rows = reported_failures(&[
            older,
            failed(
                FailureKind::Network,
                "ollama",
                "acme-private-7b",
                "Could not reach Ollama at http://localhost:11434: acme-private-7b isn't running",
            ),
            failed(
                FailureKind::Hook,
                "anthropic",
                "claude-sonnet-5-5",
                "A hook blocked this prompt: the ticket SECRET-123 is closed",
            ),
        ]);
        assert_eq!(
            (rows[0]["provider"].clone(), rows[0]["model"].clone()),
            (json!("custom"), json!("custom"))
        );
        assert_eq!(
            rows[0]["message"],
            "Could not reach <name>: <name> broke at <path> on <url>"
        );
        assert_eq!(rows[0]["version"], "unknown");
        assert_eq!(rows[1]["model"], "local");
        assert_eq!(
            rows[1]["message"],
            "Could not reach Ollama at http://localhost:11434: <name> isn't running"
        );
        assert_eq!(rows[2]["kind"], "hook");
        assert_eq!(rows[2]["message"], Value::Null, "a hook's words stay here");
        let all = serde_json::to_string(&rows).unwrap();
        for gone in ["acme", "sam", "SECRET"] {
            assert!(!all.contains(gone), "{gone} left in {all}");
        }
    }

    #[test]
    fn a_day_reports_its_latest_failures_only() {
        use haarness_insights::FailureKind;
        let rows: Vec<_> = (0..MAX_FAILURES_PER_DAY + 7)
            .map(|i| {
                let mut row = failed(FailureKind::Timeout, "openai", "gpt-5", "timed out");
                row.id = format!("turn-{i}");
                row
            })
            .collect();
        let reported = reported_failures(&rows);
        assert_eq!(reported.len(), MAX_FAILURES_PER_DAY);
        assert_eq!(reported[0]["turnId"], "turn-7");
        assert!(reported_failures(&[]).is_empty());
    }

    #[test]
    fn tool_errors_are_grouped_scrubbed_and_only_haarness_s_own_words() {
        let row =
            |tool: &str, provider: &str, model: &str, output: &str| haarness_insights::ToolError {
                tool: tool.into(),
                at: Utc::now(),
                provider: provider.into(),
                model: model.into(),
                version: Some("0.1.22".into()),
                output: output.into(),
            };
        let rows = reported_tool_errors(&[
            row(
                "edit",
                "anthropic",
                "claude-sonnet-5-5",
                "old was not found in src/app.rs; nothing was changed",
            ),
            row(
                "edit",
                "anthropic",
                "claude-sonnet-5-5",
                "old was not found in src/lib.rs; nothing was changed",
            ),
            row(
                "task",
                "gemini",
                "gemini-2.5-flash",
                "The subagent failed: Gemini is overloaded or unavailable right now",
            ),
            row(
                "bash",
                "gemini",
                "gemini-2.5-flash",
                "AWS_SECRET=abc make: *** [deploy] Error 1",
            ),
            row(
                "mcp__acme__deploy",
                "gemini",
                "gemini-2.5-flash",
                "acme says no",
            ),
            row(
                "browser_navigate",
                "gemini",
                "gemini-2.5-flash",
                "page text",
            ),
            row(
                "read",
                "acme-gateway",
                "acme-7b",
                "acme-7b asked for C:\\Users\\sam\\notes.txt, which is not there",
            ),
        ]);
        assert_eq!(
            rows,
            [
                json!({"tool": "edit", "provider": "anthropic", "model": "claude-sonnet-5-5", "version": "0.1.22",
                       "message": "old was not found in <path>; nothing was changed", "errors": 2}),
                json!({"tool": "task", "provider": "gemini", "model": "gemini-2.5-flash", "version": "0.1.22",
                       "message": "The subagent failed: Gemini is overloaded or unavailable right now", "errors": 1}),
                json!({"tool": "read", "provider": "custom", "model": "custom", "version": "0.1.22",
                       "message": "<name> asked for <path>, which is not there", "errors": 1}),
            ]
        );
        let many: Vec<_> = (0..MAX_TOOL_ERRORS_PER_DAY + 5)
            .map(|i| {
                row(
                    "webfetch",
                    "openai",
                    "gpt-5",
                    &format!("HTTP {} from the page", 400 + i),
                )
            })
            .collect();
        assert_eq!(reported_tool_errors(&many).len(), MAX_TOOL_ERRORS_PER_DAY);
    }

    #[test]
    fn turns_count_by_the_version_that_ran_them() {
        let row = |version: Option<&str>, turns, failed| haarness_insights::VersionUsage {
            version: version.map(Into::into),
            turns,
            completed: turns - failed,
            failed,
            interrupted: 0,
        };
        assert_eq!(
            reported_versions(&[
                row(None, 2, 1),
                row(Some("0.1.22"), 5, 2),
                row(Some(""), 1, 0)
            ]),
            [
                json!({"version": "0.1.22", "turns": 5, "completedTurns": 3, "failedTurns": 2, "interruptedTurns": 0}),
                json!({"version": "unknown", "turns": 3, "completedTurns": 2, "failedTurns": 1, "interruptedTurns": 0}),
            ]
        );
    }

    /// A Haarness Cloud older than failure details refuses them by name (or
    /// by the size they add); the report then goes again without them,
    /// whatever else it refused.
    #[test]
    fn an_older_cloud_gets_the_report_without_failure_details() {
        let report = json!({"schemaVersion": 1, "days": [
            {"day": "2026-11-10", "turns": 3, "failures": [{"kind": "quota"}], "toolErrorMessages": [], "versions": []},
            {"day": "2026-11-11", "turns": 1}
        ]});
        assert!(has_details(&report));
        let older = without_details(&report);
        assert!(!has_details(&older));
        assert_eq!(older["days"][0], json!({"day": "2026-11-10", "turns": 3}));
        assert_eq!(older["days"][1], report["days"][1]);
        let bad = reqwest::StatusCode::BAD_REQUEST;
        for field in DETAIL_FIELDS {
            assert!(refuses_details(
                bad,
                &format!(
                    r#"{{"code":"invalid_request","message":"Failed to deserialize the JSON body into the target type: days[0]: unknown field `{field}`, expected one of `day`, `chats`, `turns` at line 1 column 412"}}"#
                )
            ));
        }
        assert!(refuses_details(reqwest::StatusCode::PAYLOAD_TOO_LARGE, ""));
        assert!(!refuses_details(
            bad,
            r#"{"code":"invalid_request","message":"days[0].failures[0].model: must be 1 to 100 characters"}"#
        ));
        assert!(!refuses_details(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            ""
        ));
    }

    #[test]
    fn cost_is_unknown_without_priced_usage() {
        assert_eq!(cost(0.0, 0), Value::Null);
        assert_eq!(cost(1.234_567_89, 2), json!(1.234_568));
    }

    #[test]
    fn settings_live_in_the_global_configuration() {
        let directory =
            std::env::temp_dir().join(format!("haarness-usage-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&directory).unwrap();
        assert_eq!(configured(&directory), None);
        set_enabled(&directory, false).unwrap();
        assert_eq!(configured(&directory), Some(false));
        set_enabled(&directory, true).unwrap();
        assert_eq!(configured(&directory), Some(true));
        let data = directory.join("data");
        let id = install_id(&data).unwrap();
        assert_eq!(install_id(&data).unwrap(), id, "the id is stable");
        let _ = fs::remove_dir_all(&directory);
    }
}
