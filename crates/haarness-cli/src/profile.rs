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
    /// Print machine-readable JSON instead of a summary.
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
        /// Profile name.
        name: String,
        /// Archive file to write (default: <name>.profile.zip here).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Global configuration, this project's (local), or both.
        #[arg(long, value_enum, default_value = "both")]
        scope: Selection,
        /// Replace an existing archive file.
        #[arg(long)]
        force: bool,
        /// Also upload the saved archive to the configured catalog.
        #[arg(long)]
        cloud: bool,
    },
    /// Validate an archive and show its manifest without restoring anything.
    Inspect {
        /// Profile archive (.profile.zip).
        file: PathBuf,
    },
    /// Restore configuration into this installation and --workspace.
    Restore {
        /// Profile archive (.profile.zip).
        file: PathBuf,
        /// Overwrite files that already exist.
        #[arg(long)]
        force: bool,
        /// List the files a restore would write, and which exist, without
        /// changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Upload a validated archive. Each upload receives a new immutable ID.
    CloudPush {
        /// Profile archive (.profile.zip).
        file: PathBuf,
    },
    /// Discover stored profiles by name, newest first.
    CloudList {
        /// Only profiles whose name contains this text.
        #[arg(long, default_value = "")]
        query: String,
        /// Skip this many profiles.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Show at most this many profiles (1-100).
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Inspect a stored profile's metadata.
    CloudGet {
        /// Stored profile id.
        id: Uuid,
    },
    /// Download and validate an archive without restoring it.
    CloudDownload {
        /// Stored profile id.
        id: Uuid,
        /// Archive file to write.
        #[arg(short, long)]
        output: PathBuf,
        /// Replace an existing archive file.
        #[arg(long)]
        force: bool,
    },
    /// Download and restore a profile using the same local safety checks.
    CloudRestore {
        /// Stored profile id.
        id: Uuid,
        /// Overwrite files that already exist.
        #[arg(long)]
        force: bool,
        /// List the files a restore would write, and which exist, without
        /// changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Delete one archive from the catalog.
    CloudDelete {
        /// Stored profile id.
        id: Uuid,
    },
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
        print!("{}", summary(&args.command, &result)?);
    }
    Ok(())
}

fn restore(roots: &Roots, bytes: &[u8], force: bool, dry_run: bool) -> Result<serde_json::Value> {
    let archive = Archive::decode(bytes)?;
    // Which targets already exist, before a restore replaces them.
    let planned = archive.restore(roots, force, true)?;
    let overwrite: Vec<_> = planned
        .iter()
        .filter(|target| target.exists())
        .cloned()
        .collect();
    let targets = if dry_run {
        planned
    } else {
        archive.restore(roots, force, false)?
    };
    Ok(json!({
        "name": archive.manifest.name,
        "dryRun": dry_run,
        "targets": targets,
        "overwrite": overwrite,
    }))
}

/// The human-readable result of a profile command; `--json` prints the data.
fn summary(command: &Command, result: &serde_json::Value) -> Result<String> {
    use std::fmt::Write as _;
    let mut text = String::new();
    match command {
        Command::Save { .. } => {
            let manifest: haarness_profiles::Manifest =
                serde_json::from_value(result["manifest"].clone())?;
            writeln!(
                text,
                "Saved profile \"{}\" ({}) to {}",
                manifest.name,
                files(manifest.entries.len()),
                result["output"].as_str().unwrap_or_default()
            )?;
            if let Ok(record) =
                serde_json::from_value::<haarness_profiles::Record>(result["cloud"].clone())
            {
                writeln!(text, "Uploaded as {}", record.id)?;
            }
        }
        Command::Inspect { .. } => {
            let manifest: haarness_profiles::Manifest = serde_json::from_value(result.clone())?;
            writeln!(
                text,
                "Profile \"{}\", saved {}, {} scope, {}:",
                manifest.name,
                local_time(manifest.created_at),
                scope_name(manifest.scope),
                files(manifest.entries.len())
            )?;
            for entry in &manifest.entries {
                writeln!(text, "  {}  ({} bytes)", entry.path, entry.size)?;
            }
        }
        Command::Restore { force, .. } | Command::CloudRestore { force, .. } => {
            let paths = |key: &str| -> Vec<&str> {
                result[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .collect()
            };
            let (targets, overwrite) = (paths("targets"), paths("overwrite"));
            let name = result["name"].as_str().unwrap_or_default();
            if result["dryRun"] == true {
                writeln!(
                    text,
                    "Restoring profile \"{name}\" would write {}:",
                    files(targets.len())
                )?;
                for target in &targets {
                    let action = if overwrite.contains(target) {
                        "overwrite"
                    } else {
                        "create"
                    };
                    writeln!(text, "  {action:<9}  {target}")?;
                }
                if !force && !overwrite.is_empty() {
                    writeln!(
                        text,
                        "Add --force to overwrite the {} that already {}.",
                        files(overwrite.len()),
                        if overwrite.len() == 1 {
                            "exists"
                        } else {
                            "exist"
                        }
                    )?;
                }
            } else {
                writeln!(
                    text,
                    "Restored profile \"{name}\": wrote {} ({} replaced).",
                    files(targets.len()),
                    overwrite.len()
                )?;
            }
        }
        Command::CloudPush { .. } => {
            let record: haarness_profiles::Record = serde_json::from_value(result.clone())?;
            writeln!(
                text,
                "Uploaded profile \"{}\" as {}",
                record.name, record.id
            )?;
        }
        Command::CloudList { .. } => {
            let records: Vec<haarness_profiles::Record> = serde_json::from_value(result.clone())?;
            if records.is_empty() {
                writeln!(text, "No stored profiles.")?;
            }
            for record in records {
                writeln!(
                    text,
                    "{}  {}  {}, {} scope, {}",
                    record.id,
                    record.name,
                    local_time(record.created_at),
                    scope_name(record.scope),
                    files(record.entry_count)
                )?;
            }
        }
        Command::CloudGet { .. } => {
            let record: haarness_profiles::Record = serde_json::from_value(result.clone())?;
            writeln!(
                text,
                "Profile \"{}\" ({}): saved {}, uploaded {}, {} scope, {}, {} bytes",
                record.name,
                record.id,
                local_time(record.created_at),
                local_time(record.uploaded_at),
                scope_name(record.scope),
                files(record.entry_count),
                record.size_bytes
            )?;
        }
        Command::CloudDownload { id, .. } => {
            writeln!(
                text,
                "Downloaded {id} to {}",
                result["output"].as_str().unwrap_or_default()
            )?;
        }
        Command::CloudDelete { id } => writeln!(text, "Deleted {id}")?,
    }
    Ok(text)
}

fn files(count: usize) -> String {
    if count == 1 {
        "1 file".into()
    } else {
        format!("{count} files")
    }
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Global => "global",
        Scope::Local => "project",
        Scope::Both => "global and project",
    }
}

fn local_time(time: chrono::DateTime<chrono::Utc>) -> String {
    time.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
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
