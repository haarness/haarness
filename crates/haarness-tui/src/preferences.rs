//! Client-owned preferences and durable unsent prompts.
use crate::{config, state::App};
use anyhow::{Context, Result};
use haarness_protocol::{ManagementEntry, ManagementView};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The prompts set aside with the stash.
pub(crate) const STASH_FILE: &str = "tui-stash.json";

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct StashedPrompt {
    pub id: String,
    pub text: String,
    pub attachments: Vec<String>,
    pub workspace: String,
    pub shell: bool,
}

/// Changes to `tui-state.toml` that a save could not write. They stay in
/// effect until the TUI quits. Each is one change, laid over what is saved
/// when a later save writes it, so it leaves another TUI's other changes
/// alone (on the same favorite or shortcut, the later save wins): favorites
/// and recent models go in with the next save of the model lists that works
/// or the save as the TUI leaves, shortcuts only with the next `/keybinds`
/// save that works, which checks them as it checks the form. A kept shortcut
/// whose key another TUI takes meanwhile is dropped.
#[derive(Clone, Default)]
pub(crate) struct Unsaved {
    /// Favorite toggles, oldest first, as (model, favorite).
    pub favorites: Vec<(String, bool)>,
    /// Models used, oldest first.
    pub recents: Vec<String>,
    /// The shortcuts a `/keybinds` save changed, by action.
    pub keybindings: std::collections::BTreeMap<String, String>,
}

impl Unsaved {
    /// Lay the favorites and recent models over `state`.
    pub(crate) fn apply(&self, state: &mut config::TuiState) {
        for (model, favorite) in &self.favorites {
            set_favorite(&mut state.model_favorites, model, *favorite);
        }
        for model in &self.recents {
            put_first(&mut state.model_recents, model);
        }
    }
}

impl App {
    pub(crate) fn toggle_model_favorite(&mut self) {
        let Some(model) = self.filtered_models().get(self.selection).cloned() else {
            return;
        };
        let favorite = !self.model_favorites.contains(&model);
        set_favorite(&mut self.model_favorites, &model, favorite);
        self.unsaved.favorites.retain(|(m, _)| m != &model);
        self.unsaved.favorites.push((model.clone(), favorite));
        let saved = self.save_model_lists(|_| {});
        self.selection = self
            .filtered_models()
            .iter()
            .position(|m| m == &model)
            .unwrap_or(0);
        self.notice = if saved {
            "Model favorites saved"
        } else {
            // The picker shows the change; the next launch will not.
            "Model favorites not saved · kept until you quit"
        }
        .into();
    }
    pub(crate) fn remember_model(&mut self) {
        let model = format!("{}/{}", self.thread.provider, self.thread.model);
        put_first(&mut self.model_recents, &model);
        self.unsaved.recents.retain(|m| m != &model);
        self.unsaved.recents.push(model);
        self.save_model_lists(|_| {});
    }
    /// Save the changes to the model lists no save could write yet (the one
    /// just made among them), and show the lists as saved: another TUI on
    /// the same folder may have changed them since this one read them, and
    /// saving this one's whole lists would undo that. Whether they were
    /// saved; when they were not, this TUI keeps showing them.
    fn save_model_lists(&mut self, change: impl FnOnce(&mut config::TuiState)) -> bool {
        let unsaved = self.unsaved.clone();
        let mut lists = None;
        let saved = config::save_state(|s| {
            change(s);
            unsaved.apply(s);
            lists = Some((s.model_favorites.clone(), s.model_recents.clone()));
        });
        if let Some((favorites, recents)) = lists.filter(|_| saved) {
            self.unsaved.favorites.clear();
            self.unsaved.recents.clear();
            self.model_favorites = favorites;
            self.model_recents = recents;
        }
        saved
    }
    /// Carry what another `App` of this TUI could not save (a server switch
    /// starts a new one from the saved state): it stays in effect here too.
    pub(crate) fn keep_unsaved(&mut self, mut unsaved: Unsaved) {
        for (model, favorite) in &unsaved.favorites {
            set_favorite(&mut self.model_favorites, model, *favorite);
        }
        for model in &unsaved.recents {
            put_first(&mut self.model_recents, model);
        }
        crate::keybindings::lay_over_unsaved(&mut self.keybindings, &mut unsaved.keybindings);
        self.unsaved = unsaved;
    }
    pub(crate) fn stash_view(&mut self, input: &Value) -> Result<ManagementView> {
        let action = input["action"].as_str().unwrap_or("view");
        let mut prompts: Vec<StashedPrompt> = config::load_client_json(STASH_FILE)?;
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
                config::save_client_json(STASH_FILE, &prompts)?;
                self.keep_picks(&[prompts[0].text.clone()]);
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
                    self.keep_picks(&[prompts[0].text.clone()]);
                }
                config::save_client_json(STASH_FILE, &prompts)?;
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
                config::save_client_json(STASH_FILE, &prompts)?;
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
                        crate::prompt_context::display(&prompt.text).into_owned(),
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
                            icon: None,
                        },
                        ManagementEntry {
                            label: "Delete prompt".into(),
                            description: String::new(),
                            request: json!({"action":"delete","id":id}),
                            confirm: Some("Delete this saved prompt?".into()),
                            icon: None,
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
                icon: None,
            }],
            ..ManagementView::default()
        };
        for prompt in prompts {
            view.entries.push(ManagementEntry {
                label: crate::prompt_context::display(&prompt.text)
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
                icon: None,
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

/// Add `model` to `favorites` or take it out, leaving the others as they are.
fn set_favorite(favorites: &mut Vec<String>, model: &str, favorite: bool) {
    if !favorite {
        favorites.retain(|m| m != model);
    } else if !favorites.iter().any(|m| m == model) {
        favorites.push(model.to_owned());
    }
}

/// `model` as the most recent of `recents`, which keep the last ten.
fn put_first(recents: &mut Vec<String>, model: &str) {
    recents.retain(|m| m != model);
    recents.insert(0, model.to_owned());
    recents.truncate(10);
}
