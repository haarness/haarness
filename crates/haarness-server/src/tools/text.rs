//! Text files as `read` and `grep` take them: a line at a time, holding no
//! more of a line than they use, so a line that never ends (a minified
//! bundle, a JSON dump, a log) costs no more memory than a short one.

use std::{
    borrow::Cow,
    fs::File,
    io::{self, BufRead, BufReader, Read},
    path::Path,
};

use haarness_core::{CoreError, CoreResult};
use tokio_util::sync::CancellationToken;

/// The most characters of one line `read` and `grep` show.
pub(super) const LINE_CHARS: usize = 2_000;
/// What a line cut at [`LINE_CHARS`] ends with.
pub(super) const LINE_CUT: &str = "… (line truncated to 2000 characters)";
/// Bytes of a line `read` keeps: enough for [`LINE_CHARS`] characters,
/// since none takes more than four.
const READ_LINE_BYTES: usize = LINE_CHARS * 4;
/// Where a NUL byte marks a file as binary data rather than text.
const BINARY_PROBE_BYTES: usize = 8 * 1024;
/// The lines a `read` that gives no limit returns.
const READ_DEFAULT_LINES: usize = 2_000;
/// What one `read` returns at most: as much as any tool's output shows.
const READ_MAX_LINES: usize = super::TOOL_OUTPUT_MAX_LINES;
const READ_MAX_BYTES: usize = super::TOOL_OUTPUT_MAX_BYTES;

/// One line, without its `\n` or `\r\n`: its first `keep` bytes, and whether
/// it went on past them.
pub(super) struct Line<'a> {
    pub(super) bytes: &'a [u8],
    pub(super) long: bool,
}

/// Splits text into lines exactly where `str::lines` does (at `\n`, a `\r`
/// right before it dropped), keeping at most `keep` bytes of each, in one
/// buffer every line reuses.
pub(super) struct Lines<R> {
    reader: R,
    keep: usize,
    buffer: Vec<u8>,
}

impl<R: BufRead> Lines<R> {
    pub(super) fn new(reader: R, keep: usize) -> Self {
        Self {
            reader,
            keep,
            buffer: Vec::new(),
        }
    }

    /// Whether anything follows the lines read so far.
    pub(super) fn more(&mut self) -> CoreResult<bool> {
        loop {
            match self.reader.fill_buf() {
                Ok(buffer) => return Ok(!buffer.is_empty()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(CoreError::Tool(error.to_string())),
            }
        }
    }

    /// The next line, `None` at the end. Stop ends it even inside a line
    /// that never ends.
    pub(super) fn next_line(&mut self, cancel: &CancellationToken) -> CoreResult<Option<Line<'_>>> {
        self.buffer.clear();
        // The line's length and last byte before its `\n`, kept or not.
        let mut length = 0_usize;
        let mut last = None;
        let mut ended = false;
        loop {
            if cancel.is_cancelled() {
                return Err(CoreError::Cancelled);
            }
            let buffer = match self.reader.fill_buf() {
                Ok(buffer) => buffer,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(CoreError::Tool(error.to_string())),
            };
            if buffer.is_empty() {
                break;
            }
            let end = buffer.iter().position(|&byte| byte == b'\n');
            let part = &buffer[..end.unwrap_or(buffer.len())];
            if let Some(&byte) = part.last() {
                last = Some(byte);
            }
            let room = self.keep.saturating_sub(self.buffer.len());
            self.buffer.extend_from_slice(&part[..part.len().min(room)]);
            length += part.len();
            let used = part.len() + usize::from(end.is_some());
            self.reader.consume(used);
            if end.is_some() {
                ended = true;
                break;
            }
        }
        if !ended && length == 0 {
            return Ok(None);
        }
        if ended && last == Some(b'\r') {
            length -= 1;
            self.buffer.truncate(length);
        }
        Ok(Some(Line {
            long: length > self.buffer.len(),
            bytes: &self.buffer,
        }))
    }
}

impl<'a> Line<'a> {
    /// The line as text: invalid UTF-8 as U+FFFD when `lossy`, else `None`.
    /// A line cut short leaves out a character its cut split, which is no
    /// sign of text that is not UTF-8.
    pub(super) fn text(&self, lossy: bool) -> Option<Cow<'a, str>> {
        let bytes = if self.long {
            without_split_character(self.bytes)
        } else {
            self.bytes
        };
        match std::str::from_utf8(bytes) {
            Ok(text) => Some(Cow::Borrowed(text)),
            Err(_) if lossy => Some(String::from_utf8_lossy(bytes)),
            Err(_) => None,
        }
    }
}

/// `bytes` without a UTF-8 sequence its end cuts short: the bytes after the
/// last character's first byte, when fewer than that byte announces.
fn without_split_character(bytes: &[u8]) -> &[u8] {
    let from = bytes.len().saturating_sub(4);
    let Some(start) = (from..bytes.len())
        .rev()
        .find(|&index| bytes[index] & 0xC0 != 0x80)
    else {
        return bytes;
    };
    let length = match bytes[start] {
        0xF0.. => 4,
        0xE0.. => 3,
        0xC0.. => 2,
        _ => 1,
    };
    if bytes.len() - start < length {
        &bytes[..start]
    } else {
        bytes
    }
}

/// `text` as shown: at most [`LINE_CHARS`] characters, a longer one (or one
/// whose bytes went on, `long`) ending in [`LINE_CUT`].
pub(super) fn shown(text: &str, long: bool) -> Cow<'_, str> {
    match text.char_indices().nth(LINE_CHARS) {
        Some((end, _)) => Cow::Owned(format!("{}{LINE_CUT}", &text[..end])),
        None if long => Cow::Owned(format!("{text}{LINE_CUT}")),
        None => Cow::Borrowed(text),
    }
}

/// How a text file spells its characters on disk, so a tool that changes it
/// writes it back the same way: UTF-8, UTF-8 behind a byte order mark (as
/// Visual Studio and Notepad save files), or UTF-16 behind one (what Windows
/// PowerShell 5.1's `>` and `Out-File` write).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Spelling {
    #[default]
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";

/// The most bytes of a UTF-16 file `read` and `grep` decode: they cannot be
/// read a line at a time as UTF-8 is.
const UTF16_MAX_BYTES: u64 = 64 * 1024 * 1024;

impl Spelling {
    /// `text` (without a byte order mark: one it starts with is dropped) in
    /// this spelling.
    pub(super) fn encode(self, text: &str) -> Vec<u8> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        match self {
            Self::Utf8 => text.as_bytes().to_vec(),
            Self::Utf8Bom => [UTF8_BOM, text.as_bytes()].concat(),
            Self::Utf16Le => [0xff, 0xfe]
                .into_iter()
                .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
                .collect(),
            Self::Utf16Be => [0xfe, 0xff]
                .into_iter()
                .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
                .collect(),
        }
    }

    /// The spelling a new text written over a file in this spelling takes:
    /// its own, and a byte order mark when the new text starts with one.
    pub(super) fn for_new_text(self, text: &str) -> Self {
        match self {
            Self::Utf8 if text.starts_with('\u{feff}') => Self::Utf8Bom,
            spelling => spelling,
        }
    }
}

/// The text `bytes` spell, without a byte order mark, and how they spell it;
/// `None` when they are neither UTF-8 nor UTF-16 behind a byte order mark.
pub(super) fn decode(bytes: &[u8]) -> Option<(Cow<'_, str>, Spelling)> {
    if let Some(rest) = bytes.strip_prefix(UTF8_BOM) {
        return std::str::from_utf8(rest)
            .ok()
            .map(|text| (Cow::Borrowed(text), Spelling::Utf8Bom));
    }
    let utf16 = |rest: &[u8], unit: fn([u8; 2]) -> u16, spelling| {
        if !rest.len().is_multiple_of(2) {
            return None;
        }
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| unit([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units)
            .ok()
            .map(|text| (Cow::Owned(text), spelling))
    };
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return utf16(rest, u16::from_le_bytes, Spelling::Utf16Le);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return utf16(rest, u16::from_be_bytes, Spelling::Utf16Be);
    }
    std::str::from_utf8(bytes)
        .ok()
        .map(|text| (Cow::Borrowed(text), Spelling::Utf8))
}

/// A file opened for its text, and whether a NUL near its start marks it as
/// binary data (then only UTF-8 text is shown, as before lines were read one
/// at a time; other text is shown with U+FFFD for what is not UTF-8). A
/// UTF-8 byte order mark is passed over, and a UTF-16 file behind one is
/// read as the UTF-8 its text is.
pub(super) fn open_text(path: &Path) -> CoreResult<(Box<dyn BufRead>, bool)> {
    let tool = |error: io::Error| CoreError::Tool(error.to_string());
    // Before opening: opening a pipe waits for a writer.
    let metadata = std::fs::metadata(path).map_err(tool)?;
    if metadata.is_dir() {
        return Err(CoreError::Tool(format!(
            "{} is a folder; list shows what it holds",
            path.display()
        )));
    }
    // A pipe or a device (`/dev/zero`) may never end.
    if !metadata.is_file() {
        return Err(not_regular(path.display()));
    }
    let mut file = File::open(path).map_err(tool)?;
    let mut head = Vec::with_capacity(BINARY_PROBE_BYTES);
    (&mut file)
        .take(BINARY_PROBE_BYTES as u64)
        .read_to_end(&mut head)
        .map_err(tool)?;
    if head.starts_with(&[0xff, 0xfe]) || head.starts_with(&[0xfe, 0xff]) {
        if metadata.len() <= UTF16_MAX_BYTES {
            let mut bytes = head.clone();
            file.read_to_end(&mut bytes).map_err(tool)?;
            // UTF-16 text holds no NUL; data that only starts like it does.
            if let Some((text, _)) = decode(&bytes).filter(|(text, _)| !text.contains('\0')) {
                let text = text.into_owned().into_bytes();
                return Ok((Box::new(io::Cursor::new(text)), false));
            }
        }
        return Ok((
            Box::new(BufReader::new(io::Cursor::new(head).chain(file))),
            true,
        ));
    }
    if head.starts_with(UTF8_BOM) {
        head.drain(..UTF8_BOM.len());
    }
    let binary = head.contains(&0);
    Ok((
        Box::new(BufReader::new(io::Cursor::new(head).chain(file))),
        binary,
    ))
}

/// What `read` shows of a text file: `limit` lines (2000 when none is
/// given) from line `offset` (zero-based), each cut at [`LINE_CHARS`]
/// characters, joined by `\n`. It stops early at 2,000 lines or 50 KB, and
/// then, or when its limit ends before the file does, says where to go on.
pub(super) fn read_text(
    path: &Path,
    offset: usize,
    limit: Option<usize>,
    cancel: &CancellationToken,
) -> CoreResult<String> {
    let limit = limit.unwrap_or(READ_DEFAULT_LINES);
    let (reader, binary) = open_text(path)?;
    let mut lines = Lines::new(reader, READ_LINE_BYTES);
    let mut shown_lines = Vec::new();
    let mut bytes = 0_usize;
    let mut index = 0_usize;
    let mut stop = None;
    // Whether a line shown had bytes that are not UTF-8.
    let mut replaced = false;
    while shown_lines.len() < limit {
        let Some(line) = lines.next_line(cancel)? else {
            break;
        };
        index += 1;
        if index <= offset {
            continue;
        }
        let text = line.text(!binary).ok_or_else(|| {
            CoreError::Tool(format!("{} holds binary data, not text", path.display()))
        })?;
        replaced |= matches!(text, Cow::Owned(_));
        let text = shown(&text, line.long).into_owned();
        let size = text.len() + usize::from(!shown_lines.is_empty());
        if shown_lines.len() == READ_MAX_LINES {
            stop = Some(format!("{READ_MAX_LINES} lines"));
        } else if bytes + size > READ_MAX_BYTES {
            stop = Some(format!("{} KB", READ_MAX_BYTES / 1024));
        }
        if stop.is_some() {
            break;
        }
        bytes += size;
        shown_lines.push(text);
    }
    // Any byte after the last line shown is another line.
    let more = stop.is_none() && limit > 0 && shown_lines.len() == limit && lines.more()?;
    let mut notes = Vec::new();
    // Said so the model does not write the file back from what it was shown.
    if replaced {
        notes.push(
            "(this file is not UTF-8: bytes that do not decode show as \u{fffd}; edit cannot change it, and write would replace those bytes)"
                .to_owned(),
        );
    }
    let next = offset + shown_lines.len();
    // Every line was passed over: an empty result would read as an empty
    // file.
    if shown_lines.is_empty() && offset > 0 && stop.is_none() {
        let length = match index {
            0 => "is empty".to_owned(),
            1 => "has 1 line".to_owned(),
            count => format!("has {count} lines"),
        };
        notes.push(format!(
            "(the file {length}, so offset {offset} is past its end)"
        ));
    }
    if let Some(stop) = stop {
        notes.push(format!("(stopped at {stop}; read on with offset={next})"));
    } else if more {
        notes.push(format!("(more lines follow; read on with offset={next})"));
    }
    let text = shown_lines.join("\n");
    // An empty line shown is a line: its note still comes after it.
    Ok(match (shown_lines.is_empty(), notes.is_empty()) {
        (_, true) => text,
        (true, false) => notes.join("\n"),
        (false, false) => format!("{text}\n\n{}", notes.join("\n")),
    })
}

/// The refusal of a pipe, socket or device, which `shown` names.
pub(super) fn not_regular(shown: impl std::fmt::Display) -> CoreError {
    CoreError::Tool(format!("{shown} is not a regular file"))
}

/// What a path held once it was opened that is no file to read or write: a
/// pipe or a device put in a file's place after [`refuse_special`] looked.
/// [`super::io_error`] words it as [`not_regular`] does.
#[derive(Debug)]
pub(super) struct NotRegular;

impl std::fmt::Display for NotRegular {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("not a regular file")
    }
}

impl std::error::Error for NotRegular {}

/// Opens `path` without waiting for a pipe's other end, then refuses what
/// the open file is if that is neither a regular file nor a folder (a folder
/// fails as it always did). The file is then read or written as any other:
/// only its open does not wait.
fn open_regular(path: &Path, options: &std::fs::OpenOptions) -> io::Result<(File, u64)> {
    let file = open_without_waiting(path, options)?;
    let metadata = file.metadata().map_err(unreadable_handle)?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(io::Error::other(NotRegular));
    }
    #[cfg(unix)]
    {
        use nix::fcntl::{FcntlArg, OFlag, fcntl};
        let flags = OFlag::from_bits_truncate(fcntl(&file, FcntlArg::F_GETFL)?);
        fcntl(&file, FcntlArg::F_SETFL(flags - OFlag::O_NONBLOCK))?;
    }
    Ok((file, metadata.len()))
}

/// The open of [`open_regular`]: on Unix with `O_NONBLOCK`, so a pipe opens
/// at once. A pipe or socket opened to be written with nothing reading it
/// fails then (ENXIO) and is refused as no regular file; ENXIO from anything
/// else (a disk that went away) keeps its own words. An open a lease would
/// hold (a file another NFS or SMB client has open) is made again the usual
/// way, waiting as it always did.
#[cfg(unix)]
fn open_without_waiting(path: &Path, options: &std::fs::OpenOptions) -> io::Result<File> {
    use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
    let mut without_waiting = options.clone();
    without_waiting.custom_flags(nix::libc::O_NONBLOCK);
    match without_waiting.open(path) {
        Err(error)
            if error.raw_os_error() == Some(nix::libc::ENXIO)
                && std::fs::metadata(path).is_ok_and(|metadata| {
                    metadata.file_type().is_fifo() || metadata.file_type().is_socket()
                }) =>
        {
            Err(io::Error::other(NotRegular))
        }
        Err(error)
            if error.kind() == io::ErrorKind::WouldBlock
                && std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) =>
        {
            options.open(path)
        }
        opened => opened,
    }
}

#[cfg(not(unix))]
fn open_without_waiting(path: &Path, options: &std::fs::OpenOptions) -> io::Result<File> {
    options.open(path)
}

/// The failure of an open handle to say what it is: on Windows a device
/// name (`NUL`, `CON`) or a named pipe, which is no regular file; elsewhere
/// the error as it came.
fn unreadable_handle(error: io::Error) -> io::Error {
    if cfg!(windows) {
        io::Error::other(NotRegular)
    } else {
        error
    }
}

/// A file's bytes, as `tokio::fs::read` gives them, but checked on the open
/// file (see [`open_regular`]).
pub(super) async fn read_regular(path: &Path) -> io::Result<Vec<u8>> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let (mut file, length) = open_regular(&path, std::fs::OpenOptions::new().read(true))?;
        let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
    .await
    .map_err(io::Error::other)?
}

/// Writes `contents` as `tokio::fs::write` does, made or replaced whole, but
/// checked on the open file (see [`open_regular`]).
pub(super) async fn write_regular(path: &Path, contents: Vec<u8>) -> io::Result<()> {
    use std::io::Write as _;
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let (mut file, _) = open_regular(
            &path,
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true),
        )?;
        file.write_all(&contents)
    })
    .await
    .map_err(io::Error::other)?
}

/// Refuses a pipe, socket or device at `path` (or a link to one) before
/// anything opens it, in the words `read` uses: opening a pipe waits for
/// the other end, so a call would run until Stop could not end it, and a
/// device (`/dev/zero`) may never end. Nothing there, a folder or a regular
/// file passes, and is then met as before. `named` is the path as the call
/// gave it.
pub(super) async fn refuse_special(path: &Path, named: &str) -> CoreResult<()> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) if !metadata.is_file() && !metadata.is_dir() => Err(not_regular(named)),
        _ => Ok(()),
    }
}

/// A file's text for `edit`, `multiedit` and `apply_patch`, without a byte
/// order mark, and how the file spells it, so the change is written back
/// the same way. They change UTF-8 and UTF-16 text (see [`decode`]); a file
/// in another encoding says so plainly, with what to do instead. `named` is
/// the path as the call gave it.
pub(super) async fn read_text_file(
    path: &Path,
    named: &str,
    tool: &str,
) -> CoreResult<(String, Spelling)> {
    refuse_special(path, named).await?;
    let bytes = read_regular(path)
        .await
        .map_err(|error| super::io_error(named, path, &error))?;
    match decode(&bytes) {
        Some((text, spelling)) => Ok((text.into_owned(), spelling)),
        None => Err(CoreError::Tool(format!(
            "{named} is not UTF-8 or UTF-16 text (it may use a legacy code page such as Shift_JIS or \
             Windows-1252), so {tool} cannot change it without replacing the characters that do not decode; \
             nothing was changed. Change it with a command that keeps its encoding, or ask the person to \
             save it as UTF-8 first"
        ))),
    }
}

/// Bytes of a line `grep` searches: a match further into a longer line is
/// not found.
const GREP_LINE_BYTES: usize = 1024 * 1024;
/// Files up to this size `grep` reads whole, as before lines were read one
/// at a time; larger ones it reads a line at a time.
const GREP_WHOLE_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// One file's matching lines as `grep` lists them.
struct Matched<'a> {
    regex: &'a regex::Regex,
    path: &'a str,
    /// Lines the files before this one already listed.
    total: usize,
}

impl Matched<'_> {
    /// Lists line `number` when it matches in its first
    /// [`GREP_LINE_BYTES`], cut for showing (`long`: its bytes went on).
    /// True when it matches but the limit is reached, so the search stops
    /// only when there is more than the limit to show.
    fn take(&self, number: usize, text: &str, long: bool, listed: &mut Vec<String>) -> bool {
        let mut end = text.len().min(GREP_LINE_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if !self.regex.is_match(&text[..end]) {
            return false;
        }
        if self.total + listed.len() == GREP_MAX_MATCHES {
            return true;
        }
        listed.push(format!("{}:{number}:{}", self.path, shown(text, long)));
        false
    }
}
/// The most files one `grep` searches, and the most lines it returns.
pub(super) const GREP_MAX_FILES: usize = 20_000;
pub(super) const GREP_MAX_MATCHES: usize = 10_000;

/// What a `grep` walk found: `path:line:text` lines, the files it left out,
/// and why it stopped early, if it did.
pub(super) struct GrepFound {
    pub(super) matches: Vec<String>,
    pub(super) skipped: super::Skipped,
    pub(super) stopped: Option<GrepStop>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GrepStop {
    Files,
    Matches,
}

impl GrepStop {
    /// The note that ends the output, so a cut list is not read as all there is.
    pub(super) fn note(self) -> String {
        match self {
            Self::Files => format!(
                "(stopped after searching {GREP_MAX_FILES} files; narrow the pattern or the path)"
            ),
            Self::Matches => format!(
                "(stopped at {GREP_MAX_MATCHES} matching lines; narrow the pattern or the path)"
            ),
        }
    }
}

/// Searches `root` for `regex` as ripgrep would: .gitignore, .ignore,
/// repository excludes and hidden files respected, links not followed, so
/// every file is where it is named. A file that is not UTF-8 throughout is
/// not searched (as before files were read a line at a time); a secret-named
/// or denied file is counted, not read. Stop ends it between any two reads.
/// Inside `chat_home` no ignore file above the chat's own folder
/// (`workspace`) is read; see [`grep_files`].
pub(super) fn grep_walk(
    regex: &regex::Regex,
    root: &Path,
    workspace: &Path,
    chat_home: Option<&Path>,
    secrets: &super::SecretFilter,
    cancel: &CancellationToken,
) -> CoreResult<GrepFound> {
    let mut found = GrepFound {
        matches: Vec::new(),
        skipped: super::Skipped::default(),
        stopped: None,
    };
    let walk = grep_files(root, workspace, chat_home)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()));
    for (files, entry) in walk.enumerate() {
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        if files == GREP_MAX_FILES {
            found.stopped = Some(GrepStop::Files);
            break;
        }
        if let Some(hidden) = secrets.names_hidden(entry.path()) {
            found.skipped.count(hidden);
            continue;
        }
        // Relative to the project with `/`, as glob and the file listing
        // write it (Windows writes `\\`); a path outside it as the system does.
        let shown_path = match entry.path().strip_prefix(workspace) {
            Ok(relative) if cfg!(windows) => relative.to_string_lossy().replace('\\', "/"),
            Ok(relative) => relative.display().to_string(),
            Err(_) => entry.path().display().to_string(),
        };
        let mut in_file = Vec::new();
        let total = found.matches.len();
        // A file of ordinary size is read whole and checked as UTF-8 once;
        // a larger one a line at a time, so memory stays bounded.
        let whole = entry
            .metadata()
            .is_ok_and(|metadata| metadata.len() <= GREP_WHOLE_FILE_BYTES);
        let (text_file, passed) = if whole {
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            // Without a byte order mark, so `^` finds the first line; UTF-16
            // text is searched as the text it is, as ripgrep searches it.
            let Some((text, _)) = decode(&bytes) else {
                continue;
            };
            let mut passed = false;
            for (index, line) in text.lines().enumerate() {
                if index % 4096 == 4095 && cancel.is_cancelled() {
                    return Err(CoreError::Cancelled);
                }
                let matched = Matched {
                    regex,
                    path: &shown_path,
                    total,
                };
                if matched.take(index + 1, line, false, &mut in_file) {
                    passed = true;
                    break;
                }
            }
            (true, passed)
        } else {
            let Ok((reader, _)) = open_text(entry.path()) else {
                continue;
            };
            let mut lines = Lines::new(reader, GREP_LINE_BYTES);
            let mut number = 0_usize;
            loop {
                let line = match lines.next_line(cancel) {
                    Ok(Some(line)) => line,
                    Ok(None) => break (true, false),
                    Err(CoreError::Cancelled) => return Err(CoreError::Cancelled),
                    Err(_) => break (false, false),
                };
                number += 1;
                let Some(text) = line.text(false) else {
                    break (false, false);
                };
                let matched = Matched {
                    regex,
                    path: &shown_path,
                    total,
                };
                if matched.take(number, &text, line.long, &mut in_file) {
                    break (true, true);
                }
            }
        };
        if text_file {
            found.matches.append(&mut in_file);
        }
        if passed {
            found.stopped = Some(GrepStop::Matches);
            break;
        }
    }
    Ok(found)
}

/// What `grep` walks under `root`, in walk order. Outside the chat home,
/// `root` and the ignore files of every folder above it, as ripgrep reads
/// them. In the chat home nothing above the chat's own folder (`workspace`)
/// counts: a subfolder of it is walked from the chat's folder, keeping only
/// the way down to the subfolder and what is in it, so every ignore file on
/// the way is weighed as ripgrep weighs it there (a `!` line below, a
/// repository nested in the subfolder, a worktree's excludes). Only when
/// that way is itself left out (a subfolder named although ignored, which
/// ripgrep searches all the same) is the subfolder walked on its own, with
/// [`ChatFolderIgnores`] for the folders above it.
fn grep_files(
    root: &Path,
    workspace: &Path,
    chat_home: Option<&Path>,
) -> Box<dyn Iterator<Item = ignore::DirEntry>> {
    let walk = |start: &Path, parents: bool| {
        let mut builder = ignore::WalkBuilder::new(start);
        builder.standard_filters(true).parents(parents);
        builder
    };
    if !super::within_chat_home(root, chat_home) {
        return Box::new(walk(root, true).build().filter_map(Result::ok));
    }
    let Some(chat) = root
        .is_dir()
        .then(|| chat_folder_above(root, workspace))
        .flatten()
    else {
        return Box::new(walk(root, false).build().filter_map(Result::ok));
    };
    let reached = std::rc::Rc::new(std::cell::Cell::new(false));
    let way = root.to_path_buf();
    let down = walk(&chat, false)
        .filter_entry(move |entry| way.starts_with(entry.path()) || entry.path().starts_with(&way))
        .build()
        .filter_map(Result::ok);
    let seen = std::rc::Rc::clone(&reached);
    let target = root.to_path_buf();
    let down = down.inspect(move |entry| {
        if entry.path() == target {
            seen.set(true);
        }
    });
    let root = root.to_path_buf();
    let workspace = workspace.to_path_buf();
    let alone = std::iter::once(()).flat_map(move |()| {
        (!reached.get())
            .then(|| {
                let mut builder = walk(&root, false);
                if let Some(above) = ChatFolderIgnores::between(&workspace, &root) {
                    let walked = root.clone();
                    builder.filter_entry(move |entry| !above.leave_out(&walked, entry));
                }
                builder.build().filter_map(Result::ok)
            })
            .into_iter()
            .flatten()
    });
    Box::new(down.chain(alone))
}

/// The folder among `root`'s parents that is the chat's own folder `chat`,
/// written as `root` writes it.
fn chat_folder_above(root: &Path, chat: &Path) -> Option<std::path::PathBuf> {
    let chat = haarness_core::paths::canonicalize(chat).ok()?;
    root.ancestors()
        .skip(1)
        .find(|folder| {
            haarness_core::paths::canonicalize(folder).is_ok_and(|folder| folder == chat)
        })
        .map(Path::to_path_buf)
}

/// In the chat home, the ignore files of the folders from a chat's own
/// folder down to a subfolder it names although they leave the subfolder
/// out (see [`grep_files`]), weighed as a walk weighs its parents': for each
/// kind the nearest folder's line decides, `.ignore` files always count,
/// and `.gitignore` files and Git excludes when a repository holds them (up
/// to the nearest repository's own folder). The walk of the subfolder
/// reads its own folders' files and leaves out what they leave out; a `!`
/// line among them still takes a file back from the folders above. No
/// folder above the chat's own counts.
struct ChatFolderIgnores {
    /// The searched folder, canonical: walked paths are matched under it.
    root: std::path::PathBuf,
    /// The folders from the searched one's parent up to the chat's own,
    /// the nearest first.
    above: Vec<FolderRules>,
    /// Whether a repository holds the searched folder (its own `.git`, or
    /// one above it), so `.gitignore` files count.
    in_repository: bool,
    /// The searched folder's own and those in it, read as the walk meets
    /// them.
    below: std::sync::Mutex<
        std::collections::HashMap<std::path::PathBuf, std::sync::Arc<FolderRules>>,
    >,
}

/// One folder's ignore files.
struct FolderRules {
    ignore: Option<ignore::gitignore::Gitignore>,
    git_ignore: Option<ignore::gitignore::Gitignore>,
    git_exclude: Option<ignore::gitignore::Gitignore>,
    /// Whether the folder is a repository's own (holds `.git`).
    repository: bool,
}

impl FolderRules {
    fn read(folder: &Path) -> Self {
        let matcher = |file: &str| {
            let path = folder.join(file);
            path.is_file().then(|| {
                let mut builder = ignore::gitignore::GitignoreBuilder::new(folder);
                // As a walk does, an unreadable line or file leaves the rest.
                let _ = builder.add(&path);
                builder
                    .build()
                    .unwrap_or_else(|_| ignore::gitignore::Gitignore::empty())
            })
        };
        Self {
            ignore: matcher(".ignore"),
            git_ignore: matcher(".gitignore"),
            git_exclude: matcher(".git/info/exclude"),
            repository: folder.join(".git").exists(),
        }
    }
}

impl ChatFolderIgnores {
    /// Those of the folders from `chat` down to `root`; none when `root` is
    /// `chat`, a file, or outside it.
    fn between(chat: &Path, root: &Path) -> Option<Self> {
        let chat = haarness_core::paths::canonicalize(chat).ok()?;
        let root = haarness_core::paths::canonicalize(root).ok()?;
        if root == chat || !root.is_dir() || !root.starts_with(&chat) {
            return None;
        }
        let above = root
            .ancestors()
            .skip(1)
            .take_while(|folder| folder.starts_with(&chat))
            .map(FolderRules::read)
            .collect();
        let in_repository = root.ancestors().any(|folder| folder.join(".git").exists());
        Some(Self {
            root,
            above,
            in_repository,
            below: std::sync::Mutex::default(),
        })
    }

    fn below(&self, folder: &Path) -> std::sync::Arc<FolderRules> {
        let mut below = self
            .below
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::sync::Arc::clone(
            below
                .entry(folder.to_path_buf())
                .or_insert_with(|| std::sync::Arc::new(FolderRules::read(folder))),
        )
    }

    /// Whether the folders from `entry`'s own (of a walk of `walked`) up to
    /// the chat's leave it out.
    fn leave_out(&self, walked: &Path, entry: &ignore::DirEntry) -> bool {
        if entry.depth() == 0 {
            return false;
        }
        let Ok(relative) = entry.path().strip_prefix(walked) else {
            return false;
        };
        let path = self.root.join(relative);
        let folder = entry.file_type().is_some_and(|kind| kind.is_dir());
        let own = path
            .ancestors()
            .skip(1)
            .take_while(|parent| parent.starts_with(&self.root))
            .map(|parent| self.below(parent))
            .collect::<Vec<_>>();
        let in_repository = self.in_repository || own.iter().any(|rules| rules.repository);
        let matched = |matcher: &Option<ignore::gitignore::Gitignore>| {
            matcher.as_ref().map_or(ignore::Match::None, |matcher| {
                matcher.matched(&path, folder).map(|_| ())
            })
        };
        let (mut ignore, mut git_ignore, mut git_exclude) = (
            ignore::Match::None,
            ignore::Match::None,
            ignore::Match::None,
        );
        let mut past_repository = false;
        for rules in own.iter().map(|rules| &**rules).chain(&self.above) {
            ignore = ignore.or(matched(&rules.ignore));
            if in_repository && !past_repository {
                git_ignore = git_ignore.or(matched(&rules.git_ignore));
                git_exclude = git_exclude.or(matched(&rules.git_exclude));
            }
            past_repository |= rules.repository;
        }
        ignore.or(git_ignore).or(git_exclude).is_ignore()
    }
}

/// The `old` and `new` texts of an edit as a file whose lines mostly end in
/// `\r\n` spells them. `read` shows lines without their `\r`, so an `old`
/// of several lines that does not occur as written is given those ends; one
/// that does is a stretch of LF lines and keeps its own. The lines `new`
/// adds end as the line `old` is found in does: in `\r\n` there (`new`
/// given those ends), else as `new` writes them, so an LF stretch, or a CSV
/// cell holding bare LF, keeps its LF. Any other file's texts stay as they
/// are.
pub(super) fn crlf_edit<'a>(
    text: &str,
    old: &'a str,
    new: &'a str,
) -> (Cow<'a, str>, Cow<'a, str>) {
    let crlf = text.matches("\r\n").count();
    if crlf * 2 <= text.matches('\n').count() {
        return (Cow::Borrowed(old), Cow::Borrowed(new));
    }
    let spell = |value: &'a str| {
        if value.contains('\n') {
            Cow::Owned(value.replace("\r\n", "\n").replace('\n', "\r\n"))
        } else {
            Cow::Borrowed(value)
        }
    };
    let old = if old.contains('\n') && !text.contains(old) {
        spell(old)
    } else {
        Cow::Borrowed(old)
    };
    // Where `old` is first found; an `old` that is not found fails the edit.
    let Some(at) = text.find(old.as_ref()).filter(|_| !old.is_empty()) else {
        return (old, Cow::Borrowed(new));
    };
    let crlf_here = if old.contains('\n') {
        old.contains("\r\n")
    } else {
        // The end of the line `old` sits on; the last line, which has
        // none, ends as most do.
        let after = at + old.len();
        text[after..]
            .find('\n')
            .is_none_or(|end| text[..after + end].ends_with('\r'))
    };
    let new = if crlf_here {
        spell(new)
    } else {
        Cow::Borrowed(new)
    };
    (old, new)
}

/// Where an `old` of several lines is in `text` when the file's lines end
/// in `\n` here and `\r\n` there (`read` shows neither, so an `old` copied
/// from it matches no one spelling): each place its lines meet consecutive
/// lines of the file, whatever each of them ends with. The `new` for each
/// place takes the line ends of the first line it replaces. Only for an
/// `old` [`crlf_edit`]'s spellings find nowhere.
pub(super) fn any_line_ends(
    text: &str,
    old: &str,
    new: &str,
) -> Vec<(std::ops::Range<usize>, String)> {
    let old = old.replace("\r\n", "\n");
    if !old.contains('\n') {
        return Vec::new();
    }
    let pattern = old
        .split('\n')
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join("\r?\n");
    let Ok(lines) = regex::Regex::new(&pattern) else {
        return Vec::new();
    };
    let new = new.replace("\r\n", "\n");
    lines
        .find_iter(text)
        .map(|found| {
            let crlf = found
                .as_str()
                .find('\n')
                .is_some_and(|end| found.as_str()[..end].ends_with('\r'));
            let new = if crlf {
                new.replace('\n', "\r\n")
            } else {
                new.clone()
            };
            (found.range(), new)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the open does not wait: the file it gives is read and written as
    /// any other, so a filesystem that heeds the flag on a regular file never
    /// answers a read or a write with "try again".
    #[cfg(unix)]
    #[test]
    fn a_checked_file_is_read_and_written_as_usual() {
        use nix::fcntl::{FcntlArg, OFlag, fcntl};
        let root = std::env::temp_dir().join(format!("haarness-opened-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("note.txt");
        std::fs::write(&path, "one\n").unwrap();
        for options in [
            std::fs::OpenOptions::new().read(true).clone(),
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .clone(),
        ] {
            let (file, _) = open_regular(&path, &options).unwrap();
            let flags = OFlag::from_bits_truncate(fcntl(&file, FcntlArg::F_GETFL).unwrap());
            assert!(!flags.contains(OFlag::O_NONBLOCK), "{options:?}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_old_across_mixed_line_ends_is_found_line_by_line() {
        // As Git keeps a file committed with mixed ends; read showed the
        // lines without them.
        let text = "[network]\r\nhost = inventory.local\r\ntimeout = 30\n\n[storage]\r\n";
        let old = "[network]\nhost = inventory.local\ntimeout = 30\n";
        let (spelled, _) = crlf_edit(text, old, "x");
        assert!(
            !text.contains(spelled.as_ref()),
            "the one spelling of old is not there"
        );
        let found = any_line_ends(
            text,
            old,
            "[network]\nhost = inventory.local\ntimeout = 60\n",
        );
        assert_eq!(
            found,
            vec![(
                0..text.find("\n[storage]").unwrap(),
                "[network]\r\nhost = inventory.local\r\ntimeout = 60\r\n".to_owned()
            )]
        );
        // A line of old is a line of the file, not text that runs on.
        assert!(any_line_ends("a\r\nbc\n", "a\nb\nc", "x").is_empty());
        assert!(any_line_ends("one line", "one line", "x").is_empty());
    }

    fn split(text: &[u8], keep: usize) -> Vec<(Vec<u8>, bool)> {
        let cancel = CancellationToken::new();
        // A tiny buffer, so lines and `\r\n` fall across reads.
        let mut lines = Lines::new(BufReader::with_capacity(3, text), keep);
        let mut found = Vec::new();
        while let Some(line) = lines.next_line(&cancel).unwrap() {
            found.push((line.bytes.to_vec(), line.long));
        }
        found
    }

    #[test]
    fn grep_leaves_text_that_is_not_utf8_unsearched_as_before() {
        let root = std::env::temp_dir().join(format!("haarness-grep-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let root = haarness_core::paths::canonicalize(&root).unwrap();
        std::fs::write(root.join("utf8.txt"), "hit\n").unwrap();
        std::fs::write(root.join("latin1.txt"), b"hit\ncaf\xe9 hit\n").unwrap();
        let secrets = super::super::SecretFilter::new(&root, ".", &root, None);
        let regex = regex::Regex::new("hit").unwrap();
        let cancel = CancellationToken::new();
        let found = grep_walk(&regex, &root, &root, None, &secrets, &cancel).unwrap();
        assert_eq!(found.matches, vec!["utf8.txt:1:hit".to_owned()]);
        assert_eq!(found.stopped, None);
        cancel.cancel();
        assert!(matches!(
            grep_walk(&regex, &root, &root, None, &secrets, &cancel),
            Err(CoreError::Cancelled)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Every match of `needle` under `root`, sorted, for a chat working in
    /// `chat` (in the chat home `chat_home`, when there is one).
    #[cfg(unix)]
    fn chat_grep(root: &Path, chat: &Path, chat_home: Option<&Path>) -> Vec<String> {
        let secrets = super::super::SecretFilter::new(chat, ".", root, None);
        let regex = regex::Regex::new("needle").unwrap();
        let mut found = grep_walk(
            &regex,
            root,
            chat,
            chat_home,
            &secrets,
            &CancellationToken::new(),
        )
        .unwrap()
        .matches;
        found.sort();
        found
    }

    /// The chat home under a repository whose `.gitignore` hides everything
    /// (a home folder kept in Git): a chat's search still sees its files,
    /// and its own ignore files still count, in a search of a subfolder too
    /// (a folder named there is searched whatever they say, as ripgrep
    /// does). A folder that is not in the chat home reads the ignore files
    /// above it, as ripgrep does.
    #[cfg(unix)]
    #[test]
    fn grep_in_the_chat_home_reads_no_ignore_file_above_it() {
        let root = grep_fixture(&[(".gitignore", b"*\n")]);
        std::fs::create_dir(root.join(".git")).unwrap();
        let home = root.join("Chat folders");
        let chat = home.join("2026-10-28/chat-1a2b");
        let web = chat.join("packages/web");
        std::fs::create_dir_all(chat.join("notes")).unwrap();
        std::fs::create_dir_all(web.join("node_modules/dep")).unwrap();
        std::fs::create_dir_all(web.join("dist")).unwrap();
        for file in [
            "plan.md",
            "skipped.md",
            "notes/deep.md",
            "notes/deep.log",
            "packages/web/index.js",
            "packages/web/node_modules/dep/index.js",
            "packages/web/dist/index.js",
        ] {
            std::fs::write(chat.join(file), "needle\n").unwrap();
        }
        std::fs::write(chat.join(".ignore"), "skipped.md\n*.log\n").unwrap();
        std::fs::write(chat.join(".gitignore"), "node_modules/\n").unwrap();
        std::fs::write(web.join(".gitignore"), "dist/\n").unwrap();
        let search = |root: &Path| chat_grep(root, &chat, Some(&home));
        assert_eq!(
            search(&chat),
            [
                "notes/deep.md:1:needle",
                "packages/web/index.js:1:needle",
                "plan.md:1:needle"
            ]
        );
        assert_eq!(search(&chat.join("notes")), ["notes/deep.md:1:needle"]);
        assert_eq!(search(&web), ["packages/web/index.js:1:needle"]);
        assert_eq!(
            search(&web.join("node_modules")),
            ["packages/web/node_modules/dep/index.js:1:needle"]
        );
        // Anywhere else, as before.
        assert!(chat_grep(&chat, &chat, None).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A chat's folder in a chat home no repository holds: a search of a
    /// subfolder leaves out what the chat's `.ignore` leaves out; its
    /// `.gitignore` counts once the folder is a repository.
    #[cfg(unix)]
    #[test]
    fn a_subfolder_search_in_the_chat_home_keeps_the_chat_s_ignore_files() {
        let root = grep_fixture(&[]);
        let home = root.join("Chat folders");
        let chat = home.join("2026-10-28/chat-1a2b");
        std::fs::create_dir_all(chat.join("sub/build")).unwrap();
        for file in ["sub/a.log", "sub/b.txt", "sub/build/c.txt"] {
            std::fs::write(chat.join(file), "needle\n").unwrap();
        }
        std::fs::write(chat.join(".ignore"), "*.log\n").unwrap();
        std::fs::write(chat.join(".gitignore"), "build/\n").unwrap();
        let search = || chat_grep(&chat.join("sub"), &chat, Some(&home));
        assert_eq!(search(), ["sub/b.txt:1:needle", "sub/build/c.txt:1:needle"]);
        std::fs::create_dir(chat.join(".git")).unwrap();
        assert_eq!(search(), ["sub/b.txt:1:needle"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A chat's folder that is a Git repository: a search of a subfolder
    /// finds what ripgrep finds there with the chat's folder outermost, as
    /// a search of the whole folder does. A `!` line in the subfolder takes
    /// a file back from the chat's `.gitignore`, the repository's excludes
    /// count, and a repository nested below reads none of the chat's lines.
    #[cfg(unix)]
    #[test]
    fn a_subfolder_search_in_the_chat_home_weighs_ignore_files_as_ripgrep_does() {
        let root = grep_fixture(&[]);
        let home = root.join("Chat folders");
        let chat = home.join("2026-10-28/chat-1a2b");
        std::fs::create_dir_all(chat.join(".git/info")).unwrap();
        std::fs::create_dir_all(chat.join("sub/lib/.git")).unwrap();
        std::fs::write(chat.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(chat.join(".git/info/exclude"), "*.tmp\n").unwrap();
        std::fs::write(chat.join("sub/.gitignore"), "!keep.log\n").unwrap();
        for file in [
            "sub/keep.log",
            "sub/drop.log",
            "sub/ok.txt",
            "sub/a.tmp",
            "sub/lib/x.log",
        ] {
            std::fs::write(chat.join(file), "needle\n").unwrap();
        }
        let found = [
            "sub/keep.log:1:needle",
            "sub/lib/x.log:1:needle",
            "sub/ok.txt:1:needle",
        ];
        assert_eq!(chat_grep(&chat.join("sub"), &chat, Some(&home)), found);
        assert_eq!(chat_grep(&chat, &chat, Some(&home)), found);
        assert_eq!(
            chat_grep(&chat.join("sub/lib"), &chat, Some(&home)),
            ["sub/lib/x.log:1:needle"]
        );
        // A folder named although hidden on the way down: a `!` line in it
        // still takes a file back from the chat's `.gitignore`, and a
        // repository nested in it reads none of the chat's lines.
        let inner = chat.join(".hidden/inner");
        std::fs::create_dir_all(inner.join("lib/.git")).unwrap();
        std::fs::write(inner.join(".gitignore"), "!h3.log\n").unwrap();
        for file in ["h3.log", "other.log", "ok.txt", "lib/y.log"] {
            std::fs::write(inner.join(file), "needle\n").unwrap();
        }
        assert_eq!(
            chat_grep(&inner, &chat, Some(&home)),
            [
                ".hidden/inner/h3.log:1:needle",
                ".hidden/inner/lib/y.log:1:needle",
                ".hidden/inner/ok.txt:1:needle",
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn grep_fixture(files: &[(&str, &[u8])]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("haarness-grep-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let root = haarness_core::paths::canonicalize(&root).unwrap();
        for (name, body) in files {
            std::fs::write(root.join(name), body).unwrap();
        }
        root
    }

    fn grep(root: &Path, pattern: &str) -> GrepFound {
        let secrets = super::super::SecretFilter::new(root, ".", root, None);
        let regex = regex::Regex::new(pattern).unwrap();
        grep_walk(
            &regex,
            root,
            root,
            None,
            &secrets,
            &CancellationToken::new(),
        )
        .unwrap()
    }

    #[test]
    fn grep_says_it_stopped_only_when_there_were_more_matches_than_its_limit() {
        let six = "hit\n".repeat(6_000);
        let four = "hit\n".repeat(4_000);
        let root = grep_fixture(&[("a.txt", six.as_bytes()), ("b.txt", four.as_bytes())]);
        let exact = grep(&root, "hit");
        assert_eq!(exact.matches.len(), GREP_MAX_MATCHES);
        assert_eq!(exact.stopped, None);
        std::fs::write(root.join("c.txt"), "hit\n").unwrap();
        let more = grep(&root, "hit");
        assert_eq!(more.matches.len(), GREP_MAX_MATCHES);
        assert_eq!(more.stopped, Some(GrepStop::Matches));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_large_file_is_searched_a_line_at_a_time_and_a_line_in_its_first_mib() {
        // Over the size read whole: line numbers and cut lines as for any file.
        let mut large = "filler line\n".repeat(800_000).into_bytes();
        large.extend_from_slice(format!("needle{}\n", "q".repeat(3_000)).as_bytes());
        // A match past a line's first MiB is not found, in a file of any size.
        let far = format!("{}needle\n", "p".repeat(GREP_LINE_BYTES + 10));
        let root = grep_fixture(&[("large.log", &large), ("far.js", far.as_bytes())]);
        assert!(std::fs::metadata(root.join("large.log")).unwrap().len() > GREP_WHOLE_FILE_BYTES);
        let found = grep(&root, "needle");
        assert_eq!(
            found.matches,
            vec![format!(
                "large.log:800001:needle{}{LINE_CUT}",
                "q".repeat(LINE_CHARS - 6)
            )]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lines_break_where_str_lines_does() {
        for text in [
            "",
            "a",
            "a\n",
            "\n",
            "\n\n",
            "a\nb",
            "a\r\nb\r\n",
            "a\r",
            "a\rb\n",
            "\r\n\r\n",
            "a\r\r\n",
            "x\n\ny\n\r",
            "μ\r\nñ\n",
        ] {
            let found: Vec<String> = split(text.as_bytes(), 1_000)
                .into_iter()
                .map(|(bytes, long)| {
                    assert!(!long);
                    String::from_utf8(bytes).unwrap()
                })
                .collect();
            let expected: Vec<&str> = text.lines().collect();
            assert_eq!(found, expected, "{text:?}");
        }
    }

    #[test]
    fn a_long_line_keeps_only_its_head() {
        let text = format!("{}\r\nshort\n{}", "x".repeat(50), "y".repeat(10));
        let found = split(text.as_bytes(), 8);
        assert_eq!(found[0], (b"xxxxxxxx".to_vec(), true));
        assert_eq!(found[1], (b"short".to_vec(), false));
        assert_eq!(found[2], (b"yyyyyyyy".to_vec(), true));
        // Exactly `keep` bytes before `\r\n` is not long.
        assert_eq!(split(b"abcd\r\n", 4), vec![(b"abcd".to_vec(), false)]);
    }

    #[test]
    fn stop_ends_a_line_that_never_ends() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut lines = Lines::new(BufReader::new(io::repeat(b'x')), 16);
        assert!(matches!(
            lines.next_line(&cancel),
            Err(CoreError::Cancelled)
        ));
    }

    #[test]
    fn shown_text_is_cut_at_two_thousand_characters() {
        let long = "é".repeat(LINE_CHARS + 1);
        let cut = shown(&long, false);
        assert_eq!(cut.chars().count(), LINE_CHARS + LINE_CUT.chars().count());
        assert!(cut.ends_with(LINE_CUT));
        assert_eq!(shown("short", false), "short");
        assert_eq!(shown("head", true), format!("head{LINE_CUT}"));
    }

    /// A cut that splits a character of a long CJK or accented line is no
    /// sign of text that is not UTF-8.
    #[test]
    fn a_long_line_cut_inside_a_character_is_still_utf8() {
        for line in [
            "日".repeat(3_000),
            format!("a{}", "é".repeat(5_000)),
            "😀".repeat(2_500) + "x",
        ] {
            let text = format!("{line}\nnext\n");
            let mut lines = Lines::new(
                BufReader::with_capacity(5, text.as_bytes()),
                READ_LINE_BYTES,
            );
            let cancel = CancellationToken::new();
            let first = lines.next_line(&cancel).unwrap().unwrap();
            assert!(first.long);
            for lossy in [true, false] {
                let decoded = first.text(lossy).unwrap();
                assert!(
                    matches!(decoded, Cow::Borrowed(_)),
                    "{lossy}: {:?}",
                    &decoded[decoded.len() - 8..]
                );
                assert!(line.starts_with(decoded.as_ref()));
            }
        }
    }

    #[test]
    fn strict_text_keeps_a_cut_character_out_but_refuses_bad_bytes() {
        let cut = Line {
            bytes: &"aé".as_bytes()[..2],
            long: true,
        };
        assert_eq!(cut.text(false).unwrap(), "a");
        let bad = Line {
            bytes: &[b'a', 0xff, b'b'],
            long: false,
        };
        assert!(bad.text(false).is_none());
        assert_eq!(bad.text(true).unwrap(), "a\u{fffd}b");
    }
}
