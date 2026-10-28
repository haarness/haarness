//! Text a command, a tool or a server wrote, as the TUI shows it.
//!
//! Such text can carry terminal escape sequences (colours, cursor moves, a
//! window title) and other control characters. The renderer never hands
//! them to the terminal, but what follows the escape byte would show as
//! `[31m` or `]0;title` fragments, so output loses its sequences whole
//! ([`without_escapes`]). The text itself is never changed: what is
//! stored, and what the model reads, stay as written.

use std::borrow::Cow;

const ESC: char = '\u{1b}';
const BEL: char = '\u{7}';
/// The C1 forms of CSI and of the string terminator.
const C1_CSI: char = '\u{9b}';
const C1_ST: char = '\u{9c}';

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

/// `text` without its escape sequences and stray control characters: CSI
/// (`ESC [ … m`), OSC (`ESC ] … BEL`), DCS, SOS, PM and APC strings, the
/// two- and three-character escapes (`ESC 7`, `ESC ( B`) and the C1 forms
/// of all of them. Tab, line feed and carriage return stay.
///
/// Removing a sequence can complete another around it (`ESC ESC[22m[32m`),
/// and that one goes too, as removing them over and over until none is left
/// would: an unfinished sequence waits on a stack while the ones inside it
/// complete. A string that never ends stops at the end of its line, so the
/// lines after it still show. Linear in the length of `text`.
pub(crate) fn without_escapes(text: &str) -> Cow<'_, str> {
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
            let waiting = match character {
                ESC => Waiting::Introducer,
                C1_CSI => Waiting::Csi,
                // OSC, DCS, SOS, PM and APC.
                '\u{9d}' | '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => {
                    Waiting::Text { escape: false }
                }
                _ => {
                    kept.push(character);
                    return;
                }
            };
            return begin(kept, pending, waiting, character);
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
                BEL | C1_ST => return finish(kept, pending),
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
            ("\u{9b}1mc1\u{9b}22m", "c1"),
            ("\u{9d}2;name\u{9c}c1 name", "c1 name"),
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

    /// What people write reads exactly as before: emoji and their
    /// sequences, CJK, right-to-left scripts and their marks, combining
    /// marks.
    #[test]
    fn ordinary_text_is_left_as_it_is() {
        for text in [
            "👩\u{200d}🔬 ❤\u{fe0f} 🇯🇵 👍🏽 🏴\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}",
            "日本語のテキスト 中文 한국어",
            "שלום עולם مرحبا بالعالم",
            "ن\u{200c}ه e\u{301}\u{302} 葛\u{e0100}",
            "۝١٢ \u{600}١٠",
            "a\tb\nc",
        ] {
            assert_eq!(without_escapes(text), text);
        }
    }
}
