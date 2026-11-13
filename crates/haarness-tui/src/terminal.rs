//! Raw-mode/alternate-screen ownership with reliable cleanup.

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;
use crossterm::{
    cursor::{Hide, Show},
    event::{
        DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::{
    io::{IsTerminal, Read},
    time::{Duration, Instant},
};

pub struct TerminalGuard;

const MOUSE_TRACKING_ENABLE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";
const MOUSE_TRACKING_DISABLE: &str = "\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1015l\x1b[?1006l";
/// Numeric keypad mode (DECKPNM): keypad digits and Enter arrive as the main
/// keys' characters. A full-screen program that exits without leaving
/// application keypad mode (`ESC =`) has them sent as `ESC O q`, `ESC O M`,
/// which the key reader drops, so the keypad would type nothing.
const NUMERIC_KEYPAD: &str = "\x1b>";

#[cfg(unix)]
struct RawProbeGuard;

#[cfg(unix)]
impl Drop for RawProbeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn osc_text(value: &str, limit: usize) -> String {
    crate::escapes::without_escapes(value)
        .chars()
        .filter(|character| !character.is_control())
        .take(limit)
        .collect()
}

/// Ask the terminal for OSC 11 (its configured default background) and return
/// the matching theme variant. This avoids choosing dark
/// text/colors merely from process environment guesses.
pub fn detect_background_variant() -> Option<&'static str> {
    terminal_background_response()
        .as_deref()
        .and_then(parse_background_variant)
}

fn parse_background_variant(response: &str) -> Option<&'static str> {
    let marker = "\x1b]11;";
    let start = response.rfind(marker)? + marker.len();
    let value = &response[start..];
    let end = value
        .find('\x07')
        .or_else(|| value.find("\x1b\\"))
        .unwrap_or(value.len());
    let (red, green, blue) = parse_terminal_rgb(&value[..end])?;
    let luminance =
        (0.299 * f64::from(red) + 0.587 * f64::from(green) + 0.114 * f64::from(blue)) / 255.0;
    Some(if luminance > 0.5 { "light" } else { "dark" })
}

fn parse_terminal_rgb(value: &str) -> Option<(u8, u8, u8)> {
    if let Some(value) = value.strip_prefix("rgb:") {
        let mut components = value.split('/').map(parse_hex_component);
        let colors = (
            components.next()??,
            components.next()??,
            components.next()??,
        );
        return components.next().is_none().then_some(colors);
    }
    if let Some(value) = value.strip_prefix('#') {
        return match value.len() {
            6 => Some((
                u8::from_str_radix(&value[0..2], 16).ok()?,
                u8::from_str_radix(&value[2..4], 16).ok()?,
                u8::from_str_radix(&value[4..6], 16).ok()?,
            )),
            12 => Some((
                parse_hex_component(&value[0..4])?,
                parse_hex_component(&value[4..8])?,
                parse_hex_component(&value[8..12])?,
            )),
            _ => None,
        };
    }
    let value = value.strip_prefix("rgb(")?.strip_suffix(')')?;
    let mut components = value.split(',').map(|part| part.trim().parse::<u8>().ok());
    let colors = (
        components.next()??,
        components.next()??,
        components.next()??,
    );
    components.next().is_none().then_some(colors)
}

fn parse_hex_component(value: &str) -> Option<u8> {
    if value.is_empty() || value.len() > 4 {
        return None;
    }
    let component = u32::from_str_radix(value, 16).ok()?;
    let maximum = (1_u32 << (value.len() * 4)) - 1;
    u8::try_from((component * 255 + maximum / 2) / maximum).ok()
}

#[cfg(unix)]
fn terminal_background_response() -> Option<String> {
    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
    use std::os::fd::AsFd;

    let stdin_handle = io::stdin();
    let stdout_handle = io::stdout();
    if !stdin_handle.is_terminal() || !stdout_handle.is_terminal() {
        return None;
    }

    enable_raw_mode().ok()?;
    let _raw_guard = RawProbeGuard;

    let mut stdin = stdin_handle.lock();
    let mut stdout = stdout_handle.lock();
    stdout.write_all(b"\x1b]11;?\x07").ok()?;
    stdout.flush().ok()?;

    // Allow a one-second response window. Some terminal emulators answer OSC
    // queries only after flushing profile state, especially on first launch.
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut response = Vec::with_capacity(64);
    while response.len() < 256 {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let ready = {
            let mut descriptors = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
            let timeout = PollTimeout::try_from(remaining).unwrap_or(PollTimeout::MAX);
            poll(&mut descriptors, timeout).ok()?
        };
        if ready == 0 {
            break;
        }
        let mut buffer = [0_u8; 64];
        let count = stdin.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..count]);
        if response.contains(&b'\x07') || response.windows(2).any(|window| window == b"\x1b\\") {
            break;
        }
    }
    String::from_utf8(response).ok()
}

#[cfg(not(unix))]
fn terminal_background_response() -> Option<String> {
    None
}

/// Set the terminal tab/window title (OSC 0). Pass an empty string to clear.
pub fn set_title(title: &str) {
    let title = osc_text(title, 200);
    let _ = write!(io::stdout(), "\x1b]0;{title}\x07");
    let _ = io::stdout().flush();
}

/// Desktop-style notification via OSC 9 (iTerm2/kitty/WezTerm honor it;
/// others ignore it silently). Single line, kept short.
pub fn notify(message: &str) {
    if std::env::var("HAARNESS_NOTIFICATIONS")
        .is_ok_and(|value| matches!(value.to_ascii_lowercase().as_str(), "0" | "false" | "off"))
    {
        return;
    }
    let message = osc_text(message, 120);
    let _ = write!(io::stdout(), "\x1b]9;{message}\x07");
    let _ = io::stdout().flush();
    if !terminal_supports_osc9() {
        native_notification(message);
    }
}

fn terminal_supports_osc9() -> bool {
    std::env::var("TERM_PROGRAM").is_ok_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "iterm.app" | "wezterm" | "kitty"
        )
    }) || std::env::var("TERM").is_ok_and(|value| value.contains("kitty"))
}

#[cfg(target_os = "macos")]
fn native_notification(message: String) {
    std::thread::spawn(move || {
        let _ = std::process::Command::new("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "display notification (item 1 of argv) with title \"Haarness\"",
                "-e",
                "end run",
                "--",
                &message,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}

#[cfg(all(unix, not(target_os = "macos")))]
fn native_notification(message: String) {
    std::thread::spawn(move || {
        let _ = std::process::Command::new("notify-send")
            .args(["Haarness", &message])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}

#[cfg(not(unix))]
fn native_notification(_message: String) {}

/// Copy text for the user: OSC 52, which reaches a terminal over SSH too,
/// plus the native clipboard tool when one exists and, inside tmux, tmux's
/// paste buffer, which tmux passes on to the terminal it runs in (or, from a
/// tmux too old for that, the OSC 52 wrapped to pass through it).
pub fn copy_to_clipboard(text: &str) {
    // Unit tests read what was copied instead of replacing the clipboard of
    // whoever runs them.
    #[cfg(test)]
    {
        COPIED.with(|copied| copied.borrow_mut().push(text.to_owned()));
    }
    #[cfg(not(test))]
    copy_for_real(text);
}

#[cfg(test)]
thread_local! {
    static COPIED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// What this test thread copied since it last asked.
#[cfg(test)]
pub(crate) fn take_copied() -> Vec<String> {
    COPIED.with(|copied| std::mem::take(&mut *copied.borrow_mut()))
}

#[cfg(not(test))]
fn copy_for_real(text: &str) {
    static WORKERS: std::sync::OnceLock<ClipboardWorkers> = std::sync::OnceLock::new();
    let tmux = in_tmux();
    let commands = clipboard_commands(std::env::consts::OS, tmux);
    let workers = (!commands.is_empty()).then(|| {
        WORKERS.get_or_init(|| {
            let term_program = std::env::var_os("TERM_PROGRAM");
            let refuses_w = tmux_refuses_w_until_known(term_program.as_deref());
            ClipboardWorkers::start(commands, CLIPBOARD_HELPER_WAIT, None, refuses_w)
        })
    });
    let encoded = base64(text.as_bytes());
    // Cap pathological sizes; terminals commonly reject huge OSC payloads.
    if encoded.len() < 100_000 {
        let wrapped = workers
            .is_some_and(|workers| wrap_for_tmux(tmux, in_screen(), workers.tmux_refuses_w()));
        let _ = write!(io::stdout(), "{}", clipboard_sequences(&encoded, wrapped));
        let _ = io::stdout().flush();
    }
    if let Some(workers) = workers {
        workers.copy(text);
    }
}

/// How long a clipboard helper may take before it is killed.
#[cfg(not(test))]
const CLIPBOARD_HELPER_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Whether the TUI runs inside tmux, which names its server in `TMUX`.
#[cfg(not(test))]
fn in_tmux() -> bool {
    std::env::var_os("TMUX").is_some_and(|value| !value.is_empty())
}

/// Whether the TUI runs inside GNU screen, which names its session in `STY`
/// (inherited by a tmux started in it, and by a screen started in tmux).
#[cfg(not(test))]
fn in_screen() -> bool {
    std::env::var_os("STY").is_some_and(|value| !value.is_empty())
}

/// Whether a copy's OSC 52 also goes wrapped to pass through tmux: only
/// inside tmux, only while this tmux is taken to refuse `load-buffer -w`
/// (older than 3.2), when nothing else reaches the terminal around it, and
/// never inside GNU screen, which hands the wrapper on as it is, so that
/// `tmux;` lands on screen as text and shifts the row being drawn.
fn wrap_for_tmux(in_tmux: bool, in_screen: bool, tmux_refuses_w: bool) -> bool {
    in_tmux && !in_screen && tmux_refuses_w
}

/// Whether this tmux is taken to refuse `load-buffer -w` until a copy finds
/// out, from the `TERM_PROGRAM` its panes get: tmux 3.2, the first with
/// `-w`, is also the first to set it to `tmux`, so a tmux that leaves it
/// alone is taken to be older and gets the wrapped OSC 52 from the first
/// copy on.
fn tmux_refuses_w_until_known(term_program: Option<&std::ffi::OsStr>) -> bool {
    term_program != Some(std::ffi::OsStr::new("tmux"))
}

/// What a copy writes to the terminal: OSC 52 carrying `encoded` (the text
/// in base64) and, when `wrapped` (see [`wrap_for_tmux`]), the same sequence
/// again, wrapped to pass through tmux to the terminal it runs in. Under its
/// default `set-clipboard external` tmux drops the bare form, and a tmux
/// before 3.3 lets the wrapped one through always. Where `load-buffer -w`
/// works the wrapped form is left out: a newer tmux would drop it unless
/// `allow-passthrough` is on, and otherwise the terminal would be handed the
/// copy twice, which a terminal that asks before each clipboard write would
/// ask about twice.
fn clipboard_sequences(encoded: &str, wrapped: bool) -> String {
    let bare = format!("\x1b]52;c;{encoded}\x07");
    if !wrapped {
        return bare;
    }
    // Every ESC inside the wrapper is doubled.
    let wrapped = bare.replace('\x1b', "\x1b\x1b");
    format!("{bare}\x1bPtmux;{wrapped}\x1b\\")
}

/// tmux's paste buffer, filled from stdin. Under tmux's default
/// `set-clipboard external` the OSC 52 a program in a pane writes is dropped,
/// while `-w` has tmux pass the text on to the clipboard of the terminal tmux
/// runs in, whatever `set-clipboard` says: over SSH, the one on the person's
/// own machine. A tmux older than 3.2 refuses `-w` and gets the plain form,
/// which fills its paste buffer alone.
const TMUX_PASTE_BUFFER: &[&[&str]] = &[
    &["tmux", "load-buffer", "-w", "-"],
    &["tmux", "load-buffer", "-"],
];

/// Where a copy goes on `os` (as [`std::env::consts::OS`] names it): the
/// native helpers of [`clipboard_helpers`] and, inside tmux (`in_tmux`),
/// [`TMUX_PASTE_BUFFER`] whatever the native helpers did, since the clipboard
/// of the machine the TUI runs on is not the one in front of a person who
/// reached it over SSH. Each group's helpers are tried in turn until one
/// takes the text.
fn clipboard_commands(os: &str, in_tmux: bool) -> Vec<&'static [&'static [&'static str]]> {
    let mut commands = vec![clipboard_helpers(os)];
    if in_tmux {
        commands.push(TMUX_PASTE_BUFFER);
    }
    commands.retain(|group| !group.is_empty());
    commands
}

/// The threads that hand copies to the clipboard helpers, off the TUI's
/// thread: one for each group of [`clipboard_commands`], so a helper that
/// hangs (until it is killed after `wait`) holds up no other group, this
/// copy's or a later one's. A worker still busy when copies come takes only
/// the newest of them next. Whether this tmux refuses `load-buffer -w`
/// starts as a guess, which the tmux worker corrects with what each copy
/// finds.
#[cfg_attr(all(test, not(unix)), allow(dead_code))]
struct ClipboardWorkers {
    queues: Vec<std::sync::mpsc::Sender<String>>,
    tmux_refuses_w: std::sync::Arc<AtomicBool>,
}

#[cfg_attr(all(test, not(unix)), allow(dead_code))]
impl ClipboardWorkers {
    /// Start a worker for each group of `commands`, taking this tmux to
    /// refuse `load-buffer -w` (`tmux_refuses_w`) until a copy finds out.
    /// `path`, when given, is searched for the helpers instead of `PATH`.
    fn start(
        commands: Vec<&'static [&'static [&'static str]]>,
        wait: std::time::Duration,
        path: Option<std::ffi::OsString>,
        tmux_refuses_w: bool,
    ) -> Self {
        let tmux_refuses_w = std::sync::Arc::new(AtomicBool::new(tmux_refuses_w));
        let queues = commands.into_iter().filter_map(|group| {
            let (sender, receiver) = std::sync::mpsc::channel::<String>();
            let path = path.clone();
            let refuses_w = (group == TMUX_PASTE_BUFFER).then(|| tmux_refuses_w.clone());
            std::thread::Builder::new()
                .name("clipboard".into())
                .spawn(move || {
                    while let Ok(mut text) = receiver.recv() {
                        while let Ok(newer) = receiver.try_recv() {
                            text = newer;
                        }
                        let taker = group.iter().position(|helper| {
                            run_clipboard_helper(helper, &text, wait, path.as_deref())
                        });
                        // `load-buffer -w` took it, or the plain form after
                        // `-w` was refused.
                        if let (Some(taker @ (0 | 1)), Some(refuses_w)) = (taker, &refuses_w) {
                            refuses_w.store(taker == 1, Ordering::Relaxed);
                        }
                    }
                })
                .ok()
                .map(|_| sender)
        });
        Self {
            queues: queues.collect(),
            tmux_refuses_w,
        }
    }

    fn copy(&self, text: &str) {
        for queue in &self.queues {
            let _ = queue.send(text.to_owned());
        }
    }

    /// Whether this tmux refuses `load-buffer -w`, as the last copy found
    /// or, before any did, as guessed.
    fn tmux_refuses_w(&self) -> bool {
        self.tmux_refuses_w.load(Ordering::Relaxed)
    }
}

/// The clipboard tools tried in turn on `os` (as [`std::env::consts::OS`]
/// names it), each reading the text on stdin so it never shows on a command
/// line. X11's is its CLIPBOARD selection, the one Ctrl+V and Ctrl+Shift+V
/// paste, not PRIMARY, which a middle click pastes. Windows has OSC 52 only.
fn clipboard_helpers(os: &str) -> &'static [&'static [&'static str]] {
    match os {
        "macos" => &[&["pbcopy"]],
        "windows" => &[],
        _ => &[
            &["wl-copy"],
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
        ],
    }
}

/// Hand `text` to the clipboard tool `command` on its stdin; true when it took
/// it. A tool that has not exited after `wait` is killed. `path`, when given,
/// is searched for the tool instead of `PATH`.
#[cfg_attr(all(test, not(unix)), allow(dead_code))]
fn run_clipboard_helper(
    command: &[&str],
    text: &str,
    wait: std::time::Duration,
    path: Option<&std::ffi::OsStr>,
) -> bool {
    use std::process::{Command, Stdio};
    let Some((program, arguments)) = command.split_first() else {
        return false;
    };
    let mut tool = Command::new(program);
    tool.args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(path) = path {
        tool.env("PATH", path);
    }
    let Ok(mut child) = tool.spawn() else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        // Written beside the wait: a tool that stops reading cannot hold it
        // past `wait`, and once killed its pipe ends the write. A tool that
        // exits without reading says by its exit whether it took the text.
        let text = text.to_owned();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    let deadline = std::time::Instant::now() + wait;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(not(test))]
fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let bytes = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let value = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        out.push(TABLE[(value >> 18) as usize & 63] as char);
        out.push(TABLE[(value >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(value >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[value as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Whether the TUI owns the terminal now (raw mode, its alternate screen).
static ON_SCREEN: AtomicBool = AtomicBool::new(false);

/// Whether the TUI is drawing on this terminal now. Anything else written to
/// it meanwhile stays on screen over the TUI, which redraws only the cells it
/// changed.
pub(crate) fn on_screen() -> bool {
    ON_SCREEN.load(Ordering::Relaxed)
}

/// Whether the TUI has handed the terminal to a program of the person's
/// (`$EDITOR`) and takes it back when that exits.
static LENT: AtomicBool = AtomicBool::new(false);

/// The terminal handed to `$EDITOR` while this lives: text written to it
/// meanwhile would land in the editor's screen.
pub(crate) struct LentTerminal;

impl LentTerminal {
    pub(crate) fn new() -> Self {
        LENT.store(true, Ordering::Relaxed);
        Self
    }
}

impl Drop for LentTerminal {
    fn drop(&mut self) {
        LENT.store(false, Ordering::Relaxed);
    }
}

/// The terminal's settings as the shell uses them, read once the TUI has
/// left raw mode to lend the terminal to a program, and set again when the
/// program is done. A program killed in raw mode leaves raw mode behind,
/// and the TUI's own raw mode would otherwise take that for the shell's and
/// put it back when the TUI exits.
pub(crate) struct Settings {
    #[cfg(unix)]
    saved: Option<nix::sys::termios::Termios>,
}

impl Settings {
    pub(crate) fn now() -> Self {
        #[cfg(unix)]
        let saved = if cfg!(test) {
            None
        } else {
            std::fs::File::open("/dev/tty")
                .ok()
                .and_then(|tty| nix::sys::termios::tcgetattr(&tty).ok())
        };
        Self {
            #[cfg(unix)]
            saved,
        }
    }

    /// Set the settings read by [`Self::now`] again, and pop a keyboard mode
    /// the program may have pushed and not popped.
    #[cfg_attr(not(unix), allow(clippy::unused_self))] // the settings are Unix's
    pub(crate) fn put_back(&self) {
        // Unit tests that lend the terminal never touch the real one.
        if cfg!(test) {
            return;
        }
        #[cfg(unix)]
        if let Some(saved) = &self.saved
            && let Ok(tty) = std::fs::File::open("/dev/tty")
        {
            let _ = nix::sys::termios::tcsetattr(&tty, nix::sys::termios::SetArg::TCSANOW, saved);
        }
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
}

/// Whether the TUI, or a program it handed the terminal to, is drawing on it.
pub(crate) fn in_use() -> bool {
    on_screen() || LENT.load(Ordering::Relaxed)
}

/// Restore the terminal to a usable state. Safe to call more than once and
/// from a panic hook: every step is best-effort and none can panic.
pub fn restore_terminal() {
    ON_SCREEN.store(false, Ordering::Relaxed);
    // Unit tests that hand the terminal over never touch the real one.
    if cfg!(test) {
        return;
    }
    // Clear the title we set so the shell's own title comes back.
    let _ = write!(io::stdout(), "\x1b]0;\x07");
    let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableFocusChange,
        DisableBracketedPaste,
        Show,
        LeaveAlternateScreen
    );
    // Belt and braces: explicitly clear every mouse-tracking mode a crashed
    // session might have left armed, so the shell keeps native selection.
    let _ = write!(io::stdout(), "{MOUSE_TRACKING_DISABLE}");
    let _ = io::stdout().flush();
}

/// Chain terminal restoration in front of the default panic handler, so a
/// panic that ends the TUI prints its message to a working terminal instead
/// of wedging raw mode + alternate screen.
fn install_panic_hook() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !panic_ends_the_tui(on_screen(), std::thread::current().name()) {
                log_panic(info);
                return;
            }
            restore_terminal();
            default_hook(info);
        }));
    });
}

/// The TUI's loop runs on the main thread; a panic elsewhere (a background
/// task, which tokio catches, or a thread of its own) ends only that work
/// while the TUI keeps drawing. Restoring the terminal then left the TUI
/// running on a cooked, scrolling screen, and the message over it.
fn panic_ends_the_tui(on_screen: bool, thread: Option<&str>) -> bool {
    !on_screen || thread == Some("main")
}

/// A panic the TUI lives on: in the log only, with where it happened, as the
/// CLI's own hook writes every other one.
fn log_panic(info: &std::panic::PanicHookInfo<'_>) {
    let message = info
        .payload()
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| info.payload().downcast_ref::<&str>().copied())
        .unwrap_or_default();
    tracing::error!(
        location = info.location().map(ToString::to_string).as_deref(),
        thread = std::thread::current().name(),
        "panic: {}",
        message.chars().take(500).collect::<String>()
    );
}

impl TerminalGuard {
    pub fn enter() -> Result<Self> {
        install_panic_hook();
        activate_terminal()?;
        Ok(Self)
    }
}

/// Re-arm raw mode, the alternate screen, paste/mouse tracking, and enhanced
/// keyboard reporting after an external program or SIGCONT returns control.
pub fn resume_terminal() -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    activate_terminal()
}

fn activate_terminal() -> Result<()> {
    // Repair mouse modes leaked by an abnormally terminated prior run
    // before taking ownership of the terminal.
    write!(io::stdout(), "{MOUSE_TRACKING_DISABLE}")?;
    io::stdout().flush()?;
    enable_raw_mode()?;
    let entered = (|| -> Result<()> {
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            Hide,
            EnableBracketedPaste,
            EnableFocusChange
        )?;
        // Button, drag, and any-motion tracking with SGR encoding. Motion
        // is required for hover/focus feedback; the event
        // loop only mutates state when the hovered target actually changes.
        write!(io::stdout(), "{MOUSE_TRACKING_ENABLE}{NUMERIC_KEYPAD}")?;
        io::stdout().flush()?;
        Ok(())
    })();
    if let Err(error) = entered {
        restore_terminal();
        return Err(error);
    }
    ON_SCREEN.store(true, Ordering::Relaxed);
    // Kitty keyboard protocol disambiguates Shift+Enter from Enter and
    // Ctrl+M from the models shortcut where the terminal supports it.
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        let _ = execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    KEYBOARD_ENHANCED.store(enhanced, Ordering::Relaxed);
    Ok(())
}

static KEYBOARD_ENHANCED: AtomicBool = AtomicBool::new(false);

/// Whether the terminal reports modified keys unambiguously. Without it,
/// tmux and Terminal.app send Ctrl+Shift+T as plain Ctrl+T, so hints must not
/// promise Shift chords.
pub fn keyboard_enhanced() -> bool {
    KEYBOARD_ENHANCED.load(Ordering::Relaxed)
}

/// Re-emit terminal modes that terminal emulators may silently clear while
/// their tab/window is unfocused. OpenTUI performs the same repair on focus-in;
/// without it hover can work at launch and then disappear after an app switch.
pub fn restore_focus_modes() -> Result<()> {
    write!(
        io::stdout(),
        "{MOUSE_TRACKING_ENABLE}\x1b[?1004h\x1b[?2004h"
    )?;
    io::stdout().flush()?;
    Ok(())
}

/// Ctrl+Z lifecycle: return the terminal to the shell, stop
/// the entire foreground process group, and rebuild terminal modes when the
/// shell resumes the job with `fg`/SIGCONT.
#[cfg(unix)]
pub fn suspend_and_resume() -> Result<()> {
    use nix::{
        sys::signal::{Signal, killpg},
        unistd::getpgrp,
    };

    restore_terminal();
    let stopped = killpg(getpgrp(), Signal::SIGTSTP);
    // `killpg` returns here only after SIGCONT. Always try to recover terminal
    // ownership, even if the signal call itself reported an error.
    let resumed = resume_terminal();
    resumed.context("resume terminal after SIGCONT")?;
    stopped.context("suspend foreground process group")
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // same signature as on Unix, where it can fail
pub fn suspend_and_resume() -> Result<()> {
    Ok(())
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // restore_terminal pops enhancement flags unconditionally; that is
        // harmless on terminals where they were never pushed.
        restore_terminal();
    }
}

/// Hand a URL to the desktop's default browser without waiting on it. False
/// when there is no launcher to try, or it refused; the caller keeps showing
/// the link either way.
pub fn open_browser(url: &str) -> bool {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return false;
    }
    launch(std::ffi::OsStr::new(url))
}

/// Open a file this client wrote, such as an HTML report, with the desktop's
/// default application for it.
pub fn open_file(path: &std::path::Path) -> bool {
    path.is_file() && launch(path.as_os_str())
}

fn launch(target: &std::ffi::OsStr) -> bool {
    use std::process::{Command, Stdio};
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(target);
        command
    };
    // cmd ends a command at the first `&`, so a web address goes to the URL
    // protocol handler whole; files and folders still open through `start`.
    #[cfg(target_os = "windows")]
    let mut command = {
        let web = target
            .to_str()
            .is_some_and(|text| text.starts_with("https://") || text.starts_with("http://"));
        let mut command = if web {
            let mut command = Command::new("rundll32.exe");
            command.arg("url.dll,FileProtocolHandler");
            command
        } else {
            let mut command = Command::new("cmd");
            command.args(["/C", "start", ""]);
            command
        };
        command.arg(target);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(target);
        command
    };
    #[cfg(not(any(unix, target_os = "windows")))]
    return false;
    #[allow(unreachable_code)]
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::{ClipboardWorkers, run_clipboard_helper};
    use super::{
        MOUSE_TRACKING_DISABLE, MOUSE_TRACKING_ENABLE, NUMERIC_KEYPAD, clipboard_commands,
        clipboard_helpers, clipboard_sequences, osc_text, panic_ends_the_tui,
        parse_background_variant, parse_terminal_rgb, tmux_refuses_w_until_known, wrap_for_tmux,
    };

    /// X11 gets the CLIPBOARD selection that Ctrl+Shift+V pastes, from xclip
    /// or else xsel, after Wayland's wl-copy; macOS pbcopy; Windows OSC 52
    /// alone. The Linux tools are not run here (this runs on macOS too).
    #[test]
    fn clipboard_helpers_copy_to_the_clipboard_selection() {
        assert_eq!(clipboard_helpers("macos"), [["pbcopy"].as_slice()]);
        assert!(clipboard_helpers("windows").is_empty());
        for os in ["linux", "freebsd"] {
            assert_eq!(
                clipboard_helpers(os),
                [
                    ["wl-copy"].as_slice(),
                    ["xclip", "-selection", "clipboard"].as_slice(),
                    ["xsel", "--clipboard", "--input"].as_slice(),
                ]
            );
        }
    }

    /// A helper gets the text on stdin; one that never exits is killed at
    /// the limit instead of holding the copy, also with text larger than a
    /// pipe holds; one that is missing or fails says so.
    #[cfg(unix)]
    #[test]
    fn a_clipboard_helper_gets_the_text_and_one_that_hangs_is_killed() {
        use std::time::{Duration, Instant};
        let directory =
            std::env::temp_dir().join(format!("haarness-clipboard-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&directory).unwrap();
        let sink = directory.join("copied");
        let script = format!("cat > '{}'", sink.display());
        assert!(run_clipboard_helper(
            &["sh", "-c", &script],
            "copied text",
            Duration::from_secs(10),
            None
        ));
        assert_eq!(std::fs::read_to_string(&sink).unwrap(), "copied text");

        let large = "x".repeat(1 << 20);
        for text in ["short", large.as_str()] {
            let started = Instant::now();
            assert!(!run_clipboard_helper(
                &["sh", "-c", "sleep 30"],
                text,
                Duration::from_millis(200),
                None
            ));
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{:?}",
                started.elapsed()
            );
        }
        assert!(!run_clipboard_helper(
            &["sh", "-c", "exit 1"],
            "x",
            Duration::from_secs(5),
            None
        ));
        assert!(!run_clipboard_helper(
            &["haarness-no-such-clipboard-tool"],
            "x",
            Duration::from_secs(5),
            None
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// Inside tmux, and only there, a copy also goes to tmux's paste buffer,
    /// beside the native helpers, or alone where there are none (Windows).
    #[test]
    fn inside_tmux_a_copy_also_goes_to_its_paste_buffer() {
        let tmux = [
            ["tmux", "load-buffer", "-w", "-"].as_slice(),
            ["tmux", "load-buffer", "-"].as_slice(),
        ];
        for os in ["macos", "linux"] {
            assert_eq!(clipboard_commands(os, false), [clipboard_helpers(os)]);
            assert_eq!(
                clipboard_commands(os, true),
                [clipboard_helpers(os), tmux.as_slice()]
            );
        }
        assert!(clipboard_commands("windows", false).is_empty());
        assert_eq!(clipboard_commands("windows", true), [tmux.as_slice()]);
    }

    /// What the stand-ins copy: a shell line with both kinds of quote, a
    /// line that starts like an option, and letters outside ASCII.
    #[cfg(unix)]
    const COPIED: &str = "echo \"$HOME\" 'naïve café'\n--target-client=Ωmega";

    /// Stand-in helpers in a folder of their own, which is the whole search
    /// path a copy gets, so neither the clipboard nor a tmux server of the
    /// person running the tests is reached. Each adds its arguments as a line
    /// to `<name>.runs` beside it before doing what it was given.
    #[cfg(unix)]
    struct StandIns(std::path::PathBuf);

    /// Reads the text into `<name>.input` and exits 0.
    #[cfg(unix)]
    const TAKES: &str = "exec /bin/cat > \"$0.input\"";
    /// A tmux older than 3.2, which knows no `load-buffer -w`.
    #[cfg(unix)]
    const REFUSES_W: &str =
        "[ \"$1 $2\" = 'load-buffer -w' ] && exit 1\nexec /bin/cat > \"$0.input\"";
    #[cfg(unix)]
    const FAILS: &str = "exit 1";
    /// Reads the text, then runs on until `<name>.release` appears (or the
    /// stand-ins' folder is gone, so a failed test leaves nothing running).
    #[cfg(unix)]
    const WAITS: &str = "/bin/cat > \"$0.input\"
while [ ! -e \"$0.release\" ] && [ -e \"$0\" ]; do /bin/sleep 0.02; done";

    #[cfg(unix)]
    impl StandIns {
        fn new() -> Self {
            let directory =
                std::env::temp_dir().join(format!("haarness-clipboard-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(&directory).unwrap();
            Self(directory)
        }

        fn add(&self, name: &str, then: &str) {
            use std::os::unix::fs::PermissionsExt;
            let path = self.0.join(name);
            std::fs::write(
                &path,
                format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$0.runs\"\n{then}\n"),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        fn runs(&self, name: &str) -> Vec<String> {
            std::fs::read_to_string(self.0.join(format!("{name}.runs")))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn input(&self, name: &str) -> Option<String> {
            std::fs::read_to_string(self.0.join(format!("{name}.input"))).ok()
        }

        /// The workers a copy on `os` inside a tmux of 3.2 or later gets,
        /// with these stand-ins as the whole search path.
        fn workers(&self, os: &str) -> ClipboardWorkers {
            self.workers_guessing(os, false)
        }

        /// The same, taking tmux to refuse `load-buffer -w` (`refuses_w`)
        /// until a copy finds out.
        fn workers_guessing(&self, os: &str, refuses_w: bool) -> ClipboardWorkers {
            ClipboardWorkers::start(
                clipboard_commands(os, true),
                std::time::Duration::from_secs(60),
                Some(self.0.clone().into_os_string()),
                refuses_w,
            )
        }
    }

    /// Whether `done` holds within 20 seconds: the workers hand copies over
    /// on threads of their own.
    #[cfg(unix)]
    fn until(done: impl Fn() -> bool) -> bool {
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(20) {
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        done()
    }

    #[cfg(unix)]
    impl Drop for StandIns {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// tmux reads the copy from stdin, its arguments `load-buffer -w -` and
    /// nothing more, for a few words and for some hundreds of kilobytes
    /// alike, after every native helper failed and after one took the copy.
    #[cfg(unix)]
    #[test]
    fn tmux_reads_the_copy_from_stdin_beside_any_native_helper() {
        let tools = StandIns::new();
        for helper in ["wl-copy", "xclip", "xsel"] {
            tools.add(helper, FAILS);
        }
        tools.add("tmux", TAKES);
        let workers = tools.workers("linux");
        let long = format!("{COPIED}\n").repeat(9_000);
        for (copies, text) in [COPIED, long.as_str()].into_iter().enumerate() {
            workers.copy(text);
            assert!(until(|| tools.input("tmux").as_deref() == Some(text)));
            assert!(until(|| tools.runs("xsel").len() == copies + 1));
        }
        assert_eq!(tools.runs("tmux"), ["load-buffer -w -"; 2]);
        assert_eq!(tools.runs("xclip"), ["-selection clipboard"; 2]);
        assert_eq!(tools.runs("xsel"), ["--clipboard --input"; 2]);

        let tools = StandIns::new();
        tools.add("pbcopy", TAKES);
        tools.add("tmux", TAKES);
        tools.workers("macos").copy(COPIED);
        assert!(until(|| tools.input("pbcopy").as_deref() == Some(COPIED)));
        assert!(until(|| tools.input("tmux").as_deref() == Some(COPIED)));
        assert_eq!(tools.runs("tmux"), ["load-buffer -w -"]);
    }

    /// A tmux that refuses `-w` gets the plain `load-buffer -`; one that
    /// fails both, or none at all, leaves the native helper's copy as it was.
    #[cfg(unix)]
    #[test]
    fn a_tmux_that_refuses_or_fails_leaves_the_copy_to_the_rest() {
        for (tmux, runs, input) in [
            (
                Some(REFUSES_W),
                ["load-buffer -w -", "load-buffer -"].as_slice(),
                Some(COPIED),
            ),
            (
                Some(FAILS),
                ["load-buffer -w -", "load-buffer -"].as_slice(),
                None,
            ),
            (None, [].as_slice(), None),
        ] {
            let tools = StandIns::new();
            tools.add("pbcopy", TAKES);
            if let Some(tmux) = tmux {
                tools.add("tmux", tmux);
            }
            tools.workers("macos").copy(COPIED);
            assert!(until(|| tools.input("pbcopy").as_deref() == Some(COPIED)));
            assert!(until(|| tools.runs("tmux").len() == runs.len()));
            assert!(until(|| tools.input("tmux").as_deref() == input));
            assert_eq!(tools.runs("tmux"), runs);
        }
    }

    /// A native helper still running holds up no copy to tmux, neither its
    /// own nor the ones after it, and once it ends its worker goes on with
    /// only the newest copy that came meanwhile.
    #[cfg(unix)]
    #[test]
    fn a_native_helper_still_running_holds_up_no_copy_to_tmux() {
        let tools = StandIns::new();
        tools.add("wl-copy", WAITS);
        tools.add("tmux", TAKES);
        let workers = tools.workers("linux");
        workers.copy("first copy");
        assert!(until(
            || tools.input("wl-copy").as_deref() == Some("first copy")
        ));
        for text in ["first copy", "second copy", "third copy"] {
            if text != "first copy" {
                workers.copy(text);
            }
            assert!(
                until(|| tools.input("tmux").as_deref() == Some(text)),
                "{text:?} waited for the native helper"
            );
        }
        assert_eq!(tools.runs("wl-copy").len(), 1);
        std::fs::write(tools.0.join("wl-copy.release"), "").unwrap();
        assert!(until(
            || tools.input("wl-copy").as_deref() == Some("third copy")
        ));
        assert_eq!(tools.runs("wl-copy").len(), 2);
        assert_eq!(tools.runs("tmux"), ["load-buffer -w -"; 3]);
    }

    /// The wrapped OSC 52 follows the bare one, every ESC inside the wrapper
    /// doubled; without the need, the bare one goes alone.
    #[test]
    fn a_wrapped_osc_52_follows_the_bare_one_only_when_asked() {
        assert_eq!(clipboard_sequences("aGk=", false), "\x1b]52;c;aGk=\x07");
        assert_eq!(
            clipboard_sequences("aGk=", true),
            "\x1b]52;c;aGk=\x07\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\"
        );
    }

    /// The OSC 52 goes wrapped only from inside tmux, outside GNU screen,
    /// once this tmux has refused `load-buffer -w`.
    #[test]
    fn the_osc_52_goes_wrapped_only_where_tmux_cannot_pass_a_copy_on() {
        assert!(wrap_for_tmux(true, false, true));
        assert!(!wrap_for_tmux(true, false, false));
        assert!(!wrap_for_tmux(true, true, true));
        assert!(!wrap_for_tmux(false, false, true));
    }

    /// Before any copy, a tmux that set `TERM_PROGRAM=tmux` (3.2 and later)
    /// is taken to take `load-buffer -w`; any other is taken to be older.
    #[test]
    fn term_program_says_whether_tmux_is_new_enough_for_w() {
        use std::ffi::OsStr;
        assert!(!tmux_refuses_w_until_known(Some(OsStr::new("tmux"))));
        assert!(tmux_refuses_w_until_known(Some(OsStr::new("iTerm.app"))));
        assert!(tmux_refuses_w_until_known(None));
    }

    /// Whatever the guess, the copy that finds out sets it right: a tmux
    /// that takes `load-buffer -w` gets the bare OSC 52 alone, one that
    /// refuses it gets the wrapped one too (never in screen).
    #[cfg(unix)]
    #[test]
    fn each_copy_sets_right_whether_tmux_refuses_w() {
        for (guess, tmux, refuses) in [
            (false, TAKES, false),
            (true, TAKES, false),
            (false, REFUSES_W, true),
            (true, REFUSES_W, true),
        ] {
            let tools = StandIns::new();
            tools.add("pbcopy", TAKES);
            tools.add("tmux", tmux);
            let workers = tools.workers_guessing("macos", guess);
            assert_eq!(wrap_for_tmux(true, false, workers.tmux_refuses_w()), guess);
            workers.copy(COPIED);
            assert!(until(|| tools.input("tmux").as_deref() == Some(COPIED)));
            assert!(until(|| workers.tmux_refuses_w() == refuses));
            assert_eq!(
                wrap_for_tmux(true, false, workers.tmux_refuses_w()),
                refuses
            );
            assert!(!wrap_for_tmux(true, true, workers.tmux_refuses_w()));
        }
    }

    #[test]
    fn parses_terminal_background_formats_and_selects_readable_variant() {
        assert_eq!(
            parse_background_variant("\x1b]11;rgb:ffff/ffff/ffff\x07"),
            Some("light")
        );
        assert_eq!(
            parse_background_variant("noise\x1b]11;rgb:0800/1000/1800\x1b\\"),
            Some("dark")
        );
        assert_eq!(
            parse_background_variant("\x1b]11;#f7f4ee\x07"),
            Some("light")
        );
        assert_eq!(parse_terminal_rgb("rgb(15, 14, 13)"), Some((15, 14, 13)));
        assert_eq!(parse_terminal_rgb("rgb:ff/80/00"), Some((255, 128, 0)));
        assert_eq!(parse_background_variant("\x1b]10;#ffffff\x07"), None);
        assert_eq!(parse_terminal_rgb("rgb:not/a/color"), None);
    }

    #[test]
    fn osc_payloads_drop_every_control_character() {
        // A sequence goes whole; one that never ends hides the rest of its
        // line, and the next line still shows.
        assert_eq!(
            osc_text("safe\x07\x1b]9;injected\nnext\u{009c}", 120),
            "safenext"
        );
        assert_eq!(
            osc_text("\x1b]0;owned\x07\x1b]52;c;aGk=\x07Escaped", 200),
            "Escaped"
        );
    }

    #[test]
    fn mouse_mode_order_matches_any_motion_tracking_contract() {
        let normal = MOUSE_TRACKING_ENABLE.find("?1000h").unwrap();
        let drag = MOUSE_TRACKING_ENABLE.find("?1002h").unwrap();
        let motion = MOUSE_TRACKING_ENABLE.find("?1003h").unwrap();
        let sgr = MOUSE_TRACKING_ENABLE.find("?1006h").unwrap();
        assert!(normal < drag && drag < motion && motion < sgr);
        assert!(MOUSE_TRACKING_DISABLE.contains("?1003l"));
        assert!(MOUSE_TRACKING_DISABLE.contains("?1006l"));
    }

    /// The keypad types and sends like the main keys whatever mode the last
    /// program left it in: numeric mode, never application mode.
    #[test]
    fn entering_the_screen_puts_the_keypad_in_numeric_mode() {
        assert_eq!(NUMERIC_KEYPAD, "\x1b>");
        assert!(!NUMERIC_KEYPAD.contains("\x1b="));
    }

    #[test]
    fn only_a_panic_on_the_tuis_own_thread_gives_the_terminal_back() {
        // The loop's thread: the TUI ends, so the message goes to a working
        // terminal.
        assert!(panic_ends_the_tui(true, Some("main")));
        // A background task or thread: the TUI keeps drawing on its screen.
        assert!(!panic_ends_the_tui(true, Some("tokio-runtime-worker")));
        assert!(!panic_ends_the_tui(true, None));
        // Not on screen (before the first frame, after it exits, while an
        // editor or the shell has the terminal): printed as before.
        assert!(panic_ends_the_tui(false, Some("tokio-runtime-worker")));
    }

    /// Run in a process of its own (it installs the process's panic hook):
    /// the TUI on screen, a background thread panics, and the TUI must still
    /// own its screen, with nothing printed over it.
    #[test]
    fn a_background_panic_leaves_the_tui_on_its_screen() {
        if std::env::var_os("HAARNESS_TUI_PANIC_PROBE").is_some() {
            super::ON_SCREEN.store(true, std::sync::atomic::Ordering::Relaxed);
            super::install_panic_hook();
            let _ = std::thread::Builder::new()
                .name("background-task".into())
                .spawn(|| panic!("background failure"))
                .unwrap()
                .join();
            println!("on screen after the panic: {}", super::on_screen());
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "terminal::tests::a_background_panic_leaves_the_tui_on_its_screen",
                "--nocapture",
            ])
            .env("HAARNESS_TUI_PANIC_PROBE", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains("on screen after the panic: true"),
            "{stdout}\n{stderr}"
        );
        assert!(
            !stdout.contains("\x1b[?1049l"),
            "the terminal was given back"
        );
        assert!(!stderr.contains("background failure"), "{stderr}");
    }
}
