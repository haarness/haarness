//! `/compact` off the event loop. A model summary of a long chat can take
//! minutes; awaiting it in the key handler froze the TUI, and the client's
//! 15 s deadline then said "This took too long" for a summary the server
//! goes on to finish. Each request now runs on a worker task while the band
//! says `compacting`, and the chat stays usable.
//!
//! A message sent meanwhile would start a turn, and the server then throws
//! the summary away (the chat changed under it). So it waits above the
//! prompt, queued, and goes out once the compaction is over, whichever way.
//! Leaving the chat or quitting puts it back in the chat's draft instead.

use std::time::{Duration, Instant};

use haarness_protocol::{CompactRequest, Item, ItemData, ThreadId};

use crate::api::ApiClient;
use crate::state::{App, ToastVariant};

/// The first release whose server finishes a summary whose request went.
const FINISHES_CUT_COMPACTIONS: [u64; 3] = [0, 1, 24];

/// Said when the chat changed while a compaction waited for its summary.
pub(crate) const CHAT_CHANGED: &str = "The chat changed while it was being summarised";

/// Said when `!` or a command that starts a turn waits for a compaction.
pub(crate) const WAIT_FOR_COMPACTION: &str =
    "The chat is being compacted · try again once it's done";

/// Said when `/compact` is asked again while the chat is being compacted.
pub(crate) const ALREADY_COMPACTING: &str = "This chat is already being compacted";

/// Said when another compaction under way never landed within the wait.
pub(crate) const DID_NOT_COME_BACK: &str = "The summary didn't come back in time";

/// How long Cloud's word that it lost the computer must come after the
/// request went: one said sooner never reached the computer at all.
const LOST_AFTER: Duration = Duration::from_secs(5);

/// One `/compact` this TUI asked for.
pub(crate) struct Compaction {
    thread: ThreadId,
    started: Instant,
    /// The request, until it answers.
    answer: Option<tokio::sync::oneshot::Receiver<anyhow::Result<Item>>>,
    /// What the request said when its answer never came from the server
    /// (or the server said another compaction was under way). The server
    /// finishes that summary, so this waits for the compaction's row, and
    /// says it only if the row never comes.
    unanswered: Option<String>,
    /// The chat's newest item when it was asked for: a compaction after it
    /// in the chat's history is the one waited for.
    seen_sequence: u64,
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
        // One at a time: asked again, it would only wait for the first.
        if self.compacting_since().is_some() {
            self.refuse_while_compacting(ALREADY_COMPACTING);
            return;
        }
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
        let seen_sequence = self
            .items
            .iter()
            .map(|item| item.sequence)
            .max()
            .unwrap_or(0);
        self.compactions.push(Compaction {
            thread,
            started: Instant::now(),
            answer: Some(receiver),
            unanswered: None,
            seen_sequence,
        });
    }

    /// Refuse what would void the open chat's compaction, with `reason`: on
    /// a card too, since the band says `compacting` meanwhile.
    pub(crate) fn refuse_while_compacting(&mut self, reason: &'static str) {
        let said = self.language.text(reason).to_owned();
        self.show_toast(None, said, ToastVariant::Warning, Duration::from_secs(5));
        self.refuse_command(reason);
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

    /// Put the open chat's waiting messages back in the prompt, oldest first
    /// and ahead of whatever is typed there, with their attachments: when the
    /// person takes them back, leaves the chat or quits, and when one could
    /// not go out. Whether anything came back.
    pub(crate) fn return_held(&mut self) -> bool {
        let thread = self.thread.id;
        let (mine, others): (Vec<Held>, Vec<Held>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|held| held.thread == thread);
        self.held = others;
        if mine.is_empty() {
            return false;
        }
        let mut texts: Vec<String> = mine.iter().map(|held| held.text.clone()).collect();
        let draft = self.composer.expanded_text();
        if self.shell_mode {
            // Never prose in a shell line: the prompt leaves shell mode and
            // the command stays as a line of its own.
            self.shell_mode = false;
            if !draft.trim().is_empty() {
                texts.push(format!("!{draft}"));
            }
        } else if !draft.trim().is_empty() {
            texts.push(draft);
        }
        self.composer.set_text(texts.join("\n\n"));
        let mut attachments: Vec<String> =
            mine.into_iter().flat_map(|held| held.attachments).collect();
        attachments.append(&mut self.attachments);
        self.attachments = attachments;
        true
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
            if let Err(error) = self.send_held(&held, client).await {
                // It and every one after it come back together, in order.
                self.held.insert(index, held);
                self.return_held();
                tracing::warn!(
                    error = format!("{error:#}"),
                    "a held message could not be sent"
                );
                self.show_compaction_error(self.thread.id, format!("{error:#}"));
                return;
            }
        }
    }

    /// Send `held` as Enter does: queued, steering a turn that runs, or as a
    /// turn of its own. Fails, sending nothing, when it did not go out.
    async fn send_held(&mut self, held: &Held, client: &ApiClient) -> anyhow::Result<()> {
        let own = std::mem::replace(&mut self.attachments, held.attachments.clone());
        let sent = async {
            let delivery = if held.queue {
                haarness_protocol::InputDelivery::Queue
            } else {
                client.input_delivery().await?
            };
            self.input_delivery = Some(delivery);
            if delivery == haarness_protocol::InputDelivery::Queue {
                self.defer_prompt(held.text.clone(), client).await
            } else if self.active_turn.is_some() {
                self.queue_prompt(held.text.clone(), client).await
            } else {
                self.submit_prompt(held.text.clone(), client).await
            }
        }
        .await;
        self.attachments = own;
        match sent {
            Ok(true) => Ok(()),
            // Refused here, with the reason in the band.
            Ok(false) => Err(anyhow::anyhow!(self.notice.clone())),
            Err(error) => Err(error),
        }
    }

    /// Take delivery of what the compaction requests answered.
    pub(crate) fn poll_compactions(&mut self) {
        let now = Instant::now();
        let finishes = self
            .server_info
            .as_ref()
            .is_some_and(|info| crate::updates::at_least(&info.version, FINISHES_CUT_COMPACTIONS));
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
                    let out_for = now.duration_since(compaction.started);
                    if already_compacting(&error) {
                        compaction.answer = None;
                        compaction.unanswered =
                            Some(self.language.text(DID_NOT_COME_BACK).to_owned());
                        index += 1;
                    } else if finishes && cut_on_the_way(&error, out_for) {
                        compaction.answer = None;
                        compaction.unanswered = Some(format!("{error:#}"));
                        index += 1;
                    } else {
                        let compaction = self.compactions.remove(index);
                        tracing::warn!(
                            error = format!("{error:#}"),
                            status = ?crate::api::refusal_status(&error),
                            "compaction failed"
                        );
                        // A turn or an item came in while it summarized.
                        let said = if error.to_string().contains("thread changed") {
                            self.language.text(CHAT_CHANGED).to_owned()
                        } else {
                            format!("{error:#}")
                        };
                        self.show_compaction_error(compaction.thread, said);
                    }
                }
                Some(Err(tokio::sync::oneshot::error::TryRecvError::Closed)) => {
                    self.compactions.remove(index);
                }
                // Waiting for its row, and the wait is over.
                None if overdue => {
                    let compaction = self.compactions.remove(index);
                    if let Some(said) = compaction.unanswered {
                        self.show_compaction_error(compaction.thread, said);
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

    /// The open chat's history was read again (opening it, a resync): a
    /// compaction in it newer than what a wait began with is the one waited
    /// for, which landed while its row went unseen.
    pub(crate) fn compaction_rows_loaded(&mut self) {
        let thread = self.thread.id;
        let newest = self
            .items
            .iter()
            .filter(|item| matches!(item.data, ItemData::Compaction { .. }))
            .map(|item| item.sequence)
            .max();
        let Some(newest) = newest else {
            return;
        };
        self.compactions.retain(|compaction| {
            compaction.thread != thread
                || compaction.answer.is_some()
                || newest <= compaction.seen_sequence
        });
    }

    /// Something else happened in `thread` (another item, a turn): a summary
    /// waited for there could no longer be committed, so the wait ends.
    pub(crate) fn chat_changed_while_compacting(&mut self, thread: ThreadId) {
        let before = self.compactions.len();
        self.compactions
            .retain(|compaction| compaction.thread != thread || compaction.answer.is_some());
        if self.compactions.len() < before {
            let said = self.language.text(CHAT_CHANGED).to_owned();
            self.show_compaction_error(thread, said);
        }
    }

    /// The server is another process now: summaries waited for died with
    /// the one before.
    pub(crate) fn server_restarted(&mut self) {
        let (gone, kept): (Vec<Compaction>, Vec<Compaction>) =
            std::mem::take(&mut self.compactions)
                .into_iter()
                .partition(|compaction| compaction.answer.is_none());
        self.compactions = kept;
        for compaction in gone {
            if let Some(said) = compaction.unanswered {
                self.show_compaction_error(compaction.thread, said);
            }
        }
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
        // It answers whoever else waited for this chat's compaction too.
        self.compaction_arrived(thread);
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

    /// Say why `thread`'s compaction failed: under that chat's title when it
    /// is not the one open.
    fn show_compaction_error(&mut self, thread: ThreadId, said: String) {
        let title = if thread == self.thread.id {
            "Error".to_owned()
        } else {
            self.threads
                .iter()
                .find(|chat| chat.id == thread)
                .map_or_else(|| "Error".to_owned(), |chat| chat.title.clone())
        };
        self.show_toast(
            Some(title),
            said,
            ToastVariant::Error,
            Duration::from_secs(7),
        );
    }
}

/// The server said another compaction of the chat is under way: one this
/// TUI or another client asked for, which lands as its own row.
fn already_compacting(error: &anyhow::Error) -> bool {
    crate::api::refusal_status(error) == Some(reqwest::StatusCode::CONFLICT)
        && error.to_string().contains("already being compacted")
}

/// The request's answer was lost on its way back from another computer,
/// while that computer goes on with the summary (the same rule Desktop and
/// the web keep): Cloudflare's 524 at its time limit; any 5xx a proxy said
/// with no error of the server's own and nothing to read (an HTML page, an
/// empty body); Cloud losing the computer's connection once the request had
/// been out a while; or the connection through Cloud's relay dropping. The
/// server's own failures (the model failing the summary, Cloud's word that
/// the computer's runtime failed) name their code and are not cuts.
fn cut_on_the_way(error: &anyhow::Error, out_for: Duration) -> bool {
    if crate::api::lost_on_the_way(error) {
        return true;
    }
    let code = crate::api::refusal_code(error);
    match crate::api::refusal_status(error).map(|status| status.as_u16()) {
        Some(524) => true,
        // A proxy's own answer: no error of the server's, nothing to read.
        Some(500..=599) => code.is_none() && crate::api::refused_wordless(error),
        // Cloud lost the computer after the request reached it; said at once,
        // the computer was not there to take it.
        Some(409) => {
            out_for >= LOST_AFTER
                && (code == Some("remote_offline")
                    || error
                        .to_string()
                        .eq_ignore_ascii_case(crate::api::COMPUTER_UNREACHABLE))
        }
        _ => false,
    }
}
