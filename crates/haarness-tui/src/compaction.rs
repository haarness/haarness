//! `/compact` off the event loop. A model summary of a long chat can take
//! minutes; awaiting it in the key handler froze the TUI, and the client's
//! 15 s deadline then said "This took too long" for a summary the server
//! goes on to finish. Each request now runs on a worker task while the band
//! says `compacting`, and the chat stays usable.
//!
//! A message sent meanwhile would start a turn, and the server then throws
//! the summary away (the chat changed under it). So it waits above the
//! prompt, queued, and goes out once the compaction is over, whichever way.

use std::time::{Duration, Instant};

use haarness_protocol::{CompactRequest, Item, ItemData, ThreadId};

use crate::api::ApiClient;
use crate::state::{App, ToastVariant};

/// One `/compact` this TUI asked for.
pub(crate) struct Compaction {
    thread: ThreadId,
    started: Instant,
    /// The request, until it answers.
    answer: Option<tokio::sync::oneshot::Receiver<anyhow::Result<Item>>>,
    /// What a request that got no answer from the server said. The server
    /// finishes a summary whose request went, so this waits for the
    /// compaction to arrive, and says it only if it never does.
    unanswered: Option<String>,
}

/// A message sent while its chat was being compacted, waiting to go out.
pub(crate) struct Held {
    thread: ThreadId,
    text: String,
    attachments: Vec<String>,
    /// Sent with Alt+Enter, which queues it.
    queue: bool,
}

/// The same words a turn's own compaction shows in the band.
pub(crate) fn compacting_text(seconds: impl std::fmt::Display) -> String {
    format!("compacting   {seconds}s")
}

impl App {
    /// Ask the server for a compaction of the open chat, with the person's
    /// own summary when they gave one, and return at once.
    pub(crate) fn start_compaction(&mut self, summary: Option<String>, client: &ApiClient) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let client = client.clone();
        let thread = self.thread.id;
        tokio::spawn(async move {
            let request = CompactRequest {
                summary,
                files_touched: Vec::new(),
                outstanding: Vec::new(),
                todo_checkpoint: None,
                tail_start_sequence: None,
                kept_sequences: Vec::new(),
                tool_output_chars: None,
            };
            let _ = sender.send(client.compact(thread, request).await);
        });
        self.compactions.push(Compaction {
            thread,
            started: Instant::now(),
            answer: Some(receiver),
            unanswered: None,
        });
    }

    /// When the open chat's oldest running compaction started, if one runs.
    pub(crate) fn compacting_since(&self) -> Option<Instant> {
        self.compactions
            .iter()
            .filter(|compaction| compaction.thread == self.thread.id)
            .map(|compaction| compaction.started)
            .min()
    }

    /// Whether a message sent now would make the server drop the open
    /// chat's summary: it is being compacted and nothing else runs there.
    pub(crate) fn holds_for_compaction(&self) -> bool {
        self.active_turn.is_none() && self.compacting_since().is_some()
    }

    /// Take the prompt's message (and its attachments) to send once the open
    /// chat's compaction is over. It is in the prompt history from now on,
    /// as a sent one is.
    pub(crate) fn hold_for_compaction(&mut self, queue: bool) {
        let text = self.composer.take();
        let attachments = std::mem::take(&mut self.attachments);
        self.save_prompt_history();
        self.held.push(Held {
            thread: self.thread.id,
            text,
            attachments,
            queue,
        });
    }

    /// The open chat's messages waiting for its compaction, oldest first.
    pub(crate) fn held_messages(&self) -> Vec<&str> {
        self.held
            .iter()
            .filter(|held| held.thread == self.thread.id)
            .map(|held| held.text.as_str())
            .collect()
    }

    /// Send the open chat's waiting messages, in order, once nothing is
    /// compacting it: each as Enter would send it now.
    pub(crate) async fn release_held(&mut self, client: &ApiClient) {
        while !self.mention_resources.sending && self.compacting_since().is_none() {
            let Some(index) = self
                .held
                .iter()
                .position(|held| held.thread == self.thread.id)
            else {
                return;
            };
            let held = self.held.remove(index);
            if !self.send_held(held, client).await {
                return;
            }
        }
    }

    /// Send `held` as Enter does: queued, steering a turn that runs, or as a
    /// turn of its own. What does not go out comes back to the prompt, ahead
    /// of whatever is typed there now, so nothing is lost.
    async fn send_held(&mut self, held: Held, client: &ApiClient) -> bool {
        let own = std::mem::replace(&mut self.attachments, held.attachments.clone());
        let delivery = if held.queue {
            Ok(haarness_protocol::InputDelivery::Queue)
        } else {
            client.input_delivery().await
        };
        let sent = match delivery {
            Ok(delivery) => {
                self.input_delivery = Some(delivery);
                if delivery == haarness_protocol::InputDelivery::Queue {
                    self.defer_prompt(held.text.clone(), client).await
                } else if self.active_turn.is_some() {
                    self.queue_prompt(held.text.clone(), client).await
                } else {
                    self.submit_prompt(held.text.clone(), client).await
                }
            }
            Err(error) => Err(error),
        };
        if matches!(sent, Ok(true)) {
            self.attachments = own;
            return true;
        }
        let draft = self.composer.expanded_text();
        if draft.trim().is_empty() {
            self.composer.set_text(held.text);
        } else {
            self.composer.set_text(format!("{}\n\n{draft}", held.text));
        }
        self.attachments = held.attachments;
        self.attachments.extend(own);
        if let Err(error) = sent {
            tracing::warn!(
                error = format!("{error:#}"),
                "a held message could not be sent"
            );
            self.show_compaction_error(format!("{error:#}"));
        }
        false
    }

    /// Take delivery of what the compaction requests answered.
    pub(crate) fn poll_compactions(&mut self) {
        let now = Instant::now();
        let mut index = 0;
        while index < self.compactions.len() {
            let compaction = &mut self.compactions[index];
            let answered = compaction
                .answer
                .as_mut()
                .map(tokio::sync::oneshot::Receiver::try_recv);
            let overdue = now.duration_since(compaction.started) >= crate::api::COMPACT_TIMEOUT;
            match answered {
                Some(Ok(Ok(item))) => {
                    let compaction = self.compactions.remove(index);
                    self.compaction_done(compaction.thread, item);
                }
                Some(Ok(Err(error))) => {
                    if crate::api::unanswered(&error) || unanswered_by_gateway(&error) {
                        compaction.answer = None;
                        compaction.unanswered = Some(format!("{error:#}"));
                        index += 1;
                    } else {
                        self.compactions.remove(index);
                        self.compaction_refused(&error);
                    }
                }
                Some(Err(tokio::sync::oneshot::error::TryRecvError::Closed)) => {
                    self.compactions.remove(index);
                }
                // Waiting for its row, and the wait is over.
                None if overdue => {
                    let compaction = self.compactions.remove(index);
                    if let Some(said) = compaction.unanswered {
                        self.show_compaction_error(said);
                    }
                }
                Some(Err(tokio::sync::oneshot::error::TryRecvError::Empty)) | None => index += 1,
            }
        }
    }

    /// A compaction of `thread` arrived (its row, as an event): requests for
    /// it that got no answer were answered after all.
    pub(crate) fn compaction_arrived(&mut self, thread: ThreadId) {
        self.compactions
            .retain(|compaction| compaction.thread != thread || compaction.answer.is_some());
    }

    /// Requests that got no answer and now wait for the compaction itself.
    #[cfg(test)]
    pub(crate) fn compactions_waiting_for_row(&self) -> usize {
        self.compactions
            .iter()
            .filter(|compaction| compaction.answer.is_none())
            .count()
    }

    /// Move every running compaction's start `by` into the past.
    #[cfg(test)]
    pub(crate) fn age_compactions(&mut self, by: Duration) {
        for compaction in &mut self.compactions {
            compaction.started = compaction
                .started
                .checked_sub(by)
                .unwrap_or(compaction.started);
        }
    }

    fn compaction_done(&mut self, thread: ThreadId, item: Item) {
        if thread != self.thread.id {
            return;
        }
        // The row usually came first, as an event; this covers a stream that
        // was catching up.
        if matches!(item.data, ItemData::Compaction { .. })
            && !self.items.iter().any(|current| current.id == item.id)
        {
            self.items.push(item);
            self.items.sort_by_key(|current| current.sequence);
            self.refresh_open_messages();
        }
        self.notice = "Compaction checkpoint created".into();
    }

    fn compaction_refused(&mut self, error: &anyhow::Error) {
        tracing::warn!(
            error = format!("{error:#}"),
            status = ?crate::api::refusal_status(error),
            "compaction failed"
        );
        self.show_compaction_error(format!("{error:#}"));
    }

    fn show_compaction_error(&mut self, said: String) {
        self.show_toast(
            Some("Error".into()),
            said,
            ToastVariant::Error,
            Duration::from_secs(7),
        );
    }
}

/// A proxy between this TUI and the server (a time limit on the way through
/// Cloud, Cloud's relay losing the computer's answer) gave up on the request:
/// the server itself never said no. Its own failures (a provider that failed
/// the summary) always name their own code.
fn unanswered_by_gateway(error: &anyhow::Error) -> bool {
    crate::api::refusal_status(error)
        .is_some_and(|status| matches!(status.as_u16(), 502 | 504 | 524))
        && crate::api::refusal_code(error).is_none_or(|code| code == "bad_gateway")
}
