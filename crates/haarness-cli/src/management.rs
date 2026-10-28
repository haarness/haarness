use super::{
    Paths, adopt_system_mcp_entry, asset_layers, loaded_mcp_entry, mcp_config_for_display,
    mcp_config_target, mcp_entry_target, open_browser, read_mcp_entry, receive_oauth_callback,
    release_base_mcp_copy, system_mcp_entry, validate_asset_name, validate_mcp_name,
    validate_qualified_model, write_mcp_changes, write_mcp_entry,
};
use crate::plugins;
use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use haarness_core::{
    AgentRuntime, CoreError, CoreResult, McpRegistry, PermissionDecision, PermissionRule,
    SkillRegistry, ToolContext, ToolOutput,
};
use haarness_mcp::NativeMcpRegistry;
use haarness_protocol::{
    ItemData, McpConfigureRequest, QueueInputRequest, RequestId, StartTurnRequest, Thread,
    ThreadId, UpdateThreadRequest,
};
use haarness_server::ManagementRegistry;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, Weak},
    time::Duration,
};
use tokio::net::TcpListener;

mod capabilities;
mod ui;

#[derive(Clone)]
pub struct ManagementService {
    logins: Arc<std::sync::Mutex<ui::Logins>>,
    /// Where ChatGPT sign-ins go; tests point it at a fixture issuer.
    openai_oauth: haarness_providers::auth::OpenAiOAuthConfig,
    paths: Paths,
    workspace: PathBuf,
    trust_project: bool,
    skills: Arc<dyn SkillRegistry>,
    mcp: Arc<NativeMcpRegistry>,
    runtime: Arc<OnceLock<Weak<AgentRuntime>>>,
}

fn internal_agent(name: &str) -> bool {
    haarness_core::builtin_profiles()
        .iter()
        .any(|agent| agent.id == name && agent.hidden)
}

pub(super) fn ensure_user_agent(name: &str) -> Result<()> {
    anyhow::ensure!(
        !internal_agent(name),
        "This agent is managed internally by Haarness."
    );
    Ok(())
}

impl ManagementService {
    /// Native clients manage only local assets here; execution remains in the daemon.
    pub async fn desktop_asset(&self, resource: &str, input: &Value) -> Result<Value> {
        if input["action"] == "customize" {
            return self.customize_asset(resource, input).await;
        }
        match resource {
            "agent" => self.agent(input),
            "command" => self.command(input),
            "skill" => self.skill(input).await,
            "mcp" => self.mcp(input).await,
            "hooks" => super::desktop_hooks::execute(
                &self.paths,
                &self.workspace,
                self.trust_project,
                input,
            ),
            "memory" => self.desktop_memory(input),
            _ => bail!("unsupported desktop asset"),
        }
    }

    /// Save an override, preserving source permissions and skill support files.
    /// Plugin installations and inherited configuration remain untouched.
    async fn customize_asset(&self, resource: &str, input: &Value) -> Result<Value> {
        let requested_name = required_string(input, "name")?;
        let resolved = if resource == "skill" {
            Some(
                self.skills
                    .describe(requested_name)
                    .await
                    .map_err(core_error)?
                    .name,
            )
        } else {
            None
        };
        let name = resolved.as_deref().unwrap_or(requested_name);
        let scope = create_scope(input)?;
        let get = json!({"action":"get","name":name});
        let (source, target, body_key) = match resource {
            "agent" => (self.agent(&get)?, self.agent_path(name, scope)?, "prompt"),
            "command" => (
                self.command(&get)?,
                self.command_path(name, scope)?,
                "template",
            ),
            "skill" => (
                self.skill(&get).await?,
                self.skill_path(name, scope)?,
                "instructions",
            ),
            _ => bail!("Unsupported definition type"),
        };
        anyhow::ensure!(
            source["readOnly"] == true,
            "This definition already has a custom version. Reopen it to edit."
        );
        let mut values = source.clone();
        values
            .as_object_mut()
            .context("Definition must be an object")?
            .extend(
                input
                    .as_object()
                    .context("Edits must be an object")?
                    .clone(),
            );
        values["name"] = json!(name);
        let input = &values;
        let body = required_string(input, body_key)?;
        let description = required_string(input, "description")?;
        let _lock = haarness_profiles::configuration_lock(&self.paths.data)?;
        anyhow::ensure!(
            !target.exists(),
            "A custom version already exists. Reopen it to edit."
        );
        let source_path = source["path"]
            .as_str()
            .or_else(|| source.pointer("/execution/path").and_then(Value::as_str));
        let mut front =
            match source_path.and_then(|path| Some((path, fs::read_to_string(path).ok()?))) {
                Some((path, raw)) => editable_markdown(&raw, Path::new(path))?.0,
                None => BTreeMap::new(),
            };
        if resource == "command" {
            if let Some(directory) = source
                .pointer("/execution/directory")
                .and_then(Value::as_str)
            {
                front.insert("haarness-definition-directory".into(), directory.into());
            }
        }
        if let Some(path) = source_path {
            if let Some(root) = haarness_core::definitions::plugin_root(Path::new(path)) {
                front.insert(
                    "haarness-plugin-root".into(),
                    root.to_string_lossy().into_owned(),
                );
            }
            if resource == "command" && front.contains_key("haarness-plugin-root") {
                front.insert("haarness-claude-arguments".into(), "true".into());
            }
        }
        if resource == "agent" {
            validate_agent_fields(input)?;
            if !source["permissions"].is_null() {
                front.insert(
                    "permissions".into(),
                    serde_json::to_string(&source["permissions"])?,
                );
            }
            front = agent_frontmatter(name, description, input, Some(front))?;
        }
        front.insert("name".into(), name.into());
        front.insert("description".into(), description.into());
        front.insert("haarness-customized".into(), "true".into());
        if resource == "skill" {
            set_optional_frontmatter(&mut front, "notes", optional_string(input, "notes"));
        }
        // Stage outside the watched definition root, then publish the complete override.
        let parent = if resource == "skill" {
            target.parent().and_then(Path::parent)
        } else {
            target.parent()
        }
        .context("Definition has no parent directory")?;
        fs::create_dir_all(parent)?;
        let stage = parent
            .parent()
            .unwrap_or(parent)
            .join(format!(".haarness-edit-{}", uuid::Uuid::now_v7()));
        let result = (|| -> Result<()> {
            if resource == "skill" {
                let source_path = Path::new(required_string(&source, "path")?);
                copy_tree(
                    source_path.parent().context("Skill has no directory")?,
                    &stage,
                )?;
                write_markdown(&stage.join("SKILL.md"), &front, body)?;
                fs::rename(&stage, target.parent().context("Skill has no directory")?)?;
            } else {
                if resource == "command" {
                    write_command(&stage, input, front, body)?;
                } else {
                    write_markdown(&stage, &front, body)?;
                }
                // Linking fails if another writer has created the destination.
                fs::hard_link(&stage, &target)?;
                fs::remove_file(&stage)?;
            }
            Ok(())
        })();
        if result.is_err() {
            if stage.is_dir() {
                let _ = fs::remove_dir_all(&stage);
            } else {
                let _ = fs::remove_file(&stage);
            }
        }
        result?;
        Ok(changed_with_scope(
            "customize",
            resource,
            name,
            &target,
            scope,
            false,
        ))
    }

    pub fn new(
        paths: Paths,
        workspace: PathBuf,
        trust_project: bool,
        skills: Arc<dyn SkillRegistry>,
        mcp: Arc<NativeMcpRegistry>,
    ) -> Self {
        Self {
            logins: Arc::default(),
            openai_oauth: haarness_providers::auth::OpenAiOAuthConfig::default(),
            paths,
            workspace,
            trust_project,
            skills,
            mcp,
            runtime: Arc::new(OnceLock::new()),
        }
    }

    #[cfg(test)]
    fn with_openai_oauth(mut self, config: haarness_providers::auth::OpenAiOAuthConfig) -> Self {
        self.openai_oauth = config;
        self
    }

    /// Resolve the composition cycle after the runtime owns the registry and
    /// install the startup generation of managed commands, agents, and memory.
    pub fn attach_runtime(&self, runtime: &Arc<AgentRuntime>) {
        let _ = self.runtime.set(Arc::downgrade(runtime));
        self.refresh_runtime_assets(runtime);
    }

    fn runtime(&self) -> Result<Arc<AgentRuntime>> {
        self.runtime
            .get()
            .and_then(Weak::upgrade)
            .context("management runtime is not attached")
    }

    fn refresh_runtime_assets(&self, runtime: &AgentRuntime) {
        let mut layers = asset_layers(&self.paths, &self.workspace, self.trust_project);
        if let Ok(disabled) = crate::capability_state::disabled(
            &self.paths,
            &self.workspace,
            self.trust_project,
            "skill",
        ) {
            layers
                .commands
                .retain(|command| !disabled.contains(&command.name));
        }
        runtime.replace_commands(layers.commands);
        runtime.replace_custom_agents(layers.agents);
        runtime.set_memory_context(self.memory_index().unwrap_or_default());
    }

    async fn skill(&self, input: &Value) -> Result<Value> {
        let action = required_string(input, "action")?;
        match action {
            "create" => {
                let name = required_string(input, "name")?;
                let description = required_string(input, "description")?;
                let instructions = required_string(input, "instructions")?;
                let target = self.skill_path(name, create_scope(input)?)?;
                if target.exists() {
                    bail!("skill '{name}' already exists at {}", target.display());
                }
                write_skill(
                    &target,
                    name,
                    description,
                    optional_string(input, "notes"),
                    instructions,
                )?;
                Ok(changed_with_scope(
                    "create",
                    "skill",
                    name,
                    &target,
                    create_scope(input)?,
                    false,
                ))
            }
            "edit" => {
                let name = required_string(input, "name")?;
                let (target, scope) = self.resolve_skill(name, optional_scope(input)?)?;
                let raw = fs::read_to_string(&target)?;
                let (mut front, body) = editable_markdown(&raw, &target)?;
                if let Some(value) = optional_string(input, "description") {
                    front.insert("description".into(), value.into());
                }
                if input.get("notes").is_some() {
                    set_optional_frontmatter(&mut front, "notes", optional_string(input, "notes"));
                }
                let instructions = optional_string(input, "instructions").unwrap_or(body.trim());
                if instructions.is_empty() {
                    bail!("skill instructions must not be empty");
                }
                front.entry("name".into()).or_insert_with(|| name.into());
                write_markdown(&target, &front, instructions)?;
                Ok(changed_with_scope(
                    "edit", "skill", name, &target, scope, false,
                ))
            }
            "delete" => {
                let name = required_string(input, "name")?;
                let (target, scope) = self.resolve_skill(name, optional_scope(input)?)?;
                let directory = target
                    .parent()
                    .context("skill file has no containing directory")?;
                fs::remove_dir_all(directory)?;
                Ok(changed_with_scope(
                    "delete", "skill", name, directory, scope, false,
                ))
            }
            "get" => {
                let name = required_string(input, "name")?;
                if let Ok((target, scope)) = self.resolve_skill(name, optional_scope(input)?) {
                    return skill_detail(name, &target, Some(scope), true);
                }
                let info = self.skills.describe(name).await.map_err(core_error)?;
                let target = PathBuf::from(&info.path);
                let mut detail = skill_detail(&info.name, &target, None, false)?;
                if in_system_folder(&target) {
                    detail["scope"] = json!("system");
                    detail["pluginManaged"] = json!(false);
                } else {
                    detail["pluginManaged"] = json!(is_plugin_path(&target));
                }
                Ok(detail)
            }
            "list" => {
                let requested = optional_scope(input)?;
                let local_root = self.skill_root(Scope::Local);
                let global_root = self.skill_root(Scope::Global);
                let infos = self.skills.discover().await.map_err(core_error)?;
                let mut values = Vec::new();
                for info in infos {
                    let path = PathBuf::from(&info.path);
                    let scope = if path.starts_with(&local_root) {
                        Some(Scope::Local)
                    } else if path.starts_with(&global_root) {
                        Some(Scope::Global)
                    } else {
                        None
                    };
                    if requested.is_some() && requested != scope {
                        continue;
                    }
                    let base = scope.is_none() && in_system_folder(&path);
                    values.push(json!({
                        "name": info.name,
                        "description": info.description,
                        "scope": scope.map(Scope::name).or(base.then_some("system")),
                        "path": info.path,
                        "trusted": info.trusted,
                        "loaded": info.loaded,
                        "managed": scope.is_some(),
                        "customized": skill_detail(&info.name, &path, scope, scope.is_some())?
                            ["customized"],
                        "pluginManaged": scope.is_none() && !base && is_plugin_path(&path),
                    }));
                }
                Ok(json!(values))
            }
            "import" => {
                let source = PathBuf::from(required_string(input, "path")?);
                let source = absolutize(&self.workspace, &source);
                let file = if source.is_dir() {
                    source.join("SKILL.md")
                } else {
                    source.clone()
                };
                if file.file_name().and_then(|value| value.to_str()) != Some("SKILL.md") {
                    bail!("skill import path must be a skill directory or SKILL.md file");
                }
                let raw = fs::read_to_string(&file)
                    .with_context(|| format!("read imported skill {}", file.display()))?;
                let (front, _) = parse_markdown(&raw);
                let name = optional_string(input, "name")
                    .or_else(|| front.get("name").map(String::as_str))
                    .or_else(|| file.parent()?.file_name()?.to_str())
                    .context("imported skill needs a name")?;
                validate_asset_name(name, "skill")?;
                let scope = create_scope(input)?;
                let target = self.skill_path(name, scope)?;
                if target.exists() {
                    bail!("skill '{name}' already exists at {}", target.display());
                }
                let source_root = file.parent().context("SKILL.md has no directory")?;
                copy_tree(
                    source_root,
                    target.parent().context("skill target has no directory")?,
                )?;
                Ok(changed_with_scope(
                    "import", "skill", name, &target, scope, false,
                ))
            }
            _ => bail!("unsupported skill_manage action '{action}'"),
        }
    }

    /// `input` with each tool of its `tools` that the model named the way
    /// providers saw it (as it was sent the tools) under the tool's own name,
    /// so an agent the model writes covers the tools it meant.
    fn with_own_tool_names(&self, input: &Value) -> Value {
        let Some(tools) = input.get("tools").and_then(Value::as_object) else {
            return input.clone();
        };
        let snapshot = self.mcp.tool_snapshot();
        let names = haarness_core::wire_names::WireNames::new(
            snapshot.iter().map(|tool| tool.name.as_str()),
        );
        if names.is_empty() {
            return input.clone();
        }
        let mut input = input.clone();
        input["tools"] = Value::Object(names.own_keys(tools.clone()).into_iter().collect());
        input
    }

    fn agent(&self, input: &Value) -> Result<Value> {
        let action = required_string(input, "action")?;
        if let Some(name) = optional_string(input, "name") {
            ensure_user_agent(name)?;
        }
        match action {
            "create" => {
                let name = required_string(input, "name")?;
                let description = required_string(input, "description")?;
                let prompt = required_string(input, "prompt")?;
                validate_agent_fields(input)?;
                let scope = create_scope(input)?;
                let target = self.agent_path(name, scope)?;
                if target.exists() {
                    bail!("agent '{name}' already exists at {}", target.display());
                }
                let front = agent_frontmatter(name, description, input, None)?;
                write_markdown(&target, &front, prompt)?;
                Ok(changed_with_scope(
                    "create", "agent", name, &target, scope, true,
                ))
            }
            "edit" => {
                let name = required_string(input, "name")?;
                validate_agent_fields(input)?;
                let (target, scope) = self.resolve_agent(name, optional_scope(input)?)?;
                let raw = fs::read_to_string(&target)?;
                let (front, body) = editable_markdown(&raw, &target)?;
                let description = optional_string(input, "description")
                    .or_else(|| front.get("description").map(String::as_str))
                    .unwrap_or_default()
                    .to_owned();
                let front = agent_frontmatter(name, &description, input, Some(front))?;
                let prompt = optional_string(input, "prompt").unwrap_or(body.trim());
                if prompt.is_empty() {
                    bail!("agent prompt must not be empty");
                }
                write_markdown(&target, &front, prompt)?;
                Ok(changed_with_scope(
                    "edit", "agent", name, &target, scope, true,
                ))
            }
            "delete" => {
                let name = required_string(input, "name")?;
                let (target, scope) = self.resolve_agent(name, optional_scope(input)?)?;
                fs::remove_file(&target)?;
                Ok(changed_with_scope(
                    "delete", "agent", name, &target, scope, true,
                ))
            }
            "get" => {
                let name = required_string(input, "name")?;
                if let Ok((target, scope)) = self.resolve_agent(name, optional_scope(input)?) {
                    let detail = agent_detail(name, &target, Some(scope), true)?;
                    ensure_user_agent(detail["name"].as_str().unwrap_or(name))?;
                    return Ok(detail);
                }
                let agent = asset_layers(&self.paths, &self.workspace, self.trust_project)
                    .agents
                    .into_iter()
                    .find(|agent| agent.id == name);
                let Some(agent) = agent else {
                    let profile = haarness_core::builtin_profiles()
                        .into_iter()
                        .find(|profile| profile.id == name)
                        .with_context(|| format!("agent '{name}' not found"))?;
                    return Ok(json!({"name":profile.id,"description":profile.description,
                        "prompt":haarness_core::system_prompt_for(name),"mode":"all",
                        "managed":false,"native":true,"readOnly":true}));
                };
                Ok(json!({
                    "name": agent.id,
                    "description": agent.description,
                    "mode": agent.mode,
                    "model": agent.model,
                    "tools": agent.tools,
                    "permissions": agent.permissions,
                    "prompt": agent.prompt,
                    "scope": self.system_agent(&agent).then_some("system"),
                    "managed": false,
                    "readOnly": true,
                }))
            }
            "list" => {
                let requested = optional_scope(input)?;
                let mut values = Vec::new();
                let mut managed = BTreeSet::new();
                for scope in scopes(requested) {
                    for target in markdown_files(&self.agent_root(scope)) {
                        let name = target
                            .strip_prefix(self.agent_root(scope))
                            .unwrap_or(&target)
                            .with_extension("")
                            .to_string_lossy()
                            .replace(['/', '\\'], ":");
                        let detail = agent_detail(&name, &target, Some(scope), true)?;
                        if internal_agent(&name)
                            || internal_agent(detail["name"].as_str().unwrap_or(&name))
                        {
                            continue;
                        }
                        managed.insert(name.clone());
                        values.push(detail);
                    }
                }
                if requested.is_none() {
                    for profile in haarness_core::builtin_profiles() {
                        if profile.hidden || managed.contains(&profile.id) {
                            continue;
                        }
                        values.push(json!({
                            "name": profile.id,
                            "description": profile.description,
                            "native": true,
                            "managed": false,
                            "readOnly": true,
                        }));
                    }
                    let base = self.system_definitions();
                    for agent in
                        asset_layers(&self.paths, &self.workspace, self.trust_project).agents
                    {
                        if !internal_agent(&agent.id) && !managed.contains(&agent.id) {
                            values.push(json!({
                                "name": agent.id,
                                "description": agent.description,
                                "mode": agent.mode,
                                "model": agent.model,
                                "scope": same_agent(&base.0, &agent).then_some("system"),
                                "managed": false,
                                "readOnly": true,
                            }));
                        }
                    }
                }
                Ok(json!(values))
            }
            _ => bail!("unsupported agent_manage action '{action}'"),
        }
    }

    fn plugin(&self, input: &Value) -> Result<Value> {
        let action = required_string(input, "action")?;
        match action {
            "create" => {
                let name = required_string(input, "name")?;
                let description = required_string(input, "description")?;
                validate_asset_name(name, "plugin")?;
                let scope = create_scope(input)?;
                let root = self.plugin_path(name, scope)?;
                if root.exists() {
                    bail!("plugin '{name}' already exists at {}", root.display());
                }
                fs::create_dir_all(root.join(".claude-plugin"))?;
                fs::create_dir_all(root.join("tools"))?;
                fs::create_dir_all(root.join("skills"))?;
                write_plugin_manifest(
                    &root,
                    name,
                    description,
                    optional_string(input, "notes"),
                    optional_string(input, "version").unwrap_or("1.0.0"),
                )?;
                Ok(changed_with_scope(
                    "create", "plugin", name, &root, scope, true,
                ))
            }
            "edit" => {
                let name = required_string(input, "name")?;
                let (root, scope) = self.resolve_plugin(name, optional_scope(input)?)?;
                let manifest_path = root.join(".claude-plugin/plugin.json");
                let mut manifest: Value =
                    serde_json::from_str(&fs::read_to_string(&manifest_path)?)?;
                let object = manifest
                    .as_object_mut()
                    .context("plugin manifest must be a JSON object")?;
                for field in ["description", "notes", "version"] {
                    if input.get(field).is_some() {
                        if let Some(value) = optional_string(input, field) {
                            object.insert(field.into(), value.into());
                        } else {
                            object.remove(field);
                        }
                    }
                }
                fs::write(
                    &manifest_path,
                    serde_json::to_string_pretty(&manifest)? + "\n",
                )?;
                Ok(changed_with_scope(
                    "edit",
                    "plugin",
                    name,
                    &manifest_path,
                    scope,
                    true,
                ))
            }
            "delete" => {
                let name = required_string(input, "name")?;
                let (root, scope) = self.resolve_plugin(name, optional_scope(input)?)?;
                fs::remove_dir_all(&root)?;
                Ok(changed_with_scope(
                    "delete", "plugin", name, &root, scope, true,
                ))
            }
            "get" => {
                let name = required_string(input, "name")?;
                if let Ok((root, scope)) = self.resolve_plugin(name, optional_scope(input)?) {
                    return plugin_detail(name, &root, Some(scope), true);
                }
                let info = plugins::plugin_infos(
                    &self.paths.data,
                    self.paths.config_dir(),
                    &self.workspace,
                    self.trust_project,
                )
                .into_iter()
                .find(|plugin| plugin.name == name)
                .with_context(|| format!("plugin '{name}' not found"))?;
                Ok(json!({
                    "name": info.name,
                    "description": info.description,
                    "path": info.path,
                    "enabled": info.enabled,
                    "source": info.source,
                    "managed": false,
                    "readOnly": true,
                }))
            }
            "list" => {
                let requested = optional_scope(input)?;
                let mut values = Vec::new();
                let mut managed = BTreeSet::new();
                for scope in scopes(requested) {
                    for root in plugin_directories(&self.plugin_root(scope)) {
                        let name = plugin_manifest_name(&root).unwrap_or_else(|| {
                            root.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned()
                        });
                        managed.insert(name.clone());
                        values.push(plugin_detail(&name, &root, Some(scope), true)?);
                    }
                }
                if requested.is_none() {
                    for info in plugins::plugin_infos(
                        &self.paths.data,
                        self.paths.config_dir(),
                        &self.workspace,
                        self.trust_project,
                    ) {
                        if !managed.contains(&info.name) {
                            values.push(json!({
                                "name": info.name,
                                "description": info.description,
                                "path": info.path,
                                "enabled": info.enabled,
                                "source": info.source,
                                "managed": false,
                                "readOnly": true,
                            }));
                        }
                    }
                }
                Ok(json!(values))
            }
            "enable" | "disable" => {
                let name = required_string(input, "name")?;
                let (root, scope) = self.resolve_plugin(name, optional_scope(input)?)?;
                let marker = root.join(".haarness-disabled");
                if action == "enable" {
                    match fs::remove_file(&marker) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                } else {
                    fs::write(&marker, "disabled by haarness\n")?;
                }
                let mut value = changed_with_scope(action, "plugin", name, &root, scope, true);
                value["enabled"] = json!(action == "enable");
                Ok(value)
            }
            "import" => {
                let source =
                    absolutize(&self.workspace, Path::new(required_string(input, "path")?));
                let source =
                    if source.file_name().and_then(|value| value.to_str()) == Some("plugin.json") {
                        source
                            .parent()
                            .and_then(Path::parent)
                            .context("plugin.json must be under .claude-plugin")?
                            .to_path_buf()
                    } else {
                        source
                    };
                let name = optional_string(input, "name")
                    .map(ToOwned::to_owned)
                    .or_else(|| plugin_manifest_name(&source))
                    .context("imported plugin manifest needs a name")?;
                validate_asset_name(&name, "plugin")?;
                let scope = create_scope(input)?;
                let target = self.plugin_path(&name, scope)?;
                if target.exists() {
                    bail!("plugin '{name}' already exists at {}", target.display());
                }
                copy_tree(&source, &target)?;
                plugin_manifest_name(&target)
                    .context("imported plugin needs .claude-plugin/plugin.json")?;
                Ok(changed_with_scope(
                    "import", "plugin", &name, &target, scope, true,
                ))
            }
            _ => bail!("unsupported plugin_manage action '{action}'"),
        }
    }

    async fn mcp(&self, input: &Value) -> Result<Value> {
        let action = required_string(input, "action")?;
        match action {
            "add" => {
                let name = required_string(input, "name")?;
                validate_mcp_name(name)?;
                let scope = create_scope(input)?;
                let entry = mcp_entry_from_add(input)?;
                let target =
                    mcp_entry_target(&self.paths, &self.workspace, scope == Scope::Local, name);
                if read_mcp_entry(&target, name).is_ok() {
                    bail!("MCP server '{name}' already exists in {}", target.display());
                }
                write_mcp_entry(&target, name, Some(&entry))?;
                Ok(json!({
                    "action": "add", "resource": "mcp", "name": name,
                    "scope": scope.name(), "file": target, "config": redact_mcp(&entry)?,
                    "runtimeStatus": "reload_queued", "restartRequired": false,
                    "usableFrom": "after_active_work_is_idle"
                }))
            }
            "edit" => {
                let name = required_string(input, "name")?;
                self.adopt_base_mcp(name, optional_scope(input)?)?;
                let (target, scope, before) = self.resolve_mcp(name, optional_scope(input)?)?;
                let mut entry = before.clone();
                apply_mcp_edit(&mut entry, input)?;
                validate_mcp_entry(&entry)?;
                write_mcp_changes(&target, name, &before, &entry)?;
                Ok(json!({
                    "action": "edit", "resource": "mcp", "name": name,
                    "scope": scope.name(), "file": target, "config": redact_mcp(&entry)?,
                    "runtimeStatus": "reload_queued", "restartRequired": false,
                    "usableFrom": "after_active_work_is_idle"
                }))
            }
            "remove" => {
                let name = required_string(input, "name")?;
                let (target, scope, _) = self.resolve_mcp(name, optional_scope(input)?)?;
                write_mcp_entry(&target, name, None)?;
                Ok(
                    json!({"action":"remove","resource":"mcp","name":name,"scope":scope.name(),"file":target,"restartRequired":false}),
                )
            }
            "get" => {
                let name = required_string(input, "name")?;
                let (target, scope, entry) =
                    self.resolve_mcp_readable(name, optional_scope(input)?)?;
                let live = self
                    .mcp
                    .list()
                    .await
                    .map_err(core_error)?
                    .into_iter()
                    .find(|item| item.name == name);
                Ok(json!({
                    "name":name,"scope":scope.map_or("system", Scope::name),"file":target,
                    "config":mcp_config_for_display(&entry, false)?,
                    "runtimeStatus":live.as_ref().map(|item| item.status.as_str()),
                    "runtimeDetail":live.and_then(|item| item.detail),
                    "managed":scope.is_some(),"pluginManaged":false,"readOnly":scope.is_none()
                }))
            }
            "list" => {
                let requested = optional_scope(input)?;
                let live = self.mcp.list().await.map_err(core_error)?;
                let live: BTreeMap<_, _> = live
                    .into_iter()
                    .map(|item| (item.name.clone(), item))
                    .collect();
                let mut values = Vec::new();
                let mut configured = BTreeSet::new();
                for scope in scopes(requested) {
                    let target =
                        mcp_config_target(&self.paths, &self.workspace, scope == Scope::Local);
                    for (name, entry) in mcp_entries(&target)? {
                        configured.insert(name.clone());
                        let status = live.get(&name);
                        values.push(json!({
                            "name":name,"scope":scope.name(),"file":target,
                            "config":mcp_config_for_display(&entry, false)?,
                            "runtimeStatus":status.map(|item| item.status.as_str()),
                            "runtimeDetail":status.and_then(|item| item.detail.as_deref()),
                            "managed":true,"pluginManaged":false
                        }));
                    }
                }
                if requested.is_none() {
                    // Servers the system folder defines and no layer above it
                    // overrides: listed, never changed in place.
                    let base = crate::layout::SystemFolder::for_workspace(&self.workspace);
                    for file in base.iter().flat_map(|base| {
                        [
                            "opencode.jsonc",
                            "opencode.json",
                            "haarness.jsonc",
                            "haarness.json",
                        ]
                        .into_iter()
                        .filter_map(|name| base.entry(name))
                    }) {
                        for (name, entry) in mcp_entries(&file).unwrap_or_default() {
                            if configured.contains(&name) {
                                continue;
                            }
                            // A later base file replaces an earlier one's entry.
                            values.retain(|row| row["name"] != name.as_str());
                            let status = live.get(&name);
                            values.push(json!({
                                "name":name,"scope":"system","file":file,
                                "config":mcp_config_for_display(&entry, false)?,
                                "runtimeStatus":status.map(|item| item.status.as_str()),
                                "runtimeDetail":status.and_then(|item| item.detail.as_deref()),
                                "managed":false,"pluginManaged":false,"readOnly":true
                            }));
                        }
                    }
                    configured.extend(
                        values
                            .iter()
                            .filter_map(|row| row["name"].as_str().map(str::to_owned)),
                    );
                    // Desktop names the servers its own connection runs (this
                    // process has no live registry of its own).
                    let mut extra: BTreeSet<String> = input
                        .get("names")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .filter(|name| !live.contains_key(*name))
                        .map(str::to_owned)
                        .collect();
                    // A server from a file of either scope other than the one listed
                    // above is edited where it is; one from a plugin (or anything else
                    // outside the person's settings) is read-only here.
                    let editable = |name: &str| self.resolve_mcp(name, None).is_ok();
                    for (name, status) in live {
                        extra.remove(&name);
                        if !configured.contains(&name) {
                            let editable = editable(&name);
                            values.push(json!({
                                "name":name,"runtimeStatus":status.status,"runtimeDetail":status.detail,
                                "managed":editable,"pluginManaged":!editable,"readOnly":!editable
                            }));
                        }
                    }
                    for name in extra.into_iter().filter(|name| !configured.contains(name)) {
                        let editable = editable(&name);
                        values.push(json!({
                            "name":name,"managed":editable,"pluginManaged":!editable,"readOnly":!editable
                        }));
                    }
                }
                Ok(json!(values))
            }
            "enable" | "disable" => {
                let name = required_string(input, "name")?;
                self.adopt_base_mcp(name, optional_scope(input)?)?;
                let (target, scope, before) = self.resolve_mcp(name, optional_scope(input)?)?;
                let mut entry = before.clone();
                entry.enabled = action == "enable";
                write_mcp_changes(&target, name, &before, &entry)?;
                // Turned back on as the system folder has it: its entry applies again.
                if action == "enable"
                    && let Some(source) = release_base_mcp_copy(
                        &self.paths,
                        &self.workspace,
                        name,
                        scope == Scope::Local,
                    )?
                {
                    return Ok(
                        json!({"action":action,"resource":"mcp","name":name,"scope":"system","file":source,"runtimeStatus":"reload_queued","restartRequired":false,"usableFrom":"after_active_work_is_idle"}),
                    );
                }
                Ok(
                    json!({"action":action,"resource":"mcp","name":name,"scope":scope.name(),"file":target,"runtimeStatus":"reload_queued","restartRequired":false,"usableFrom":"after_active_work_is_idle"}),
                )
            }
            "logout" => {
                let name = required_string(input, "name")?;
                let (target, _, entry) = self.resolve_mcp_readable(name, optional_scope(input)?)?;
                let entry = loaded_mcp_entry(&target, entry);
                self.mcp.clear_oauth(name).await.map_err(core_error)?;
                let live = if entry.enabled {
                    self.mcp.reconnect(name).await.map_err(core_error)?
                } else {
                    self.mcp.configure(name, entry).await.map_err(core_error)?
                };
                Ok(
                    json!({"action":"logout","resource":"mcp","name":name,"runtimeStatus":live.status,"credentialsRemoved":true}),
                )
            }
            "auth" => {
                let name = required_string(input, "name")?;
                self.resolve_mcp_readable(name, optional_scope(input)?)?;
                let callback_port = input
                    .get("callbackPort")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let callback_port =
                    u16::try_from(callback_port).context("callbackPort exceeds 65535")?;
                let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, callback_port)).await?;
                let address = listener.local_addr()?;
                let redirect_uri = format!("http://{address}/mcp/oauth/callback");
                let session = self
                    .mcp
                    .begin_oauth(name, &redirect_uri)
                    .await
                    .map_err(core_error)?;
                open_browser(&session.authorization_url)?;
                let callback_url =
                    receive_oauth_callback(&listener, &redirect_uri, &session.authorization_url)
                        .await?;
                session.complete(&callback_url).await.map_err(core_error)?;
                let live = self.mcp.reconnect(name).await.map_err(core_error)?;
                Ok(
                    json!({"action":"auth","resource":"mcp","name":name,"authenticated":true,"runtimeStatus":live.status}),
                )
            }
            _ => bail!("unsupported mcp_manage action '{action}'"),
        }
    }

    fn command(&self, input: &Value) -> Result<Value> {
        let action = required_string(input, "action")?;
        let result = match action {
            "create" => {
                let name = required_string(input, "name")?;
                let template = required_string(input, "template")?;
                let scope = create_scope(input)?;
                let target = self.command_path(name, scope)?;
                if target.exists() {
                    bail!("command '{name}' already exists at {}", target.display());
                }
                write_command(&target, input, BTreeMap::new(), template)?;
                changed_with_scope("create", "command", name, &target, scope, false)
            }
            "edit" => {
                let name = required_string(input, "name")?;
                let (target, scope) = self.resolve_command(name, optional_scope(input)?)?;
                let raw = fs::read_to_string(&target)?;
                let (front, body) = editable_markdown(&raw, &target)?;
                let template = optional_string(input, "template").unwrap_or(body.trim());
                write_command(&target, input, front, template)?;
                changed_with_scope("edit", "command", name, &target, scope, false)
            }
            "delete" => {
                let name = required_string(input, "name")?;
                let (target, scope) = self.resolve_command(name, optional_scope(input)?)?;
                fs::remove_file(&target)?;
                changed_with_scope("delete", "command", name, &target, scope, false)
            }
            "get" => {
                let name = required_string(input, "name")?;
                if let Ok((target, scope)) = self.resolve_command(name, optional_scope(input)?) {
                    command_detail(name, &target, Some(scope), true)?
                } else {
                    let command = asset_layers(&self.paths, &self.workspace, self.trust_project)
                        .commands
                        .into_iter()
                        .find(|command| command.name == name)
                        .with_context(|| format!("command '{name}' not found"))?;
                    let base = same_command(&self.system_definitions().1, &command);
                    let mut value = serde_json::to_value(command)?;
                    if base {
                        value["scope"] = json!("system");
                    }
                    value["managed"] = json!(false);
                    value["readOnly"] = json!(true);
                    value
                }
            }
            "list" => {
                let requested = optional_scope(input)?;
                let mut values = Vec::new();
                let mut managed = BTreeSet::new();
                for scope in scopes(requested) {
                    let root = self.command_root(scope);
                    for target in markdown_files(&root) {
                        let name = target
                            .strip_prefix(&root)
                            .unwrap_or(&target)
                            .with_extension("")
                            .to_string_lossy()
                            .replace('\\', "/")
                            .replace("%3A", ":");
                        managed.insert(name.clone());
                        values.push(command_detail(&name, &target, Some(scope), true)?);
                    }
                }
                if requested.is_none() {
                    let base = self.system_definitions();
                    for command in
                        asset_layers(&self.paths, &self.workspace, self.trust_project).commands
                    {
                        if !managed.contains(&command.name) {
                            let from_system = same_command(&base.1, &command);
                            let mut value = serde_json::to_value(command)?;
                            if from_system {
                                value["scope"] = json!("system");
                            }
                            value["managed"] = json!(false);
                            value["readOnly"] = json!(true);
                            values.push(value);
                        }
                    }
                }
                Value::Array(values)
            }
            _ => bail!("unsupported command_manage action '{action}'"),
        };
        Ok(result)
    }

    fn desktop_memory(&self, input: &Value) -> Result<Value> {
        let root = self.memory_root();
        let action = required_string(input, "action")?;
        let settings_path = root.join("settings.json");
        let revision = |path: &Path| -> Result<String> {
            let bytes = match fs::read(path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(error.into()),
            };
            Ok(format!("{:x}", Sha256::digest(bytes)))
        };
        let _lock = haarness_profiles::configuration_lock(&self.paths.data)?;
        let _editor_lock = super::desktop::editor_lock(&self.paths.data, "memory")?;
        haarness_profiles::check_path(&root)?;
        match action {
            "status" | "configure" => {
                if action == "configure" {
                    anyhow::ensure!(
                        input["revision"].as_str() == Some(&revision(&settings_path)?),
                        "Memory settings changed. Refresh before saving."
                    );
                    let settings: super::memory_settings::MemorySettings =
                        serde_json::from_value(input["config"].clone())?;
                    settings.validate()?;
                    haarness_profiles::write(
                        &settings_path,
                        &serde_json::to_vec_pretty(&settings)?,
                        true,
                    )?;
                }
                let read = super::memory_settings::read(&root);
                Ok(
                    json!({"config": read.settings, "settingsUnreadable": read.unreadable,
                    "directory": root, "revision": revision(&settings_path)?,
                    "memories": self.memory_entries(&json!({}))?}),
                )
            }
            "read" | "create" | "save" | "delete" => {
                let name = required_string(input, "name")?;
                validate_memory_name(name)?;
                let path = root.join(format!("{name}.md"));
                haarness_profiles::check_path(&path)?;
                if action == "create" {
                    anyhow::ensure!(!path.exists(), "A memory with this name already exists");
                } else if action != "read" {
                    anyhow::ensure!(
                        input["revision"].as_str() == Some(&revision(&path)?),
                        "Memory changed. Reopen it before saving."
                    );
                }
                let mut result =
                    self.memory_unlocked(if action == "create" { "save" } else { action }, input)?;
                result["revision"] = json!(revision(&path)?);
                Ok(result)
            }
            _ => bail!("unsupported desktop memory action"),
        }
    }

    fn memory(&self, action: &str, input: &Value) -> Result<Value> {
        let _lock = super::desktop::editor_lock(&self.paths.data, "memory")?;
        self.memory_unlocked(action, input)
    }

    fn memory_unlocked(&self, action: &str, input: &Value) -> Result<Value> {
        let root = self.memory_root();
        let result = match action {
            "save" => {
                let name = required_string(input, "name")?;
                validate_memory_name(name)?;
                let description = required_string(input, "description")?;
                let content = required_string(input, "content")?;
                if content.len() > 40 * 1024 {
                    bail!("memory content exceeds the 40 KiB limit");
                }
                let kind = optional_string(input, "type").unwrap_or("project");
                if !matches!(kind, "user" | "feedback" | "project" | "reference") {
                    bail!("memory type must be user, feedback, project, or reference");
                }
                fs::create_dir_all(&root)?;
                let target = root.join(format!("{name}.md"));
                haarness_profiles::check_path(&target)?;
                let mut front = match fs::read_to_string(&target) {
                    Ok(raw) => parse_markdown(&raw).0,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                    Err(error) => return Err(error.into()),
                };
                front.extend(BTreeMap::from([
                    ("name".into(), name.into()),
                    ("description".into(), description.into()),
                    ("type".into(), kind.into()),
                ]));
                let temporary = root.join(format!(".{}.tmp", uuid::Uuid::now_v7()));
                let result = (|| -> Result<()> {
                    write_markdown(&temporary, &front, content)?;
                    haarness_profiles::write(&target, &fs::read(&temporary)?, true)?;
                    Ok(())
                })();
                let _ = fs::remove_file(temporary);
                result?;
                changed("save", "memory", name, &target, false)
            }
            "delete" => {
                let name = required_string(input, "name")?;
                validate_memory_name(name)?;
                let target = root.join(format!("{name}.md"));
                if !target.is_file() {
                    bail!("memory '{name}' not found");
                }
                fs::remove_file(&target)?;
                changed("delete", "memory", name, &target, false)
            }
            "read" => {
                let name = required_string(input, "name")?;
                validate_memory_name(name)?;
                memory_detail(&root.join(format!("{name}.md")))?
            }
            "list" => Value::Array(
                self.memory_entries(input)?
                    .into_iter()
                    .take(limit(input))
                    .collect(),
            ),
            "search" => {
                let query = required_string(input, "query")?.to_ascii_lowercase();
                Value::Array(
                    markdown_files(&root)
                        .into_iter()
                        .map(|path| memory_detail(&path))
                        .collect::<Result<Vec<_>>>()?
                        .into_iter()
                        .filter(|entry| entry.to_string().to_ascii_lowercase().contains(&query))
                        .take(limit(input))
                        .collect(),
                )
            }
            _ => bail!("unsupported memory action '{action}'"),
        };
        Ok(result)
    }

    async fn task_abort(&self, input: &Value, context: &ToolContext) -> Result<Value> {
        let child = parse_thread_id(required_string(input, "taskId")?)?;
        let info = self
            .runtime()?
            .abort_child_task(context.thread_id, child)
            .await
            .map_err(core_error)?;
        Ok(json!({"status":"cancelled","taskId":info.child_thread_id,"turnId":info.child_turn_id}))
    }

    async fn session_manage(&self, input: &Value, context: &ToolContext) -> Result<Value> {
        let runtime = self.runtime()?;
        let action = required_string(input, "action")?;
        let include_archived = input
            .get("includeArchived")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match action {
            "list" => {
                let query = optional_string(input, "query").map(str::to_ascii_lowercase);
                let mut threads = runtime
                    .list_threads(include_archived)
                    .await
                    .map_err(core_error)?;
                threads.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
                Ok(Value::Array(
                    threads
                        .into_iter()
                        .filter(|thread| {
                            query.as_ref().is_none_or(|query| {
                                thread.title.to_ascii_lowercase().contains(query)
                            })
                        })
                        .take(limit(input))
                        .map(thread_summary)
                        .collect(),
                ))
            }
            "search" => {
                let query = required_string(input, "query")?.to_ascii_lowercase();
                let mut matches = Vec::new();
                let mut threads = runtime
                    .list_threads(include_archived)
                    .await
                    .map_err(core_error)?;
                threads.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
                // Transcript search is intentionally bounded. A model should
                // refine its query instead of making one call load every
                // historical transcript into server memory.
                for thread in threads.into_iter().take(200) {
                    for item in runtime.items(thread.id, None).await.map_err(core_error)? {
                        if item_visible_text(&item.data)
                            .is_some_and(|text| text.to_ascii_lowercase().contains(&query))
                        {
                            matches.push(json!({
                                "thread": thread_summary(thread.clone()),
                                "sequence": item.sequence,
                                "item": item.data,
                            }));
                            if matches.len() >= limit(input) {
                                return Ok(Value::Array(matches));
                            }
                        }
                    }
                }
                Ok(Value::Array(matches))
            }
            "read" => {
                let thread = self.resolve_thread(input, context.thread_id).await?;
                let after = input.get("after").and_then(Value::as_u64);
                let mut items = runtime.items(thread.id, after).await.map_err(core_error)?;
                let max = limit(input);
                if after.is_none() && items.len() > max {
                    items = items.split_off(items.len() - max);
                } else {
                    items.truncate(max);
                }
                Ok(json!({"thread":thread_summary(thread),"items":items}))
            }
            "digest" => {
                let thread = self.resolve_thread(input, context.thread_id).await?;
                let active = runtime.active_turn(thread.id).await.map_err(core_error)?;
                let tasks = runtime
                    .background_tasks(thread.id)
                    .await
                    .map_err(core_error)?;
                let todos = runtime.todos(thread.id).await.map_err(core_error)?;
                let items = runtime.items(thread.id, None).await.map_err(core_error)?;
                let recent = items
                    .into_iter()
                    .rev()
                    .filter(|item| {
                        matches!(
                            &item.data,
                            ItemData::UserMessage {
                                synthetic: false,
                                ..
                            } | ItemData::AssistantMessage { .. }
                                | ItemData::Error { .. }
                        )
                    })
                    .take(8)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>();
                Ok(
                    json!({"thread":thread_summary(thread),"activeTurn":active,"tasks":tasks,"todos":todos,"recent":recent}),
                )
            }
            _ => bail!("unsupported session_manage action '{action}'"),
        }
    }

    async fn session_rename(&self, input: &Value, context: &ToolContext) -> Result<Value> {
        let title = required_string(input, "title")?;
        if title.chars().count() > 100 || title.contains(['\r', '\n']) {
            bail!("session title must be one line of at most 100 characters");
        }
        let thread = self
            .runtime()?
            .update_thread(
                context.thread_id,
                UpdateThreadRequest {
                    title: Some(title.into()),
                    archived: None,
                    agent: None,
                    provider: None,
                    model: None,
                    ultracode: None,
                },
            )
            .await
            .map_err(core_error)?;
        Ok(thread_summary(thread))
    }

    async fn session_message(&self, input: &Value, context: &ToolContext) -> Result<Value> {
        let target = self.resolve_thread(input, context.thread_id).await?;
        if target.id == context.thread_id {
            bail!(
                "session_message targets another session; use the current reply for this session"
            );
        }
        let runtime = self.runtime()?;
        let message = required_string(input, "message")?;
        let payload = format!(
            "{}{}:\n{message}",
            haarness_core::SESSION_MESSAGE_PREFIX,
            short_id(context.thread_id)
        );
        let active = runtime.active_turn(target.id).await.map_err(core_error)?;
        let pending_agent_switch = active
            .as_ref()
            .is_some_and(|turn| turn.agent != target.agent);
        let queued = if active.is_some() && !pending_agent_switch {
            runtime
                .queue_input(
                    target.id,
                    QueueInputRequest {
                        preview: None,
                        reasoning_effort: None,
                        delivery: haarness_protocol::InputDelivery::Steer,
                        request_id: RequestId::new(),
                        text: payload,
                        attachments: Vec::new(),
                        resources: Vec::new(),
                        synthetic: false,
                    },
                )
                .await
                .map_err(core_error)?
                .queued
        } else if pending_agent_switch {
            runtime
                .wait_until_quiet(target.id, false, timeout(input), context.cancel.clone())
                .await
                .map_err(core_error)?;
            // Queueing at the idle boundary is atomic with a concurrently
            // scheduled handoff continuation: this message either joins that
            // target-agent turn or starts it with the updated thread identity.
            runtime
                .queue_input(
                    target.id,
                    QueueInputRequest {
                        preview: None,
                        reasoning_effort: None,
                        delivery: haarness_protocol::InputDelivery::Steer,
                        request_id: RequestId::new(),
                        text: payload,
                        attachments: Vec::new(),
                        resources: Vec::new(),
                        synthetic: false,
                    },
                )
                .await
                .map_err(core_error)?
                .queued
        } else {
            let identity = latest_turn_identity(&runtime, &target).await?;
            runtime
                .start_turn(
                    target.id,
                    StartTurnRequest {
                        request_id: RequestId::new(),
                        text: payload,
                        attachments: Vec::new(),
                        resources: Vec::new(),
                        synthetic: false,
                        auto_approve: false,
                        agent: Some(identity.0),
                        provider: Some(identity.1),
                        model: Some(identity.2),
                        reasoning_effort: None,
                        tools: None,
                        command: None,
                    },
                )
                .await
                .map_err(core_error)?;
            false
        };
        let wait = input.get("wait").and_then(Value::as_bool).unwrap_or(false);
        if wait {
            runtime
                .wait_until_quiet(
                    target.id,
                    input
                        .get("waitForBackground")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    timeout(input),
                    context.cancel.clone(),
                )
                .await
                .map_err(core_error)?;
        }
        Ok(
            json!({"threadId":target.id,"queued":queued,"waited":wait,"status":if wait {"idle"} else {"delivered"}}),
        )
    }

    async fn session_wait(&self, input: &Value, context: &ToolContext) -> Result<Value> {
        let target = self.resolve_thread(input, context.thread_id).await?;
        if target.id == context.thread_id {
            bail!("a running session cannot wait for itself to become idle");
        }
        self.runtime()?
            .wait_until_quiet(
                target.id,
                input
                    .get("waitForBackground")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                timeout(input),
                context.cancel.clone(),
            )
            .await
            .map_err(core_error)?;
        Ok(json!({"threadId":target.id,"status":"idle"}))
    }

    async fn switch_details(&self, input: &Value, thread: ThreadId) -> Result<Value> {
        let runtime = self.runtime()?;
        let current = runtime.get_thread(thread).await.map_err(core_error)?;
        anyhow::ensure!(
            current.parent_id.is_none(),
            "agent_switch is available only in root sessions"
        );
        let target = required_string(input, "agent")?;
        ensure_user_agent(target)?;
        let profile = runtime
            .agent_profiles()
            .into_iter()
            .find(|profile| profile.id == target)
            .with_context(|| format!("unknown agent '{target}'"))?;
        anyhow::ensure!(
            profile.mode.as_deref() != Some("subagent"),
            "agent '{target}' is subagent-only"
        );
        let previous = format!("{}/{}", current.provider, current.model);
        // Name the model the next turn runs on, resolved as a turn resolves
        // the agent's value: an alias shows its release, and `inherit` or no
        // model shows the chat's own model, which clients present as no change.
        let model = match profile.model.as_deref() {
            Some(value) => runtime
                .resolve_definition_model(Some(&current.provider), value)
                .map_err(core_error)?
                .map(|(provider, model)| format!("{provider}/{model}")),
            None => None,
        }
        .unwrap_or_else(|| previous.clone());
        Ok(json!({
            "threadId": thread, "agent": target, "previousAgent": current.agent,
            "model": model, "previousModel": previous,
        }))
    }

    async fn agent_switch(&self, input: &Value, context: &ToolContext) -> Result<Value> {
        // Approval binds the exact destination and model resolved before the prompt.
        let details = self.switch_details(input, context.thread_id).await?;
        anyhow::ensure!(
            input.get("__agentSwitch") == Some(&details),
            "Agent or model changed while awaiting approval. Request the switch again."
        );
        let runtime = self.runtime()?;
        let current = runtime
            .get_thread(context.thread_id)
            .await
            .map_err(core_error)?;
        if current.parent_id.is_some() {
            bail!("agent_switch is available only in root sessions");
        }
        let target = required_string(input, "agent")?;
        let profile = runtime
            .agent_profiles()
            .into_iter()
            .find(|profile| profile.id == target)
            .with_context(|| format!("unknown agent '{target}'"))?;
        if profile.mode.as_deref() == Some("subagent") {
            bail!("agent '{target}' is subagent-only");
        }
        if current.agent == target {
            return Ok(json!({"status":"unchanged","agent":target}));
        }
        // Apply the approved model itself; the handoff turn below runs on it
        // too rather than resolving the agent's alias again.
        let (provider, model): (Option<String>, Option<String>) = details["model"]
            .as_str()
            .filter(|model| Some(*model) != details["previousModel"].as_str())
            .and_then(|value| value.split_once('/'))
            .map_or((None, None), |(provider, model)| {
                (Some(provider.into()), Some(model.into()))
            });
        runtime
            .update_thread(
                current.id,
                UpdateThreadRequest {
                    title: None,
                    archived: None,
                    agent: Some(target.into()),
                    provider: provider.clone(),
                    model: model.clone(),
                    ultracode: None,
                },
            )
            .await
            .map_err(core_error)?;
        let weak = Arc::downgrade(&runtime);
        let thread_id = current.id;
        let target = target.to_owned();
        let continuation_agent = target.clone();
        let reason = optional_string(input, "reason")
            .unwrap_or("Continue from the prior agent after the requested switch.")
            .to_owned();
        tokio::spawn(async move {
            let Some(runtime) = weak.upgrade() else {
                return;
            };
            let cancel = tokio_util::sync::CancellationToken::new();
            if runtime
                .wait_until_quiet(thread_id, false, Duration::from_secs(3600), cancel)
                .await
                .is_err()
            {
                return;
            }
            let _ = runtime
                .start_turn(
                    thread_id,
                    StartTurnRequest {
                        request_id: RequestId::new(),
                        text: format!("Agent handoff completed. {reason}"),
                        attachments: Vec::new(),
                        resources: Vec::new(),
                        synthetic: true,
                        auto_approve: false,
                        agent: Some(continuation_agent),
                        provider,
                        model,
                        reasoning_effort: None,
                        tools: None,
                        command: None,
                    },
                )
                .await;
        });
        Ok(
            json!({"status":"switching","agent":target,"instruction":"End this turn now; the target agent will continue automatically."}),
        )
    }

    async fn resolve_thread(&self, input: &Value, default: ThreadId) -> Result<Thread> {
        let runtime = self.runtime()?;
        let Some(value) = optional_string(input, "threadId") else {
            return runtime.get_thread(default).await.map_err(core_error);
        };
        if let Ok(id) = parse_thread_id(value) {
            return runtime.get_thread(id).await.map_err(core_error);
        }
        let matches = runtime
            .list_threads(true)
            .await
            .map_err(core_error)?
            .into_iter()
            .filter(|thread| thread.id.to_string().starts_with(value))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [thread] => Ok(thread.clone()),
            [] => bail!("session '{value}' not found"),
            _ => bail!("session prefix '{value}' is ambiguous"),
        }
    }

    fn command_root(&self, scope: Scope) -> PathBuf {
        match scope {
            Scope::Local => self.workspace.join(".haarness/commands"),
            Scope::Global => self
                .paths
                .config
                .parent()
                .unwrap_or(Path::new("."))
                .join("commands"),
        }
    }

    fn command_path(&self, name: &str, scope: Scope) -> Result<PathBuf> {
        validate_command_name(name)?;
        let mut path = name
            .split('/')
            .fold(self.command_root(scope), |path, part| {
                path.join(part.replace(':', "%3A"))
            });
        path.set_extension("md");
        Ok(path)
    }

    fn resolve_command(&self, name: &str, scope: Option<Scope>) -> Result<(PathBuf, Scope)> {
        resolve_scoped(
            scope,
            |scope| self.command_path(name, scope),
            "command",
            name,
        )
    }

    pub(super) fn memory_root(&self) -> PathBuf {
        memory_root_for(&self.paths, &self.workspace)
    }

    fn memory_entries(&self, _input: &Value) -> Result<Vec<Value>> {
        let mut entries = Vec::new();
        for path in markdown_files(&self.memory_root()) {
            entries.push(memory_summary(&path)?);
        }
        Ok(entries)
    }

    pub(super) fn memory_index(&self) -> Result<String> {
        let settings = super::memory_settings::load(&self.memory_root());
        if !settings.enabled {
            return Ok(String::new());
        }
        let mut index = String::new();
        for entry in self
            .memory_entries(&json!({}))?
            .into_iter()
            .take(settings.index_limit)
        {
            let line = format!(
                "- {} [{}]: {}\n",
                entry
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("memory"),
                entry
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("project"),
                entry
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            );
            if index.len() + line.len() > 25 * 1024 {
                break;
            }
            index.push_str(&line);
        }
        Ok(index)
    }

    fn skill_root(&self, scope: Scope) -> PathBuf {
        match scope {
            Scope::Local => self.workspace.join(".haarness/skills"),
            Scope::Global => self.paths.skills.clone(),
        }
    }

    fn skill_path(&self, name: &str, scope: Scope) -> Result<PathBuf> {
        Ok(managed_name_path(&self.skill_root(scope), name)?.join("SKILL.md"))
    }

    fn resolve_skill(&self, name: &str, scope: Option<Scope>) -> Result<(PathBuf, Scope)> {
        resolve_scoped(scope, |scope| self.skill_path(name, scope), "skill", name)
    }

    fn agent_root(&self, scope: Scope) -> PathBuf {
        match scope {
            Scope::Local => self.workspace.join(".haarness/agents"),
            Scope::Global => self
                .paths
                .config
                .parent()
                .unwrap_or(Path::new("."))
                .join("agents"),
        }
    }

    fn agent_path(&self, name: &str, scope: Scope) -> Result<PathBuf> {
        let mut path = managed_name_path(&self.agent_root(scope), name)?;
        path.set_extension("md");
        Ok(path)
    }

    fn resolve_agent(&self, name: &str, scope: Option<Scope>) -> Result<(PathBuf, Scope)> {
        resolve_scoped(scope, |scope| self.agent_path(name, scope), "agent", name)
    }

    fn plugin_root(&self, scope: Scope) -> PathBuf {
        match scope {
            Scope::Local => self.workspace.join(".haarness/plugins/local"),
            Scope::Global => self.paths.config_dir().join("plugins/local"),
        }
    }

    fn plugin_path(&self, name: &str, scope: Scope) -> Result<PathBuf> {
        managed_name_path(&self.plugin_root(scope), name)
    }

    fn resolve_plugin(&self, name: &str, scope: Option<Scope>) -> Result<(PathBuf, Scope)> {
        resolve_scoped(scope, |scope| self.plugin_path(name, scope), "plugin", name)
    }

    fn resolve_mcp(
        &self,
        name: &str,
        scope: Option<Scope>,
    ) -> Result<(PathBuf, Scope, McpConfigureRequest)> {
        for scope in scopes(scope) {
            let target =
                mcp_entry_target(&self.paths, &self.workspace, scope == Scope::Local, name);
            if let Ok(entry) = read_mcp_entry(&target, name) {
                return Ok((target, scope, entry));
            }
        }
        if let Some((file, _)) = system_mcp_entry(&self.workspace, name) {
            bail!(
                "MCP server '{name}' comes from the system folder ({}), which Haarness does not change; turn it off instead",
                file.display()
            );
        }
        if asset_layers(&self.paths, &self.workspace, self.trust_project)
            .mcp
            .contains_key(name)
        {
            bail!(
                "MCP server '{name}' is plugin-managed and read-only; change the containing plugin instead"
            );
        }
        bail!("MCP server '{name}' not found in local or global configuration")
    }

    /// The agents and commands the system folder defines, empty without one.
    fn system_definitions(
        &self,
    ) -> (
        Vec<haarness_core::CustomAgent>,
        Vec<haarness_protocol::CommandInfo>,
    ) {
        crate::layout::SystemFolder::for_workspace(&self.workspace)
            .map(|base| crate::jsonconfig::system_definitions(&base))
            .unwrap_or_default()
    }

    /// Whether the agent in effect is the system folder's own definition.
    fn system_agent(&self, agent: &haarness_core::CustomAgent) -> bool {
        same_agent(&self.system_definitions().0, agent)
    }

    /// [`Self::resolve_mcp`], else the system folder's entry (no scope), for an
    /// action that only reads the server or its sign-in.
    fn resolve_mcp_readable(
        &self,
        name: &str,
        scope: Option<Scope>,
    ) -> Result<(PathBuf, Option<Scope>, McpConfigureRequest)> {
        match self.resolve_mcp(name, scope) {
            Ok((target, scope, entry)) => Ok((target, Some(scope), entry)),
            Err(error) => system_mcp_entry(&self.workspace, name)
                .filter(|_| scope.is_none())
                .map(|(file, entry)| (file, None, entry))
                .ok_or(error),
        }
    }

    /// Before changing MCP server `name`: a server only the system folder
    /// defines is copied into the chosen layer (global by default) first, so
    /// the change is the person's own version over it.
    fn adopt_base_mcp(&self, name: &str, scope: Option<Scope>) -> Result<()> {
        adopt_system_mcp_entry(
            &self.paths,
            &self.workspace,
            name,
            scope == Some(Scope::Local),
        )
    }
}

/// Whether `path` is in the system folder (`HAARNESS_SYSTEM_CONFIG_DIR`), whose
/// definitions are listed with a `system` scope and never changed in place.
fn in_system_folder(path: &Path) -> bool {
    crate::layout::system_config_dir().is_some_and(|base| {
        path.starts_with(&base)
            || path
                .canonicalize()
                .ok()
                .zip(base.canonicalize().ok())
                .is_some_and(|(path, base)| path.starts_with(base))
    })
}

/// Whether `agent`, as configuration resolved it, is one of the system folder's
/// definitions rather than a version of a layer above it.
fn same_agent(base: &[haarness_core::CustomAgent], agent: &haarness_core::CustomAgent) -> bool {
    base.iter().any(|candidate| {
        candidate.id == agent.id
            && candidate.prompt == agent.prompt
            && candidate.description == agent.description
            && candidate.model == agent.model
    })
}

/// [`same_agent`] for a command.
fn same_command(
    base: &[haarness_protocol::CommandInfo],
    command: &haarness_protocol::CommandInfo,
) -> bool {
    base.iter().any(|candidate| {
        candidate.name == command.name
            && candidate.template == command.template
            && candidate.execution.path == command.execution.path
    })
}

pub(super) fn memory_root_for(paths: &Paths, workspace: &Path) -> PathBuf {
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    // Layout migration intentionally aliases the application data directory.
    // Resolve that trusted root, while still rejecting links below it.
    paths
        .data
        .canonicalize()
        .unwrap_or_else(|_| paths.data.clone())
        .join("memory")
        .join(format!("{digest:x}"))
}

#[async_trait]
impl ManagementRegistry for ManagementService {
    async fn prepare(
        &self,
        resource: &str,
        mut arguments: Value,
        thread: ThreadId,
    ) -> CoreResult<Value> {
        if resource == "agent_switch" {
            arguments["__agentSwitch"] = self
                .switch_details(&arguments, thread)
                .await
                .map_err(|error| CoreError::Tool(error.to_string()))?;
        }
        Ok(arguments)
    }

    async fn user_request(&self, resource: &str, input: Value) -> CoreResult<Value> {
        if resource == "capabilities" {
            return self
                .capability_request(&input)
                .await
                .map_err(|e| CoreError::Tool(e.to_string()));
        }
        if resource == "marketplace" {
            return self
                .marketplace_request(&input)
                .await
                .map_err(|e| CoreError::Tool(e.to_string()));
        }
        if matches!(
            resource,
            "cloud_agent_manage" | "cloud_workspace" | "cloud_chat" | "cloud_schedule"
        ) {
            let cloud = haarness_account::cloud::Cloud::new(
                super::account::client().map_err(|e| CoreError::Tool(e.to_string()))?,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .map_err(|e| CoreError::Tool(e.to_string()))?;
            return match resource {
                "cloud_agent_manage" => cloud.agent(input).await,
                "cloud_chat" => cloud.chat(input).await,
                "cloud_schedule" => cloud.schedule(input).await,
                _ => cloud.workspace(input).await,
            }
            .map_err(|e| CoreError::Tool(e.to_string()));
        }
        self.ui_request(resource, &input)
            .await
            .map_err(|error| CoreError::Tool(error.to_string()))
    }

    async fn execute(
        &self,
        resource: &str,
        arguments: Value,
        context: ToolContext,
    ) -> CoreResult<ToolOutput> {
        if context.cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        if resource.starts_with("memory_")
            && !super::memory_settings::load(&self.memory_root()).enabled
        {
            return Err(CoreError::Tool("Project memory is disabled".into()));
        }
        let mut result = match resource {
            "marketplace" => {
                if arguments.get("manifest").is_some() {
                    return Err(CoreError::Invalid("Use a marketplace resource id".into()));
                }
                self.marketplace_request(&arguments).await
            }
            "capabilities" => self.capability_request(&arguments).await,
            "skill" => self.skill(&arguments).await,
            "agent" => self.agent(&self.with_own_tool_names(&arguments)),
            "plugin" => self.plugin(&arguments),
            "mcp" => self.mcp(&arguments).await,
            "command" => self.command(&arguments),
            "memory_list" => self.memory("list", &arguments),
            "memory_read" => self.memory("read", &arguments),
            "memory_search" => self.memory("search", &arguments),
            "memory_save" => self.memory("save", &arguments),
            "memory_delete" => self.memory("delete", &arguments),
            "task_abort" => self.task_abort(&arguments, &context).await,
            "session_manage" => self.session_manage(&arguments, &context).await,
            "session_rename" => self.session_rename(&arguments, &context).await,
            "session_message" => self.session_message(&arguments, &context).await,
            "session_wait" => self.session_wait(&arguments, &context).await,
            "agent_switch" => self.agent_switch(&arguments, &context).await,
            "cloud_agent_manage" | "cloud_workspace" | "cloud_chat" | "cloud_schedule" => {
                let client =
                    super::account::client().map_err(|e| CoreError::Tool(e.to_string()))?;
                let cloud = haarness_account::cloud::Cloud::new(client, context.cancel.clone())
                    .await
                    .map_err(|e| CoreError::Tool(e.to_string()))?;
                if resource == "cloud_agent_manage" {
                    cloud.agent(arguments.clone()).await
                } else if resource == "cloud_schedule" {
                    cloud.schedule(arguments.clone()).await
                } else if resource == "cloud_chat" {
                    cloud.chat(arguments.clone()).await
                } else {
                    cloud.workspace(arguments.clone()).await
                }
                .map_err(anyhow::Error::from)
            }
            _ => Err(anyhow::anyhow!("unknown management resource '{resource}'")),
        }
        .map_err(|error| CoreError::Tool(error.to_string()))?;
        let action = arguments
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("manage");
        let assets_changed = (matches!(
            resource,
            "skill" | "agent" | "plugin" | "command" | "memory_save" | "memory_delete"
        ) && !matches!(action, "get" | "list" | "read" | "search"))
            || resource == "mcp"
                && matches!(action, "add" | "edit" | "remove" | "enable" | "disable");
        if assets_changed {
            let runtime = self
                .runtime()
                .map_err(|error| CoreError::Tool(error.to_string()))?;
            if matches!(resource, "skill" | "agent" | "plugin" | "command") {
                result["restartRequired"] = json!(false);
            }
            // Commands, agents, and memory are lock-backed runtime overlays and
            // are safe to replace while the management turn is still active.
            // Refresh them before notifying clients so a command created in one
            // turn is authoritative for the very next submission. The complete
            // configuration generation (notably tools, MCP, providers, hooks,
            // and workflows) still waits for the idle boundary below.
            self.refresh_runtime_assets(&runtime);
            runtime.events().publish(
                "assets.updated",
                Some(context.thread_id),
                Some(context.turn_id),
                None,
                json!({"resource":resource,"action":action,"status":"live_overlays_refreshed"}),
            );
            runtime.events().publish(
                "configuration.reload_queued",
                Some(context.thread_id),
                Some(context.turn_id),
                None,
                json!({"resource":resource,"action":action,"status":"waiting_for_idle"}),
            );
        }
        Ok(ToolOutput {
            text: serde_json::to_string_pretty(&result)
                .map_err(|error| CoreError::Storage(error.to_string()))?,
            metadata: json!({"title": format!("{resource} {action}"), "management": result}),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    Local,
    Global,
}

impl Scope {
    fn name(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Global => "global",
        }
    }
}

fn optional_scope(input: &Value) -> Result<Option<Scope>> {
    match input.get("scope").and_then(Value::as_str) {
        // A row of the system folder sends its scope back: nothing is written
        // there, so it reads as no scope (project, then global, then what
        // the configuration resolves).
        None | Some("system") => Ok(None),
        Some("local") => Ok(Some(Scope::Local)),
        Some("global") => Ok(Some(Scope::Global)),
        Some(value) => bail!("scope must be local or global, got '{value}'"),
    }
}

fn create_scope(input: &Value) -> Result<Scope> {
    Ok(optional_scope(input)?.unwrap_or(Scope::Global))
}

fn scopes(scope: Option<Scope>) -> Vec<Scope> {
    scope.map_or_else(|| vec![Scope::Local, Scope::Global], |scope| vec![scope])
}

fn resolve_scoped(
    scope: Option<Scope>,
    path: impl Fn(Scope) -> Result<PathBuf>,
    kind: &str,
    name: &str,
) -> Result<(PathBuf, Scope)> {
    for scope in scopes(scope) {
        let target = path(scope)?;
        if target.exists() {
            return Ok((target, scope));
        }
    }
    bail!("{kind} '{name}' not found in local or global managed configuration")
}

fn required_string<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{key} is required"))
}

fn optional_string<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str).map(str::trim)
}

fn managed_name_path(root: &Path, name: &str) -> Result<PathBuf> {
    validate_asset_name(name, "asset")?;
    Ok(name
        .split(':')
        .fold(root.to_path_buf(), |path, part| path.join(part)))
}

fn parse_markdown(content: &str) -> (BTreeMap<String, String>, &str) {
    haarness_core::definitions::markdown(content).unwrap_or_else(|_| (BTreeMap::new(), content))
}

/// The frontmatter an edit keeps. Rewriting a file whose frontmatter does not
/// parse would drop its fields or turn the old block into body text, so the
/// edit fails and the file stays as it is.
fn editable_markdown<'a>(
    content: &'a str,
    path: &Path,
) -> Result<(BTreeMap<String, String>, &'a str)> {
    haarness_core::definitions::markdown(content).map_err(|error| {
        let error = match error {
            CoreError::Invalid(message) => message,
            error => error.to_string(),
        };
        anyhow::anyhow!(
            "{}: {error}. Nothing was changed; fix the frontmatter in the file, then try again.",
            path.display()
        )
    })
}

fn write_markdown(path: &Path, front: &BTreeMap<String, String>, body: &str) -> Result<()> {
    if body.trim().is_empty() {
        bail!("markdown body must not be empty");
    }
    let parent = path.parent().context("managed markdown has no parent")?;
    fs::create_dir_all(parent)?;
    let mut content = String::from("---\n");
    for (key, value) in front {
        if !value.trim().is_empty() {
            content.push_str(key);
            content.push_str(": ");
            // JSON strings are valid YAML scalars; preserve multiline text,
            // colons and quotes instead of flattening or corrupting metadata.
            if serde_json::from_str::<Value>(value).is_ok_and(|v| v.is_object() || v.is_array())
                || haarness_core::definitions::plain_value(value)
            {
                content.push_str(value);
            } else {
                content.push_str(&serde_json::to_string(value)?);
            }
            content.push('\n');
        }
    }
    content.push_str("---\n");
    content.push_str(body.trim());
    content.push('\n');
    fs::write(path, content)?;
    Ok(())
}

fn write_command(
    target: &Path,
    input: &Value,
    mut front: BTreeMap<String, String>,
    template: &str,
) -> Result<()> {
    if template.trim().is_empty() {
        bail!("command template must not be empty");
    }
    if let Some(model) = optional_string(input, "model") {
        validate_qualified_model(model, "command")?;
    }
    for (input_key, front_key) in [
        ("description", "description"),
        ("agent", "agent"),
        ("model", "model"),
        ("argumentHint", "argument-hint"),
    ] {
        if input.get(input_key).is_some() {
            set_optional_frontmatter(&mut front, front_key, optional_string(input, input_key));
        }
    }
    write_markdown(target, &front, template)
}

fn command_detail(name: &str, target: &Path, scope: Option<Scope>, managed: bool) -> Result<Value> {
    let raw = fs::read_to_string(target)?;
    let (front, body) = parse_markdown(&raw);
    Ok(json!({
        "name":name,
        "description":front.get("description"),
        "agent":front.get("agent"),
        "model":front.get("model"),
        "argumentHint":front.get("argument-hint"),
        "template":body.trim(),
        "scope":scope.map(Scope::name),
        "path":target,
        "managed":managed,
        "readOnly":!managed,
        "customized":front.get("haarness-customized").is_some_and(|value| value == "true"),
    }))
}

fn validate_memory_name(name: &str) -> Result<()> {
    validate_asset_name(name, "memory")?;
    if name.eq_ignore_ascii_case("MEMORY") || name.starts_with('.') || name.contains(':') {
        bail!("memory name must be a simple non-reserved filename");
    }
    Ok(())
}

fn validate_command_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.split(['/', ':']).any(|part| {
            part.is_empty()
                || !part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
        })
    {
        bail!(
            "command name must contain slash- or colon-separated ASCII letters, digits, hyphens, or underscores"
        );
    }
    Ok(())
}

fn memory_summary(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path)?;
    let (front, _) = parse_markdown(&raw);
    let fallback = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("memory");
    Ok(json!({
        "name":front.get("name").map(String::as_str).unwrap_or(fallback),
        "type":front.get("type").map(String::as_str).unwrap_or("project"),
        "description":front.get("description").map(String::as_str).unwrap_or(""),
        "path":path,
    }))
}

fn memory_detail(path: &Path) -> Result<Value> {
    if !path.is_file() {
        bail!(
            "memory '{}' not found",
            path.file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("memory")
        );
    }
    let raw = fs::read_to_string(path)?;
    let (front, body) = parse_markdown(&raw);
    let mut value = memory_summary(path)?;
    value["content"] = json!(body.trim());
    value["metadata"] = serde_json::to_value(front)?;
    Ok(value)
}

fn limit(input: &Value) -> usize {
    input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 100) as usize
}

fn timeout(input: &Value) -> Duration {
    Duration::from_secs(
        input
            .get("timeoutSeconds")
            .and_then(Value::as_u64)
            .unwrap_or(120)
            .clamp(1, 3600),
    )
}

fn parse_thread_id(value: &str) -> Result<ThreadId> {
    value
        .parse::<uuid::Uuid>()
        .map(ThreadId)
        .with_context(|| format!("'{value}' is not a thread UUID"))
}

fn short_id(id: ThreadId) -> String {
    id.to_string().chars().take(8).collect()
}

fn thread_summary(thread: Thread) -> Value {
    json!({
        "id":thread.id,
        "title":thread.title,
        "parentId":thread.parent_id,
        "archived":thread.archived,
        "agent":thread.agent,
        "provider":thread.provider,
        "model":thread.model,
        "ultracode":thread.ultracode,
        "updatedAt":thread.updated_at,
    })
}

fn item_visible_text(data: &ItemData) -> Option<String> {
    match data {
        ItemData::UserMessage {
            text, synthetic, ..
        } if !synthetic => Some(text.clone()),
        ItemData::AssistantMessage { text } | ItemData::Reasoning { text, .. } => {
            Some(text.clone())
        }
        ItemData::ToolCall {
            name, arguments, ..
        } => Some(format!("{name} {arguments}")),
        ItemData::Error { message, .. } => Some(message.clone()),
        _ => None,
    }
}

async fn latest_turn_identity(
    runtime: &AgentRuntime,
    thread: &Thread,
) -> Result<(String, String, String)> {
    for item in runtime
        .items(thread.id, None)
        .await
        .map_err(core_error)?
        .into_iter()
        .rev()
    {
        if let Some(turn_id) = item.turn_id
            && let Ok(turn) = runtime.turn(turn_id).await
        {
            return Ok((turn.agent, turn.provider, turn.model));
        }
    }
    Ok((
        thread.agent.clone(),
        thread.provider.clone(),
        thread.model.clone(),
    ))
}

fn write_skill(
    target: &Path,
    name: &str,
    description: &str,
    notes: Option<&str>,
    instructions: &str,
) -> Result<()> {
    validate_asset_name(name, "skill")?;
    let mut front = BTreeMap::from([
        ("name".into(), name.into()),
        ("description".into(), description.into()),
    ]);
    if let Some(notes) = notes.filter(|value| !value.is_empty()) {
        front.insert("notes".into(), notes.into());
    }
    write_markdown(target, &front, instructions)
}

fn set_optional_frontmatter(front: &mut BTreeMap<String, String>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        front.insert(key.into(), value.into());
    } else {
        front.remove(key);
    }
}

fn skill_detail(name: &str, target: &Path, scope: Option<Scope>, managed: bool) -> Result<Value> {
    let raw = fs::read_to_string(target)?;
    let (front, body) = parse_markdown(&raw);
    Ok(json!({
        "name": name,
        "description": front.get("description"),
        "notes": front.get("notes"),
        "instructions": body.trim(),
        "scope": scope.map(Scope::name),
        "path": target,
        "managed": managed,
        "readOnly": !managed,
        "customized": front.get("haarness-customized").is_some_and(|value| value == "true"),
    }))
}

fn validate_agent_fields(input: &Value) -> Result<()> {
    if let Some(mode) = optional_string(input, "mode")
        && !matches!(mode, "all" | "primary" | "subagent")
    {
        bail!("agent mode must be all, primary, or subagent");
    }
    if let Some(model) = optional_string(input, "model") {
        validate_qualified_model(model, "agent")?;
    }
    if let Some(tools) = input.get("tools")
        && !tools.is_object()
    {
        bail!("agent tools must be an object of boolean values");
    }
    Ok(())
}

fn agent_frontmatter(
    name: &str,
    description: &str,
    input: &Value,
    existing: Option<BTreeMap<String, String>>,
) -> Result<BTreeMap<String, String>> {
    let mut front = existing.unwrap_or_default();
    front.insert("name".into(), name.into());
    if !description.is_empty() {
        front.insert("description".into(), description.into());
    }
    for field in ["mode", "model"] {
        if input.get(field).is_some() {
            set_optional_frontmatter(&mut front, field, optional_string(input, field));
        }
    }
    if let Some(tools) = input.get("tools") {
        let object = tools.as_object().context("agent tools must be an object")?;
        if object.values().any(|value| !value.is_boolean()) {
            bail!("agent tools values must be booleans");
        }
        front.insert("tools".into(), serde_json::to_string(object)?);
    }
    if let Some(directories) = input.get("externalDirectories") {
        let directories = directories
            .as_array()
            .context("externalDirectories must be an array")?;
        let mut rules = front
            .get("permissions")
            .and_then(|raw| serde_json::from_str::<Vec<PermissionRule>>(raw).ok())
            .unwrap_or_default();
        for directory in directories {
            let directory = directory
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .context("externalDirectories entries must be non-empty strings")?;
            let pattern = if directory.contains('*') {
                directory.to_owned()
            } else {
                format!("{}/*", directory.trim_end_matches(['/', '\\']))
            };
            let rule = PermissionRule {
                pattern: format!("external_directory:{pattern}"),
                decision: PermissionDecision::Allow,
            };
            if !rules.contains(&rule) {
                rules.push(rule);
            }
        }
        front.insert("permissions".into(), serde_json::to_string(&rules)?);
    }
    Ok(front)
}

fn agent_detail(name: &str, target: &Path, scope: Option<Scope>, managed: bool) -> Result<Value> {
    let raw = fs::read_to_string(target)?;
    let (front, body) = parse_markdown(&raw);
    Ok(json!({
        "name":front.get("name").map(String::as_str).unwrap_or(name),
        "description":front.get("description"),"mode":front.get("mode"),
        "model":front.get("model"),"tools":json_front(&front,"tools"),
        "permissions":json_front(&front,"permissions"),"prompt":body.trim(),
        "scope":scope.map(Scope::name),"file":target,"managed":managed,"readOnly":!managed,
        "customized":front.get("haarness-customized").is_some_and(|value| value == "true")
    }))
}

fn json_front(front: &BTreeMap<String, String>, key: &str) -> Value {
    front
        .get(key)
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or(Value::Null)
}

fn markdown_files(root: &Path) -> Vec<PathBuf> {
    walk_files(root, |path| {
        path.extension().and_then(|value| value.to_str()) == Some("md")
    })
}

fn walk_files(root: &Path, accept: impl Fn(&Path) -> bool + Copy) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            values.extend(walk_files(&path, accept));
        } else if accept(&path) {
            values.push(path);
        }
    }
    values.sort();
    values
}

fn plugin_directories(root: &Path) -> Vec<PathBuf> {
    plugin_directories_at_depth(root, 0)
}

fn plugin_directories_at_depth(root: &Path, depth: usize) -> Vec<PathBuf> {
    if depth > 16 {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.join(".claude-plugin/plugin.json").is_file() {
            values.push(path);
        } else if path.is_dir() {
            values.extend(plugin_directories_at_depth(&path, depth + 1));
        }
    }
    values.sort();
    values
}

fn plugin_manifest_name(root: &Path) -> Option<String> {
    let value: Value =
        serde_json::from_str(&fs::read_to_string(root.join(".claude-plugin/plugin.json")).ok()?)
            .ok()?;
    value
        .get("name")?
        .as_str()
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn plugin_detail(name: &str, root: &Path, scope: Option<Scope>, managed: bool) -> Result<Value> {
    let manifest: Value = serde_json::from_str(&fs::read_to_string(
        root.join(".claude-plugin/plugin.json"),
    )?)?;
    Ok(json!({
        "name":manifest.get("name").and_then(Value::as_str).unwrap_or(name),
        "description":manifest.get("description"),"notes":manifest.get("notes"),
        "version":manifest.get("version"),"scope":scope.map(Scope::name),"path":root,
        "enabled":!root.join(".haarness-disabled").exists(),"managed":managed,"readOnly":!managed
    }))
}

fn write_plugin_manifest(
    root: &Path,
    name: &str,
    description: &str,
    notes: Option<&str>,
    version: &str,
) -> Result<()> {
    let mut manifest = Map::from_iter([
        ("name".into(), json!(name)),
        ("description".into(), json!(description)),
        ("version".into(), json!(version)),
    ]);
    if let Some(notes) = notes.filter(|value| !value.is_empty()) {
        manifest.insert("notes".into(), json!(notes));
    }
    fs::write(
        root.join(".claude-plugin/plugin.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    let source = source
        .canonicalize()
        .with_context(|| format!("resolve {}", source.display()))?;
    if !source.is_dir() {
        bail!("import source {} is not a directory", source.display());
    }
    copy_tree_inner(&source, &source, target)
}

fn copy_tree_inner(root: &Path, source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            let resolved = source_path.canonicalize()?;
            if !resolved.starts_with(root) {
                bail!(
                    "import symlink escapes its source root: {}",
                    source_path.display()
                );
            }
            copy_tree_inner(root, &resolved, &target_path)?;
        } else if metadata.is_dir() {
            copy_tree_inner(root, &source_path, &target_path)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &target_path)?;
        } else {
            bail!("unsupported import entry {}", source_path.display());
        }
    }
    Ok(())
}

fn mcp_entry_from_add(input: &Value) -> Result<McpConfigureRequest> {
    let kind = required_string(input, "type")?;
    let enabled = input
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let entry = match kind {
        "local" => McpConfigureRequest {
            transport: "stdio".into(),
            command: Some(required_string(input, "command")?.into()),
            args: string_array(input, "args")?,
            env: string_map(
                input,
                if input.get("environment").is_some() {
                    "environment"
                } else {
                    "env"
                },
            )?,
            cwd: None,
            url: None,
            headers: BTreeMap::new(),
            headers_helper: None,
            timeout_ms: None,
            oauth: None,
            oauth_client_id: None,
            oauth_scopes: Vec::new(),
            enabled,
            apps: input.get("apps").and_then(Value::as_bool).filter(|on| !on),
        },
        "remote" => McpConfigureRequest {
            transport: "streamable_http".into(),
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            url: Some(required_string(input, "url")?.into()),
            headers: string_map(input, "headers")?,
            headers_helper: None,
            timeout_ms: None,
            oauth: input.get("oauth").and_then(Value::as_bool),
            oauth_client_id: optional_string(input, "oauthClientId").map(ToOwned::to_owned),
            oauth_scopes: string_array(input, "oauthScopes")?,
            enabled,
            apps: input.get("apps").and_then(Value::as_bool).filter(|on| !on),
        },
        _ => bail!("MCP type must be local or remote"),
    };
    validate_mcp_entry(&entry)?;
    Ok(entry)
}

fn apply_mcp_edit(entry: &mut McpConfigureRequest, input: &Value) -> Result<()> {
    // The arguments were shown for the command as it was.
    let shown_for = entry.command.clone();
    if let Some(command) = optional_string(input, "command") {
        entry.command = Some(command.into());
    }
    if input.get("args").is_some() {
        entry.args = edited_secret_args(
            shown_for.as_deref(),
            &entry.args,
            string_array(input, "args")?,
        )?;
    }
    let environment = if input.get("environment").is_some() {
        "environment"
    } else {
        "env"
    };
    if input.get(environment).is_some() {
        entry.env = edited_secret_map(&entry.env, string_map(input, environment)?)?;
    }
    if let Some(url) = optional_string(input, "url") {
        // The displayed URL hides credentials; sent back unchanged, it
        // keeps the URL it stands for.
        let unchanged = entry
            .url
            .as_deref()
            .is_some_and(|existing| haarness_core::redact::url(existing) == url);
        if url.contains(haarness_core::redact::REDACTED) && !unchanged {
            bail!("Provide the full URL: it still contains a hidden part");
        }
        if !unchanged {
            entry.url = Some(url.into());
        }
    }
    if input.get("headers").is_some() {
        entry.headers = edited_secret_map(&entry.headers, string_map(input, "headers")?)?;
    }
    if input.get("oauth").is_some() {
        entry.oauth = input.get("oauth").and_then(Value::as_bool);
    }
    if input.get("oauthClientId").is_some() {
        entry.oauth_client_id = optional_string(input, "oauthClientId").map(ToOwned::to_owned);
    }
    if input.get("oauthScopes").is_some() {
        entry.oauth_scopes = string_array(input, "oauthScopes")?;
    }
    if let Some(apps) = input.get("apps") {
        // On is the default and is saved as no setting at all.
        entry.apps = match apps {
            Value::Bool(false) => Some(false),
            Value::Bool(true) | Value::Null => None,
            _ => bail!("apps must be true or false"),
        };
    }
    Ok(())
}

// Management GET redacts credentials. Saving an unchanged editor must preserve
// their values rather than replace them with the display placeholder.
fn edited_secret_map(
    existing: &BTreeMap<String, String>,
    mut next: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    for (name, value) in &mut next {
        if value == "<redacted>" {
            value.clone_from(
                existing
                    .get(name)
                    .with_context(|| format!("Provide a value for {name}"))?,
            );
        }
    }
    Ok(next)
}

/// Arguments sent back with hidden parts keep the values those parts stand
/// for. A list sent back exactly as displayed is unchanged. Otherwise each
/// hidden value is matched by what identifies it (its own text, as in
/// `--token=<redacted>`, or for a bare value the flag before it, as in
/// `--api-key <redacted>`), and only when exactly one argument fits; a
/// value that cannot be matched must be given in full, so a reordered or
/// shortened list never puts one secret in another's place.
fn edited_secret_args(
    command: Option<&str>,
    existing: &[String],
    next: Vec<String>,
) -> Result<Vec<String>> {
    use haarness_core::redact::REDACTED;
    fn key(args: &[String], index: usize) -> (Option<&str>, &str) {
        let previous = (args[index] == REDACTED)
            .then(|| index.checked_sub(1).map(|previous| args[previous].as_str()))
            .flatten();
        (previous, args[index].as_str())
    }
    let shown = haarness_core::redact::command_args(command, existing);
    if next == shown {
        return Ok(existing.to_vec());
    }
    let mut used = BTreeSet::new();
    let mut restored = Vec::with_capacity(next.len());
    for (index, value) in next.iter().enumerate() {
        if !value.contains(REDACTED) {
            restored.push(value.clone());
            continue;
        }
        let wanted = key(&next, index);
        let mut matches = (0..shown.len())
            .filter(|&candidate| shown[candidate].contains(REDACTED))
            .filter(|&candidate| key(&shown, candidate) == wanted);
        match (matches.next(), matches.next()) {
            (Some(candidate), None) if used.insert(candidate) => {
                restored.push(existing[candidate].clone());
            }
            _ => bail!(
                "Give argument {} ({value}) in full: its hidden value cannot be matched to \
                 the one it stood for",
                index + 1
            ),
        }
    }
    Ok(restored)
}

fn validate_mcp_entry(entry: &McpConfigureRequest) -> Result<()> {
    match entry.transport.as_str() {
        "stdio"
            if entry
                .command
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()) =>
        {
            Ok(())
        }
        "streamable_http"
            if entry.url.as_deref().is_some_and(|value| {
                value.starts_with("http://") || value.starts_with("https://")
            }) =>
        {
            Ok(())
        }
        "stdio" => bail!("local MCP config requires command"),
        "streamable_http" => bail!("remote MCP config requires an http:// or https:// URL"),
        value => bail!("unsupported MCP transport '{value}'"),
    }
}

fn string_array(input: &Value, key: &str) -> Result<Vec<String>> {
    let Some(value) = input.get(key) else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .context(format!("{key} must be an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .context(format!("{key} entries must be strings"))
        })
        .collect()
}

fn string_map(input: &Value, key: &str) -> Result<BTreeMap<String, String>> {
    let Some(value) = input.get(key) else {
        return Ok(BTreeMap::new());
    };
    value
        .as_object()
        .context(format!("{key} must be an object"))?
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_owned()))
                .context("map values must be strings")
        })
        .collect()
}

fn mcp_entries(path: &Path) -> Result<BTreeMap<String, McpConfigureRequest>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    if path.extension().is_some_and(|value| value != "toml") {
        let root = crate::jsonconfig::read_json(path)
            .map_err(anyhow::Error::msg)?
            .unwrap_or_default();
        return Ok(root
            .get("mcp")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| {
                crate::jsonconfig::mcp_entry(value).map(|entry| (name.clone(), entry))
            })
            .collect());
    }
    let root: toml::Value = toml::from_str(&fs::read_to_string(path)?)?;
    root.get("mcp").cloned().map_or_else(
        || Ok(BTreeMap::new()),
        |value| value.try_into().map_err(Into::into),
    )
}

fn redact_mcp(entry: &McpConfigureRequest) -> Result<Value> {
    mcp_config_for_display(entry, false)
}

fn changed(action: &str, resource: &str, name: &str, path: &Path, restart: bool) -> Value {
    json!({"action":action,"resource":resource,"name":name,"path":path,"restartRequired":restart})
}

fn changed_with_scope(
    action: &str,
    resource: &str,
    name: &str,
    path: &Path,
    scope: Scope,
    restart: bool,
) -> Value {
    let mut value = changed(action, resource, name, path, restart);
    value["scope"] = json!(scope.name());
    value
}

/// A path an import names, as the file tools read it: relative to the
/// workspace, or in the home folder for `~/…`.
fn absolutize(workspace: &Path, path: &Path) -> PathBuf {
    let expanded = haarness_core::expand_home(&path.to_string_lossy()).into_owned();
    let path = Path::new(&expanded);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    }
}

fn is_plugin_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "plugins")
}

/// A runtime error as a management answer: a tool's failure reads as its
/// own sentence, without the `tool error:` category.
pub(super) fn plain_error(error: CoreError) -> anyhow::Error {
    core_error(error)
}

fn core_error(error: CoreError) -> anyhow::Error {
    match error {
        CoreError::Tool(message) => anyhow::anyhow!(message),
        other => anyhow::anyhow!(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haarness_providers::CredentialStore;
    use haarness_server::FilesystemSkillRegistry;

    #[test]
    fn an_import_path_from_the_home_folder_is_read_there() {
        let home = haarness_protocol::home_dir().unwrap();
        let workspace = Path::new("/work/project");
        assert_eq!(
            absolutize(workspace, Path::new("~/skills/guide")),
            home.join("skills/guide")
        );
        assert_eq!(
            absolutize(workspace, Path::new("./~/guide")),
            workspace.join("./~/guide")
        );
        assert_eq!(
            absolutize(workspace, Path::new("/elsewhere/guide")),
            Path::new("/elsewhere/guide")
        );
    }

    /// An agent the model writes names MCP tools the way providers saw
    /// them; the agent covers the tools themselves.
    #[tokio::test]
    async fn an_agent_the_model_writes_covers_the_tools_it_names() {
        let root = std::env::temp_dir().join(format!(
            "haarness-agent-wire-names-{}",
            uuid::Uuid::now_v7()
        ));
        fs::create_dir_all(&root).unwrap();
        let mcp = Arc::new(NativeMcpRegistry::default());
        let config: haarness_protocol::McpConfigureRequest = serde_json::from_value(json!({
            "transport": "stdio",
            "command": "python3",
            "args": [concat!(env!("CARGO_MANIFEST_DIR"), "/../haarness-mcp/tests/fixtures/names.py")],
            "env": {"LABEL": "Docs Server", "TOOLS": "[\"files.read\"]"},
            "enabled": true
        }))
        .unwrap();
        assert_eq!(
            mcp.configure("Docs Server", config).await.unwrap().status,
            "connected"
        );
        let service = ManagementService::new(
            Paths::new(Some(&root.join("home"))).unwrap(),
            root.clone(),
            true,
            Arc::new(FilesystemSkillRegistry::new(vec![])),
            mcp.clone(),
        );
        let written = service.with_own_tool_names(&json!({
            "action": "create",
            "tools": {"mcp__Docs_Server__files_read": false, "read": true, "mcp__Docs_Server__*": false}
        }));
        // A pattern matches the tools' own names, as everywhere else.
        assert_eq!(
            written["tools"],
            json!({"mcp__Docs Server__files.read": false, "read": true, "mcp__Docs_Server__*": false})
        );
        mcp.disconnect("Docs Server").await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn extension_management_defaults_to_local_and_requires_project_trust() {
        let root = std::env::temp_dir().join(format!(
            "haarness-untrusted-extension-{}",
            uuid::Uuid::now_v7()
        ));
        fs::create_dir_all(&root).unwrap();
        let service = ManagementService::new(
            Paths::new(Some(&root.join("home"))).unwrap(),
            root.clone(),
            false,
            Arc::new(FilesystemSkillRegistry::new(vec![])),
            Arc::new(NativeMcpRegistry::default()),
        );
        for scope in [Value::Null, json!("local")] {
            let error = service
                .capability_request(
                    &json!({"action":"disable", "kind":"extension", "name":"proof", "scope":scope}),
                )
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Project configuration is turned off")
            );
        }
        assert!(!root.join(".haarness").exists());
        fs::create_dir_all(root.join(".haarness/extensions")).unwrap();
        fs::write(
            root.join(".haarness/extensions/proof.ts"),
            "export default {};",
        )
        .unwrap();
        fs::write(
            root.join(".haarness/extensions.json"),
            r#"{"enabled":true,"entries":[]}"#,
        )
        .unwrap();
        let error = service
            .capability_request(
                &json!({"action":"disable", "kind":"extension", "name":"proof", "scope":"global"}),
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Select a unique installed extension")
        );
        assert!(!service.paths.config_dir().join("extensions.json").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn desktop_memory_crud_conflicts_settings_and_project_isolation() {
        let root =
            std::env::temp_dir().join(format!("haarness-memory-editor-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let workspace = root.join("project");
        fs::create_dir_all(&workspace).unwrap();
        let paths = Paths::new(Some(&root.join("home"))).unwrap();
        let make = |workspace| {
            ManagementService::new(
                paths.clone(),
                workspace,
                true,
                Arc::new(FilesystemSkillRegistry::new(vec![])),
                Arc::new(NativeMcpRegistry::default()),
            )
        };
        let service = make(workspace);
        let request = |input: Value| service.desktop_memory(&input);
        let value = json!({"action":"create","name":"architecture","description":"Project architecture","type":"project","content":"Keep modules separate."});
        request(value.clone()).unwrap();
        assert!(request(value.clone()).is_err());
        service
            .memory(
                "save",
                &json!({"name":"second","description":"Second summary","content":"Other guidance"}),
            )
            .unwrap();
        let read = request(json!({"action":"read","name":"architecture"})).unwrap();
        let target = service.memory_root().join("architecture.md");
        fs::write(
            &target,
            fs::read_to_string(&target)
                .unwrap()
                .replace("type: project", "custom: keep-me\ntype: project"),
        )
        .unwrap();
        assert!(
            request(json!({"action":"delete","name":"architecture","revision":read["revision"]}))
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        let read = request(json!({"action":"read","name":"architecture"})).unwrap();
        request(json!({"action":"save","name":"architecture","revision":read["revision"],"description":"Updated","content":"Updated content"})).unwrap();
        assert!(
            fs::read_to_string(&target)
                .unwrap()
                .contains("custom: keep-me")
        );
        let status = request(json!({"action":"status"})).unwrap();
        assert_eq!(status["config"]["enabled"], true);
        #[cfg(unix)]
        {
            let alias = root.join("legacy-data");
            std::os::unix::fs::symlink(&paths.data, &alias).unwrap();
            let mut aliased = paths.clone();
            aliased.data = alias;
            let service = ManagementService::new(
                aliased,
                service.workspace.clone(),
                true,
                Arc::new(FilesystemSkillRegistry::new(vec![])),
                Arc::new(NativeMcpRegistry::default()),
            );
            assert_eq!(
                service.desktop_memory(&json!({"action":"status"})).unwrap()["memories"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            std::os::unix::fs::symlink(&target, service.memory_root().join("linked.md")).unwrap();
            assert!(
                service
                    .desktop_memory(&json!({"action":"read","name":"linked"}))
                    .is_err()
            );
            fs::remove_file(service.memory_root().join("linked.md")).unwrap();
        }

        assert!(request(json!({"action":"configure","revision":status["revision"],"config":{"enabled":true,"indexLimit":0}})).is_err());
        let status=request(json!({"action":"configure","revision":status["revision"],"config":{"enabled":true,"indexLimit":1}})).unwrap();
        assert!(!service.memory_index().unwrap().contains("Second summary"));
        let config = json!({"action":"configure","revision":status["revision"],"config":{"enabled":false,"indexLimit":1}});
        request(config.clone()).unwrap();
        assert!(request(config).is_err());
        assert!(service.memory_index().unwrap().is_empty());
        assert_eq!(
            request(json!({"action":"status"})).unwrap()["memories"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let mut runtime = haarness_core::RuntimeConfig::default();
        super::super::memory_settings::load(&service.memory_root()).apply(&mut runtime);
        assert_eq!(runtime.tool_availability.get("memory_save"), Some(&false));
        let other = root.join("other");
        fs::create_dir_all(&other).unwrap();
        assert!(make(other).memory_entries(&json!({})).unwrap().is_empty());
        assert!(
            request(
                json!({"action":"create","name":"../escape","description":"No","content":"No"})
            )
            .is_err()
        );
        let read = request(json!({"action":"read","name":"architecture"})).unwrap();
        request(json!({"action":"delete","name":"architecture","revision":read["revision"]}))
            .unwrap();
        assert!(!target.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_scope_and_normalizes_agent_external_directories() {
        assert_eq!(
            optional_scope(&json!({"scope":"local"})).unwrap(),
            Some(Scope::Local)
        );
        let front = agent_frontmatter(
            "reviewer",
            "Review code",
            &json!({"externalDirectories":["/tmp/cache", "/opt/shared/*"]}),
            None,
        )
        .unwrap();
        let permissions: Vec<PermissionRule> = serde_json::from_str(&front["permissions"]).unwrap();
        assert_eq!(permissions[0].pattern, "external_directory:/tmp/cache/*");
        assert_eq!(permissions[1].pattern, "external_directory:/opt/shared/*");
    }

    #[test]
    fn copy_tree_rejects_external_symlinks() {
        let root =
            std::env::temp_dir().join(format!("haarness-manage-copy-{}", uuid::Uuid::now_v7()));
        let source = root.join("source");
        let outside = root.join("outside");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), "nope").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, source.join("escape")).unwrap();
            assert!(
                copy_tree(&source, &root.join("target"))
                    .unwrap_err()
                    .to_string()
                    .contains("escapes")
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn definition_customizations_preserve_sources_and_reject_overwrites() {
        let root = std::env::temp_dir().join(format!(
            "haarness-definition-overrides-{}",
            uuid::Uuid::now_v7()
        ));
        let workspace = root.join("workspace");
        let data = root.join("data");
        let config = root.join("config");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&config).unwrap();
        let paths = Paths {
            root: Some(root.clone()),
            data: data.clone(),
            cache: root.join("cache"),
            database: data.join("haarness.db"),
            auth: config.join("auth.json"),
            runtime: root.join("cache/runtime.json"),
            server_lock: root.join("cache/server.lock"),
            skills: data.join("skills"),
            config: config.join("config.toml"),
            workspace_id: None,
        };
        let original_skill = root.join("plugin/skills/guide/SKILL.md");
        fs::create_dir_all(original_skill.parent().unwrap().join("references")).unwrap();
        fs::write(&original_skill, "---\nname: guide\ndescription: Original guide\ncustom: keep\n---\nOriginal instructions.\n").unwrap();
        fs::write(
            original_skill.parent().unwrap().join("references/help.txt"),
            "support",
        )
        .unwrap();
        let skills: Arc<dyn SkillRegistry> = Arc::new(FilesystemSkillRegistry::new(vec![
            (root.join("plugin/skills"), true),
            (paths.skills.clone(), true),
            (workspace.join(".haarness/skills"), true),
        ]));
        let mcp = Arc::new(NativeMcpRegistry::with_auth_dir(data.join("mcp-oauth")));
        let service =
            ManagementService::new(paths.clone(), workspace.clone(), true, skills, mcp.clone());

        // Internal agents remain private even if an old override exists on disk.
        for name in ["title", "compaction"] {
            let target = service.agent_path(name, Scope::Global).unwrap();
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(
                &target,
                format!("---\nname: {name}\ndescription: Internal\n---\nInternal instructions.\n"),
            )
            .unwrap();
            for action in ["get", "create", "edit", "customize", "delete"] {
                assert!(
                    service
                        .desktop_asset(
                            "agent",
                            &json!({"action":action,"name":name,
                    "scope":"global","description":"Changed","prompt":"Changed"})
                        )
                        .await
                        .is_err()
                );
            }
            assert!(
                fs::read_to_string(&target)
                    .unwrap()
                    .contains("Internal instructions.")
            );
        }
        let listed = service.agent(&json!({"action":"list"})).unwrap();
        assert!(
            listed
                .as_array()
                .unwrap()
                .iter()
                .all(|row| !internal_agent(row["name"].as_str().unwrap()))
        );
        let original = fs::read_to_string(&original_skill).unwrap();
        service
            .desktop_asset(
                "skill",
                &json!({"action":"customize","name":"guide","scope":"local",
            "description":"Updated guide","instructions":"Updated instructions."}),
            )
            .await
            .unwrap();
        let target = workspace.join(".haarness/skills/guide/SKILL.md");
        let detail = service
            .skill(&json!({"action":"get","name":"guide","scope":"local"}))
            .await
            .unwrap();
        assert_eq!(detail["customized"], true);
        assert_eq!(detail["instructions"], "Updated instructions.");
        assert!(
            fs::read_to_string(&target)
                .unwrap()
                .contains("custom: keep")
        );
        assert_eq!(
            fs::read_to_string(target.parent().unwrap().join("references/help.txt")).unwrap(),
            "support"
        );
        assert_eq!(fs::read_to_string(&original_skill).unwrap(), original);
        assert!(
            service
                .desktop_asset(
                    "skill",
                    &json!({"action":"customize","name":"guide","scope":"local",
            "description":"Overwrite","instructions":"Bad overwrite"})
                )
                .await
                .is_err()
        );
        assert!(
            service
                .desktop_asset(
                    "agent",
                    &json!({"action":"customize","name":"../escape","prompt":"No"})
                )
                .await
                .is_err()
        );
        let builtin = service
            .agent(&json!({"action":"get","name":"build"}))
            .unwrap();
        assert_eq!(builtin["readOnly"], true);
        assert!(!builtin["prompt"].as_str().unwrap().is_empty());
        service
            .desktop_asset(
                "agent",
                &json!({"action":"customize","name":"build","scope":"global",
            "description":"Custom build","prompt":"Custom build instructions"}),
            )
            .await
            .unwrap();
        let list = service.agent(&json!({"action":"list"})).unwrap();
        assert_eq!(
            list.as_array()
                .unwrap()
                .iter()
                .filter(|item| item["name"] == "build")
                .count(),
            1
        );
        assert_eq!(
            service
                .agent(&json!({"action":"get","name":"build"}))
                .unwrap()["customized"],
            true
        );
        service
            .agent(&json!({"action":"delete","name":"build","scope":"global"}))
            .unwrap();
        assert_eq!(
            service
                .agent(&json!({"action":"get","name":"build"}))
                .unwrap(),
            builtin
        );
        fs::create_dir_all(workspace.join(".opencode/agents")).unwrap();
        let agent_file = workspace.join(".opencode/agents/inherited.md");
        let agent_source = "---\ndescription: Inherited\nmode: subagent\ntools: {\"bash\":false}\npermissions: [{\"pattern\":\"bash:*\",\"decision\":\"deny\"}]\n---\nInherited instructions.\n";
        fs::write(&agent_file, agent_source).unwrap();
        service
            .desktop_asset(
                "agent",
                &json!({"action":"customize","name":"inherited","scope":"local",
            "prompt":"Updated inherited instructions."}),
            )
            .await
            .unwrap();
        let agent = service
            .agent(&json!({"action":"get","name":"inherited"}))
            .unwrap();
        assert_eq!(agent["tools"]["bash"], false);
        assert_eq!(agent["mode"], "subagent");
        assert_eq!(agent["permissions"][0]["decision"], "deny");
        assert_eq!(fs::read_to_string(&agent_file).unwrap(), agent_source);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn edits_keep_claude_code_frontmatter_and_leave_unparseable_files_unchanged() {
        let root = std::env::temp_dir().join(format!(
            "haarness-management-frontmatter-{}",
            uuid::Uuid::now_v7()
        ));
        let workspace = root.join("workspace");
        for directory in [
            workspace.join(".haarness/commands"),
            workspace.join(".haarness/agents"),
            workspace.join(".haarness/skills/broken"),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        let service = ManagementService::new(
            Paths::new(Some(&root.join("home"))).unwrap(),
            workspace.clone(),
            true,
            Arc::new(FilesystemSkillRegistry::new(vec![(
                workspace.join(".haarness/skills"),
                true,
            )])),
            Arc::new(NativeMcpRegistry::default()),
        );

        // Claude Code writes these values unquoted; an edit keeps every field.
        let export = workspace.join(".haarness/commands/export.md");
        fs::write(
            &export,
            "---\ndescription: Export: one file\nargument-hint: [filename] [format]\nallowed-tools: Bash(git commit: *), Read\n---\nExport $1 as $2.\n",
        )
        .unwrap();
        let detail = service
            .command(&json!({"action":"get","name":"export"}))
            .unwrap();
        assert_eq!(detail["description"], "Export: one file");
        assert_eq!(detail["argumentHint"], "[filename] [format]");
        service
            .command(&json!({"action":"edit","name":"export","template":"Export $1 now."}))
            .unwrap();
        let saved = fs::read_to_string(&export).unwrap();
        // Written back as YAML any reader accepts, not in the repaired form.
        assert!(
            saved.contains("\nallowed-tools: \"Bash(git commit: *), Read\"\n"),
            "{saved}"
        );
        assert!(
            saved.contains("\nargument-hint: \"[filename] [format]\"\n"),
            "{saved}"
        );
        let (front, body) = haarness_core::definitions::markdown(&saved).unwrap();
        assert_eq!(front["description"], "Export: one file");
        assert_eq!(front["argument-hint"], "[filename] [format]");
        assert_eq!(front["allowed-tools"], "Bash(git commit: *), Read");
        assert_eq!(body.trim(), "Export $1 now.");

        // Frontmatter that still does not parse: the edit fails and the file
        // keeps every byte, with or without a replacement body.
        let broken =
            "---\ndescription: Broken\nallowed-tools:\n  - Bash(git commit: *)\n---\nBody\n";
        let command = workspace.join(".haarness/commands/broken.md");
        let agent = workspace.join(".haarness/agents/broken.md");
        let skill = workspace.join(".haarness/skills/broken/SKILL.md");
        for path in [&command, &agent, &skill] {
            fs::write(path, broken).unwrap();
        }
        let errors = [
            service
                .command(&json!({"action":"edit","name":"broken","template":"Replaced"}))
                .unwrap_err(),
            service
                .command(&json!({"action":"edit","name":"broken","description":"Renamed"}))
                .unwrap_err(),
            service
                .agent(&json!({"action":"edit","name":"broken","prompt":"Replaced"}))
                .unwrap_err(),
            service
                .agent(&json!({"action":"edit","name":"broken","description":"Renamed"}))
                .unwrap_err(),
            service
                .skill(&json!({"action":"edit","name":"broken","instructions":"Replaced"}))
                .await
                .unwrap_err(),
            service
                .skill(&json!({"action":"edit","name":"broken","description":"Renamed"}))
                .await
                .unwrap_err(),
        ];
        for (error, path) in errors
            .iter()
            .zip([&command, &command, &agent, &agent, &skill, &skill])
        {
            let error = error.to_string();
            assert!(
                error.starts_with(&format!(
                    "{}: invalid definition frontmatter",
                    path.display()
                )) && error.contains("Nothing was changed"),
                "{error}"
            );
        }
        for path in [&command, &agent, &skill] {
            assert_eq!(fs::read_to_string(path).unwrap(), broken);
        }
        // Frontmatter written as code is never rewritten into YAML around it.
        let scripted = "---js\nmodule.exports = { tools: [] }\n---\nBody\n";
        fs::write(&agent, scripted).unwrap();
        let error = service
            .agent(&json!({"action":"edit","name":"broken","prompt":"Replaced"}))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("JavaScript frontmatter is not supported")
                && error.contains("Nothing was changed"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&agent).unwrap(), scripted);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn marketplace_manage_installs_from_claude_code_plugin_marketplaces() {
        // The model's install tool reaches Claude-format marketplaces, as
        // `/plugin` and Desktop's Plugins page do.
        let root = std::env::temp_dir().join(format!(
            "haarness-management-plugin-market-{}",
            uuid::Uuid::now_v7()
        ));
        let workspace = root.join("workspace");
        let data = root.join("data");
        let config = root.join("config");
        let market = root.join("market");
        for dir in [&workspace, &data, &config] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::create_dir_all(market.join(".claude-plugin")).unwrap();
        fs::create_dir_all(market.join("plugins/kit/.claude-plugin")).unwrap();
        fs::write(
            market.join(".claude-plugin/marketplace.json"),
            r#"{"name":"team-tools","plugins":[{"name":"kit","source":"./plugins/kit","description":"Fixture kit"}]}"#,
        )
        .unwrap();
        fs::write(
            market.join("plugins/kit/.claude-plugin/plugin.json"),
            r#"{"name":"kit"}"#,
        )
        .unwrap();
        let paths = Paths {
            root: Some(root.clone()),
            data: data.clone(),
            cache: root.join("cache"),
            database: data.join("haarness.db"),
            auth: config.join("auth.json"),
            runtime: root.join("cache/runtime.json"),
            server_lock: root.join("cache/server.lock"),
            skills: data.join("skills"),
            config: config.join("config.toml"),
            workspace_id: None,
        };
        let skills: Arc<dyn SkillRegistry> = Arc::new(FilesystemSkillRegistry::new(vec![(
            paths.skills.clone(),
            true,
        )]));
        let mcp = Arc::new(NativeMcpRegistry::with_auth_dir(data.join("mcp-oauth")));
        let service = ManagementService::new(paths, workspace, true, skills, mcp);
        let request = async |input: Value| service.marketplace_request(&input).await;

        request(json!({"action":"add_plugin_marketplace","source":market.to_str().unwrap()}))
            .await
            .unwrap();
        let listed = request(json!({"action":"plugin_marketplaces"}))
            .await
            .unwrap();
        assert_eq!(listed[0]["name"], "team-tools");
        assert_eq!(listed[0]["plugins"][0]["install"], "kit@team-tools");
        assert_eq!(listed[0]["plugins"][0]["installed"], false);

        let installed = request(json!({"action":"install","plugin":"kit@team-tools"}))
            .await
            .unwrap();
        assert!(
            installed["plugins"]
                .as_array()
                .unwrap()
                .iter()
                .any(|plugin| plugin["name"] == "kit" && plugin["enabled"] == true)
        );
        let listed = request(json!({"action":"plugin_marketplaces"}))
            .await
            .unwrap();
        assert_eq!(listed[0]["plugins"][0]["installed"], true);
        let disabled = request(json!({"action":"disable","plugin":"kit"}))
            .await
            .unwrap();
        assert!(
            disabled["plugins"]
                .as_array()
                .unwrap()
                .iter()
                .any(|plugin| plugin["name"] == "kit" && plugin["enabled"] == false)
        );
        let removed = request(json!({"action":"remove","plugin":"kit"}))
            .await
            .unwrap();
        assert!(
            !removed["plugins"]
                .as_array()
                .unwrap()
                .iter()
                .any(|plugin| plugin["name"] == "kit")
        );
        assert!(
            request(json!({"action":"install","plugin":"missing@team-tools"}))
                .await
                .unwrap_err()
                .to_string()
                .contains("has no plugin missing")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hidden_mcp_arguments_are_restored_only_where_they_match() {
        let args = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect();
        let existing: Vec<String> = args(&["--api-key", "AAA", "--token", "BBB", "--token=CCC"]);
        let hidden = "<redacted>";
        // Sent back as displayed: unchanged.
        let shown = haarness_core::redact::args(&existing);
        assert_eq!(
            edited_secret_args(None, &existing, shown.clone()).unwrap(),
            existing
        );
        // A shortened list keeps each value with its own flag.
        assert_eq!(
            edited_secret_args(None, &existing, args(&["--token", hidden])).unwrap(),
            args(&["--token", "BBB"])
        );
        // Swapped pairs keep their values.
        assert_eq!(
            edited_secret_args(
                None,
                &existing,
                args(&[
                    "--token",
                    hidden,
                    "--api-key",
                    hidden,
                    "--token=<redacted>",
                    "-v"
                ])
            )
            .unwrap(),
            args(&["--token", "BBB", "--api-key", "AAA", "--token=CCC", "-v"])
        );
        // A hidden value under another flag, or used twice, is refused.
        for next in [
            args(&["--secret", hidden]),
            args(&["--token", hidden, "--token", hidden]),
            args(&["--api-key=<redacted>"]),
        ] {
            let error = edited_secret_args(None, &existing, next)
                .unwrap_err()
                .to_string();
            assert!(error.contains("in full"), "{error}");
        }
        // Two values behind the same flag cannot be told apart once moved,
        // but come back unchanged.
        let headers: Vec<String> = args(&["-H", "A: 1", "-H", "B: 2", "--x"]);
        let shown = haarness_core::redact::args(&headers);
        assert_eq!(edited_secret_args(None, &headers, shown).unwrap(), headers);
        assert!(edited_secret_args(None, &headers, args(&["-H", hidden])).is_err());
        // Addresses and ssh targets are shown whole, so a list holding them
        // can be edited freely; a proxy's password moves with its own text.
        let existing: Vec<String> = args(&[
            "--member=group:devs@example.com",
            "scp",
            "alex@build.fixture.test:icon@2x.png",
            "${USER:-root}@10.0.0.5:/tmp/",
            "--proxy",
            "user:PRIVATE@proxy.fixture.test:3128",
        ]);
        let shown = haarness_core::redact::args(&existing);
        assert_eq!(shown[..4], existing[..4]);
        assert_eq!(shown[5], "<redacted>@proxy.fixture.test:3128");
        let mut next = shown.clone();
        next.reverse();
        next.push("-v".into());
        let mut expected = existing.clone();
        expected.reverse();
        expected.push("-v".into());
        assert_eq!(edited_secret_args(None, &existing, next).unwrap(), expected);
        // A server run through sshpass has its password in its first
        // arguments; saved back as shown, it keeps it.
        let existing: Vec<String> = args(&["-p", "PRIVATE", "ssh", "-p", "22", "host"]);
        let shown = haarness_core::redact::command_args(Some("/usr/bin/sshpass"), &existing);
        assert_eq!(shown, args(&["-p", hidden, "ssh", "-p", "22", "host"]));
        assert_eq!(
            edited_secret_args(Some("/usr/bin/sshpass"), &existing, shown).unwrap(),
            existing
        );
    }

    #[tokio::test]
    async fn non_marketplace_managers_round_trip_real_files_and_queue_mcp_definitions() {
        let root = std::env::temp_dir().join(format!(
            "haarness-management-roundtrip-{}",
            uuid::Uuid::now_v7()
        ));
        let workspace = root.join("workspace");
        let data = root.join("data");
        let config = root.join("config");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&config).unwrap();
        let paths = Paths {
            root: Some(root.clone()),
            data: data.clone(),
            cache: root.join("cache"),
            database: data.join("haarness.db"),
            auth: config.join("auth.json"),
            runtime: root.join("cache/runtime.json"),
            server_lock: root.join("cache/server.lock"),
            skills: data.join("skills"),
            config: config.join("config.toml"),
            workspace_id: None,
        };
        let skills: Arc<dyn SkillRegistry> = Arc::new(FilesystemSkillRegistry::new(vec![
            (paths.skills.clone(), true),
            (workspace.join(".haarness/skills"), true),
        ]));
        let mcp = Arc::new(NativeMcpRegistry::with_auth_dir(data.join("mcp-oauth")));
        let service =
            ManagementService::new(paths.clone(), workspace.clone(), true, skills, mcp.clone());

        service
            .skill(&json!({
                "action":"create","name":"fixture-skill","scope":"local",
                "description":"Fixture skill","instructions":"Do the fixture work."
            }))
            .await
            .unwrap();
        let skill = service
            .skill(&json!({"action":"get","name":"fixture-skill"}))
            .await
            .unwrap();
        assert_eq!(skill["scope"], "local");
        assert_eq!(skill["instructions"], "Do the fixture work.");
        service
            .skill(&json!({
                "action":"edit","name":"fixture-skill","notes":"Needs fixture access",
                "instructions":"Do the updated fixture work."
            }))
            .await
            .unwrap();

        service
            .agent(&json!({
                "action":"create","name":"fixture-agent","scope":"global",
                "description":"Fixture agent","prompt":"Inspect fixtures only.",
                "mode":"subagent","tools":{"bash":false},
                "externalDirectories":["/tmp/fixture"]
            }))
            .unwrap();
        let agent = service
            .agent(&json!({"action":"get","name":"fixture-agent"}))
            .unwrap();
        assert_eq!(agent["mode"], "subagent");
        assert_eq!(agent["tools"]["bash"], false);

        service
            .plugin(&json!({
                "action":"create","name":"team:fixture-plugin","scope":"local",
                "description":"Fixture plugin","version":"1.2.3"
            }))
            .unwrap();
        assert!(
            plugins::all_enabled_roots(&data, paths.config_dir(), &workspace, true)
                .iter()
                .any(|path| path.ends_with("fixture-plugin"))
        );
        service
            .plugin(&json!({"action":"disable","name":"team:fixture-plugin"}))
            .unwrap();
        assert!(plugins::all_enabled_roots(&data, paths.config_dir(), &workspace, true).is_empty());
        service
            .plugin(&json!({"action":"enable","name":"team:fixture-plugin"}))
            .unwrap();
        let plugin_list = service
            .plugin(&json!({"action":"list","scope":"local"}))
            .unwrap();
        assert_eq!(plugin_list[0]["name"], "team:fixture-plugin");

        let mcp_add = service
            .desktop_asset(
                "mcp",
                &json!({
                    "action":"add","name":"fixture-mcp","scope":"local","type":"remote",
                    "url":"http://127.0.0.1:9/mcp?api_key=fixture-key","enabled":false,
                    "headers":{"Authorization":"Bearer fixture-secret"},
                    "oauth":true,"oauthClientId":"fixture-client","oauthScopes":["read","write"]
                }),
            )
            .await
            .unwrap();
        assert_eq!(mcp_add["runtimeStatus"], "reload_queued");
        assert!(mcp.list().await.unwrap().is_empty());
        let mcp_detail = service
            .mcp(&json!({"action":"get","name":"fixture-mcp"}))
            .await
            .unwrap();
        assert!(mcp_detail["runtimeStatus"].is_null());
        let persisted =
            read_mcp_entry(&mcp_config_target(&paths, &workspace, true), "fixture-mcp").unwrap();
        assert_eq!(mcp_add["config"]["headers"]["Authorization"], "<redacted>");
        assert_eq!(persisted.headers["Authorization"], "Bearer fixture-secret");
        assert_eq!(
            mcp_add["config"]["url"],
            "http://127.0.0.1:9/mcp?api_key=<redacted>"
        );
        assert!(!mcp_detail.to_string().contains("fixture-key"));
        let elsewhere = json!({"action":"edit","name":"fixture-mcp","scope":"local",
            "url":"http://127.0.0.1:10/mcp?api_key=<redacted>"});
        assert!(
            service
                .mcp(&elsewhere)
                .await
                .unwrap_err()
                .to_string()
                .contains("Provide the full URL")
        );
        let mut edit = mcp_detail["config"].clone();
        edit["action"] = json!("edit");
        edit["name"] = json!("fixture-mcp");
        edit["scope"] = json!("local");
        service.mcp(&edit).await.unwrap();
        let edited =
            read_mcp_entry(&mcp_config_target(&paths, &workspace, true), "fixture-mcp").unwrap();
        assert_eq!(edited.headers["Authorization"], "Bearer fixture-secret");
        assert_eq!(
            edited.url.as_deref(),
            Some("http://127.0.0.1:9/mcp?api_key=fixture-key")
        );
        assert_eq!(persisted.oauth, Some(true));
        assert_eq!(persisted.oauth_client_id.as_deref(), Some("fixture-client"));
        assert_eq!(persisted.oauth_scopes, ["read", "write"]);
        let local_add = json!({
            "action":"add","name":"desktop-local","scope":"global","type":"local",
            "command":"/path with spaces/server",
            "args":["--path","/a b","--flag=x=y","--api-key","fixture-arg-secret"],
            "environment":{"FIXTURE_TOKEN":"fixture=secret with spaces","EMPTY":""},"enabled":false
        });
        let result = service.desktop_asset("mcp", &local_add).await.unwrap();
        assert_eq!(result["config"]["env"]["FIXTURE_TOKEN"], "<redacted>");
        assert!(!result.to_string().contains("fixture=secret"));
        assert!(!result.to_string().contains("fixture-arg-secret"));
        assert_eq!(
            result["config"]["args"],
            json!(["--path", "/a b", "--flag=x=y", "--api-key", "<redacted>"])
        );
        let target = mcp_config_target(&paths, &workspace, false);
        let arguments = [
            "--path",
            "/a b",
            "--flag=x=y",
            "--api-key",
            "fixture-arg-secret",
        ];
        assert_eq!(
            read_mcp_entry(&target, "desktop-local").unwrap().args,
            arguments
        );
        // Arguments sent back as displayed keep their hidden values.
        service
            .mcp(
                &json!({"action":"edit","name":"desktop-local","scope":"global",
                "args": result["config"]["args"]}),
            )
            .await
            .unwrap();
        let local = read_mcp_entry(&target, "desktop-local").unwrap();
        assert_eq!(local.args, arguments);
        assert_eq!(local.env["FIXTURE_TOKEN"], "fixture=secret with spaces");
        assert_eq!(local.env["EMPTY"], "");
        let before = fs::read(&target).unwrap();
        assert!(
            service
                .desktop_asset("mcp", &local_add)
                .await
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
        assert_eq!(fs::read(&target).unwrap(), before);
        assert_eq!(
            mcp.configure("fixture-mcp", persisted)
                .await
                .unwrap()
                .status,
            "disabled"
        );
        let logout = service
            .mcp(&json!({"action":"logout","name":"fixture-mcp"}))
            .await
            .unwrap();
        assert_eq!(logout["runtimeStatus"], "disabled");
        service
            .mcp(&json!({"action":"remove","name":"fixture-mcp"}))
            .await
            .unwrap();
        assert_eq!(mcp.list().await.unwrap()[0].status, "disabled");

        service
            .plugin(&json!({"action":"delete","name":"team:fixture-plugin"}))
            .unwrap();
        service
            .agent(&json!({"action":"delete","name":"fixture-agent"}))
            .unwrap();
        service
            .skill(&json!({"action":"delete","name":"fixture-skill"}))
            .await
            .unwrap();
        assert!(
            !workspace
                .join(".haarness/plugins/team/fixture-plugin")
                .exists()
        );
        assert!(!config.join("agents/fixture-agent.md").exists());
        assert!(!workspace.join(".haarness/skills/fixture-skill").exists());

        fs::remove_dir_all(root).unwrap();
    }

    /// Enable, disable and edit write back the entry as the file has it, so a
    /// `${TOKEN}` reference is never replaced by the token (or, unset, by
    /// nothing) in what may be a committed project file.
    #[tokio::test]
    async fn mcp_write_backs_keep_environment_references_as_written() {
        let root = std::env::temp_dir().join(format!(
            "haarness-management-mcp-references-{}",
            uuid::Uuid::now_v7()
        ));
        let workspace = root.join("workspace");
        let data = root.join("data");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&data).unwrap();
        let paths = Paths {
            root: Some(root.clone()),
            data: data.clone(),
            cache: root.join("cache"),
            database: data.join("haarness.db"),
            auth: root.join("config/auth.json"),
            runtime: root.join("cache/runtime.json"),
            server_lock: root.join("cache/server.lock"),
            skills: data.join("skills"),
            config: root.join("config/config.toml"),
            workspace_id: None,
        };
        let skills: Arc<dyn SkillRegistry> = Arc::new(FilesystemSkillRegistry::new(vec![(
            paths.skills.clone(),
            true,
        )]));
        let mcp = Arc::new(NativeMcpRegistry::with_auth_dir(data.join("mcp-oauth")));
        let service = ManagementService::new(paths, workspace.clone(), true, skills, mcp);
        let file = workspace.join("haarness.json");
        fs::write(
            &file,
            r#"{"mcp": {
                "remote": {"type": "remote", "url": "https://example.test/${HAARNESS_FIXTURE_REGION:-eu}/mcp",
                           "headers": {"Authorization": "Bearer ${HAARNESS_FIXTURE_TOKEN}"}, "enabled": false},
                "local": {"type": "local", "command": ["server", "${HAARNESS_FIXTURE_ARG:-default}"],
                          "environment": {"TOKEN": "${HAARNESS_FIXTURE_TOKEN}"}, "enabled": false}
            }}"#,
        )
        .unwrap();
        for request in [
            json!({"action": "enable", "name": "local", "scope": "local"}),
            json!({"action": "disable", "name": "local", "scope": "local"}),
            json!({"action": "enable", "name": "remote", "scope": "local"}),
            json!({"action": "edit", "name": "remote", "scope": "local", "oauth": false}),
        ] {
            service.mcp(&request).await.unwrap();
        }
        let text = fs::read_to_string(&file).unwrap();
        let written: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            written["mcp"]["local"],
            json!({"type": "local", "command": ["server", "${HAARNESS_FIXTURE_ARG:-default}"],
                "environment": {"TOKEN": "${HAARNESS_FIXTURE_TOKEN}"}, "enabled": false})
        );
        assert_eq!(
            written["mcp"]["remote"]["headers"]["Authorization"],
            "Bearer ${HAARNESS_FIXTURE_TOKEN}"
        );
        assert_eq!(
            written["mcp"]["remote"]["url"],
            "https://example.test/${HAARNESS_FIXTURE_REGION:-eu}/mcp"
        );
        assert_eq!(written["mcp"]["remote"]["enabled"], true);
        assert_eq!(written["mcp"]["remote"]["oauth"], false);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_refused_sign_in_reads_as_its_own_sentence() {
        let refused = CoreError::Tool(
            "MCP server \"x\" sent a file: sign-in link; only http and https links open".into(),
        );
        assert_eq!(
            plain_error(refused).to_string(),
            "MCP server \"x\" sent a file: sign-in link; only http and https links open"
        );
        assert_eq!(
            plain_error(CoreError::NotFound("MCP server x".into())).to_string(),
            "not found: MCP server x"
        );
    }

    pub(super) fn isolated_service(name: &str) -> (PathBuf, PathBuf, Paths, ManagementService) {
        let root = std::env::temp_dir().join(format!("haarness-{name}-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("workspace");
        let data = root.join("data");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();
        let paths = Paths {
            root: Some(root.clone()),
            data: data.clone(),
            cache: root.join("cache"),
            database: data.join("haarness.db"),
            auth: root.join("config/auth.json"),
            runtime: root.join("cache/runtime.json"),
            server_lock: root.join("cache/server.lock"),
            skills: data.join("skills"),
            config: root.join("config/config.toml"),
            workspace_id: None,
        };
        let skills: Arc<dyn SkillRegistry> = Arc::new(FilesystemSkillRegistry::new(vec![(
            paths.skills.clone(),
            true,
        )]));
        let mcp = Arc::new(NativeMcpRegistry::with_auth_dir(data.join("mcp-oauth")));
        let service = ManagementService::new(paths.clone(), workspace.clone(), true, skills, mcp);
        (root, workspace, paths, service)
    }

    /// Enable, disable and edit write only the keys that changed, under the
    /// entry's own names: an `opencode.json` entry stays one opencode reads
    /// (`type`, a `command` list, `environment`, its `timeout`), a Claude
    /// entry keeps `disabled`, and comments and unknown keys stay.
    #[tokio::test]
    async fn mcp_saves_write_only_what_changed_in_the_entrys_own_shape() {
        let (root, workspace, _, service) = isolated_service("mcp-own-shape");
        let file = workspace.join("opencode.json");
        fs::write(
            &file,
            r#"{
  // The team's servers, shared with opencode.
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "files": {"type": "local", "command": ["npx", "-y", "@fixture/files"],
              "environment": {"ROOT": "{env:FIXTURE_ROOT}"}, "timeout": 5000},
    "claude": {"command": "server", "args": ["--x"], "disabled": true, "x-team-note": "keep"},
    "remote": {"type": "remote", "url": "https://mcp.fixture.test/mcp", "oauth": {"clientId": "old"}},
    "gated": {"type": "remote", "url": "https://gated.fixture.test/mcp"}
  }
}
"#,
        )
        .unwrap();
        for request in [
            json!({"action": "disable", "name": "files", "scope": "local"}),
            json!({"action": "edit", "name": "files", "scope": "local",
                "args": ["-y", "@fixture/files", "--root", "."]}),
            json!({"action": "enable", "name": "claude", "scope": "local"}),
            json!({"action": "edit", "name": "remote", "scope": "local", "oauth": true,
                "oauthClientId": "team-client", "oauthScopes": ["read", "write"]}),
            json!({"action": "edit", "name": "gated", "scope": "local", "oauth": false}),
        ] {
            service.mcp(&request).await.unwrap();
        }
        let text = fs::read_to_string(&file).unwrap();
        assert!(
            text.contains("// The team's servers, shared with opencode."),
            "{text}"
        );
        let written: Value =
            jsonc_parser::parse_to_serde_value(&text, &jsonc_parser::ParseOptions::default())
                .unwrap();
        assert_eq!(
            written["mcp"]["files"],
            json!({"type": "local", "command": ["npx", "-y", "@fixture/files", "--root", "."],
                "environment": {"ROOT": "{env:FIXTURE_ROOT}"}, "timeout": 5000, "enabled": false})
        );
        assert_eq!(
            written["mcp"]["claude"],
            json!({"command": "server", "args": ["--x"], "disabled": false, "x-team-note": "keep"})
        );
        // opencode's own OAuth shape: an object, or `false`.
        assert_eq!(
            written["mcp"]["remote"],
            json!({"type": "remote", "url": "https://mcp.fixture.test/mcp",
                "oauth": {"clientId": "team-client", "scope": "read write"}})
        );
        assert_eq!(
            written["mcp"]["gated"],
            json!({"type": "remote", "url": "https://gated.fixture.test/mcp", "oauth": false})
        );
        // A key written twice is refused instead of edited where it is not read.
        fs::write(
            &file,
            r#"{"mcp": {"twice": {"command": "a", "enabled": true, "enabled": false}}}"#,
        )
        .unwrap();
        let error = service
            .mcp(&json!({"action": "enable", "name": "twice", "scope": "local"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("\"mcp.twice.enabled\" is written more than once"),
            "{error}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Desktop names the servers its connection runs; those Haarness cannot
    /// edit (a plugin's, or set outside the person's files) are read-only,
    /// while one in another file of the same scope is edited where it is.
    #[tokio::test]
    async fn the_list_says_which_named_servers_are_read_only() {
        let (root, workspace, _, service) = isolated_service("mcp-read-only");
        fs::create_dir_all(workspace.join(".haarness")).unwrap();
        fs::write(
            workspace.join(".haarness/haarness.json"),
            r#"{"mcp": {"own": {"command": "own-mcp"}}}"#,
        )
        .unwrap();
        fs::write(
            workspace.join("opencode.json"),
            r#"{"mcp": {"shared": {"type": "local", "command": ["shared-mcp"]}}}"#,
        )
        .unwrap();
        let listed = service
            .mcp(&json!({"action": "list", "names": ["own", "shared", "from-plugin"]}))
            .await
            .unwrap();
        let entry = |name: &str| {
            listed
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["name"] == name)
                .cloned()
                .unwrap_or_else(|| panic!("{name} is not listed: {listed}"))
        };
        assert_eq!(entry("own")["managed"], true);
        assert_ne!(entry("own")["readOnly"], true);
        assert_eq!(entry("shared")["readOnly"], false);
        assert_eq!(entry("from-plugin")["readOnly"], true);
        assert_eq!(entry("from-plugin")["pluginManaged"], true);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn interactive_apps_turn_off_as_a_setting_and_back_on_as_none() {
        let (root, workspace, _, service) = isolated_service("mcp-apps-switch");
        let file = workspace.join("haarness.json");
        fs::write(
            &file,
            r#"{
  // Servers for this project.
  "mcp": {
    "weather": {"command": "weather-mcp", "env": {"TOKEN": "${WEATHER_TOKEN}", "KEY": "${keychain:weather-key}"}}
  }
}
"#,
        )
        .unwrap();
        let read = || -> Value {
            jsonc_parser::parse_to_serde_value(
                &fs::read_to_string(&file).unwrap(),
                &jsonc_parser::ParseOptions::default(),
            )
            .unwrap()
        };
        service
            .mcp(&json!({"action": "edit", "name": "weather", "apps": false}))
            .await
            .unwrap();
        // Only the switch is written; the references stay as written.
        assert_eq!(
            read()["mcp"]["weather"],
            json!({"command": "weather-mcp", "env": {"TOKEN": "${WEATHER_TOKEN}", "KEY": "${keychain:weather-key}"}, "apps": false})
        );
        assert_eq!(read_mcp_entry(&file, "weather").unwrap().apps, Some(false));
        service
            .mcp(&json!({"action": "edit", "name": "weather", "apps": true}))
            .await
            .unwrap();
        assert_eq!(
            read()["mcp"]["weather"],
            json!({"command": "weather-mcp", "env": {"TOKEN": "${WEATHER_TOKEN}", "KEY": "${keychain:weather-key}"}})
        );
        assert!(
            fs::read_to_string(&file)
                .unwrap()
                .contains("// Servers for this project.")
        );
        let error = service
            .mcp(&json!({"action": "edit", "name": "weather", "apps": "no"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("apps must be true or false"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    /// A stand-in for auth.openai.com: hands out a device code, refuses the
    /// first poll the way the real service does, then issues tokens.
    async fn fixture_issuer() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use axum::{Json, Router, http::StatusCode, routing::post};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let polls = Arc::new(AtomicUsize::new(0));
        let counter = polls.clone();
        let router = Router::new()
            .route(
                "/api/accounts/deviceauth/usercode",
                post(|Json(body): Json<Value>| async move {
                    assert_eq!(body["client_id"], "app_EMoamEEZ73f0CkXaXp7hrann");
                    Json(json!({"device_auth_id":"dev-1","user_code":"ABCD-EFGH","interval":"1"}))
                }),
            )
            .route(
                "/api/accounts/deviceauth/token",
                post(move |Json(body): Json<Value>| {
                    let counter = counter.clone();
                    async move {
                        assert_eq!(body["device_auth_id"], "dev-1");
                        assert_eq!(body["user_code"], "ABCD-EFGH");
                        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                            return (StatusCode::FORBIDDEN, Json(json!({"error":"pending"})));
                        }
                        (
                            StatusCode::OK,
                            Json(json!({"authorization_code":"code-1","code_verifier":"verifier-1"})),
                        )
                    }
                }),
            )
            .route(
                "/oauth/token",
                post(|body: String| async move {
                    assert!(body.contains("grant_type=authorization_code"), "{body}");
                    Json(json!({"access_token":"access-1","refresh_token":"refresh-1","expires_in":3600}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (format!("http://{address}"), polls)
    }

    #[test]
    fn remote_control_qr_codes_are_square_with_finder_patterns() {
        let rows = ui::qr_rows(
            "https://haarness.ai/app/remote/sessions/0199f2d4-5a1b-7c3d-8e4f-a0b1c2d3e4f5",
        );
        let size = rows.len();
        assert!(size >= 21 && (size - 21).is_multiple_of(4), "{size}");
        assert!(rows.iter().all(|row| row.len() == size));
        // Each corner except the bottom right holds a 7×7 finder pattern.
        for (top, left) in [(0, 0), (0, size - 7), (size - 7, 0)] {
            assert_eq!(&rows[top][left..left + 7], "1111111");
            assert_eq!(&rows[top + 1][left..left + 7], "1000001");
            assert_eq!(&rows[top + 3][left..left + 7], "1011101");
        }
    }

    fn login_service(root: &Path, issuer: &str, callback_port: u16) -> ManagementService {
        let paths = Paths::new(Some(&root.join("home"))).unwrap();
        ManagementService::new(
            paths,
            root.join("project"),
            true,
            Arc::new(FilesystemSkillRegistry::new(vec![])),
            Arc::new(NativeMcpRegistry::default()),
        )
        .with_openai_oauth(haarness_providers::auth::OpenAiOAuthConfig {
            issuer: issuer.into(),
            client_id: "app_EMoamEEZ73f0CkXaXp7hrann".into(),
            codex_endpoint: "https://chatgpt.example/responses".into(),
            callback_port,
            user_agent: "haarness-test".into(),
        })
    }

    async fn wait_for_login(service: &ManagementService, id: &str) -> Value {
        for _ in 0..100 {
            let status = service
                .ui_request("providers", &json!({"action":"login_status","id":id}))
                .await
                .unwrap();
            if status["data"]["state"] != "pending" {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("sign-in never completed");
    }

    #[tokio::test]
    async fn device_sign_in_reports_code_then_connects_and_stores_the_credential() {
        let root = std::env::temp_dir().join(format!("haarness-connect-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join("project")).unwrap();
        let (issuer, polls) = fixture_issuer().await;
        let service = login_service(&root, &issuer, 0);
        let started = service
            .ui_request(
                "providers",
                &json!({"action":"login","name":"openai","method":"device"}),
            )
            .await
            .unwrap();
        assert_eq!(started["title"], "Sign in to openai");
        let data = &started["data"];
        assert_eq!(data["state"], "pending");
        assert_eq!(data["method"], "device");
        assert_eq!(data["code"], "ABCD-EFGH");
        assert_eq!(data["url"], format!("{issuer}/codex/device"));
        let lines = started["lines"].as_array().unwrap();
        assert!(
            lines
                .iter()
                .any(|line| line.as_str().unwrap().starts_with("Code  ABCD-EFGH")),
            "{lines:?}"
        );
        assert!(
            started["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["label"] == "Cancel sign-in"),
            "{started}"
        );
        let id = data["id"].as_str().unwrap().to_owned();
        let done = wait_for_login(&service, &id).await;
        assert_eq!(done["data"]["state"], "connected", "{done}");
        assert_eq!(done["title"], "Connected openai");
        assert!(
            polls.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "the first poll is refused"
        );
        let stored = CredentialStore::new(service.paths.auth.clone())
            .get("openai")
            .unwrap()
            .expect("credential stored");
        assert_eq!(stored.kind(), "oauth");
        assert_eq!(stored.oauth_parts().unwrap().access, "access-1");
        // A finished sign-in is forgotten; polling it again says so.
        assert!(
            service
                .ui_request("providers", &json!({"action":"login_status","id":id}))
                .await
                .is_err()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn browser_sign_in_completes_from_the_localhost_callback() {
        let root = std::env::temp_dir().join(format!("haarness-connect-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join("project")).unwrap();
        let (issuer, _) = fixture_issuer().await;
        let probe = std::net::TcpListener::bind(("localhost", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let service = login_service(&root, &issuer, port);
        let started = service
            .ui_request(
                "providers",
                &json!({"action":"login","name":"openai","method":"browser"}),
            )
            .await
            .unwrap();
        let data = &started["data"];
        assert_eq!(data["method"], "browser");
        assert_eq!(data["code"], "");
        let url = reqwest::Url::parse(data["url"].as_str().unwrap()).unwrap();
        assert_eq!(url.path(), "/oauth/authorize");
        let state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .unwrap();
        // Play the browser: the provider redirects back to the callback port.
        let response = reqwest::get(format!(
            "http://localhost:{port}/auth/callback?code=code-1&state={state}"
        ))
        .await
        .unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        let id = data["id"].as_str().unwrap().to_owned();
        let done = wait_for_login(&service, &id).await;
        assert_eq!(done["data"]["state"], "connected", "{done}");
        assert_eq!(
            CredentialStore::new(service.paths.auth.clone())
                .get("openai")
                .unwrap()
                .unwrap()
                .kind(),
            "oauth"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cancelling_a_sign_in_forgets_it_and_says_so() {
        let root = std::env::temp_dir().join(format!("haarness-connect-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join("project")).unwrap();
        let (issuer, _) = fixture_issuer().await;
        let service = login_service(&root, &issuer, 0);
        let started = service
            .ui_request(
                "providers",
                &json!({"action":"login","name":"openai","method":"device"}),
            )
            .await
            .unwrap();
        let id = started["data"]["id"].as_str().unwrap().to_owned();
        let cancelled = service
            .ui_request("providers", &json!({"action":"login_cancel","id":id}))
            .await
            .unwrap();
        assert_eq!(cancelled["data"]["state"], "cancelled", "{cancelled}");
        assert_eq!(cancelled["title"], "Sign-in cancelled");
        assert!(
            service
                .ui_request("providers", &json!({"action":"login_status","id":id}))
                .await
                .is_err()
        );
        assert!(
            CredentialStore::new(service.paths.auth.clone())
                .get("openai")
                .unwrap()
                .is_none()
        );
        assert!(
            service
                .ui_request(
                    "providers",
                    &json!({"action":"login","name":"anthropic","method":"browser"})
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("use an API key")
        );
        // The connect catalog is a view, never a mutation: without a runtime it
        // reports the missing runtime, not an unsupported action.
        let error = service
            .ui_request("providers", &json!({"action":"connect"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("runtime"), "{error}");
        assert!(!error.contains("Unsupported"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_browser_sign_in_can_always_be_retried_on_its_port() {
        let root = std::env::temp_dir().join(format!("haarness-connect-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join("project")).unwrap();
        let (issuer, _) = fixture_issuer().await;
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let service = login_service(&root, &issuer, port);
        let browser = json!({"action":"login","name":"openai","method":"browser"});
        let start = || service.ui_request("providers", &browser);
        let first = start().await.unwrap();
        let first = first["data"]["id"].as_str().unwrap().to_owned();
        service
            .ui_request("providers", &json!({"action":"login_cancel","id":first}))
            .await
            .unwrap();
        // Cancelled: the port is free again.
        let second = start().await.unwrap();
        let second = second["data"]["id"].as_str().unwrap().to_owned();
        // Never cancelled (a client went away): the new attempt replaces it.
        let third = start().await.unwrap();
        assert!(
            service
                .ui_request("providers", &json!({"action":"login_status","id":second}))
                .await
                .is_err(),
            "the replaced attempt is forgotten"
        );
        let url = reqwest::Url::parse(third["data"]["url"].as_str().unwrap()).unwrap();
        let state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .unwrap();
        let response = reqwest::get(format!(
            "http://127.0.0.1:{port}/auth/callback?code=code-1&state={state}"
        ))
        .await
        .unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        let done = wait_for_login(&service, third["data"]["id"].as_str().unwrap()).await;
        assert_eq!(done["data"]["state"], "connected", "{done}");
        let _ = fs::remove_dir_all(root);
    }

    /// Answers key checks like a provider: `sk-good` is accepted, the
    /// provider cannot be reached for `sk-offline`, anything else is refused.
    struct KeyCheckingProvider;

    #[async_trait]
    impl haarness_core::ModelProvider for KeyCheckingProvider {
        fn id(&self) -> &str {
            "acme"
        }
        fn info(&self) -> haarness_protocol::ProviderInfo {
            haarness_protocol::ProviderInfo {
                id: "acme".into(),
                name: "Acme".into(),
                authenticated: false,
                models: Vec::new(),
            }
        }
        async fn check_api_key(&self, key: &str) -> haarness_core::KeyCheck {
            use haarness_core::KeyCheck;
            match key {
                "sk-good" => KeyCheck::Accepted,
                "sk-offline" => KeyCheck::Unchecked("Haarness could not reach Acme".into()),
                _ => KeyCheck::Rejected(Some("invalid x-api-key".into())),
            }
        }
        async fn stream(
            &self,
            _request: haarness_core::ProviderRequest,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> CoreResult<haarness_core::ProviderStream> {
            Err(CoreError::Provider("unused".into()))
        }
    }

    #[tokio::test]
    async fn a_refused_api_key_is_not_saved_and_an_unchecked_one_is_saved_with_a_warning() {
        let root =
            std::env::temp_dir().join(format!("haarness-key-check-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join("project")).unwrap();
        let service = login_service(&root, "http://127.0.0.1:1", 0);
        let provider: Arc<dyn haarness_core::ModelProvider> = Arc::new(KeyCheckingProvider);
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(haarness_store::SqliteStore::open(root.join("haarness.db")).unwrap()),
            Arc::new(haarness_core::StaticProviderRegistry::new(vec![provider])),
            Arc::new(haarness_core::EmptyToolRegistry),
            haarness_core::EventBus::new(8),
        ));
        service.attach_runtime(&runtime);
        let service = &service;
        let save = |key: &'static str| async move {
            service
                .ui_request(
                    "providers",
                    &json!({"action":"credential","name":"acme","key":key}),
                )
                .await
        };
        let stored = || {
            CredentialStore::new(service.paths.auth.clone())
                .get("acme")
                .unwrap()
                .and_then(|credential| credential.api_key().map(str::to_owned))
        };
        let refused = save("sk-fake").await.unwrap_err().to_string();
        assert_eq!(
            refused,
            "Acme rejected this API key (invalid x-api-key). Check that you copied all of it, then try again."
        );
        assert_eq!(stored(), None, "a refused key is not saved");

        let offline = save("sk-offline").await.unwrap();
        assert_eq!(
            offline["data"]["warning"],
            "Saved, but the key could not be checked: Haarness could not reach Acme. If chats then fail to authenticate, replace the key."
        );
        assert_eq!(offline["lines"][1], offline["data"]["warning"]);
        assert_eq!(stored().as_deref(), Some("sk-offline"));

        let accepted = save("sk-good").await.unwrap();
        assert!(accepted["data"].get("warning").is_none(), "{accepted}");
        assert_eq!(stored().as_deref(), Some("sk-good"));
        let _ = fs::remove_dir_all(root);
    }

    struct ClaudeCatalog;
    #[async_trait]
    impl haarness_core::ModelProvider for ClaudeCatalog {
        fn id(&self) -> &str {
            "acme"
        }
        fn info(&self) -> haarness_protocol::ProviderInfo {
            let model = |id: &str| haarness_protocol::ModelInfo {
                id: id.into(),
                display_name: id.into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: true,
                context_window: None,
                max_output_tokens: None,
                pricing: None,
            };
            haarness_protocol::ProviderInfo {
                id: "acme".into(),
                name: "Acme".into(),
                authenticated: true,
                models: vec![model("claude-opus-5-5"), model("claude-sonnet-5-5")],
            }
        }
        async fn stream(
            &self,
            _request: haarness_core::ProviderRequest,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> CoreResult<haarness_core::ProviderStream> {
            Err(CoreError::Provider("unused".into()))
        }
    }

    #[tokio::test]
    async fn agent_switch_discloses_and_applies_the_model_the_agent_resolves_to() {
        let root =
            std::env::temp_dir().join(format!("haarness-agent-switch-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join("project")).unwrap();
        let service = ManagementService::new(
            Paths::new(Some(&root.join("home"))).unwrap(),
            root.join("project"),
            true,
            Arc::new(FilesystemSkillRegistry::new(vec![])),
            Arc::new(NativeMcpRegistry::default()),
        );
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(haarness_store::SqliteStore::open(root.join("haarness.db")).unwrap()),
            Arc::new(haarness_core::StaticProviderRegistry::new(vec![
                Arc::new(ClaudeCatalog) as Arc<dyn haarness_core::ModelProvider>,
            ])),
            Arc::new(haarness_core::EmptyToolRegistry),
            haarness_core::EventBus::new(8),
        ));
        service.attach_runtime(&runtime);
        let agent = |id: &str, model: &str| haarness_core::CustomAgent {
            id: id.into(),
            model: Some(model.into()),
            ..Default::default()
        };
        runtime.replace_custom_agents(vec![
            agent("pinned", "opus"),
            agent("release", "claude-opus-5-5"),
            agent("follows", "inherit"),
        ]);
        let thread = runtime
            .create_thread(haarness_protocol::CreateThreadRequest {
                provider: Some("acme".into()),
                model: Some("claude-sonnet-5-5".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let details = |agent: &'static str| {
            let service = &service;
            async move {
                service
                    .switch_details(&json!({"agent": agent}), thread.id)
                    .await
                    .unwrap()
            }
        };
        // An alias or bare release shows the model it resolves to.
        for agent in ["pinned", "release"] {
            let switch = details(agent).await;
            assert_eq!(switch["model"], "acme/claude-opus-5-5", "{agent}");
            assert_eq!(switch["previousModel"], "acme/claude-sonnet-5-5");
        }
        // `inherit` keeps the chat's model, so no change is claimed.
        let follows = details("follows").await;
        assert_eq!(follows["model"], follows["previousModel"]);

        let context = ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: thread.id,
            turn_id: haarness_protocol::TurnId::new(),
            workspace: root.join("project"),
            authorized_external_paths: Vec::new(),
            cancel: tokio_util::sync::CancellationToken::new(),
            invoker: None,
        };
        let switch = details("pinned").await;
        service
            .agent_switch(&json!({"agent":"pinned","__agentSwitch":switch}), &context)
            .await
            .unwrap();
        let switched = runtime.get_thread(thread.id).await.unwrap();
        assert_eq!(
            (
                switched.agent.as_str(),
                switched.provider.as_str(),
                switched.model.as_str()
            ),
            ("pinned", "acme", "claude-opus-5-5")
        );
        let _ = fs::remove_dir_all(root);
    }
}
