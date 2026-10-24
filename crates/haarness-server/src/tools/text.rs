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
/// What one `read` returns at most: as much as any tool's output shows.
const READ_MAX_LINES: usize = super::TOOL_OUTPUT_MAX_LINES;
const READ_MAX_BYTES: usize = super::TOOL_OUTPUT_MAX_BYTES;

/// One line, without its `\n` or `\r\n`: its first `keep` bytes, and whether
/// it went on past them.
pub(super) struct Line {
    pub(super) bytes: Vec<u8>,
    pub(super) long: bool,
}

/// Splits text into lines exactly where `str::lines` does (at `\n`, a `\r`
/// right before it dropped), keeping at most `keep` bytes of each.
pub(super) struct Lines<R> {
    reader: R,
    keep: usize,
}

impl<R: BufRead> Lines<R> {
    pub(super) fn new(reader: R, keep: usize) -> Self {
        Self { reader, keep }
    }

    /// The next line, `None` at the end. Stop ends it even inside a line
    /// that never ends.
    pub(super) fn next_line(&mut self, cancel: &CancellationToken) -> CoreResult<Option<Line>> {
        let mut bytes = Vec::new();
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
            let room = self.keep.saturating_sub(bytes.len());
            bytes.extend_from_slice(&part[..part.len().min(room)]);
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
            bytes.truncate(length);
        }
        Ok(Some(Line {
            long: length > bytes.len(),
            bytes,
        }))
    }
}

impl Line {
    /// The line as text: invalid UTF-8 as U+FFFD when `lossy`, else `None`.
    /// A line cut short keeps a partial character at its end out.
    pub(super) fn text(&self, lossy: bool) -> Option<Cow<'_, str>> {
        if lossy {
            return Some(String::from_utf8_lossy(&self.bytes));
        }
        match std::str::from_utf8(&self.bytes) {
            Ok(text) => Some(Cow::Borrowed(text)),
            Err(error) if self.long && error.error_len().is_none() => Some(Cow::Borrowed(
                std::str::from_utf8(&self.bytes[..error.valid_up_to()]).ok()?,
            )),
            Err(_) => None,
        }
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

/// A file opened for its text, and whether a NUL near its start marks it as
/// binary data (then only UTF-8 text is shown, as before lines were read one
/// at a time; other text is shown with U+FFFD for what is not UTF-8).
pub(super) fn open_text(path: &Path) -> CoreResult<(impl BufRead, bool)> {
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
        return Err(CoreError::Tool(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let mut file = File::open(path).map_err(tool)?;
    let mut head = Vec::with_capacity(BINARY_PROBE_BYTES);
    (&mut file)
        .take(BINARY_PROBE_BYTES as u64)
        .read_to_end(&mut head)
        .map_err(tool)?;
    let binary = head.contains(&0);
    Ok((BufReader::new(io::Cursor::new(head).chain(file)), binary))
}

/// What `read` shows of a text file: `limit` lines from line `offset`
/// (zero-based), each cut at [`LINE_CHARS`] characters, joined by `\n`. It
/// stops early at 2,000 lines or 50 KB and then says where to go on.
pub(super) fn read_text(
    path: &Path,
    offset: usize,
    limit: usize,
    cancel: &CancellationToken,
) -> CoreResult<String> {
    let (reader, binary) = open_text(path)?;
    let mut lines = Lines::new(reader, READ_LINE_BYTES);
    let mut shown_lines = Vec::new();
    let mut bytes = 0_usize;
    let mut index = 0_usize;
    let mut stop = None;
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
    let mut text = shown_lines.join("\n");
    if let Some(stop) = stop {
        let next = offset + shown_lines.len();
        let note = format!("(stopped at {stop}; read on with offset={next})");
        text = if text.is_empty() {
            note
        } else {
            format!("{text}\n\n{note}")
        };
    }
    Ok(text)
}

/// Bytes of a line `grep` searches: a match further into a longer line is
/// not found.
const GREP_LINE_BYTES: usize = 1024 * 1024;
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
pub(super) fn grep_walk(
    regex: &regex::Regex,
    root: &Path,
    workspace: &Path,
    secrets: &super::SecretFilter,
    cancel: &CancellationToken,
) -> CoreResult<GrepFound> {
    let mut found = GrepFound {
        matches: Vec::new(),
        skipped: super::Skipped::default(),
        stopped: None,
    };
    let walk = ignore::WalkBuilder::new(root)
        .standard_filters(true)
        .build()
        .filter_map(Result::ok)
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
        let shown_path = entry
            .path()
            .strip_prefix(workspace)
            .unwrap_or(entry.path())
            .display()
            .to_string();
        let Ok((reader, _)) = open_text(entry.path()) else {
            continue;
        };
        let mut lines = Lines::new(reader, GREP_LINE_BYTES);
        let mut in_file = Vec::new();
        let mut number = 0_usize;
        let text_file = loop {
            let line = match lines.next_line(cancel) {
                Ok(Some(line)) => line,
                Ok(None) => break true,
                Err(CoreError::Cancelled) => return Err(CoreError::Cancelled),
                Err(_) => break false,
            };
            number += 1;
            let Some(text) = line.text(false) else {
                break false;
            };
            if regex.is_match(&text) {
                in_file.push(format!("{shown_path}:{number}:{}", shown(&text, line.long)));
                if found.matches.len() + in_file.len() == GREP_MAX_MATCHES {
                    found.stopped = Some(GrepStop::Matches);
                    break true;
                }
            }
        };
        if text_file {
            found.matches.append(&mut in_file);
        }
        if found.stopped.is_some() {
            break;
        }
    }
    Ok(found)
}

/// The `old` and `new` texts of an edit as a file with CRLF line ends spells
/// them. `read` shows lines without their `\r`, so an `old` of several lines
/// never occurs in such a file as written: when it does not, and most of the
/// file's lines end in `\r\n`, both are given those ends. Otherwise, as
/// they are.
pub(super) fn crlf_edit<'a>(
    text: &str,
    old: &'a str,
    new: &'a str,
) -> (Cow<'a, str>, Cow<'a, str>) {
    let crlf = text.matches("\r\n").count();
    if !old.contains('\n') || text.contains(old) || crlf * 2 <= text.matches('\n').count() {
        return (Cow::Borrowed(old), Cow::Borrowed(new));
    }
    let spell = |value: &str| value.replace("\r\n", "\n").replace('\n', "\r\n");
    (Cow::Owned(spell(old)), Cow::Owned(spell(new)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(text: &[u8], keep: usize) -> Vec<(Vec<u8>, bool)> {
        let cancel = CancellationToken::new();
        // A tiny buffer, so lines and `\r\n` fall across reads.
        let mut lines = Lines::new(BufReader::with_capacity(3, text), keep);
        let mut found = Vec::new();
        while let Some(line) = lines.next_line(&cancel).unwrap() {
            found.push((line.bytes, line.long));
        }
        found
    }

    #[test]
    fn grep_leaves_text_that_is_not_utf8_unsearched_as_before() {
        let root = std::env::temp_dir().join(format!("haarness-grep-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("utf8.txt"), "hit\n").unwrap();
        std::fs::write(root.join("latin1.txt"), b"hit\ncaf\xe9 hit\n").unwrap();
        let secrets = super::super::SecretFilter::new(&root, ".", &root, None);
        let regex = regex::Regex::new("hit").unwrap();
        let cancel = CancellationToken::new();
        let found = grep_walk(&regex, &root, &root, &secrets, &cancel).unwrap();
        assert_eq!(found.matches, vec!["utf8.txt:1:hit".to_owned()]);
        assert_eq!(found.stopped, None);
        cancel.cancel();
        assert!(matches!(
            grep_walk(&regex, &root, &root, &secrets, &cancel),
            Err(CoreError::Cancelled)
        ));
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

    #[test]
    fn strict_text_keeps_a_cut_character_out_but_refuses_bad_bytes() {
        let cut = Line {
            bytes: "aé".as_bytes()[..2].to_vec(),
            long: true,
        };
        assert_eq!(cut.text(false).unwrap(), "a");
        let bad = Line {
            bytes: vec![b'a', 0xff, b'b'],
            long: false,
        };
        assert!(bad.text(false).is_none());
        assert_eq!(bad.text(true).unwrap(), "a\u{fffd}b");
    }
}
