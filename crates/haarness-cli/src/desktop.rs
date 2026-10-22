//! Native desktop operations remain in the CLI composition root. This process
//! never owns conversation execution; connect reuses the canonical daemon.
use super::{
    Credential, CredentialStore, FileConfig, Paths, ensure_workspace_server,
    inspect_workspace_server, replace_incompatible_workspace_server, server_activity,
};
use anyhow::{Context as _, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{
    ffi::OsStr,
    fs,
    io::{IsTerminal as _, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
};

const LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Connect,
    Projects {
        #[serde(default)]
        directories: Vec<PathBuf>,
    },
    /// Sent by `projects` to a detached copy of this command: register the
    /// folders it could not wait for, however long they take.
    Register {
        directories: Vec<PathBuf>,
    },
    Configuration,
    ReadConfiguration {
        id: String,
    },
    WriteConfiguration {
        id: String,
        text: String,
        revision: String,
    },
    Credentials,
    SetCredential {
        provider: String,
        key: String,
    },
    RemoveCredential {
        provider: String,
    },
    /// Keep an MCP server's secret in the system's keychain; its settings
    /// then name it as `${keychain:NAME}`.
    StoreMcpSecret {
        name: String,
        secret: String,
    },
    Extensions {
        #[serde(default)]
        project: bool,
    },
    Asset {
        kind: String,
        input: Value,
    },
}

pub async fn run(paths: &Paths, workspace: &Path, trust: bool) -> Result<()> {
    // Typed at a terminal, `haarness desktop` opens Haarness Desktop. Desktop
    // itself runs it with a request on standard input, which never is one.
    if std::io::stdin().is_terminal() {
        return open_desktop();
    }
    let mut bytes = Vec::new();
    std::io::stdin().take(LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= LIMIT, "Desktop request exceeds 4 MiB");
    let request: Request = serde_json::from_slice(&bytes).context("Invalid desktop request")?;
    let output = execute(paths, workspace, trust, request).await?;
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

async fn execute(paths: &Paths, workspace: &Path, trust: bool, request: Request) -> Result<Value> {
    match request {
        Request::Projects { directories } => {
            let manager = paths.workspace_manager()?;
            // A sidebar folder that does not answer within a second (a slow
            // network volume, one that went away) is left as registered
            // before instead of holding the request, and registered by a
            // detached copy of this command that can wait for it.
            let mut registered = manager.register_existing(&directories)?;
            let project_ids: std::collections::HashSet<_> = registered
                .workspaces
                .iter()
                .map(|workspace| workspace.project_id.clone())
                .collect();
            // Opening an existing linked checkout also makes its primary
            // project folder selectable, without starting either daemon.
            let roots: Vec<PathBuf> = manager
                .list(None)?
                .projects
                .into_iter()
                .filter(|project| project_ids.contains(&project.id))
                .map(|project| PathBuf::from(project.root))
                .collect();
            registered
                .pending
                .extend(manager.register_existing(&roots)?.pending);
            if !registered.pending.is_empty()
                && let Err(error) = register_later(paths, workspace, &registered.pending)
            {
                tracing::warn!(%error, "could not start registering slow project folders");
            }
            Ok(serde_json::to_value(
                manager.list(paths.workspace_id.as_deref())?,
            )?)
        }
        Request::Register { directories } => {
            let manager = paths.workspace_manager()?;
            let locks = paths.cache.join("registrations");
            fs::create_dir_all(&locks)?;
            std::thread::scope(|scope| {
                for directory in &directories {
                    let (manager, locks) = (&manager, &locks);
                    scope.spawn(move || register_waiting(manager, locks, directory));
                }
            });
            Ok(json!({}))
        }
        Request::Asset { kind, input } => {
            let service = super::management::ManagementService::new(
                paths.clone(),
                workspace.to_path_buf(),
                trust,
                std::sync::Arc::new(haarness_server::FilesystemSkillRegistry::new_scoped(
                    super::skill_roots(
                        paths,
                        workspace,
                        trust,
                        &super::load_config(paths, workspace, trust)?,
                    ),
                )),
                std::sync::Arc::new(haarness_mcp::NativeMcpRegistry::default()),
            );
            service.desktop_asset(&kind, &input).await
        }
        Request::Connect => {
            // Build freshness and wire compatibility are different concerns.
            // A compatible daemon with unfinished work must remain reachable:
            // its permission/question may be the very thing keeping it busy,
            // and replacing it would end the terminals open in it.
            if let Some(live) = inspect_workspace_server(paths, workspace).await?
                && live.info.project_trusted == Some(trust)
                && live.info.workspace.as_deref().is_some_and(|actual| {
                    super::comparable_workspace(Path::new(actual))
                        == super::comparable_workspace(workspace)
                })
                && super::server_compatibility_error(&live.info).is_none()
                && super::local_server_compatibility_error(&live).is_some()
            {
                let client =
                    haarness_tui::api::ApiClient::new(&live.discovery.url, &live.discovery.token)?;
                let activity = tokio::time::timeout(
                    std::time::Duration::from_secs(4),
                    server_activity(&client),
                )
                .await
                .context("Timed out checking existing workspace activity")??;
                if !activity.idle() {
                    return Ok(json!({"schemaVersion":1,"url":live.discovery.url,
                        "token":live.discovery.token,"workspace":workspace,
                        "workspaceId":paths.workspace_id,"upgradePending":true}));
                }
            }
            // A desktop opening can change the saved project configuration
            // setting. Apply it to an idle daemon instead of reporting a
            // trusted connection while reusing an untrusted runtime.
            if let Some(live) = inspect_workspace_server(paths, workspace).await?
                && live.info.project_trusted != Some(trust)
            {
                replace_incompatible_workspace_server(
                    paths,
                    workspace,
                    live,
                    "project configuration setting changed",
                )
                .await?;
            }
            let discovery = ensure_workspace_server(paths, workspace, trust).await?;
            Ok(
                json!({"schemaVersion":1,"url":discovery.url,"token":discovery.token,
                "workspace":workspace,"workspaceId":paths.workspace_id,"upgradePending":false}),
            )
        }
        Request::Configuration => Ok(
            json!({"files": configuration_files(paths, workspace).into_iter()
            .map(|(id, path)| json!({"id":id,"path":path,"exists":path.is_file()})).collect::<Vec<_>>(),
            "trustProject":trust,"data":paths.data,"logs":paths.data.join("logs")}),
        ),
        Request::ReadConfiguration { id } => {
            let path = configuration_path(paths, workspace, &id)?;
            let text = read(&path)?;
            Ok(json!({"id":id,"path":path,"text":text,"revision":revision(&text)}))
        }
        Request::WriteConfiguration {
            id,
            text,
            revision: expected,
        } => {
            ensure!(text.len() as u64 <= LIMIT, "Configuration exceeds 4 MiB");
            let path = configuration_path(paths, workspace, &id)?;
            validate(&path, &text)?;
            let _lock = haarness_profiles::configuration_lock(&paths.data)?;
            ensure!(
                revision(&read(&path)?) == expected,
                "Configuration changed outside this editor. Reload before saving."
            );
            write(&path, text.as_bytes())?;
            Ok(json!({"id":id,"path":path,"text":text,"revision":revision(&text)}))
        }
        Request::Credentials => {
            let entries = CredentialStore::new(paths.auth.clone()).all()?;
            Ok(
                json!(entries.into_iter().map(|(provider, credential)|
                json!({"provider":provider,"kind":credential.kind()})).collect::<Vec<_>>()),
            )
        }
        Request::SetCredential { provider, key } => {
            ensure!(
                !provider.trim().is_empty() && provider.len() <= 200,
                "Invalid provider"
            );
            ensure!(
                !key.trim().is_empty() && key.len() <= 65536 && !key.contains(['\r', '\n']),
                "Invalid API key"
            );
            CredentialStore::new(paths.auth.clone()).set(&provider, &Credential::api(key))?;
            Ok(json!({"provider":provider,"connected":true}))
        }
        Request::RemoveCredential { provider } => {
            CredentialStore::new(paths.auth.clone()).remove(&provider)?;
            Ok(json!({"provider":provider,"connected":false}))
        }
        Request::StoreMcpSecret { name, secret } => {
            use haarness_mcp::keychain;
            ensure!(
                keychain::valid_name(&name),
                "\"{name}\" is not a usable name: use letters, digits, '.', '_' and '-'"
            );
            ensure!(
                !secret.is_empty() && secret.len() <= 65536,
                "Enter the secret to store"
            );
            keychain::set(&name, &secret)
                .await
                .map_err(|error| match error {
                    keychain::Error::Missing => anyhow::anyhow!("The secret was not stored"),
                    keychain::Error::Unavailable(reason) => anyhow::anyhow!(
                        "The secret was not stored in the {}: {reason}",
                        keychain::store_name()
                    ),
                })?;
            Ok(json!({"name":name,"reference":format!("${{keychain:{name}}}")}))
        }
        Request::Extensions { project } => Ok(serde_json::to_value(
            super::extensions::scoped_config(paths, workspace, project)?,
        )?),
    }
}

fn configuration_files(paths: &Paths, workspace: &Path) -> Vec<(String, PathBuf)> {
    let global = paths.config.parent().unwrap_or(Path::new("."));
    vec![
        ("global-json".into(), global.join("haarness.json")),
        ("global-jsonc".into(), global.join("haarness.jsonc")),
        ("global-toml".into(), paths.config.clone()),
        ("project-json".into(), workspace.join("haarness.json")),
        ("project-jsonc".into(), workspace.join("haarness.jsonc")),
        (
            "project-toml".into(),
            workspace.join(".haarness/config.toml"),
        ),
        ("extensions".into(), super::extensions::config_path(paths)),
        ("project-instructions".into(), workspace.join("AGENTS.md")),
    ]
}

fn configuration_path(paths: &Paths, workspace: &Path, id: &str) -> Result<PathBuf> {
    let path = configuration_files(paths, workspace)
        .into_iter()
        .find(|(name, _)| name == id)
        .map(|(_, path)| path)
        .context("Unknown configuration document")?;
    // A project cannot redirect the settings editor into arbitrary host files.
    let root = if id.starts_with("project-") {
        workspace
    } else {
        paths.config.parent().context("Missing config directory")?
    };
    let root = root.canonicalize()?;
    let mut existing = path.as_path();
    while !existing.exists() {
        existing = existing.parent().context("Invalid configuration path")?;
    }
    ensure!(
        existing.canonicalize()?.starts_with(root),
        "Configuration symlink escapes its root"
    );
    Ok(path)
}

fn read(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    let mut file = fs::File::open(path)?.take(LIMIT + 1);
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    ensure!(text.len() as u64 <= LIMIT, "Configuration exceeds 4 MiB");
    Ok(text)
}

/// Open the installed Haarness Desktop, installing it first (as the
/// installer would) when it is missing.
fn open_desktop() -> Result<()> {
    #[cfg(all(unix, not(target_os = "macos")))]
    ensure!(
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some(),
        "Haarness Desktop needs a graphical session: DISPLAY and WAYLAND_DISPLAY are not set"
    );
    let app = match installed_desktop() {
        Some(app) => app,
        None if open_registered_desktop() => {
            println!("Opening Haarness Desktop");
            return Ok(());
        }
        None => {
            install_desktop()?;
            installed_desktop().context("Haarness Desktop was installed but cannot be found")?
        }
    };
    println!("Opening Haarness Desktop: {}", app.display());
    launch_desktop(&app)
}

/// Where the installers and packages put Desktop, the usual place first.
fn desktop_candidates(
    home: &Path,
    search_path: Option<&OsStr>,
    local_app_data: Option<&Path>,
    program_files: Option<&Path>,
) -> Vec<PathBuf> {
    let _ = (home, search_path, local_app_data, program_files);
    #[cfg(target_os = "macos")]
    return vec![
        PathBuf::from("/Applications/Haarness.app"),
        home.join("Applications/Haarness.app"),
    ];
    #[cfg(all(unix, not(target_os = "macos")))]
    return search_path
        .map(|value| {
            std::env::split_paths(value)
                .map(|directory| directory.join("haarness-desktop"))
                .collect()
        })
        .unwrap_or_else(Vec::new)
        .into_iter()
        .chain([
            home.join(".local/bin/haarness-desktop"),
            PathBuf::from("/opt/Haarness/haarness-desktop"),
        ])
        .collect();
    #[cfg(windows)]
    return [
        local_app_data.map(|root| root.join("Programs").join("Haarness").join("Haarness.exe")),
        program_files.map(|root| root.join("Haarness").join("Haarness.exe")),
    ]
    .into_iter()
    .flatten()
    .collect();
}

fn find_desktop(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|candidate| {
        if cfg!(target_os = "macos") {
            candidate.join("Contents/Info.plist").is_file()
        } else {
            candidate.is_file()
        }
    })
}

fn installed_desktop() -> Option<PathBuf> {
    let home = crate::browser::discovery::home_dir().unwrap_or_default();
    let path = std::env::var_os("PATH");
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let programs = std::env::var_os("ProgramFiles").map(PathBuf::from);
    find_desktop(desktop_candidates(
        &home,
        path.as_deref(),
        local.as_deref(),
        programs.as_deref(),
    ))
}

/// macOS also finds an app moved anywhere else by its bundle identifier.
fn open_registered_desktop() -> bool {
    cfg!(target_os = "macos")
        && ProcessCommand::new("open")
            .args(["-b", "dev.haarness.desktop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

fn launch_desktop(app: &Path) -> Result<()> {
    if cfg!(target_os = "macos") {
        let status = ProcessCommand::new("open").arg("-a").arg(app).status()?;
        ensure!(status.success(), "Could not open {}", app.display());
        return Ok(());
    }
    let mut command = ProcessCommand::new(app);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::detach_from_caller(&mut command);
    command
        .spawn()
        .with_context(|| format!("Could not start {}", app.display()))?;
    Ok(())
}

/// The installer `haarness upgrade` also embeds, asked for Desktop only, in
/// its default place, where [`installed_desktop`] looks.
fn install_desktop() -> Result<()> {
    let release_url =
        std::env::var("HAARNESS_RELEASE_URL").unwrap_or_else(|_| "https://haarness.ai".to_string());
    println!("Haarness Desktop is not installed yet; installing it from {release_url}.");
    #[cfg(unix)]
    let mut command = {
        let mut command = ProcessCommand::new("sh");
        command.arg("-c").arg(include_str!("../../../install.sh"));
        command
    };
    #[cfg(windows)]
    let mut command = {
        let mut command = ProcessCommand::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            include_str!("../../../install.ps1"),
        ]);
        command
    };
    let status = command
        .env("HAARNESS_PRODUCT", "desktop")
        .env("HAARNESS_RELEASE_URL", &release_url)
        .env_remove("HAARNESS_INSTALL_DIR")
        .status()?;
    ensure!(
        status.success(),
        "Installing Haarness Desktop failed; see the installer output above"
    );
    Ok(())
}

/// Register `directories` in a detached copy of this command, so the
/// answer to Desktop does not wait for them. Its output goes nowhere and it
/// outlives this process.
fn register_later(paths: &Paths, workspace: &Path, directories: &[PathBuf]) -> Result<()> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    if let Some(root) = &paths.root {
        command.arg("--data-dir").arg(root);
    }
    command
        .arg("--workspace")
        .arg(workspace)
        .args(["--disable-project-config", "desktop"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    crate::detach_from_caller(&mut command);
    let mut child = command.spawn()?;
    let request = json!({"action": "register", "directories": directories});
    child
        .stdin
        .take()
        .context("registration input")?
        .write_all(request.to_string().as_bytes())?;
    Ok(())
}

/// Register one folder however long it takes, and the project of a linked
/// checkout with it. A folder another registration already waits on is
/// skipped, so a folder that hangs holds one waiting process, not one per
/// Desktop request.
fn register_waiting(
    manager: &haarness_workspaces::WorkspaceManager,
    locks: &Path,
    directory: &Path,
) {
    let key = format!(
        "{:x}",
        Sha256::digest(directory.to_string_lossy().as_bytes())
    );
    let Ok(lock) = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(locks.join(&key[..16]))
    else {
        return;
    };
    if fs2::FileExt::try_lock_exclusive(&lock).is_err() || !directory.is_dir() {
        return;
    }
    let Ok(workspace) = manager.register_local(directory) else {
        return;
    };
    if let Ok(project) = manager.project_for_workspace(&workspace.id) {
        let root = Path::new(&project.root);
        if root != directory && root.is_dir() {
            let _ = manager.register_local(root);
        }
    }
}

fn revision(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn validate(path: &Path, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("toml") => {
            let _: FileConfig = toml::from_str(text).context("Invalid TOML configuration")?;
        }
        Some("json" | "jsonc") => {
            let value = jsonc_parser::parse_to_serde_value::<Value>(
                text,
                &jsonc_parser::ParseOptions::default(),
            )?;
            ensure!(value.is_object(), "Configuration must be an object");
            if path
                .file_name()
                .is_some_and(|name| name == "extensions.json")
            {
                let _: haarness_extensions::Config = serde_json::from_value(value)?;
            }
        }
        Some("md") => {}
        _ => bail!("Unsupported configuration format"),
    }
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(path.parent().context("Missing parent directory")?)?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::now_v7()));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Serialize editor read/compare/write operations across native CLI processes.
pub(super) fn editor_lock(data: &Path, resource: &str) -> anyhow::Result<std::fs::File> {
    let path = data
        .canonicalize()?
        .join(format!(".desktop-{resource}.lock"));
    haarness_profiles::check_path(&path)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    fs2::FileExt::lock_exclusive(&file)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn configuration_rejects_invalid_and_stale_writes() {
        let root =
            std::env::temp_dir().join(format!("haarness-desktop-config-{}", uuid::Uuid::now_v7()));
        let paths = Paths::new(Some(&root)).unwrap();
        let input = Request::WriteConfiguration {
            id: "global-json".into(),
            text: "{\"model\":\"echo/echo-1\"}".into(),
            revision: revision(""),
        };
        let saved = execute(&paths, &root, false, input).await.unwrap();
        for (text, rev) in [
            ("{invalid", saved["revision"].as_str().unwrap()),
            ("{}", "stale"),
        ] {
            assert!(
                execute(
                    &paths,
                    &root,
                    false,
                    Request::WriteConfiguration {
                        id: "global-json".into(),
                        text: text.into(),
                        revision: rev.into(),
                    }
                )
                .await
                .is_err()
            );
        }
        assert_eq!(
            read(&paths.config.with_file_name("haarness.json")).unwrap(),
            saved["text"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn projects_register_sidebar_folders_and_the_project_of_a_linked_checkout() {
        let root = std::env::temp_dir().join(format!(
            "haarness-desktop-projects-{}",
            uuid::Uuid::now_v7()
        ));
        let paths = Paths::new(Some(&root.join("home"))).unwrap();
        let repository = root.join("repository");
        let linked = root.join("linked");
        let plain = root.join("plain");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&plain).unwrap();
        let git = |arguments: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["-c", "user.email=t@example.invalid", "-c", "user.name=T"])
                .args(arguments)
                .output()
                .unwrap()
                .status;
            assert!(status.success(), "git {arguments:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ]);
        let listed = execute(
            &paths,
            &plain,
            false,
            Request::Projects {
                directories: vec![linked.clone(), plain.clone(), root.join("missing")],
            },
        )
        .await
        .unwrap();
        let mut workspaces: Vec<(String, Option<String>)> = listed["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|workspace| {
                (
                    workspace["name"].as_str().unwrap().to_owned(),
                    workspace["branch"].as_str().map(str::to_owned),
                )
            })
            .collect();
        workspaces.sort();
        assert_eq!(
            workspaces,
            [
                ("linked".into(), Some("linked".into())),
                ("plain".into(), None),
                ("repository".into(), Some("main".into())),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn the_detached_registration_waits_for_every_folder_and_a_linked_checkouts_project() {
        let root = std::env::temp_dir().join(format!(
            "haarness-desktop-register-{}",
            uuid::Uuid::now_v7()
        ));
        let paths = Paths::new(Some(&root.join("home"))).unwrap();
        let repository = root.join("repository");
        let linked = root.join("linked");
        let plain = root.join("plain");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&plain).unwrap();
        let git = |arguments: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["-c", "user.email=t@example.invalid", "-c", "user.name=T"])
                .args(arguments)
                .output()
                .unwrap()
                .status;
            assert!(status.success(), "git {arguments:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ]);
        let request: Request = serde_json::from_value(json!({
            "action": "register",
            "directories": [linked, plain, root.join("missing")],
        }))
        .unwrap();
        execute(&paths, &plain, false, request).await.unwrap();
        let mut names: Vec<_> = paths
            .workspace_manager()
            .unwrap()
            .registered()
            .unwrap()
            .into_iter()
            .map(|workspace| workspace.name)
            .collect();
        names.sort();
        assert_eq!(names, ["linked", "plain", "repository"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_desktop_where_the_installers_put_it() {
        let root =
            std::env::temp_dir().join(format!("haarness-desktop-find-{}", uuid::Uuid::now_v7()));
        let home = root.join("home");
        let local = root.join("local");
        let programs = root.join("programs");
        let candidates = || desktop_candidates(&home, None, Some(&local), Some(&programs));
        let user_install = if cfg!(target_os = "macos") {
            home.join("Applications/Haarness.app")
        } else if cfg!(windows) {
            local.join("Programs").join("Haarness").join("Haarness.exe")
        } else {
            home.join(".local/bin/haarness-desktop")
        };
        // Nothing in the per-user place (macOS still looks in /Applications).
        assert!(find_desktop(candidates()).is_none_or(|found| found != user_install));
        if cfg!(target_os = "macos") {
            fs::create_dir_all(user_install.join("Contents")).unwrap();
            fs::write(user_install.join("Contents/Info.plist"), "").unwrap();
        } else {
            fs::create_dir_all(user_install.parent().unwrap()).unwrap();
            fs::write(&user_install, "").unwrap();
        }
        assert!(candidates().contains(&user_install));
        assert!(find_desktop(vec![user_install.clone()]).is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn finds_the_desktop_command_on_the_search_path() {
        let root = std::env::temp_dir().join(format!(
            "haarness-desktop-path-find-{}",
            uuid::Uuid::now_v7()
        ));
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("haarness-desktop"), "").unwrap();
        let path = std::env::join_paths([&bin]).unwrap();
        let found = find_desktop(desktop_candidates(
            &root.join("home"),
            Some(&path),
            None,
            None,
        ));
        assert_eq!(found, Some(bin.join("haarness-desktop")));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn project_configuration_cannot_follow_an_external_symlink() {
        let root =
            std::env::temp_dir().join(format!("haarness-desktop-path-{}", uuid::Uuid::now_v7()));
        let paths = Paths::new(Some(&root)).unwrap();
        let workspace = root.join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(root.join("outside.json"), "{}").unwrap();
        std::os::unix::fs::symlink(root.join("outside.json"), workspace.join("haarness.json"))
            .unwrap();
        assert!(configuration_path(&paths, &workspace, "project-json").is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
