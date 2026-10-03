use crate::{
    api::ApiClient,
    state::{App, OptionEntry, OptionsKind, Overlay},
};
use anyhow::{Result, bail};
use haarness_protocol::{InputDelivery, InputQueueAction, QueueInputRequest, RequestId};

impl App {
    pub async fn defer_prompt(&mut self, text: String, client: &ApiClient) -> Result<bool> {
        if text.trim_start().starts_with('/') || self.shell_mode {
            bail!("Commands cannot be queued. Use a normal message or Steer.");
        }
        if !client
            .info()
            .await?
            .capabilities
            .iter()
            .any(|c| c == haarness_protocol::INPUT_QUEUE_CAPABILITY)
        {
            bail!("This server does not support deferred messages. Update it before using Queue.");
        }
        let response = client
            .queue_input(
                self.thread.id,
                QueueInputRequest {
                    preview: false,
                    delivery: InputDelivery::Queue,
                    request_id: RequestId::new(),
                    text,
                    attachments: self.attachments.clone(),
                    synthetic: false,
                },
            )
            .await?;
        self.attachments.clear();
        if let Some(turn) = response.turn {
            self.active_turn = Some(turn.id);
        }
        self.input_queue = client.input_queue(self.thread.id).await?;
        self.refresh_needed = true;
        self.notice = "Queued after current work · /queue to manage".into();
        Ok(true)
    }
    pub async fn open_input_queue(&mut self, client: &ApiClient) -> Result<()> {
        self.input_queue = client.input_queue(self.thread.id).await?;
        self.show_input_queue();
        Ok(())
    }
    fn show_input_queue(&mut self) {
        if self.input_queue.items.is_empty() {
            self.close_overlay();
            self.notice = "No queued messages · Alt+Enter queues a prompt".into();
            return;
        }
        self.open_overlay(Overlay::Options {
            title: "Queued messages · after current work".into(),
            kind: OptionsKind::InputQueue,
            entries: self
                .input_queue
                .items
                .iter()
                .enumerate()
                .map(|(i, item)| OptionEntry {
                    label: format!("{}. {}", i + 1, item.text),
                    description: format!(
                        "{} attachment(s) · Enter for actions",
                        item.attachments.len()
                    ),
                    value: item.id.to_string(),
                })
                .collect(),
        });
    }
    pub fn input_queue_actions(&mut self, value: &str) -> Result<()> {
        let id = RequestId(value.parse()?);
        let item = self
            .input_queue
            .items
            .iter()
            .find(|item| item.id == id)
            .ok_or_else(|| anyhow::anyhow!("Message is no longer pending"))?;
        self.input_queue_edit = Some((id, self.input_queue.revision));
        let title = format!("Queued: {}", item.text);
        self.open_overlay(Overlay::Options {
            title,
            kind: OptionsKind::InputQueueActions,
            entries: [
                ("edit", "Edit message"),
                (
                    "steer",
                    if self.active_turn.is_some() {
                        "Steer now"
                    } else {
                        "Send now"
                    },
                ),
                ("up", "Move up"),
                ("down", "Move down"),
                ("remove", "Remove message"),
            ]
            .iter()
            .map(|(value, label)| OptionEntry {
                label: (*label).into(),
                description: String::new(),
                value: (*value).into(),
            })
            .collect(),
        });
        Ok(())
    }
    pub async fn input_queue_action(&mut self, value: &str, client: &ApiClient) -> Result<()> {
        let (id, revision) = self
            .input_queue_edit
            .ok_or_else(|| anyhow::anyhow!("Choose a pending message first"))?;
        if value == "edit" {
            let text = self
                .input_queue
                .items
                .iter()
                .find(|item| item.id == id)
                .ok_or_else(|| anyhow::anyhow!("Message is no longer pending"))?
                .text
                .clone();
            self.open_overlay(Overlay::Options {
                title: "Edit queued message".into(),
                kind: OptionsKind::InputQueueEdit,
                entries: vec![OptionEntry {
                    label: "Save changes".into(),
                    description: "Enter saves · Esc cancels · composer draft is preserved".into(),
                    value: "save".into(),
                }],
            });
            self.overlay_filter = text;
            return Ok(());
        }
        let action = match value {
            "steer" => InputQueueAction::Steer { revision, id },
            "remove" => InputQueueAction::Remove { revision, id },
            "up" | "down" => {
                let mut ids: Vec<_> = self.input_queue.items.iter().map(|item| item.id).collect();
                let index = ids
                    .iter()
                    .position(|key| *key == id)
                    .ok_or_else(|| anyhow::anyhow!("Message is no longer pending"))?;
                let target = if value == "up" {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(ids.len() - 1)
                };
                ids.swap(index, target);
                InputQueueAction::Reorder { revision, ids }
            }
            _ => bail!("Unknown queue action"),
        };
        self.apply_input_queue_action(action, client).await?;
        self.show_input_queue();
        Ok(())
    }
    pub async fn save_input_queue_edit(&mut self, text: String, client: &ApiClient) -> Result<()> {
        let (id, revision) = self
            .input_queue_edit
            .ok_or_else(|| anyhow::anyhow!("Choose a pending message first"))?;
        if let Err(error) = self
            .apply_input_queue_action(
                InputQueueAction::Edit {
                    revision,
                    id,
                    text: text.clone(),
                },
                client,
            )
            .await
        {
            // Restore the exact edit after conflict/network failure; no draft loss.
            self.open_overlay(Overlay::Options {
                title: "Edit queued message · save failed".into(),
                kind: OptionsKind::InputQueueEdit,
                entries: vec![OptionEntry {
                    label: "Save changes".into(),
                    description: error.to_string(),
                    value: "save".into(),
                }],
            });
            self.overlay_filter = text;
            return Ok(());
        }
        self.show_input_queue();
        Ok(())
    }
    async fn apply_input_queue_action(
        &mut self,
        action: InputQueueAction,
        client: &ApiClient,
    ) -> Result<()> {
        match client.change_input_queue(self.thread.id, action).await {
            Ok(queue) => {
                self.input_queue = queue;
                self.refresh_needed = true;
                Ok(())
            }
            Err(error) => {
                if let Ok(queue) = client.input_queue(self.thread.id).await {
                    self.input_queue = queue;
                }
                Err(error)
            }
        }
    }
}
