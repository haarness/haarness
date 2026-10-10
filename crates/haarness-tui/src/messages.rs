//! Search durable conversation messages without mutating history or drafts.
use anyhow::{Context, Result, ensure};
use haarness_protocol::{ForkThreadRequest, Item, ItemData, ItemId, ThreadId};

use crate::{
    api::ApiClient,
    state::{App, OptionEntry, OptionsKind, Overlay, OverlayChoice},
};

pub struct BrowserPosition {
    thread: ThreadId,
    query: String,
    selected: Option<String>,
}

pub(crate) fn message_text(item: &Item) -> Option<(&str, &str)> {
    match &item.data {
        ItemData::UserMessage {
            text,
            synthetic: false,
            ..
        } => Some(("You", text)),
        ItemData::AssistantMessage { text } if !text.trim().is_empty() => Some(("Assistant", text)),
        _ => None,
    }
}

impl App {
    fn message_available(&self, item: &Item) -> bool {
        item.thread_id == self.thread.id
            && self
                .thread
                .revert
                .as_ref()
                .is_none_or(|revert| item.sequence <= revert.sequence)
            && message_text(item).is_some()
    }

    fn message_entries(&self) -> Vec<OptionEntry> {
        let waiting = self
            .pending_follow_ups()
            .iter()
            .map(|item| item.id)
            .collect::<std::collections::HashSet<_>>();
        self.items
            .iter()
            .rev()
            .filter(|item| self.message_available(item) && !waiting.contains(&item.id))
            .map(|item| {
                let (role, text) = message_text(item).expect("visible message");
                OptionEntry {
                    label: format!(
                        "{role} · #{} · {}",
                        item.sequence,
                        item.created_at
                            .with_timezone(&chrono::Local)
                            .format("%b %d %H:%M")
                    ),
                    description: if text.trim().is_empty() {
                        "[Attachments]".into()
                    } else {
                        crate::prompt_context::display(text).into_owned()
                    },
                    value: format!("{}/{}", self.thread.id, item.id),
                }
            })
            .collect()
    }

    pub(crate) fn remember_message_browser(&mut self) {
        if !matches!(
            self.overlay,
            Overlay::Options {
                kind: OptionsKind::Messages,
                ..
            }
        ) {
            return;
        }
        self.message_browser = Some(BrowserPosition {
            thread: self.thread.id,
            query: self.overlay_filter.clone(),
            selected: match self.selected_overlay_choice() {
                Some(OverlayChoice::Option(OptionsKind::Messages, value)) => Some(value),
                _ => None,
            },
        });
    }

    pub(crate) fn open_messages(&mut self, query: Option<&str>) {
        let previous = self
            .message_browser
            .as_ref()
            .filter(|position| position.thread == self.thread.id);
        let filter = query
            .map(str::to_owned)
            .unwrap_or_else(|| previous.map_or_else(String::new, |p| p.query.clone()));
        let selected = if query.is_none() {
            previous.and_then(|p| p.selected.clone())
        } else {
            None
        };
        self.open_overlay(Overlay::Options {
            title: "Messages".into(),
            kind: OptionsKind::Messages,
            entries: self.message_entries(),
        });
        self.overlay_filter = filter;
        if let Overlay::Options { ref entries, .. } = self.overlay {
            self.selection = self
                .filtered_options(entries)
                .iter()
                .position(|entry| Some(&entry.value) == selected.as_ref())
                .unwrap_or(0);
        }
    }

    pub(crate) fn refresh_open_messages(&mut self) {
        if matches!(
            self.overlay,
            Overlay::Options {
                kind: OptionsKind::Messages,
                ..
            }
        ) {
            self.remember_message_browser();
            self.open_messages(None);
        }
    }

    fn browser_item(&self, key: &str) -> Result<&Item> {
        let (thread, item) = key.split_once('/').context("Invalid message selection")?;
        let thread = ThreadId(thread.parse().context("Invalid message session")?);
        let id = ItemId(item.parse().context("Invalid message ID")?);
        ensure!(
            thread == self.thread.id,
            "This message belongs to another session"
        );
        ensure!(
            !self
                .pending_follow_ups()
                .iter()
                .any(|pending| pending.id == id),
            "This message is still queued"
        );
        self.items
            .iter()
            .find(|item| item.id == id && self.message_available(item))
            .context("This message is no longer available. Reopen /messages.")
    }

    pub(crate) fn open_message_actions(&mut self, key: &str) -> Result<()> {
        let item = self.browser_item(key)?;
        let (role, _) = message_text(item).expect("visible message");
        let mut actions = vec![
            (
                "jump",
                "Jump to message",
                "View this point in the conversation",
            ),
            (
                "read",
                "Read message",
                "Open the complete text in a scrollable panel",
            ),
            (
                "copy",
                "Copy message",
                "Copy the complete text to the clipboard",
            ),
        ];
        if !self.is_child_view() {
            actions.push((
                "draft",
                "Add text to draft",
                "Append message text without sending or replacing your draft",
            ));
            actions.push((
                "fork",
                "Fork from message",
                if role == "You" {
                    "New session before this prompt"
                } else {
                    "New session through this reply"
                },
            ));
        }
        actions.push((
            "back",
            "Back to messages",
            "Keep your search and selected message",
        ));
        let title = format!("{role} message · #{}", item.sequence);
        self.open_overlay(Overlay::Options {
            title,
            kind: OptionsKind::MessageActions,
            entries: actions
                .into_iter()
                .map(|(action, label, description)| OptionEntry {
                    label: label.into(),
                    description: description.into(),
                    value: format!("{action}:{key}"),
                })
                .collect(),
        });
        Ok(())
    }

    pub(crate) async fn execute_message_action(
        &mut self,
        value: &str,
        client: &ApiClient,
    ) -> Result<()> {
        let (action, key) = value.split_once(':').context("Invalid message action")?;
        let item = self.browser_item(key)?.clone();
        let (role, text) = message_text(&item).expect("visible message");
        match action {
            "back" => self.open_messages(None),
            "read" => self.open_overlay(Overlay::Panel {
                title: format!("{role} message · #{}", item.sequence),
                lines: if text.is_empty() {
                    vec!["[Attachments]".into()]
                } else {
                    text.lines().map(str::to_owned).collect()
                },
            }),
            "jump" => {
                self.close_overlay();
                self.selection_range = None;
                self.pending_message_jump
                    .set(Some((self.thread.id, item.id)));
                self.notice = format!(
                    "Viewing {role} message · #{} · Ctrl+End returns to latest",
                    item.sequence
                );
            }
            "copy" => {
                crate::terminal::copy_to_clipboard(text);
                self.close_overlay();
                self.notice = "Message copied".into();
            }
            "draft" => {
                ensure!(
                    !self.is_child_view(),
                    "Child-agent conversations are read-only"
                );
                ensure!(
                    !self.shell_mode,
                    "Leave shell mode before adding message text to your draft"
                );
                let draft = self.composer.expanded_text();
                self.composer.set_text(if draft.is_empty() {
                    text.into()
                } else {
                    format!("{draft}\n\n{text}")
                });
                self.close_overlay();
                self.notice = "Message text added to your draft · not sent".into();
            }
            "fork" => {
                ensure!(
                    !self.is_child_view(),
                    "Child-agent conversations are read-only"
                );
                let request = if matches!(item.data, ItemData::UserMessage { .. }) {
                    ForkThreadRequest {
                        before_item_id: Some(item.id),
                        ..Default::default()
                    }
                } else {
                    ForkThreadRequest {
                        up_to_sequence: Some(item.sequence),
                        ..Default::default()
                    }
                };
                let thread = client.fork_thread_with(self.thread.id, request).await?;
                self.refresh_threads(client).await?;
                self.switch_thread(thread, client).await?;
                self.notice = "Forked from selected message".into();
            }
            _ => anyhow::bail!("Unknown message action"),
        }
        Ok(())
    }
}
