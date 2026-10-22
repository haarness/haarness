//! Full-screen terminal tabs backed by server-owned PTYs.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use haarness_opentui::{Color, Line, Span, attr};
use haarness_protocol::{PtyId, PtyInfo, PtyStatus, PtyUpdateRequest};

use crate::width::DisplayWidth as _;
use crate::{api::PtyClientEvent, locale::Language, theme::Theme};

const SCROLLBACK_LINES: usize = 10_000;

pub struct TerminalWorkspace {
    tabs: Vec<TerminalTab>,
    active: usize,
}

struct TerminalTab {
    info: PtyInfo,
    parser: vt100::Parser,
    connected: bool,
    exited: bool,
}

impl TerminalWorkspace {
    #[must_use]
    pub fn new(infos: Vec<PtyInfo>) -> Self {
        Self {
            tabs: infos.into_iter().map(TerminalTab::new).collect(),
            active: 0,
        }
    }

    #[must_use]
    pub fn ids(&self) -> Vec<PtyId> {
        self.tabs.iter().map(|tab| tab.info.id).collect()
    }

    #[must_use]
    pub fn active_id(&self) -> Option<PtyId> {
        self.tabs.get(self.active).map(|tab| tab.info.id)
    }

    pub fn push(&mut self, info: PtyInfo) {
        self.tabs.push(TerminalTab::new(info));
        self.active = self.tabs.len().saturating_sub(1);
    }

    pub fn remove(&mut self, id: PtyId) {
        let Some(index) = self.tabs.iter().position(|tab| tab.info.id == id) else {
            return;
        };
        self.tabs.remove(index);
        // A shell exiting in a tab to the left must not move the one shown.
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
    }

    /// Show this tab, when it is open here.
    pub fn focus(&mut self, id: PtyId) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.info.id == id) {
            self.active = index;
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn next(&mut self, direction: isize) {
        if self.tabs.len() < 2 {
            return;
        }
        self.active = if direction < 0 {
            self.active.checked_sub(1).unwrap_or(self.tabs.len() - 1)
        } else {
            (self.active + 1) % self.tabs.len()
        };
    }

    pub fn scroll(&mut self, delta: isize) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let current = tab.parser.screen().scrollback();
        let next = if delta < 0 {
            current.saturating_add(delta.unsigned_abs())
        } else {
            current.saturating_sub(delta.unsigned_abs())
        };
        tab.parser.screen_mut().set_scrollback(next);
    }

    /// One page of scrollback: the visible rows less a little overlap.
    #[must_use]
    pub fn page_rows(&self) -> isize {
        self.tabs.get(self.active).map_or(10, |tab| {
            isize::try_from(tab.info.rows.saturating_sub(2).max(1)).unwrap_or(isize::MAX)
        })
    }

    pub fn follow_output(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.parser.screen_mut().set_scrollback(0);
        }
    }

    pub fn apply(&mut self, event: PtyClientEvent) {
        let id = match &event {
            PtyClientEvent::Connected(id)
            | PtyClientEvent::Disconnected(id)
            | PtyClientEvent::Exited(id)
            | PtyClientEvent::Replay { id, .. }
            | PtyClientEvent::Output { id, .. } => *id,
        };
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.info.id == id) else {
            return;
        };
        match event {
            PtyClientEvent::Connected(_) => tab.connected = true,
            PtyClientEvent::Disconnected(_) => tab.connected = false,
            PtyClientEvent::Exited(_) => {
                tab.connected = false;
                tab.exited = true;
                tab.info.status = PtyStatus::Exited;
            }
            // Older output is gone: start over, in the modes it left on.
            PtyClientEvent::Replay {
                truncated: true,
                modes,
                ..
            } => {
                tab.reset();
                if let Some(modes) = modes {
                    tab.parser.process(modes.as_bytes());
                }
            }
            PtyClientEvent::Replay { .. } => {}
            PtyClientEvent::Output { bytes, .. } => tab.parser.process(&bytes),
        }
    }

    /// The bytes `key` sends to the active terminal. A program that turned on
    /// application cursor keys (vim, less, htop and other full-screen
    /// programs do) reads the arrows, Home and End as `ESC O x`, which is what
    /// the `xterm-256color` entry its `TERM` names says they send.
    #[must_use]
    pub fn input_bytes(&self, key: KeyEvent) -> Option<Vec<u8>> {
        let mut bytes = key_bytes(key)?;
        let application = self
            .tabs
            .get(self.active)
            .is_some_and(|tab| tab.parser.screen().application_cursor());
        if application
            && let [0x1b, prefix @ b'[', b'A'..=b'D' | b'H' | b'F'] = bytes.as_mut_slice()
        {
            *prefix = b'O';
        }
        Some(bytes)
    }

    #[must_use]
    pub fn paste_bytes(&self, text: &str) -> Option<Vec<u8>> {
        let tab = self.tabs.get(self.active)?;
        if tab.parser.screen().bracketed_paste() {
            let mut bytes = b"\x1b[200~".to_vec();
            bytes.extend_from_slice(text.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            Some(bytes)
        } else {
            Some(text.as_bytes().to_vec())
        }
    }

    /// Keep the active child PTY and local emulator aligned with the actual
    /// content rectangle. Background tabs resize when selected.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Option<(PtyId, PtyUpdateRequest)> {
        let tab = self.tabs.get_mut(self.active)?;
        let cols = cols.max(1);
        let rows = rows.max(1);
        if tab.info.cols == cols && tab.info.rows == rows {
            return None;
        }
        tab.parser.screen_mut().set_size(rows, cols);
        tab.info.cols = cols;
        tab.info.rows = rows;
        Some((
            tab.info.id,
            PtyUpdateRequest {
                title: None,
                cols: Some(cols),
                rows: Some(rows),
            },
        ))
    }
}

impl TerminalTab {
    fn new(info: PtyInfo) -> Self {
        let exited = info.status == PtyStatus::Exited;
        Self {
            parser: vt100::Parser::new(info.rows.max(1), info.cols.max(1), SCROLLBACK_LINES),
            info,
            connected: false,
            exited,
        }
    }

    fn reset(&mut self) {
        self.parser = vt100::Parser::new(
            self.info.rows.max(1),
            self.info.cols.max(1),
            SCROLLBACK_LINES,
        );
    }
}

#[must_use]
pub fn dimensions(size: (u32, u32)) -> (u16, u16) {
    let cols = u16::try_from(size.0.max(1)).unwrap_or(u16::MAX);
    let rows = u16::try_from(size.1.saturating_sub(2).max(1)).unwrap_or(u16::MAX);
    (cols, rows)
}

/// The way back to the chat. Kitty-protocol terminals report Ctrl+] as
/// itself, but tmux and Terminal.app send the legacy GS byte (0x1d), which
/// crossterm decodes as Ctrl+5; xterm sends the same byte for Ctrl+5. F12 also
/// works on layouts where `]` needs AltGr or Option.
#[must_use]
pub fn is_detach_key(key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char(']' | '5') => key.modifiers.contains(KeyModifiers::CONTROL),
        KeyCode::F(12) => true,
        _ => false,
    }
}

/// Key hints for the footer. Without enhanced key reporting, Ctrl+Shift+T and
/// Ctrl+Shift+W arrive as Ctrl+T and Ctrl+W and belong to the shell, so they
/// are not offered.
fn footer_hints(enhanced: bool) -> &'static [&'static str] {
    if enhanced {
        &[
            "ctrl+] or f12 chat",
            "ctrl+shift+t new",
            "ctrl+shift+w close",
            "alt+←/→ tab",
            "shift+pgup scroll",
        ]
    } else {
        &[
            "ctrl+] or f12 chat",
            "alt+←/→ tab",
            "shift+pgup scroll",
            "exit closes the tab",
        ]
    }
}

#[must_use]
pub fn frame(
    terminal: &TerminalWorkspace,
    theme: Theme,
    language: Language,
    width: usize,
    height: usize,
) -> Vec<Line> {
    let mut lines = Vec::with_capacity(height);
    let mut tabs = vec![Span::new(" TERMINAL  ", theme.primary).with_attributes(attr::BOLD)];
    for (index, tab) in terminal.tabs.iter().enumerate() {
        let active = index == terminal.active;
        let state = if tab.exited {
            "×"
        } else if tab.connected {
            "●"
        } else {
            "○"
        };
        let mut span = Span::new(
            format!(" {state} {} ", tab.info.title),
            if active { theme.text } else { theme.muted },
        );
        if active {
            span = span.on(theme.selected).with_attributes(attr::BOLD);
        }
        tabs.push(span);
    }
    lines.push(Line::new(tabs));

    let content_rows = height.saturating_sub(2);
    if let Some(tab) = terminal.tabs.get(terminal.active) {
        let screen = tab.parser.screen();
        let (screen_rows, screen_cols) = screen.size();
        let (cursor_row, cursor_col) = screen.cursor_position();
        for row in 0..content_rows {
            if row >= usize::from(screen_rows) {
                lines.push(Line::empty());
                continue;
            }
            let mut spans: Vec<Span> = Vec::new();
            let visible_cols = width.min(usize::from(screen_cols));
            for col in 0..visible_cols {
                let Some(cell) = screen.cell(
                    u16::try_from(row).unwrap_or(u16::MAX),
                    u16::try_from(col).unwrap_or(u16::MAX),
                ) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let text = if cell.has_contents() {
                    cell.contents().to_owned()
                } else {
                    " ".to_owned()
                };
                let mut attributes = 0;
                if cell.bold() {
                    attributes |= attr::BOLD;
                }
                if cell.dim() {
                    attributes |= attr::DIM;
                }
                if cell.italic() {
                    attributes |= attr::ITALIC;
                }
                if cell.underline() {
                    attributes |= attr::UNDERLINE;
                }
                if cell.inverse() {
                    attributes |= attr::INVERSE;
                }
                if screen.scrollback() == 0
                    && !screen.hide_cursor()
                    && usize::from(cursor_row) == row
                    && usize::from(cursor_col) == col
                {
                    attributes ^= attr::INVERSE;
                }
                let foreground = terminal_color(cell.fgcolor(), theme.text);
                let background = match cell.bgcolor() {
                    vt100::Color::Default => None,
                    color => Some(terminal_color(color, theme.bg)),
                };
                if let Some(previous) = spans.last_mut()
                    && previous.fg == foreground
                    && previous.bg == background
                    && previous.attributes == attributes
                {
                    previous.text.push_str(&text);
                } else {
                    let mut span = Span::new(text, foreground).with_attributes(attributes);
                    if let Some(background) = background {
                        span = span.on(background);
                    }
                    spans.push(span);
                }
            }
            lines.push(Line::new(spans));
        }
    } else {
        lines.extend((0..content_rows).map(|_| Line::empty()));
    }
    // Whole hints only, in order: the way back to the chat always shows.
    let mut footer = String::new();
    for hint in footer_hints(crate::terminal::keyboard_enhanced()) {
        let hint = language.text(hint);
        let next = if footer.is_empty() {
            format!(" {hint}")
        } else {
            format!("{footer}   {hint}")
        };
        if !footer.is_empty() && next.width() >= width {
            break;
        }
        footer = next;
    }
    lines.push(Line::plain(footer, theme.muted));
    lines.truncate(height);
    while lines.len() < height {
        lines.push(Line::empty());
    }
    lines
}

#[must_use]
pub fn key_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let sequence: Vec<u8> = match key.code {
        KeyCode::Char(character) if ctrl => {
            let upper = character.to_ascii_uppercase();
            if upper.is_ascii_alphabetic() {
                vec![(upper as u8).saturating_sub(b'@')]
            } else {
                // The remaining C0 controls, in both their symbol form and
                // the digit form legacy terminals decode them to.
                match character {
                    ' ' | '@' | '2' => vec![0],
                    '\\' | '4' => vec![0x1c],
                    '^' | '6' => vec![0x1e],
                    '_' | '/' | '7' => vec![0x1f],
                    _ => return None,
                }
            }
        }
        KeyCode::Char(character) => {
            let mut bytes = Vec::new();
            if alt {
                bytes.push(0x1b);
            }
            let mut encoded = [0; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            bytes
        }
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::F(number @ 1..=4) => vec![0x1b, b'O', b'P' + number - 1],
        KeyCode::F(number @ 5..=12) => format!("\x1b[{}~", number + 10).into_bytes(),
        _ => return None,
    };
    Some(sequence)
}

fn terminal_color(color: vt100::Color, default: Color) -> Color {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Rgb(red, green, blue) => Color::rgb(red, green, blue),
        vt100::Color::Idx(index) => ansi_color(index),
    }
}

fn ansi_color(index: u8) -> Color {
    const BASIC: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    let (red, green, blue) = match index {
        0..=15 => BASIC[usize::from(index)],
        16..=231 => {
            let value = index - 16;
            let channel = |part: u8| if part == 0 { 0 } else { 55 + part * 40 };
            (
                channel(value / 36),
                channel((value % 36) / 6),
                channel(value % 6),
            )
        }
        _ => {
            let gray = 8 + (index - 232) * 10;
            (gray, gray, gray)
        }
    };
    Color::rgb(red, green, blue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_terminal_navigation_and_control_keys() {
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        assert_eq!(
            key_bytes(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(vec![b'\r'])
        );
        assert_eq!(
            key_bytes(key(KeyCode::Up, KeyModifiers::NONE)),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_bytes(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![3])
        );
        assert_eq!(
            key_bytes(key(KeyCode::Char('x'), KeyModifiers::ALT)),
            Some(b"\x1bx".to_vec())
        );
    }

    #[test]
    fn parser_consumes_child_escape_sequences_instead_of_leaking_them() {
        let info = PtyInfo {
            id: PtyId::new(),
            title: "test".into(),
            command: "sh".into(),
            args: Vec::new(),
            cwd: "/tmp".into(),
            status: PtyStatus::Running,
            pid: None,
            cols: 20,
            rows: 4,
            created: false,
        };
        let mut terminal = TerminalWorkspace::new(vec![info]);
        let id = terminal.active_id().unwrap();
        terminal.apply(PtyClientEvent::Output {
            id,
            bytes: b"plain \x1b[31mred\x1b[0m".to_vec(),
        });
        let rendered = frame(&terminal, Theme::editorial_dark(), Language::English, 20, 6);
        let body = rendered[1].text();
        assert!(body.contains("plain red"));
        assert!(!body.contains('\x1b'));
        // A narrow footer drops whole hints, never the way back.
        assert_eq!(rendered[5].text(), " ctrl+] or f12 chat");
        let wide = frame(&terminal, Theme::editorial_dark(), Language::English, 60, 6);
        assert_eq!(
            wide[5].text(),
            " ctrl+] or f12 chat   alt+←/→ tab   shift+pgup scroll"
        );
    }

    fn workspace() -> (TerminalWorkspace, PtyId) {
        let terminal = TerminalWorkspace::new(vec![PtyInfo {
            id: PtyId::new(),
            title: "test".into(),
            command: "sh".into(),
            args: Vec::new(),
            cwd: "/tmp".into(),
            status: PtyStatus::Running,
            pid: None,
            cols: 20,
            rows: 4,
            created: false,
        }]);
        let id = terminal.active_id().unwrap();
        (terminal, id)
    }

    #[test]
    fn arrows_follow_the_programs_cursor_key_mode() {
        let (mut terminal, id) = workspace();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(
            terminal.input_bytes(key(KeyCode::Up)),
            Some(b"\x1b[A".to_vec())
        );
        terminal.apply(PtyClientEvent::Output {
            id,
            bytes: b"\x1b[?1h\x1b=".to_vec(),
        });
        for (code, sent) in [
            (KeyCode::Up, b"\x1bOA"),
            (KeyCode::Down, b"\x1bOB"),
            (KeyCode::Right, b"\x1bOC"),
            (KeyCode::Left, b"\x1bOD"),
            (KeyCode::Home, b"\x1bOH"),
            (KeyCode::End, b"\x1bOF"),
        ] {
            assert_eq!(
                terminal.input_bytes(key(code)),
                Some(sent.to_vec()),
                "{code:?}"
            );
        }
        // Other keys are as before.
        assert_eq!(
            terminal.input_bytes(key(KeyCode::PageUp)),
            Some(b"\x1b[5~".to_vec())
        );
        assert_eq!(
            terminal.input_bytes(key(KeyCode::Char('x'))),
            Some(b"x".to_vec())
        );
        terminal.apply(PtyClientEvent::Output {
            id,
            bytes: b"\x1b[?1l".to_vec(),
        });
        assert_eq!(
            terminal.input_bytes(key(KeyCode::Up)),
            Some(b"\x1b[A".to_vec())
        );
    }

    #[test]
    fn a_truncated_replay_starts_over_in_the_modes_the_lost_output_left_on() {
        let (mut terminal, id) = workspace();
        terminal.apply(PtyClientEvent::Output {
            id,
            bytes: b"old screen".to_vec(),
        });
        terminal.apply(PtyClientEvent::Replay {
            id,
            truncated: true,
            modes: Some("\x1b[?1049h\x1b[?1h\x1b[?2004h".into()),
        });
        terminal.apply(PtyClientEvent::Output {
            id,
            bytes: b"redrawn".to_vec(),
        });
        let rendered = frame(&terminal, Theme::editorial_dark(), Language::English, 20, 6);
        assert!(
            rendered[1].text().starts_with("redrawn"),
            "{}",
            rendered[1].text()
        );
        let screen = terminal.tabs[0].parser.screen();
        assert!(screen.alternate_screen() && screen.application_cursor());
        assert_eq!(
            terminal.paste_bytes("p"),
            Some(b"\x1b[200~p\x1b[201~".to_vec())
        );
    }
}
