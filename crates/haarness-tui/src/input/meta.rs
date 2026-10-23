//! Option with an arrow from macOS Terminal. Terminal.app, and iTerm2's
//! Terminal.app preset, send Option+Up as Escape followed by the arrow's own
//! sequence, `ESC ESC [ A` (tmux usually re-encodes it as `ESC [1;3A` before
//! the TUI sees it, but not always). crossterm reads that as an Escape press
//! and then `[` and `A` typed, so the key would arm an interrupt, reject a
//! focused approval and type into the prompt. When those keys arrived
//! together, in the same read, they are one Alt key again; so is an Escape
//! typed and followed by an arrow within one read.

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use futures::{FutureExt as _, StreamExt};

/// The longest sequence after the doubled Escape: `[1;16A`.
const LONGEST: usize = 6;

/// How many events [`TerminalInput::input_waiting`] reads ahead to look.
const READ_AHEAD: usize = 64;

/// Where events come from: the terminal, or a test's reads.
pub(crate) trait Source {
    /// The next event, waiting for one.
    fn next(&mut self) -> impl Future<Output = Option<io::Result<Event>>>;
    /// An event that came with the last read, already decoded; never waits.
    fn ready(&mut self) -> Option<io::Result<Event>>;
}

impl Source for EventStream {
    fn next(&mut self) -> impl Future<Output = Option<io::Result<Event>>> {
        StreamExt::next(self)
    }

    fn ready(&mut self) -> Option<io::Result<Event>> {
        // Ask first, so a lone Escape never leaves the stream waiting on a
        // waker nobody holds.
        if !crossterm::event::poll(Duration::ZERO).unwrap_or(false) {
            return None;
        }
        StreamExt::next(self).now_or_never().flatten()
    }
}

/// The terminal's events, with an Escape-prefixed key joined into one, each
/// stamped with when it was read: a key that waited behind slow work keeps
/// the moment it came in.
pub(crate) struct TerminalInput<S = EventStream> {
    source: S,
    /// Events already read and decoded, not handed out yet, each with the
    /// time of the read that brought it.
    ready: VecDeque<(io::Result<Event>, Instant)>,
    /// When the event `next` handed out last was read.
    read_at: Instant,
    /// The read whose decoded events are still coming from the source, if
    /// it was not taken to its end: what the source hands over next came
    /// with it. Once the source has nothing more, the next event is a read
    /// of its own.
    batch: Option<Instant>,
}

impl TerminalInput {
    pub(crate) fn new() -> Self {
        Self::reading(EventStream::new())
    }
}

impl<S: Source> TerminalInput<S> {
    fn reading(source: S) -> Self {
        Self {
            source,
            ready: VecDeque::new(),
            read_at: Instant::now(),
            batch: None,
        }
    }

    /// An event the source already has, stamped with the read it came with.
    fn take_ready(&mut self) -> Option<(io::Result<Event>, Instant)> {
        let Some(event) = self.source.ready() else {
            self.batch = None;
            return None;
        };
        let read = *self.batch.get_or_insert_with(Instant::now);
        Some((event, read))
    }

    /// When the event [`Self::next`] handed out last was read.
    pub(crate) const fn read_at(&self) -> Instant {
        self.read_at
    }

    /// Whether a key, a paste or a click is already here, read or not: it
    /// came before now. What the terminal sent is read (and stamped) to
    /// tell; pointer motion, scrolling, focus and resizes do not count.
    pub(crate) fn input_waiting(&mut self) -> bool {
        while self.ready.len() < READ_AHEAD
            && let Some(entry) = self.take_ready()
        {
            self.ready.push_back(entry);
        }
        self.ready.iter().any(|(event, _)| match event {
            Ok(Event::Key(key)) => key.kind != KeyEventKind::Release,
            Ok(Event::Paste(_)) => true,
            Ok(Event::Mouse(mouse)) => matches!(mouse.kind, MouseEventKind::Down(_)),
            _ => false,
        })
    }

    /// The next event. Cancel safe: it only waits before taking anything.
    pub(crate) async fn next(&mut self) -> Option<io::Result<Event>> {
        let (event, read_at) = match self.ready.pop_front() {
            Some(entry) => entry,
            None => {
                let event = self.source.next().await?;
                let read = *self.batch.get_or_insert_with(Instant::now);
                (event, read)
            }
        };
        self.read_at = read_at;
        if !escape(&event) {
            return Some(event);
        }
        // Several Option+arrows can come in one read (a held key, bunched
        // SSH packets, a slow frame), so this runs for every Escape, queued
        // or new: what else that read brought tops the queue up.
        while self.ready.len() < LONGEST {
            let Some((next, read)) = self.take_ready() else {
                break;
            };
            let key = matches!(next, Ok(Event::Key(_)));
            self.ready.push_back((next, read));
            if !key {
                break;
            }
        }
        Some(join(event, read_at, &mut self.ready))
    }
}

fn escape(event: &io::Result<Event>) -> bool {
    matches!(event, Ok(Event::Key(key)) if bare_escape(key))
}

/// `event` as it is, or, when it is an Escape read at `read` and the keys at
/// the front of `ready` that came with it (in the same read) spell a cursor
/// key, that key with Alt, which takes them out of `ready`. An Escape read
/// ahead, before the keys after it were sent, stays an Escape.
fn join(
    event: io::Result<Event>,
    read: Instant,
    ready: &mut VecDeque<(io::Result<Event>, Instant)>,
) -> io::Result<Event> {
    if !escape(&event) {
        return event;
    }
    let keys = ready
        .iter()
        .map_while(|(event, at)| match event {
            Ok(Event::Key(key)) if *at == read => Some(*key),
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

/// A complete cursor or editing key sequence after its Escape: `[A`,
/// `[1;2A`, `[5~` or `[5;2~`. Not the `OA` form: the TUI never turns on
/// application cursor keys, and Escape then a typed "OH" must stay text.
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
        let read = Instant::now();
        let mut ready = events
            .into_iter()
            .map(|key| (Ok(Event::Key(key)), read))
            .collect::<VecDeque<_>>();
        let mut out = Vec::new();
        while let Some((event, at)) = ready.pop_front() {
            let Ok(Event::Key(key)) = join(event, at, &mut ready) else {
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

    /// A test's terminal: each inner queue is what one read brought.
    struct Reads(VecDeque<VecDeque<KeyEvent>>);

    impl Source for Reads {
        fn next(&mut self) -> impl Future<Output = Option<io::Result<Event>>> {
            while self.0.front().is_some_and(VecDeque::is_empty) {
                self.0.pop_front();
            }
            std::future::ready(self.ready())
        }

        fn ready(&mut self) -> Option<io::Result<Event>> {
            Some(Ok(Event::Key(self.0.front_mut()?.pop_front()?)))
        }
    }

    /// A terminal whose source hands over what was sent since the last look,
    /// as a live terminal does, so a test can send more between reads.
    struct Live(VecDeque<KeyEvent>);

    impl Source for Live {
        fn next(&mut self) -> impl Future<Output = Option<io::Result<Event>>> {
            std::future::ready(self.ready())
        }

        fn ready(&mut self) -> Option<io::Result<Event>> {
            Some(Ok(Event::Key(self.0.pop_front()?)))
        }
    }

    /// An Escape read ahead (by `input_waiting`, at an approval's deadline)
    /// stays an Escape when the arrow keys come in a later read: joined into
    /// Alt+↑ it would send a settling answer that the Escape takes back. Keys
    /// read with it still join.
    #[tokio::test]
    async fn an_escape_read_ahead_never_joins_keys_from_a_later_read() {
        let mut input = TerminalInput::reading(Live(VecDeque::from([escape()])));
        assert!(input.input_waiting());
        std::thread::sleep(std::time::Duration::from_millis(2));
        input.source.0.extend(typed("[A"));
        let mut out = Vec::new();
        while let Some(Ok(Event::Key(key))) = input.next().await {
            out.push(key);
        }
        assert_eq!(out, [vec![escape()], typed("[A")].concat());

        let mut input = TerminalInput::reading(Live([vec![escape()], typed("[A")].concat().into()));
        assert!(input.input_waiting());
        let Some(Ok(Event::Key(key))) = input.next().await else {
            panic!("no key");
        };
        assert_eq!(key, KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
    }

    /// Keys waiting count as input that came first, and are still handed
    /// out in order, stamped when they were read.
    #[tokio::test]
    async fn waiting_keys_are_read_ahead_and_handed_out_in_order() {
        let key = |character| KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
        let mut input = TerminalInput::reading(Reads(VecDeque::from([VecDeque::from([
            key('y'),
            key('e'),
        ])])));
        assert!(input.input_waiting());
        let mut out = Vec::new();
        while let Some(Ok(Event::Key(key))) = input.next().await {
            out.push(key.code);
        }
        assert_eq!(out, [KeyCode::Char('y'), KeyCode::Char('e')]);
        assert!(!input.input_waiting());
    }

    async fn handed_out(reads: Vec<Vec<KeyEvent>>) -> Vec<KeyEvent> {
        let mut input =
            TerminalInput::reading(Reads(reads.into_iter().map(VecDeque::from).collect()));
        let mut out = Vec::new();
        while let Some(Ok(Event::Key(key))) = input.next().await {
            out.push(key);
        }
        out
    }

    #[tokio::test]
    async fn the_reader_joins_queued_escapes_and_never_across_reads() {
        let option_up = || [vec![escape()], typed("[A")].concat();
        let alt_up = KeyEvent::new(KeyCode::Up, KeyModifiers::ALT);
        // Three in one read: the first top-up stops at six events, so the
        // second and third Escapes are joined from the queue.
        assert_eq!(
            handed_out(vec![[option_up(), option_up(), option_up()].concat()]).await,
            [alt_up, alt_up, alt_up]
        );
        // An Escape that ends one read is an Escape, whatever the next brings.
        assert_eq!(
            handed_out(vec![vec![escape()], typed("[A")]).await,
            [vec![escape()], typed("[A")].concat()
        );
        assert_eq!(
            handed_out(vec![option_up(), vec![escape()], typed("x")]).await,
            [vec![alt_up, escape()], typed("x")].concat()
        );
    }

    #[test]
    fn anything_else_after_an_escape_is_left_alone() {
        for text in [
            "", "[", "[1", "[1;", "[x", "x[A", "[2A", "[;3A", "[1;0A", "[9~", "OA", "OH", "OF",
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
