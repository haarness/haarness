//! Raw-mode/alternate-screen ownership with reliable cleanup.

use anyhow::{Context, Result};
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

#[cfg(unix)]
struct RawProbeGuard;

#[cfg(unix)]
impl Drop for RawProbeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn osc_text(value: &str, limit: usize) -> String {
    value
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

/// Copy text for the user: OSC 52 (works over SSH and inside tmux with
/// passthrough) plus the native clipboard tool when one exists.
pub fn copy_to_clipboard(text: &str) {
    let encoded = base64(text.as_bytes());
    // Cap pathological sizes; terminals commonly reject huge OSC payloads.
    if encoded.len() < 100_000 {
        let _ = write!(io::stdout(), "\x1b]52;c;{encoded}\x07");
        let _ = io::stdout().flush();
    }
    #[cfg(target_os = "macos")]
    native_copy("pbcopy", text);
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if !native_copy("wl-copy", text) {
            native_copy("xclip", text);
        }
    }
}

#[cfg(unix)]
fn native_copy(tool: &str, text: &str) -> bool {
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new(tool)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().map(|status| status.success()).unwrap_or(false)
}

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

/// Restore the terminal to a usable state. Safe to call more than once and
/// from a panic hook: every step is best-effort and none can panic.
pub fn restore_terminal() {
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
/// panic prints its message to a working terminal instead of wedging raw
/// mode + alternate screen.
fn install_panic_hook() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            default_hook(info);
        }));
    });
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
        write!(io::stdout(), "{MOUSE_TRACKING_ENABLE}")?;
        io::stdout().flush()?;
        Ok(())
    })();
    if let Err(error) = entered {
        restore_terminal();
        return Err(error);
    }
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
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]).arg(target);
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
    use super::{
        MOUSE_TRACKING_DISABLE, MOUSE_TRACKING_ENABLE, osc_text, parse_background_variant,
        parse_terminal_rgb,
    };

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
        assert_eq!(
            osc_text("safe\x07\x1b]9;injected\nnext\u{009c}", 120),
            "safe]9;injectednext"
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
}
