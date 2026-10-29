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
                json!({"skills":skills,"agents":agents,"plugins":plugins,"mcp":mcp,"extensions":extensions,"marketplace":self.marketplace_installed()?,"applyWhen":"idle"}),
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
                "install"
                    | "marketplace"
                    | "marketplace_update"
                    | "marketplace_auto_update"
                    | "update"
                    | "enable"
                    | "disable"
                    | "remove"
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
                "Project configuration is turned off"
            );
            crate::extensions::manage(&self.paths, &self.workspace, self.trust_project, input)?
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

    /// Installation receipts plus whether each resource is currently on, so
    /// clients offer the one toggle that applies.
    pub(crate) fn marketplace_installed(&self) -> Result<Value> {
        let mut rows =
            crate::marketplace::installed(&self.paths, &self.workspace, self.trust_project)?;
        let state =
            crate::capability_state::load(&self.paths, &self.workspace, self.trust_project)?;
        let extensions =
            crate::extensions::catalog(&self.paths, &self.workspace, self.trust_project)?;
        for row in rows.as_array_mut().into_iter().flatten() {
            let name = row["name"].as_str().unwrap_or_default().to_owned();
            let enabled = match row["kind"].as_str() {
                Some("plugin" | "mcp") => !Path::new(row["path"].as_str().unwrap_or_default())
                    .join(".haarness-disabled")
                    .exists(),
                Some(kind @ ("skill" | "agent")) => state
                    .get(kind)
                    .and_then(|items| items.get(&name))
                    .copied()
                    .unwrap_or(true),
                Some("extension") => extensions
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|entry| entry["source"] == row["path"])
                    .and_then(|entry| entry["enabled"].as_bool())
                    .unwrap_or(true),
                _ => true,
            };
            row["enabled"] = json!(enabled);
        }
        Ok(rows)
    }

    pub(crate) async fn marketplace_request(&self, input: &Value) -> Result<Value> {
        let action = required_string(input, "action")?;
        if action == "installed" {
            return self.marketplace_installed();
        }
        if action == "plugin_marketplaces" {
            let data = self.paths.data.clone();
            return Ok(
                tokio::task::spawn_blocking(move || plugins::marketplace_catalog(&data)).await?,
            );
        }
        if action == "update_plugin_marketplace" {
            let data = self.paths.data.clone();
            let name = input["name"]
                .as_str()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned);
            let refreshed = tokio::task::spawn_blocking(move || match name {
                Some(name) => plugins::marketplace_update(&data, &name).map(|one| vec![one]),
                None => Ok(plugins::marketplace_update_all(&data, false)),
            })
            .await??;
            if refreshed.iter().any(plugins::MarketplaceRefresh::changed) {
                self.capabilities_changed("plugin", "marketplace_update");
            }
            return Ok(json!({"marketplaces":refreshed,"restartRequired":false}));
        }
        if action == "plugin_marketplace_auto_update" {
            let data = self.paths.data.clone();
            let name = required_string(input, "name")?.to_owned();
            let enabled = input["enabled"]
                .as_bool()
                .context("Say whether it updates automatically: enabled true or false")?;
            let source = tokio::task::spawn_blocking(move || {
                plugins::marketplace_auto_update(&data, &name, enabled)
            })
            .await??;
            return Ok(json!({
                "name": input["name"],
                "source": haarness_core::redact::url(&source.source),
                "autoUpdate": source.auto_update,
                "commit": source.commit,
                "updatedAt": source.updated_at,
            }));
        }
        // Claude Code plugin marketplaces use the same plugin service as
        // `/plugin` in the TUI and Desktop's Plugins page.
        let plugin = match action {
            "add_plugin_marketplace" => Some(("marketplace", required_string(input, "source")?)),
            "install" | "update" | "enable" | "disable" | "remove" => input
                .get("plugin")
                .and_then(Value::as_str)
                .filter(|plugin| !plugin.trim().is_empty())
                .map(|plugin| (action, plugin)),
            _ => None,
        };
        if let Some((action, target)) = plugin {
            let service = plugins::PluginService::new(
                self.paths.data.clone(),
                self.paths.config_dir().into(),
                self.workspace.clone(),
                self.trust_project,
            );
            let request = haarness_protocol::PluginActionRequest {
                action: action.into(),
                target: target.into(),
            };
            let plugins = tokio::task::spawn_blocking(move || {
                haarness_server::PluginRegistry::apply(&service, &request)
            })
            .await??;
            self.capabilities_changed("plugin", action);
            return Ok(
                json!({"action":action,"plugin":target,"plugins":plugins,"restartRequired":false}),
            );
        }
        if matches!(
            action,
            "search" | "detail" | "versions" | "collections" | "manifest"
        ) {
            return crate::marketplace::fetch(input).await;
        }
        if action == "check_updates" {
            ensure!(
                input["scope"].is_null()
                    || matches!(input["scope"].as_str(), Some("local" | "global")),
                "Scope must be local or global"
            );
            let (summary, changed) = crate::marketplace::check_updates(
                &self.paths,
                &self.workspace,
                self.trust_project,
                input["scope"].as_str(),
            )
            .await?;
            if changed {
                self.capabilities_changed("marketplace", "update");
            }
            return Ok(summary);
        }
        if matches!(action, "install" | "update") {
            let manifest = if action == "install" && input.get("manifest").is_some() {
                input["manifest"].clone()
            } else {
                crate::marketplace::fetch(input).await?
            };
            let mut input = input.clone();
            // A version asked for by number that is not the one the catalog
            // offers now (a rollback) stays put: following new versions
            // would undo it at the next round.
            if crate::marketplace::names_its_version(&input) {
                let id = manifest["resource"]["id"].as_str().unwrap_or_default();
                let active = crate::marketplace::active_version_number(id)
                    .await
                    .ok()
                    .flatten();
                if crate::marketplace::pins(&manifest, active) {
                    input["autoUpdate"] = json!(false);
                }
            }
            let result = crate::marketplace::install(
                &self.paths,
                &self.workspace,
                self.trust_project,
                &manifest,
                &input,
            )
            .await?;
            self.capabilities_changed("marketplace", action);
            return Ok(result);
        }
        let wanted = required_string(input, "id")?;
        let rows = crate::marketplace::installed(&self.paths, &self.workspace, self.trust_project)?;
        let in_scope = |r: &&Value| input.get("scope").is_none() || r["scope"] == input["scope"];
        // `publisher/slug` selects like the resource id; receipts written
        // before they recorded both are found through the catalog.
        let named = |r: &&Value| {
            r["id"] == wanted
                || wanted.split_once('/').is_some_and(|(publisher, slug)| {
                    r["publisher"] == publisher && r["slug"] == slug
                })
        };
        let mut rows: Vec<_> = rows
            .as_array()
            .into_iter()
            .flatten()
            .filter(in_scope)
            .collect();
        if rows.iter().any(named) {
            rows.retain(named);
        } else if wanted.contains('/') && uuid::Uuid::parse_str(wanted).is_err() {
            let id = crate::marketplace::resolve(input).await?;
            rows.retain(|r| r["id"] == id);
        } else {
            rows.clear();
        }
        ensure!(
            !rows.is_empty(),
            "{wanted} is not installed{}",
            match input["scope"].as_str() {
                Some("local") => " for this project",
                Some("global") => " for all projects",
                _ => "",
            }
        );
        ensure!(
            rows.len() == 1,
            "{wanted} is installed for this project and for all projects; choose one with its scope"
        );
        let receipt = rows[0];
        let id = receipt["id"].as_str().context("Invalid receipt id")?;
        if action == "auto_update" {
            let enabled = input["enabled"]
                .as_bool()
                .context("Say whether it updates automatically: enabled true or false")?;
            // Only the receipt changes; nothing the runtime loads does.
            return crate::marketplace::set_auto_update(
                &self.paths,
                &self.workspace,
                receipt,
                enabled,
            );
        }
        ensure!(
            matches!(action, "enable" | "disable" | "remove"),
            "Unsupported marketplace action"
        );
        // Held until the receipt is gone, so an update that started meanwhile
        // finds it removed instead of installing the resource again.
        let _lock = crate::desktop::editor_lock(&self.paths.data, "marketplace")?;
        let (receipt_file, receipt) =
            crate::marketplace::reread_receipt(&self.paths, &self.workspace, receipt)?;
        let receipt = &receipt;
        let kind = receipt["kind"].as_str().context("Invalid receipt kind")?;
        let path = Path::new(receipt["path"].as_str().context("Invalid receipt path")?);
        let target = if let Some(plugin) = receipt["pluginName"].as_str() {
            json!({"kind":"plugin","name":plugin})
        } else {
            json!({"kind":kind,"name":receipt["name"]})
        };
        if action == "remove" && kind != "extension" {
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
            if action == "remove"
                && kind == "extension"
                && let Some(root) = receipt["packageRoot"]
                    .as_str()
                    .map(Path::new)
                    .filter(|root| root.exists())
            {
                ensure!(
                    receipt["installedDigest"] == crate::marketplace::tree_digest(root)?,
                    "Resource was modified locally; preserve your edits before removing it"
                );
            }
            let mut request = target;
            request["action"] = json!(action);
            request["scope"] = receipt["scope"].clone();
            self.capability_request(&request).await?;
            if action == "remove" && kind == "extension" {
                crate::extensions::remove_marketplace_package(&self.paths, receipt)?;
            }
        }
        if action == "remove" {
            fs::remove_file(&receipt_file)?;
        }
        self.capabilities_changed(kind, action);
        Ok(json!({"id":id,"action":action,"restartRequired":false}))
    }

    /// One background round (see `auto_update`): what is installed for all
    /// projects and the Git plugin marketplaces once per interval on this
    /// computer, then this project's own installs once per interval. Nothing
    /// here fails the server; a round that cannot reach the network says so
    /// at info level and waits for the next one.
    pub(crate) async fn automatic_update_round(&self, every: std::time::Duration) {
        use crate::auto_update::{STAMP, claim};
        let data = self.paths.data.clone();
        let claimed = |stamp: PathBuf| {
            let data = data.clone();
            async move {
                tokio::task::spawn_blocking(move || {
                    claim(&data, &stamp, every, std::time::SystemTime::now())
                })
                .await
                .map_err(anyhow::Error::from)
                .and_then(|claimed| claimed)
                .unwrap_or_else(|error| {
                    tracing::warn!(error = %format!("{error:#}"), "automatic update round skipped");
                    false
                })
            }
        };
        let mut changed = false;
        let check = async |scope: &str| match crate::marketplace::check_updates(
            &self.paths,
            &self.workspace,
            self.trust_project,
            Some(scope),
        )
        .await
        {
            Ok((summary, updated)) => {
                tracing::debug!(%summary, scope, "automatic update round");
                updated
            }
            Err(error) => {
                tracing::info!(error = %format!("{error:#}"), scope, "automatic update round failed");
                false
            }
        };
        if claimed(data.join(STAMP)).await {
            changed |= check("global").await;
            let refreshed = tokio::task::spawn_blocking({
                let data = data.clone();
                move || plugins::marketplace_update_all(&data, true)
            })
            .await
            .unwrap_or_default();
            for refresh in &refreshed {
                if let Some(error) = &refresh.error {
                    tracing::info!(marketplace = %refresh.name, %error, "plugin marketplace not refreshed");
                } else if refresh.changed() {
                    tracing::info!(marketplace = %refresh.name, plugins = ?refresh.plugins, "plugin marketplace refreshed");
                }
                changed |= refresh.changed();
            }
        }
        // A project's round runs only where it has installs that follow new
        // versions; its stamp lives in the data folder, never the project.
        let follows = self.trust_project
            && crate::marketplace::installed(&self.paths, &self.workspace, true).is_ok_and(
                |rows| {
                    rows.as_array().into_iter().flatten().any(|row| {
                        row["scope"] == "local" && crate::marketplace::follows_new_versions(row)
                    })
                },
            );
        if follows && claimed(crate::auto_update::project_stamp(&data, &self.workspace)).await {
            changed |= check("local").await;
        }
        if changed {
            self.capabilities_changed("marketplace", "update");
        }
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
