//! The prompt editor: cursor movement, word operations, kill/yank-style
//! deletion, undo/redo, input history, and collapsed large pastes. Pure state
//! with no terminal knowledge, so every behavior is unit-testable.

use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

const PASTE_LINE_THRESHOLD: usize = 8;
const PASTE_CHAR_THRESHOLD: usize = 1000;
const UNDO_CAP: usize = 100;
pub const HISTORY_CAP: usize = 100;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum LastEdit {
    #[default]
    None,
    Typing,
    Other,
}

#[derive(Clone, Copy)]
enum ExpansionKind {
    Paste(usize),
    Context(usize),
}
#[derive(Clone)]
struct Expansion {
    start: usize,
    end: usize,
    content: String,
    kind: ExpansionKind,
}

#[derive(Clone)]
struct EditSnapshot {
    text: String,
    cursor: usize,
    expansions: Vec<Expansion>,
}

#[derive(Default)]
pub struct Composer {
    text: String,
    cursor: usize,
    undo: Vec<EditSnapshot>,
    redo: Vec<EditSnapshot>,
    history: Vec<String>,
    history_index: Option<usize>,
    history_owner: Option<(String, String)>,
    pending_history: Option<String>,
    stash: Option<EditSnapshot>,
    expansions: Vec<Expansion>,
    last_edit: LastEdit,
}

impl Composer {
    #[must_use]
    #[cfg(test)]
    pub fn with_history(history: Vec<String>) -> Self {
        Self {
            history,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
    #[must_use]
    #[cfg(test)]
    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub(crate) fn history_owner(&self) -> Option<&(String, String)> {
        self.history_owner.as_ref()
    }
    pub(crate) fn set_history_owner(
        &mut self,
        scope: String,
        thread: String,
        history: Vec<String>,
    ) {
        self.history_owner = Some((scope, thread));
        self.history = history;
        self.history_index = None;
        self.stash = None;
    }
    pub(crate) fn pending_history(&mut self) -> Option<String> {
        self.pending_history.take()
    }
    fn snapshot(&mut self, edit: LastEdit) {
        let coalesce = edit == LastEdit::Typing && self.last_edit == LastEdit::Typing;
        if !coalesce {
            self.undo.push(self.edit_snapshot());
            if self.undo.len() > UNDO_CAP {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = edit;
    }

    fn edit_snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            expansions: self.expansions.clone(),
        }
    }

    fn restore_snapshot(&mut self, snapshot: EditSnapshot) {
        self.text = snapshot.text;
        self.cursor = snapshot.cursor;
        self.expansions = snapshot.expansions;
    }

    fn move_cursor(&mut self, mut cursor: usize) {
        for expansion in &self.expansions {
            if expansion.start < cursor && cursor < expansion.end {
                cursor = if cursor < self.cursor {
                    expansion.start
                } else {
                    expansion.end
                };
                break;
            }
        }
        cursor = if cursor < self.cursor {
            self.grapheme_floor(cursor)
        } else {
            self.grapheme_ceil(cursor)
        };
        if cursor != self.cursor {
            self.cursor = cursor;
            self.last_edit = LastEdit::None;
        }
    }

    fn grapheme_floor(&self, position: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .map(|(start, _)| start)
            .chain(std::iter::once(self.text.len()))
            .take_while(|start| *start <= position)
            .last()
            .unwrap_or(0)
    }
    fn grapheme_ceil(&self, position: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .map(|(start, _)| start)
            .find(|start| *start >= position)
            .unwrap_or(self.text.len())
    }

    /// All edits update the tracked ranges. A partial deletion removes an
    /// entire chip, and insertions cannot split its display placeholder.
    fn edit(&mut self, mut start: usize, mut end: usize, value: &str, kind: LastEdit) -> usize {
        if start > end || end > self.text.len() {
            return self.cursor;
        }
        if start == end {
            if let Some(expansion) = self
                .expansions
                .iter()
                .find(|e| e.start < start && start < e.end)
            {
                start = expansion.end;
            }
            start = self.grapheme_ceil(start);
            end = start;
        } else {
            start = self.grapheme_floor(start);
            end = self.grapheme_ceil(end);
            for expansion in &self.expansions {
                if start < expansion.end && expansion.start < end {
                    start = start.min(expansion.start);
                    end = end.max(expansion.end);
                }
            }
        }
        self.snapshot(kind);
        self.expansions.retain_mut(|expansion| {
            if start < expansion.end && expansion.start < end {
                return false;
            }
            if expansion.start >= end {
                expansion.start = expansion.start - (end - start) + value.len();
                expansion.end = expansion.end - (end - start) + value.len();
            }
            true
        });
        self.text.replace_range(start..end, value);
        self.cursor = self.grapheme_ceil(start + value.len());
        self.history_index = None;
        start
    }

    pub fn insert_char(&mut self, character: char) {
        self.edit(
            self.cursor,
            self.cursor,
            &character.to_string(),
            if character.is_whitespace() {
                LastEdit::Other
            } else {
                LastEdit::Typing
            },
        );
    }

    pub fn insert_str(&mut self, value: &str) {
        if !value.is_empty() {
            self.edit(self.cursor, self.cursor, value, LastEdit::Other);
        }
    }

    /// Large pastes are opaque chips with tracked byte ranges. Their labels
    /// never act as delimiters in ordinary text or inside another payload.
    pub fn paste(&mut self, value: &str) {
        let value = value.replace("\r\n", "\n").replace('\r', "\n");
        let lines = value.lines().count();
        if lines > PASTE_LINE_THRESHOLD || value.len() > PASTE_CHAR_THRESHOLD {
            let id = self
                .expansions
                .iter()
                .filter_map(|e| match e.kind {
                    ExpansionKind::Paste(id) => Some(id),
                    ExpansionKind::Context(_) => None,
                })
                .max()
                .unwrap_or(0)
                + 1;
            let placeholder = format!("[#{id} pasted {} lines]", lines.max(1));
            let start = self.edit(self.cursor, self.cursor, &placeholder, LastEdit::Other);
            self.expansions.push(Expansion {
                start,
                end: start + placeholder.len(),
                content: value,
                kind: ExpansionKind::Paste(id),
            });
        } else {
            self.insert_str(&value);
        }
    }

    pub fn backspace(&mut self) {
        if let Some(previous) = self.previous_boundary() {
            self.edit(previous, self.cursor, "", LastEdit::Other);
        }
    }
    pub fn delete(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.edit(self.cursor, next, "", LastEdit::Other);
        }
    }
    pub fn delete_word_back(&mut self) {
        let target = self.word_left_boundary();
        if target != self.cursor {
            self.edit(target, self.cursor, "", LastEdit::Other);
        }
    }
    pub fn delete_word_forward(&mut self) {
        let target = self.word_right_boundary();
        if target != self.cursor {
            self.edit(self.cursor, target, "", LastEdit::Other);
        }
    }
    pub fn kill_to_end(&mut self) {
        let end = self.line_end();
        if end != self.cursor {
            self.edit(self.cursor, end, "", LastEdit::Other);
        }
    }
    pub fn kill_to_start(&mut self) {
        let start = self.line_start();
        if start != self.cursor {
            self.edit(start, self.cursor, "", LastEdit::Other);
        }
    }

    pub fn move_left(&mut self) {
        if let Some(previous) = self.previous_boundary() {
            self.move_cursor(previous);
        }
    }
    pub fn move_right(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.move_cursor(next);
        }
    }
    pub fn move_word_left(&mut self) {
        self.move_cursor(self.word_left_boundary());
    }
    pub fn move_word_right(&mut self) {
        self.move_cursor(self.word_right_boundary());
    }
    pub fn move_home(&mut self) {
        self.move_cursor(self.line_start());
    }
    pub fn move_end(&mut self) {
        self.move_cursor(self.line_end());
    }

    /// Move the cursor one visual line up. Returns false at the first line so
    /// the caller can fall through to history recall.
    pub fn move_up(&mut self) -> bool {
        let start = self.line_start();
        if start == 0 {
            return false;
        }
        let column = self.text[start..self.cursor].width();
        let previous_start = self.text[..start - 1]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let cursor = self.line_boundary_at_column(previous_start, start - 1, column);
        self.move_cursor(cursor);
        true
    }

    /// Move the cursor one visual line down. Returns false at the last line.
    pub fn move_down(&mut self) -> bool {
        let end = self.line_end();
        if end == self.text.len() {
            return false;
        }
        let start = self.line_start();
        let column = self.text[start..self.cursor].width();
        let next_start = end + 1;
        let next_end = self.text[next_start..]
            .find('\n')
            .map_or(self.text.len(), |index| next_start + index);
        let cursor = self.line_boundary_at_column(next_start, next_end, column);
        self.move_cursor(cursor);
        true
    }

    pub fn undo(&mut self) {
        if let Some(snapshot) = self.undo.pop() {
            self.redo.push(self.edit_snapshot());
            self.restore_snapshot(snapshot);
            self.last_edit = LastEdit::None;
        }
    }
    pub fn redo(&mut self) {
        if let Some(snapshot) = self.redo.pop() {
            self.undo.push(self.edit_snapshot());
            self.restore_snapshot(snapshot);
            self.last_edit = LastEdit::None;
        }
    }

    /// Recall the previous history entry, stashing the in-progress draft on
    /// first use. Returns false when there is nothing older.
    pub fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let next_index = match self.history_index {
            None => {
                self.stash = Some(self.edit_snapshot());
                self.history.len() - 1
            }
            Some(0) => return false,
            Some(index) => index - 1,
        };
        self.history_index = Some(next_index);
        self.text = self.history[next_index].clone();
        self.expansions.clear();
        self.last_edit = LastEdit::None;
        self.cursor = self.text.len();
        true
    }

    /// Walk forward through history, restoring the stashed draft at the end.
    pub fn history_next(&mut self) -> bool {
        let Some(index) = self.history_index else {
            return false;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            self.text = self.history[index + 1].clone();
            self.expansions.clear();
        } else {
            self.history_index = None;
            if let Some(stash) = self.stash.take() {
                self.restore_snapshot(stash);
            }
        }
        self.cursor = self.text.len();
        true
    }

    /// Attach an opaque file or delegation reference to the draft.
    pub fn attach_context(
        &mut self,
        start: usize,
        end: usize,
        label: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            start <= end && end <= self.text.len(),
            "Invalid context range"
        );
        let contexts = self
            .expansions
            .iter()
            .filter(|e| matches!(e.kind, ExpansionKind::Context(_)))
            .collect::<Vec<_>>();
        anyhow::ensure!(
            contexts.len() < 20
                && contexts.iter().map(|e| e.content.len()).sum::<usize>() + content.len()
                    <= 128 * 1024,
            "Context must fit within 128 KiB and 20 references"
        );
        let next = contexts
            .iter()
            .filter_map(|e| match e.kind {
                ExpansionKind::Context(id) => Some(id),
                ExpansionKind::Paste(_) => None,
            })
            .max()
            .unwrap_or(0)
            + 1;
        let placeholder = format!(
            "[context {}: {}]",
            next,
            label.replace(['\n', '\r', ']'], " ")
        );
        let start = self.edit(start, end, &format!("{placeholder} "), LastEdit::Other);
        self.expansions.push(Expansion {
            start,
            end: start + placeholder.len(),
            content: content.into(),
            kind: ExpansionKind::Context(next),
        });
        Ok(())
    }

    pub fn expanded_text(&self) -> String {
        let mut expansions = self.expansions.iter().collect::<Vec<_>>();
        expansions.sort_by_key(|e| e.start);
        let mut value = String::new();
        let mut offset = 0;
        for expansion in expansions {
            value.push_str(&self.text[offset..expansion.start]);
            value.push_str(&expansion.content);
            offset = expansion.end;
        }
        value.push_str(&self.text[offset..]);
        value
    }

    pub fn take(&mut self) -> String {
        let value = self.expanded_text();
        if !value.trim().is_empty() {
            self.pending_history = Some(value.clone());
        }
        if !value.trim().is_empty() && self.history.last() != Some(&value) {
            self.history.retain(|entry| entry != &value);
            self.history.push(value.clone());
            if self.history.len() > HISTORY_CAP {
                self.history.remove(0);
            }
        }
        self.clear();
        value
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.expansions.clear();
        self.undo.clear();
        self.redo.clear();
        self.history_index = None;
        self.stash = None;
        self.last_edit = LastEdit::None;
    }

    /// Replace the whole content (used by command completion).
    pub fn set_text(&mut self, value: String) {
        self.snapshot(LastEdit::Other);
        self.cursor = value.len();
        self.text = value;
        self.expansions.clear();
        self.history_index = None;
    }

    /// Replace a byte range (used by @-mention completion); the cursor lands
    /// after the replacement.
    pub fn replace_range(&mut self, start: usize, end: usize, replacement: &str) {
        self.edit(start, end, replacement, LastEdit::Other);
    }

    /// The active @-mention token under the cursor: (start byte of `@`,
    /// query after it). Only tokens that begin a word count.
    #[must_use]
    pub fn mention_query(&self) -> Option<(usize, String)> {
        let before = &self.text[..self.cursor];
        let start = before.rfind('@')?;
        if start > 0 {
            let previous = before[..start].chars().next_back()?;
            if !previous.is_whitespace() {
                return None;
            }
        }
        let query = &before[start + 1..];
        if query.contains(char::is_whitespace) {
            return None;
        }
        Some((start, query.to_owned()))
    }

    fn previous_boundary(&self) -> Option<usize> {
        if let Some(expansion) = self.expansions.iter().find(|e| e.end == self.cursor) {
            return Some(expansion.start);
        }
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map(|(index, _)| index)
    }
    fn next_boundary(&self) -> Option<usize> {
        if let Some(expansion) = self.expansions.iter().find(|e| e.start == self.cursor) {
            return Some(expansion.end);
        }
        self.text[self.cursor..]
            .graphemes(true)
            .next()
            .map(|g| self.cursor + g.len())
    }
    fn line_start(&self) -> usize {
        self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1)
    }
    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| self.cursor + index)
    }
    /// Byte boundary nearest the requested terminal-cell column, never inside
    /// a UTF-8 code point or a double-width character.
    fn line_boundary_at_column(&self, start: usize, end: usize, target: usize) -> usize {
        let mut column = 0;
        for (offset, grapheme) in self.text[start..end].grapheme_indices(true) {
            let next = column + grapheme.width();
            if next > target {
                return if target - column >= next - target {
                    start + offset + grapheme.len()
                } else {
                    start + offset
                };
            }
            column = next;
            if column == target {
                return start + offset + grapheme.len();
            }
        }
        end
    }
    fn word_left_boundary(&self) -> usize {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches(|character: char| !character.is_alphanumeric());
        let trimmed = trimmed.trim_end_matches(char::is_alphanumeric);
        trimmed.len()
    }
    fn word_right_boundary(&self) -> usize {
        let after = &self.text[self.cursor..];
        let skipped = after.len()
            - after
                .trim_start_matches(|character: char| !character.is_alphanumeric())
                .len();
        let word = after[skipped..].len()
            - after[skipped..]
                .trim_start_matches(char::is_alphanumeric)
                .len();
        self.cursor + skipped + word
    }
}

#[cfg(test)]
mod tests {
    use super::Composer;

    fn typed(text: &str) -> Composer {
        let mut composer = Composer::default();
        for character in text.chars() {
            composer.insert_char(character);
        }
        composer
    }

    #[test]
    fn cursor_moves_and_inserts_mid_text() {
        let mut composer = typed("hello");
        composer.move_left();
        composer.move_left();
        composer.insert_char('X');
        assert_eq!(composer.text(), "helXlo");
        composer.move_home();
        composer.insert_char('>');
        assert_eq!(composer.text(), ">helXlo");
        composer.move_end();
        composer.insert_char('!');
        assert_eq!(composer.text(), ">helXlo!");
    }

    #[test]
    fn vertical_motion_keeps_unicode_boundaries_and_display_columns() {
        let mut composer = typed("é\nx");
        assert!(composer.move_up());
        composer.insert_char('!');
        assert_eq!(composer.text(), "é!\nx");

        let mut wide = typed("界a\nxy");
        wide.move_left();
        assert!(wide.move_up());
        assert_eq!(wide.cursor(), "界".len());
        wide.insert_char('!');
        assert_eq!(wide.text(), "界!a\nxy");
    }

    #[test]
    fn word_motion_and_word_deletion() {
        let mut composer = typed("cargo test --workspace");
        composer.delete_word_back();
        assert_eq!(composer.text(), "cargo test --");
        composer.delete_word_back();
        assert_eq!(composer.text(), "cargo ");
        composer.move_word_left();
        assert_eq!(composer.cursor(), 0);
        composer.move_word_right();
        assert_eq!(composer.cursor(), 5);
        composer.delete_word_forward();
        assert_eq!(composer.text(), "cargo");
    }

    #[test]
    fn kill_line_operations_are_line_scoped() {
        let mut composer = typed("first line");
        composer.insert_char('\n');
        for character in "second".chars() {
            composer.insert_char(character);
        }
        composer.move_home();
        composer.kill_to_end();
        assert_eq!(composer.text(), "first line\n");
        composer.undo();
        assert_eq!(composer.text(), "first line\nsecond");
        composer.move_end();
        composer.kill_to_start();
        assert_eq!(composer.text(), "first line\n");
    }

    #[test]
    fn undo_coalesces_typing_and_redo_restores() {
        let mut composer = typed("hello world");
        composer.undo();
        // "world" was one typing run after the space boundary.
        assert_eq!(composer.text(), "hello ");
        composer.undo();
        assert_eq!(composer.text(), "hello");
        composer.redo();
        assert_eq!(composer.text(), "hello ");
        composer.redo();
        assert_eq!(composer.text(), "hello world");
    }

    #[test]
    fn cursor_motion_starts_a_new_typing_undo_group() {
        let mut composer = typed("abc");
        composer.move_left();
        composer.insert_char('X');
        assert_eq!(composer.text(), "abXc");

        composer.undo();
        assert_eq!(composer.text(), "abc");
        assert_eq!(composer.cursor(), 2);

        composer.redo();
        assert_eq!(composer.text(), "abXc");
    }

    #[test]
    fn history_recall_stashes_the_draft() {
        let mut composer = Composer::with_history(vec!["one".into(), "two".into()]);
        for character in "draft".chars() {
            composer.insert_char(character);
        }
        assert!(composer.history_prev());
        assert_eq!(composer.text(), "two");
        assert!(composer.history_prev());
        assert_eq!(composer.text(), "one");
        assert!(!composer.history_prev());
        assert!(composer.history_next());
        assert_eq!(composer.text(), "two");
        assert!(composer.history_next());
        assert_eq!(composer.text(), "draft");
    }

    #[test]
    fn take_records_history_without_consecutive_duplicates() {
        let mut composer = typed("run it");
        assert_eq!(composer.take(), "run it");
        let mut again = Composer::with_history(composer.history().to_vec());
        for character in "run it".chars() {
            again.insert_char(character);
        }
        again.take();
        assert_eq!(again.history().len(), 1);
    }

    #[test]
    fn large_pastes_collapse_and_expand_on_take() {
        let mut composer = typed("see: ");
        let mut blob = String::new();
        for line in 1..=20 {
            use std::fmt::Write as _;
            let _ = writeln!(blob, "line {line}");
        }
        composer.paste(&blob);
        assert!(
            composer.text().contains("[#1 pasted 20 lines]"),
            "{}",
            composer.text()
        );
        assert!(composer.text().len() < blob.len());
        let submitted = composer.take();
        assert!(submitted.starts_with("see: line 1"));
        assert!(submitted.contains("line 20"));
    }

    #[test]
    fn undo_and_redo_restore_collapsed_paste_payloads() {
        let mut first = String::new();
        let mut second = String::new();
        for line in 1..=10 {
            use std::fmt::Write as _;
            let _ = writeln!(first, "first {line}");
            let _ = writeln!(second, "second {line}");
        }

        let mut composer = Composer::default();
        composer.paste(&first);
        composer.undo();
        assert_eq!(composer.text(), "");

        composer.paste(&second);
        assert_eq!(composer.text(), "[#1 pasted 10 lines]");
        composer.undo();
        assert_eq!(composer.text(), "");
        composer.redo();
        assert_eq!(composer.text(), "[#1 pasted 10 lines]");
        assert_eq!(composer.take(), second);
    }

    #[test]
    fn small_pastes_insert_verbatim() {
        let mut composer = Composer::default();
        composer.paste("just words");
        assert_eq!(composer.text(), "just words");
    }

    #[test]
    fn bracketed_paste_carriage_returns_normalize_to_newlines() {
        let mut composer = Composer::default();
        let mut blob = String::new();
        for line in 1..=20 {
            use std::fmt::Write as _;
            let _ = write!(blob, "cr-line-{line}\r");
        }
        composer.paste(&blob);
        assert!(
            composer.text().contains("pasted 20 lines"),
            "{}",
            composer.text()
        );
        assert!(composer.take().contains("cr-line-20\n"));
    }

    #[test]
    fn multiline_cursor_navigation() {
        let mut composer = typed("alpha");
        composer.insert_char('\n');
        for character in "beta long".chars() {
            composer.insert_char(character);
        }
        assert!(composer.move_up());
        // Column clamps to the shorter first line.
        assert!(composer.cursor() <= 5);
        assert!(!composer.move_up(), "top line falls through to history");
        assert!(composer.move_down());
        assert!(!composer.move_down(), "bottom line is terminal");
    }

    #[test]
    fn utf8_boundaries_are_respected() {
        let mut composer = typed("héllo");
        composer.move_left();
        composer.move_left();
        composer.move_left();
        composer.move_left();
        composer.backspace();
        assert_eq!(composer.text(), "éllo");
        let mut wide = typed("日本語");
        wide.move_left();
        wide.backspace();
        assert_eq!(wide.text(), "日語");
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;

    #[test]
    fn every_partial_paste_edit_removes_a_complete_chip_and_undo_restores_the_payload() {
        let content = "line\n".repeat(12);
        let edits: [fn(&mut Composer); 6] = [
            Composer::backspace,
            Composer::delete_word_back,
            Composer::kill_to_start,
            |c| {
                c.move_home();
                c.delete();
            },
            |c| {
                c.move_home();
                c.delete_word_forward();
            },
            |c| {
                c.move_home();
                c.kill_to_end();
            },
        ];
        for edit in edits {
            let mut c = Composer::default();
            c.paste(&content);
            edit(&mut c);
            assert_eq!(c.text(), "");
            c.undo();
            assert_eq!(c.expanded_text(), content);
            c.redo();
            assert_eq!(c.take(), "");
        }
    }

    #[test]
    fn expansions_use_original_ranges_and_never_reinterpret_literal_marker_text() {
        let first = format!("FIRST [#2 pasted 1 lines] {}", "a".repeat(1100));
        let second = "b".repeat(1100);
        let mut c = Composer::default();
        c.insert_str("literal [#1 pasted 1 lines] ");
        c.paste(&first);
        c.insert_str(" | ");
        c.paste(&second);
        c.move_home();
        c.insert_str("PREFIX ");
        assert_eq!(
            c.expanded_text(),
            format!("PREFIX literal [#1 pasted 1 lines] {first} | {second}")
        );
        assert_eq!(
            c.take(),
            format!("PREFIX literal [#1 pasted 1 lines] {first} | {second}")
        );
    }

    #[test]
    fn history_and_context_payloads_keep_their_own_expansion_ranges() {
        let mut c = Composer::with_history(vec!["literal [#1 pasted 1 lines]".into()]);
        c.paste(&"x".repeat(1100));
        let original = c.expanded_text();
        c.history_prev();
        assert_eq!(c.expanded_text(), "literal [#1 pasted 1 lines]");
        c.history_next();
        assert_eq!(c.expanded_text(), original);
        c.move_end();
        c.insert_str(" @file");
        let end = c.cursor();
        c.attach_context(end - 5, end, "file", "literal [#1 pasted 1 lines]")
            .unwrap();
        assert_eq!(c.take(), format!("{original} literal [#1 pasted 1 lines] "));
    }

    #[test]
    fn a_paste_before_a_leading_combining_mark_keeps_exact_payload_boundaries() {
        let mut c = Composer::default();
        c.insert_str("\u{301}suffix");
        c.move_home();
        let payload = "x".repeat(1100);
        c.paste(&payload);
        assert_eq!(c.take(), format!("{payload}\u{301}suffix"));
    }

    #[test]
    fn caret_and_deletion_keep_unicode_graphemes_whole() {
        for grapheme in ["e\u{301}", "👍🏽", "🇯🇵", "👨‍👩‍👧‍👦"] {
            let mut c = Composer::default();
            c.insert_str(&format!("A{grapheme}"));
            c.move_left();
            assert_eq!(c.cursor(), 1);
            c.delete();
            assert_eq!(c.text(), "A");
            c.undo();
            c.move_end();
            c.backspace();
            assert_eq!(c.text(), "A");
        }
    }

    #[test]
    fn insertion_next_to_a_chip_and_middle_range_replacement_keep_surrounding_text() {
        let mut c = Composer::default();
        c.insert_str("before ");
        c.paste(&"x".repeat(1100));
        c.insert_str(" after");
        c.replace_range(10, 12, "replacement");
        assert_eq!(c.take(), "before replacement after");
    }
}
