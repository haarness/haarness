//! Host-level discovery delegates execution to the existing leased workspace daemons.
use super::{Paths, ensure_workspace_server};
use anyhow::{Context, Result, ensure};
use haarness_protocol::{WorkspaceInfo, WorkspaceListResponse};
use haarness_server::remote::{
    BrowseOptions, DirectoryEntry, DirectoryListing, WorkspaceHost, WorkspaceTarget,
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
        tokio::task::spawn_blocking(move || Ok(paths.workspace_manager()?.list(None)?)).await?
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
        self.target(workspace.id.clone()).await?;
        Ok(workspace)
    }
    async fn target(&self, id: String) -> Result<WorkspaceTarget> {
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
        let discovery = ensure_workspace_server(&paths, &path, self.trust, false).await?;
        Ok(WorkspaceTarget {
            url: discovery.url,
            token: discovery.token,
        })
    }
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
