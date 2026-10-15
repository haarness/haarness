//! Host-level discovery delegates execution to the existing leased workspace daemons.
use super::{
    Paths, ensure_workspace_server_for_remote, inspect_workspace_server, server_compatibility_error,
};
use anyhow::{Context, Result, ensure};
use haarness_protocol::{WorkspaceInfo, WorkspaceListResponse};
use haarness_server::remote::{
    BrowseOptions, DirectoryEntry, DirectoryListing, ProjectBusy, WorkspaceHost, WorkspaceTarget,
};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

pub struct Host {
    pub paths: Paths,
    pub workspace: PathBuf,
    pub url: String,
    pub token: String,
    pub trust: bool,
    pub locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}
#[async_trait::async_trait]
impl WorkspaceHost for Host {
    async fn catalog(&self) -> Result<WorkspaceListResponse> {
        let paths = self.paths.clone();
        tokio::task::spawn_blocking(move || {
            let mut catalog = paths.workspace_manager()?.list(None)?;
            let hidden = hidden_projects(&paths)?;
            catalog
                .workspaces
                .retain(|workspace| !hidden.contains(&workspace.id));
            catalog.projects.retain(|project| {
                catalog
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.project_id == project.id)
            });
            Ok(catalog)
        })
        .await?
    }
    async fn browse(
        &self,
        path: Option<String>,
        options: BrowseOptions,
    ) -> Result<DirectoryListing> {
        let default = self.workspace.clone();
        tokio::task::spawn_blocking(move || list_directories(path, default, options)).await?
    }
    async fn open(&self, path: String) -> Result<WorkspaceInfo> {
        let paths = self.paths.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            let path = PathBuf::from(path);
            ensure!(
                path.is_absolute(),
                "Use an absolute path on the remote server"
            );
            Ok::<_, anyhow::Error>(paths.workspace_manager()?.register_local(&path)?)
        })
        .await??;
        set_visible(&self.paths, &workspace.id, true)?;
        self.target(workspace.id.clone()).await?;
        Ok(workspace)
    }
    async fn close(&self, id: String) -> Result<()> {
        self.paths.workspace_manager()?.get(&id)?;
        set_visible(&self.paths, &id, false)
    }
    async fn target(&self, id: String) -> Result<WorkspaceTarget> {
        ensure!(
            !hidden_projects(&self.paths)?.contains(&id),
            "Project is closed on this remote host; open its folder to reconnect"
        );
        let paths = self.paths.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>(paths.workspace_manager()?.get(&id)?)
        })
        .await??;
        let lock = self
            .locks
            .lock()
            .await
            .entry(workspace.id.clone())
            .or_default()
            .clone();
        let _guard = lock.lock().await;
        let path = PathBuf::from(workspace.path);
        ensure!(path.is_dir(), "Project folder is unavailable");
        if path == self.workspace {
            return Ok(WorkspaceTarget {
                url: self.url.clone(),
                token: self.token.clone(),
            });
        }
        let paths = self.paths.scoped(&workspace.id, false)?;
        let discovery = ensure_workspace_server_for_remote(&paths, &path, self.trust).await?;
        Ok(WorkspaceTarget {
            url: discovery.url,
            token: discovery.token,
        })
    }
    /// A server another Haarness build runs cannot be replaced while it works,
    /// but reading from it changes none of that work: reads reach it as it is,
    /// so the project can still be viewed. Only a server that speaks this
    /// build's API and confines a published chat's requests (`remote-chat-scope.v1`)
    /// is read from; anything else keeps the busy answer.
    async fn read_target(&self, id: String) -> Result<WorkspaceTarget> {
        let error = match self.target(id.clone()).await {
            Err(error) if error.downcast_ref::<ProjectBusy>().is_some() => error,
            other => return other,
        };
        let paths = self.paths.clone();
        let workspace = tokio::task::spawn_blocking(move || {
            Ok::<_, anyhow::Error>(paths.workspace_manager()?.get(&id)?)
        })
        .await??;
        let paths = self.paths.scoped(&workspace.id, false)?;
        match inspect_workspace_server(&paths, std::path::Path::new(&workspace.path)).await {
            Ok(Some(live)) if readable(&live.info) => Ok(WorkspaceTarget {
                url: live.discovery.url,
                token: live.discovery.token,
            }),
            // Gone or replaced meanwhile: the next request starts over.
            _ => Err(error),
        }
    }
}

/// Whether reads may go to a running server of another build as it is.
fn readable(info: &haarness_protocol::ServerInfo) -> bool {
    server_compatibility_error(info).is_none()
        && info
            .capabilities
            .iter()
            .any(|capability| capability == "remote-chat-scope.v1")
}

fn list_directories(
    path: Option<String>,
    default: PathBuf,
    options: BrowseOptions,
) -> Result<DirectoryListing> {
    use std::collections::BTreeMap;
    let limit = options.limit.unwrap_or(200);
    ensure!(
        (1..=200).contains(&limit),
        "Choose a page size between 1 and 200"
    );
    ensure!(
        options.prefix.len() <= 4096 && options.after.as_ref().is_none_or(|v| v.len() <= 4096),
        "Folder filter is too long"
    );
    let path = match path.as_deref() {
        Some("~") => directories::BaseDirs::new()
            .context("Home folder is unavailable")?
            .home_dir()
            .to_path_buf(),
        Some(value) if value.starts_with("~/") => directories::BaseDirs::new()
            .context("Home folder is unavailable")?
            .home_dir()
            .join(&value[2..]),
        Some(value) => {
            ensure!(value.len() <= 16384, "Folder path is too long");
            PathBuf::from(value)
        }
        None => default,
    };
    ensure!(
        path.is_absolute(),
        "Use an absolute path on the remote server"
    );
    let path = path.canonicalize().context("Folder is unavailable")?;
    ensure!(path.is_dir(), "Select a folder");
    let prefix = options.prefix.to_lowercase();
    let after = options.after.map(|name| (name.to_lowercase(), name));
    // Keep at most one page plus a look-ahead entry, even in very large folders.
    let mut entries = BTreeMap::new();
    for entry in std::fs::read_dir(&path).context("This folder cannot be read")? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let key = (name.to_lowercase(), name.clone());
        if (!options.hidden && name.starts_with('.') && !options.prefix.starts_with('.'))
            || !key.0.starts_with(&prefix)
            || after.as_ref().is_some_and(|after| &key <= after)
            || !entry.path().is_dir()
        {
            continue;
        }
        entries.insert(
            key,
            DirectoryEntry {
                name,
                path: entry.path().display().to_string(),
            },
        );
        if entries.len() > limit + 1 {
            entries.pop_last();
        }
    }
    let more = entries.len() > limit;
    if more {
        entries.pop_last();
    }
    let next_cursor = more.then(|| entries.last_key_value().unwrap().1.name.clone());
    Ok(DirectoryListing {
        parent: path.parent().map(|path| path.display().to_string()),
        path: path.display().to_string(),
        directories: entries.into_values().collect(),
        next_cursor,
    })
}

/// Where the projects closed for remote access are listed.
pub(crate) fn hidden_projects_file(paths: &Paths) -> std::path::PathBuf {
    paths.config_dir().join("remote-hidden-projects.json")
}
/// Projects closed for remote access: Cloud cannot reach them.
pub(crate) fn hidden_projects(paths: &Paths) -> Result<std::collections::HashSet<String>> {
    match std::fs::read(hidden_projects_file(paths)) {
        Ok(data) => Ok(serde_json::from_slice(&data)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(std::collections::HashSet::default())
        }
        Err(error) => Err(error.into()),
    }
}
fn set_visible(paths: &Paths, id: &str, visible: bool) -> Result<()> {
    std::fs::create_dir_all(paths.config_dir())?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(paths.config_dir().join("remote-hidden-projects.lock"))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let mut hidden = hidden_projects(paths)?;
    if visible {
        hidden.remove(id);
    } else {
        hidden.insert(id.to_owned());
    }
    let temporary = paths.config_dir().join("remote-hidden-projects.json.tmp");
    std::fs::write(&temporary, serde_json::to_vec(&hidden)?)?;
    std::fs::rename(
        temporary,
        paths.config_dir().join("remote-hidden-projects.json"),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::readable;
    use haarness_protocol::{
        PTY_CAPABILITY, SCHEMA_VERSION, ServerInfo, WORKSPACE_HISTORY_CAPABILITY,
    };

    fn info(capabilities: &[&str]) -> ServerInfo {
        ServerInfo {
            name: "haarness".into(),
            version: "0.0.0-other".into(),
            schema_version: SCHEMA_VERSION.into(),
            server_instance_id: uuid::Uuid::now_v7(),
            event_cursor: 0,
            capabilities: capabilities.iter().map(|value| (*value).into()).collect(),
            workspace: None,
            git_branch: None,
            default_provider: None,
            default_model: None,
            default_agent: None,
            project_trusted: None,
            pid: None,
        }
    }

    #[test]
    fn reads_reach_another_build_only_when_it_speaks_this_api_and_confines_shared_chats() {
        let full = [
            WORKSPACE_HISTORY_CAPABILITY,
            PTY_CAPABILITY,
            "remote-chat-scope.v1",
        ];
        assert!(readable(&info(&full)));
        // Without scope enforcement a published chat's reads would see every chat.
        assert!(!readable(&info(&full[..2])));
        assert!(!readable(&info(&full[1..])));
        let mut other_schema = info(&full);
        other_schema.schema_version = "0".into();
        assert!(!readable(&other_schema));
    }
}
