//! Usage statistics (docs/USAGE-STATISTICS.md): daily counts of chats, turns,
//! tokens, models and tool calls, shared with Haarness Cloud unless turned off.
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

/// `0`, `false`, `off`, `no` or `disabled` turns usage statistics off;
/// `1`, `true`, `on` or `yes` turns them on whatever the settings say.
pub(crate) const ENV: &str = "HAARNESS_TELEMETRY";
const ENDPOINT: &str = "api/v1/telemetry/usage";
const WINDOW_DAYS: u32 = 7;
const UPLOAD_EVERY: Duration = Duration::hours(6);
const FIRST_CHECK_AFTER: std::time::Duration = std::time::Duration::from_secs(60);
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Built-in tools reported by name. Any other tool (plugins, extensions, MCP
/// servers) is counted as `mcp` or `other`, so custom names never leave.
const BUILTIN_TOOLS: &[&str] = &[
    "apply_patch",
    "bash",
    "bash_output",
    "bash_stop",
    "edit",
    "get_goal",
    "glob",
    "grep",
    "list",
    "load_tool",
    "memory_read",
    "propose_goal_criteria",
    "question",
    "read",
    "session_manage",
    "skill",
    "StructuredOutput",
    "task",
    "todo",
    "todoread",
    "todowrite",
    "update_goal",
    "webfetch",
    "websearch",
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
    crate::jsonconfig::write_entry(&target, "telemetry", "usage", Some(json!(enabled)))
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

fn reported_tool(name: &str) -> &str {
    if BUILTIN_TOOLS.contains(&name) {
        name
    } else if name.starts_with("mcp__") {
        "mcp"
    } else {
        "other"
    }
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
            json!({
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
            })
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
    let mut request = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(concat!("haarness/", env!("CARGO_PKG_VERSION")))
        .build()?
        .post(endpoint()?)
        .json(&report);
    if let Some(token) = bearer().await {
        request = request.bearer_auth(token);
    }
    let result = async {
        let response = request.send().await.context("send usage statistics")?;
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
        assert_eq!(reported_tool("mcp__acme__deploy"), "mcp");
        assert_eq!(reported_tool("acme_release"), "other");
        assert_eq!(clean("claude sonnet<script>"), "claudesonnetscript");
        assert_eq!(clean("   "), "other");
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
