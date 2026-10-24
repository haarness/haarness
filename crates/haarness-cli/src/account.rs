use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use haarness_account::{Client, Config};
use reqwest::Method;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::io::{Read, Write};

#[derive(Args, Clone)]
pub struct LoginArgs {
    /// Sign in on a headless machine with a browser on another device.
    #[arg(long)]
    pub device: bool,
    /// Print the sign-in link without opening a browser.
    #[arg(long)]
    pub no_browser: bool,
    /// Print the sign-in link and the result as JSON lines.
    #[arg(long)]
    pub json: bool,
}
#[derive(Args)]
#[allow(clippy::struct_excessive_bools)] // independent CLI switches validated by clap
pub struct AccountArgs {
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
    /// Switch the account used for Cloud models and sessions.
    #[arg(long)]
    pub select: Option<String>,
    /// List the account’s Cloud routers.
    #[arg(long)]
    pub routers: bool,
    /// Choose the Cloud router used by native model requests.
    #[arg(long)]
    pub router: Option<String>,
    /// Private Desktop bridge: one request on stdin; no project configuration.
    #[arg(long, hide = true)]
    pub rpc: bool,
    /// Show the selected account's plan, credit balances and billing links.
    #[arg(long, conflicts_with_all = ["rpc", "watch_billing"])]
    pub billing: bool,
    /// Stream account billing snapshots as JSON lines for native clients.
    #[arg(long, hide = true, conflicts_with_all = ["rpc", "select"])]
    pub watch_billing: bool,
}
#[derive(Args)]
pub struct CloudArgs {
    #[command(subcommand)]
    pub command: CloudCommand,
}
#[derive(Subcommand)]
pub enum CloudCommand {
    /// Sign in to Haarness Cloud (the same as `haarness login`).
    Login(LoginArgs),
    /// Manage reusable cloud-agent templates.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Start, list and attach to cloud sessions, blank or from a cloud agent.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Compatibility commands for existing Cloud workspaces.
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
}
#[derive(Subcommand)]
pub enum AgentCommand {
    /// List your cloud agents.
    List {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show one cloud agent.
    Show {
        /// Cloud agent id.
        id: String,
    },
    /// Create a cloud agent from a JSON file with its name, configuration and
    /// optional write-only environment values.
    Create {
        /// The agent's JSON file.
        #[arg(long)]
        file: std::path::PathBuf,
    },
    /// Replace a cloud agent's configuration from a JSON file with its
    /// version and the full updated configuration.
    Update {
        /// Cloud agent id.
        id: String,
        /// The agent's JSON file.
        #[arg(long)]
        file: std::path::PathBuf,
    },
    /// Delete a cloud agent.
    Delete {
        /// Cloud agent id.
        id: String,
    },
}
#[derive(Subcommand)]
pub enum SessionCommand {
    /// List your cloud sessions.
    List {
        /// Print JSON.
        #[arg(long)]
        json: bool,
        /// Only sessions started from this cloud agent (its id).
        #[arg(long)]
        agent: Option<String>,
    },
    /// Start a cloud session: a blank one, or one from a cloud agent.
    Create {
        /// Start from this cloud agent (its id).
        #[arg(long)]
        agent: Option<String>,
        /// The session's title.
        #[arg(long)]
        title: Option<String>,
        /// JSON creation body with configuration and optional encrypted environment values.
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        /// Create the session without starting it.
        #[arg(long)]
        stopped: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Save configuration/environment while stopped. File must include the current version.
    Update {
        /// Cloud session id.
        id: String,
        /// JSON file with the current version and the new settings.
        #[arg(long)]
        file: std::path::PathBuf,
    },
    /// Capture saved settings in an inert template with a fresh deploy key.
    SaveAgent {
        /// Cloud session id.
        id: String,
        /// The new cloud agent's name.
        #[arg(long)]
        name: String,
        /// The session's current `version`; a session changed since is refused.
        #[arg(long)]
        version: i64,
        /// Copy the session's environment values into the agent as well.
        #[arg(long)]
        include_environment: bool,
    },
    /// Show one cloud session as JSON.
    Show {
        /// Cloud session id.
        id: String,
    },
    /// Start a stopped session.
    Start {
        /// Cloud session id.
        id: String,
    },
    /// Stop a running session.
    Stop {
        /// Cloud session id.
        id: String,
    },
    /// Open a session in the terminal UI.
    Attach {
        /// Cloud session id.
        id: String,
    },
}
#[derive(Subcommand)]
pub enum WorkspaceCommand {
    /// List existing Cloud workspaces.
    List {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Create a Cloud workspace. New work belongs in `cloud session create`.
    Create {
        /// Workspace name.
        name: String,
        /// Git repository to clone into it.
        #[arg(long)]
        repo: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Start a stopped workspace.
    Start {
        /// Cloud workspace id.
        id: String,
    },
    /// Stop a running workspace.
    Stop {
        /// Cloud workspace id.
        id: String,
    },
    /// Open a workspace in the terminal UI.
    Attach {
        /// Cloud workspace id.
        id: String,
    },
}
pub fn client() -> Result<Client> {
    Ok(Client::new(Config::from_env()?)?)
}
pub async fn login(args: &LoginArgs) -> Result<()> {
    let client = client()?;
    let pending = client.begin_login(args.device).await?;
    if args.json {
        println!(
            "{}",
            json!({"type":"login","url":pending.url,"code":pending.code})
        );
        std::io::stdout().flush()?;
    } else {
        eprintln!("Sign in to Haarness:\n{}", pending.url);
        if let Some(code) = &pending.code {
            eprintln!("Code: {code}");
        }
    }
    if !args.no_browser {
        let _ = super::open_browser(&pending.url);
    }
    pending.finish().await?;
    let status = client.status().await?;
    if args.json {
        println!("{}", json!({"type":"complete","account":status}));
    } else {
        println!(
            "Signed in as {}. Haarness Cloud is ready.",
            status
                .pointer("/user/email")
                .and_then(Value::as_str)
                .unwrap_or("your Haarness account")
        );
    }
    Ok(())
}
pub async fn account(args: &AccountArgs, config_dir: &std::path::Path) -> Result<()> {
    let client = client()?;
    if args.watch_billing {
        let mut watcher = haarness_account::billing::watch(client);
        loop {
            let state = watcher.updates.borrow_and_update().clone();
            println!("{}", serde_json::to_string(&state)?);
            std::io::stdout().flush()?;
            if watcher.updates.changed().await.is_err() {
                break;
            }
        }
        return Ok(());
    }
    if args.rpc {
        let mut input = String::new();
        std::io::stdin()
            .take(64 * 1024)
            .read_to_string(&mut input)?;
        let input: Value = serde_json::from_str(&input)?;
        let value = match input["action"].as_str() {
            // Signed out is an answer here, not a failure: Desktop shows no
            // computers until there is an account to list them from.
            Some("remote-hosts") if !client.config.signed_in() => {
                json!({"loggedIn":false,"items":[]})
            }
            Some("remote-hosts") => super::remote_cloud::computers(&client, config_dir).await?,
            _ => rpc(&client, input).await?,
        };
        println!("{value}");
        return Ok(());
    }
    if let Some(id) = &args.select {
        client.select_account(id).await?;
    }
    if let Some(id) = &args.router {
        client.select_router(id).await?;
    }
    if args.routers {
        println!("{}", client.routers().await?);
        return Ok(());
    }
    if args.billing {
        let state = client.billing_summary().await?;
        if args.json {
            println!("{}", serde_json::to_string(&state)?);
        } else {
            println!("{}", state.lines().join("\n"));
        }
        return Ok(());
    }
    let status = client.status().await?;
    if args.json {
        println!("{status}");
    } else if status["loggedIn"] == true {
        println!(
            "Signed in as {}",
            status
                .pointer("/user/email")
                .and_then(Value::as_str)
                .unwrap_or("Haarness user")
        );
        if let Some(accounts) = status["accounts"].as_array() {
            for a in accounts {
                println!(
                    "{} {} ({})",
                    if a["id"] == status["selectedAccountId"] {
                        "*"
                    } else {
                        " "
                    },
                    a["name"].as_str().unwrap_or(""),
                    a["id"].as_str().unwrap_or("")
                );
            }
        }
    } else {
        println!("Not signed in. Run `haarness login` to use Cloud services.");
    }
    Ok(())
}
pub async fn logout() -> Result<()> {
    let client = client()?;
    let signed_in = client.config.signed_in();
    client.logout().await?;
    println!(
        "{}",
        if signed_in {
            "Signed out of Haarness."
        } else {
            "You were not signed in to Haarness."
        }
    );
    Ok(())
}
fn id(input: &Value) -> Result<&str> {
    let id = input["id"].as_str().context("Missing resource id")?;
    uuid::Uuid::parse_str(id).context("Invalid resource id")?;
    Ok(id)
}

pub(super) async fn list_all(client: &Client, path: &str) -> Result<Value> {
    let mut items = Vec::new();
    let mut next = path.to_owned();
    let mut seen = std::collections::HashSet::new();
    loop {
        let page = client.request(Method::GET, &next, None).await?;
        items.extend(
            page["items"]
                .as_array()
                .context("Invalid Cloud list response")?
                .iter()
                .cloned(),
        );
        let Some(cursor) = page["nextCursor"].as_str() else {
            break;
        };
        // The Cloud API uses UUID cursors. Validate before adding one to an authenticated URL.
        uuid::Uuid::parse_str(cursor).context("Invalid Cloud pagination cursor")?;
        anyhow::ensure!(
            seen.insert(cursor.to_owned()),
            "Cloud pagination cursor repeated"
        );
        next = format!(
            "{path}{}cursor={cursor}",
            if path.contains('?') { '&' } else { '?' }
        );
    }
    Ok(json!({"items":items,"nextCursor":null}))
}
pub async fn rpc(client: &Client, input: Value) -> Result<Value> {
    let action = input["action"].as_str().unwrap_or("status");
    let query = client
        .config
        .selected_account()
        .map(|id| format!("?accountId={id}"))
        .unwrap_or_default();
    Ok(match action {
        "status" => client.status().await?,
        "routers" => client.routers().await?,
        "select-router" => client.select_router(id(&input)?).await?,
        "billing-summary" => serde_json::to_value(client.billing_summary().await?)?,
        // Desktop opens another computer through Cloud's relay with the
        // current short-lived sign-in, and keeps it in memory only. It gets
        // one that stays valid at least five more minutes (so a connection
        // opened with it has time before Cloud ends it), and a token Cloud
        // refused comes back as `rejected` and is renewed first. Cloud mints
        // no short-lived credential from an API key, so with HAARNESS_API_KEY
        // set this is the key itself, without an expiry.
        "remote-auth" => {
            let (token, expires) = client
                .bearer_until(
                    input["rejected"].as_str(),
                    chrono::Duration::minutes(5),
                )
                .await?;
            json!({"token":token,"apiUrl":client.config.api_url,"expiresAt":expires})
        }
        "logout" => {
            client.logout().await?;
            json!({"loggedIn":false})
        }
        "select" => {
            client.select_account(id(&input)?).await?;
            client.status().await?
        }
        "workspaces" => {
            client
                .request(Method::GET, &format!("api/v1/workspaces{query}"), None)
                .await?
        }
        "agents" | "sessions" => {
            let resource = if action == "agents" {
                "cloud-agents"
            } else {
                "sessions"
            };
            list_all(client, &format!("api/v1/{resource}{query}")).await?
        }
        "create-session" => {
            client
                .request(
                    Method::POST,
                    "api/v1/sessions",
                    Some(json!({"accountId":client.config.selected_account(),"cloudAgentId":if input["id"].is_null() { None } else { Some(id(&input)?) },"title":input["name"],"configuration":input["configuration"]})),
                )
                .await?
        }
        "session" => {
            client
                .request(
                    Method::GET,
                    &format!("api/v1/sessions/{}", id(&input)?),
                    None,
                )
                .await?
        }
        "session-attach" | "session-start" | "session-stop" => {
            let action = action.strip_prefix("session-").unwrap_or(action);
            client
                .request(
                    Method::POST,
                    &format!("api/v1/sessions/{}/{action}", id(&input)?),
                    Some(json!({})),
                )
                .await?
        }
        "billing" => {
            client
                .request(Method::GET, &format!("api/v1/billing/wallet{query}"), None)
                .await?
        }
        "usage" => {
            client
                .request(Method::GET, &format!("api/v1/billing/usage{query}"), None)
                .await?
        }
        "create" => {
            let name = input["name"]
                .as_str()
                .filter(|n| !n.trim().is_empty())
                .context("Enter a workspace name")?;
            let template = match input["repo"].as_str().filter(|s| !s.is_empty()) {
                Some(url) => json!({"kind":"git","url":url}),
                None => json!({"kind":"empty"}),
            };
            client.request(Method::POST,"api/v1/workspaces",Some(json!({"name":name,"accountId":client.config.selected_account(),"template":template}))).await?
        }
        "attach" | "start" | "stop" => {
            client
                .request(
                    Method::POST,
                    &format!("api/v1/workspaces/{}/{action}", id(&input)?),
                    Some(json!({})),
                )
                .await?
        }
        "web" => {
            let page = match input["page"].as_str() {
                Some("billing" | "topup") => "app/billing",
                Some("usage") => "app/billing/usage",
                Some("agents") => "app/agents",
                Some("sessions") => "app/sessions",

                _ => "app",
            };
            {
                let base = client.config.web_origin()?;
                let mut url = base.join(&if input["page"] == "session-settings" { format!("app/sessions/{}/settings", id(&input)?) } else {page.to_owned()})?;
                if matches!(input["page"].as_str(), Some("billing" | "topup" | "usage")) {
                    if let Some(account) = client.config.selected_account() {
                        url.query_pairs_mut().append_pair("accountId", &account);
                    }
                }
                if input["page"] == "topup" { url.set_fragment(Some("top-up")); }
                json!({"url":url.to_string()})
            }
        }
        _ => bail!("Unknown account action"),
    })
}
pub async fn cloud(args: &CloudArgs) -> Result<()> {
    let client = client()?;
    match &args.command {
        CloudCommand::Login(args) => return login(args).await,
        CloudCommand::Agent { command } => {
            let value = match command {
                AgentCommand::List { .. } => rpc(&client, json!({"action":"agents"})).await?,
                AgentCommand::Show { id: agent } => {
                    client
                        .request(
                            Method::GET,
                            &format!("api/v1/cloud-agents/{}", id(&json!({"id":agent}))?),
                            None,
                        )
                        .await?
                }
                AgentCommand::Create { file } => {
                    let mut value: Value =
                        serde_json::from_slice(&std::fs::read(file).context("Read agent JSON")?)?;
                    if value.get("accountId").is_none() {
                        value["accountId"] = json!(client.config.selected_account());
                    }
                    client
                        .request(Method::POST, "api/v1/cloud-agents", Some(value))
                        .await?
                }
                AgentCommand::Update { id: agent, file } => {
                    client
                        .request(
                            Method::PUT,
                            &format!("api/v1/cloud-agents/{}", id(&json!({"id":agent}))?),
                            Some(serde_json::from_slice(&std::fs::read(file)?)?),
                        )
                        .await?
                }
                AgentCommand::Delete { id: agent } => {
                    client
                        .request(
                            Method::DELETE,
                            &format!("api/v1/cloud-agents/{}", id(&json!({"id":agent}))?),
                            None,
                        )
                        .await?
                }
            };
            if matches!(command, AgentCommand::List { json: false }) {
                print_list(
                    &value,
                    "name",
                    false,
                    "No cloud agents yet. Save one from a session with `haarness cloud session save-agent`, or create one with `haarness cloud agent create --file agent.json`.",
                );
            } else {
                println!("{value}");
            }
        }
        CloudCommand::Session { command } => match command {
            SessionCommand::List {
                json: as_json,
                agent,
            } => {
                let mut path = format!(
                    "api/v1/sessions?accountId={}",
                    client.config.selected_account().unwrap_or_default()
                );
                if let Some(agent) = agent {
                    write!(path, "&cloudAgentId={}", id(&json!({"id":agent}))?)?;
                }
                // Omitting accountId selects the signed-in personal account.
                if client.config.selected_account().is_none() {
                    path = path.replace("?accountId=", "?");
                }
                let value = list_all(&client, &path).await?;
                if *as_json {
                    println!("{value}");
                } else {
                    print_list(
                        &value,
                        "title",
                        true,
                        &no_sessions_hint(client.config.web_origin().ok()),
                    );
                }
            }
            SessionCommand::Create {
                agent,
                title,
                file,
                stopped,
                json: as_json,
            } => {
                let mut body: Value = if let Some(file) = file {
                    serde_json::from_slice(&std::fs::read(file)?)?
                } else {
                    json!({})
                };
                if !body.is_object() {
                    bail!("Session file must contain a JSON object");
                }
                if let Some(agent) = agent {
                    body["cloudAgentId"] = json!(id(&json!({"id":agent}))?);
                }
                if let Some(title) = title {
                    body["title"] = json!(title);
                }
                if *stopped {
                    body["start"] = json!(false);
                }
                if body.get("accountId").is_none() {
                    body["accountId"] = json!(client.config.selected_account());
                }
                let value = client
                    .request(Method::POST, "api/v1/sessions", Some(body))
                    .await?;
                if *as_json {
                    println!("{value}");
                } else {
                    println!(
                        "Session {} created. Use `haarness cloud session list`, then `haarness cloud session attach <id>`.",
                        value["session"]["id"].as_str().unwrap_or("")
                    );
                }
            }
            SessionCommand::Update { id: session, file } => {
                let body: Value = serde_json::from_slice(&std::fs::read(file)?)?;
                println!(
                    "{}",
                    client
                        .request(
                            Method::PATCH,
                            &format!("api/v1/sessions/{}", id(&json!({"id":session}))?),
                            Some(body)
                        )
                        .await?
                );
            }
            SessionCommand::SaveAgent {
                id: session,
                name,
                version,
                include_environment,
            } => {
                println!("{}", client.request(Method::POST, &format!("api/v1/sessions/{}/save-agent", id(&json!({"id":session}))?), Some(json!({"name":name,"version":version,"includeEnvironment":include_environment}))).await?);
            }
            SessionCommand::Show { id } => println!(
                "{}",
                rpc(&client, json!({"action":"session","id":id})).await?
            ),
            SessionCommand::Start { id } | SessionCommand::Stop { id } => {
                let action = if matches!(command, SessionCommand::Start { .. }) {
                    "session-start"
                } else {
                    "session-stop"
                };
                rpc(&client, json!({"action":action,"id":id})).await?;
                println!("Session operation requested.");
            }
            SessionCommand::Attach { id } => attach(client, id, "session-attach").await?,
        },
        CloudCommand::Workspace { command } => match command {
            WorkspaceCommand::List { json: as_json } => {
                let v = rpc(&client, json!({"action":"workspaces"})).await?;
                if *as_json {
                    println!("{v}")
                } else if let Some(items) = v["items"].as_array() {
                    if items.is_empty() {
                        println!(
                            "No Cloud workspaces. New work runs in sessions: start one with `haarness cloud session create`."
                        );
                    }
                    for w in items {
                        println!(
                            "{}  {}  {}",
                            w["id"].as_str().unwrap_or(""),
                            w["name"].as_str().unwrap_or(""),
                            w["observedState"].as_str().unwrap_or("")
                        );
                    }
                }
            }
            WorkspaceCommand::Create {
                name,
                repo,
                json: as_json,
            } => {
                let v = rpc(&client, json!({"action":"create","name":name,"repo":repo})).await?;
                if *as_json {
                    println!("{v}")
                } else {
                    println!(
                        "Creating Cloud workspace {name}. Use `haarness cloud workspace list` to see when it is ready."
                    );
                }
            }
            WorkspaceCommand::Start { id } | WorkspaceCommand::Stop { id } => {
                let action = if matches!(command, WorkspaceCommand::Start { .. }) {
                    "start"
                } else {
                    "stop"
                };
                rpc(&client, json!({"action":action,"id":id})).await?;
                println!("Workspace {action} requested.");
            }
            WorkspaceCommand::Attach { id } => {
                attach(client, id, "attach").await?;
            }
        },
    }
    Ok(())
}

async fn attach(client: Client, id: &str, action: &'static str) -> Result<()> {
    super::require_terminal()?;
    let value = rpc(&client, json!({"action":action,"id":id})).await?;
    let url = value["gatewayUrl"]
        .as_str()
        .context("Missing gateway URL")?;
    haarness_account::safe_url(url)?;
    let token = value["ticket"]
        .as_str()
        .context("Missing workspace ticket")?;
    let endpoint = std::sync::Arc::new(std::sync::RwLock::new(
        haarness_tui::api::EndpointCredentials::new(url, token),
    ));
    let shared = endpoint.clone();
    let id = id.to_owned();
    let renew = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            if let Ok(v) = rpc(&client, json!({"action":action,"id":id})).await {
                if let (Some(url), Some(token)) = (v["gatewayUrl"].as_str(), v["ticket"].as_str()) {
                    if haarness_account::safe_url(url).is_ok() {
                        *shared
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            haarness_tui::api::EndpointCredentials::new(url, token);
                    }
                }
            }
        }
    });
    let resolver = std::sync::Arc::new(move || {
        Some(
            endpoint
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        )
    });
    let result = if action == "session-attach" {
        haarness_tui::resume_attached_with_resolver(url, token, resolver).await
    } else {
        haarness_tui::run_attached_with_resolver(url, token, resolver).await
    };
    renew.abort();
    result?;
    Ok(())
}

/// Blank sessions need no agent, and Home starts one from a prompt.
fn no_sessions_hint(web_origin: Option<reqwest::Url>) -> String {
    let home = web_origin
        .and_then(|origin| origin.join("app").ok())
        .map(|home| format!(", or from Home at {home}"))
        .unwrap_or_default();
    format!("No cloud sessions yet. Start one with `haarness cloud session create`{home}.")
}

fn print_list(value: &Value, label: &str, session: bool, empty: &str) {
    if let Some(items) = value["items"].as_array() {
        for item in items {
            println!(
                "{}  {}  {}",
                item["id"].as_str().unwrap_or(""),
                item[label].as_str().unwrap_or(""),
                if session {
                    item["workspace"]["observedState"].as_str().unwrap_or("")
                } else {
                    "template"
                }
            );
        }
        if items.is_empty() {
            println!("{empty}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[tokio::test]
    async fn cloud_lists_follow_pages_and_preserve_account_filters() {
        use axum::{Json, Router, extract::Query, routing::get};
        let cursor = uuid::Uuid::now_v7().to_string();
        let app = Router::new().route(
            "/api/v1/sessions",
            get(
                move |Query(q): Query<std::collections::HashMap<String, String>>| {
                    let cursor = cursor.clone();
                    async move {
                        assert_eq!(q.get("accountId").map(String::as_str), Some("account"));
                        assert_eq!(q.get("cloudAgentId").map(String::as_str), Some("agent"));
                        Json(if let Some(value) = q.get("cursor") {
                            assert_eq!(value, &cursor);
                            json!({"items":[{"id":"second"}],"nextCursor":null})
                        } else {
                            json!({"items":[{"id":"first"}],"nextCursor":cursor})
                        })
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let dir =
            std::env::temp_dir().join(format!("haarness-cloud-pages-{}", uuid::Uuid::now_v7()));
        let config = Config::new(&url, dir.clone(), "haarness-cli").unwrap();
        haarness_account::credentials::save(
            &config.path(),
            &haarness_account::Credentials {
                api_url: url,
                access_token: "fixture-access".into(),
                refresh_token: "fixture-refresh".into(),
                access_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            },
        )
        .unwrap();
        let result = list_all(
            &Client::new(config).unwrap(),
            "api/v1/sessions?accountId=account&cloudAgentId=agent",
        )
        .await;
        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(
            result.unwrap(),
            json!({"items":[{"id":"first"},{"id":"second"}],"nextCursor":null})
        );
    }
    #[tokio::test]
    async fn desktop_relay_sign_in_has_headroom_and_renews_a_refused_one() {
        use axum::{Json, Router, routing::post};
        let renewals = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = renewals.clone();
        let app = Router::new().route(
            "/oauth/token",
            post(move || {
                let count = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                async move {
                    Json(json!({"access_token":format!("renewed-{count}"),"refresh_token":format!("refresh-{count}"),"expires_in":600}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let dir =
            std::env::temp_dir().join(format!("haarness-relay-auth-{}", uuid::Uuid::now_v7()));
        let config = Config::new(&url, dir.clone(), "haarness-cli").unwrap();
        haarness_account::credentials::save(
            &config.path(),
            &haarness_account::Credentials {
                api_url: url.clone(),
                access_token: "fixture-access".into(),
                refresh_token: "fixture-refresh".into(),
                access_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            },
        )
        .unwrap();
        let config_path = config.path();
        let client = Client::new(config).unwrap();
        let current = rpc(&client, json!({"action":"remote-auth"})).await.unwrap();
        let renewed = rpc(
            &client,
            json!({"action":"remote-auth","rejected":"fixture-access"}),
        )
        .await
        .unwrap();
        assert_eq!(current["token"], "fixture-access");
        assert_eq!(current["apiUrl"], url);
        assert!(current["expiresAt"].as_str().is_some());
        assert_eq!(renewed["token"], "renewed-1");
        assert_eq!(renewals.load(std::sync::atomic::Ordering::SeqCst), 1);
        // One that would expire within five minutes is renewed before Desktop gets it.
        haarness_account::credentials::save(
            &config_path,
            &haarness_account::Credentials {
                api_url: url.clone(),
                access_token: "almost-expired".into(),
                refresh_token: "refresh-1".into(),
                access_expires_at: chrono::Utc::now() + chrono::Duration::minutes(3),
            },
        )
        .unwrap();
        let fresh = rpc(&client, json!({"action":"remote-auth"})).await.unwrap();
        assert_eq!(fresh["token"], "renewed-2");
        let expires: chrono::DateTime<chrono::Utc> =
            serde_json::from_value(fresh["expiresAt"].clone()).unwrap();
        assert!(expires > chrono::Utc::now() + chrono::Duration::minutes(9));
        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn cloud_commands_keep_templates_sessions_and_legacy_workspaces_distinct() {
        let args = crate::Cli::try_parse_from([
            "haarness",
            "cloud",
            "session",
            "create",
            "--agent",
            "template-id",
            "--title",
            "Independent work",
            "--json",
        ])
        .unwrap();
        assert!(matches!(
            args.command,
            Some(crate::Command::Cloud(CloudArgs {
                command: CloudCommand::Session {
                    command: SessionCommand::Create { json: true, .. }
                }
            }))
        ));
        assert!(crate::Cli::try_parse_from(["haarness", "cloud", "session", "create"]).is_ok());
        assert!(
            crate::Cli::try_parse_from([
                "haarness",
                "cloud",
                "session",
                "create",
                "--file",
                "session.json",
                "--stopped"
            ])
            .is_ok()
        );
        assert!(
            crate::Cli::try_parse_from([
                "haarness",
                "cloud",
                "session",
                "update",
                "session-id",
                "--file",
                "settings.json"
            ])
            .is_ok()
        );
        assert!(
            crate::Cli::try_parse_from([
                "haarness",
                "cloud",
                "agent",
                "create",
                "--file",
                "agent.json"
            ])
            .is_ok()
        );
        assert!(crate::Cli::try_parse_from(["haarness", "cloud", "workspace", "list"]).is_ok());
    }
    #[test]
    fn an_empty_session_list_points_at_starting_one() {
        assert_eq!(
            no_sessions_hint(Some("https://cloud.example.test/".parse().unwrap())),
            "No cloud sessions yet. Start one with `haarness cloud session create`, or from Home at https://cloud.example.test/app."
        );
        assert!(!no_sessions_hint(None).contains("agent"));
    }
    #[test]
    fn identifiers_cannot_change_the_cloud_api_path() {
        assert!(id(&json!({"id":"../../api-keys"})).is_err());
        assert!(id(&json!({"id":uuid::Uuid::now_v7().to_string()})).is_ok());
    }
}
