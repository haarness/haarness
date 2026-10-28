//! Text a command, a tool or a server wrote, as the TUI draws it.
//!
//! Such text may hold terminal escape sequences (colours, cursor moves, a
//! window title) and other control characters. The renderer never hands
//! them to the terminal, but what follows the escape byte would show as
//! `[31m` or `]0;title` fragments, so output loses its sequences whole
//! ([`without_escapes`]). An approval card is read for what will run, so
//! nothing on it may be hidden or reordered: there the invisible characters
//! show as codes such as `\u{1B}` instead ([`literal`], [`marked`]).
//! Neither changes the text itself: what is stored, and what the model
//! reads, stay as written.

use std::borrow::Cow;

use haarness_opentui::{Color, Span};

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
/// nothing and goes on its own: a program's 8-bit control bytes reach the
/// TUI as U+FFFD, so a C1 code point here is almost always text decoded
/// twice (`â\u{80}\u{9c}` for an opening quote), and the line around it
/// shows.
///
/// Removing a sequence can complete another around it (`ESC ESC[22m[32m`),
/// and that one goes too, as removing them over and over until none is left
/// would: an unfinished sequence waits on a stack while the ones inside it
/// complete. A string that never ends stops where its line does, and the
/// lines below it still show. Linear in the length of `text`.
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

/// Whether an approval card shows `character` as a code. That is any
/// control character other than tab and line feed (a carriage return
/// always, also at the end of a line, where a shell keeps it in the word
/// before it: `cd dist` then CR LF goes into a folder named `dist` and a
/// carriage return), an invisible format character or another
/// default-ignorable one, such as the direction marks, overrides and
/// isolates that reorder what is shown and the zero-width spaces that hide
/// a difference, and the line and paragraph separators. Left as they are:
/// the joiners U+200C and U+200D and the variation selectors
/// U+FE00–U+FE0F and U+E0100–U+E01EF, without which emoji sequences and
/// many writing systems would not join or pick their glyphs, and the format
/// characters that draw a visible sign (U+0600–U+0605, U+06DD).
#[must_use]
fn shows_as_code(character: char) -> bool {
    if character.is_control() {
        return !matches!(character, '\t' | '\n');
    }
    matches!(
        u32::from(character),
        0xad | 0x34f
            | 0x61c
            | 0x115f..=0x1160
            | 0x17b4..=0x17b5
            | 0x180b..=0x180f
            | 0x200b
            | 0x200e..=0x200f
            | 0x2028..=0x202e
            | 0x2060..=0x206f
            | 0x3164
            | 0xfeff
            | 0xffa0
            | 0xfff0..=0xfffb
            | 0x13430..=0x1343f
            | 0x1bca0..=0x1bca3
            | 0x1d173..=0x1d17a
            | 0xe0000..=0xe00ff
            | 0xe01f0..=0xe0fff
    )
}

/// The flags of England, Scotland and Wales are a black flag followed by
/// tag characters that spell the region and a cancel tag. Those tags draw
/// the flag, so they stay; every other tag character is a code.
const BLACK_FLAG: char = '\u{1f3f4}';
const FLAG_TAGS: [&str; 3] = [
    "\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}",
    "\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}",
    "\u{e0067}\u{e0062}\u{e0077}\u{e006c}\u{e0073}\u{e007f}",
];

/// Whether `character` overrides or isolates the direction of the text
/// after it (U+202A–U+202E, U+2066–U+2069). In the lines of a file an edit
/// changes, only these are codes: they can make a changed line read as
/// something it is not, while marks and joiners there belong to the text.
fn turns_direction(character: char) -> bool {
    matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// `text` with each character `is_code` names handed to `code`, which
/// writes it; the rest is kept as it is.
fn with_codes(text: &str, is_code: fn(char) -> bool, code: impl Fn(&mut String, char)) -> String {
    if !text.chars().any(is_code) {
        return text.to_owned();
    }
    let mut shown = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(character) = rest.chars().next() {
        rest = &rest[character.len_utf8()..];
        if character == BLACK_FLAG
            && let Some(tags) = FLAG_TAGS.iter().find(|tags| rest.starts_with(**tags))
        {
            shown.push(character);
            shown.push_str(tags);
            rest = &rest[tags.len()..];
        } else if is_code(character) {
            code(&mut shown, character);
        } else {
            shown.push(character);
        }
    }
    shown
}

/// Writes `character` as a code a reader can see: `\u{1B}`.
fn push_code(shown: &mut String, character: char) {
    use std::fmt::Write as _;
    let _ = write!(shown, "\\u{{{:X}}}", u32::from(character));
}

/// `text` with each character [`shows_as_code`] names written as its code,
/// for a line of plain text such as the status band's.
#[must_use]
pub(crate) fn literal(text: &str) -> String {
    with_codes(text, shows_as_code, push_code)
}

/// Opens and closes a code in [`marked`] text. Both are format characters,
/// so `text` itself can no longer hold one there, and they take no cell
/// wherever text is measured or wrapped.
const CODE_START: char = '\u{2061}';
const CODE_END: char = '\u{2062}';

/// [`literal`] with each code between two markers, so every row the text
/// wraps onto can draw the codes quieter than the text around them
/// ([`marked_spans`]).
#[must_use]
pub(crate) fn marked(text: &str) -> String {
    with_codes(text, shows_as_code, push_marked)
}

/// [`marked`] for the lines of a file an edit changes: only what
/// [`turns_direction`] names is a code there.
#[must_use]
pub(crate) fn marked_directions(text: &str) -> String {
    with_codes(text, turns_direction, push_marked)
}

fn push_marked(shown: &mut String, character: char) {
    shown.push(CODE_START);
    push_code(shown, character);
    shown.push(CODE_END);
}

/// Whether a code is still open at the end of `row`, a row of [`marked`]
/// text that began inside one when `open`.
#[must_use]
pub(crate) fn code_open_after(row: &str, open: bool) -> bool {
    row.chars().fold(open, |open, character| match character {
        CODE_START => true,
        CODE_END => false,
        _ => open,
    })
}

/// One row of [`marked`] text as spans: the text in `fg`, its codes in
/// `quiet`, both on `bg`. `in_code` carries a code a wrap broke across rows
/// on to the next one.
pub(crate) fn marked_spans(
    row: &str,
    fg: Color,
    quiet: Color,
    bg: Color,
    in_code: &mut bool,
) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut run = String::new();
    let flush = |run: &mut String, code: bool, spans: &mut Vec<Span>| {
        if !run.is_empty() {
            spans.push(Span::new(std::mem::take(run), if code { quiet } else { fg }).on(bg));
        }
    };
    for character in row.chars() {
        match character {
            CODE_START => {
                flush(&mut run, *in_code, &mut spans);
                *in_code = true;
            }
            CODE_END => {
                flush(&mut run, *in_code, &mut spans);
                *in_code = false;
            }
            _ => run.push(character),
        }
    }
    flush(&mut run, *in_code, &mut spans);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::width::DisplayWidth as _;

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
            assert_eq!(literal(text), text);
            assert_eq!(marked(text), text);
        }
    }

    #[test]
    fn approval_text_shows_what_is_invisible_as_a_code() {
        assert_eq!(
            literal("cat report\u{202e}fdp.sh"),
            "cat report\\u{202E}fdp.sh"
        );
        assert_eq!(literal("cd dist\r\nrm -r *"), "cd dist\\u{D}\nrm -r *");
        assert_eq!(literal("x\u{1b}[1Ay"), "x\\u{1B}[1Ay");
        assert_eq!(
            literal("p\u{200b}q\u{2067}r\u{feff}s\u{e0070}"),
            "p\\u{200B}q\\u{2067}r\\u{FEFF}s\\u{E0070}"
        );
        assert_eq!(literal("a\u{2029}b\u{ad}c"), "a\\u{2029}b\\u{AD}c");
        // Tags that spell no flag, or more than one, show.
        assert_eq!(literal("🏴\u{e0061}\u{e007f}"), "🏴\\u{E0061}\\u{E007F}");
    }

    /// In an edit's lines only direction overrides and isolates are codes;
    /// right-to-left text and its marks read as written.
    #[test]
    fn an_edit_shows_only_what_turns_the_direction() {
        let line = "if (role != \"user\u{202e} \u{2066}// admin\u{2069} \u{2066}\") {";
        assert_eq!(
            marked_directions(line).replace([CODE_START, CODE_END], ""),
            "if (role != \"user\\u{202E} \\u{2066}// admin\\u{2069} \\u{2066}\") {"
        );
        for text in ["مرحبا\u{200f} بالعالم", "a\u{200b}b\tc", "שלום"] {
            assert_eq!(marked_directions(text), text);
        }
    }

    #[test]
    fn marked_codes_draw_quieter_also_across_a_wrap() {
        let text = marked("ls\u{2066}x");
        // The markers take no cell, so the card wraps the codes as shown.
        let shown = literal("ls\u{2066}x");
        assert_eq!(text.width(), shown.width());
        assert_eq!(
            textwrap::core::display_width(&text),
            textwrap::core::display_width(&shown)
        );
        let (fg, quiet, bg) = (
            Color::rgb(1, 1, 1),
            Color::rgb(2, 2, 2),
            Color::rgb(3, 3, 3),
        );
        let mut in_code = false;
        let spans = marked_spans(&text, fg, quiet, bg, &mut in_code);
        let parts: Vec<_> = spans
            .iter()
            .map(|span| (span.text.as_str(), span.fg == quiet))
            .collect();
        assert_eq!(parts, [("ls", false), ("\\u{2066}", true), ("x", false)]);
        // A code broken across two rows stays quiet on both.
        let cut = text.find("2066").unwrap();
        let mut in_code = false;
        let first = marked_spans(&text[..cut], fg, quiet, bg, &mut in_code);
        assert!(in_code);
        let second = marked_spans(&text[cut..], fg, quiet, bg, &mut in_code);
        assert_eq!(first.last().unwrap().text, "\\u{");
        assert!(first.last().unwrap().fg == quiet && second[0].fg == quiet);
        assert_eq!(second[0].text, "2066}");
        assert!(!in_code);
    }
}
