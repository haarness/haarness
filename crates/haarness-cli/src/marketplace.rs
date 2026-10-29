//! Cloud catalog transport and verified, scoped package installation. All clients
//! (CLI, tools, Desktop, Cloud and SDK) use this adapter through management.
use crate::Paths;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
    time::Duration,
};

const MAX_ARCHIVE: usize = 256 * 1024 * 1024;
const MAX_EXPANDED: u64 = 512 * 1024 * 1024;

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(clap::Subcommand)]
enum Command {
    /// Browse public and signed-in account-private resources.
    Search {
        /// Only this kind: skill, agent, mcp, plugin or extension.
        #[arg(long)]
        kind: Option<String>,
        /// Search text.
        #[arg(long)]
        q: Option<String>,
        /// Only resources in this collection.
        #[arg(long)]
        collection: Option<String>,
        /// Continue from the `nextCursor` of an earlier page.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show one resource's details.
    Detail {
        /// Resource id, or publisher/slug.
        id: String,
        /// The resource's kind, to find it by publisher/slug.
        #[arg(long)]
        kind: Option<String>,
    },
    /// List one resource's published versions.
    Versions {
        /// Resource id, or publisher/slug.
        id: String,
        /// The resource's kind, to find it by publisher/slug.
        #[arg(long)]
        kind: Option<String>,
    },
    /// List marketplace collections.
    Collections,
    /// List resources installed from the marketplace.
    Installed {
        /// Print the install records as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Verify and install a catalog resource on this machine.
    Install {
        /// Resource kind: skill, agent, mcp, plugin or extension.
        kind: String,
        /// Resource id, or publisher/slug.
        id: String,
        /// Version number (default: the latest).
        #[arg(long)]
        version: Option<u64>,
        /// Install it for this project only.
        #[arg(long, conflicts_with = "global")]
        local: bool,
        /// Install it for all projects (the default).
        #[arg(long)]
        global: bool,
        /// Settings the resource asks for, as a JSON object.
        #[arg(long, default_value = "{}")]
        values: String,
        /// Stay on the installed version instead of following new ones.
        #[arg(long)]
        no_auto_update: bool,
    },
    /// Install another version of an installed resource.
    Update {
        /// Resource id, or publisher/slug.
        id: String,
        /// Version number (default: the latest).
        #[arg(long)]
        version: Option<u64>,
        /// The copy installed for this project.
        #[arg(long)]
        local: bool,
    },
    /// Install new versions of a resource when they are published, or stop.
    AutoUpdate {
        /// Resource id, or publisher/slug.
        id: String,
        /// Stay on the installed version.
        #[arg(long)]
        off: bool,
        /// The copy installed for this project.
        #[arg(long)]
        local: bool,
    },
    /// Install new versions of every resource that updates automatically.
    CheckUpdates,
    /// Turn an installed resource on.
    Enable {
        /// Resource id, or publisher/slug.
        id: String,
        /// The copy installed for this project.
        #[arg(long)]
        local: bool,
    },
    /// Turn an installed resource off without removing it.
    Disable {
        /// Resource id, or publisher/slug.
        id: String,
        /// The copy installed for this project.
        #[arg(long)]
        local: bool,
    },
    /// Uninstall a resource.
    Remove {
        /// Resource id, or publisher/slug.
        id: String,
        /// The copy installed for this project.
        #[arg(long)]
        local: bool,
    },
    /// Machine-readable management request, also exposed through the SDK.
    Request {
        /// The request as JSON, such as '{"action":"installed"}'.
        json: String,
    },
}

pub async fn run(paths: &Paths, workspace: &Path, trust: bool, args: Args) -> Result<()> {
    let input = match args.command {
        Command::Search {
            kind,
            q,
            collection,
            cursor,
        } => json!({"action":"search","kind":kind,"q":q,"collection":collection,"cursor":cursor}),
        Command::Detail { id, kind } => json!({"action":"detail","id":id,"kind":kind}),
        Command::Versions { id, kind } => json!({"action":"versions","id":id,"kind":kind}),
        Command::Collections => json!({"action":"collections"}),
        Command::Installed { json: false } => {
            let installed = service(paths, workspace, trust)
                .await?
                .marketplace_installed()?;
            let rows = installed.as_array().map(Vec::as_slice).unwrap_or_default();
            if rows.is_empty() {
                println!(
                    "Nothing installed from the marketplace. Find resources with `haarness marketplace search`."
                );
            }
            for row in rows {
                let text = |key: &str| row[key].as_str().unwrap_or_default().to_owned();
                println!(
                    "{}\t{}\tversion {}\t{}\t{}{}",
                    text("name"),
                    text("kind"),
                    row["version"]["number"],
                    if text("scope") == "local" {
                        "this project"
                    } else {
                        "all projects"
                    },
                    update_state(row),
                    if row["enabled"] == false { "\toff" } else { "" }
                );
            }
            return Ok(());
        }
        Command::Installed { json: true } => json!({"action":"installed"}),
        Command::Install {
            kind,
            id,
            version,
            local,
            global: _,
            values,
            no_auto_update,
        } => {
            let mut input = json!({"action":"install","kind":kind,"id":id,"version":version,"scope":if local {"local"} else {"global"},"values":serde_json::from_str::<Value>(&values)?});
            if no_auto_update {
                input["autoUpdate"] = json!(false);
            }
            input
        }
        Command::Update { id, version, local } => {
            json!({"action":"update","id":id,"version":version,"scope":if local {"local"} else {"global"}})
        }
        Command::AutoUpdate { id, off, local } => {
            let receipt = service(paths, workspace, trust)
                .await?
                .marketplace_request(&json!({"action":"auto_update","id":id,"enabled":!off,"scope":if local {"local"} else {"global"}}))
                .await?;
            let name = receipt["name"].as_str().unwrap_or(&id);
            if off {
                println!(
                    "{name} stays on version {} until you update it.",
                    receipt["version"]["number"]
                );
            } else {
                println!("{name} now installs new versions when they are published.");
            }
            return Ok(());
        }
        Command::CheckUpdates => {
            let summary = service(paths, workspace, trust)
                .await?
                .marketplace_request(&json!({"action":"check_updates"}))
                .await?;
            for line in summary_lines(&summary) {
                println!("{line}");
            }
            return Ok(());
        }
        Command::Enable { id, local } => {
            json!({"action":"enable","id":id,"scope":if local {"local"} else {"global"}})
        }
        Command::Disable { id, local } => {
            json!({"action":"disable","id":id,"scope":if local {"local"} else {"global"}})
        }
        Command::Remove { id, local } => {
            json!({"action":"remove","id":id,"scope":if local {"local"} else {"global"}})
        }
        Command::Request { json } => serde_json::from_str(&json)?,
    };
    let service = service(paths, workspace, trust).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&service.marketplace_request(&input).await?)?
    );
    Ok(())
}

/// How an installed resource gets new versions, for `installed`.
fn update_state(row: &Value) -> &'static str {
    if !follows_new_versions(row) {
        "pinned"
    } else if row["updateHeld"].is_string() {
        "has local edits, so automatic updates wait"
    } else {
        "updates automatically"
    }
}

/// What a round of `check_updates` did, one short line each.
pub(crate) fn summary_lines(summary: &Value) -> Vec<String> {
    let list = |key: &str| summary[key].as_array().cloned().unwrap_or_default();
    let checked = summary["checked"].as_u64().unwrap_or_default();
    if checked == 0 {
        return vec!["Nothing installed from the marketplace updates automatically.".into()];
    }
    let mut lines = Vec::new();
    for row in list("updated") {
        lines.push(format!(
            "Updated {} to version {}.",
            row["name"].as_str().unwrap_or_default(),
            row["to"]
        ));
    }
    for row in list("held") {
        lines.push(format!(
            "{} has local edits, so it stays on version {}.",
            row["name"].as_str().unwrap_or_default(),
            row["version"]
        ));
    }
    for row in list("failed") {
        lines.push(format!(
            "Could not check {}: {}",
            row["name"].as_str().unwrap_or_default(),
            row["error"].as_str().unwrap_or_default()
        ));
    }
    if lines.is_empty() {
        lines.push(if checked == 1 {
            "Checked 1 resource: it is up to date.".into()
        } else {
            format!("Checked {checked} resources: all are up to date.")
        });
    }
    lines
}

pub async fn service(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
) -> Result<crate::management::ManagementService> {
    let config = crate::load_config(paths, workspace, trust)?;
    let skills = std::sync::Arc::new(haarness_server::FilesystemSkillRegistry::new_scoped(
        crate::skill_roots(paths, workspace, trust, &config),
    ));
    let mcp = crate::mcp_from_config(paths, workspace, &config).await;
    Ok(crate::management::ManagementService::new(
        paths.clone(),
        workspace.to_path_buf(),
        trust,
        skills,
        mcp,
    ))
}

pub async fn catalog(path: &str, query: &[(&str, String)]) -> Result<Value> {
    // A Cloud workspace can read only its owning account's catalog. The server
    // verifies the current generation token; it grants no billing or write API access.
    if let (Ok(base), Ok(workspace), Ok(token)) = (
        std::env::var("HAARNESS_CLOUD_MARKETPLACE_URL"),
        std::env::var("HAARNESS_CLOUD_WORKSPACE_ID"),
        std::env::var("HAARNESS_SERVER_TOKEN"),
    ) {
        let mut url = reqwest::Url::parse(&base)?.join(path)?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none(),
            "Invalid Cloud catalog address"
        );
        url.query_pairs_mut()
            .extend_pairs(query.iter().map(|(k, v)| (*k, v)));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let response = client
            .get(url)
            .header("x-haarness-workspace", workspace)
            .bearer_auth(token)
            .send()
            .await
            .map_err(unreachable)?;
        if !response.status().is_success() {
            return Err(refusal(response, "Cloud catalog request").await);
        }
        // The authorized manifest carries a short-lived object-store URL.
        // Preserve it for Cloud runtimes too; never forward runtime credentials to R2.
        return Ok(response.json().await?);
    }
    let config = haarness_account::Config::from_env()?;
    let mut url = config.api_url.join(path)?;
    url.query_pairs_mut()
        .extend_pairs(query.iter().map(|(k, v)| (*k, v)));
    if let Some(account) = config.selected_account() {
        url.query_pairs_mut().append_pair("accountId", &account);
    }
    if config.signed_in() {
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        return haarness_account::Client::new(config)?
            .request(reqwest::Method::GET, &path, None)
            .await
            .map_err(account_error);
    }
    let bytes = download(url.as_str(), 8 * 1024 * 1024).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn fetch(input: &Value) -> Result<Value> {
    let action = input["action"].as_str().unwrap_or("search");
    let mut query = Vec::new();
    for key in [
        "kind",
        "q",
        "category",
        "collection",
        "publisher",
        "cursor",
        "limit",
        "version",
    ] {
        if let Some(value) = input.get(key).filter(|v| !v.is_null()) {
            query.push((
                key,
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string()),
            ));
        }
    }
    let base = "api/v1/marketplace";
    let path = match action {
        "search" => format!("{base}/resources"),
        "collections" => format!("{base}/collections"),
        "detail" | "versions" | "manifest" | "install" | "update" => {
            let id = resolve(input).await?;
            let suffix = match action {
                "detail" => "",
                "versions" => "/versions",
                _ => "/install-manifest",
            };
            format!("{base}/resources/{id}{suffix}")
        }
        _ => bail!("Unsupported catalog action"),
    };
    catalog(&path, &query).await
}

pub(crate) async fn resolve(input: &Value) -> Result<String> {
    let id = input["id"]
        .as_str()
        .context("Resource id or publisher/slug is required")?;
    if let Ok(id) = uuid::Uuid::parse_str(id) {
        return Ok(id.to_string());
    }
    let (publisher, slug) = id
        .split_once('/')
        .context("Use a resource UUID or publisher/slug")?;
    let mut query = vec![
        ("publisher", publisher.to_owned()),
        ("q", slug.to_owned()),
        ("limit", "100".into()),
    ];
    if let Some(kind) = input["kind"].as_str() {
        query.push(("kind", kind.to_owned()));
    }
    let page = catalog("api/v1/marketplace/resources", &query).await?;
    let matches: Vec<_> = page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["slug"] == slug && item["publisher"]["slug"] == publisher)
        .collect();
    ensure!(
        matches.len() == 1,
        "Resource not found or ambiguous; specify its kind or UUID"
    );
    Ok(matches[0]["id"]
        .as_str()
        .context("Catalog resource has no id")?
        .into())
}

/// A transport failure in words a person can act on. reqwest's own text
/// ("error sending request") names neither the cause nor the fix.
fn unreachable(error: reqwest::Error) -> anyhow::Error {
    let message = if error.is_timeout() {
        "The marketplace did not answer in time. Try again."
    } else {
        "Can't reach the marketplace. Check your internet connection and try again."
    };
    tracing::debug!(error = %error.without_url(), "marketplace request failed");
    anyhow::anyhow!(message)
}

/// Cloud says why it refused (a yanked version and its reason, a missing one)
/// in the body; the status alone told people nothing they could act on.
async fn refusal(response: reqwest::Response, what: &str) -> anyhow::Error {
    let status = response.status();
    let message = response
        .bytes()
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|body| body["message"].as_str().map(str::to_owned))
        .filter(|message| !message.trim().is_empty());
    match message {
        Some(message) => {
            let mut chars = message.chars();
            let first = chars.next().map(|c| c.to_uppercase().collect::<String>());
            anyhow::anyhow!("{}{} ({status})", first.unwrap_or_default(), chars.as_str())
        }
        None => anyhow::anyhow!("{what} failed ({status})"),
    }
}

/// A signed-in catalog request's failure; transport failures read as above
/// instead of the account client's raw reqwest text.
fn account_error(error: haarness_account::Error) -> anyhow::Error {
    match error {
        haarness_account::Error::Http(error) => unreachable(error),
        error => error.into(),
    }
}

async fn download(url: &str, limit: usize) -> Result<Vec<u8>> {
    let parsed = reqwest::Url::parse(url)?;
    let private_catalog_host = std::env::var("HAARNESS_CLOUD_MARKETPLACE_URL")
        .ok()
        .and_then(|s| reqwest::Url::parse(&s).ok())
        .is_some_and(|base| base.scheme() == "http" && base.host_str() == parsed.host_str());
    ensure!(
        parsed.scheme() == "https"
            || (parsed.scheme() == "http"
                && (private_catalog_host
                    || matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))),
        "Downloads require HTTPS, loopback, or the configured private Cloud host"
    );
    ensure!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "URL credentials are not allowed"
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut request = client.get(parsed.clone());
    if let (Ok(base), Ok(workspace), Ok(token)) = (
        std::env::var("HAARNESS_CLOUD_MARKETPLACE_URL"),
        std::env::var("HAARNESS_CLOUD_WORKSPACE_ID"),
        std::env::var("HAARNESS_SERVER_TOKEN"),
    ) {
        let base = reqwest::Url::parse(&base)?;
        let segments = parsed
            .path()
            .trim_matches('/')
            .split('/')
            .collect::<Vec<_>>();
        if parsed.origin() == base.origin()
            && segments.len() == 6
            && segments[..4] == ["api", "v1", "marketplace", "resources"]
            && uuid::Uuid::parse_str(segments[4]).is_ok()
            && segments[5] == "artifact"
        {
            request = request
                .header("x-haarness-workspace", workspace)
                .bearer_auth(token);
        }
    }
    let mut response = request.send().await.map_err(unreachable)?;
    if !response.status().is_success() {
        return Err(refusal(response, "Marketplace download").await);
    }
    ensure!(
        response.content_length().unwrap_or_default() <= limit as u64,
        "Download exceeds size limit"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)?
    {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= limit,
            "Download exceeds size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn validate_manifest(manifest: &Value) -> Result<(&str, &str, &str)> {
    ensure!(
        manifest["manifestVersion"] == 1,
        "Unsupported install manifest version"
    );
    ensure!(
        manifest["version"]["status"] == "approved" && manifest["version"]["yanked"] == false,
        "Only approved, non-yanked versions can be installed"
    );
    let id = manifest["resource"]["id"]
        .as_str()
        .context("Missing resource id")?;
    uuid::Uuid::parse_str(id)?;
    let kind = manifest["resource"]["kind"]
        .as_str()
        .context("Missing resource kind")?;
    ensure!(
        matches!(kind, "skill" | "agent" | "plugin" | "mcp" | "extension"),
        "Unknown resource kind"
    );
    let slug = manifest["resource"]["slug"]
        .as_str()
        .context("Missing resource slug")?;
    safe_name(slug)?;
    ensure!(
        manifest["lock"]["resourceId"] == id
            && manifest["lock"]["versionId"] == manifest["version"]["id"]
            && manifest["lock"]["contentDigest"] == manifest["version"]["contentDigest"],
        "Manifest lock does not match resource/version"
    );
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))?;
    for (key, lower) in [("minRuntimeVersion", true), ("maxRuntimeVersion", false)] {
        if let Some(version) = manifest["compatibility"][key].as_str() {
            let version = semver::Version::parse(version)?;
            ensure!(
                if lower {
                    current >= version
                } else {
                    current <= version
                },
                "Resource is incompatible with this Haarness version"
            );
        }
    }
    if let Some(platforms) = manifest["compatibility"]["platforms"]
        .as_array()
        .filter(|p| !p.is_empty())
    {
        let platform = if cfg!(target_os = "macos") {
            "darwin"
        } else {
            std::env::consts::OS
        };
        ensure!(
            platforms.iter().any(|p| p == platform || p == "all"),
            "Resource does not support this platform"
        );
    }
    ensure!(
        !manifest["install"].is_null(),
        "This resource has manual installation instructions; it cannot be loaded as a runtime extension"
    );
    Ok((kind, slug, id))
}

fn safe_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
        "Invalid package name"
    );
    Ok(())
}

fn extract(bytes: &[u8], root: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    ensure!(archive.len() <= 20_000, "Archive has too many entries");
    let mut total = 0_u64;
    let mut seen = BTreeSet::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let path = file
            .enclosed_name()
            .context("Archive path escapes its root")?;
        ensure!(
            !path.as_os_str().is_empty()
                && !file.name().contains('\\')
                && path.components().all(|c| matches!(c, Component::Normal(_))),
            "Invalid archive path"
        );
        ensure!(seen.insert(path.clone()), "Duplicate archive entry");
        let mode = file.unix_mode().unwrap_or_default() & 0o170_000;
        ensure!(
            mode == 0 || mode == 0o100_000 || mode == 0o040_000,
            "Links and special files are not allowed in marketplace archives"
        );
        total = total
            .checked_add(file.size())
            .context("Archive size overflow")?;
        ensure!(total <= MAX_EXPANDED, "Expanded archive is too large");
        let target = root.join(path);
        if file.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        fs::create_dir_all(target.parent().context("Archive file parent")?)?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(MAX_EXPANDED + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 == file.size(), "Archive size mismatch");
        fs::write(&target, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &target,
                fs::Permissions::from_mode(if file.unix_mode().unwrap_or_default() & 0o111 != 0 {
                    0o700
                } else {
                    0o600
                }),
            )?;
        }
    }
    Ok(())
}

pub fn receipt_root(paths: &Paths, workspace: &Path, local: bool) -> PathBuf {
    if local {
        workspace.join(".haarness/marketplace")
    } else {
        paths.config_dir().join("marketplace")
    }
}

pub fn installed(paths: &Paths, workspace: &Path, trust: bool) -> Result<Value> {
    let mut rows = Vec::new();
    for local in [false, true] {
        if local && !trust {
            continue;
        }
        let root = receipt_root(paths, workspace, local);
        let entries = match fs::read_dir(root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                haarness_profiles::check_path(&path)?;
                let row: Value = serde_json::from_slice(&fs::read(&path)?)?;
                ensure!(
                    path.file_stem().and_then(|s| s.to_str()) == row["id"].as_str(),
                    "Invalid installation receipt id"
                );
                ensure!(
                    row["scope"] == if local { "local" } else { "global" },
                    "Invalid installation receipt scope"
                );
                validate_receipt(paths, workspace, &row)?;
                rows.push(row);
            }
        }
    }
    Ok(json!(rows))
}

/// Receipts cannot authorize removal or updates outside their managed package root.
pub fn validate_receipt(paths: &Paths, workspace: &Path, receipt: &Value) -> Result<()> {
    uuid::Uuid::parse_str(receipt["id"].as_str().context("Invalid receipt id")?)?;
    let name = receipt["name"].as_str().context("Invalid receipt name")?;
    let path = Path::new(receipt["path"].as_str().context("Invalid receipt path")?);
    ensure!(
        matches!(receipt["scope"].as_str(), Some("local" | "global")),
        "Invalid receipt scope"
    );
    let local = receipt["scope"] == "local";
    let root = if local {
        workspace.join(".haarness")
    } else {
        paths.config_dir().to_path_buf()
    };
    if receipt["kind"] == "extension" {
        ensure!(
            path.starts_with(paths.data.join("extensions/packages")),
            "Invalid extension installation path"
        );
    } else {
        safe_name(name)?;
        let target = match receipt["kind"].as_str() {
            Some("skill") => root.join("skills").join(name),
            Some("agent") => root.join("agents").join(format!("{name}.md")),
            Some("plugin" | "mcp") => {
                let plugin = if receipt["kind"] == "mcp" {
                    format!("mcp-{name}")
                } else {
                    name.into()
                };
                ensure!(receipt["pluginName"] == plugin, "Invalid plugin receipt");
                root.join(if local { "plugins" } else { "plugins/local" })
                    .join(plugin)
            }
            _ => bail!("Invalid receipt kind"),
        };
        ensure!(
            path == target,
            "Installation receipt points outside its managed package"
        );
    }
    ensure!(
        path.is_absolute() && !path.components().any(|c| matches!(c, Component::ParentDir)),
        "Invalid installation path"
    );
    haarness_profiles::check_path(path)?;
    Ok(())
}

/// Stage away from runtime discovery, validate completely, then atomically publish.
/// Existing non-marketplace content and locally modified installs are preserved.
pub async fn install(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    manifest: &Value,
    input: &Value,
) -> Result<Value> {
    let (kind, slug, id) = validate_manifest(manifest)?;
    ensure!(
        input["scope"].is_null() || matches!(input["scope"].as_str(), Some("local" | "global")),
        "Scope must be local or global"
    );
    let local = input["scope"] == "local";
    ensure!(!local || trust, "Project configuration is turned off");
    let root = if local {
        workspace.join(".haarness")
    } else {
        paths.config_dir().to_path_buf()
    };
    let receipt_file = receipt_root(paths, workspace, local).join(format!("{id}.json"));
    haarness_profiles::check_path(&receipt_file)?;
    let old: Option<Value> = if receipt_file.exists() {
        Some(serde_json::from_slice(&fs::read(&receipt_file)?)?)
    } else {
        None
    };
    if let Some(old) = &old {
        validate_receipt(paths, workspace, old)?;
        ensure!(
            old["id"] == id && old["kind"] == kind,
            "Installation receipt does not match resource"
        );
    }
    let name = input["name"]
        .as_str()
        .or_else(|| old.as_ref().and_then(|r| r["name"].as_str()))
        .unwrap_or(slug);
    if kind != "extension" {
        safe_name(name)?;
    }
    if let Some(old) = &old {
        ensure!(
            old["name"] == name,
            "An existing installation cannot be renamed during update"
        );
    }
    let stage = receipt_root(paths, workspace, local)
        .join(".staging")
        .join(uuid::Uuid::now_v7().to_string());
    haarness_profiles::check_path(&stage)?;
    fs::create_dir_all(&stage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o700))?;
    }
    let result = async {
        let unpacked = stage.join("unpacked");
        fs::create_dir(&unpacked)?;
        let mut npm_archive = None;
        let artifact = if kind == "extension" {
            manifest["install"]["extension"].get("artifact").filter(|v| !v.is_null())
                .or_else(|| manifest.get("artifact").filter(|v| !v.is_null()))
        } else { manifest.get("artifact").filter(|v| !v.is_null()) };
        if let Some(artifact) = artifact {
            let bytes = download(artifact["url"].as_str().context("Missing artifact URL")?, MAX_ARCHIVE).await?;
            ensure!(artifact["sizeBytes"].as_u64() == Some(bytes.len() as u64), "Artifact size mismatch");
            ensure!(artifact["sha256"].as_str() == Some(format!("{:x}", Sha256::digest(&bytes)).as_str()), "Artifact checksum mismatch");
            if kind == "extension" {
                use base64::Engine as _;
                let integrity = format!("sha512-{}", base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(&bytes)));
                ensure!(manifest["install"]["extension"]["integrity"] == integrity, "Extension package integrity mismatch");
                let path = stage.join("package.tgz");
                fs::write(&path, bytes)?;
                npm_archive = Some(path);
            } else {
                let destination=unpacked.clone();
                tokio::task::spawn_blocking(move || extract(&bytes,&destination)).await??;
            }
        } else { ensure!(matches!(kind,"mcp"|"extension"), "This resource requires an artifact"); }
        let (source, target) = match kind {
            "skill" => {
                let source = if unpacked.join("SKILL.md").is_file() {unpacked.clone()} else {unpacked.join(slug)};
                let text = fs::read_to_string(source.join("SKILL.md")).context("Skill archive must contain SKILL.md")?;
                let (front, _) = haarness_core::definitions::markdown(&text)?;
                ensure!(front.get("name").is_none_or(|n| n == name), "Skill name must match its installation name");
                (source,root.join("skills").join(name))
            }
            "agent" => {
                let files: Vec<_> = fs::read_dir(&unpacked)?.collect::<std::io::Result<Vec<_>>>()?;
                ensure!(files.len() == 1 && files[0].path().extension().is_some_and(|e|e=="md"), "Agent archive must contain one Markdown file");
                haarness_core::definitions::markdown(&fs::read_to_string(files[0].path())?)?;
                (files[0].path(),root.join("agents").join(format!("{name}.md")))
            }
            "plugin" => {
                let source = if unpacked.join(".claude-plugin/plugin.json").exists() {unpacked.clone()} else {unpacked.join(slug)};
                let plugin: Value = serde_json::from_slice(&fs::read(source.join(".claude-plugin/plugin.json")).context("Plugin archive needs .claude-plugin/plugin.json")?)?;
                ensure!(plugin["name"] == name, "Plugin manifest name must match its installation name");
                crate::plugins::validate_plugin_tree(&source)?;
                (source,root.join(if local {"plugins"} else {"plugins/local"}).join(name))
            }
            "mcp" => {
                // A dedicated, discoverable bundle owns config and optional support files.
                // This lets unload remove the whole server without losing its settings.
                let bundle = stage.join("bundle");
                // The catalog scanner accepts a single enclosing ZIP directory.
                // Keep CLAUDE_PLUGIN_ROOT relative to the same .mcp.json directory.
                let mut source = unpacked.clone();
                if !source.join(".mcp.json").is_file() {
                    let entries = fs::read_dir(&source)?.collect::<std::io::Result<Vec<_>>>()?;
                    if entries.len() == 1 && entries[0].path().join(".mcp.json").is_file() {
                        source = entries[0].path();
                    }
                }
                fs::rename(&source, &bundle)?;
                let mut config = manifest["install"]["mcp"]["config"].clone();
                ensure!(config.is_object(), "MCP configuration must be an object");
                substitute(&mut config, input.get("values").unwrap_or(&Value::Null))?;
                if config.get("cwd").is_some() { bail!("Marketplace MCP cwd must use its bundle directory through CLAUDE_PLUGIN_ROOT"); }
                fs::create_dir_all(bundle.join(".claude-plugin"))?;
                haarness_profiles::write(&bundle.join(".claude-plugin/plugin.json"), &serde_json::to_vec(&json!({"name":format!("mcp-{name}"),"description":manifest["resource"]["name"]}))?, true)?;
                haarness_profiles::write(&bundle.join(".mcp.json"), &serde_json::to_vec_pretty(&json!({"mcpServers":{name:config}}))?, true)?;
                (bundle, root.join(if local {"plugins"} else {"plugins/local"}).join(format!("mcp-{name}")))
            }
            "extension" => return crate::extensions::marketplace_install(paths, workspace, trust, local, manifest, input, old.as_ref(), &receipt_file, npm_archive.as_deref()).await,
            _ => unreachable!(),
        };
        let _lock = crate::desktop::editor_lock(&paths.data, "marketplace")?;
        haarness_profiles::check_path(&target)?;
        ensure!(old.is_some() || !target.exists(), "A resource already exists with this name; choose another name or remove it explicitly");
        if let Some(old) = &old {
            ensure!(old["path"].as_str() == target.to_str(), "Installation scope/path changed");
            if target.exists() { ensure!(old["installedDigest"] == tree_digest(&target)?, "Resource was modified locally; preserve your edits before updating"); }
        }
        let digest = tree_digest(&source)?;
        let receipt = json!({"id":id,"kind":kind,"name":name,"publisher":manifest["resource"]["publisher"],"slug":slug,"scope":if local {"local"} else {"global"},"path":target,"version":manifest["version"],"lock":manifest["lock"],"installedDigest":digest,"pluginName":if kind=="mcp" {Some(format!("mcp-{name}"))} else if kind=="plugin" {Some(name.to_owned())} else {None},"autoUpdate":auto_update_choice(input, old.as_ref()),"restartRequired":false});
        fs::create_dir_all(target.parent().context("Target parent")?)?;
        let backup = stage.join("previous");
        let existed = target.exists();
        // Preserve disabled plugin state across version changes (digest ignores marker).
        if target.is_dir() && target.join(".haarness-disabled").exists() { fs::write(source.join(".haarness-disabled"), "disabled\n")?; }
        if existed { fs::rename(&target, &backup)?; }
        if let Err(error) = fs::rename(&source, &target).and_then(|()| haarness_profiles::write(&receipt_file, &serde_json::to_vec_pretty(&receipt).unwrap_or_default(), true).map_err(std::io::Error::other)) {
            if target.is_dir() { let _ = fs::remove_dir_all(&target); } else { let _ = fs::remove_file(&target); }
            if existed { let _ = fs::rename(&backup, &target); }
            return Err(error.into());
        }
        if old.is_none() && matches!(kind,"skill"|"agent") {
            crate::capability_state::set(paths,workspace,trust,kind,name,true,local)?;
        }
        Ok(receipt)
    }.await;
    let _ = fs::remove_dir_all(stage);
    result
}

/// Whether a receipt follows new versions on its own. A receipt written
/// before automatic updates existed has no field and stays where it is.
pub fn follows_new_versions(receipt: &Value) -> bool {
    receipt["autoUpdate"].as_bool().unwrap_or(false)
}

/// `autoUpdate` for the receipt an install writes: what the request says,
/// else what the installed copy already did, else on for a new install of
/// the latest version and off for one that names its version.
pub(crate) fn auto_update_choice(input: &Value, old: Option<&Value>) -> bool {
    input["autoUpdate"].as_bool().unwrap_or_else(|| match old {
        Some(old) => follows_new_versions(old),
        None => input["version"].is_null(),
    })
}

/// The receipt file of `receipt`, read again and checked as `installed`
/// checks it. Callers hold the marketplace editor lock.
fn reread_receipt(paths: &Paths, workspace: &Path, receipt: &Value) -> Result<(PathBuf, Value)> {
    let id = uuid::Uuid::parse_str(receipt["id"].as_str().context("Invalid receipt id")?)?;
    let local = match receipt["scope"].as_str() {
        Some("local") => true,
        Some("global") => false,
        _ => bail!("Invalid receipt scope"),
    };
    let file = receipt_root(paths, workspace, local).join(format!("{id}.json"));
    haarness_profiles::check_path(&file)?;
    let current: Value =
        serde_json::from_slice(&fs::read(&file).context("The resource is no longer installed")?)?;
    ensure!(
        current["id"] == id.to_string() && current["scope"] == receipt["scope"],
        "Invalid installation receipt"
    );
    validate_receipt(paths, workspace, &current)?;
    Ok((file, current))
}

/// Turn automatic updates on or off for one installed resource.
pub fn set_auto_update(
    paths: &Paths,
    workspace: &Path,
    receipt: &Value,
    enabled: bool,
) -> Result<Value> {
    let _lock = crate::desktop::editor_lock(&paths.data, "marketplace")?;
    let (file, mut current) = reread_receipt(paths, workspace, receipt)?;
    current["autoUpdate"] = json!(enabled);
    if !enabled && let Some(fields) = current.as_object_mut() {
        // A hold explains why an automatic update waits; there is none to wait now.
        fields.remove("updateHeld");
    }
    haarness_profiles::write(&file, &serde_json::to_vec_pretty(&current)?, true)?;
    Ok(current)
}

/// Record (`Some`) or clear (`None`) why an automatic update waits. Writes
/// only when that changes and the same version is still installed; returns
/// whether it wrote.
fn set_update_held(
    paths: &Paths,
    workspace: &Path,
    receipt: &Value,
    reason: Option<&str>,
) -> Result<bool> {
    let _lock = crate::desktop::editor_lock(&paths.data, "marketplace")?;
    let (file, mut current) = reread_receipt(paths, workspace, receipt)?;
    if current["version"]["id"] != receipt["version"]["id"]
        || current["updateHeld"].as_str() == reason
    {
        return Ok(false);
    }
    match reason {
        Some(reason) => current["updateHeld"] = json!(reason),
        None => {
            if let Some(fields) = current.as_object_mut() {
                fields.remove("updateHeld");
            }
        }
    }
    haarness_profiles::write(&file, &serde_json::to_vec_pretty(&current)?, true)?;
    Ok(true)
}

/// Whether the installed files differ from what the marketplace installed.
pub fn locally_modified(receipt: &Value) -> Result<bool> {
    let path = if receipt["kind"] == "extension" {
        receipt["packageRoot"]
            .as_str()
            .or_else(|| receipt["path"].as_str())
    } else {
        receipt["path"].as_str()
    }
    .map(Path::new)
    .context("Invalid receipt path")?;
    Ok(path.exists() && receipt["installedDigest"] != tree_digest(path)?)
}

/// One round of automatic updates over the receipts of `scope` (both when
/// `None`) that follow new versions. A locally modified install is never
/// replaced: the hold is recorded on its receipt instead. A resource the
/// catalog cannot answer for is left for the next round. Returns the
/// summary and whether anything on disk changed.
pub async fn check_updates(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    scope: Option<&str>,
) -> Result<(Value, bool)> {
    check_updates_from(&CloudCatalog, paths, workspace, trust, scope).await
}

/// Where a round of automatic updates reads resource details and install
/// manifests: Haarness Cloud, or a stand-in in tests.
#[async_trait::async_trait]
pub(crate) trait CatalogReader: Sync {
    /// `GET api/v1/marketplace/resources/{id}`.
    async fn detail(&self, id: &str) -> Result<Value>;
    /// The install manifest `fetch` reads for `input`.
    async fn manifest(&self, input: &Value) -> Result<Value>;
}

struct CloudCatalog;

#[async_trait::async_trait]
impl CatalogReader for CloudCatalog {
    async fn detail(&self, id: &str) -> Result<Value> {
        catalog(&format!("api/v1/marketplace/resources/{id}"), &[]).await
    }
    async fn manifest(&self, input: &Value) -> Result<Value> {
        fetch(input).await
    }
}

pub(crate) async fn check_updates_from(
    reader: &dyn CatalogReader,
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    scope: Option<&str>,
) -> Result<(Value, bool)> {
    let rows = installed(paths, workspace, trust)?;
    let mut checked = 0;
    let mut changed = false;
    let (mut updated, mut held, mut failed) = (Vec::new(), Vec::new(), Vec::new());
    for row in rows.as_array().into_iter().flatten() {
        if !follows_new_versions(row) || scope.is_some_and(|scope| row["scope"] != scope) {
            continue;
        }
        checked += 1;
        let id = row["id"].as_str().unwrap_or_default();
        let name = row["name"].as_str().unwrap_or(id);
        let about = |extra: Value| {
            let mut value = json!({"id":id,"name":name,"kind":row["kind"],"scope":row["scope"],"version":row["version"]["number"]});
            if let (Some(value), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
                value.extend(extra.clone());
            }
            value
        };
        let modified = match locally_modified(row) {
            Ok(modified) => modified,
            Err(error) => {
                failed.push(about(json!({"error":error.to_string()})));
                continue;
            }
        };
        // Edits undone since the last round: nothing waits any more.
        if !modified && row["updateHeld"].is_string() {
            changed |= recorded(set_update_held(paths, workspace, row, None));
        }
        let detail = match reader.detail(id).await {
            Ok(detail) => detail,
            Err(error) => {
                tracing::info!(resource = %id, error = %error, "automatic update check skipped");
                failed.push(about(json!({"error":error.to_string()})));
                continue;
            }
        };
        let active = &detail["activeVersion"];
        let newer = active.is_object()
            && active["id"] != row["version"]["id"]
            && active["yanked"] != true
            && active["status"]
                .as_str()
                .is_none_or(|status| status == "approved");
        if !newer {
            continue;
        }
        if modified {
            changed |= recorded(set_update_held(paths, workspace, row, Some("modified")));
            held.push(about(json!({"available":active["number"]})));
            continue;
        }
        // The exact version the detail named, so a version published
        // meanwhile is not installed unseen.
        let input = json!({"action":"update","id":id,"kind":row["kind"],"version":active["number"],"scope":row["scope"]});
        let result = async {
            let manifest = reader.manifest(&input).await?;
            ensure!(
                manifest["version"]["id"] == active["id"],
                "The catalog changed while checking; trying again next time"
            );
            install(paths, workspace, trust, &manifest, &input).await
        }
        .await;
        match result {
            Ok(receipt) => {
                changed = true;
                tracing::info!(resource = %id, version = %receipt["version"]["number"], "installed a new version automatically");
                updated.push(about(
                    json!({"from":row["version"]["number"],"to":receipt["version"]["number"]}),
                ));
            }
            Err(error) => {
                tracing::info!(resource = %id, error = %error, "automatic update failed");
                failed.push(about(json!({"error":error.to_string()})));
            }
        }
    }
    Ok((
        json!({"checked":checked,"updated":updated,"held":held,"failed":failed}),
        changed,
    ))
}

/// Whether a hold was written; a receipt that cannot be rewritten is logged
/// and the round goes on with the others.
fn recorded(result: Result<bool>) -> bool {
    result.unwrap_or_else(|error| {
        tracing::warn!(error = %error, "could not record why an automatic update waits");
        false
    })
}

fn substitute(value: &mut Value, values: &Value) -> Result<()> {
    match value {
        Value::String(text) => {
            // Resolve declared placeholders without invoking a shell or changing
            // JSON structure, in one pass over the configured text: a value is
            // inserted as typed, so a password holding `${OTHER}` or `${` is
            // never read as another placeholder.
            let mut filled = String::with_capacity(text.len());
            let mut rest = text.as_str();
            while let Some(start) = rest.find("${") {
                filled.push_str(&rest[..start]);
                let after = &rest[start + 2..];
                let Some((name, tail)) = after.split_once('}') else {
                    bail!("Provide a value for {after}: this MCP server needs it to connect");
                };
                if name == "CLAUDE_PLUGIN_ROOT" {
                    // Left for the plugin loader.
                    filled.push_str(&rest[start..start + 2]);
                    rest = after;
                    continue;
                }
                let Some(value) = values.get(name).and_then(Value::as_str) else {
                    bail!("Provide a value for {name}: this MCP server needs it to connect");
                };
                filled.push_str(value);
                rest = tail;
            }
            filled.push_str(rest);
            *text = filled;
        }
        Value::Array(items) => {
            for item in items {
                substitute(item, values)?;
            }
        }
        Value::Object(items) => {
            for item in items.values_mut() {
                substitute(item, values)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn tree_digest(root: &Path) -> Result<String> {
    fn walk(path: &Path, relative: &Path, hash: &mut Sha256) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "Installed resource contains a symbolic link"
        );
        if metadata.is_dir() {
            let mut children = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            children.sort_by_key(std::fs::DirEntry::file_name);
            for child in children {
                if child.file_name() != ".haarness-disabled" {
                    walk(&child.path(), &relative.join(child.file_name()), hash)?;
                }
            }
        } else {
            ensure!(metadata.is_file(), "Unsupported installed file type");
            hash.update(relative.to_string_lossy().as_bytes());
            hash.update([0]);
            hash.update(metadata.len().to_le_bytes());
            hash.update(fs::read(path)?);
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    walk(root, Path::new(""), &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn a_refused_download_says_why() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .route(
                "/yanked",
                axum::routing::get(|| async {
                    (
                        axum::http::StatusCode::GONE,
                        r#"{"code":"gone","message":"version 1 of demo was yanked: leaks a token"}"#,
                    )
                }),
            )
            .route(
                "/bare",
                axum::routing::get(|| async { axum::http::StatusCode::BAD_GATEWAY }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let yanked = download(&format!("http://{address}/yanked"), 1024)
            .await
            .unwrap_err();
        assert_eq!(
            yanked.to_string(),
            "Version 1 of demo was yanked: leaks a token (410 Gone)"
        );
        let bare = download(&format!("http://{address}/bare"), 1024)
            .await
            .unwrap_err();
        assert_eq!(
            bare.to_string(),
            "Marketplace download failed (502 Bad Gateway)"
        );
        server.abort();
    }

    #[tokio::test]
    async fn an_unreachable_marketplace_is_explained_in_words() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let offline = reqwest::get(format!("http://127.0.0.1:{port}/api/v1/marketplace"))
            .await
            .unwrap_err();
        // Signed in, the account client's error used to end in reqwest's
        // "error sending request for url (…)".
        assert_eq!(
            account_error(haarness_account::Error::Http(offline)).to_string(),
            "Can't reach the marketplace. Check your internet connection and try again."
        );
        assert!(
            account_error(haarness_account::Error::NotLoggedIn)
                .to_string()
                .starts_with("Sign in to Haarness")
        );
    }

    fn archive(files: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, body) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
    async fn manifest(kind: &str, bytes: Vec<u8>) -> (Value, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let sha = format!("{:x}", Sha256::digest(&bytes));
        let size = bytes.len();
        let app = axum::Router::new().route(
            "/artifact",
            axum::routing::get(move || {
                let bytes = bytes.clone();
                async move { bytes }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let id = uuid::Uuid::now_v7();
        let version = uuid::Uuid::now_v7();
        (
            json!({"manifestVersion":1,"resource":{"id":id,"kind":kind,"slug":"demo","name":"Demo"},"version":{"id":version,"number":1,"status":"approved","yanked":false},"compatibility":{"platforms":[]},"capabilities":{},"artifact":{"url":format!("http://{address}/artifact"),"sizeBytes":size,"sha256":sha},"install":{kind:{}},"lock":{"resourceId":id,"versionId":version}}),
            server,
        )
    }

    #[tokio::test]
    async fn mcp_support_files_keep_the_same_bundle_root_with_or_without_a_zip_wrapper() {
        let root =
            std::env::temp_dir().join(format!("haarness-mcp-bundle-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let paths = Paths::new(Some(&root.join("data"))).unwrap();
        for (index, prefix) in ["", "wrapped/"].into_iter().enumerate() {
            let workspace = root.join(format!("workspace-{index}"));
            fs::create_dir_all(&workspace).unwrap();
            let config = json!({"command":"python3", "args":["${CLAUDE_PLUGIN_ROOT}/server.py"]});
            let bytes = archive(&[
                (
                    &format!("{prefix}.mcp.json"),
                    &json!({"mcpServers":{"demo":config}}).to_string(),
                ),
                (&format!("{prefix}server.py"), "print('MCP_BUNDLE_PROOF')"),
            ]);
            let (mut manifest, server) = manifest("mcp", bytes).await;
            manifest["install"]["mcp"] = json!({"config":config});
            let receipt = install(
                &paths,
                &workspace,
                true,
                &manifest,
                &json!({"scope":"local"}),
            )
            .await
            .unwrap();
            let bundle = Path::new(receipt["path"].as_str().unwrap());
            assert_eq!(
                fs::read_to_string(bundle.join("server.py")).unwrap(),
                "print('MCP_BUNDLE_PROOF')"
            );
            let installed: Value =
                serde_json::from_slice(&fs::read(bundle.join(".mcp.json")).unwrap()).unwrap();
            assert_eq!(installed["mcpServers"]["demo"], config);
            assert!(!bundle.join("wrapped").exists());
            server.abort();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn mirrored_extension_checks_integrity_before_running_package_manager() {
        let root = std::env::temp_dir().join(format!(
            "haarness-marketplace-integrity-{}",
            uuid::Uuid::now_v7()
        ));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let paths = Paths::new(Some(&root.join("data"))).unwrap();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let (mut manifest, server) = manifest("extension", b"tampered npm package".to_vec()).await;
        manifest["install"]["extension"] =
            json!({"extension_type":"core", "npm_spec":"demo@1.0.0", "integrity":"sha512-wrong"});
        let error = install(
            &paths,
            &workspace,
            true,
            &manifest,
            &json!({"scope":"local"}),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("integrity mismatch"));
        assert!(
            !paths.data.join("extensions/packages").exists(),
            "must fail before Bun runs"
        );
        assert_eq!(installed(&paths, &workspace, true).unwrap(), json!([]));
        server.abort();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn verified_scoped_install_update_preserves_edits_and_rejects_invalid_artifacts() {
        let root =
            std::env::temp_dir().join(format!("haarness-marketplace-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let paths = Paths::new(Some(&root.join("home"))).unwrap();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let bytes = archive(&[
            (
                "demo/SKILL.md",
                "---\nname: demo\ndescription: Proof\n---\nReturn marketplace-proof.\n",
            ),
            ("demo/references/proof.txt", "support file"),
        ]);
        let (mut manifest, server) = manifest("skill", bytes).await;
        let input = json!({"scope":"local"});
        let first = install(&paths, &workspace, true, &manifest, &input)
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join(".haarness/skills/demo/references/proof.txt"))
                .unwrap(),
            "support file"
        );
        assert!(!paths.skills.join("demo").exists());
        assert_eq!(
            installed(&paths, &workspace, true)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        install(&paths, &workspace, true, &manifest, &input)
            .await
            .unwrap();
        let skill = Path::new(first["path"].as_str().unwrap()).join("SKILL.md");
        fs::write(&skill, "my edits").unwrap();
        assert!(
            install(&paths, &workspace, true, &manifest, &input)
                .await
                .unwrap_err()
                .to_string()
                .contains("modified locally")
        );
        assert_eq!(fs::read_to_string(skill).unwrap(), "my edits");
        manifest["version"]["yanked"] = json!(true);
        assert!(
            install(&paths, &workspace, true, &manifest, &input)
                .await
                .unwrap_err()
                .to_string()
                .contains("non-yanked")
        );
        manifest["version"]["yanked"] = json!(false);
        manifest["artifact"]["sha256"] = json!("bad");
        assert!(
            install(&paths, &workspace, true, &manifest, &input)
                .await
                .unwrap_err()
                .to_string()
                .contains("checksum")
        );
        server.abort();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn plugin_and_agent_installs_have_valid_runtime_layouts_and_collision_protection() {
        let root = std::env::temp_dir().join(format!(
            "haarness-marketplace-kinds-{}",
            uuid::Uuid::now_v7()
        ));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let paths = Paths::new(Some(&root.join("home"))).unwrap();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        for (kind, files) in [
            (
                "plugin",
                vec![
                    (".claude-plugin/plugin.json", r#"{"name":"demo"}"#),
                    (
                        "skills/proof/SKILL.md",
                        "---\nname: proof\ndescription: plugin\n---\nproof",
                    ),
                ],
            ),
            (
                "agent",
                vec![(
                    "demo.md",
                    "---\ndescription: verifier\nmode: subagent\n---\nVerify proof",
                )],
            ),
        ] {
            let (mut manifest, server) = manifest(kind, archive(&files)).await;
            install(
                &paths,
                &workspace,
                true,
                &manifest,
                &json!({"scope":"global"}),
            )
            .await
            .unwrap();
            let id = uuid::Uuid::now_v7();
            manifest["resource"]["id"] = json!(id);
            manifest["lock"]["resourceId"] = json!(id);
            assert!(
                install(
                    &paths,
                    &workspace,
                    true,
                    &manifest,
                    &json!({"scope":"global"})
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("already exists")
            );
            server.abort();
        }
        let layers = crate::asset_layers(&paths, &workspace, true);
        assert!(layers.agents.iter().any(|a| a.id == "demo"));
        assert!(
            crate::plugins::plugin_infos(&paths.data, paths.config_dir(), &workspace, true)
                .iter()
                .any(|p| p.name == "demo")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archives_cannot_escape_or_install_links_and_placeholders_are_not_shell_expanded() {
        let root = std::env::temp_dir().join(format!(
            "haarness-marketplace-archive-{}",
            uuid::Uuid::now_v7()
        ));
        fs::create_dir_all(&root).unwrap();
        assert!(extract(&archive(&[("../escape", "bad")]), &root).is_err());
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.add_symlink("escape", "/tmp", zip::write::SimpleFileOptions::default())
            .unwrap();
        assert!(extract(&zip.finish().unwrap().into_inner(), &root).is_err());
        let mut config =
            json!({"env":{"TOKEN":"${TOKEN}"},"args":["${CLAUDE_PLUGIN_ROOT}/server.js"]});
        assert_eq!(
            substitute(&mut config.clone(), &json!({}))
                .unwrap_err()
                .to_string(),
            "Provide a value for TOKEN: this MCP server needs it to connect"
        );
        substitute(&mut config, &json!({"TOKEN":"$(do not execute)"})).unwrap();
        assert_eq!(config["env"]["TOKEN"], "$(do not execute)");
        // A value goes in as typed, even when it looks like a placeholder.
        let mut config =
            json!({"env":{"TOKEN":"${TOKEN}","USER":"${USER}"},"args":["--key=${TOKEN}"]});
        substitute(
            &mut config,
            &json!({"TOKEN":"pa${USER}ss${","USER":"me ${TOKEN}"}),
        )
        .unwrap();
        assert_eq!(config["env"]["TOKEN"], "pa${USER}ss${");
        assert_eq!(config["env"]["USER"], "me ${TOKEN}");
        assert_eq!(config["args"][0], "--key=pa${USER}ss${");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn automatic_updates_default_on_for_latest_installs_and_stay_as_they_were() {
        let old = |value: Option<bool>| {
            let mut receipt = json!({"id":"x"});
            if let Some(value) = value {
                receipt["autoUpdate"] = json!(value);
            }
            receipt
        };
        // A new install of the latest version follows new ones; one that
        // names its version stays on it; the request can say otherwise.
        assert!(auto_update_choice(&json!({}), None));
        assert!(!auto_update_choice(&json!({"version":3}), None));
        assert!(!auto_update_choice(&json!({"autoUpdate":false}), None));
        assert!(auto_update_choice(
            &json!({"version":3,"autoUpdate":true}),
            None
        ));
        // An update keeps what the installed copy did, even with a version.
        assert!(auto_update_choice(
            &json!({"version":4}),
            Some(&old(Some(true)))
        ));
        assert!(!auto_update_choice(&json!({}), Some(&old(Some(false)))));
        assert!(auto_update_choice(
            &json!({"autoUpdate":true}),
            Some(&old(Some(false)))
        ));
        // Receipts written before automatic updates have no field: they stay put.
        assert!(!follows_new_versions(&old(None)));
        assert!(!auto_update_choice(&json!({}), Some(&old(None))));
    }

    /// A catalog in memory: each resource's detail, and its manifests by
    /// version number. Offline answers every request with the transport error.
    #[derive(Default)]
    struct FakeCatalog {
        details: std::sync::Mutex<std::collections::BTreeMap<String, Value>>,
        manifests: std::sync::Mutex<std::collections::BTreeMap<(String, u64), Value>>,
        offline: std::sync::atomic::AtomicBool,
        requests: std::sync::Mutex<Vec<String>>,
    }

    impl FakeCatalog {
        fn publish(&self, manifest: &Value) {
            let id = manifest["resource"]["id"].as_str().unwrap().to_owned();
            let number = manifest["version"]["number"].as_u64().unwrap();
            self.manifests
                .lock()
                .unwrap()
                .insert((id.clone(), number), manifest.clone());
            self.details.lock().unwrap().insert(
                id.clone(),
                json!({"id":id,"activeVersion":manifest["version"]}),
            );
        }
    }

    #[async_trait::async_trait]
    impl CatalogReader for FakeCatalog {
        async fn detail(&self, id: &str) -> Result<Value> {
            self.requests.lock().unwrap().push(format!("detail {id}"));
            ensure!(
                !self.offline.load(std::sync::atomic::Ordering::SeqCst),
                "Can't reach the marketplace. Check your internet connection and try again."
            );
            self.details
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .context("not found")
        }
        async fn manifest(&self, input: &Value) -> Result<Value> {
            let id = input["id"].as_str().unwrap().to_owned();
            let number = input["version"].as_u64().context("a version number")?;
            self.requests
                .lock()
                .unwrap()
                .push(format!("manifest {id} {number}"));
            self.manifests
                .lock()
                .unwrap()
                .get(&(id, number))
                .cloned()
                .context("no such version")
        }
    }

    /// Version `number` of the skill `id` (a new resource when `None`), its
    /// instructions saying `body`.
    async fn skill_version(
        id: Option<&str>,
        number: u64,
        body: &str,
    ) -> (Value, tokio::task::JoinHandle<()>) {
        let bytes = archive(&[(
            "SKILL.md",
            &format!("---\ndescription: Proof\n---\n{body}\n"),
        )]);
        let (mut manifest, server) = manifest("skill", bytes).await;
        if let Some(id) = id {
            manifest["resource"]["id"] = json!(id);
            manifest["lock"]["resourceId"] = json!(id);
        }
        manifest["version"]["number"] = json!(number);
        (manifest, server)
    }

    #[tokio::test]
    async fn check_updates_installs_what_is_new_and_leaves_pinned_modified_yanked_and_unreachable_ones()
     {
        let root = std::env::temp_dir().join(format!(
            "haarness-marketplace-updates-{}",
            uuid::Uuid::now_v7()
        ));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let paths = Paths::new(Some(&root.join("home"))).unwrap();
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let catalog = FakeCatalog::default();
        let mut servers = Vec::new();
        let mut ids = std::collections::BTreeMap::new();
        // Version 1 of five skills: four installed as the latest, one with its version named.
        for name in ["follows", "pinned", "edited", "yanked", "offline"] {
            let (manifest, server) = skill_version(None, 1, &format!("{name} one")).await;
            servers.push(server);
            catalog.publish(&manifest);
            let mut input = json!({"scope":"local","name":name});
            if name == "pinned" {
                input["version"] = json!(1);
            }
            let receipt = install(&paths, &workspace, true, &manifest, &input)
                .await
                .unwrap();
            assert_eq!(receipt["autoUpdate"], name != "pinned", "{name}");
            ids.insert(name, receipt["id"].as_str().unwrap().to_owned());
        }
        let skill = |name: &str| workspace.join(format!(".haarness/skills/{name}/SKILL.md"));
        let receipt = |name: &str| -> Value {
            serde_json::from_slice(
                &fs::read(
                    workspace
                        .join(".haarness/marketplace")
                        .join(format!("{}.json", ids[name])),
                )
                .unwrap(),
            )
            .unwrap()
        };
        // Nothing new yet: nothing changes.
        let (summary, changed) = check_updates_from(&catalog, &paths, &workspace, true, None)
            .await
            .unwrap();
        assert_eq!(
            summary,
            json!({"checked":4,"updated":[],"held":[],"failed":[]})
        );
        assert!(!changed);

        // Version 2 of each; the yanked one's is yanked.
        for name in ["follows", "pinned", "edited", "yanked", "offline"] {
            let (mut manifest, server) =
                skill_version(Some(&ids[name]), 2, &format!("{name} two")).await;
            if name == "yanked" {
                manifest["version"]["yanked"] = json!(true);
            }
            servers.push(server);
            catalog.publish(&manifest);
        }
        fs::write(skill("edited"), "my own edits").unwrap();
        let (summary, changed) = {
            // The "offline" resource's detail fails; check it on its own.
            let kept = catalog
                .details
                .lock()
                .unwrap()
                .remove(&ids["offline"])
                .unwrap();
            let result = check_updates_from(&catalog, &paths, &workspace, true, Some("local"))
                .await
                .unwrap();
            catalog
                .details
                .lock()
                .unwrap()
                .insert(ids["offline"].clone(), kept);
            result
        };
        assert!(changed);
        assert_eq!(summary["checked"], 4);
        assert_eq!(
            summary["updated"],
            json!([{"id":ids["follows"],"name":"follows","kind":"skill","scope":"local","version":1,"from":1,"to":2}])
        );
        assert_eq!(summary["held"][0]["name"], "edited");
        assert_eq!(summary["held"][0]["available"], 2);
        assert_eq!(summary["failed"][0]["name"], "offline");
        assert!(
            fs::read_to_string(skill("follows"))
                .unwrap()
                .contains("follows two")
        );
        assert_eq!(receipt("follows")["version"]["number"], 2);
        assert_eq!(receipt("follows")["autoUpdate"], true);
        // Pinned: never asked about. Yanked: not installed.
        assert!(
            fs::read_to_string(skill("pinned"))
                .unwrap()
                .contains("pinned one")
        );
        assert!(
            !catalog
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.contains(&ids["pinned"]))
        );
        assert!(
            fs::read_to_string(skill("yanked"))
                .unwrap()
                .contains("yanked one")
        );
        // Local edits are kept, and the receipt says why the update waits.
        assert_eq!(fs::read_to_string(skill("edited")).unwrap(), "my own edits");
        assert_eq!(receipt("edited")["updateHeld"], "modified");
        assert_eq!(receipt("edited")["version"]["number"], 1);

        // Offline: the round fails quietly, changes nothing, and tries again next time.
        catalog
            .offline
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let (summary, changed) = check_updates_from(&catalog, &paths, &workspace, true, None)
            .await
            .unwrap();
        assert!(!changed);
        assert_eq!(summary["updated"], json!([]));
        assert_eq!(summary["failed"].as_array().unwrap().len(), 4);
        assert!(
            summary["failed"][0]["error"]
                .as_str()
                .unwrap()
                .starts_with("Can't reach the marketplace")
        );
        catalog
            .offline
            .store(false, std::sync::atomic::Ordering::SeqCst);

        // Edits undone: the held one updates and its hold is gone.
        fs::write(
            skill("edited"),
            "---\ndescription: Proof\n---\nedited one\n",
        )
        .unwrap();
        let (summary, _) = check_updates_from(&catalog, &paths, &workspace, true, None)
            .await
            .unwrap();
        let mut updated = summary["updated"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        updated.sort_unstable();
        assert_eq!(updated, ["edited", "offline"]);
        assert!(receipt("edited")["updateHeld"].is_null());
        assert!(
            fs::read_to_string(skill("edited"))
                .unwrap()
                .contains("edited two")
        );

        // Turned off, it is not checked; turned on again, it is.
        let off = set_auto_update(&paths, &workspace, &receipt("follows"), false).unwrap();
        assert_eq!(off["autoUpdate"], false);
        assert_eq!(receipt("follows")["autoUpdate"], false);
        let (summary, _) = check_updates_from(&catalog, &paths, &workspace, true, None)
            .await
            .unwrap();
        assert_eq!(summary["checked"], 3);
        set_auto_update(&paths, &workspace, &receipt("follows"), true).unwrap();
        assert_eq!(receipt("follows")["autoUpdate"], true);
        // An update by hand keeps the setting.
        let (manifest, server) = skill_version(Some(&ids["pinned"]), 3, "pinned three").await;
        servers.push(server);
        install(
            &paths,
            &workspace,
            true,
            &manifest,
            &json!({"scope":"local","version":3}),
        )
        .await
        .unwrap();
        assert_eq!(receipt("pinned")["autoUpdate"], false);
        assert_eq!(receipt("pinned")["version"]["number"], 3);

        // A receipt from before automatic updates (no field) is left alone.
        let mut legacy = receipt("follows");
        legacy.as_object_mut().unwrap().remove("autoUpdate");
        fs::write(
            workspace
                .join(".haarness/marketplace")
                .join(format!("{}.json", ids["follows"])),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();
        let (manifest, server) = skill_version(Some(&ids["follows"]), 3, "follows three").await;
        servers.push(server);
        catalog.publish(&manifest);
        let (summary, _) = check_updates_from(&catalog, &paths, &workspace, true, None)
            .await
            .unwrap();
        assert!(
            !summary["updated"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["name"] == "follows")
        );
        assert!(
            fs::read_to_string(skill("follows"))
                .unwrap()
                .contains("follows two")
        );
        for server in servers {
            server.abort();
        }
        fs::remove_dir_all(root).unwrap();
    }
}
