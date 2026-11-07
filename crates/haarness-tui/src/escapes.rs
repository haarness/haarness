//! Text a command, a tool or a server wrote, as the TUI draws it.
//!
//! Such text may hold terminal escape sequences (colours, cursor moves, a
//! window title) and other control characters. The renderer never hands
//! them to the terminal, but what follows the escape byte would show as
//! `[31m` or `]0;title` fragments, so output loses its sequences whole
//! ([`without_escapes`], shared with the server and the CLI through
//! `haarness_protocol`). An approval card is read for what will run, so
//! nothing on it may be hidden or reordered: there the invisible characters
//! show as codes such as `\u{1B}` instead ([`spelled_out`], [`marked`]).
//! Neither changes the text itself: what is stored, and what the model
//! reads, stay as written.

use haarness_opentui::{Color, Span};
pub(crate) use haarness_protocol::without_escapes;

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
pub(crate) fn spelled_out(text: &str) -> String {
    with_codes(text, shows_as_code, push_code)
}

/// Open and close a code in [`marked`] text. Both are format characters
/// that take no cell wherever text is measured or wrapped. The text's own
/// are never left in it: [`marked`] writes them as codes, and
/// [`marked_directions`] leaves them out.
const CODE_START: char = '\u{2061}';
const CODE_END: char = '\u{2062}';

/// [`spelled_out`] with each code between two markers, so every row the text
/// wraps onto can draw the codes quieter than the text around them
/// ([`marked_spans`]).
#[must_use]
pub(crate) fn marked(text: &str) -> String {
    with_codes(text, shows_as_code, push_marked)
}

/// [`marked`] for the lines of a file an edit changes: only what
/// [`turns_direction`] names is a code there. A function application or
/// invisible times of the file's own (U+2061, U+2062, as a formula copied
/// from a word processor holds them) shows nothing either way, so it is
/// left out rather than read as a marker.
#[must_use]
pub(crate) fn marked_directions(text: &str) -> String {
    with_codes(
        text,
        |character| turns_direction(character) || matches!(character, CODE_START | CODE_END),
        |shown, character| {
            if turns_direction(character) {
                push_marked(shown, character);
            }
        },
    )
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
            assert_eq!(spelled_out(text), text);
            assert_eq!(marked(text), text);
        }
    }

    #[test]
    fn approval_text_shows_what_is_invisible_as_a_code() {
        assert_eq!(
            spelled_out("cat report\u{202e}fdp.sh"),
            "cat report\\u{202E}fdp.sh"
        );
        assert_eq!(spelled_out("cd dist\r\nrm -r *"), "cd dist\\u{D}\nrm -r *");
        assert_eq!(spelled_out("x\u{1b}[1Ay"), "x\\u{1B}[1Ay");
        assert_eq!(
            spelled_out("p\u{200b}q\u{2067}r\u{feff}s\u{e0070}"),
            "p\\u{200B}q\\u{2067}r\\u{FEFF}s\\u{E0070}"
        );
        assert_eq!(spelled_out("a\u{2029}b\u{ad}c"), "a\\u{2029}b\\u{AD}c");
        // Tags that spell no flag, or more than one, show.
        assert_eq!(
            spelled_out("🏴\u{e0061}\u{e007f}"),
            "🏴\\u{E0061}\\u{E007F}"
        );
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
        // A formula's invisible function application is no marker: the
        // `x` after it is not drawn as a code.
        let formula = marked_directions("y = sin\u{2061}x");
        let mut in_code = false;
        let spans = marked_spans(
            &formula,
            Color::rgb(1, 1, 1),
            Color::rgb(2, 2, 2),
            Color::rgb(3, 3, 3),
            &mut in_code,
        );
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(spans[0].text, "y = sinx");
        assert!(!in_code);
    }

    #[test]
    fn marked_codes_draw_quieter_also_across_a_wrap() {
        let text = marked("ls\u{2066}x");
        // The markers take no cell, so the card wraps the codes as shown.
        let shown = spelled_out("ls\u{2066}x");
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
