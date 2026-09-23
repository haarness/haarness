use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use haarness_account::{Client, Config};
use reqwest::Method;
use serde_json::{Value, json};
use std::io::{Read, Write};

#[derive(Args, Clone)]
pub struct LoginArgs {
    /// Sign in on a headless machine with a browser on another device.
    #[arg(long)]
    pub device: bool,
    /// Print the sign-in link without opening a browser.
    #[arg(long)]
    pub no_browser: bool,
    #[arg(long)]
    pub json: bool,
}
#[derive(Args)]
pub struct AccountArgs {
    #[arg(long)]
    pub json: bool,
    /// Switch the account used for Cloud models and workspaces.
    #[arg(long)]
    pub select: Option<String>,
    /// Private Desktop bridge: one request on stdin; no project configuration.
    #[arg(long, hide = true)]
    pub rpc: bool,
}
#[derive(Args)]
pub struct CloudArgs {
    #[command(subcommand)]
    pub command: CloudCommand,
}
#[derive(Subcommand)]
pub enum CloudCommand {
    Login(LoginArgs),
    /// Manage persistent Cloud workspaces.
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
}
#[derive(Subcommand)]
pub enum WorkspaceCommand {
    List {
        #[arg(long)]
        json: bool,
    },
    Create {
        name: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Start {
        id: String,
    },
    Stop {
        id: String,
    },
    Attach {
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
pub async fn account(args: &AccountArgs) -> Result<()> {
    let client = client()?;
    if args.rpc {
        let mut input = String::new();
        std::io::stdin()
            .take(64 * 1024)
            .read_to_string(&mut input)?;
        let input: Value = serde_json::from_str(&input)?;
        println!("{}", rpc(&client, input).await?);
        return Ok(());
    }
    if let Some(id) = &args.select {
        client.select_account(id).await?;
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
    client()?.logout().await?;
    println!("Signed out of Haarness.");
    Ok(())
}
fn id(input: &Value) -> Result<&str> {
    let id = input["id"].as_str().context("Missing workspace id")?;
    uuid::Uuid::parse_str(id).context("Invalid workspace id")?;
    Ok(id)
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
                Some("billing") => "app/billing",
                Some("usage") => "app/billing/usage",
                _ => "app",
            };
            {
                let base = std::env::var("HAARNESS_CLOUD_WEB_ORIGIN")
                    .ok()
                    .map(|url| haarness_account::safe_url(&url))
                    .transpose()?
                    .unwrap_or_else(|| client.config.api_url.clone());
                json!({"url":base.join(page)?.to_string()})
            }
        }
        _ => bail!("Unknown account action"),
    })
}
pub async fn cloud(args: &CloudArgs) -> Result<()> {
    let client = client()?;
    match &args.command {
        CloudCommand::Login(args) => return login(args).await,
        CloudCommand::Workspace { command } => match command {
            WorkspaceCommand::List { json: as_json } => {
                let v = rpc(&client, json!({"action":"workspaces"})).await?;
                if *as_json {
                    println!("{v}")
                } else if let Some(items) = v["items"].as_array() {
                    if items.is_empty() {
                        println!(
                            "No Cloud workspaces yet. Run `haarness cloud workspace create <name>`."
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
                let value = rpc(&client, json!({"action":"attach","id":id})).await?;
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
                let id = id.clone();
                let renew = tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        if let Ok(v) = rpc(&client, json!({"action":"attach","id":id})).await {
                            if let (Some(url), Some(token)) =
                                (v["gatewayUrl"].as_str(), v["ticket"].as_str())
                            {
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
                let result = haarness_tui::run_attached_with_resolver(url, token, resolver).await;
                renew.abort();
                result?;
            }
        },
    }
    Ok(())
}
