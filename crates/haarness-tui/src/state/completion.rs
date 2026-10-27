//! What the reader is told when a turn ends.
//!
//! A completed turn's "finished" banner waits a short grace before it goes
//! out. The daemon starts the next queued prompt (or the reply to steering a
//! stop left unread) as a new turn moments after the previous one ends, so a
//! banner sent at once would call work finished while it is still going. A
//! turn that starts in the same conversation inside the grace drops the held
//! banner and carries the run's start forward, so the last turn of a queue is
//! judged by how long the whole queue took and announced once. A failure is
//! never taken back and goes out at once; a stop says nothing. A turn that
//! ends while work it started in the background (a child agent, a workflow
//! run) still owes the conversation a result is no end at all: it drops a
//! held banner and carries the run on, and the turn that receives the last
//! result is announced.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use haarness_protocol::ThreadId;

use super::{App, ToastVariant};

/// Matches Desktop's `COMPLETION_GRACE_MS`: the next queued turn starts
/// 31–192 ms after the previous one ends.
pub const COMPLETION_GRACE: Duration = Duration::from_millis(500);

/// Work at least this long earns a "finished" banner; quick turns stay quiet
/// because the reader is still looking at them.
const NOTIFY_AFTER: Duration = Duration::from_secs(20);

/// How long a run waiting on background work is remembered: past this its
/// work ended without a result long ago, or the conversation was left.
const OWED_RUN_LIMIT: Duration = Duration::from_secs(24 * 60 * 60);

/// How a turn ended, read from its `turn.finished` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    Completed,
    Failed,
    /// Completed or failed while background work it started still owes the
    /// conversation a result (the payload's `background` count).
    Owed,
    /// Interrupted (a stop), or anything else that is not a completion.
    Stopped,
}

impl TurnEnd {
    pub fn from_payload(payload: &serde_json::Value) -> Self {
        let owed = payload["background"]
            .as_u64()
            .is_some_and(|count| count > 0);
        match payload["status"].as_str() {
            Some("completed" | "failed") if owed => Self::Owed,
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
    /// Set when a new turn drops a held completion, or when a turn ends with
    /// background work still owed; read by the next end.
    runs: HashMap<ThreadId, Run>,
}

#[derive(Debug, Clone, Copy)]
struct Run {
    started: Instant,
    /// The run waits on background work: only the turn that work's result
    /// starts carries it on.
    owed: bool,
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
                self.runs.insert(
                    thread,
                    Run {
                        started,
                        owed: false,
                    },
                );
            }
        }
    }

    /// A person's own prompt reached `thread` and starts its next turn. A
    /// run waiting on background work does not carry on into it: that work
    /// was stopped without a result, or the person moved on, and only a
    /// delivered result (a runtime message) continues the run.
    pub fn prompted(&mut self, thread: ThreadId) {
        if self.runs.get(&thread).is_some_and(|run| run.owed) {
            self.runs.remove(&thread);
        }
    }

    /// Remember that `thread`'s run from `started` waits on background work.
    fn owe(&mut self, thread: ThreadId, started: Instant) {
        self.runs
            .retain(|_, run| !run.owed || run.started.elapsed() < OWED_RUN_LIMIT);
        self.runs.insert(
            thread,
            Run {
                started,
                owed: true,
            },
        );
    }

    /// Forget anything `thread` was holding: a newer end is the true one.
    fn settle(&mut self, thread: ThreadId) -> Option<Instant> {
        self.held.retain(|held| held.thread != thread);
        self.runs.remove(&thread).map(|run| run.started)
    }

    /// Whether `thread`'s run waits on background work.
    #[cfg(test)]
    pub fn owes(&self, thread: ThreadId) -> bool {
        self.runs.get(&thread).is_some_and(|run| run.owed)
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
            // Still at work: the turn that receives the last result is the
            // end, judged from when this run began.
            TurnEnd::Owed => {
                if let Some(started) = run.into_iter().chain(started).min() {
                    self.completions.owe(thread, started);
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
