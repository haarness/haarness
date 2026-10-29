//! Interactive client adapter. Forms carry revisions and use the same domain
//! operations as Desktop and CLI; the terminal never edits the host's files.
use super::*;
use anyhow::ensure;
use haarness_core::KeyCheck;
use haarness_protocol::{
    ManagementEntry as Entry, ManagementField as Field, ManagementView as View,
};
use haarness_providers::auth::{
    CopilotOAuthClient, Credential, CredentialStore, OpenAiOAuthClient, XaiOAuthClient,
};
use haarness_scheduler::{CreateSchedule, Scheduler, Trigger, UpdateSchedule};
use std::{future::Future, pin::Pin, sync::Mutex, time::Instant};
use uuid::Uuid;

type Completion<T = ()> = Pin<Box<dyn Future<Output = Result<T>> + Send>>;

pub(super) type Logins = BTreeMap<String, Login>;
pub(super) struct Login {
    created: Instant,
    task: tokio::task::JoinHandle<()>,
    result: Arc<Mutex<Option<std::result::Result<(), String>>>>,
    callback: Option<tokio::sync::oneshot::Sender<String>>,
    /// What the wait screen shows: the resource ("providers"/"mcp"), the
    /// target's id and display name, the method, and the URL/code to open.
    resource: String,
    target: String,
    name: String,
    method: String,
    url: String,
    code: String,
}

impl Login {
    /// The machine-readable half of every login view, so clients can poll
    /// without parsing prose.
    fn data(&self, id: &str, state: &str, error: Option<&str>) -> Value {
        json!({
            "id": id,
            "resource": self.resource,
            "provider": self.target,
            "name": self.name,
            "method": self.method,
            "url": self.url,
            "code": self.code,
            "state": state,
            "error": error,
        })
    }

    fn title(&self) -> String {
        format!("Sign in to {}", self.name)
    }

    fn instructions(&self) -> Vec<String> {
        // The bare URL first: generic panels turn a URL-only line into a link.
        let mut lines = vec![self.url.clone()];
        if !self.code.is_empty() {
            lines.push(format!("Code  {}", self.code));
        }
        lines.push(if self.code.is_empty() {
            "Finish signing in in your browser. This page updates by itself.".into()
        } else {
            "Enter the code in your browser. This page updates by itself.".into()
        });
        lines
    }
}
fn field(key: &str, label: &str, value: impl ToString, required: bool) -> Field {
    Field {
        key: key.into(),
        label: label.into(),
        value: value.to_string(),
        required,
        secret: false,
        choices: vec![],
    }
}
fn choice(key: &str, label: &str, value: &str, values: &[&str]) -> Field {
    Field {
        choices: values.iter().map(|v| (*v).into()).collect(),
        ..field(key, label, value, true)
    }
}
fn entry(label: impl Into<String>, description: impl Into<String>, request: Value) -> Entry {
    Entry {
        label: label.into(),
        description: description.into(),
        request,
        confirm: None,
    }
}
fn destructive(label: &str, request: Value) -> Entry {
    // The label already says what happens and the confirmation asks again;
    // a stock "This changes the selected resource" only added jargon.
    Entry {
        confirm: Some(format!("{label}?")),
        ..entry(label, "", request)
    }
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn form(title: &str, submit: Value, fields: Vec<Field>) -> View {
    View {
        title: title.into(),
        fields,
        submit: Some(submit),
        ..View::default()
    }
}
fn revision(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}
// What `/remote-control` says in clients that render the view as text.
const SHARE_SCAN: &str =
    "Scan the code or open the link on your phone or any browser signed in to Haarness Cloud.";
const SHARE_RUNS_HERE: &str = "The chat keeps running on this computer, with its files and model keys, until you stop sharing.";
const SHARE_OFFER: &str = "Continue this chat from your phone or any browser. It keeps running on this computer, with its files and model keys.";
const SHARE_STOPPED: &str = "Sharing was stopped from Haarness Cloud. Share again for a new link.";
const SHARE_SIGN_IN: &str =
    "Sign in to Haarness Cloud to continue this chat from your phone or any browser.";
/// A QR code as rows of `1` (dark) and `0` (light) modules, without the
/// quiet zone, so each client can draw it in its own medium.
pub(crate) fn qr_rows(text: &str) -> Vec<String> {
    let Ok(code) = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::M) else {
        return Vec::new();
    };
    code.to_colors()
        .chunks(code.width())
        .map(|row| {
            row.iter()
                .map(|module| {
                    if *module == qrcode::Color::Dark {
                        '1'
                    } else {
                        '0'
                    }
                })
                .collect()
        })
        .collect()
}
fn output(view: View) -> Result<Value> {
    Ok(serde_json::to_value(view)?)
}
/// Why a hook for `event` never runs: the values the event sends, none of
/// which its matcher matches.
fn never_runs_line(event: &str) -> String {
    match haarness_server::hooks::matcher_target(event) {
        haarness_server::hooks::MatcherTarget::Values(values) => format!(
            "Never runs: {event} matches only {}, and this matcher matches none of them.",
            values.join(", ")
        ),
        _ => "Never runs: its matcher matches nothing this event sends.".into(),
    }
}
fn row_request(action: &str, row: &Value) -> Value {
    json!({"action":action,"name":row["name"],"id":row["id"],"scope":row["scope"],"revision":row["revision"]})
}

/// `/usage-statistics` in the TUI; Desktop reads `data`.
fn usage_statistics_view(status: Value, input: &Value) -> View {
    let setting = &status["setting"];
    let enabled = setting["enabled"].as_bool().unwrap_or(false);
    let decided_by = match setting["source"].as_str() {
        Some("environment") => format!(" ({} decides)", crate::usage_stats::ENV),
        Some("doNotTrack") => " (DO_NOT_TRACK is set)".to_owned(),
        _ => String::new(),
    };
    let mut lines = vec![format!(
        "Sharing: {}{decided_by}",
        if enabled { "on" } else { "off" }
    )];
    if text(input, "action") == "preview" {
        // The header shows the first lines; the JSON follows under "Read everything".
        let days = status["report"]["days"].as_array().map_or(0, Vec::len);
        lines.push(if days == 0 {
            "No activity in the last 7 days yet, so there is nothing to send.".to_owned()
        } else {
            format!("The report covers {days} day{} with activity. Read everything to see it exactly as it would be sent.", if days == 1 { "" } else { "s" })
        });
        lines.push(String::new());
        lines.extend(
            serde_json::to_string_pretty(&status["report"])
                .unwrap_or_default()
                .lines()
                .map(str::to_owned),
        );
    } else {
        lines.push("Daily counts of chats, turns, tokens, models and tool calls. Never prompts, code, file names or paths.".to_owned());
        lines.push("Linked to your Haarness account when you are signed in.".to_owned());
        if let Some(sent) = status["lastSentAt"].as_str() {
            lines.push(format!("Last sent: {sent}"));
        }
    }
    let mut entries = Vec::new();
    if setting["editable"].as_bool().unwrap_or(false) {
        entries.push(entry(
            if enabled {
                "Stop sharing usage statistics"
            } else {
                "Share usage statistics"
            },
            "Applies to every project on this computer",
            json!({"action":"set","enabled":!enabled}),
        ));
    }
    entries.push(entry(
        "Show what is shared",
        "The report exactly as it would be sent",
        json!({"action":"preview"}),
    ));
    View {
        title: "Usage statistics".into(),
        lines,
        entries,
        data: Some(status),
        ..View::default()
    }
}

impl ManagementService {
    pub(super) async fn ui_request(&self, resource: &str, input: &Value) -> Result<Value> {
        if resource == "marketplace-ui" {
            return output(self.marketplace_view(input).await?);
        }
        if resource == "capabilities-ui" {
            return output(self.capabilities_view(input).await?);
        }
        if resource == "browser-settings" {
            let result = crate::browser_settings::request(
                self.trust_project,
                self.paths.config_dir(),
                &self.workspace,
                input,
            )?;
            return output(View {
                title: "Browser settings".into(),
                data: Some(result),
                ..View::default()
            });
        }
        if resource == "usage-statistics" {
            let result =
                crate::usage_stats::request(&self.paths.data, self.paths.config_dir(), input)?;
            return output(usage_statistics_view(result, input));
        }
        if resource == "input-settings" {
            let result = crate::input_settings::request(
                self.trust_project,
                self.paths.config_dir(),
                &self.workspace,
                input,
            )?;
            if result.get("title").is_some() {
                return Ok(result);
            }
            return output(View {
                title: "Input settings".into(),
                data: Some(result),
                ..View::default()
            });
        }
        let action = required_string(input, "action")?;
        if resource == "assets" {
            let kind = required_string(input, "kind")?;
            anyhow::ensure!(
                matches!(kind, "agent" | "command" | "skill"),
                "Unsupported definition type"
            );
            let result = self.desktop_asset(kind, input).await?;
            if !matches!(action, "get" | "list") {
                if let Ok(runtime) = self.runtime() {
                    self.refresh_runtime_assets(&runtime);
                    runtime.events().publish(
                        "assets.updated",
                        None,
                        None,
                        None,
                        json!({"resource":kind,"action":action}),
                    );
                    runtime.events().publish(
                        "configuration.reload_queued",
                        None,
                        None,
                        None,
                        json!({"resource":kind,"action":action}),
                    );
                }
            }
            return Ok(result);
        }
        if resource == "providers" && matches!(action, "router_form" | "router_connect") {
            if action == "router_form" {
                return output(router_form(input, false));
            }
            let result = crate::router_connection::connect(
                &self.paths,
                &self.workspace,
                self.trust_project,
                &crate::router_connection::ConnectRequest {
                    url: text(input, "url").into(),
                    name: text(input, "label").into(),
                    key: text(input, "key").into(),
                    project: text(input, "scope") == "project",
                },
            )
            .await?;
            return match result {
                crate::router_connection::ConnectResult::NeedsKey => output(router_form(input, true)),
                crate::router_connection::ConnectResult::Connected { name, models, .. } => output(View {
                    title: "Router connected".into(),
                    lines: vec![if models == 0 { format!("{name} · Self-hosted · no models yet") } else { format!("{name} · Self-hosted · {models} models available") },
                        if models == 0 { crate::router_connection::NO_MODELS_YET.into() } else { "Models refresh automatically. Open the model picker to choose one.".into() },
                        "Saved on this Haarness server. If a turn is running, the connection becomes available when it finishes.".into()],
                    entries: vec![entry("Connect another router", "", json!({"action":"router_form"})), entry("Back to providers", "", json!({"action":"view"}))],
                    ..View::default()
                }),
            };
        }
        if resource == "sharing" {
            return self.sharing_view(input).await.and_then(output);
        }
        if resource == "remote-control" {
            return self.remote_control_view(input).await.and_then(output);
        }
        if action == "login_callback_form" {
            let mut callback = field(
                "callback",
                "Callback URL from the browser address bar",
                "",
                true,
            );
            callback.secret = true;
            return output(form(
                "Complete remote authorization",
                json!({"action":"login_callback","id":input["id"]}),
                vec![callback],
            ));
        }
        if action == "login_callback" {
            let mut logins = self
                .logins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let login = logins
                .get_mut(required_string(input, "id")?)
                .context("Login expired; start again")?;
            let callback = required_string(input, "callback")?.to_owned();
            let sender = login
                .callback
                .take()
                .context("Callback already submitted")?;
            sender
                .send(callback)
                .map_err(|_| anyhow::anyhow!("Login already completed"))?;
            return output(View {
                title: "Authorization submitted".into(),
                entries: vec![entry(
                    "Check status",
                    "",
                    json!({"action":"login_status","id":input["id"]}),
                )],
                ..View::default()
            });
        }
        if action == "login_cancel" {
            let mut login = self
                .logins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(required_string(input, "id")?)
                .context("Login expired; start again")?;
            login.task.abort();
            let _ = (&mut login.task).await;
            let result = login
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let id = required_string(input, "id")?;
            let (title, state, error) = match &result {
                Some(Ok(())) => (format!("Connected {}", login.name), "connected", None),
                Some(Err(error)) => (
                    format!("Could not connect {}", login.name),
                    "failed",
                    Some(error.as_str()),
                ),
                None => ("Sign-in cancelled".to_owned(), "cancelled", None),
            };
            return output(View {
                title,
                data: Some(login.data(id, state, error)),
                lines: error
                    .map(|error| vec![format!("Error  {error}")])
                    .unwrap_or_default(),
                entries: vec![entry("Back", "", json!({"action":"view"}))],
                ..View::default()
            });
        }
        if action == "login_status" {
            let id = required_string(input, "id")?;
            let mut logins = self
                .logins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let login = logins.get(id).context("Login expired; start again")?;
            let result = login
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let mut view = View {
                title: login.title(),
                ..View::default()
            };
            match result {
                None => {
                    view.data = Some(login.data(id, "pending", None));
                    view.lines = login.instructions();
                    view.entries.push(entry(
                        "Check status",
                        "Refresh this page by hand",
                        input.clone(),
                    ));
                    view.entries.push(entry(
                        "Cancel sign-in",
                        "",
                        json!({"action":"login_cancel","id":id}),
                    ));
                }
                Some(Ok(())) => {
                    view.title = format!("Connected {}", login.name);
                    view.data = Some(login.data(id, "connected", None));
                    view.lines.push(if login.resource == "mcp" {
                        format!("{} is connected.", login.name)
                    } else {
                        format!(
                            "{} is connected. Open the model picker to choose a model.",
                            login.name
                        )
                    });
                    view.entries
                        .push(entry("Back", "", json!({"action":"view"})));
                    logins.remove(id);
                }
                Some(Err(error)) => {
                    view.title = format!("Could not connect {}", login.name);
                    view.data = Some(login.data(id, "failed", Some(&error)));
                    view.lines.push(format!("Error  {error}"));
                    view.entries.push(entry(
                        "Try again",
                        "",
                        json!({"action":"login","name":login.target,"method":login.method}),
                    ));
                    view.entries
                        .push(entry("Back", "", json!({"action":"view"})));
                    logins.remove(id);
                }
            }
            return output(view);
        }
        if action == "login" {
            return self.start_login(resource, input).await;
        }
        if resource == "schedules" && action == "history" {
            let rows = self
                .scheduler()?
                .history(required_string(input, "id")?, 50)?;
            return output(View {
                title: "Schedule run history".into(),
                lines: rows
                    .into_iter()
                    .map(|row| serde_json::to_string_pretty(&row))
                    .collect::<std::result::Result<Vec<_>, _>>()?,
                entries: vec![
                    entry("Refresh", "", input.clone()),
                    entry("Back", "", json!({"action":"inspect","id":input["id"]})),
                ],
                ..View::default()
            });
        }

        if matches!(
            action,
            "view" | "inspect" | "form" | "edit_form" | "restore_preview" | "connect"
        ) {
            return output(self.ui_view(resource, input).await?);
        }
        let mut warning = None;
        match resource {
            // Signing out of Haarness Cloud signs out of the account; there
            // is no provider key to remove.
            "providers" if action == "logout" && text(input, "name") == "haarness" => {
                haarness_account::Client::new(haarness_account::Config::from_env()?)?
                    .logout()
                    .await?;
            }
            "providers" if action == "credential" => {
                warning = self.check_credential(input).await?;
                self.provider_mutation(input)?;
            }
            "providers" => self.provider_mutation(input)?,
            "mcp" => {
                if action == "restart" {
                    self.mcp
                        .reconnect(required_string(input, "name")?)
                        .await
                        .map_err(core_error)?;
                } else {
                    let mut data = input.clone();
                    if let Some(args) = data["argsText"].as_str() {
                        data["args"] =
                            json!(args.lines().filter(|v| !v.is_empty()).collect::<Vec<_>>());
                    }
                    if let Some(scopes) = data["oauthScopesText"].as_str() {
                        data["oauthScopes"] = json!(
                            scopes
                                .split(',')
                                .map(str::trim)
                                .filter(|v| !v.is_empty())
                                .collect::<Vec<_>>()
                        );
                    }
                    self.mcp(&data).await?;
                }
            }
            "memory" => {
                self.desktop_memory(input)?;
            }
            "hooks" => {
                let mut data = input.clone();
                if let Some(timeout) = data["timeout"].as_str() {
                    data["timeout"] = json!(
                        timeout
                            .parse::<u64>()
                            .context("Timeout must be a whole number")?
                    );
                }
                crate::desktop_hooks::execute(
                    &self.paths,
                    &self.workspace,
                    self.trust_project,
                    &data,
                )?;
            }
            "schedules" => self.schedule_mutation(input)?,
            "profiles" => self.profile_mutation(input)?,
            _ => bail!("Unsupported management resource"),
        }
        if let Ok(runtime) = self.runtime() {
            self.refresh_runtime_assets(&runtime);
        }
        let mut view = self.ui_view(resource, &json!({"action":"view"})).await?;
        view.lines.insert(
            0,
            "Saved. Runtime configuration changes apply when active work is idle.".into(),
        );
        if let Some(warning) = warning {
            view.lines.insert(1, warning.clone());
            match view.data.as_mut().and_then(Value::as_object_mut) {
                Some(data) => {
                    data.insert("warning".into(), json!(warning));
                }
                None => view.data = Some(json!({ "warning": warning })),
            }
        }
        output(view)
    }

    /// Ask the provider about an API key before it is stored. A refusal is an
    /// error and nothing is saved; a key that could not be checked is saved
    /// with the returned warning.
    async fn check_credential(&self, input: &Value) -> Result<Option<String>> {
        let id = required_string(input, "name")?;
        let key = required_string(input, "key")?.trim();
        let provider = self
            .runtime()
            .ok()
            .and_then(|runtime| runtime.providers().get(id));
        let Some(provider) = provider.filter(|_| !key.is_empty()) else {
            return Ok(None);
        };
        let name = provider.info().name;
        Ok(match provider.check_api_key(key).await {
            KeyCheck::Accepted | KeyCheck::Unsupported => None,
            KeyCheck::Rejected(reason) => bail!(
                "{name} rejected this API key{}. Check that you copied all of it, then try again.",
                reason
                    .map(|reason| format!(" ({reason})"))
                    .unwrap_or_default()
            ),
            KeyCheck::Unchecked(reason) => Some(format!(
                "Saved, but the key could not be checked: {reason}. If chats then fail to authenticate, replace the key."
            )),
        })
    }

    async fn capabilities_view(&self, input: &Value) -> Result<View> {
        let action = text(input, "action");
        if action != "view" {
            self.capability_request(input).await?;
        }
        let catalog = self.capability_request(&json!({"action":"list"})).await?;
        let mut view=View {title:"Runtime capabilities".into(),lines:vec!["Changes persist and apply when active work is idle. Installed files are kept when disabled.".into()],..View::default()};
        for (group, kind) in [
            ("skills", "skill"),
            ("agents", "agent"),
            ("plugins", "plugin"),
            ("mcp", "mcp"),
            ("extensions", "extension"),
        ] {
            for row in catalog[group].as_array().into_iter().flatten() {
                let enabled = row["enabled"]
                    .as_bool()
                    .or_else(|| row["config"]["enabled"].as_bool())
                    .unwrap_or(true);
                view.entries.push(entry(format!("{} {} · {}",if enabled {"Disable"} else {"Enable"},text(row,"name"),kind),text(row,"description"),json!({"action":if enabled {"disable"} else {"enable"},"kind":kind,"name":row["name"],"scope":row.get("scope").cloned().unwrap_or(json!("local"))})));
            }
        }
        Ok(view)
    }

    async fn marketplace_view(&self, input: &Value) -> Result<View> {
        let action = text(input, "action");
        if action == "inspect" {
            let manifest = self
                .marketplace_request(
                    &json!({"action":"manifest","id":input["id"],"version":input["version"]}),
                )
                .await?;
            let resource = &manifest["resource"];
            let mut lines = vec![format!(
                "{} · version {} · {}",
                text(resource, "kind"),
                manifest["version"]["number"],
                text(resource, "publisher")
            )];
            // Say what the resource can do in words rather than raw JSON.
            let capabilities = &manifest["capabilities"];
            let listed = |key: &str| {
                capabilities[key]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .filter(|items| !items.is_empty())
            };
            for (flag, line) in [
                ("executable", "Runs programs on this computer"),
                ("shell", "Runs shell commands"),
                ("network", "Uses the network"),
            ] {
                if capabilities[flag] == true {
                    lines.push(line.into());
                }
            }
            for (key, label) in [
                ("mcp", "MCP servers"),
                ("hooks", "Hooks"),
                ("secrets", "Asks for"),
            ] {
                if let Some(items) = listed(key) {
                    lines.push(format!("{label}: {items}"));
                }
            }
            let mut fields = vec![choice(
                "scope",
                "Installation scope",
                "local",
                &["local", "global"],
            )];
            // One field per value an MCP server asks for; each installer
            // supplies their own, and secrets stay masked.
            for placeholder in manifest["install"]["mcp"]["env_placeholders"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let name = text(placeholder, "name");
                if name.is_empty() {
                    continue;
                }
                let description = text(placeholder, "description");
                let mut value = field(
                    &format!("value:{name}"),
                    &if description.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{name} · {description}")
                    },
                    "",
                    placeholder["required"] != false,
                );
                let upper = name.to_ascii_uppercase();
                value.secret = ["KEY", "TOKEN", "SECRET", "PASSWORD"]
                    .iter()
                    .any(|word| upper.contains(word));
                fields.push(value);
            }
            return Ok(View {
                title: format!("Install {}", text(resource, "name")),
                lines,
                fields,
                // The version shown is the one installed. Picked from the
                // listing (the latest), it follows new versions afterwards.
                submit: Some(
                    json!({"action":"install","id":input["id"],"version":manifest["version"]["number"],"autoUpdate":input["version"].is_null()}),
                ),
                ..View::default()
            });
        }
        let mut notice = None;
        if matches!(
            action,
            "install" | "update" | "enable" | "disable" | "remove" | "auto_update"
        ) {
            let mut request = input.clone();
            if let Some(values) = input["valuesText"].as_str() {
                request["values"] = serde_json::from_str(values)?;
            }
            for (key, value) in input.as_object().into_iter().flatten() {
                if let Some(name) = key.strip_prefix("value:") {
                    request["values"][name] = value.clone();
                }
            }
            self.marketplace_request(&request).await?;
            notice = Some(match action {
                // Only the receipt changes, so nothing waits for idle.
                "auto_update" if input["enabled"] == true => {
                    format!("{} now updates automatically.", text(input, "name"))
                }
                "auto_update" => format!(
                    "{} stays on this version until you update it.",
                    text(input, "name")
                ),
                _ => {
                    let done = match action {
                        "install" => "Installed",
                        "update" => "Updated",
                        "enable" => "Turned on",
                        "disable" => "Turned off",
                        _ => "Removed",
                    };
                    format!("{done}. Changes apply when active work is idle.")
                }
            });
        }
        let mut view=View {title:"Marketplace".into(),fields:vec![field("q","Search marketplace",text(input,"q"),false),choice("kind","Resource type",input["kind"].as_str().filter(|kind| !kind.is_empty()).unwrap_or("all"),&["all","skill","agent","mcp","plugin","extension"])],submit:Some(json!({"action":"search"})),lines:vec!["Public resources are visible to everyone. Private resources belong to your Haarness account and its members.".into()],..View::default()};
        if let Some(notice) = notice {
            view.lines.insert(0, notice);
        }
        view.entries.push(entry(
            "Installed resources",
            "Manage versions and availability",
            json!({"action":"installed"}),
        ));
        // Changing an installed resource stays on the installed list, where
        // its new state shows; an install goes back to browsing.
        if matches!(
            action,
            "installed" | "enable" | "disable" | "update" | "remove" | "auto_update"
        ) {
            let installed = self
                .marketplace_request(&json!({"action":"installed"}))
                .await?;
            if installed.as_array().is_none_or(Vec::is_empty) {
                view.lines
                    .push("Nothing is installed from the marketplace yet.".into());
            }
            for row in installed.as_array().into_iter().flatten() {
                let name = text(row, "name");
                let enabled = row["enabled"] != false;
                let automatic = crate::marketplace::follows_new_versions(row);
                let version = &row["version"]["number"];
                // The version once, and how it changes: on its own, held
                // back by local edits, or only when asked.
                let details = format!(
                    "{} · {} · {} · {}{}",
                    text(row, "kind"),
                    if row["scope"] == "local" {
                        "this project"
                    } else {
                        "all projects"
                    },
                    if automatic {
                        format!("version {version}")
                    } else {
                        format!("pinned to version {version}")
                    },
                    if enabled { "on" } else { "off" },
                    match (automatic, row["updateHeld"].is_string()) {
                        (true, true) => " · has local edits, so automatic updates wait",
                        (true, false) => " · updates automatically",
                        _ => "",
                    }
                );
                let request = |action: &str| json!({"action":action,"id":row["id"],"scope":row["scope"],"name":row["name"]});
                // Only the toggle that applies: the state is in the description.
                let toggle = if enabled { "disable" } else { "enable" };
                view.entries.push(entry(
                    format!("{} {name}", if enabled { "Disable" } else { "Enable" }),
                    details,
                    request(toggle),
                ));
                view.entries.push(entry(
                    format!("Update {name}"),
                    "Install the latest version",
                    request("update"),
                ));
                let mut follow = request("auto_update");
                follow["enabled"] = json!(!automatic);
                view.entries.push(if automatic {
                    entry(
                        format!("Stop automatic updates for {name}"),
                        format!("Stay on version {version}"),
                        follow,
                    )
                } else {
                    entry(
                        format!("Update {name} automatically"),
                        "Install new versions when they are published",
                        follow,
                    )
                });
                let mut remove = entry(format!("Remove {name}"), "Uninstall it", request("remove"));
                remove.confirm = Some(format!("Remove {name}?"));
                view.entries.push(remove);
            }
        } else {
            // Offline or Cloud down: say so, and keep installed resources reachable.
            // "all" (the form's default) searches every kind.
            let kind = input["kind"]
                .as_str()
                .filter(|kind| !matches!(*kind, "" | "all"));
            match self
                .marketplace_request(
                    &json!({"action":"search","q":input["q"],"kind":kind,"cursor":input["cursor"]}),
                )
                .await
            {
                Ok(page) => {
                    for row in page["items"].as_array().into_iter().flatten() {
                        view.entries.push(entry(
                            format!(
                                "{} · {} · {}",
                                text(row, "name"),
                                text(row, "kind"),
                                text(row, "visibility")
                            ),
                            text(row, "summary"),
                            json!({"action":"inspect","id":row["id"]}),
                        ));
                    }
                    if page["nextCursor"].is_string() {
                        view.entries.push(entry("Next page","",json!({"action":"search","q":input["q"],"kind":kind,"cursor":page["nextCursor"]})));
                    }
                }
                Err(error) => {
                    view.lines.pop();
                    view.lines.push(error.to_string());
                    view.entries.push(entry(
                        "Try again",
                        "",
                        json!({"action":"search","q":input["q"],"kind":kind}),
                    ));
                }
            }
        }
        Ok(view)
    }

    async fn sharing_view(&self, input: &Value) -> Result<View> {
        use haarness_server::sharing::Snapshot;
        let thread: ThreadId = serde_json::from_value(input["threadId"].clone())?;
        let runtime = self.runtime()?;
        let current = runtime.get_thread(thread).await?;
        let action = text(input, "action");
        if action == "preview" {
            let archive = runtime.export_session_archive(thread, false).await?;
            let preview =
                Snapshot::prepare(&self.paths.data, &archive, input["includeTools"] == true)?;
            let messages = &preview.messages;
            return Ok(View {
                title: "Review shared snapshot".into(),
                lines: messages
                    .iter()
                    .map(|m| format!("{}: {}", m.role, m.text))
                    .collect(),
                entries: vec![destructive(
                    "Create shared link",
                    json!({"action":"create","threadId":thread,"preview":preview.token}),
                )],
                ..View::default()
            });
        }
        if action == "create" {
            let snapshot = Snapshot::publish(&self.paths.data, required_string(input, "preview")?)?;
            return Ok(View {
                title: "Shared link created".into(),
                lines: vec![
                    format!("/s/{}", snapshot.token),
                    format!("Expires {}", snapshot.expires_at),
                ],
                entries: vec![destructive(
                    "Revoke link",
                    json!({"action":"revoke","token":snapshot.token,"threadId":thread}),
                )],
                ..View::default()
            });
        }
        if action == "revoke" {
            Snapshot::revoke(&self.paths.data, required_string(input, "token")?)?;
        }
        let mut view=View{title:format!("Share · {}",current.title),lines:vec!["Publishes a read-only snapshot on this server. Anyone who can reach the server can open its link until it expires or you revoke it.".into(),"Only the text in the preview is published. Review it for secrets before you create a link.".into()],entries:vec![entry("Review conversation snapshot","Messages only · expires in seven days",json!({"action":"preview","threadId":thread,"includeTools":false})),entry("Review with tool details","Includes commands and tool outputs",json!({"action":"preview","threadId":thread,"includeTools":true}))],..View::default()};
        for row in Snapshot::list(&self.paths.data, thread)? {
            view.lines.push(format!("/s/{}", row.token));
            view.entries.push(destructive(
                "Revoke existing link",
                json!({"action":"revoke","threadId":thread,"token":row.token}),
            ));
        }
        Ok(view)
    }

    /// `/remote-control` in the TUI and Desktop: continue this chat from a
    /// phone or browser through Haarness Cloud. `status` answers from local
    /// state only; `view` also asks Cloud whether it reaches the chat.
    async fn remote_control_view(&self, input: &Value) -> Result<View> {
        use crate::remote_cloud::{self, ShareState};
        let thread: ThreadId =
            serde_json::from_value(input["threadId"].clone()).context("Open a chat first")?;
        let current = self.runtime()?.get_thread(thread).await?;
        let action = text(input, "action");
        let share = match action {
            "start" => {
                ensure!(
                    !current.is_subagent,
                    "Share the conversation that started this agent; its agents come with it."
                );
                remote_cloud::share_chat(&self.paths, &self.workspace, thread.0).await?
            }
            "stop" => remote_cloud::stop_chat(&self.paths, &self.workspace, thread.0).await?,
            "status" => {
                remote_cloud::chat_status(&self.paths, &self.workspace, thread.0, false).await?
            }
            "" | "view" => {
                remote_cloud::chat_status(&self.paths, &self.workspace, thread.0, true).await?
            }
            other => bail!("Unknown remote control action {other}"),
        };
        let request = |action: &str| json!({"action":action,"threadId":thread});
        let mut view = View {
            title: "Remote control".into(),
            ..View::default()
        };
        match share.state {
            ShareState::On | ShareState::Connecting => {
                view.lines.extend(share.url.clone());
                view.lines.push(if share.state == ShareState::On {
                    SHARE_SCAN.into()
                } else {
                    "Connecting to Haarness Cloud…".into()
                });
                view.lines.push(SHARE_RUNS_HERE.into());
                view.entries
                    .push(destructive("Stop sharing", request("stop")));
            }
            ShareState::Off | ShareState::Stopped => {
                let stopped = share.state == ShareState::Stopped;
                view.lines
                    .push(if stopped { SHARE_STOPPED } else { SHARE_OFFER }.into());
                view.entries.push(entry(
                    "Share this chat",
                    "Get a link for your phone or browser",
                    request("start"),
                ));
            }
            ShareState::SignedOut => {
                view.lines.push(SHARE_SIGN_IN.into());
                view.entries.push(entry(
                    "Sign in to Haarness Cloud",
                    "",
                    json!({"action":"login","name":"haarness","method":"browser"}),
                ));
            }
        }
        if let Some(error) = &share.error {
            view.lines.push(error.clone());
        }
        let mut data = serde_json::to_value(&share)?;
        data["chat"] = json!(current.title);
        data["qr"] = json!(share.url.as_deref().map(qr_rows).unwrap_or_default());
        view.data = Some(data);
        Ok(view)
    }

    async fn ui_view(&self, resource: &str, input: &Value) -> Result<View> {
        match resource {
            "providers" => self.provider_view(input),
            "mcp" => self.mcp_view(input).await,
            "memory" => self.memory_view(input),
            "hooks" => self.hooks_view(input),
            "schedules" => self.schedule_view(input),
            "profiles" => self.profile_view(input),
            _ => bail!("Unsupported management resource"),
        }
    }

    fn provider_view(&self, input: &Value) -> Result<View> {
        let id = text(input, "name");
        let providers = self.runtime()?.providers().list();
        match text(input, "action") {
            "form" if id.is_empty() => Ok(form(
                "Custom endpoint",
                json!({"action":"add"}),
                vec![
                    field("name", "Provider ID", "", true),
                    choice("scope", "Scope", "global", &["global", "project"]),
                    field("label", "Display name", "", true),
                    field("url", "Endpoint URL (HTTPS or local HTTP)", "", true),
                    choice(
                        "protocol",
                        "Protocol",
                        "openai-chat-completions",
                        &[
                            "openai-chat-completions",
                            "openai-responses",
                            "anthropic-messages",
                        ],
                    ),
                    field("models", "Model IDs (comma separated)", "", true),
                    choice(
                        "requiresAuth",
                        "Requires authentication",
                        "true",
                        &["true", "false"],
                    ),
                ],
            )),
            "edit_form" => {
                let (_, path, section) = self.provider_source(input)?;
                Ok(form(
                    "Edit custom endpoint",
                    json!({"action":"edit","name":id,"scope":input["scope"],"source":path,"revision":revision(&path)?}),
                    vec![
                        field(
                            "label",
                            "Display name",
                            section.name.as_deref().unwrap_or(id),
                            true,
                        ),
                        field(
                            "url",
                            "Endpoint URL (HTTPS or local HTTP)",
                            section.base_url.as_deref().unwrap_or(""),
                            true,
                        ),
                        field(
                            "models",
                            "Model IDs (optional; empty discovers automatically)",
                            section.models.join(", "),
                            false,
                        ),
                    ],
                ))
            }
            "form" => {
                let name = provider_name(&providers, id);
                let guide = haarness_providers::connect::guide(id);
                let label = guide
                    .methods
                    .iter()
                    .find(|method| method.kind == haarness_providers::connect::MethodKind::Api)
                    .map_or("API key", |method| key_field_label(method.label));
                let mut key = field("key", label, "", true);
                key.secret = true;
                let mut lines = Vec::new();
                if let Some(url) = guide.key_url {
                    lines.push(format!("Create a key  {url}"));
                }
                if !guide.env.is_empty() {
                    lines.push(format!("Or set  {}", guide.env.join(" / ")));
                }
                if let Some(note) = guide.note {
                    lines.push(note.to_owned());
                }
                lines.push("Stored in owner-only auth.json on this Haarness server.".into());
                Ok(View {
                    lines,
                    ..form(
                        &format!("Connect {name}"),
                        json!({"action":"credential","name":id}),
                        vec![key],
                    )
                })
            }
            "connect" => Ok(self.connect_view(&providers, id)),
            "inspect" => {
                ensure!(providers.iter().any(|p| p.id == id), "Unknown provider");
                let info = providers.iter().find(|p| p.id == id);
                let name = provider_name(&providers, id);
                let store = CredentialStore::new(self.paths.auth.clone());
                let stored = store.get(id).ok().flatten();
                let guide = haarness_providers::connect::guide(id);
                let connected = info.is_some_and(|p| p.authenticated);
                let models = info.map_or(0, |p| p.models.len());
                let mut view = View {
                    title: name.clone(),
                    lines: vec![match (&stored, connected) {
                        (None, true) if id == "haarness" => {
                            format!("Status  Connected · signed in · {models} models")
                        }
                        (Some(credential), _) if credential.kind() == "oauth" => {
                            format!("Status  Connected · signed in · {models} models")
                        }
                        (Some(_), _) => format!("Status  Connected · API key · {models} models"),
                        (None, true) if guide.methods.is_empty() => {
                            format!("Status  Connected · {models} models")
                        }
                        (None, true) => {
                            format!("Status  Connected · from the environment · {models} models")
                        }
                        (None, false) => "Status  Not connected".into(),
                    }],
                    ..View::default()
                };
                for method in &guide.methods {
                    match method.kind {
                        haarness_providers::connect::MethodKind::Oauth => view.entries.push(entry(
                            method.label,
                            method.description,
                            json!({"action":"login","name":id,"method":method.id}),
                        )),
                        haarness_providers::connect::MethodKind::Api => view.entries.push(entry(
                            if stored.is_some() {
                                replace_label(method.label)
                            } else {
                                method.label.to_owned()
                            },
                            method.description,
                            json!({"action":"form","name":id}),
                        )),
                    }
                }
                if let Some(note) = guide.note {
                    view.lines.push(note.to_owned());
                }
                if stored.is_some() {
                    view.entries.push(destructive(
                        "Disconnect",
                        json!({"action":"logout","name":id}),
                    ));
                } else if id == "haarness" && connected {
                    view.entries.push(Entry {
                        description: "Cloud models stop here until you sign in again".into(),
                        ..destructive("Sign out of Haarness", json!({"action":"logout","name":id}))
                    });
                }
                for (scope, path, section) in self.provider_sources(id)? {
                    let request =
                        json!({"name":id,"scope":scope,"source":path,"revision":revision(&path)?});
                    let mut edit = request.clone();
                    edit["action"] = json!("edit_form");
                    view.entries.push(entry(
                        format!("Edit {scope} provider"),
                        section.name.unwrap_or_default(),
                        edit,
                    ));
                    let mut remove = request;
                    remove["action"] = json!("remove");
                    view.entries
                        .push(destructive(&format!("Remove {scope} provider"), remove));
                }
                Ok(view)
            }
            _ => {
                let mut sorted: Vec<_> = providers.iter().collect();
                sorted.sort_by_key(|p| {
                    (
                        !p.authenticated,
                        haarness_providers::connect::rank(&p.id, &p.name),
                    )
                });
                Ok(View {
                    title: "Providers".into(),
                    entries: std::iter::once(entry(
                        "Connect a provider",
                        "Sign in or add an API key",
                        json!({"action":"connect"}),
                    ))
                    .chain(std::iter::once(entry(
                        "Connect Haarness Router",
                        "Self-hosted · address first, access key only if needed",
                        json!({"action":"router_form"}),
                    )))
                    .chain(std::iter::once(entry(
                        "Add a custom endpoint",
                        "Any OpenAI-, Anthropic- or Gemini-compatible API",
                        json!({"action":"form"}),
                    )))
                    .chain(sorted.into_iter().map(|p| {
                        entry(
                            p.name.clone(),
                            provider_status(p),
                            json!({"action":"inspect","name":p.id}),
                        )
                    }))
                    .collect(),
                    ..View::default()
                })
            }
        }
    }

    /// The connect picker's catalog: every provider with how it can be
    /// connected, popular ones first. With a `name`, only that provider.
    fn connect_view(&self, providers: &[haarness_protocol::ProviderInfo], only: &str) -> View {
        let store = CredentialStore::new(self.paths.auth.clone());
        let mut rows: Vec<_> = providers
            .iter()
            .filter(|p| only.is_empty() || p.id == only)
            .collect();
        rows.sort_by_key(|p| haarness_providers::connect::rank(&p.id, &p.name));
        let catalog: Vec<Value> = rows
            .iter()
            .map(|p| {
                let guide = haarness_providers::connect::guide(&p.id);
                let stored = store.get(&p.id).ok().flatten();
                let source = match (&stored, p.authenticated, guide.methods.is_empty()) {
                    // Cloud is signed in through the Haarness account, not a stored key.
                    (None, true, _) if p.id == "haarness" => Some("oauth"),
                    (Some(credential), _, _) => Some(credential.kind()),
                    (None, true, true) => Some("local"),
                    (None, true, false) => Some("environment"),
                    (None, false, _) => None,
                };
                json!({
                    "id": p.id,
                    "name": p.name,
                    "connected": p.authenticated,
                    "models": p.models.len(),
                    "source": source,
                    "methods": guide.methods,
                    "keyUrl": guide.key_url,
                    "env": guide.env,
                    "note": guide.note,
                    "popular": guide.popular,
                })
            })
            .collect();
        View {
            title: "Connect a provider".into(),
            data: Some(json!({"providers": catalog})),
            entries: rows
                .iter()
                .map(|p| {
                    entry(
                        p.name.clone(),
                        provider_status(p),
                        json!({"action":"inspect","name":p.id}),
                    )
                })
                .collect(),
            ..View::default()
        }
    }
    fn provider_sources(&self, id: &str) -> Result<Vec<(String, PathBuf, crate::ProviderSection)>> {
        let mut entries = Vec::new();
        for project in [false, true] {
            if project && !self.trust_project {
                continue;
            }
            for path in crate::scoped_config_files(&self.paths, &self.workspace, project) {
                if !path.exists() {
                    continue;
                }
                let root: Value = if path.extension().is_some_and(|e| e == "toml") {
                    serde_json::to_value(toml::from_str::<toml::Value>(&fs::read_to_string(
                        &path,
                    )?)?)?
                } else {
                    crate::jsonconfig::read_json(&path)
                        .map_err(anyhow::Error::msg)?
                        .unwrap_or_default()
                };
                if let Some(value) = root.get("providers").and_then(|p| p.get(id)) {
                    let section = serde_json::from_value(value.clone())?;
                    if !entries.iter().any(|(_, existing, _)| existing == &path) {
                        entries.push((
                            if project { "project" } else { "global" }.into(),
                            path,
                            section,
                        ));
                    }
                }
            }
        }
        Ok(entries)
    }
    fn provider_source(&self, input: &Value) -> Result<(String, PathBuf, crate::ProviderSection)> {
        self.provider_sources(required_string(input, "name")?)?
            .into_iter()
            .find(|(scope, path, _)| {
                input["scope"] == *scope && input["source"].as_str() == path.to_str()
            })
            .context("Provider source changed; reopen the provider")
    }
    fn provider_mutation(&self, input: &Value) -> Result<()> {
        let id = required_string(input, "name")?;
        crate::validate_provider_id(id)?;
        let store = CredentialStore::new(self.paths.auth.clone());
        let _lock = haarness_profiles::configuration_lock(&self.paths.data)?;
        match text(input, "action") {
            "credential" => {
                let key = required_string(input, "key")?;
                ensure!(!key.trim().is_empty(), "Enter an API key");
                store.set(id, &Credential::api(key.to_owned()))?;
            }
            "logout" => {
                store.remove(id)?;
            }
            "add" => {
                ensure!(
                    !haarness_providers::builtin_provider(id),
                    "{id} is a built-in provider. Give your endpoint its own ID, for example {id}-proxy"
                );
                let url = required_string(input, "url")?;
                ensure!(
                    crate::valid_provider_url(url),
                    "Use HTTPS, or HTTP on a loopback address"
                );
                let protocol = required_string(input, "protocol")?;
                haarness_providers::CompatibleProtocol::parse(protocol)
                    .map_err(anyhow::Error::msg)?;
                let models = required_string(input, "models")?
                    .split(',')
                    .map(str::trim)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                ensure!(
                    !models.is_empty() && models.iter().all(|s| !s.is_empty()),
                    "Enter model IDs"
                );
                let current = crate::load_config(&self.paths, &self.workspace, self.trust_project)?;
                ensure!(
                    !current.providers.contains_key(id),
                    "Provider already exists"
                );
                let section = crate::ProviderSection {
                    protocol: protocol.into(),
                    name: Some(required_string(input, "label")?.into()),
                    base_url: Some(url.into()),
                    models,
                    requires_auth: text(input, "requiresAuth") != "false",
                    ..crate::ProviderSection::default()
                };
                let project = text(input, "scope") == "project";
                ensure!(
                    !project || self.trust_project,
                    "Trust this project before changing its providers"
                );
                crate::write_provider_entry(
                    &crate::provider_config_target(&self.paths, &self.workspace, project),
                    id,
                    Some(&section),
                )?;
            }
            "edit" | "remove" => {
                let (_, path, section) = self.provider_source(input)?;
                ensure!(
                    input["revision"].as_str() == Some(&revision(&path)?),
                    "Configuration changed; reopen the provider"
                );
                if text(input, "action") == "edit" {
                    let url = required_string(input, "url")?;
                    let base_url = if section.integration.is_some() {
                        crate::router_connection::normalize_url(url)?
                    } else {
                        ensure!(
                            crate::valid_provider_url(url),
                            "Use HTTPS, or HTTP on a loopback address"
                        );
                        url.into()
                    };
                    let name = required_string(input, "label")?.to_owned();
                    let models: Vec<String> = text(input, "models")
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                    // Only what the form changed is written: the entry's
                    // other settings (context window, tools, headers, keys
                    // Haarness does not know) stay as written.
                    let mut fields = Vec::new();
                    if section.base_url.as_deref() != Some(base_url.as_str()) {
                        fields.push(("base_url".to_owned(), Some(json!(base_url))));
                    }
                    // The form shows the id where the entry has no name.
                    if section.name.as_deref().unwrap_or(id) != name {
                        fields.push(("name".to_owned(), Some(json!(name))));
                    }
                    if section.models != models {
                        fields.push(("models".to_owned(), Some(json!(models))));
                    }
                    crate::write_entry_fields(&path, "providers", id, fields)?;
                } else {
                    crate::write_provider_entry(&path, id, None)?;
                }
            }
            _ => bail!("Unsupported provider action"),
        }
        Ok(())
    }

    async fn mcp_view(&self, input: &Value) -> Result<View> {
        let action = text(input, "action");
        let name = text(input, "name");
        if action == "edit_form" {
            let row = self
                .mcp(&json!({"action":"get","name":name,"scope":input["scope"]}))
                .await?;
            let config = &row["config"];
            let fields = if config["command"].is_string() {
                vec![
                    field("command", "Executable", text(config, "command"), true),
                    field(
                        "argsText",
                        "Arguments (one per line)",
                        config["args"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n"),
                        false,
                    ),
                ]
            } else {
                vec![field("url", "Server URL", text(config, "url"), true)]
            };
            return Ok(form(
                "Edit MCP connection",
                row_request("edit", &row),
                fields,
            ));
        }
        if action == "form" {
            let kind = if text(input, "type") == "remote" {
                "remote"
            } else {
                "local"
            };
            let mut fields = vec![
                field("name", "Server name", name, true),
                choice("scope", "Scope", "global", &["global", "local"]),
            ];
            if kind == "local" {
                fields.extend([
                    field("command", "Executable", "", true),
                    field("argsText", "Arguments (one per line)", "", false),
                ]);
            } else {
                fields.push(field("url", "Server URL", "", true));
                fields.push(field(
                    "oauthClientId",
                    "OAuth client ID (optional)",
                    "",
                    false,
                ));
                fields.push(field(
                    "oauthScopesText",
                    "OAuth scopes (comma separated)",
                    "",
                    false,
                ));
            }
            return Ok(form(
                "Add MCP server",
                json!({"action":"add","type":kind}),
                fields,
            ));
        }
        if action == "inspect" {
            let row = self
                .mcp(&json!({"action":"get","name":name,"scope":input["scope"]}))
                .await?;
            let enabled = row["config"]["enabled"].as_bool().unwrap_or(true);
            // A server of the system folder is never changed there: turning it
            // off saves the person's own copy, and it has no Edit or Remove.
            let base = row["scope"] == "system";
            let mut view = View {
                title: format!("MCP · {name}"),
                lines: vec![
                    format!("Status: {}", text(&row, "runtimeStatus")),
                    if base {
                        format!("Source: {} (system folder, read-only)", text(&row, "file"))
                    } else {
                        format!("Source: {}", text(&row, "file"))
                    },
                ],
                entries: vec![entry(
                    if enabled { "Disable" } else { "Enable" },
                    if base {
                        "Saved as your own setting over the system folder's"
                    } else {
                        "Persist this server's availability"
                    },
                    row_request(if enabled { "disable" } else { "enable" }, &row),
                )],
                ..View::default()
            };
            if !base {
                view.entries.push(entry(
                    "Edit connection",
                    "Change executable or endpoint",
                    row_request("edit_form", &row),
                ));
            }
            view.entries.extend([
                entry(
                    "Restart",
                    "Reconnect this server",
                    row_request("restart", &row),
                ),
                entry(
                    "Authenticate",
                    "Open the OAuth authorization URL",
                    row_request("login", &row),
                ),
                destructive("Clear authorization", row_request("logout", &row)),
            ]);
            if !base {
                view.entries
                    .push(destructive("Remove server", row_request("remove", &row)));
            }
            view.lines
                .push(serde_json::to_string_pretty(&row["config"])?);
            return Ok(view);
        }
        let rows = self.mcp(&json!({"action":"list"})).await?;
        let mut view = View {
            title: "MCP servers".into(),
            entries: vec![
                entry(
                    "Add local server",
                    "An executable on this server",
                    json!({"action":"form","type":"local"}),
                ),
                entry(
                    "Add remote server",
                    "An HTTP MCP endpoint",
                    json!({"action":"form","type":"remote"}),
                ),
            ],
            ..View::default()
        };
        for row in rows.as_array().into_iter().flatten() {
            if row["scope"] == "system" {
                view.entries.push(entry(
                    text(row, "name"),
                    format!("{} · system", text(row, "runtimeStatus")),
                    row_request("inspect", row),
                ));
            } else if row["readOnly"] == true {
                view.lines.push(format!(
                    "{} · {} · managed by plugin",
                    text(row, "name"),
                    text(row, "runtimeStatus")
                ));
            } else {
                view.entries.push(entry(
                    text(row, "name"),
                    text(row, "runtimeStatus"),
                    row_request("inspect", row),
                ));
            }
        }
        Ok(view)
    }

    fn memory_view(&self, input: &Value) -> Result<View> {
        let action = text(input, "action");
        let name = text(input, "name");
        if action == "form" || action == "inspect" {
            let row = if name.is_empty() {
                json!({})
            } else {
                self.desktop_memory(&json!({"action":"read","name":name}))?
            };
            if action == "form" {
                let mut fields = vec![
                    field(
                        "description",
                        "Description",
                        text(&row, "description"),
                        true,
                    ),
                    choice(
                        "type",
                        "Type",
                        if text(&row, "type").is_empty() {
                            "project"
                        } else {
                            text(&row, "type")
                        },
                        &["project", "user", "feedback", "reference"],
                    ),
                    field("content", "Content", text(&row, "content"), true),
                ];
                if name.is_empty() {
                    fields.insert(0, field("name", "Memory name", "", true));
                }
                return Ok(form(
                    "Memory",
                    json!({"action":if name.is_empty(){"create"}else{"save"},"name":name,"revision":row["revision"]}),
                    fields,
                ));
            }
            return Ok(View {
                title: name.into(),
                lines: vec![
                    text(&row, "description").into(),
                    text(&row, "content").into(),
                ],
                entries: vec![
                    entry("Edit", "", json!({"action":"form","name":name})),
                    destructive("Delete memory", row_request("delete", &row)),
                ],
                ..View::default()
            });
        }
        let status = self.desktop_memory(&json!({"action":"status"}))?;
        let mut config = status["config"].clone();
        config["enabled"] = json!(!config["enabled"].as_bool().unwrap_or(true));
        let mut view = View {
            title: "Project memory".into(),
            lines: [format!("Directory: {}", text(&status, "directory"))]
                .into_iter()
                .chain(status["settingsUnreadable"].as_bool().unwrap_or(false).then(|| {
                    "Memory is off because its settings file could not be read. Saving replaces it."
                        .to_owned()
                }))
                .collect(),
            entries: vec![
                entry("Create memory", "", json!({"action":"form"})),
                entry(
                    if config["enabled"] == true {
                        "Enable memory"
                    } else {
                        "Disable memory"
                    },
                    "Applies to this project",
                    json!({"action":"configure","config":config,"revision":status["revision"]}),
                ),
            ],
            ..View::default()
        };
        for row in status["memories"].as_array().into_iter().flatten() {
            view.entries.push(entry(
                text(row, "name"),
                text(row, "description"),
                row_request("inspect", row),
            ));
        }
        Ok(view)
    }

    fn hooks_view(&self, input: &Value) -> Result<View> {
        let status = crate::desktop_hooks::execute(
            &self.paths,
            &self.workspace,
            self.trust_project,
            &json!({"action":"list"}),
        )?;
        let id = text(input, "id");
        let row = status["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| text(r, "id") == id)
            .cloned()
            .unwrap_or(json!({}));
        if text(input, "action") == "form" {
            ensure!(
                id.is_empty() || row["source"]["editable"] == true,
                "Hook is not editable"
            );
            let mut fields = vec![
                choice(
                    "event",
                    "Event",
                    if id.is_empty() {
                        "PostToolUse"
                    } else {
                        text(&row, "event")
                    },
                    crate::desktop_hooks::EVENTS,
                ),
                field(
                    "matcher",
                    "Matcher",
                    if id.is_empty() {
                        "*"
                    } else {
                        text(&row, "matcher")
                    },
                    true,
                ),
                field("command", "Command", text(&row["handler"], "command"), true),
                field(
                    "timeout",
                    "Timeout in seconds",
                    row["handler"]["timeout"].as_u64().unwrap_or(30),
                    true,
                ),
            ];
            if id.is_empty() {
                fields.insert(0, choice("scope", "Scope", "global", &["global", "local"]));
            }
            return Ok(form(
                "Hook",
                json!({"action":if id.is_empty(){"create"}else{"update"},"id":id,"revision":row["revision"],"args":row["handler"].get("args").cloned().unwrap_or(json!([])),"async":row["handler"]["async"].as_bool().unwrap_or(false),"shell":text(&row["handler"],"shell")}),
                fields,
            ));
        }
        if text(input, "action") == "inspect" {
            ensure!(
                !row.is_null() && row.get("id").is_some(),
                "Hook no longer exists"
            );
            let mut view = View {
                title: format!("{} · {}", text(&row, "event"), text(&row, "matcher")),
                lines: vec![
                    text(&row["handler"], "command").into(),
                    if row["source"]["scope"] == "system" {
                        format!(
                            "Source: {} (system folder, read-only)",
                            text(&row["source"], "path")
                        )
                    } else {
                        format!("Source: {}", text(&row["source"], "path"))
                    },
                ],
                ..View::default()
            };
            if row["neverRuns"] == true {
                view.lines.push(never_runs_line(text(&row, "event")));
            }
            if row["source"]["editable"] == true {
                view.entries
                    .push(entry("Edit", "", row_request("form", &row)));
                let enabled = row["handler"]["disabled"] == true;
                view.entries.push(entry(
                    if enabled { "Enable" } else { "Disable" },
                    "",
                    json!({"action":"toggle","id":id,"revision":row["revision"],"enabled":enabled}),
                ));
                view.entries
                    .push(destructive("Delete hook", row_request("delete", &row)));
            }
            return Ok(view);
        }
        let mut view = View {
            title: "Hooks".into(),
            entries: vec![entry(
                "Create hook",
                "Run a command on a runtime event",
                json!({"action":"form"}),
            )],
            ..View::default()
        };
        for row in status["items"].as_array().into_iter().flatten() {
            let scope = text(&row["source"], "scope");
            view.entries.push(entry(
                format!(
                    "{} · {}",
                    text(row, "event"),
                    text(&row["handler"], "command")
                ),
                if row["neverRuns"] == true {
                    format!("{scope} · never runs")
                } else {
                    scope.to_owned()
                },
                row_request("inspect", row),
            ));
        }
        Ok(view)
    }

    fn scheduler(&self) -> Result<Scheduler> {
        Scheduler::discover(std::env::current_exe()?, self.paths.root.clone())
    }
    fn schedule_view(&self, input: &Value) -> Result<View> {
        let scheduler = self.scheduler()?;
        let id = text(input, "id");
        if text(input, "action") == "form" {
            let row = if id.is_empty() {
                json!({})
            } else {
                serde_json::to_value(scheduler.require(id)?)?
            };
            let trigger = &row["trigger"];
            let kind = trigger["type"].as_str().unwrap_or("interval");
            let value = match kind {
                "cron" => text(trigger, "expression").to_owned(),
                "once" => text(trigger, "at").to_owned(),
                _ => trigger["seconds"].as_u64().unwrap_or(3600).to_string(),
            };
            return Ok(form(
                "Schedule",
                json!({"action":if id.is_empty(){"create"}else{"update"},"id":id}),
                vec![
                    field("name", "Name", text(&row, "name"), true),
                    field("prompt", "Prompt", text(&row, "prompt"), true),
                    choice(
                        "triggerType",
                        "Trigger",
                        kind,
                        &["interval", "cron", "once"],
                    ),
                    field(
                        "triggerValue",
                        "Seconds, cron expression, or RFC3339 time",
                        value,
                        true,
                    ),
                    field(
                        "timezone",
                        "Timezone (optional)",
                        text(&row, "timezone"),
                        false,
                    ),
                ],
            ));
        }
        if text(input, "action") == "inspect" {
            let row = scheduler.require(id)?;
            return Ok(View {
                title: row.name.clone(),
                lines: vec![
                    Scheduler::summary(&row),
                    row.prompt,
                    format!("Workspace: {}", row.workspace.display()),
                ],
                entries: vec![
                    entry("Edit", "", json!({"action":"form","id":id})),
                    entry(
                        if row.enabled { "Disable" } else { "Enable" },
                        "",
                        json!({"action":if row.enabled{"disable"}else{"enable"},"id":id}),
                    ),
                    entry(
                        "Run now",
                        "Start a scheduled run",
                        json!({"action":"run","id":id}),
                    ),
                    entry("Run history", "", json!({"action":"history","id":id})),
                    destructive("Delete schedule", json!({"action":"remove","id":id})),
                ],
                ..View::default()
            });
        }
        let mut view = View {
            title: "Schedules".into(),
            entries: vec![entry(
                "Create schedule",
                "New schedules start disabled",
                json!({"action":"form"}),
            )],
            ..View::default()
        };
        for row in scheduler.list()? {
            view.entries.push(entry(
                row.name.clone(),
                Scheduler::summary(&row),
                json!({"action":"inspect","id":row.id}),
            ));
        }
        Ok(view)
    }
    fn schedule_mutation(&self, input: &Value) -> Result<()> {
        let scheduler = self.scheduler()?;
        let id = text(input, "id");
        match text(input, "action") {
            "create" | "update" => {
                let value = required_string(input, "triggerValue")?;
                let trigger = match text(input, "triggerType") {
                    "interval" => Trigger::Interval {
                        seconds: value.parse().context("Enter an interval in seconds")?,
                    },
                    "cron" => Trigger::Cron {
                        expression: value.into(),
                    },
                    "once" => Trigger::Once {
                        at: chrono::DateTime::parse_from_rfc3339(value)?
                            .with_timezone(&chrono::Utc),
                    },
                    _ => bail!("Choose a trigger"),
                };
                let name = Some(required_string(input, "name")?.into());
                let prompt = Some(required_string(input, "prompt")?.into());
                let timezone = optional_string(input, "timezone")
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned);
                if text(input, "action") == "create" {
                    scheduler.create(CreateSchedule {
                        name,
                        prompt,
                        workspace: self.workspace.clone(),
                        trust_project: self.trust_project,
                        trigger: Some(trigger),
                        timezone,
                        enabled: false,
                        ..CreateSchedule::default()
                    })?;
                } else {
                    scheduler.update(
                        id,
                        UpdateSchedule {
                            name,
                            prompt,
                            trigger: Some(trigger),
                            timezone: Some(timezone),
                            ..UpdateSchedule::default()
                        },
                    )?;
                }
            }
            "enable" | "disable" => {
                scheduler.toggle(id, text(input, "action") == "enable")?;
            }
            "remove" => {
                scheduler.remove(id)?;
            }
            "run" => {
                scheduler.start_now(id)?;
            }
            _ => bail!("Unsupported schedule action"),
        }
        Ok(())
    }
    fn profile_roots(&self) -> haarness_profiles::Roots {
        haarness_profiles::Roots {
            config: self.paths.config_dir().to_path_buf(),
            data: self.paths.data.clone(),
            project: self.workspace.clone(),
        }
    }
    fn profile_view(&self, input: &Value) -> Result<View> {
        let catalog = haarness_profiles::Catalog::new(&self.paths.data);
        if text(input, "action") == "restore_preview" {
            let id: Uuid = required_string(input, "id")?.parse()?;
            let archive = haarness_profiles::Archive::decode(&catalog.download(id)?)?;
            let targets = archive.restore(&self.profile_roots(), true, true)?;
            return Ok(View {
                title: "Restore configuration profile".into(),
                lines: std::iter::once("These files will be replaced:".into())
                    .chain(targets.into_iter().map(|p| p.display().to_string()))
                    .collect(),
                entries: vec![destructive(
                    "Replace configuration files",
                    json!({"action":"restore","id":id,"force":true}),
                )],
                ..View::default()
            });
        }
        if text(input, "action") == "form" {
            return Ok(form(
                "Save configuration profile",
                json!({"action":"save"}),
                vec![
                    field("name", "Name", "", true),
                    choice("scope", "Scope", "both", &["both", "global", "local"]),
                ],
            ));
        }
        if text(input, "action") == "inspect" {
            let id: Uuid = required_string(input, "id")?.parse()?;
            let row = catalog.get(id)?;
            let archive = haarness_profiles::Archive::decode(&catalog.download(id)?)?;
            return Ok(View {
                title: row.name,
                lines: vec![
                    format!(
                        "{} configuration files · {} bytes",
                        row.entry_count, row.size_bytes
                    ),
                    format!("Scope: {:?}", row.scope),
                    serde_json::to_string_pretty(&archive.manifest)?,
                ],
                entries: vec![
                    entry(
                        "Restore profile",
                        "Review the files that will be replaced",
                        json!({"action":"restore_preview","id":id}),
                    ),
                    destructive("Delete profile", json!({"action":"remove","id":id})),
                ],
                ..View::default()
            });
        }
        Ok(View{title:"Configuration profiles".into(),lines:vec!["Profiles exclude credential stores. Inline configuration secrets remain in the archive.".into()],entries:std::iter::once(entry("Save profile","Capture configuration from this server",json!({"action":"form"}))).chain(catalog.list("",0,100)?.into_iter().map(|r|entry(r.name,format!("{} files",r.entry_count),json!({"action":"inspect","id":r.id})))).collect(),..View::default()})
    }
    fn profile_mutation(&self, input: &Value) -> Result<()> {
        let catalog = haarness_profiles::Catalog::new(&self.paths.data);
        match text(input, "action") {
            "save" => {
                let scope = match text(input, "scope") {
                    "global" => haarness_profiles::Scope::Global,
                    "local" => haarness_profiles::Scope::Local,
                    "both" => haarness_profiles::Scope::Both,
                    _ => bail!("Choose profile scope"),
                };
                catalog.put(&haarness_profiles::Archive::capture(
                    &self.profile_roots(),
                    required_string(input, "name")?,
                    scope,
                )?)?;
            }
            "restore" => {
                let archive = haarness_profiles::Archive::decode(
                    &catalog.download(required_string(input, "id")?.parse()?)?,
                )?;
                archive.restore(&self.profile_roots(), input["force"] == true, false)?;
            }
            "remove" => {
                catalog.remove(required_string(input, "id")?.parse()?)?;
            }
            _ => bail!("Unsupported profile action"),
        }
        Ok(())
    }

    /// Stop pending provider sign-ins of one provider and method.
    async fn cancel_logins(&self, target: &str, method: &str) {
        let stale: Vec<Login> = {
            let mut logins = self
                .logins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let ids: Vec<String> = logins
                .iter()
                .filter(|(_, login)| {
                    login.resource == "providers"
                        && login.target == target
                        && login.method == method
                })
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter().filter_map(|id| logins.remove(id)).collect()
        };
        for mut login in stale {
            login.task.abort();
            let _ = (&mut login.task).await;
        }
    }

    async fn start_login(&self, resource: &str, input: &Value) -> Result<Value> {
        let name = required_string(input, "name")?.to_owned();
        let method = match text(input, "method") {
            "" | "browser" => "browser",
            "device" | "headless" => "device",
            other => bail!("Unknown sign-in method {other}"),
        };
        let display = if resource == "mcp" {
            name.clone()
        } else {
            provider_name(
                &self
                    .runtime()
                    .map(|runtime| runtime.providers().list())
                    .unwrap_or_default(),
                &name,
            )
        };
        let provider_id = name.clone();
        let mut callback_sender = None;
        let (url, code, completion): (String, String, Completion) = if resource == "mcp" {
            self.resolve_mcp_readable(&name, optional_scope(input)?)?;
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
            let redirect = format!("http://{}/mcp/oauth/callback", listener.local_addr()?);
            let session = self
                .mcp
                .begin_oauth(&name, &redirect)
                .await
                .map_err(core_error)?;
            let url = session.authorization_url.clone();
            let authorization = url.clone();
            let mcp = self.mcp.clone();
            let (sender, receiver) = tokio::sync::oneshot::channel::<String>();
            callback_sender = Some(sender);
            (
                url,
                String::new(),
                Box::pin(async move {
                    let callback = tokio::select! {
                        value = receive_oauth_callback(&listener, &redirect, &authorization) => value?,
                        value = receiver => value.context("Authorization cancelled")?,
                    };
                    session.complete(&callback).await.map_err(core_error)?;
                    mcp.reconnect(&name).await.map_err(core_error)?;
                    Ok(())
                }),
            )
        } else if name == "haarness" {
            // Haarness Cloud is the Haarness account itself: the account client
            // keeps its own credentials, and the provider reads them on its next
            // model refresh, so nothing goes into the provider key store.
            let account = haarness_account::Client::new(haarness_account::Config::from_env()?)?;
            let login = account.begin_login(method == "device").await?;
            (
                login.url.clone(),
                login.code.clone().unwrap_or_default(),
                Box::pin(async move { Ok(login.finish().await?) }),
            )
        } else {
            let (url, code, future): (String, String, Completion<Credential>) =
                match (name.as_str(), method) {
                    ("openai", "browser") => {
                        // A new attempt replaces one still holding the
                        // callback port, so the browser returns to this one.
                        self.cancel_logins(&name, method).await;
                        let login = OpenAiOAuthClient::new(self.openai_oauth.clone())
                            .start_browser_login()
                            .await?;
                        (
                            login.url.to_string(),
                            String::new(),
                            Box::pin(async move { Ok(login.finish().await?) }),
                        )
                    }
                    ("openai", _) => {
                        let login = OpenAiOAuthClient::new(self.openai_oauth.clone())
                            .start_device_login()
                            .await?;
                        (
                            login.verification_url.clone(),
                            login.user_code.clone(),
                            Box::pin(async move { Ok(login.finish().await?) }),
                        )
                    }
                    ("xai", _) => {
                        // xAI's complete verification URL carries the code, so
                        // the browser method needs no typing; the code is still
                        // shown for a browser opened by hand.
                        let login = XaiOAuthClient::default().start_device_login().await?;
                        (
                            if method == "browser" {
                                login.verification_url.clone()
                            } else {
                                login.instructions_url.clone()
                            },
                            login.user_code.clone(),
                            Box::pin(async move { Ok(login.finish().await?) }),
                        )
                    }
                    ("github-copilot", _) => {
                        let login = CopilotOAuthClient::default().start_device_login().await?;
                        (
                            login.verification_url.clone(),
                            login.user_code.clone(),
                            Box::pin(async move { Ok(login.finish().await?) }),
                        )
                    }
                    _ => bail!("{display} has no sign-in; use an API key"),
                };
            let store = CredentialStore::new(self.paths.auth.clone());
            (
                url,
                code,
                Box::pin(async move {
                    store.set(&name, &future.await?)?;
                    Ok(())
                }),
            )
        };
        let mut logins = self
            .logins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        logins.retain(|_, login| {
            if login.created.elapsed() > Duration::from_secs(900) {
                login.task.abort();
                false
            } else {
                true
            }
        });
        ensure!(
            logins.len() < 16,
            "Too many pending logins; cancel one before starting another"
        );
        let id = Uuid::now_v7().to_string();
        let result = Arc::new(Mutex::new(None));
        let target = result.clone();
        let task = tokio::spawn(async move {
            let outcome = tokio::time::timeout(Duration::from_secs(600), completion)
                .await
                .map_err(|_| "Login timed out".into())
                .and_then(|r| r.map_err(|e| e.to_string()));
            *target
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
        });
        let remote_callback = callback_sender.is_some();
        let login = Login {
            created: Instant::now(),
            task,
            callback: callback_sender,
            result,
            resource: resource.into(),
            target: provider_id,
            name: display,
            method: method.into(),
            url,
            code,
        };
        let view = View {
            title: login.title(),
            data: Some(login.data(&id, "pending", None)),
            lines: login.instructions(),
            entries: [if remote_callback { Some(entry("Paste callback URL", "For remote servers: paste the URL after browser authorization if localhost cannot connect", json!({"action":"login_callback_form","id":id}))) } else { None }].into_iter().flatten().chain(vec![
                entry(
                    "Check status",
                    "Refresh this page by hand",
                    json!({"action":"login_status","id":id}),
                ),
                entry("Cancel sign-in", "", json!({"action":"login_cancel","id":id})),
            ]).collect(),
            ..View::default()
        };
        logins.insert(id, login);
        output(view)
    }
}

/// "Use an API key" → "Replace the API key": the action once one is stored.
fn replace_label(label: &str) -> String {
    format!("Replace the {}", key_field_label(label))
}

/// "Use an API key" → "API key": the noun a form field is labelled with.
fn key_field_label(label: &str) -> &str {
    ["Use an ", "Use a ", "Use "]
        .iter()
        .find_map(|prefix| label.strip_prefix(prefix))
        .unwrap_or(label)
}

/// A provider's display name, falling back to its id for unknown ids.
fn provider_name(providers: &[haarness_protocol::ProviderInfo], id: &str) -> String {
    providers
        .iter()
        .find(|p| p.id == id)
        .map_or_else(|| id.to_owned(), |p| p.name.clone())
}

/// One-line status for a provider row in the manager and the connect picker.
fn provider_status(provider: &haarness_protocol::ProviderInfo) -> String {
    if provider.authenticated {
        return match provider.models.len() {
            0 => "Connected".into(),
            1 => "Connected · 1 model".into(),
            count => format!("Connected · {count} models"),
        };
    }
    let guide = haarness_providers::connect::guide(&provider.id);
    let oauth = guide
        .methods
        .iter()
        .find(|method| method.kind == haarness_providers::connect::MethodKind::Oauth);
    let api = guide
        .methods
        .iter()
        .any(|method| method.kind == haarness_providers::connect::MethodKind::Api);
    // "Sign in with ChatGPT" continues a sentence here; only its first letter
    // drops, so brand names keep their own spelling.
    let continued = |label: &str| {
        let mut letters = label.chars();
        letters.next().map_or_else(String::new, |first| {
            first.to_lowercase().chain(letters).collect()
        })
    };
    match (oauth, guide.note) {
        (Some(method), _) if api => format!(
            "Not connected · {} or use an API key",
            continued(method.label)
        ),
        (Some(method), _) => format!("Not connected · {}", continued(method.label)),
        (None, Some(note)) if guide.methods.is_empty() => format!("Not connected · {note}"),
        _ => "Not connected · API key".into(),
    }
}

fn router_form(input: &Value, needs_key: bool) -> View {
    let advanced = input["advanced"] == true;
    let mut fields = vec![field("url", "Router address", text(input, "url"), true)];
    if needs_key {
        let mut key = field("key", "Router access key", "", true);
        key.secret = true;
        fields.push(key);
    }
    if advanced {
        fields.push(field(
            "label",
            "Connection name (optional)",
            text(input, "label"),
            false,
        ));
        fields.push(choice(
            "scope",
            "Available in",
            if text(input, "scope") == "project" {
                "project"
            } else {
                "global"
            },
            &["global", "project"],
        ));
    }
    let mut request = json!({"action":"router_connect","advanced":advanced,"label":text(input,"label"),"scope":text(input,"scope")});
    let mut advanced_request = request.clone();
    advanced_request["action"] = json!("router_form");
    advanced_request["advanced"] = json!(!advanced);
    advanced_request["url"] = json!(text(input, "url"));
    request["url"] = json!(text(input, "url"));
    View {
        data: None,
        title: "Connect Haarness Router".into(), fields, submit: Some(request),
        lines: vec!["Self-hosted · Enter the address of the router you run, for example http://localhost:8080. A key is optional unless the router requires one.".into(),
            "The connection is checked and saved on this Haarness server. localhost refers to this server.".into(),
            if needs_key { "This router requires an access key. Enter it to connect.".into() } else { "Your provider API keys stay on the router.".into() }],
        entries: vec![entry(if advanced { "Hide advanced" } else { "Advanced" }, "Connection name and project scope", advanced_request)],
    }
}

#[cfg(test)]
mod tests {
    use super::replace_label;
    use serde_json::{Value, json};
    use std::fs;

    /// Editing a provider in Settings writes its name, address and models, and
    /// nothing else: a TOML entry keeps its context window and tool settings,
    /// and a JSON one gains no loaded defaults and keeps its comments.
    #[test]
    fn a_provider_edit_writes_only_what_the_form_changed() {
        let (root, workspace, paths, service) =
            crate::management::tests::isolated_service("provider-edit");
        fs::write(
            &paths.config,
            r#"[providers.gateway]
protocol = "openai-chat-completions"
name = "Gateway"
base_url = "https://gateway.fixture.test/v1"
models = ["a"]
context_window = 200000
max_output_tokens = 8000
tools = false
api_key_envs = ["GATEWAY_KEY", "TEAM_GATEWAY_KEY"]
"#,
        )
        .unwrap();
        let project = workspace.join("haarness.json");
        let project_text = r#"{
  "providers": {
    // The team's proxy.
    "proxy": {"protocol": "openai-chat-completions", "base_url": "https://proxy.fixture.test/v1",
              "models": ["m"], "x-team-note": "keep"},
    "plain": {"protocol": "openai-chat-completions", "base_url": "https://plain.fixture.test/v1",
              "models": ["m"]}
  }
}
"#;
        fs::write(&project, project_text).unwrap();
        for (id, scope, path, url, label, models) in [
            (
                "gateway",
                "global",
                paths.config.clone(),
                "https://gateway.fixture.test/v1",
                "Team gateway",
                "a, b",
            ),
            (
                "proxy",
                "project",
                project.clone(),
                "https://proxy.fixture.test/v2",
                "Proxy",
                "m",
            ),
            // The form shows the id as the name of an entry without one.
            (
                "plain",
                "project",
                project.clone(),
                "https://plain.fixture.test/v1",
                "plain",
                "m, n",
            ),
        ] {
            service
                .provider_mutation(&json!({"action": "edit", "name": id, "scope": scope,
                    "source": path, "revision": super::revision(&path).unwrap(), "url": url,
                    "label": label, "models": models}))
                .unwrap();
        }
        let global: toml::Value =
            toml::from_str(&fs::read_to_string(&paths.config).unwrap()).unwrap();
        let gateway = &global["providers"]["gateway"];
        assert_eq!(gateway["name"].as_str(), Some("Team gateway"));
        assert_eq!(
            gateway["models"],
            toml::Value::try_from(["a", "b"]).unwrap()
        );
        assert_eq!(gateway["context_window"].as_integer(), Some(200_000));
        assert_eq!(gateway["max_output_tokens"].as_integer(), Some(8000));
        assert_eq!(gateway["tools"].as_bool(), Some(false));
        assert_eq!(
            gateway["api_key_envs"],
            toml::Value::try_from(["GATEWAY_KEY", "TEAM_GATEWAY_KEY"]).unwrap()
        );
        let text = fs::read_to_string(&project).unwrap();
        assert!(text.contains("// The team's proxy."), "{text}");
        let written: Value =
            jsonc_parser::parse_to_serde_value(&text, &jsonc_parser::ParseOptions::default())
                .unwrap();
        assert_eq!(
            written["providers"]["proxy"],
            json!({"protocol": "openai-chat-completions", "base_url": "https://proxy.fixture.test/v2",
                "models": ["m"], "x-team-note": "keep", "name": "Proxy"})
        );
        assert_eq!(
            written["providers"]["plain"],
            json!({"protocol": "openai-chat-completions", "base_url": "https://plain.fixture.test/v1",
                "models": ["m", "n"]})
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replacing_a_stored_key_reads_as_a_sentence() {
        // "Use an API key" once became "Replace then API key": the "Use a"
        // rule matched the start of "Use an".
        assert_eq!(replace_label("Use an API key"), "Replace the API key");
        assert_eq!(
            replace_label("Use an access token"),
            "Replace the access token"
        );
        assert_eq!(
            replace_label("Use a GitHub token"),
            "Replace the GitHub token"
        );
    }

    /// The installed list says how each resource gets new versions, with one
    /// entry to change it, and why an automatic update waits.
    #[tokio::test]
    async fn installed_resources_say_how_they_get_new_versions() {
        use std::sync::Arc;
        let root =
            std::env::temp_dir().join(format!("haarness-ui-auto-update-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let paths = crate::Paths::new(Some(&root.join("home"))).unwrap();
        let workspace = root.join("workspace");
        let skill = workspace.join(".haarness/skills/demo");
        fs::create_dir_all(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\ndescription: Demo\n---\nDemo\n",
        )
        .unwrap();
        let id = uuid::Uuid::now_v7();
        let receipts = workspace.join(".haarness/marketplace");
        fs::create_dir_all(&receipts).unwrap();
        let write = |extra: Value| {
            let mut receipt = json!({"id":id,"kind":"skill","name":"demo","slug":"demo","scope":"local","path":skill,
                "version":{"id":"00000000-0000-0000-0000-000000000003","number":3},
                "installedDigest":crate::marketplace::tree_digest(&skill).unwrap()});
            receipt
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            fs::write(receipts.join(format!("{id}.json")), receipt.to_string()).unwrap();
        };
        write(json!({"autoUpdate":true}));
        let skills: Arc<dyn haarness_core::SkillRegistry> = Arc::new(
            haarness_server::FilesystemSkillRegistry::new(vec![(paths.skills.clone(), true)]),
        );
        let mcp = Arc::new(haarness_mcp::NativeMcpRegistry::with_auth_dir(
            paths.data.join("mcp-oauth"),
        ));
        let service =
            crate::management::ManagementService::new(paths, workspace.clone(), true, skills, mcp);
        let view = async |input: Value| service.ui_request("marketplace-ui", &input).await.unwrap();
        let entry = |view: &Value, label: &str| {
            view["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["label"] == label)
                .cloned()
                .unwrap_or_else(|| panic!("no {label}: {view:#}"))
        };
        let listed = view(json!({"action":"installed"})).await;
        assert_eq!(
            entry(&listed, "Disable demo")["description"],
            "skill · this project · version 3 · on · updates automatically"
        );
        assert_eq!(
            entry(&listed, "Update demo")["description"],
            "Install the latest version"
        );
        let stop = entry(&listed, "Stop automatic updates for demo");
        assert_eq!(stop["description"], "Stay on version 3");
        assert!(stop["confirm"].is_null(), "no confirmation for a setting");

        let pinned = view(stop["request"].clone()).await;
        assert_eq!(
            pinned["lines"][0],
            "demo stays on this version until you update it."
        );
        assert_eq!(
            entry(&pinned, "Disable demo")["description"],
            "skill · this project · pinned to version 3 · on"
        );
        let follow = entry(&pinned, "Update demo automatically");
        assert_eq!(follow["request"]["enabled"], true);
        let following = view(follow["request"].clone()).await;
        assert_eq!(following["lines"][0], "demo now updates automatically.");

        write(json!({"autoUpdate":true,"updateHeld":"modified"}));
        let held = view(json!({"action":"installed"})).await;
        assert_eq!(
            entry(&held, "Disable demo")["description"],
            "skill · this project · version 3 · on · has local edits, so automatic updates wait"
        );
        // Turning it off clears the hold: nothing waits any more.
        view(entry(&held, "Stop automatic updates for demo")["request"].clone()).await;
        let receipt: Value =
            serde_json::from_slice(&fs::read(receipts.join(format!("{id}.json"))).unwrap())
                .unwrap();
        assert_eq!(receipt["autoUpdate"], false);
        assert!(receipt["updateHeld"].is_null());
        fs::remove_dir_all(root).unwrap();
    }
}
