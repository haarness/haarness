//! Option with an arrow from macOS Terminal. Terminal.app, and iTerm2's
//! Terminal.app preset, send Option+Up as Escape followed by the arrow's own
//! sequence, `ESC ESC [ A` (tmux usually re-encodes it as `ESC [1;3A` before
//! the TUI sees it, but not always). crossterm reads that as an Escape press
//! and then `[` and `A` typed, so the key would arm an interrupt, reject a
//! focused approval and type into the prompt. When those keys arrived
//! together, in the same read, they are one Alt key again; so is an Escape
//! typed and followed by an arrow within one read.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::{FutureExt as _, StreamExt as _};

/// The longest sequence after the doubled Escape: `[1;16A`.
const LONGEST: usize = 6;

/// The terminal's events, with an Escape-prefixed key joined into one.
pub(crate) struct TerminalInput {
    stream: EventStream,
    /// Events already read and decoded, not handed out yet.
    ready: VecDeque<io::Result<Event>>,
}

impl TerminalInput {
    pub(crate) fn new() -> Self {
        Self {
            stream: EventStream::new(),
            ready: VecDeque::new(),
        }
    }

    /// The next event. Cancel safe: it only waits before taking anything.
    pub(crate) async fn next(&mut self) -> Option<io::Result<Event>> {
        let event = match self.ready.pop_front() {
            Some(event) => event,
            None => self.stream.next().await?,
        };
        if !escape(&event) {
            return Some(event);
        }
        // Several Option+arrows can come in one read (a held key, bunched
        // SSH packets, a slow frame), so this runs for every Escape, queued
        // or new. Keys from the same read are already decoded; ask first, so
        // a lone Escape never leaves the stream waiting on a waker nobody
        // holds.
        while self.ready.len() < LONGEST && crossterm::event::poll(Duration::ZERO).unwrap_or(false)
        {
            let Some(Some(next)) = self.stream.next().now_or_never() else {
                break;
            };
            let key = matches!(next, Ok(Event::Key(_)));
            self.ready.push_back(next);
            if !key {
                break;
            }
        }
        Some(join(event, &mut self.ready))
    }
}

fn escape(event: &io::Result<Event>) -> bool {
    matches!(event, Ok(Event::Key(key)) if bare_escape(key))
}

/// `event` as it is, or, when it is an Escape and the keys at the front of
/// `ready` (what came with it) spell a cursor key, that key with Alt, which
/// takes them out of `ready`.
fn join(event: io::Result<Event>, ready: &mut VecDeque<io::Result<Event>>) -> io::Result<Event> {
    if !escape(&event) {
        return event;
    }
    let keys = ready
        .iter()
        .map_while(|event| match event {
            Ok(Event::Key(key)) => Some(*key),
            _ => None,
        })
        .collect::<Vec<_>>();
    let Some((key, taken)) = escaped_key(&keys) else {
        return event;
    };
    ready.drain(..taken);
    Ok(Event::Key(key))
}

fn bare_escape(key: &KeyEvent) -> bool {
    key.code == KeyCode::Esc && key.modifiers.is_empty() && key.kind == KeyEventKind::Press
}

/// The Alt key that `keys`, read right after an Escape, spell, and how many of
/// them it takes: `[`, `A` is Alt+Up and `[`, `1`, `;`, `2`, `A` Alt+Shift+Up.
pub(crate) fn escaped_key(keys: &[KeyEvent]) -> Option<(KeyEvent, usize)> {
    let mut sequence = String::new();
    for (index, key) in keys.iter().take(LONGEST).enumerate() {
        let KeyCode::Char(character) = key.code else {
            return None;
        };
        if key.kind != KeyEventKind::Press || !KeyModifiers::SHIFT.contains(key.modifiers) {
            return None;
        }
        sequence.push(character);
        if let Some((code, modifiers)) = decode(&sequence) {
            return Some((
                KeyEvent::new(code, modifiers | KeyModifiers::ALT),
                index + 1,
            ));
        }
    }
    None
}

/// A complete cursor or editing key sequence after its Escape: `[A`, `OA`,
/// `[1;2A`, `[5~` or `[5;2~`.
fn decode(sequence: &str) -> Option<(KeyCode, KeyModifiers)> {
    let letter = |character| match character {
        'A' => Some(KeyCode::Up),
        'B' => Some(KeyCode::Down),
        'C' => Some(KeyCode::Right),
        'D' => Some(KeyCode::Left),
        'H' => Some(KeyCode::Home),
        'F' => Some(KeyCode::End),
        _ => None,
    };
    if let Some(rest) = sequence.strip_prefix('O') {
        let mut characters = rest.chars();
        let code = letter(characters.next()?)?;
        return characters
            .next()
            .is_none()
            .then_some((code, KeyModifiers::NONE));
    }
    let rest = sequence.strip_prefix('[')?;
    let last = rest.chars().last()?;
    let body = &rest[..rest.len() - last.len_utf8()];
    let (number, modifier) = body.split_once(';').unwrap_or((body, ""));
    let modifiers = if modifier.is_empty() {
        KeyModifiers::NONE
    } else {
        modifiers(modifier.parse().ok()?)?
    };
    let code = if last == '~' {
        match number {
            "1" => KeyCode::Home,
            "2" => KeyCode::Insert,
            "3" => KeyCode::Delete,
            "4" => KeyCode::End,
            "5" => KeyCode::PageUp,
            "6" => KeyCode::PageDown,
            _ => return None,
        }
    } else {
        // `[A`, or `[1;2A` with its modifier.
        if !(number.is_empty() && modifier.is_empty() || number == "1" && !modifier.is_empty()) {
            return None;
        }
        letter(last)?
    };
    Some((code, modifiers))
}

/// An xterm modifier parameter: one more than the Shift, Alt, Control and
/// Super bits.
fn modifiers(parameter: u8) -> Option<KeyModifiers> {
    let bits = parameter.checked_sub(1).filter(|bits| *bits < 16)?;
    let mut modifiers = KeyModifiers::NONE;
    for (bit, modifier) in [
        (1, KeyModifiers::SHIFT),
        (2, KeyModifiers::ALT),
        (4, KeyModifiers::CONTROL),
        (8, KeyModifiers::SUPER),
    ] {
        if bits & bit != 0 {
            modifiers |= modifier;
        }
    }
    Some(modifiers)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What crossterm makes of the bytes after `ESC ESC`.
    fn typed(text: &str) -> Vec<KeyEvent> {
        text.chars()
            .map(|character| {
                KeyEvent::new(
                    KeyCode::Char(character),
                    if character.is_uppercase() {
                        KeyModifiers::SHIFT
                    } else {
                        KeyModifiers::NONE
                    },
                )
            })
            .collect()
    }

    fn joined(text: &str) -> Option<(KeyCode, KeyModifiers, usize)> {
        escaped_key(&typed(text)).map(|(key, taken)| (key.code, key.modifiers, taken))
    }

    #[test]
    fn terminal_app_option_arrows_are_alt_arrows() {
        let alt = KeyModifiers::ALT;
        assert_eq!(joined("[A"), Some((KeyCode::Up, alt, 2)));
        assert_eq!(joined("[B"), Some((KeyCode::Down, alt, 2)));
        assert_eq!(joined("[C"), Some((KeyCode::Right, alt, 2)));
        assert_eq!(joined("[D"), Some((KeyCode::Left, alt, 2)));
        assert_eq!(joined("[H"), Some((KeyCode::Home, alt, 2)));
        assert_eq!(joined("[F"), Some((KeyCode::End, alt, 2)));
        assert_eq!(joined("OA"), Some((KeyCode::Up, alt, 2)));
        assert_eq!(joined("[5~"), Some((KeyCode::PageUp, alt, 3)));
        assert_eq!(joined("[6~"), Some((KeyCode::PageDown, alt, 3)));
        assert_eq!(
            joined("[1;2A"),
            Some((KeyCode::Up, alt | KeyModifiers::SHIFT, 5))
        );
        assert_eq!(
            joined("[1;5B"),
            Some((KeyCode::Down, alt | KeyModifiers::CONTROL, 5))
        );
        assert_eq!(
            joined("[5;2~"),
            Some((KeyCode::PageUp, alt | KeyModifiers::SHIFT, 5))
        );
        // Keys typed after the sequence stay keys of their own.
        assert_eq!(joined("[Axyz"), Some((KeyCode::Up, alt, 2)));
    }

    /// What `TerminalInput` hands out when one read brought `events`.
    fn read(events: Vec<KeyEvent>) -> Vec<KeyEvent> {
        let mut ready = events
            .into_iter()
            .map(|key| Ok(Event::Key(key)))
            .collect::<VecDeque<_>>();
        let mut out = Vec::new();
        while let Some(event) = ready.pop_front() {
            let Ok(Event::Key(key)) = join(event, &mut ready) else {
                panic!("not a key");
            };
            out.push(key);
        }
        out
    }

    fn escape() -> KeyEvent {
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
    }

    #[test]
    fn several_option_arrows_in_one_read_are_each_one_key() {
        let option_up = || [vec![escape()], typed("[A")].concat();
        let alt_up = KeyEvent::new(KeyCode::Up, KeyModifiers::ALT);
        assert_eq!(read(option_up()), [alt_up]);
        assert_eq!(read([option_up(), option_up()].concat()), [alt_up, alt_up]);
        assert_eq!(
            read([option_up(), option_up(), option_up()].concat()),
            [alt_up, alt_up, alt_up]
        );
        // Typed text around them stays text.
        assert_eq!(
            read([typed("d"), option_up(), typed("x")].concat()),
            [typed("d"), vec![alt_up], typed("x")].concat()
        );
    }

    #[test]
    fn an_escape_with_text_or_alone_stays_an_escape() {
        assert_eq!(read(vec![escape()]), [escape()]);
        assert_eq!(
            read([vec![escape()], typed("hi")].concat()),
            [vec![escape()], typed("hi")].concat()
        );
        assert_eq!(read(vec![escape(), escape()]), [escape(), escape()]);
        // A lone Escape after a joined key is still a lone Escape.
        assert_eq!(
            read([vec![escape()], typed("[A"), vec![escape()]].concat()),
            [KeyEvent::new(KeyCode::Up, KeyModifiers::ALT), escape()]
        );
    }

    #[test]
    fn anything_else_after_an_escape_is_left_alone() {
        for text in [
            "", "[", "[1", "[1;", "[x", "x[A", "[2A", "[;3A", "[1;0A", "[9~", "O", "Ox",
        ] {
            assert_eq!(joined(text), None, "{text:?}");
        }
        // Only plain presses take part: a modified key was typed on purpose.
        let mut keys = typed("[A");
        keys[1].modifiers = KeyModifiers::CONTROL;
        assert_eq!(escaped_key(&keys), None);
        keys = typed("[A");
        keys[0].kind = KeyEventKind::Release;
        assert_eq!(escaped_key(&keys), None);
        keys = typed("[A");
        keys.insert(1, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(escaped_key(&keys), None);
    }
}
