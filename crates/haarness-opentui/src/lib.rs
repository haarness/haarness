//! Audited `OpenTUI` FFI boundary and verified one-binary extraction.

use fs2::FileExt;
use libloading::Library;
use sha2::{Digest, Sha256};
use std::{
    ffi::c_void,
    fs::{self, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};
use thiserror::Error;

pub const OPENTUI_VERSION: &str = "0.5.9";
pub const REQUIRED_SYMBOLS: &[&str] = &[
    "createRenderer",
    "setTerminalEnvVar",
    "setCursorPosition",
    "destroyRenderer",
    "getTerminalCapabilities",
    "getNextBuffer",
    "bufferClear",
    "bufferDrawText",
    "bufferGetFgPtr",
    "bufferGetBgPtr",
    "bufferWriteResolvedChars",
    "render",
    "resizeRenderer",
];
const ARCHIVE: &[u8] = include_bytes!(env!("HAARNESS_OPENTUI_ARCHIVE"));
const ARCHIVE_SHA: &str = env!("HAARNESS_OPENTUI_ARCHIVE_SHA");
const LIBRARY_SHA: &str = env!("HAARNESS_OPENTUI_LIBRARY_SHA");
const LIBRARY_NAME: &str = env!("HAARNESS_OPENTUI_LIBRARY_NAME");
const NATIVE_TARGET: &str = env!("HAARNESS_OPENTUI_TARGET");

#[derive(Debug, Error)]
pub enum OpenTuiError {
    #[error("OpenTUI I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("OpenTUI archive error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("OpenTUI native load error: {0}")]
    Load(#[from] libloading::Error),
    #[error("OpenTUI integrity failure: {0}")]
    Integrity(String),
    #[error("OpenTUI renderer initialization failed")]
    Initialization,
}

/// Haarness's cache folder, resolved as the CLI resolves it:
/// `HAARNESS_DATA_DIR` or `HAARNESS_HOME` keep everything under one folder,
/// then `XDG_CACHE_HOME`, then `~/.cache/haarness`.
fn cache_root() -> Option<PathBuf> {
    let absolute = |key| {
        std::env::var_os(key)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(root) = std::env::var_os("HAARNESS_DATA_DIR")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .or_else(|| absolute("HAARNESS_HOME"))
    {
        return Some(root.join("cache"));
    }
    if let Some(root) = absolute("XDG_CACHE_HOME") {
        return Some(root.join("haarness"));
    }
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".cache/haarness"))
}

pub fn extracted_library_path() -> Result<PathBuf, OpenTuiError> {
    if hex::encode(Sha256::digest(ARCHIVE)) != ARCHIVE_SHA {
        return Err(OpenTuiError::Integrity(
            "embedded archive checksum mismatch".into(),
        ));
    }
    let directory = cache_root()
        .ok_or_else(|| OpenTuiError::Integrity("OS cache directory is unavailable".into()))?
        .join("native")
        .join(env!("CARGO_PKG_VERSION"))
        .join(NATIVE_TARGET)
        .join(LIBRARY_SHA);
    fs::create_dir_all(&directory)?;
    reject_symlink(&directory)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(".extract.lock"))?;
    lock.lock_exclusive()?;
    let output = directory.join(LIBRARY_NAME);
    if regular_file_with_hash(&output, LIBRARY_SHA)? {
        return Ok(output);
    }
    if output.exists() {
        fs::remove_file(&output)?;
    }
    let bytes = library_bytes()?;
    if hex::encode(Sha256::digest(&bytes)) != LIBRARY_SHA {
        return Err(OpenTuiError::Integrity(
            "native library checksum mismatch".into(),
        ));
    }
    let temporary = directory.join(format!(".{LIBRARY_NAME}.{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    set_private_permissions(&file)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, &output)?;
    // Windows cannot open a directory as a file; NTFS journals the rename.
    #[cfg(unix)]
    fs::File::open(&directory)?.sync_all()?;
    Ok(output)
}

#[derive(Debug, Clone)]
pub struct NativeReport {
    pub version: String,
    pub target: String,
    pub path: PathBuf,
    pub sha256: String,
    pub symbols: Vec<String>,
}
pub fn doctor() -> Result<NativeReport, OpenTuiError> {
    let path = extracted_library_path()?;
    let library = unsafe { Library::new(&path)? };
    for symbol in REQUIRED_SYMBOLS {
        unsafe {
            let _: libloading::Symbol<'_, *const c_void> = library.get(symbol.as_bytes())?;
        }
    }
    Ok(NativeReport {
        version: OPENTUI_VERSION.into(),
        target: NATIVE_TARGET.into(),
        path,
        sha256: LIBRARY_SHA.into(),
        symbols: REQUIRED_SYMBOLS
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
    })
}

type NativeHandle = u32;
type CreateRenderer = unsafe extern "C" fn(u32, u32, u8, u8, *const c_void) -> NativeHandle;
type SetTerminalEnvVar = unsafe extern "C" fn(NativeHandle, *const u8, u32, *const u8, u32) -> bool;
type SetCursorPosition = unsafe extern "C" fn(NativeHandle, i32, i32, bool);
type DestroyRenderer = unsafe extern "C" fn(NativeHandle, bool);
type GetNextBuffer = unsafe extern "C" fn(NativeHandle) -> NativeHandle;
type BufferClear = unsafe extern "C" fn(NativeHandle, *const u16);
type BufferDrawText =
    unsafe extern "C" fn(NativeHandle, *const u8, u32, u32, u32, *const u16, *const u16, u32);
type BufferWrite = unsafe extern "C" fn(NativeHandle, *mut u8, u32, bool) -> u32;
type Render = unsafe extern "C" fn(NativeHandle, bool) -> u8;
type ResizeRenderer = unsafe extern "C" fn(NativeHandle, u32, u32);
type GetColorPointer = unsafe extern "C" fn(NativeHandle) -> *const [u16; 4];
type GetTerminalCapabilities = unsafe extern "C" fn(NativeHandle, *mut NativeCapabilities);

const FORWARDED_ENV_KEYS: &[&str] = &[
    "SSH_CONNECTION",
    "SSH_CLIENT",
    "SSH_TTY",
    "MOSH_CONNECTION",
    "TMUX",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "ZELLIJ_PANE_ID",
    "TERM",
    "OPENTUI_GRAPHICS",
    "OPENTUI_IMAGE_PROTOCOL",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_FEATURES",
    "ALACRITTY_SOCKET",
    "ALACRITTY_LOG",
    "COLORTERM",
    "TERMUX_VERSION",
    "VHS_RECORD",
    "OPENTUI_FORCE_WCWIDTH",
    "OPENTUI_FORCE_UNICODE",
    "OPENTUI_FORCE_NOZWJ",
    "OPENTUI_FORCE_EXPLICIT_WIDTH",
    "OPENTUI_NOTIFICATION_PROTOCOL",
    "OPENTUI_NOTIFICATIONS",
    "WT_SESSION",
    "STY",
    "WSL_DISTRO_NAME",
    "WSL_INTEROP",
];

#[repr(C)]
#[allow(dead_code)]
struct NativeCapabilities {
    kitty_keyboard: bool,
    kitty_graphics: bool,
    rgb: bool,
    ansi256: bool,
    unicode: u8,
    sgr_pixels: bool,
    color_scheme_updates: bool,
    explicit_width: bool,
    scaled_text: bool,
    sixel: bool,
    focus_tracking: bool,
    sync: bool,
    bracketed_paste: bool,
    hyperlinks: bool,
    osc52: bool,
    notifications: bool,
    explicit_cursor_positioning: bool,
    remote: bool,
    multiplexer: u8,
    image_protocol: u8,
    term_name_ptr: *const u8,
    term_name_len: usize,
    term_version_ptr: *const u8,
    term_version_len: usize,
    term_from_xtversion: bool,
    osc52_support: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorSupport {
    pub truecolor: bool,
    pub ansi256: bool,
}

/// Text attribute bits accepted by `bufferDrawText`.
///
/// The values mirror `OpenTUI` v0.5.9's attribute bitfield. Rendering paths must
/// treat them as best-effort styling: a terminal or a future native build that
/// ignores a bit simply falls back to plain text in the span's colors.
mod handles;
pub use handles::keep_standard_handles_from_children;

pub mod attr {
    pub const NONE: u32 = 0;
    pub const BOLD: u32 = 1;
    pub const DIM: u32 = 2;
    pub const ITALIC: u32 = 4;
    pub const UNDERLINE: u32 = 8;
    pub const INVERSE: u32 = 16;
}

/// `OpenTUI` v0.5.9 uses four packed `u16` values. The low byte is RGBA8 and
/// the high byte carries color intent metadata. Zero metadata means RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color([u16; 4]);

impl Color {
    /// Let the terminal's configured background show through.
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

    #[must_use]
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self([red as u16, green as u16, blue as u16, 255])
    }

    #[must_use]
    pub const fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self([red as u16, green as u16, blue as u16, alpha as u16])
    }

    const fn as_ptr(&self) -> *const u16 {
        self.0.as_ptr()
    }

    /// RGBA components (0-255 each, stored as u16 for the native ABI).
    #[must_use]
    pub const fn components(&self) -> [u16; 4] {
        self.0
    }
}

pub const DEFAULT_BACKGROUND: Color = Color::rgb(15, 14, 13);

pub struct Renderer {
    // Function pointers remain valid only while the dynamic library is loaded.
    #[allow(dead_code)]
    library: Library,
    handle: NativeHandle,
    destroy: DestroyRenderer,
    next_buffer: GetNextBuffer,
    clear: BufferClear,
    draw_text: BufferDrawText,
    write: BufferWrite,
    render: Render,
    resize: ResizeRenderer,
    get_fg: GetColorPointer,
    get_bg: GetColorPointer,
    get_capabilities: GetTerminalCapabilities,
    width: u32,
    height: u32,
    background: Color,
}
impl Renderer {
    pub fn new(width: u32, height: u32, testing: bool) -> Result<Self, OpenTuiError> {
        let env = FORWARDED_ENV_KEYS.iter().filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| ((*key).to_owned(), value))
        });
        Self::new_with_env(width, height, testing, env)
    }
    fn new_with_env(
        width: u32,
        height: u32,
        testing: bool,
        env: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, OpenTuiError> {
        let path = extracted_library_path()?;
        let library = unsafe { Library::new(path)? };
        let create: CreateRenderer = unsafe { *library.get(b"createRenderer")? };
        let set_env: SetTerminalEnvVar = unsafe { *library.get(b"setTerminalEnvVar")? };
        let set_cursor: SetCursorPosition = unsafe { *library.get(b"setCursorPosition")? };
        let destroy = unsafe { *library.get(b"destroyRenderer")? };
        let next_buffer = unsafe { *library.get(b"getNextBuffer")? };
        let clear = unsafe { *library.get(b"bufferClear")? };
        let draw_text = unsafe { *library.get(b"bufferDrawText")? };
        let write = unsafe { *library.get(b"bufferWriteResolvedChars")? };
        let render = unsafe { *library.get(b"render")? };
        let resize = unsafe { *library.get(b"resizeRenderer")? };
        let get_fg = unsafe { *library.get(b"bufferGetFgPtr")? };
        let get_bg = unsafe { *library.get(b"bufferGetBgPtr")? };
        let get_capabilities = unsafe { *library.get(b"getTerminalCapabilities")? };
        let buffered_output = u8::from(testing);
        let handle = unsafe { create(width, height, buffered_output, 0, std::ptr::null()) };
        if handle == 0 {
            return Err(OpenTuiError::Initialization);
        }
        // Haarness draws its own input and nested-PTY cursors as cells. The
        // native renderer otherwise restores a visible white cursor at (1, 1)
        // after each frame, overriding TerminalGuard's initial cursor hide.
        unsafe { set_cursor(handle, 1, 1, false) };
        let renderer = Self {
            library,
            handle,
            destroy,
            next_buffer,
            clear,
            draw_text,
            write,
            render,
            resize,
            get_fg,
            get_bg,
            get_capabilities,
            width,
            height,
            background: DEFAULT_BACKGROUND,
        };
        for (key, value) in env {
            let key_len = u32::try_from(key.len()).map_err(|_| {
                OpenTuiError::Integrity("terminal environment key is too large".into())
            })?;
            let value_len = u32::try_from(value.len()).map_err(|_| {
                OpenTuiError::Integrity("terminal environment value is too large".into())
            })?;
            if !unsafe {
                set_env(
                    renderer.handle,
                    key.as_ptr(),
                    key_len,
                    value.as_ptr(),
                    value_len,
                )
            } {
                return Err(OpenTuiError::Initialization);
            }
        }
        Ok(renderer)
    }
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    /// Background used to clear the frame; the theme's page color.
    pub fn set_background(&mut self, color: Color) {
        self.background = color;
    }
    #[must_use]
    pub fn color_support(&self) -> ColorSupport {
        let mut capabilities = std::mem::MaybeUninit::<NativeCapabilities>::uninit();
        unsafe {
            (self.get_capabilities)(self.handle, capabilities.as_mut_ptr());
            let capabilities = capabilities.assume_init();
            ColorSupport {
                truecolor: capabilities.rgb,
                ansi256: capabilities.ansi256,
            }
        }
    }
    pub fn resize(&mut self, width: u32, height: u32) {
        unsafe {
            (self.resize)(self.handle, width, height);
        }
        self.width = width;
        self.height = height;
    }
    /// Draw span-styled lines; each span renders in its own colors and
    /// attributes at its cell offset.
    pub fn draw_lines(&mut self, lines: &[Line], force: bool) {
        self.fill_lines(lines);
        unsafe {
            (self.render)(self.handle, force);
        }
    }
    pub fn snapshot_lines(&mut self, lines: &[Line]) -> String {
        let buffer = self.fill_lines(lines);
        self.resolved_chars(buffer)
    }
    /// Render `lines` headlessly and report the foreground and background
    /// actually present in the native cell at `(x, y)`.
    pub fn cell_colors(&mut self, lines: &[Line], x: u32, y: u32) -> Option<(Color, Color)> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let buffer = self.fill_lines(lines);
        let index = (y * self.width + x) as usize;
        let foreground = unsafe { *(self.get_fg)(buffer).add(index) };
        let background = unsafe { *(self.get_bg)(buffer).add(index) };
        Some((Color(foreground), Color(background)))
    }
    fn resolved_chars(&mut self, buffer: NativeHandle) -> String {
        let mut output = vec![0_u8; (self.width as usize + 1) * self.height as usize * 4];
        let capacity = u32::try_from(output.len()).unwrap_or(u32::MAX);
        let count = unsafe { (self.write)(buffer, output.as_mut_ptr(), capacity, true) } as usize;
        output.truncate(count);
        String::from_utf8_lossy(&output).into_owned()
    }
    fn fill_lines(&mut self, lines: &[Line]) -> NativeHandle {
        let buffer = unsafe { (self.next_buffer)(self.handle) };
        unsafe {
            (self.clear)(buffer, self.background.as_ptr());
        }
        for (y, line) in lines.iter().take(self.height as usize).enumerate() {
            let mut x: u32 = 0;
            for span in &line.spans {
                if span.text.is_empty() {
                    continue;
                }
                let length = u32::try_from(span.text.len()).unwrap_or(u32::MAX);
                let background = span.bg.unwrap_or(self.background);
                unsafe {
                    (self.draw_text)(
                        buffer,
                        span.text.as_ptr(),
                        length,
                        x,
                        u32::try_from(y).unwrap_or(0),
                        span.fg.as_ptr(),
                        background.as_ptr(),
                        span.attributes,
                    );
                }
                x = x.saturating_add(u32::try_from(span.width()).unwrap_or(u32::MAX));
                if x >= self.width {
                    break;
                }
            }
        }
        buffer
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        if self.handle != 0 {
            unsafe {
                (self.destroy)(self.handle, false);
            }
        }
    }
}
/// One styled run of text within a [`Line`]. A `bg` of `None` inherits the
/// renderer's background (the theme page color).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub fg: Color,
    pub bg: Option<Color>,
    pub attributes: u32,
    /// Whether a mouse selection copies this text. Decoration, such as a
    /// gutter bar, is drawn but never copied (`OpenTUI`'s `selectable`). The
    /// renderer ignores it.
    pub selectable: bool,
}

impl Span {
    #[must_use]
    pub fn new(text: impl Into<String>, fg: Color) -> Self {
        Self {
            text: text.into(),
            fg,
            bg: None,
            attributes: attr::NONE,
            selectable: true,
        }
    }
    /// The same text as decoration: drawn, never copied by a selection.
    #[must_use]
    pub fn unselectable(mut self) -> Self {
        self.selectable = false;
        self
    }
    #[must_use]
    pub fn on(mut self, bg: Color) -> Self {
        self.bg = Some(bg);
        self
    }
    #[must_use]
    pub fn with_attributes(mut self, attributes: u32) -> Self {
        self.attributes = attributes;
        self
    }
    /// Display width in terminal cells, as the renderer lays it out.
    #[must_use]
    pub fn width(&self) -> usize {
        display_width(&self.text)
    }
}

/// Cells the renderer gives `text`: each grapheme cluster takes the width
/// `unicode-width` gives that cluster alone.
///
/// `unicode-width` measures a whole string with a few ligatures that join
/// separate clusters, most often an Arabic lam followed by an alef ("لا",
/// "للإغلاق"), which it counts as one cell. The renderer draws the lam and
/// the alef in a cell each, so a width taken from the whole string put the
/// next span on top of the word's last letters. Every other rule (emoji
/// sequences, combining marks, CJK) applies inside one cluster and gives the
/// same result either way.
#[must_use]
pub fn display_width(text: &str) -> usize {
    use unicode_segmentation::UnicodeSegmentation as _;
    use unicode_width::UnicodeWidthStr as _;
    if !text.chars().any(joins_across_clusters) {
        return text.width();
    }
    text.graphemes(true).map(str::width).sum()
}

/// Scripts in which `unicode-width` joins characters from separate grapheme
/// clusters into one cell: Hebrew, Arabic, Khmer, Buginese, Tifinagh, Lisu,
/// Old Turkic and Kirat Rai.
fn joins_across_clusters(character: char) -> bool {
    matches!(
        u32::from(character),
        0x0590..=0x08FF
            | 0x1780..=0x17FF
            | 0x1A00..=0x1A1F
            | 0x2D30..=0x2D7F
            | 0xA4D0..=0xA4FF
            | 0xFB1D..=0xFDFF
            | 0xFE70..=0xFEFF
            | 0x10C00..=0x10C4F
            | 0x16D40..=0x16D7F
    )
}

/// One terminal row composed of styled spans drawn left to right.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    #[must_use]
    pub fn new(spans: Vec<Span>) -> Self {
        Self { spans }
    }
    #[must_use]
    pub fn plain(text: impl Into<String>, fg: Color) -> Self {
        Self {
            spans: vec![Span::new(text, fg)],
        }
    }
    #[must_use]
    pub fn empty() -> Self {
        Self { spans: Vec::new() }
    }
    /// Concatenated text of every span, without styling.
    #[must_use]
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
    /// Display width in terminal cells.
    #[must_use]
    pub fn width(&self) -> usize {
        self.spans.iter().map(Span::width).sum()
    }
}

fn library_bytes() -> Result<Vec<u8>, OpenTuiError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(ARCHIVE))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.name().rsplit('/').next() == Some(LIBRARY_NAME) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            return Ok(bytes);
        }
    }
    Err(OpenTuiError::Integrity(format!(
        "archive contains no {LIBRARY_NAME}"
    )))
}
fn reject_symlink(path: &Path) -> Result<(), OpenTuiError> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(OpenTuiError::Integrity(format!(
            "cache path is a symlink: {}",
            path.display()
        )));
    }
    Ok(())
}
fn regular_file_with_hash(path: &Path, expected: &str) -> Result<bool, OpenTuiError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() {
        return Err(OpenTuiError::Integrity(format!(
            "native cache entry is not a regular file: {}",
            path.display()
        )));
    }
    Ok(hex::encode(Sha256::digest(fs::read(path)?)) == expected)
}
#[cfg(unix)]
fn set_private_permissions(file: &fs::File) -> Result<(), OpenTuiError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o700))?;
    Ok(())
}
#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // same signature as on Unix, where it can fail
fn set_private_permissions(_file: &fs::File) -> Result<(), OpenTuiError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_archive_and_abi_are_valid() {
        let report = doctor().unwrap();
        assert_eq!(report.version, OPENTUI_VERSION);
        assert_eq!(report.symbols, REQUIRED_SYMBOLS);
    }
    #[test]
    fn apple_terminal_environment_selects_ansi256_over_false_truecolor_claim() {
        let renderer = Renderer::new_with_env(
            8,
            2,
            true,
            [
                ("TERM".into(), "xterm-256color".into()),
                ("TERM_PROGRAM".into(), "Apple_Terminal".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ],
        )
        .unwrap();

        assert_eq!(
            renderer.color_support(),
            ColorSupport {
                truecolor: false,
                ansi256: true,
            }
        );
    }
    #[test]
    fn headless_renderer_draws_text() {
        let mut renderer = Renderer::new(32, 4, true).unwrap();
        let foreground = Color::rgb(231, 225, 213);
        let output = renderer.snapshot_lines(&[Line::plain("rust-opentui-smoke", foreground)]);
        assert!(output.contains("rust-opentui-smoke"), "{output:?}");

        let colors = renderer
            .cell_colors(&[Line::plain("x", foreground)], 0, 0)
            .unwrap();
        assert_eq!(colors.0, foreground);
        assert_eq!(colors.1, DEFAULT_BACKGROUND);
    }

    #[test]
    fn resize_preserves_renderer_and_updates_dimensions() {
        let mut renderer = Renderer::new(20, 4, true).unwrap();
        renderer.resize(40, 8);
        assert_eq!(renderer.size(), (40, 8));
        let output = renderer.snapshot_lines(&[Line::plain("resized", Color::rgb(51, 184, 166))]);
        assert!(output.contains("resized"), "{output:?}");
    }

    #[test]
    fn spans_render_each_run_in_its_own_colors_at_cell_offsets() {
        let mut renderer = Renderer::new(32, 4, true).unwrap();
        let red = Color::rgb(200, 40, 40);
        let green = Color::rgb(40, 200, 40);
        let blue = Color::rgb(40, 40, 200);
        let surface = Color::rgb(27, 25, 23);
        let lines = [Line::new(vec![
            Span::new("ab", red),
            Span::new("cde", green).on(surface),
            Span::new("f", blue),
        ])];

        let output = renderer.snapshot_lines(&lines);
        assert!(output.contains("abcdef"), "{output:?}");
        assert_eq!(renderer.cell_colors(&lines, 0, 0).unwrap().0, red);
        assert_eq!(renderer.cell_colors(&lines, 1, 0).unwrap().0, red);
        let (fg, bg) = renderer.cell_colors(&lines, 2, 0).unwrap();
        assert_eq!(fg, green);
        assert_eq!(bg, surface);
        assert_eq!(renderer.cell_colors(&lines, 4, 0).unwrap().0, green);
        assert_eq!(renderer.cell_colors(&lines, 5, 0).unwrap().0, blue);
        assert_eq!(
            renderer.cell_colors(&lines, 5, 0).unwrap().1,
            DEFAULT_BACKGROUND
        );
    }

    #[test]
    fn set_background_changes_the_clear_color() {
        let mut renderer = Renderer::new(16, 2, true).unwrap();
        let page = Color::rgb(16, 32, 48);
        renderer.set_background(page);
        let lines = [Line::plain("t", Color::rgb(255, 255, 255))];
        // An empty cell far from the text carries the new page color.
        assert_eq!(renderer.cell_colors(&lines, 10, 1).unwrap().1, page);

        // A fresh renderer still defaults to the editorial page color.
        let mut fresh = Renderer::new(16, 2, true).unwrap();
        assert_eq!(
            fresh.cell_colors(&lines, 10, 1).unwrap().1,
            DEFAULT_BACKGROUND
        );
    }

    #[test]
    fn transparent_background_reaches_the_native_frame_buffer() {
        let mut renderer = Renderer::new(16, 2, true).unwrap();
        renderer.set_background(Color::TRANSPARENT);
        let lines = [Line::plain("t", Color::rgb(255, 255, 255))];

        assert_eq!(
            renderer.cell_colors(&lines, 10, 1).unwrap().1,
            Color::TRANSPARENT
        );
        assert_eq!(
            renderer.cell_colors(&lines, 0, 0).unwrap().1,
            Color::TRANSPARENT
        );
    }

    #[test]
    fn wide_glyphs_advance_spans_by_display_width() {
        let mut renderer = Renderer::new(16, 2, true).unwrap();
        let cyan = Color::rgb(62, 184, 166);
        let white = Color::rgb(255, 255, 255);
        // "日本" occupies four cells, so "x" must land at cell 4.
        let lines = [Line::new(vec![
            Span::new("日本", cyan),
            Span::new("x", white),
        ])];
        assert_eq!(renderer.cell_colors(&lines, 4, 0).unwrap().0, white);
    }

    /// The renderer gives an Arabic lam and the alef after it a cell each.
    /// `unicode-width` counts the pair as one (the lam-alef ligature), so a
    /// span placed after "للإغلاق" by that width covered its last letters.
    #[test]
    fn arabic_lam_alef_takes_the_cells_the_renderer_draws() {
        let mut renderer = Renderer::new(24, 2, true).unwrap();
        let white = Color::rgb(255, 255, 255);
        let word = "esc \u{644}\u{644}\u{625}\u{63a}\u{644}\u{627}\u{642}";
        let lines = [Line::new(vec![
            Span::new(word, white),
            Span::new("|", white),
        ])];
        let drawn = renderer.snapshot_lines(&lines);
        assert!(drawn.starts_with(&format!("{word}|")), "{drawn:?}");
        assert_eq!(Span::new(word, white).width(), 11);
    }

    /// The width every caller uses is the width the renderer draws, for
    /// Latin, CJK, emoji sequences, combining marks and right-to-left text.
    #[test]
    fn display_width_matches_the_cells_the_renderer_draws() {
        let mut renderer = Renderer::new(40, 2, true).unwrap();
        let cyan = Color::rgb(62, 184, 166);
        let white = Color::rgb(255, 255, 255);
        for (text, cells) in [
            ("esc close", 9),
            ("日本語", 6),
            ("e\u{301}t\u{e9}", 3),
            ("👩\u{200d}💻 ok", 5),
            ("👍🏽", 2),
            ("🇯🇵", 2),
            ("\u{644}\u{627}", 2),
            (
                "\u{627}\u{644}\u{625}\u{639}\u{62f}\u{627}\u{62f}\u{627}\u{62a}",
                9,
            ),
            ("\u{644}\u{64e}\u{627}", 2),
            ("\u{5e9}\u{5dc}\u{5d5}\u{5dd}", 4),
        ] {
            assert_eq!(display_width(text), cells, "{text}");
            let lines = [Line::new(vec![
                Span::new(text, cyan),
                Span::new("|", white),
            ])];
            let at = u32::try_from(cells).unwrap();
            assert_eq!(
                renderer.cell_colors(&lines, at, 0).unwrap().0,
                white,
                "{text}"
            );
            assert_eq!(
                renderer.cell_colors(&lines, at - 1, 0).unwrap().0,
                cyan,
                "{text}"
            );
        }
    }

    #[test]
    fn renderer_can_be_recreated_after_destruction() {
        // Proof for $EDITOR suspend/resume: drop and rebuild in one process.
        let first = Renderer::new(20, 4, true).unwrap();
        drop(first);
        let mut second = Renderer::new(24, 6, true).unwrap();
        let output = second.snapshot_lines(&[Line::plain("reborn", Color::rgb(51, 184, 166))]);
        assert!(output.contains("reborn"), "{output:?}");
    }
}
