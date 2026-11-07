//! Text with terminal escape sequences and other control characters taken
//! out, for a client that shows text it did not write: a chat title, a
//! command's output, a terminal's name. Printed as it is, a sequence could
//! set the terminal's window title, write its clipboard or redraw the
//! screen; in a renderer that never passes one on, what follows the escape
//! byte would still show as `[31m` or `]0;title` fragments.

use std::borrow::Cow;

const ESC: char = '\u{1b}';
const BEL: char = '\u{7}';

/// What an escape sequence still waits for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Waiting {
    /// The byte after `ESC`.
    Introducer,
    /// `ESC` and intermediates (`ESC ( B`): more of them, or the final byte.
    Intermediate,
    /// `ESC [` or CSI: parameters and intermediates, or the final byte.
    Csi,
    /// An OSC, DCS, SOS, PM or APC string, up to BEL or ST; `escape` once
    /// its `ESC` has come and the `\` of ST may follow.
    Text { escape: bool },
}

struct Pending {
    /// Where the sequence starts in the text kept so far.
    start: usize,
    waiting: Waiting,
}

/// `text` with every escape sequence and every other control character
/// taken out: CSI (`ESC [ … m`), OSC (`ESC ] … BEL`), DCS, SOS, PM and APC
/// strings, and the two- and three-character escapes (`ESC 7`, `ESC ( B`).
/// Tab, line feed and carriage return stay. A C1 control (U+0080–U+009F) starts
/// nothing and goes on its own: a program's 8-bit control bytes reach a
/// client as U+FFFD, so a C1 code point here is almost always text decoded
/// twice (`â\u{80}\u{9c}` for an opening quote), and the line around it
/// shows. Gone, it cannot begin a sequence in a terminal either.
///
/// Removing a sequence can complete another around it (`ESC ESC[22m[32m`),
/// and that one goes too, as removing them over and over until none is left
/// would: an unfinished sequence waits on a stack while the ones inside it
/// complete. A string that never ends stops where its line does, and the
/// lines below it still show. Linear in the length of `text`.
#[must_use]
pub fn without_escapes(text: &str) -> Cow<'_, str> {
    if !text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\t' | '\n' | '\r'))
    {
        return Cow::Borrowed(text);
    }
    let mut kept = String::with_capacity(text.len());
    let mut pending: Vec<Pending> = Vec::new();
    for character in text.chars() {
        step(&mut kept, &mut pending, character);
    }
    // A string still open at the end ends with the text: it is dropped. An
    // unfinished escape is only its first characters, shown as written
    // below without the control character that began it.
    if let Some(open) = pending
        .last()
        .filter(|open| matches!(open.waiting, Waiting::Text { .. }))
    {
        kept.truncate(open.start);
    }
    kept.retain(|character| !character.is_control() || matches!(character, '\t' | '\n' | '\r'));
    Cow::Owned(kept)
}

/// Reads one more character of the text into `kept`.
fn step(kept: &mut String, pending: &mut Vec<Pending>, character: char) {
    loop {
        let Some(open) = pending.last_mut() else {
            if character == ESC {
                return begin(kept, pending, Waiting::Introducer, ESC);
            }
            kept.push(character);
            return;
        };
        let code = u32::from(character);
        match open.waiting {
            Waiting::Introducer | Waiting::Intermediate | Waiting::Csi if character == ESC => {
                // A sequence inside an unfinished one: once it is removed,
                // the outer one reads on from what follows it.
                return begin(kept, pending, Waiting::Introducer, ESC);
            }
            Waiting::Introducer => match character {
                '[' => open.waiting = Waiting::Csi,
                ']' | 'P' | 'X' | '^' | '_' => open.waiting = Waiting::Text { escape: false },
                ' '..='/' => open.waiting = Waiting::Intermediate,
                '0'..='~' => return finish(kept, pending),
                _ => {
                    // Not an escape after all: none of the unfinished ones
                    // under it can end now, since this one never will.
                    pending.clear();
                    continue;
                }
            },
            Waiting::Intermediate => match code {
                0x20..=0x2f => {}
                0x30..=0x7e => return finish(kept, pending),
                _ => {
                    pending.clear();
                    continue;
                }
            },
            Waiting::Csi => match code {
                0x20..=0x3f => {}
                0x40..=0x7e => return finish(kept, pending),
                _ => {
                    pending.clear();
                    continue;
                }
            },
            Waiting::Text { escape: true } => {
                if character == '\\' {
                    return finish(kept, pending);
                }
                // An escape that is not ST ends the string, as a terminal
                // reads it, and begins a sequence of its own.
                let start = open.start;
                pending.pop();
                kept.truncate(start);
                begin(kept, pending, Waiting::Introducer, ESC);
                continue;
            }
            Waiting::Text { escape: false } => match character {
                BEL => return finish(kept, pending),
                ESC => open.waiting = Waiting::Text { escape: true },
                '\n' => {
                    // Never ended on its line: drop it and keep the line
                    // break for whatever waits under it.
                    let start = open.start;
                    pending.pop();
                    kept.truncate(start);
                    continue;
                }
                _ => {}
            },
        }
        kept.push(character);
        return;
    }
}

/// Opens a sequence at `introducer`, which is kept with what follows it
/// until the sequence completes, so an unfinished one can still be shown.
fn begin(kept: &mut String, pending: &mut Vec<Pending>, waiting: Waiting, introducer: char) {
    pending.push(Pending {
        start: kept.len(),
        waiting,
    });
    kept.push(introducer);
}

fn finish(kept: &mut String, pending: &mut Vec<Pending>) {
    if let Some(done) = pending.pop() {
        kept.truncate(done.start);
    }
}

/// `title` as a chat stores it: escape sequences and other control
/// characters taken out ([`without_escapes`]), each run of tabs and line
/// breaks read as one space so the words on either side stay apart, and
/// the ends trimmed. Every other character stays as written. Empty when
/// nothing printable is left.
#[must_use]
pub fn clean_chat_title(title: &str) -> String {
    let text = without_escapes(title);
    let mut clean = String::with_capacity(text.len());
    let mut space = false;
    for character in text.chars() {
        if matches!(character, '\t' | '\n' | '\r') {
            space = true;
        } else {
            if space {
                clean.push(' ');
                space = false;
            }
            clean.push(character);
        }
    }
    clean.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_titles_and_cursor_moves_leave_only_their_text() {
        for (raw, shown) in [
            ("\u{1b}[32mok\u{1b}[39m", "ok"),
            ("\u{1b}[1;38;2;0;128;255mbold\u{1b}[m", "bold"),
            ("\u{1b}[?25l\u{1b}[2K\u{1b}[1Gready", "ready"),
            ("\u{1b}]0;build logs\u{7}after", "after"),
            ("\u{1b}]2;tab name\u{1b}\\after", "after"),
            (
                "\u{1b}]8;;https://example.org/docs\u{1b}\\docs\u{1b}]8;;\u{1b}\\ page",
                "docs page",
            ),
            ("\u{1b}Pq#1;2;9;9;9\u{1b}\\x", "x"),
            ("\u{1b}_hidden\u{1b}\\shown", "shown"),
            ("\u{1b}(B\u{1b}7\u{1b}8\u{1b}>plain", "plain"),
        ] {
            assert_eq!(without_escapes(raw), shown, "{raw:?}");
        }
    }

    #[test]
    fn a_sequence_that_removing_another_completes_goes_too() {
        assert_eq!(without_escapes("\u{1b}\u{1b}[22m[32mok"), "ok");
        assert_eq!(without_escapes("x\u{1b}[3\u{1b}[39m2my"), "xy");
        assert_eq!(without_escapes("\u{1b}\u{1b}]0;t\u{7}[1mz"), "z");
        let nested = "\u{1b}".repeat(500) + &"[m".repeat(500);
        assert_eq!(without_escapes(&nested), "");
    }

    #[test]
    fn tabs_line_breaks_and_text_after_a_broken_escape_stay() {
        assert_eq!(without_escapes("a\tb\r\nc\n"), "a\tb\r\nc\n");
        // A lone escape before a line break or a letter outside every form.
        assert_eq!(without_escapes("end\u{1b}\nnext"), "end\nnext");
        assert_eq!(without_escapes("ü\u{1b}ß"), "üß");
        // An unfinished CSI shows what came after its escape.
        assert_eq!(without_escapes("y\u{1b}[38;5"), "y[38;5");
        // A title that never ends hides only the rest of its own line.
        assert_eq!(without_escapes("a\u{1b}]0;name\nb"), "a\nb");
        assert_eq!(without_escapes("a\u{1b}]0;name"), "a");
        // Other control characters go; text with none is returned as is.
        assert_eq!(
            without_escapes("ring\u{7} zero\u{0} rub\u{7f}"),
            "ring zero rub"
        );
        assert!(matches!(without_escapes("plain\ttext"), Cow::Borrowed(_)));
    }

    /// Text decoded twice carries C1 code points: each goes on its own and
    /// starts nothing, so the rest of its line still shows.
    #[test]
    fn a_c1_control_goes_alone_and_hides_nothing() {
        for (raw, shown) in [
            (
                "She wrote â\u{80}\u{9c}soonâ\u{80}\u{9d} and stopped.",
                "She wrote âsoonâ and stopped.",
            ),
            ("Ð\u{9f}Ñ\u{80}Ð¸ the rest", "ÐÑÐ¸ the rest"),
            ("ç\u{9b}® list\nnext", "ç® list\nnext"),
            ("a\u{90}b\u{98}c\u{9e}d\u{9f}e\u{9c}f", "abcdef"),
            ("\u{9b}31mtext", "31mtext"),
        ] {
            assert_eq!(without_escapes(raw), shown, "{raw:?}");
        }
    }

    #[test]
    fn a_chat_title_keeps_its_words_without_what_would_drive_a_terminal() {
        for (raw, stored) in [
            (
                "\u{1b}]0;owned\u{7}\u{1b}]52;c;aGk=\u{7}Fix the parser",
                "Fix the parser",
            ),
            ("\u{1b}[1mBold\u{1b}[0m title", "Bold title"),
            ("\u{9b}31mtext", "31mtext"),
            ("Fix bug\nin parser\r\n", "Fix bug in parser"),
            ("  Plan\t\tthe   release  ", "Plan the   release"),
            ("ring\u{7}\u{0}\u{7f}\u{85}less", "ringless"),
            ("日本語 👩\u{200d}🔬 مرحبا", "日本語 👩\u{200d}🔬 مرحبا"),
        ] {
            assert_eq!(clean_chat_title(raw), stored, "{raw:?}");
        }
        assert_eq!(clean_chat_title("\u{1b}]0;owned\u{7}\u{1b}[2J"), "");
        assert_eq!(clean_chat_title(" \n\t"), "");
    }
}
