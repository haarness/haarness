use async_trait::async_trait;
use haarness_core::{CoreError, CoreResult, SkillRegistry};
use haarness_protocol::SkillInfo;
use parking_lot::RwLock;
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::RwLock as AsyncRwLock;

#[derive(Clone)]
struct CachedSkill {
    execution: haarness_protocol::DefinitionExecution,
    aliases: Vec<String>,
    path: PathBuf,
    trusted: bool,
    description: String,
    body: String,
}

#[derive(Clone)]
pub struct FilesystemSkillRegistry {
    catalog: Arc<RwLock<BTreeMap<String, CachedSkill>>>,
    loaded: Arc<AsyncRwLock<HashSet<String>>>,
    disabled: Arc<RwLock<HashSet<String>>>,
}
impl FilesystemSkillRegistry {
    #[must_use]
    pub fn new(roots: Vec<(PathBuf, bool)>) -> Self {
        let catalog = catalog_for_roots(
            &roots
                .into_iter()
                .map(|(path, trusted)| (path, trusted, None))
                .collect::<Vec<_>>(),
        );
        Self {
            catalog: Arc::new(RwLock::new(catalog)),
            loaded: Arc::new(AsyncRwLock::new(HashSet::new())),
            disabled: Arc::default(),
        }
    }

    pub fn new_scoped(roots: Vec<(PathBuf, bool, Option<PathBuf>)>) -> Self {
        Self {
            catalog: Arc::new(RwLock::new(catalog_for_roots(&roots))),
            loaded: Arc::new(AsyncRwLock::new(HashSet::new())),
            disabled: Arc::default(),
        }
    }
    /// Replace the ordered discovery roots without replacing the registry
    /// object held by REST routes and already-composed native tools.
    pub fn replace_scoped_roots(&self, roots: Vec<(PathBuf, bool, Option<PathBuf>)>) {
        *self.catalog.write() = catalog_for_roots(&roots);
    }

    pub fn set_disabled(&self, names: Vec<String>) {
        *self.disabled.write() = names.into_iter().collect();
    }

    fn locate(&self, name: &str) -> Option<(PathBuf, bool, String)> {
        let catalog = self.catalog.read();
        let skill = catalog.get(name).or_else(|| {
            let mut matches = catalog
                .values()
                .filter(|s| s.aliases.iter().any(|a| a == name));
            let first = matches.next()?;
            matches.next().is_none().then_some(first)
        })?;
        let canonical = catalog
            .iter()
            .find(|(_, entry)| entry.path == skill.path)?
            .0;
        if self.disabled.read().contains(canonical) {
            return None;
        }
        Some((skill.path.clone(), skill.trusted, skill.body.clone()))
    }
}
#[async_trait]
impl SkillRegistry for FilesystemSkillRegistry {
    async fn discover(&self) -> CoreResult<Vec<SkillInfo>> {
        let loaded = self.loaded.read().await;
        Ok(self
            .catalog
            .read()
            .clone()
            .into_iter()
            .filter(|(name, _)| !self.disabled.read().contains(name))
            .map(|(name, skill)| SkillInfo {
                execution: skill.execution,
                loaded: loaded.contains(&name),
                name,
                description: skill.description,
                path: skill.path.display().to_string(),
                trusted: skill.trusted,
            })
            .collect())
    }
    async fn describe(&self, name: &str) -> CoreResult<SkillInfo> {
        let (path, _, _) = self
            .locate(name)
            .ok_or_else(|| CoreError::NotFound(format!("skill {name}")))?;
        self.discover()
            .await?
            .into_iter()
            .find(|s| Path::new(&s.path) == path)
            .ok_or_else(|| CoreError::NotFound(format!("skill {name}")))
    }
    async fn load(&self, name: &str) -> CoreResult<String> {
        let (_path, trusted, body) = self
            .locate(name)
            .ok_or_else(|| CoreError::NotFound(format!("skill {name}")))?;
        if !trusted {
            return Err(CoreError::PermissionDenied(format!(
                "project skill {name} is unavailable because project configuration is turned off"
            )));
        }
        let canonical = self.describe(name).await?.name;
        self.loaded.write().await.insert(canonical);
        Ok(body)
    }
}

fn catalog_for_roots(roots: &[(PathBuf, bool, Option<PathBuf>)]) -> BTreeMap<String, CachedSkill> {
    let mut catalog = BTreeMap::new();
    // Global roots are supplied first and project/plugin roots later. Later
    // roots intentionally replace earlier names, matching nearest-project
    // precedence. Bodies are snapshotted so active turns cannot observe a
    // partially-written skill before the coordinated reload boundary.
    for (root, trusted, source_plugin) in roots {
        for path in skill_files(root, 0) {
            let body = std::fs::read_to_string(&path).unwrap_or_default();
            let front = match haarness_core::definitions::markdown(&body) {
                Ok((front, _)) => front,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "skipped skill whose frontmatter does not parse");
                    continue;
                }
            };
            let bare = haarness_core::definitions::skill_name(&front, &path);
            if bare.is_empty() {
                continue;
            }
            let plugin_root = source_plugin
                .clone()
                .or_else(|| haarness_core::definitions::plugin_root(&path));
            let name = plugin_root
                .as_deref()
                .and_then(haarness_core::definitions::plugin_name)
                .map_or_else(|| bare.clone(), |plugin| format!("{plugin}:{bare}"));
            let execution =
                haarness_core::definitions::execution(&front, &path, plugin_root.as_deref());
            let mut aliases = vec![bare];
            if let Some(display) = front.get("name") {
                aliases.push(display.clone());
            }
            let description = front
                .get("description")
                .cloned()
                .unwrap_or_else(|| "Agent skill".into());
            catalog.insert(
                name,
                CachedSkill {
                    execution,
                    aliases,
                    path,
                    trusted: *trusted,
                    description,
                    body,
                },
            );
        }
    }
    catalog
}

fn skill_files(root: &Path, depth: usize) -> Vec<PathBuf> {
    if depth > 16 {
        return Vec::new();
    }
    if root.is_file() {
        return if root.file_name().is_some_and(|n| n == "SKILL.md") {
            vec![root.to_path_buf()]
        } else {
            vec![]
        };
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            files.extend(skill_files(&path, depth + 1));
        } else if path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") {
            files.push(path);
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::FilesystemSkillRegistry;
    use haarness_core::SkillRegistry;

    #[tokio::test]
    async fn recursively_discovers_and_prefers_the_nearest_root() {
        let root =
            std::env::temp_dir().join(format!("haarness-skill-catalog-{}", uuid::Uuid::now_v7()));
        let global = root.join("global/nested/demo");
        let project = root.join("project/deeper/demo");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            global.join("SKILL.md"),
            "---\nname: demo\ndescription: global\n---\nglobal body",
        )
        .unwrap();
        std::fs::write(
            project.join("SKILL.md"),
            "---\nname: demo\ndescription: project\n---\nproject body",
        )
        .unwrap();
        let registry = FilesystemSkillRegistry::new(vec![
            (root.join("global"), true),
            (root.join("project"), true),
        ]);

        let skills = registry.discover().await.unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "project");
        assert!(
            registry
                .load("demo")
                .await
                .unwrap()
                .contains("project body")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn body_changes_are_visible_only_after_generation_refresh() {
        let root =
            std::env::temp_dir().join(format!("haarness-skill-reload-{}", uuid::Uuid::now_v7()));
        let directory = root.join("demo");
        let path = directory.join("SKILL.md");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&path, "---\nname: demo\n---\nold body").unwrap();
        let registry = FilesystemSkillRegistry::new(vec![(root.clone(), true)]);

        std::fs::write(&path, "---\nname: demo\n---\nnew body").unwrap();
        assert!(registry.load("demo").await.unwrap().contains("old body"));
        registry.replace_scoped_roots(vec![(root.clone(), true, None)]);
        assert!(registry.load("demo").await.unwrap().contains("new body"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
