//! Client-owned preferences and durable unsent prompts.
use crate::{config, state::App};
use anyhow::{Context, Result};
use haarness_protocol::{ManagementEntry, ManagementView};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct StashedPrompt {
    pub id: String,
    pub text: String,
    pub attachments: Vec<String>,
    pub workspace: String,
    pub shell: bool,
}
impl App {
    pub(crate) fn toggle_model_favorite(&mut self) {
        let Some(model) = self.filtered_models().get(self.selection).cloned() else {
            return;
        };
        if self.model_favorites.contains(&model) {
            self.model_favorites.retain(|m| m != &model);
        } else {
            self.model_favorites.push(model.clone());
        }
        let favorites = self.model_favorites.clone();
        config::save_state(|s| s.model_favorites = favorites);
        self.selection = self
            .filtered_models()
            .iter()
            .position(|m| m == &model)
            .unwrap_or(0);
        self.notice = "Model favorites saved".into();
    }
    pub(crate) fn remember_model(&mut self) {
        let model = format!("{}/{}", self.thread.provider, self.thread.model);
        self.model_recents.retain(|m| m != &model);
        self.model_recents.insert(0, model);
        self.model_recents.truncate(10);
        let recents = self.model_recents.clone();
        config::save_state(|s| s.model_recents = recents);
    }
    pub(crate) fn stash_view(&mut self, input: &Value) -> Result<ManagementView> {
        let action = input["action"].as_str().unwrap_or("view");
        let mut prompts: Vec<StashedPrompt> = config::load_client_json("tui-stash.json")?;
        match action {
            "save" => {
                anyhow::ensure!(
                    !self.composer.is_empty() || !self.attachments.is_empty(),
                    "The composer is empty"
                );
                anyhow::ensure!(
                    prompts.len() < 50,
                    "Stash is full; restore or delete a saved prompt first"
                );
                prompts.insert(0, self.current_stash());
                config::save_client_json("tui-stash.json", &prompts)?;
                self.composer.clear();
                self.attachments.clear();
                self.shell_mode = false;
            }
            "restore" => {
                let id = input["id"].as_str().context("Missing stash ID")?;
                let index = prompts
                    .iter()
                    .position(|p| p.id == id)
                    .context("Prompt no longer exists")?;
                let restored = prompts.remove(index);
                // Swapping preserves the draft already in the composer.
                if !self.composer.is_empty() || !self.attachments.is_empty() {
                    prompts.insert(0, self.current_stash());
                }
                config::save_client_json("tui-stash.json", &prompts)?;
                self.composer.set_text(restored.text);
                self.attachments = restored.attachments;
                self.shell_mode = restored.shell;
                self.notice = if restored.workspace == self.stash_workspace() {
                    "Prompt restored".into()
                } else {
                    "Prompt restored from another workspace; review its file paths before sending"
                        .into()
                };
            }
            "delete" => {
                let id = input["id"].as_str().context("Missing stash ID")?;
                prompts.retain(|p| p.id != id);
                config::save_client_json("tui-stash.json", &prompts)?;
            }
            "inspect" => {
                let id = input["id"].as_str().context("Missing stash ID")?;
                let prompt = prompts
                    .iter()
                    .find(|p| p.id == id)
                    .context("Prompt no longer exists")?;
                return Ok(ManagementView {
                    title: "Saved prompt".into(),
                    lines: vec![
                        prompt.text.clone(),
                        format!(
                            "{} attachments · {}",
                            prompt.attachments.len(),
                            prompt.workspace
                        ),
                    ],
                    entries: vec![
                        ManagementEntry {
                            label: "Restore prompt".into(),
                            description: "Preserve the current draft in the stash".into(),
                            request: json!({"action":"restore","id":id}),
                            confirm: None,
                        },
                        ManagementEntry {
                            label: "Delete prompt".into(),
                            description: String::new(),
                            request: json!({"action":"delete","id":id}),
                            confirm: Some("Delete this saved prompt?".into()),
                        },
                    ],
                    ..ManagementView::default()
                });
            }
            _ => {}
        }
        let mut view = ManagementView {
            title: "Prompt stash".into(),
            entries: vec![ManagementEntry {
                label: "Save current draft".into(),
                description: "Preserves text, pasted content and attachments".into(),
                request: json!({"action":"save"}),
                confirm: None,
            }],
            ..ManagementView::default()
        };
        for prompt in prompts {
            view.entries.push(ManagementEntry {
                label: prompt
                    .text
                    .lines()
                    .next()
                    .filter(|v| !v.is_empty())
                    .unwrap_or("Attachments")
                    .chars()
                    .take(100)
                    .collect(),
                description: prompt.workspace,
                request: json!({"action":"inspect","id":prompt.id}),
                confirm: None,
            });
        }
        Ok(view)
    }
    fn stash_workspace(&self) -> String {
        self.server_info
            .as_ref()
            .and_then(|i| i.workspace.clone())
            .unwrap_or_default()
    }
    fn current_stash(&self) -> StashedPrompt {
        StashedPrompt {
            id: uuid::Uuid::now_v7().to_string(),
            text: self.composer.expanded_text(),
            attachments: self.attachments.clone(),
            workspace: self.stash_workspace(),
            shell: self.shell_mode,
        }
    }
}
