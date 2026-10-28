use crate::{
    api::ApiClient,
    state::{App, OptionEntry, OptionsKind, Overlay, QueueEdit},
};
use anyhow::{Result, bail};
use haarness_protocol::{InputDelivery, InputQueueAction, QueueInputRequest, RequestId};

/// Why a save was refused, in the words the editing line shows.
const ALREADY_GONE: &str = "Already sent or removed · Esc or click restores your draft";
const CHANGED_ELSEWHERE: &str = "Changed elsewhere · Esc or click restores your draft";
const QUEUE_MOVED: &str = "Queue changed elsewhere · Enter saves again · Esc or click cancels";

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
        if self.still_sending() {
            return Ok(false);
        }
        let draft = text.clone();
        let Some((text, resources)) = self.take_mentions(&text)? else {
            return Ok(false);
        };
        let request = QueueInputRequest {
            preview: Some(false),
            reasoning_effort: self.requested_effort(),
            delivery: InputDelivery::Queue,
            request_id: RequestId::new(),
            text,
            attachments: self.attachments.clone(),
            resources,
            synthetic: false,
            ..Default::default()
        };
        if !request.resources.is_empty() {
            let mentions = request.resources.clone();
            let (client, chat) = (client.clone(), self.thread.id);
            self.send_with_mentions(draft, &mentions, async move {
                client
                    .queue_input(chat, request)
                    .await
                    .map(crate::resource_mentions::Outcome::Queued)
            })
            .await;
            return Ok(true);
        }
        let response = client.queue_input(self.thread.id, request).await?;
        self.attachments.clear();
        if let Some(turn) = response.turn {
            self.active_turn = Some(turn.id);
        }
        self.input_queue = client.input_queue(self.thread.id).await?;
        self.refresh_needed = true;
        self.notice = "Queued after current work · click it to edit, send or remove".into();
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
        let editing = self.queue_edit.as_ref().map(|edit| edit.id);
        self.open_overlay(Overlay::Options {
            title: if self.active_turn.is_some() {
                "Queued messages · after current work".into()
            } else {
                "Queued messages · on hold".into()
            },
            kind: OptionsKind::InputQueue,
            entries: self
                .input_queue
                .items
                .iter()
                .enumerate()
                .map(|(i, item)| OptionEntry {
                    label: format!("{}. {}", i + 1, queued_label(&item.text)),
                    description: if editing == Some(item.id) {
                        "editing in the prompt".into()
                    } else {
                        match item.attachments.len() {
                            0 => "Enter for actions".into(),
                            1 => "1 attachment · Enter for actions".into(),
                            count => format!("{count} attachments · Enter for actions"),
                        }
                    },
                    value: item.id.to_string(),
                })
                .collect(),
        });
    }
    /// A row chosen in the `/queue` list.
    pub fn input_queue_actions(&mut self, value: &str) -> Result<()> {
        self.open_queue_item_actions(RequestId(value.parse()?), true)
    }
    /// What can be done to one queued message: the same menu whether it was
    /// reached from its row in the strip or from the `/queue` list.
    pub fn open_queue_item_actions(&mut self, id: RequestId, listed: bool) -> Result<()> {
        let items = &self.input_queue.items;
        let index = items
            .iter()
            .position(|item| item.id == id)
            .ok_or_else(|| anyhow::anyhow!("Message is no longer pending"))?;
        let title = format!("Queued: {}", queued_label(&items[index].text));
        let mut entries = vec![
            if self.active_turn.is_some() {
                (
                    "steer",
                    "Steer now",
                    "hand it to the current work at its next step",
                )
            } else {
                (
                    "steer",
                    "Send now",
                    "run it now; the rest of the queue follows",
                )
            },
            (
                "edit",
                "Edit",
                "load it into the prompt; Enter saves your changes",
            ),
            ("remove", "Remove", "discard it; the agent never sees it"),
        ];
        // Moves that cannot happen are not offered at all.
        if index > 0 {
            entries.push(("up", "Move up", ""));
        }
        if index + 1 < items.len() {
            entries.push(("down", "Move down", ""));
        }
        self.input_queue_edit = Some((id, self.input_queue.revision));
        self.input_queue_listed = listed;
        self.open_overlay(Overlay::Options {
            title,
            kind: OptionsKind::InputQueueActions,
            entries: entries
                .into_iter()
                .map(|(value, label, description)| OptionEntry {
                    label: label.into(),
                    description: description.into(),
                    value: value.into(),
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
            return self.edit_queued_message(id);
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
        let leaves_queue = matches!(
            action,
            InputQueueAction::Steer { .. } | InputQueueAction::Remove { .. }
        );
        if let Err(error) = self.apply_input_queue_action(action, client).await {
            if is_conflict(&error) {
                if self.input_queue.items.iter().any(|item| item.id == id) {
                    bail!("The queue changed elsewhere · try again");
                }
                bail!("This message was already sent or removed");
            }
            return Err(error);
        }
        // A message that was just sent or discarded can no longer be saved,
        // so an edit of it ends here and the set-aside draft comes back.
        if leaves_queue && self.queue_edit.as_ref().is_some_and(|edit| edit.id == id) {
            self.cancel_queue_edit();
        }
        if self.input_queue_listed {
            self.show_input_queue();
        } else {
            self.close_overlay();
        }
        Ok(())
    }
    /// Load a queued message into the prompt. The draft that was there is set
    /// aside; switching from one queued message to another keeps the
    /// original draft, since that is the only one worth restoring.
    pub(crate) fn edit_queued_message(&mut self, id: RequestId) -> Result<()> {
        let item = self
            .input_queue
            .items
            .iter()
            .find(|item| item.id == id)
            .ok_or_else(|| anyhow::anyhow!("Message is no longer pending"))?;
        let (text, attachments) = (item.text.clone(), item.attachments.clone());
        let original = (text.clone(), attachments.clone());
        let edit = match self.queue_edit.take() {
            Some(previous) => QueueEdit {
                id,
                original,
                conflict: None,
                ..previous
            },
            None => QueueEdit {
                id,
                original,
                draft: self.composer.set_aside(),
                attachments: std::mem::take(&mut self.attachments),
                shell: std::mem::take(&mut self.shell_mode),
                conflict: None,
            },
        };
        self.queue_edit = Some(edit);
        self.close_overlay();
        self.composer.clear();
        self.composer.set_text(text);
        self.attachments = attachments;
        self.shell_mode = false;
        self.mention_suggestions.clear();
        self.autocomplete_hidden = true;
        self.selection = 0;
        // The strip says how to save and how to back out; the band does not
        // say it a third time.
        self.notice.clear();
        Ok(())
    }
    /// End an edit without saving and put the set-aside draft back. Returns
    /// whether there was an edit to end.
    pub fn cancel_queue_edit(&mut self) -> bool {
        let Some(edit) = self.queue_edit.take() else {
            return false;
        };
        self.composer.restore(edit.draft);
        self.attachments = edit.attachments;
        self.shell_mode = edit.shell;
        self.mention_suggestions.clear();
        self.autocomplete_hidden = false;
        self.selection = 0;
        true
    }
    /// Enter while editing: replace the queued message in place, keeping its
    /// position. Saving never sends or resumes anything, so a paused queue
    /// stays paused. A refusal leaves the edited text in the prompt.
    pub async fn save_queue_edit(&mut self, client: &ApiClient) -> Result<()> {
        let Some(id) = self.queue_edit.as_ref().map(|edit| edit.id) else {
            return Ok(());
        };
        // The edit saves text, so a pick in it is its `@name`.
        let text = crate::resource_mentions::plain(&self.composer.expanded_text());
        if text.trim_start().starts_with('/') {
            self.notice = "Commands cannot be queued. Use a normal message or Steer.".into();
            return Ok(());
        }
        if text.trim().is_empty() && self.attachments.is_empty() {
            self.notice =
                "A queued message needs text or an attachment · Remove discards it".into();
            return Ok(());
        }
        // Other messages may run or move meanwhile; only a change to this one
        // stops the save, so it never overwrites another client's version.
        let original = self.queue_edit.as_ref().map(|edit| edit.original.clone());
        let refusal = match self.input_queue.items.iter().find(|item| item.id == id) {
            None => Some(ALREADY_GONE),
            Some(item) if Some((item.text.clone(), item.attachments.clone())) != original => {
                Some(CHANGED_ELSEWHERE)
            }
            Some(_) => None,
        };
        if let Some(reason) = refusal {
            if let Some(edit) = &mut self.queue_edit {
                edit.conflict = Some(reason.into());
            }
            self.notice = reason.into();
            return Ok(());
        }
        // The latest revision this client has seen: an in-place edit by id
        // should not fail merely because another message moved meanwhile.
        let action = InputQueueAction::Edit {
            revision: self.input_queue.revision,
            id,
            text,
            attachments: Some(self.attachments.clone()),
        };
        match self.apply_input_queue_action(action, client).await {
            Ok(()) => {
                // The message was recorded in prompt history when it was
                // first queued; the prompt simply goes back to the draft.
                self.cancel_queue_edit();
                self.notice = "Queued message saved".into();
                Ok(())
            }
            Err(error) if is_conflict(&error) => {
                let pending = self.input_queue.items.iter().any(|item| item.id == id);
                let reason = if pending { QUEUE_MOVED } else { ALREADY_GONE };
                if let Some(edit) = &mut self.queue_edit {
                    edit.conflict = Some(reason.into());
                }
                self.notice = reason.into();
                Ok(())
            }
            Err(error) => Err(error),
        }
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

/// A queued message as one menu line; an attachment-only message still
/// needs a name.
pub(crate) fn queued_label(text: &str) -> String {
    let text = crate::prompt_context::display(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        "Attachments".into()
    } else {
        text
    }
}

/// The server refuses a queue change with 409 when the message was already
/// sent or removed, or the queue moved on since this client last saw it.
fn is_conflict(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .and_then(reqwest::Error::status)
        == Some(reqwest::StatusCode::CONFLICT)
}
