//! `/compact` off the event loop. A model summary of a long chat can take
//! minutes; awaiting it in the key handler froze the TUI, and the client's
//! 15 s deadline then said "This took too long" for a summary the server
//! goes on to finish. Each request now runs on a worker task while the band
//! says `compacting`, and the chat stays usable.

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
/// Cloud) gave up on the request: the server itself never said no. Its own
/// failures (a provider that failed the summary) always name their code.
fn unanswered_by_gateway(error: &anyhow::Error) -> bool {
    crate::api::refusal_status(error)
        .is_some_and(|status| matches!(status.as_u16(), 502 | 504 | 524))
        && crate::api::refusal_code(error).is_none()
}
