//! The terminal modes a program set in output that has since left the replay
//! buffer, and where a replay that starts inside that buffer can cleanly begin.
//!
//! A full-screen program (vim, htop, tmux) switches to the alternate screen,
//! turns on mouse reporting, application cursor keys and bracketed paste once,
//! as it starts. After two megabytes of its redraws those switches are no
//! longer in the buffer, so a client that replays it (a view shown again, a
//! reconnect) would draw the program on the main screen and send it keys and
//! clicks it does not expect. The buffer keeps a `Modes` fed with every byte it
//! drops and hands a truncated replay the switches still in effect.

use std::fmt::Write as _;

/// How far past the first point outside a sequence a truncated replay looks
/// for the start of a whole line.
pub(crate) const CLEAN_START_LIMIT: usize = 64 * 1024;
/// The parameters of a sequence that count; later ones are ignored, as
/// terminals do (vte keeps 32).
const PARAMS_LIMIT: usize = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    /// Text and control characters.
    #[default]
    Ground,
    /// After ESC.
    Escape,
    /// After ESC and an intermediate byte, until the final byte.
    EscapeIntermediate,
    /// Inside `ESC [`, until the final byte.
    Csi,
    /// Inside an OSC, DCS, APC, PM or SOS string, until BEL or `ESC \`.
    String,
    /// ESC inside such a string: `\` ends it, anything else starts a sequence.
    StringEscape,
}

// A terminal's modes are flags.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Modes {
    state: State,
    /// The last byte was a line break outside any sequence.
    line_start: bool,
    /// The CSI parameters so far, at most `PARAMS_LIMIT`; the last one is
    /// being read. An empty one is 0, and one with sub-parameters (`:`),
    /// which no mode has, is `None`.
    params: Vec<Option<u16>>,
    /// More parameters came than `PARAMS_LIMIT`.
    full: bool,
    /// The marker before the parameters (`?`, `>`, `<`, `=`).
    marker: Option<u8>,
    /// An intermediate byte, or a marker after a parameter: not a mode change.
    ignored: bool,
    /// The mode (47, 1047 or 1049) that switched to the alternate screen.
    alternate: Option<u16>,
    application_cursor: bool,
    application_keypad: bool,
    no_autowrap: bool,
    hidden_cursor: bool,
    insert: bool,
    /// The mouse tracking mode on: 9, 1000, 1002 or 1003.
    mouse: Option<u16>,
    /// The mouse report encoding: 1005, 1006, 1015 or 1016.
    mouse_encoding: Option<u16>,
    focus_reports: bool,
    bracketed_paste: bool,
}

impl Modes {
    pub(crate) fn feed(&mut self, bytes: impl IntoIterator<Item = u8>) {
        for byte in bytes {
            self.byte(byte);
        }
    }

    fn byte(&mut self, byte: u8) {
        self.step(byte);
        self.line_start = byte == b'\n' && self.state == State::Ground;
    }

    fn step(&mut self, byte: u8) {
        match self.state {
            State::Ground => {
                if byte == 0x1b {
                    self.state = State::Escape;
                }
            }
            State::Escape => self.escape(byte),
            State::EscapeIntermediate => match byte {
                0x1b => self.state = State::Escape,
                0x18 | 0x1a | 0x30..=0x7e => self.state = State::Ground,
                _ => {}
            },
            State::Csi => match byte {
                0x1b => self.state = State::Escape,
                0x18 | 0x1a => self.state = State::Ground,
                0x3c..=0x3f if self.params.is_empty() && self.marker.is_none() => {
                    self.marker = Some(byte);
                }
                0x30..=0x3b => self.param(byte),
                0x3c..=0x3f | 0x20..=0x2f => self.ignored = true,
                0x40..=0x7e => {
                    self.dispatch(byte);
                    self.state = State::Ground;
                }
                // Control characters run inside a sequence without ending it.
                _ => {}
            },
            State::String => match byte {
                0x07 | 0x18 | 0x1a => self.state = State::Ground,
                0x1b => self.state = State::StringEscape,
                _ => {}
            },
            State::StringEscape => {
                if byte == b'\\' {
                    self.state = State::Ground;
                } else {
                    self.escape(byte);
                }
            }
        }
    }

    fn escape(&mut self, byte: u8) {
        self.state = match byte {
            b'[' => {
                self.params.clear();
                self.full = false;
                self.marker = None;
                self.ignored = false;
                State::Csi
            }
            b']' | b'P' | b'_' | b'^' | b'X' => State::String,
            b'c' => {
                *self = Self::default();
                State::Ground
            }
            b'=' => {
                self.application_keypad = true;
                State::Ground
            }
            b'>' => {
                self.application_keypad = false;
                State::Ground
            }
            0x20..=0x2f => State::EscapeIntermediate,
            // Another ESC starts over; other control characters run inside
            // a sequence without ending it.
            0x00..=0x17 | 0x19 | 0x1b..=0x1f => State::Escape,
            _ => State::Ground,
        };
    }

    /// A digit, `:` or `;` of a CSI sequence's parameters.
    fn param(&mut self, byte: u8) {
        if self.params.is_empty() {
            self.params.push(Some(0));
        }
        if byte == b';' {
            if self.params.len() < PARAMS_LIMIT {
                self.params.push(Some(0));
            } else {
                self.full = true;
            }
            return;
        }
        if self.full {
            return;
        }
        let current = self.params.last_mut().expect("a parameter is being read");
        *current = match (byte, *current) {
            (b':', _) | (_, None) => None,
            (digit, Some(value)) => Some(
                value
                    .saturating_mul(10)
                    .saturating_add(u16::from(digit - b'0')),
            ),
        };
    }

    /// A CSI sequence ended with `last`: `h` sets and `l` resets the modes it
    /// lists, private (`?`) or ANSI.
    fn dispatch(&mut self, last: u8) {
        let set = match last {
            b'h' => true,
            b'l' => false,
            _ => return,
        };
        if self.ignored {
            return;
        }
        let params = std::mem::take(&mut self.params);
        for mode in params.iter().copied().flatten() {
            match self.marker {
                Some(b'?') => self.private(mode, set),
                None if mode == 4 => self.insert = set,
                _ => {}
            }
        }
        self.params = params;
    }

    fn private(&mut self, mode: u16, set: bool) {
        match mode {
            1 => self.application_cursor = set,
            7 => self.no_autowrap = !set,
            25 => self.hidden_cursor = !set,
            66 => self.application_keypad = set,
            47 | 1047 | 1049 => self.alternate = set.then_some(mode),
            // Turning any tracking mode off turns tracking off, as xterm does.
            9 | 1000 | 1002 | 1003 => self.mouse = set.then_some(mode),
            1005 | 1006 | 1015 | 1016 => {
                if set {
                    self.mouse_encoding = Some(mode);
                } else if self.mouse_encoding == Some(mode) {
                    self.mouse_encoding = None;
                }
            }
            1004 => self.focus_reports = set,
            2004 => self.bracketed_paste = set,
            _ => {}
        }
    }

    /// The sequences that put a freshly reset terminal into these modes;
    /// empty when they are all at their defaults. The alternate screen comes
    /// first, since switching to it is what the rest of the replay draws on.
    pub(crate) fn preamble(&self) -> String {
        let mut out = String::new();
        if let Some(mode) = self.alternate {
            let _ = write!(out, "\x1b[?{mode}h");
        }
        if self.application_cursor {
            out.push_str("\x1b[?1h");
        }
        if self.application_keypad {
            out.push_str("\x1b=");
        }
        if self.no_autowrap {
            out.push_str("\x1b[?7l");
        }
        if self.hidden_cursor {
            out.push_str("\x1b[?25l");
        }
        if self.insert {
            out.push_str("\x1b[4h");
        }
        if let Some(mode) = self.mouse {
            let _ = write!(out, "\x1b[?{mode}h");
        }
        if let Some(mode) = self.mouse_encoding {
            let _ = write!(out, "\x1b[?{mode}h");
        }
        if self.focus_reports {
            out.push_str("\x1b[?1004h");
        }
        if self.bracketed_paste {
            out.push_str("\x1b[?2004h");
        }
        out
    }

    /// Where a replay of `bytes`, which follow the output these modes were
    /// fed, should start so that it begins neither inside an escape sequence
    /// nor inside a character, and the modes in effect at that point. Output
    /// cut just after a line break starts where it is. Otherwise the start
    /// of the first whole line is preferred, within `CLEAN_START_LIMIT` bytes
    /// of the first character boundary outside a sequence, else that
    /// boundary. A string however long (an image, a clipboard copy) is
    /// skipped to its end: its payload is not text. Output that is all
    /// inside one gives nothing to replay.
    pub(crate) fn clean_start(&self, bytes: impl IntoIterator<Item = u8>) -> (usize, Self) {
        if self.line_start {
            return (0, self.clone());
        }
        let mut modes = self.clone();
        let mut boundary: Option<(usize, Self)> = None;
        let mut count = 0;
        for (index, byte) in bytes.into_iter().enumerate() {
            count = index + 1;
            if boundary.is_none() && modes.state == State::Ground && !continuation(byte) {
                boundary = Some((index, modes.clone()));
            }
            if boundary
                .as_ref()
                .is_some_and(|(at, _)| index >= at + CLEAN_START_LIMIT)
            {
                break;
            }
            modes.byte(byte);
            if byte == b'\n' && modes.state == State::Ground {
                return (index + 1, modes);
            }
        }
        boundary.unwrap_or((count, modes))
    }
}

/// A byte that continues a UTF-8 character rather than starting one.
fn continuation(byte: u8) -> bool {
    byte & 0xc0 == 0x80
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fed(bytes: &[u8]) -> Modes {
        let mut modes = Modes::default();
        modes.feed(bytes.iter().copied());
        modes
    }

    #[test]
    fn a_full_screen_programs_modes_are_restored_in_order() {
        let modes = fed(
            b"\x1b[?1049h\x1b[22;0;0t\x1b[?1h\x1b=\x1b[?25l\x1b[?1000;1006h\x1b[?2004h\x1b[Hhello",
        );
        assert_eq!(
            modes.preamble(),
            "\x1b[?1049h\x1b[?1h\x1b=\x1b[?25l\x1b[?1000h\x1b[?1006h\x1b[?2004h"
        );
        let mut quit = modes.clone();
        quit.feed(*b"\x1b[?1000l\x1b[?1006l\x1b[?25h\x1b[?1l\x1b>\x1b[?1049l");
        assert_eq!(quit.preamble(), "\x1b[?2004h");
        assert_eq!(fed(b"plain text\r\n").preamble(), "");
    }

    #[test]
    fn sequences_split_across_feeds_still_count() {
        let mut modes = Modes::default();
        for byte in b"\x1b[?2004h\x1b[?1049h" {
            modes.feed([*byte]);
        }
        assert_eq!(modes.preamble(), "\x1b[?1049h\x1b[?2004h");
    }

    #[test]
    fn text_inside_strings_and_other_sequences_is_not_a_mode_change() {
        // A window title naming a mode, a key-modifier request (`>`), an
        // intermediate byte and a marker after a parameter change nothing.
        let modes = fed(b"\x1b]0;[?1049h title\x07\x1bP+q[?2004h\x1b\\\x1b[>1h\x1b[?1$h\x1b[1?1h");
        assert_eq!(modes.preamble(), "");
        // ESC inside a string ends it and starts a sequence of its own, as
        // in a terminal.
        assert_eq!(fed(b"\x1b]0;title\x1b[?1h").preamble(), "\x1b[?1h");
    }

    #[test]
    fn a_long_mode_list_applies_its_first_32_modes() {
        let on = b"\x1b[?1049h\x1b[?1000h\x1b[?2004h";
        // A program's exit turns everything off in one long list.
        let mut list = b"\x1b[?1000".to_vec();
        for _ in 0..20 {
            list.extend(b";1");
        }
        list.extend(b";1049;2004l");
        let mut modes = fed(on);
        modes.feed(list);
        assert_eq!(modes.preamble(), "");
        // Past the 32nd, as terminals do, parameters are ignored.
        let mut list = b"\x1b[?1000".to_vec();
        for _ in 0..30 {
            list.extend(b";;");
        }
        list.extend(b";1049;2004l");
        let mut modes = fed(on);
        modes.feed(list);
        assert_eq!(modes.preamble(), "\x1b[?1049h\x1b[?2004h");
        // Sub-parameters and overlong numbers are no mode.
        assert_eq!(fed(b"\x1b[?1:1h\x1b[?99999999h").preamble(), "");
    }

    #[test]
    fn a_reset_clears_every_mode_and_tracking_off_turns_any_mode_off() {
        assert_eq!(fed(b"\x1b[?1049h\x1b[?1h\x1b[4h\x1bc").preamble(), "");
        assert_eq!(fed(b"\x1b[?1002h\x1b[?1000l").preamble(), "");
        assert_eq!(fed(b"\x1b[?1006h\x1b[?1015l").preamble(), "\x1b[?1006h");
        assert_eq!(fed(b"\x1b[?7l\x1b[4h").preamble(), "\x1b[?7l\x1b[4h");
    }

    #[test]
    fn a_clean_start_is_the_next_whole_line_outside_a_sequence() {
        // Cut inside `ESC [ 3 1 m`: the start skips to the next line and
        // keeps the mode set on the way.
        let mut modes = Modes::default();
        modes.feed(*b"\x1b[3");
        let (start, at) = modes.clean_start(*b"1mred\x1b[?2004h\nnext line");
        assert_eq!(start, 14);
        assert_eq!(at.preamble(), "\x1b[?2004h");
        assert_eq!(at.state, State::Ground);

        // A line break inside a title does not count.
        let (start, _) = Modes::default().clean_start(*b"\x1b]0;a\nb\x07c\nd");
        assert_eq!(start, 10);
    }

    #[test]
    fn without_a_line_break_the_start_is_the_next_character_outside_a_sequence() {
        // A cut inside 日 (E6 97 A5): the start is the next character.
        let (start, at) = Modes::default().clean_start([0x97, 0xa5, b'x', b'y']);
        assert_eq!(start, 2);
        assert_eq!(at, Modes::default());
        // A cut inside a cursor move followed by a cut character.
        let mut modes = Modes::default();
        modes.feed(*b"\x1b[1;");
        let (start, at) = modes.clean_start([b'2', b'H', 0x97, 0xa5, b'z']);
        assert_eq!(start, 4);
        assert_eq!(at.state, State::Ground);
    }

    #[test]
    fn a_long_string_cut_in_the_middle_is_skipped_to_its_end() {
        // An image or a clipboard copy (OSC 52) longer than the search for a
        // line: its payload is never replayed as text.
        let mut modes = Modes::default();
        modes.feed(*b"\x1b]52;c;");
        let mut bytes = vec![b'Q'; CLEAN_START_LIMIT * 2];
        bytes.extend(b"\x07\x1b[?2004hprompt\nnext");
        let (start, at) = modes.clean_start(bytes.iter().copied());
        assert_eq!(&bytes[start..], b"next");
        assert_eq!(at.preamble(), "\x1b[?2004h");
        // All of it inside one string: nothing to replay.
        let (start, _) = modes.clean_start(std::iter::repeat_n(b'Q', CLEAN_START_LIMIT + 10));
        assert_eq!(start, CLEAN_START_LIMIT + 10);
    }

    #[test]
    fn output_cut_just_after_a_line_break_starts_where_it_is() {
        let mut modes = Modes::default();
        modes.feed(*b"\x1b[?2004hold line\r\n");
        let (start, at) = modes.clean_start(*b"kept line\r\nnext");
        assert_eq!(start, 0);
        assert_eq!(at.preamble(), "\x1b[?2004h");
        // A line break inside a string is not one.
        let mut modes = Modes::default();
        modes.feed(*b"\x1b]0;a\n");
        assert_eq!(modes.clean_start(*b"b\x07c\nd").0, 4);
    }
}
