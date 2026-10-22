//! `/btw`: a side chat beside the open chat. Its questions read the chat
//! without adding to it or running tools, follow-ups carry the side chat's
//! earlier questions, and answers stream in as they are written.
//!
//! One side chat per chat, kept here rather than in the view so it outlives
//! it: leaving for the chat keeps the thread, its draft and any answer still
//! being written, and `/btw` brings it back.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt as _;
use haarness_protocol::{BTW_STREAM_CAPABILITY, BtwEvent, BtwTurn, ThreadId};

use crate::api::ApiClient;
use crate::input::AppAction;
use crate::input::composer::Composer;
use crate::state::{App, Overlay};

/// A follow-up carries at most this many of the side chat's last finished
/// questions: the side chat has no compaction.
const HISTORY: usize = 40;
/// How many of the oldest go at once when the thread outgrows what is sent.
const HISTORY_BLOCK: usize = 20;
/// Bytes of earlier questions and answers a follow-up carries at most (the
/// server refuses more than 1 MiB).
const HISTORY_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TurnState {
    Pending,
    Done,
    Error(String),
    Stopped,
}

#[derive(Clone, Debug)]
pub(crate) struct Turn {
    pub question: String,
    pub answer: String,
    pub state: TurnState,
}

#[derive(Default)]
pub(crate) struct Thread {
    pub turns: Vec<Turn>,
    pub draft: String,
    /// Changes when the thread starts over, so an answer still arriving for
    /// the old one is dropped.
    pub key: u64,
}

impl Thread {
    pub fn busy(&self) -> bool {
        self.turns
            .last()
            .is_some_and(|turn| turn.state == TurnState::Pending)
    }

    /// The finished questions, which are what a follow-up sends as its
    /// thread. Past [`HISTORY`] questions or [`HISTORY_BYTES`] the oldest go
    /// [`HISTORY_BLOCK`] at a time, so the first question sent stays the same
    /// from one trim to the next and each follow-up reads back the previous
    /// one's prompt cache.
    fn history(&self) -> Vec<BtwTurn> {
        let finished = self
            .turns
            .iter()
            .filter(|turn| turn.state == TurnState::Done && !turn.answer.trim().is_empty())
            .map(|turn| BtwTurn {
                question: turn.question.clone(),
                answer: turn.answer.clone(),
            })
            .collect::<Vec<_>>();
        let size = |from: usize| -> usize {
            finished[from..]
                .iter()
                .map(|turn| turn.question.len() + turn.answer.len())
                .sum()
        };
        let mut start = finished
            .len()
            .saturating_sub(HISTORY)
            .div_ceil(HISTORY_BLOCK)
            * HISTORY_BLOCK;
        while start + HISTORY_BLOCK < finished.len() && size(start) > HISTORY_BYTES {
            start += HISTORY_BLOCK;
        }
        // A last block still too large gives way one question at a time.
        while start < finished.len() && size(start) > HISTORY_BYTES {
            start += 1;
        }
        finished[start.min(finished.len())..].to_vec()
    }

    /// The last answer that finished.
    pub fn last_answer(&self) -> Option<&str> {
        self.turns
            .iter()
            .rev()
            .find(|turn| turn.state == TurnState::Done && !turn.answer.is_empty())
            .map(|turn| turn.answer.as_str())
    }
}

/// What an answer being written sends back to the event loop.
pub(crate) struct Message {
    chat: ThreadId,
    key: u64,
    event: Result<BtwEvent, String>,
}

/// Stops a running answer when it is dropped.
struct Running(tokio::task::JoinHandle<()>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Default)]
pub(crate) struct SideChats {
    threads: HashMap<ThreadId, Thread>,
    /// The side chat's own message box.
    pub composer: Composer,
    /// The chat whose side chat the view shows.
    pub chat: Option<ThreadId>,
    /// Rows scrolled up from the latest.
    pub scroll: usize,
    pub max_scroll: std::cell::Cell<usize>,
    /// Rows the thread had on screen in the last frame.
    pub page: std::cell::Cell<usize>,
    running: HashMap<ThreadId, Running>,
    /// Where answers being written report to; set by the event loop.
    pub(crate) sender: Option<tokio::sync::mpsc::UnboundedSender<Message>>,
    seq: u64,
}

impl SideChats {
    pub fn thread(&self, chat: ThreadId) -> Option<&Thread> {
        self.threads.get(&chat)
    }

    /// The side chat the view shows.
    pub fn shown(&self) -> Option<&Thread> {
        self.chat.and_then(|chat| self.threads.get(&chat))
    }

    pub fn busy(&self, chat: ThreadId) -> bool {
        self.threads.get(&chat).is_some_and(Thread::busy)
    }

    fn fresh(&mut self) -> Thread {
        self.seq += 1;
        Thread {
            key: self.seq,
            ..Thread::default()
        }
    }

    /// Ends the answer `chat` is writing, if it is still being written.
    fn stop(&mut self, chat: ThreadId) -> bool {
        self.running.remove(&chat);
        let Some(turn) = self
            .threads
            .get_mut(&chat)
            .and_then(|thread| thread.turns.last_mut())
            .filter(|turn| turn.state == TurnState::Pending)
        else {
            return false;
        };
        turn.state = TurnState::Stopped;
        true
    }
}

impl App {
    /// Opens the open chat's side chat; `question` is asked there, or left
    /// in its box while the side chat is still answering.
    pub(crate) fn open_side_chat(&mut self, question: Option<String>, client: &ApiClient) {
        let chat = self.thread.id;
        self.side.chat = Some(chat);
        self.side.scroll = 0;
        let draft = self
            .side
            .thread(chat)
            .map(|thread| thread.draft.clone())
            .unwrap_or_default();
        self.side.composer.set_text(draft);
        self.open_overlay(Overlay::SideChat);
        // The view says what it does itself; a notice from the chat is stale here.
        self.notice.clear();
        let Some(question) = question.filter(|question| !question.trim().is_empty()) else {
            return;
        };
        if self.side.busy(chat) {
            // Kept with whatever the box already holds.
            let draft = self.side.composer.text().trim_end().to_owned();
            self.side.composer.set_text(if draft.is_empty() {
                question
            } else {
                format!("{draft}\n{question}")
            });
            self.notice = self
                .language
                .text("Still answering · your question is in the box")
                .into();
            return;
        }
        self.ask_side_question(question, client);
    }

    /// Back to the chat. An answer still being written keeps going.
    pub(crate) fn close_side_chat(&mut self) {
        if let Some(chat) = self.side.chat {
            let draft = self.side.composer.text().to_owned();
            self.side.threads.entry(chat).or_default().draft = draft;
        }
        self.close_overlay();
    }

    fn ask_side_question(&mut self, question: String, client: &ApiClient) {
        let Some(chat) = self.side.chat else { return };
        if !self.side.threads.contains_key(&chat) {
            let fresh = self.side.fresh();
            self.side.threads.insert(chat, fresh);
        }
        let thread = self.side.threads.get_mut(&chat).expect("side chat exists");
        let history = thread.history();
        let key = thread.key;
        thread.turns.push(Turn {
            question: question.clone(),
            answer: String::new(),
            state: TurnState::Pending,
        });
        self.side.scroll = 0;
        let Some(sender) = self.side.sender.clone() else {
            return;
        };
        // An older server answers in one piece, without its follow-ups' thread.
        let streamed = self.server_info.as_ref().is_none_or(|info| {
            info.capabilities
                .iter()
                .any(|capability| capability == BTW_STREAM_CAPABILITY)
        });
        let client = client.clone();
        let task = tokio::spawn(async move {
            let send = |event| {
                let _ = sender.send(Message { chat, key, event });
            };
            if !streamed {
                send(
                    client
                        .btw(chat, question, history)
                        .await
                        .map(|answer| BtwEvent::Done {
                            answer: answer.answer,
                            input_tokens: answer.input_tokens,
                            output_tokens: answer.output_tokens,
                        })
                        .map_err(|error| format!("{error:#}")),
                );
                return;
            }
            match client.btw_stream(chat, question, history).await {
                Ok(mut events) => {
                    while let Some(event) = events.next().await {
                        send(event.map_err(|error| format!("{error:#}")));
                    }
                }
                Err(error) => send(Err(format!("{error:#}"))),
            }
        });
        self.side.running.insert(chat, Running(task));
    }

    /// An answer being written moved on.
    pub(crate) fn side_message(&mut self, message: Message) {
        let Some(thread) = self
            .side
            .threads
            .get_mut(&message.chat)
            .filter(|thread| thread.key == message.key)
        else {
            return;
        };
        let Some(turn) = thread
            .turns
            .last_mut()
            .filter(|turn| turn.state == TurnState::Pending)
        else {
            return;
        };
        let ended = match message.event {
            Ok(BtwEvent::Delta { text }) => {
                turn.answer.push_str(&text);
                false
            }
            Ok(BtwEvent::Text { text }) => {
                turn.answer = text;
                false
            }
            Ok(BtwEvent::Done { answer, .. }) => {
                turn.answer = answer;
                turn.state = TurnState::Done;
                true
            }
            Ok(BtwEvent::Error { message, .. }) | Err(message) => {
                turn.state = TurnState::Error(message);
                true
            }
        };
        if ended {
            self.side.running.remove(&message.chat);
        }
    }

    /// Ctrl+C in the side chat: clears the box first, then stops an answer
    /// being written, then goes back to the chat.
    pub(crate) fn side_interrupt(&mut self) {
        let Some(chat) = self.side.chat else { return };
        // The stopped answer says so itself.
        if !self.side.composer.is_empty() {
            self.side.composer.clear();
        } else if !self.side.stop(chat) {
            self.close_side_chat();
        }
    }

    fn send_side_question(&mut self, client: &ApiClient) {
        let Some(chat) = self.side.chat else { return };
        let text = self.side.composer.expanded_text();
        let question = text.trim();
        if question.is_empty() {
            return;
        }
        match question {
            "/clear" | "/new" => {
                self.side.composer.clear();
                self.notice.clear();
                self.side.stop(chat);
                let fresh = self.side.fresh();
                self.side.threads.insert(chat, fresh);
                self.side.scroll = 0;
                return;
            }
            "/copy" => {
                self.side.composer.clear();
                match self.side.thread(chat).and_then(Thread::last_answer) {
                    Some(answer) => {
                        crate::terminal::copy_to_clipboard(answer);
                        self.notice = self.language.text("Copied the answer").into();
                    }
                    None => self.notice = self.language.text("Nothing to copy yet").into(),
                }
                return;
            }
            _ => {}
        }
        if self.side.busy(chat) {
            self.notice = self
                .language
                .text("Still answering · ctrl+c stops it")
                .into();
            return;
        }
        let question = question.to_owned();
        self.side.composer.clear();
        self.ask_side_question(question, client);
    }

    fn scroll_side_chat(&mut self, rows: isize) {
        let next = self.side.scroll.saturating_add_signed(rows);
        self.side.scroll = next.min(self.side.max_scroll.get());
    }

    /// Mouse wheel over the side chat: up reads earlier.
    pub(crate) fn wheel_side_chat(&mut self, up: bool) {
        self.scroll_side_chat(if up { 3 } else { -3 });
    }

    /// A key while the side chat is open. It owns every key: its box edits,
    /// Enter asks, Esc goes back to the chat.
    pub(crate) fn handle_side_key(&mut self, key: KeyEvent, client: &ApiClient) -> AppAction {
        let newline = key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
        let page = (self.side.page.get() / 2).max(1).cast_signed();
        match key.code {
            KeyCode::Esc => {
                self.close_side_chat();
                return AppAction::Continue;
            }
            KeyCode::Enter if !newline => {
                self.send_side_question(client);
                return AppAction::Continue;
            }
            KeyCode::PageUp => {
                self.scroll_side_chat(page);
                return AppAction::Continue;
            }
            KeyCode::PageDown => {
                self.scroll_side_chat(-page);
                return AppAction::Continue;
            }
            _ => {}
        }
        let composer = &mut self.side.composer;
        let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Enter => composer.insert_char('\n'),
            KeyCode::Char(character) if plain => composer.insert_char(character),
            KeyCode::Backspace
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                composer.delete_word_back();
            }
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                composer.move_word_left();
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                composer.move_word_right();
            }
            KeyCode::Backspace => composer.backspace(),
            KeyCode::Delete => composer.delete(),
            KeyCode::Left => composer.move_left(),
            KeyCode::Right => composer.move_right(),
            KeyCode::Home => composer.move_home(),
            KeyCode::End => composer.move_end(),
            KeyCode::Up => {
                composer.move_up();
            }
            KeyCode::Down => {
                composer.move_down();
            }
            KeyCode::Char('z' | 'Z') if key.modifiers == KeyModifiers::SUPER => composer.undo(),
            KeyCode::Char('z' | 'Z')
                if key.modifiers == KeyModifiers::SUPER | KeyModifiers::SHIFT =>
            {
                composer.redo();
            }
            KeyCode::Char(character) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                match character {
                    'a' => composer.move_home(),
                    'e' => composer.move_end(),
                    'f' => composer.move_right(),
                    'b' => composer.move_left(),
                    'w' => composer.delete_word_back(),
                    'k' => composer.kill_to_end(),
                    'u' => composer.kill_to_start(),
                    'j' => composer.insert_char('\n'),
                    '-' => composer.undo(),
                    '.' => composer.redo(),
                    _ => {}
                }
            }
            _ => {}
        }
        AppAction::Continue
    }

    /// Text pasted while the side chat is open goes into its box.
    pub(crate) fn paste_side_chat(&mut self, text: &str) {
        self.side.composer.paste(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(count: usize, answer: &str) -> Thread {
        Thread {
            turns: (0..count)
                .map(|at| Turn {
                    question: format!("q{at}"),
                    answer: answer.to_owned(),
                    state: TurnState::Done,
                })
                .collect(),
            ..Thread::default()
        }
    }

    #[test]
    fn a_long_side_chat_drops_its_oldest_questions_in_blocks() {
        let first = |count: usize, answer: &str| {
            thread(count, answer)
                .history()
                .first()
                .map(|turn| turn.question.clone())
        };
        assert_eq!(thread(HISTORY, "a").history().len(), HISTORY);
        assert_eq!(first(HISTORY, "a").as_deref(), Some("q0"));
        // Between two trims the first question sent stays put.
        for count in [HISTORY + 1, HISTORY + 7, HISTORY + HISTORY_BLOCK] {
            assert_eq!(first(count, "a").as_deref(), Some("q20"), "{count}");
        }
        assert_eq!(
            first(HISTORY + HISTORY_BLOCK + 1, "a").as_deref(),
            Some("q40")
        );
        // Long answers give way by size, a block at a time too.
        let long = "x".repeat(HISTORY_BYTES / 25);
        assert_eq!(first(30, &long).as_deref(), Some("q20"));
        // An answer larger than the whole budget is not sent at all.
        assert!(
            thread(1, &"x".repeat(HISTORY_BYTES + 1))
                .history()
                .is_empty()
        );
        // Unfinished questions are never part of the thread.
        let mut unfinished = thread(2, "a");
        unfinished.turns[1].state = TurnState::Stopped;
        assert_eq!(unfinished.history().len(), 1);
    }
}
