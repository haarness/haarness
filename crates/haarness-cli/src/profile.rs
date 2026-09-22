use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand, ValueEnum};
use haarness_profiles::{Archive, MAX_ARCHIVE, Roots, Scope};
use serde_json::json;
use uuid::Uuid;

#[derive(Args)]
pub struct ProfileArgs {
    #[command(subcommand)]
    command: Command,
    /// Authenticated Haarness server URL (HTTPS; HTTP accepted only on loopback).
    #[arg(long, global = true, env = "HAARNESS_PROFILE_URL")]
    url: Option<String>,
    /// Catalog server token; prefer HAARNESS_PROFILE_TOKEN to shell history.
    #[arg(
        long,
        global = true,
        env = "HAARNESS_PROFILE_TOKEN",
        hide_env_values = true
    )]
    token: Option<String>,
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Clone, Copy, Default, ValueEnum)]
enum Selection {
    Global,
    Local,
    #[default]
    Both,
}
impl From<Selection> for Scope {
    fn from(value: Selection) -> Self {
        match value {
            Selection::Global => Self::Global,
            Selection::Local => Self::Local,
            Selection::Both => Self::Both,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Save portable configuration; excludes credential stores and runtime state.
    Save {
        name: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "both")]
        scope: Selection,
        #[arg(long)]
        force: bool,
        /// Also upload the saved archive to the configured catalog.
        #[arg(long)]
        cloud: bool,
    },
    /// Validate an archive and show its manifest without restoring anything.
    Inspect { file: PathBuf },
    /// Restore configuration into this installation and --workspace.
    Restore {
        file: PathBuf,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Upload a validated archive. Each upload receives a new immutable ID.
    CloudPush { file: PathBuf },
    /// Discover stored profiles by name, newest first.
    CloudList {
        #[arg(long, default_value = "")]
        query: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Inspect a stored profile's metadata.
    CloudGet { id: Uuid },
    /// Download and validate an archive without restoring it.
    CloudDownload {
        id: Uuid,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Download and restore a profile using the same local safety checks.
    CloudRestore {
        id: Uuid,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Delete one archive from the catalog.
    CloudDelete { id: Uuid },
}

pub async fn run(paths: &super::Paths, workspace: &Path, args: &ProfileArgs) -> Result<()> {
    let roots = Roots {
        config: paths
            .config
            .parent()
            .context("config directory unavailable")?
            .canonicalize()?,
        data: paths.data.canonicalize()?,
        project: workspace
            .canonicalize()
            .context("canonicalize profile workspace")?,
    };
    let result = match &args.command {
        Command::Save {
            name,
            output,
            scope,
            force,
            cloud,
        } => {
            // Validate cloud settings before writing a local archive when upload was requested.
            if *cloud {
                let _ = remote(args)?;
            }
            let bytes = Archive::capture(&roots, name, (*scope).into())?;
            let fallback = format!("{}.profile.zip", slug(name));
            let output = destination(output.as_deref().unwrap_or(Path::new(&fallback)))?;
            haarness_profiles::write(&output, &bytes, *force)?;
            eprintln!(
                "Profile excludes credential stores. Inline secrets in configuration or scripts are retained; inspect before sharing."
            );
            let manifest = Archive::decode(&bytes)?.manifest;
            let cloud = if *cloud {
                Some(upload(args, bytes).await?)
            } else {
                None
            };
            json!({"output":output, "manifest":manifest, "cloud":cloud})
        }
        Command::Inspect { file } => {
            serde_json::to_value(Archive::decode(&haarness_profiles::read(file)?)?.manifest)?
        }
        Command::Restore {
            file,
            force,
            dry_run,
        } => restore(&roots, &haarness_profiles::read(file)?, *force, *dry_run)?,
        Command::CloudPush { file } => upload(args, haarness_profiles::read(file)?).await?,
        Command::CloudList {
            query,
            offset,
            limit,
        } => {
            ensure!((1..=100).contains(limit), "limit must be between 1 and 100");
            let (client, mut url) = remote(args)?;
            url.query_pairs_mut()
                .append_pair("query", query)
                .append_pair("offset", &offset.to_string())
                .append_pair("limit", &limit.to_string());
            let response = send(client.get(url), args).await?;
            serde_json::from_slice(&bounded(response, 1024 * 1024).await?)?
        }
        Command::CloudGet { id } => {
            let response = request(args, &id.to_string(), false).await?;
            serde_json::from_slice(&bounded(response, 1024 * 1024).await?)?
        }
        Command::CloudDownload { id, output, force } => {
            let bytes = download(args, *id).await?;
            let output = destination(output)?;
            haarness_profiles::write(&output, &bytes, *force)?;
            json!({"id":id, "output":output})
        }
        Command::CloudRestore { id, force, dry_run } => {
            restore(&roots, &download(args, *id).await?, *force, *dry_run)?
        }
        Command::CloudDelete { id } => {
            request(args, &id.to_string(), true).await?;
            json!({"deleted":id})
        }
    };
    if args.json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }
    Ok(())
}

fn restore(roots: &Roots, bytes: &[u8], force: bool, dry_run: bool) -> Result<serde_json::Value> {
    let archive = Archive::decode(bytes)?;
    let targets = archive.restore(roots, force, dry_run)?;
    Ok(json!({"name":archive.manifest.name, "dryRun":dry_run, "targets":targets}))
}

fn slug(name: &str) -> String {
    let value = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    if value.trim_matches('-').is_empty() {
        "profile".into()
    } else {
        value
    }
}

fn destination(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path)?;
    let parent = path.parent().context("archive path has no parent")?;
    // Resolve a user-selected directory, but never resolve a symlink at the output filename.
    std::fs::create_dir_all(parent)?;
    Ok(parent
        .canonicalize()?
        .join(path.file_name().context("archive filename missing")?))
}

fn remote(args: &ProfileArgs) -> Result<(reqwest::Client, reqwest::Url)> {
    let mut url = reqwest::Url::parse(
        args.url
            .as_deref()
            .context("set --url or HAARNESS_PROFILE_URL for cloud profiles")?,
    )
    .context("invalid profile server URL")?;
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "profile server requires HTTPS (HTTP allowed only on loopback)"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "profile server URL must not contain credentials, query, or fragment"
    );
    ensure!(
        args.token.as_ref().is_some_and(|s| !s.trim().is_empty()),
        "set --token or HAARNESS_PROFILE_TOKEN"
    );
    let base = url.path().trim_end_matches('/');
    let path = if base.ends_with("/v1") {
        format!("{base}/profiles")
    } else {
        format!("{base}/v1/profiles")
    };
    url.set_path(&path);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    Ok((client, url))
}

async fn send(request: reqwest::RequestBuilder, args: &ProfileArgs) -> Result<reqwest::Response> {
    let response = request
        .bearer_auth(args.token.as_deref().context("profile token missing")?)
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .context("profile server request failed")?;
    ensure!(
        response.status().is_success(),
        "profile server returned HTTP {}",
        response.status()
    );
    Ok(response)
}

async fn bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= limit as u64),
        "profile response exceeds size limit"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)?
    {
        ensure!(
            chunk.len() <= limit.saturating_sub(bytes.len()),
            "profile response exceeds size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn upload(args: &ProfileArgs, bytes: Vec<u8>) -> Result<serde_json::Value> {
    Archive::decode(&bytes)?;
    let (client, url) = remote(args)?;
    let response = send(
        client
            .post(url)
            .header("Content-Type", "application/zip")
            .body(bytes),
        args,
    )
    .await?;
    Ok(serde_json::from_slice(
        &bounded(response, 1024 * 1024).await?,
    )?)
}

async fn request(args: &ProfileArgs, suffix: &str, delete: bool) -> Result<reqwest::Response> {
    let (client, mut url) = remote(args)?;
    url.set_path(&format!("{}/{suffix}", url.path()));
    send(
        if delete {
            client.delete(url)
        } else {
            client.get(url)
        },
        args,
    )
    .await
}

async fn download(args: &ProfileArgs, id: Uuid) -> Result<Vec<u8>> {
    let bytes = bounded(
        request(args, &format!("{id}/download"), false).await?,
        MAX_ARCHIVE,
    )
    .await?;
    Archive::decode(&bytes)?;
    Ok(bytes)
}
