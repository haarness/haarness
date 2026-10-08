//! What the reader is told when a turn ends.
//!
//! A completed turn's "finished" banner waits a short grace before it goes
//! out. The daemon starts the next queued prompt (or the reply to steering a
//! stop left unread) as a new turn moments after the previous one ends, so a
//! banner sent at once would call work finished while it is still going. A
//! turn that starts in the same conversation inside the grace drops the held
//! banner and carries the run's start forward, so the last turn of a queue is
//! judged by how long the whole queue took and announced once. A failure is
//! never taken back and goes out at once; a stop says nothing.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use haarness_protocol::ThreadId;

use super::{App, ToastVariant};

/// Matches Desktop's `COMPLETION_GRACE_MS` and Upstream, which measured the
/// next turn starting 31–192 ms after the previous one ended.
pub const COMPLETION_GRACE: Duration = Duration::from_millis(500);

/// Work at least this long earns a "finished" banner; quick turns stay quiet
/// because the reader is still looking at them.
const NOTIFY_AFTER: Duration = Duration::from_secs(20);

/// How a turn ended, read from the `turn.finished` payload's `status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    Completed,
    Failed,
    /// Interrupted (a stop), or anything else that is not a completion.
    Stopped,
}

impl TurnEnd {
    pub fn from_status(status: &serde_json::Value) -> Self {
        match status.as_str() {
            Some("completed") => Self::Completed,
            Some("failed") => Self::Failed,
            _ => Self::Stopped,
        }
    }
}

/// Completions waiting out [`COMPLETION_GRACE`], at most one per conversation.
#[derive(Debug, Default)]
pub struct HeldCompletions {
    held: Vec<Held>,
    /// When the current run of back-to-back turns began, per conversation.
    /// Set when a new turn drops a held completion; read by the next end.
    runs: HashMap<ThreadId, Instant>,
}

#[derive(Debug)]
struct Held {
    thread: ThreadId,
    message: String,
    /// Another conversation than the one on screen: it is announced however
    /// long it took, with a card even for a reader watching the terminal.
    elsewhere: bool,
    /// When the announced run of turns began, if known.
    started: Option<Instant>,
    finished: Instant,
}

impl HeldCompletions {
    /// A turn started in `thread`. Whatever it was about to call finished is
    /// still going, so its banner is dropped and the run's start carries on.
    pub fn resume(&mut self, thread: ThreadId) {
        if let Some(index) = self.held.iter().position(|held| held.thread == thread) {
            let held = self.held.remove(index);
            if let Some(started) = held.started {
                self.runs.insert(thread, started);
            }
        }
    }

    /// Forget anything `thread` was holding: a newer end is the true one.
    fn settle(&mut self, thread: ThreadId) -> Option<Instant> {
        self.held.retain(|held| held.thread != thread);
        self.runs.remove(&thread)
    }

    /// Whether `thread` has a completion waiting out its grace.
    #[cfg(test)]
    pub fn holds(&self, thread: ThreadId) -> bool {
        self.held.iter().any(|held| held.thread == thread)
    }
}

impl App {
    /// A turn ended in `thread`, titled `title`. `elsewhere` marks a
    /// conversation other than the one on screen; `started` is when the
    /// turn began, if known.
    pub(super) fn turn_ended(
        &mut self,
        thread: ThreadId,
        end: TurnEnd,
        title: &str,
        elsewhere: bool,
        started: Option<Instant>,
    ) {
        let run = self.completions.settle(thread);
        match end {
            TurnEnd::Completed => self.completions.held.push(Held {
                thread,
                message: format!("Haarness finished: {title}"),
                elsewhere,
                started: run.into_iter().chain(started).min(),
                finished: Instant::now(),
            }),
            TurnEnd::Failed => {
                let message = format!("Haarness failed: {title}");
                self.pending_notification = Some(message.clone());
                // The error is already in the transcript in front of a
                // reader watching this conversation.
                if elsewhere || !self.terminal_focused {
                    self.show_toast(None, message, ToastVariant::Error, Duration::from_secs(8));
                }
            }
            // The reader (or another client) asked for the stop.
            TurnEnd::Stopped => {}
        }
    }

    /// Send each held completion whose grace has run out by `now`. The event
    /// loop calls this on its tick.
    pub fn release_completions(&mut self, now: Instant) {
        let (due, held) = std::mem::take(&mut self.completions.held)
            .into_iter()
            .partition(|held| now.saturating_duration_since(held.finished) >= COMPLETION_GRACE);
        self.completions.held = held;
        for held in due {
            let long = held.started.is_some_and(|started| {
                held.finished.saturating_duration_since(started) >= NOTIFY_AFTER
            });
            if !held.elsewhere && !long {
                continue;
            }
            self.pending_notification = Some(held.message.clone());
            // The reply just landed in front of a reader watching this
            // conversation; only an unfocused terminal needs a card.
            if held.elsewhere || !self.terminal_focused {
                self.show_toast(
                    None,
                    held.message,
                    ToastVariant::Success,
                    Duration::from_secs(5),
                );
            }
        }
    }
}
