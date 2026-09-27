use super::*;
use anyhow::ensure;
use haarness_server::{FilesystemSkillRegistry, PluginRegistry};

impl ManagementService {
    pub(crate) async fn capability_request(&self, input: &Value) -> Result<Value> {
        let action = required_string(input, "action")?;
        ensure!(
            input["scope"].is_null() || matches!(input["scope"].as_str(), Some("local" | "global")),
            "Scope must be local or global"
        );
        if action == "list" {
            let config =
                super::super::load_config(&self.paths, &self.workspace, self.trust_project)?;
            let all_skills = FilesystemSkillRegistry::new_scoped(super::super::skill_roots(
                &self.paths,
                &self.workspace,
                self.trust_project,
                &config,
            ));
            let disabled =
                crate::capability_state::load(&self.paths, &self.workspace, self.trust_project)?;
            let skills = all_skills.discover().await?.into_iter().map(|item| {
                let scope = if Path::new(&item.path).starts_with(self.skill_root(Scope::Local)) {Some("local")} else if Path::new(&item.path).starts_with(self.skill_root(Scope::Global)) {Some("global")} else {None};
                let plugin = item.execution.plugin_root.as_ref().and_then(|p|haarness_core::definitions::plugin_name(Path::new(p)));
                json!({"kind":"skill","name":item.name,"description":item.description,"enabled":disabled.get("skill").and_then(|items|items.get(&item.name)).copied().unwrap_or(true),"plugin":plugin,"path":item.path,"scope":scope,"managed":scope.is_some(),"readOnly":scope.is_none()})
            }).collect::<Vec<_>>();
            let mut agents = self.agent(&json!({"action":"list"}))?;
            for item in agents.as_array_mut().into_iter().flatten() {
                item["enabled"] = json!(
                    disabled
                        .get("agent")
                        .and_then(|items| items.get(item["name"].as_str().unwrap_or_default()))
                        .copied()
                        .unwrap_or(true)
                );
                item["kind"] = json!("agent");
            }
            let plugins = plugins::plugin_infos(
                &self.paths.data,
                self.paths.config_dir(),
                &self.workspace,
                self.trust_project,
            );
            let mut mcp = self.mcp(&json!({"action":"list"})).await?;
            for item in mcp.as_array_mut().into_iter().flatten() {
                item["enabled"] = json!(
                    disabled
                        .get("mcp")
                        .and_then(|items| items.get(item["name"].as_str().unwrap_or_default()))
                        .copied()
                        .unwrap_or_else(|| item["config"]["enabled"]
                            .as_bool()
                            .unwrap_or(item["runtimeStatus"] != "disabled"))
                );
            }
            let extensions =
                crate::extensions::catalog(&self.paths, &self.workspace, self.trust_project)?;
            return Ok(
                json!({"skills":skills,"agents":agents,"plugins":plugins,"mcp":mcp,"extensions":extensions,"marketplace":crate::marketplace::installed(&self.paths,&self.workspace,self.trust_project)?,"applyWhen":"idle"}),
            );
        }
        let kind = required_string(input, "kind")?;
        let name = input["name"].as_str().unwrap_or_default();
        let result = if matches!(kind, "skill" | "agent" | "mcp")
            && matches!(action, "enable" | "disable")
        {
            if kind == "agent" {
                ensure_user_agent(name)?;
            }
            // Confirm the definition exists; disabling never deletes its original files.
            let all = Box::pin(self.capability_request(&json!({"action":"list"}))).await?;
            let group = match kind {
                "skill" => "skills",
                "agent" => "agents",
                _ => "mcp",
            };
            ensure!(
                all[group]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|v| v["name"] == name),
                "Capability not found"
            );
            crate::capability_state::set(
                &self.paths,
                &self.workspace,
                self.trust_project,
                kind,
                name,
                action == "enable",
                input["scope"] != "global",
            )?
        } else if kind == "plugin"
            && matches!(
                action,
                "install" | "marketplace" | "update" | "enable" | "disable" | "remove"
            )
        {
            let service = plugins::PluginService::new(
                self.paths.data.clone(),
                self.paths.config_dir().into(),
                self.workspace.clone(),
                self.trust_project,
            );
            {
                let request = haarness_protocol::PluginActionRequest {
                    action: action.into(),
                    target: input["target"].as_str().unwrap_or(name).into(),
                };
                json!(tokio::task::spawn_blocking(move || service.apply(&request)).await??)
            }
        } else if kind == "extension" {
            ensure!(
                input["scope"] == "global" || self.trust_project,
                "Project configuration is disabled"
            );
            crate::extensions::manage(&self.paths, &self.workspace, self.trust_project, input)
                .await?
        } else {
            let mut input = input.clone();
            if action == "remove" && kind != "mcp" {
                input["action"] = json!("delete");
            }
            self.desktop_asset(kind, &input).await?
        };
        if !matches!(action, "get" | "list") {
            self.capabilities_changed(kind, action);
        }
        Ok(result)
    }

    pub(crate) async fn marketplace_request(&self, input: &Value) -> Result<Value> {
        let action = required_string(input, "action")?;
        if action == "installed" {
            return crate::marketplace::installed(&self.paths, &self.workspace, self.trust_project);
        }
        if matches!(
            action,
            "search" | "detail" | "versions" | "collections" | "manifest"
        ) {
            return crate::marketplace::fetch(input).await;
        }
        if matches!(action, "install" | "update") {
            let manifest = if action == "install" && input.get("manifest").is_some() {
                input["manifest"].clone()
            } else {
                crate::marketplace::fetch(input).await?
            };
            let result = crate::marketplace::install(
                &self.paths,
                &self.workspace,
                self.trust_project,
                &manifest,
                input,
            )
            .await?;
            self.capabilities_changed("marketplace", action);
            return Ok(result);
        }
        let id = required_string(input, "id")?;
        let rows = crate::marketplace::installed(&self.paths, &self.workspace, self.trust_project)?;
        let rows: Vec<_> = rows
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| {
                r["id"] == id && (input.get("scope").is_none() || r["scope"] == input["scope"])
            })
            .collect();
        ensure!(
            rows.len() == 1,
            "Select an installed resource and its scope"
        );
        let receipt = rows[0];
        ensure!(
            matches!(action, "enable" | "disable" | "remove"),
            "Unsupported marketplace action"
        );
        let kind = receipt["kind"].as_str().context("Invalid receipt kind")?;
        let path = Path::new(receipt["path"].as_str().context("Invalid receipt path")?);
        let local = receipt["scope"] == "local";
        let target = if let Some(plugin) = receipt["pluginName"].as_str() {
            json!({"kind":"plugin","name":plugin})
        } else {
            json!({"kind":kind,"name":receipt["name"]})
        };
        if action == "remove" && kind != "extension" {
            let _lock = crate::desktop::editor_lock(&self.paths.data, "marketplace")?;
            crate::marketplace::validate_receipt(&self.paths, &self.workspace, receipt)?;
            if path.exists() {
                ensure!(
                    receipt["installedDigest"] == crate::marketplace::tree_digest(path)?,
                    "Resource was modified locally; preserve your edits before removing it"
                );
                if path.is_dir() {
                    fs::remove_dir_all(path)?;
                } else {
                    fs::remove_file(path)?;
                }
            }
        } else if matches!(kind, "plugin" | "mcp") {
            let _lock = crate::desktop::editor_lock(&self.paths.data, "marketplace")?;
            crate::marketplace::validate_receipt(&self.paths, &self.workspace, receipt)?;
            ensure!(
                path.is_dir(),
                "Installed plugin is missing; reinstall the resource"
            );
            let marker = path.join(".haarness-disabled");
            if action == "disable" {
                haarness_profiles::write(&marker, b"disabled by haarness\n", true)?;
            } else if marker.exists() {
                fs::remove_file(marker)?;
            }
        } else {
            let mut request = target;
            request["action"] = json!(action);
            request["scope"] = receipt["scope"].clone();
            self.capability_request(&request).await?;
        }
        if action == "remove" {
            let id = uuid::Uuid::parse_str(id)?;
            fs::remove_file(
                crate::marketplace::receipt_root(&self.paths, &self.workspace, local)
                    .join(format!("{id}.json")),
            )?;
        }
        self.capabilities_changed(kind, action);
        Ok(json!({"id":id,"action":action,"restartRequired":false}))
    }

    fn capabilities_changed(&self, kind: &str, action: &str) {
        if let Ok(runtime) = self.runtime() {
            // The watcher publishes assets.updated only after the whole generation
            // is installed. Clients retain their catalogs while a turn is active.
            runtime.events().publish(
                "configuration.reload_queued",
                None,
                None,
                None,
                json!({"resource":kind,"action":action,"status":"waiting_for_idle"}),
            );
        }
    }
}
