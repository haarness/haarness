//! Project files mentioned in answers, opened with a click.
//!
//! The Markdown renderer puts a [`Reference`] on the spans of inline code
//! shaped like a path (`src/app.rs:42`) and of a Markdown link whose target
//! is a path ([`haarness_opentui::Span::opens`]). On a computer that has the
//! chat's files, the transcript turns those that point at a real file of the
//! project into click targets, and a click starts `$VISUAL` or `$EDITOR` on
//! that file at that line.
//!
//! Three rules keep a click from running anything the project ships. The
//! editor receives one argument naming the file, absolute and resolved, so
//! no file name can turn into an option. `open`, `xdg-open` and the other
//! system openers, which start scripts and applications, are never handed a
//! file. And a path counts only while its resolved location, symbolic links
//! followed, stays inside the project folder.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

/// A path in an answer and the line to open it at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reference {
    pub path: String,
    pub line: Option<u32>,
}

impl Reference {
    /// The form a span keeps: line number, colon, path. The number never
    /// holds a colon, so the first one splits them, whatever the path holds.
    pub(crate) fn encode(&self) -> Arc<str> {
        let line = self.line.map(|line| line.to_string()).unwrap_or_default();
        format!("{line}:{}", self.path).into()
    }

    pub(crate) fn decode(encoded: &str) -> Option<Self> {
        let (number, path) = encoded.split_once(':')?;
        let line = if number.is_empty() {
            None
        } else {
            Some(number.parse().ok()?)
        };
        (!path.is_empty()).then(|| Self {
            path: path.to_owned(),
            line,
        })
    }
}

/// Characters that make inline code a command, an expression or a pattern
/// rather than a file name.
const NOT_IN_NAMES: &str = "!\"$%&'*,;<=>?^`{|}";

/// The path in a piece of inline code, if the code is shaped like one.
///
/// Turned down: anything with white space, quoting, shell or glob syntax;
/// text opening with `-` (an option), `~` (a home folder), `@` (a package
/// scope) or `+`; web addresses; Rust-style `a::b` and `key:value` pairs.
/// Kept: text holding a folder separator, or a bare name with a lettered
/// extension (`main.rs`, `.env`). Parentheses are kept only as a whole
/// folder name at the start of a segment (`app/(site)/page.tsx`), so a call
/// such as `run()` stays code. `backslash` makes `\` a separator, as on
/// Windows; elsewhere it marks an escape and the code is not a path. The
/// file is not looked for here.
pub(crate) fn code_reference(code: &str, backslash: bool) -> Option<Reference> {
    let text = code.trim();
    if text.is_empty()
        || text.len() > 400
        || names_a_share(text)
        || text.starts_with(['-', '~', '@', '+'])
        || text.contains("://")
        || text.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || NOT_IN_NAMES.contains(character)
                || (character == '\\' && !backslash)
        })
    {
        return None;
    }
    let (path, line) = locate(text);
    if path.is_empty() || path.ends_with(['/', '\\']) || without_drive(path).contains(':') {
        return None;
    }
    let separators: &[char] = if backslash { &['/', '\\'] } else { &['/'] };
    let foldered = path.contains(separators);
    if path.contains(['(', ')']) && !(foldered && path.split(separators).all(grouped_or_plain)) {
        return None;
    }
    let name = path.rsplit(separators).next().unwrap_or(path);
    if !foldered && !has_extension(name) {
        return None;
    }
    Some(Reference {
        path: path.to_owned(),
        line,
    })
}

/// Whether `path` opens with two separators in any mix (`//`, `\\`, `/\`):
/// a share on another computer, or a Windows device path (`\\?\`, `\\.\`).
/// Windows would reach such a path over the network just to look at it.
fn names_a_share(path: &str) -> bool {
    let mut characters = path.chars();
    matches!(
        (characters.next(), characters.next()),
        (Some('/' | '\\'), Some('/' | '\\'))
    )
}

/// Whether a path segment has no parentheses, or opens with one group of
/// them and has none after it: `(site)`, `(.)photo`.
fn grouped_or_plain(segment: &str) -> bool {
    match segment.strip_prefix('(') {
        None => !segment.contains(['(', ')']),
        Some(inner) => inner.split_once(')').is_some_and(|(group, rest)| {
            !group.is_empty() && !group.contains('(') && !rest.contains(['(', ')'])
        }),
    }
}

/// The path a Markdown link's target points at on this computer, if any.
///
/// Read as a path: a relative or absolute one, its `%20`-style escapes
/// decoded, a query dropped, a line given as `:42`, `:42-60` or a `#L42`
/// fragment; and a `file:` URL with no host or `localhost`. Not a path:
/// other schemes (`https:`, `mailto:`, `tel:`), `//server/…` and `\\server`
/// shares, and a bare `#heading`. A name with an extension followed by a
/// line, `README.md:3`, is a path even though it looks like a scheme.
pub(crate) fn link_reference(href: &str) -> Option<Reference> {
    let text = href.trim();
    if text.is_empty()
        || text.starts_with('#')
        || names_a_share(text)
        || text.chars().any(char::is_control)
    {
        return None;
    }
    if text
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
    {
        let url = url::Url::parse(text).ok()?;
        if url
            .host_str()
            .is_some_and(|host| !host.is_empty() && !host.eq_ignore_ascii_case("localhost"))
        {
            return None;
        }
        // `file:////host/share` is a share too, whatever the URL parser
        // makes of it.
        if text[5..]
            .strip_prefix("//")
            .is_some_and(|rest| names_a_share(rest) || names_a_share(url.path()))
        {
            return None;
        }
        let path = url.to_file_path().ok()?;
        let path = path.to_str()?;
        if names_a_share(path) {
            return None;
        }
        return Some(Reference {
            path: path.to_owned(),
            line: url.fragment().and_then(fragment_line),
        });
    }
    let (before, fragment) = text
        .split_once('#')
        .map_or((text, None), |(before, fragment)| (before, Some(fragment)));
    let before = before.split('?').next().unwrap_or(before);
    let (path, line) = locate(before);
    if path.is_empty() {
        return None;
    }
    // Something like a scheme is one, unless it is `name.ext:line`.
    if scheme(before).is_some()
        && (line.is_none() || path.contains(['/', '\\']) || !path.contains('.'))
    {
        return None;
    }
    // Escapes are read before the share test: `%2F%2Fhost` is `//host`.
    let path = unescape(path)?;
    if path.chars().any(char::is_control) || names_a_share(&path) {
        return None;
    }
    Some(Reference {
        path,
        line: fragment.and_then(fragment_line).or(line),
    })
}

/// The URL scheme at the start of `text`, if it has one; a drive letter
/// (`C:/…`) is not one.
fn scheme(text: &str) -> Option<&str> {
    let (scheme, _) = text.split_once(':')?;
    let mut characters = scheme.chars();
    let starts = characters.next()?.is_ascii_alphabetic();
    let rest = characters
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '+' | '.' | '-'));
    (starts && rest && without_drive(text) == text).then_some(scheme)
}

/// `path` after a leading drive letter (`C:/`, `C:\`), or all of it.
fn without_drive(path: &str) -> &str {
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
    {
        &path[2..]
    } else {
        path
    }
}

/// Whether `name` ends in `.` and an extension made of ASCII letters,
/// digits, `_` and `-`, at least one a letter: `.rs` and `.env` count, the
/// `.0` of `v2.0` does not.
fn has_extension(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        extension
            .chars()
            .any(|character| character.is_ascii_alphabetic())
            && extension.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
    })
}

/// Split the line number off the end of `text`. Accepted endings: `:42`,
/// `:42:7` (line and column), `:42-60` and `:42:7-60:3` (a span, opened at
/// its first line), `#L42`, `#L42C7` and `#L42-L60`. Line 0 counts as none.
fn locate(text: &str) -> (&str, Option<u32>) {
    if let Some(at) = text.rfind("#L")
        && let Some(line) = fragment_line(&text[at + 1..])
    {
        return (&text[..at], (line > 0).then_some(line));
    }
    for (at, _) in text.match_indices(':') {
        if let Some(line) = line_suffix(&text[at + 1..]) {
            return (&text[..at], (line > 0).then_some(line));
        }
    }
    (text, None)
}

/// The first number of `42`, `42:7`, `42-60` or `42:7-60:3`.
fn line_suffix(text: &str) -> Option<u32> {
    let (start, end) = text
        .split_once('-')
        .map_or((text, None), |(start, end)| (start, Some(end)));
    let first = numbered(start, ':')?;
    if let Some(end) = end {
        numbered(end, ':')?;
    }
    Some(first)
}

/// The first number of a `L42`, `L42C7`, `L42-L60` or `L42-60` fragment.
fn fragment_line(fragment: &str) -> Option<u32> {
    let rest = fragment.strip_prefix('L')?;
    let (start, end) = rest
        .split_once('-')
        .map_or((rest, None), |(start, end)| (start, Some(end)));
    let first = numbered(start, 'C')?;
    if let Some(end) = end {
        numbered(end.strip_prefix('L').unwrap_or(end), 'C')?;
    }
    Some(first)
}

/// The line of `42` or of `42<column mark>7`, digits only.
fn numbered(part: &str, column_mark: char) -> Option<u32> {
    let (line, column) = part
        .split_once(column_mark)
        .map_or((part, None), |(line, column)| (line, Some(column)));
    let digits = |value: &str| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(line) || column.is_some_and(|column| !digits(column)) {
        return None;
    }
    line.parse().ok()
}

/// `text` with `%xx` escapes decoded, if the result is UTF-8.
fn unescape(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `path`, relative to the project or absolute, as an absolute path inside
/// the project, worked out from its text alone: `.` and `..` applied, and
/// nothing read from the disk. It must fall under `root` (the project folder
/// as the server gives it) or `resolved_root` (the same folder with its links
/// followed). A share or a device path, a drive letter with no separator
/// after it (`C:notes.md`) and anything that climbs out of the project give
/// None, so the disk is never asked about a place outside the project.
pub(crate) fn inside_project(path: &str, root: &Path, resolved_root: &Path) -> Option<PathBuf> {
    let bytes = path.as_bytes();
    // A project kept on a share gets no links: every look would cross the
    // network.
    if names_a_share(path)
        || root.to_str().is_none_or(names_a_share)
        || (bytes.len() >= 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && !matches!(bytes.get(2), Some(b'/' | b'\\')))
    {
        return None;
    }
    let named = Path::new(path);
    let candidate = if named.is_absolute() {
        lexical(named)?
    } else {
        lexical(&root.join(named))?
    };
    (candidate.starts_with(lexical(root)?) || candidate.starts_with(resolved_root))
        .then_some(candidate)
}

/// `path` with `.` dropped and each `..` taking off the folder before it,
/// from the text alone. None for a `..` above the top, and on Windows for a
/// prefix other than a drive letter.
fn lexical(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                if !matches!(prefix.kind(), std::path::Prefix::Disk(_)) {
                    return None;
                }
                out.push(component.as_os_str());
            }
            Component::RootDir | Component::Normal(_) => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
        }
    }
    Some(out)
}

/// The canonical path of the regular file `reference` points at, when it
/// lies inside the project: first by its text ([`inside_project`]), then
/// link by link ([`without_links`]), then once more after the disk has
/// resolved it. `root` is the project folder as given, `resolved_root` the
/// same with its links followed. Anything else (missing, a folder, outside)
/// is None.
pub(crate) fn project_file(
    root: &Path,
    resolved_root: &Path,
    reference: &Reference,
) -> Option<PathBuf> {
    let candidate = inside_project(&reference.path, root, resolved_root)?;
    let candidate = without_links(&candidate, root, resolved_root)?;
    let file = dunce::canonicalize(candidate).ok()?;
    (file.starts_with(resolved_root) && file.metadata().ok()?.is_file()).then_some(file)
}

/// How many symbolic links a path may pass through before it counts as a
/// loop.
const LINK_HOPS: usize = 40;

/// `candidate` (absolute, inside the project by its text) with every
/// symbolic link on the way replaced by its target. A link is only read
/// (`lstat`, `readlink`), never followed, and its target must be inside the
/// project by its text before anything under it is looked at: a link to a
/// share or out of the project is refused without the disk going there.
/// None for such a link, a loop, or a name that is not there.
pub(crate) fn without_links(
    candidate: &Path,
    root: &Path,
    resolved_root: &Path,
) -> Option<PathBuf> {
    let given_root = lexical(root)?;
    // The project folder `path` is under, and the names below it, the next
    // one to walk last.
    let below = |path: &Path| -> Option<(PathBuf, Vec<OsString>)> {
        let base = if path.starts_with(resolved_root) {
            resolved_root.to_path_buf()
        } else {
            given_root.clone()
        };
        let mut names: Vec<OsString> = path
            .strip_prefix(&base)
            .ok()?
            .components()
            .map(|component| component.as_os_str().to_owned())
            .collect();
        names.reverse();
        Some((base, names))
    };
    let (mut current, mut names) = below(candidate)?;
    let mut hops = 0;
    while let Some(name) = names.pop() {
        let next = current.join(&name);
        if !std::fs::symlink_metadata(&next)
            .ok()?
            .file_type()
            .is_symlink()
        {
            current = next;
            continue;
        }
        hops += 1;
        if hops > LINK_HOPS {
            return None;
        }
        let target = std::fs::read_link(&next).ok()?;
        let target = if target.is_absolute() {
            target
        } else {
            current.join(target)
        };
        let through = inside_project(target.to_str()?, root, resolved_root)?;
        // Go on from the top of where the link leads, walking its names
        // first and then the rest of the path.
        let (base, leading) = below(&through)?;
        names.extend(leading);
        current = base;
    }
    Some(current)
}

/// What a click on a path found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClickCheck {
    /// A project file, to open.
    Open(Opening),
    /// No project file (any more): the click goes to what lies beneath.
    Gone,
    /// The disk did not answer within [`CLICK_CHECK`]; the path as written.
    Slow(String),
}

/// A file a click is about to open: canonical path, line, and the path
/// relative to the project for messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opening {
    pub path: PathBuf,
    pub line: Option<u32>,
    pub shown: String,
}

/// Age after which a lookup's answer is asked for again (the old answer
/// stays in use meanwhile).
const FRESH: Duration = Duration::from_secs(2);
/// A lookup running this long is stuck on a volume that stopped answering:
/// until it ends, every path waiting for a fresh answer is no link.
const STUCK: Duration = Duration::from_secs(1);
/// A GUI editor that exits with an error this soon after it was started is
/// reported; later, its exit is the person's own doing.
const LAUNCH_FAILURE: Duration = Duration::from_secs(5);
/// How long a click waits for its file to be checked; a slower disk opens
/// nothing rather than freeze the screen.
const CLICK_CHECK: Duration = Duration::from_millis(300);
/// Answers kept at most; above it, settled ones are dropped.
const REMEMBERED: usize = 2048;

/// What a reference to look up is: the project folder and the encoded
/// reference.
type Lookup = (PathBuf, Arc<str>);

/// Lookup answers for the paths drawn in the transcript.
///
/// The disk is only ever read on a thread of its own, so a slow or
/// unreachable volume never holds up a frame. A path seen for the first time
/// is plain text until its answer arrives, normally by the next frame. Once
/// answered, the answer is reused for [`FRESH`], then refreshed in the
/// background while the old one still shows, so a link does not blink. A
/// lookup that has been running for [`STUCK`] means the volume stopped
/// answering, and the paths still waiting on it are withdrawn until it ends.
#[derive(Default)]
pub(crate) struct ReplyFiles {
    known: Arc<Mutex<Known>>,
    lookups: std::cell::RefCell<Option<mpsc::Sender<Lookup>>>,
    /// GUI editors this TUI started, kept until they exit.
    started: std::cell::RefCell<Vec<Started>>,
}

#[derive(Default)]
struct Known {
    /// Project folder the answers belong to.
    root: PathBuf,
    answers: HashMap<Arc<str>, Answer>,
    /// When the lookup thread started the lookup it is on, if it is on one.
    busy: Option<Instant>,
    /// The project folder with its links followed, once the lookup thread
    /// has resolved it.
    resolved_root: Option<PathBuf>,
    /// How long a click's check is held up, standing for a slow disk (tests
    /// only).
    #[cfg(test)]
    check_delay: Duration,
}

impl Known {
    /// Whether the lookup under way has run for [`STUCK`].
    fn stuck(&self, now: Instant) -> bool {
        self.busy
            .is_some_and(|since| now.duration_since(since) >= STUCK)
    }
}

struct Answer {
    /// The file's canonical path, when it is a project file.
    file: Option<PathBuf>,
    at: Instant,
    /// When a lookup was asked for and has not answered yet.
    asked: Option<Instant>,
}

/// A GUI editor launched for a file.
struct Started {
    child: std::process::Child,
    editor: Option<String>,
    file: String,
    at: Instant,
}

impl ReplyFiles {
    /// Whether each of `links` (spans' [`Reference::encode`]) points at a
    /// file of the project in `root`, as last looked up, under one lock for
    /// the frame that asks; a missing or stale answer is looked up again in
    /// the background.
    pub(crate) fn are_files(&self, root: &Path, links: &[&Arc<str>]) -> Vec<bool> {
        let mut known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        if known.root != root {
            root.clone_into(&mut known.root);
            known.answers.clear();
            known.resolved_root = None;
        }
        let now = Instant::now();
        let stuck = known.stuck(now);
        links
            .iter()
            .map(|link| self.answer(&mut known, root, link, now, stuck))
            .collect()
    }

    /// [`Self::are_files`] for one of the links.
    fn answer(
        &self,
        known: &mut Known,
        root: &Path,
        link: &Arc<str>,
        now: Instant,
        stuck: bool,
    ) -> bool {
        if let Some(answer) = known.answers.get_mut(link) {
            return match answer.asked {
                Some(_) => answer.file.is_some() && !stuck,
                None => {
                    if now.duration_since(answer.at) >= FRESH {
                        answer.asked = Some(now);
                        self.ask(root, link);
                    }
                    answer.file.is_some()
                }
            };
        }
        if known.answers.len() >= REMEMBERED {
            known.answers.retain(|_, answer| answer.asked.is_some());
        }
        known.answers.insert(
            Arc::clone(link),
            Answer {
                file: None,
                at: now,
                asked: Some(now),
            },
        );
        self.ask(root, link);
        false
    }

    /// Queue `link` for the lookup thread, starting it the first time.
    fn ask(&self, root: &Path, link: &Arc<str>) {
        let mut lookups = self.lookups.borrow_mut();
        let sender = lookups.get_or_insert_with(|| lookup_thread(Arc::clone(&self.known)));
        if sender.send((root.to_owned(), Arc::clone(link))).is_err() {
            // The thread has ended; a later frame starts a new one.
            *lookups = None;
        }
    }

    /// Block until every lookup asked so far has answered (tests only).
    #[cfg(test)]
    pub(crate) fn settle(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
            if known.answers.values().all(|answer| answer.asked.is_none()) {
                return;
            }
            drop(known);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Leave `link` waiting since `asked` for a fresh answer, with the
    /// lookup thread busy since `busy` (or idle), as a slow volume or one
    /// that stopped answering leaves them (tests only).
    #[cfg(test)]
    pub(crate) fn stall(&self, link: &Arc<str>, asked: Instant, busy: Option<Instant>) {
        let mut known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(answer) = known.answers.get_mut(link) {
            answer.asked = Some(asked);
        }
        known.busy = busy;
    }

    /// Make every answer `ago` older (tests only).
    #[cfg(test)]
    pub(crate) fn age_answers(&self, ago: Duration) {
        let mut known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        for answer in known.answers.values_mut() {
            answer.at = answer.at.checked_sub(ago).unwrap_or(answer.at);
        }
    }

    /// The file a fresh answer (under [`FRESH`]) found for `link` in `root`,
    /// and the project folder with its links followed; and that folder
    /// alone when the answer is older.
    fn fresh(&self, root: &Path, link: &str) -> (Option<PathBuf>, Option<PathBuf>) {
        let known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        if known.root != root {
            return (None, None);
        }
        let file = known
            .answers
            .get(link)
            .filter(|answer| answer.at.elapsed() < FRESH)
            .and_then(|answer| answer.file.clone());
        (file, known.resolved_root.clone())
    }

    /// Hold every click's check up for `delay`, as a slow disk would (tests
    /// only).
    #[cfg(test)]
    pub(crate) fn slow_checks(&self, delay: Duration) {
        self.known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .check_delay = delay;
    }

    /// Move the launch of every tracked GUI editor `ago` into the past
    /// (tests only).
    #[cfg(all(test, unix))]
    pub(crate) fn launched_earlier(&self, ago: Duration) {
        for started in self.started.borrow_mut().iter_mut() {
            started.at = started.at.checked_sub(ago).unwrap_or(started.at);
        }
    }

    /// Track a GUI editor until it exits (see [`Self::failed_editors`]).
    /// `editor` is its name, or None for the system's text editor.
    pub(crate) fn started(&self, child: std::process::Child, editor: Option<String>, file: String) {
        self.started.borrow_mut().push(Started {
            child,
            editor,
            file,
            at: Instant::now(),
        });
    }

    /// GUI editors that exited with an error within [`LAUNCH_FAILURE`] of
    /// their launch, as `(editor, file)`; ones still running stay tracked.
    pub(crate) fn failed_editors(&self) -> Vec<(Option<String>, String)> {
        let mut failed = Vec::new();
        self.started
            .borrow_mut()
            .retain_mut(|started| match started.child.try_wait() {
                Ok(None) => true,
                Ok(Some(status)) => {
                    if !status.success() && started.at.elapsed() < LAUNCH_FAILURE {
                        failed.push((started.editor.clone(), started.file.clone()));
                    }
                    false
                }
                Err(_) => false,
            });
        failed
    }
}

/// Start the lookup thread. It ends with the [`ReplyFiles`] that owns the
/// sender, and drops answers for a project that is no longer the one shown.
/// The project folder's own links are resolved once it is there; while it
/// is not (a volume not mounted yet, a folder made later), it is tried again
/// at most every [`FRESH`].
fn lookup_thread(known: Arc<Mutex<Known>>) -> mpsc::Sender<Lookup> {
    let (sender, receiver) = mpsc::channel::<Lookup>();
    let spawned = std::thread::Builder::new()
        .name("reply-files".into())
        .spawn(move || {
            let mut resolved: Option<(PathBuf, PathBuf)> = None;
            let mut failed: Option<(PathBuf, Instant)> = None;
            for (root, link) in receiver {
                known.lock().unwrap_or_else(PoisonError::into_inner).busy = Some(Instant::now());
                if resolved.as_ref().is_none_or(|(folder, _)| *folder != root)
                    && failed
                        .as_ref()
                        .is_none_or(|(folder, at)| *folder != root || at.elapsed() >= FRESH)
                {
                    resolved = resolve_root(&root).map(|path| (root.clone(), path));
                    failed = resolved.is_none().then(|| (root.clone(), Instant::now()));
                }
                let resolved_root = resolved
                    .as_ref()
                    .filter(|(folder, _)| *folder == root)
                    .map(|(_, path)| path.as_path());
                let file = Reference::decode(&link).zip(resolved_root).and_then(
                    |(reference, resolved_root)| project_file(&root, resolved_root, &reference),
                );
                let mut known = known.lock().unwrap_or_else(PoisonError::into_inner);
                known.busy = None;
                if known.root == root {
                    known.resolved_root = resolved_root.map(Path::to_path_buf);
                    known.answers.insert(
                        link,
                        Answer {
                            file,
                            at: Instant::now(),
                            asked: None,
                        },
                    );
                }
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "the lookup thread for paths in answers did not start");
    }
    sender
}

/// The project folder with its links followed; None for a folder on a
/// share, which is never looked at, and for one that is not there.
fn resolve_root(root: &Path) -> Option<PathBuf> {
    lexical(root)?;
    if root.to_str().is_none_or(names_a_share) {
        return None;
    }
    dunce::canonicalize(root).ok()
}

/// `work`'s result, if it comes within `limit`. It runs on a thread of its
/// own, which a disk that never answers keeps; the caller goes on.
pub(crate) fn within<T: Send + 'static>(
    limit: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("reply-file-check".into())
        .spawn(move || {
            let _ = sender.send(work());
        })
        .ok()?;
    receiver.recv_timeout(limit).ok()
}

/// `$VISUAL` if set and not blank, else `$EDITOR`.
pub(crate) fn configured_editor() -> Option<String> {
    ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|editor| !editor.trim().is_empty())
}

/// The command that opens a file: program and arguments; `gui` when the
/// editor draws its own window, so it is started and left to run, and
/// otherwise it gets the terminal until it quits; and the editor's name for
/// a failure message, None for the system's text editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Launch {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub gui: bool,
    pub editor: Option<String>,
}

impl Launch {
    pub(crate) fn command(&self) -> std::process::Command {
        #[cfg(windows)]
        let program = std::env::var_os("PATH")
            .and_then(|path| find_program(&self.program, &path, &["exe", "cmd", "bat", "com"]))
            .map_or_else(|| self.program.clone(), PathBuf::into_os_string);
        #[cfg(not(windows))]
        let program = self.program.clone();
        let mut command = std::process::Command::new(program);
        command.args(&self.args);
        // A GUI editor gets a process group of its own: Ctrl+Z in the TUI
        // must not stop it, nor closing the terminal end it with unsaved work.
        #[cfg(unix)]
        if self.gui {
            std::os::unix::process::CommandExt::process_group(&mut command, 0);
        }
        command
    }
}

/// The command for `editor` (the text of `$VISUAL`/`$EDITOR`, or None) to
/// open the absolute path `file` at `line` on `os` (as
/// `std::env::consts::OS` names it).
///
/// Outside Windows the editor's text goes to `sh -lc` with `"$@"` after it,
/// the way Alt+E runs it, and the file and line follow as arguments, so the
/// shell never parses the path. GUI editors lose `--wait`, `-w` and the like:
/// the TUI does not wait for them, and a waiting CLI would otherwise linger
/// for every click until its tab is closed. Unset, or set to a system opener
/// that would run the file: the system's text editor on macOS (`open -t`,
/// which only ever opens text), Notepad on Windows, `vi` elsewhere (Alt+E's
/// choice too).
pub(crate) fn launch(editor: Option<&str>, os: &str, file: &Path, line: Option<u32>) -> Launch {
    // In shell text a backslash escapes; Windows paths use it as a separator.
    let shell = os != "windows";
    let configured = editor
        .map(str::trim)
        .filter(|editor| !editor.is_empty())
        .filter(|editor| !OPENERS.contains(&program_name(editor, shell).as_str()));
    let Some(editor) = configured else {
        return match os {
            "macos" => Launch {
                program: "open".into(),
                args: vec!["-t".into(), file.into()],
                gui: true,
                editor: None,
            },
            "windows" => Launch {
                program: "notepad.exe".into(),
                args: vec![file.into()],
                gui: true,
                editor: Some("Notepad".into()),
            },
            _ => through_shell("vi", false, file, line),
        };
    };
    let name = program_name(editor, shell);
    let gui = GUI_EDITORS.contains(&name.as_str());
    let words = words(editor, shell);
    if !shell {
        // Without a shell the words are the program and its arguments.
        let mut parts = words
            .into_iter()
            .filter(|word| !(gui && WAITS.contains(&word.raw)))
            .map(|word| OsString::from(word.value));
        let program = parts.next().unwrap_or_default();
        let mut args: Vec<OsString> = parts.collect();
        args.extend(location(&name, file, line));
        return Launch {
            program,
            args,
            gui,
            editor: Some(name),
        };
    }
    let text = if gui {
        words
            .iter()
            .filter(|word| !WAITS.contains(&word.raw))
            .map(|word| word.raw)
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        editor.to_owned()
    };
    through_shell(&text, gui, file, line)
}

/// `sh -lc '<editor> "$@"' sh <location…>`.
fn through_shell(editor: &str, gui: bool, file: &Path, line: Option<u32>) -> Launch {
    let name = program_name(editor, true);
    let mut args: Vec<OsString> =
        vec!["-lc".into(), format!("{editor} \"$@\"").into(), "sh".into()];
    args.extend(location(&name, file, line));
    Launch {
        program: "sh".into(),
        args,
        gui,
        editor: Some(name),
    }
}

/// Editors that draw their own window instead of using the terminal.
const GUI_EDITORS: &[&str] = &[
    "bbedit",
    "clion",
    "code",
    "code-insiders",
    "codium",
    "cursor",
    "gedit",
    "goland",
    "gvim",
    "idea",
    "kate",
    "mate",
    "mvim",
    "notepad",
    "notepad++",
    "phpstorm",
    "pluma",
    "pycharm",
    "rubymine",
    "rustrover",
    "subl",
    "sublime_text",
    "vscodium",
    "webstorm",
    "windsurf",
    "xed",
    "zed",
    "zeditor",
];

/// Options by which a GUI editor's command waits for the file to be closed.
const WAITS: &[&str] = &["--block", "--nofork", "--wait", "-f", "-w"];

/// Programs that open a file with whatever the system runs it with.
const OPENERS: &[&str] = &[
    "cmd",
    "explorer",
    "gio",
    "gnome-open",
    "kde-open",
    "open",
    "rundll32",
    "start",
    "wslview",
    "xdg-open",
];

/// Arguments naming `file`, at `line` when there is one, in the form the
/// editor `name` reads.
fn location(name: &str, file: &Path, line: Option<u32>) -> Vec<OsString> {
    let Some(line) = line else {
        return vec![file.into()];
    };
    let at = |suffix: String| {
        let mut path = file.as_os_str().to_owned();
        path.push(suffix);
        path
    };
    match name {
        "bbedit" | "emacs" | "emacsclient" | "gedit" | "gvim" | "jed" | "joe" | "kak" | "mg"
        | "micro" | "mvim" | "nano" | "ne" | "nvim" | "pico" | "pluma" | "vi" | "view" | "vim"
        | "xed" => vec![format!("+{line}").into(), file.into()],
        "code" | "code-insiders" | "codium" | "cursor" | "vscodium" | "windsurf" => {
            vec!["-g".into(), at(format!(":{line}"))]
        }
        "helix" | "hx" | "sublime_text" | "subl" | "zed" | "zeditor" => {
            vec![at(format!(":{line}"))]
        }
        "clion" | "goland" | "idea" | "phpstorm" | "pycharm" | "rubymine" | "rustrover"
        | "webstorm" => vec!["--line".into(), line.to_string().into(), file.into()],
        "kate" | "mate" => vec!["-l".into(), line.to_string().into(), file.into()],
        "notepad++" => vec![format!("-n{line}").into(), file.into()],
        // For any other editor the line is left out: a `+42` it does not
        // understand could be opened as a second file.
        _ => vec![file.into()],
    }
}

/// A shell word: its source text (`raw`, quotes included) and its value.
struct Word<'a> {
    raw: &'a str,
    value: String,
}

/// Split `text` into words at unquoted white space, honouring single and
/// double quotes, and backslash escapes when `backslash` is set.
fn words(text: &str, backslash: bool) -> Vec<Word<'_>> {
    let mut words = Vec::new();
    let mut start = None;
    let mut value = String::new();
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if start.is_none() {
            if character.is_whitespace() {
                continue;
            }
            start = Some(index);
        }
        if escaped {
            value.push(character);
            escaped = false;
            continue;
        }
        match (quote, character) {
            (None | Some('"'), '\\') if backslash => escaped = true,
            (None, '"' | '\'') => quote = Some(character),
            (Some(open), _) if open == character => quote = None,
            (None, _) if character.is_whitespace() => {
                if let Some(from) = start.take() {
                    words.push(Word {
                        raw: &text[from..index],
                        value: std::mem::take(&mut value),
                    });
                }
            }
            _ => value.push(character),
        }
    }
    if let Some(from) = start {
        words.push(Word {
            raw: &text[from..],
            value,
        });
    }
    words
}

/// The program an editor command runs, as a lowercase file name with no
/// folder and no `.exe`/`.cmd`/`.bat`: `/usr/local/bin/code --wait` gives
/// `code`. Leading `NAME=value` settings (`TERM=xterm vim`) are skipped.
fn program_name(editor: &str, backslash: bool) -> String {
    let words = words(editor, backslash);
    let program = words
        .iter()
        .map(|word| word.value.as_str())
        .find(|word| {
            word.split_once('=').is_none_or(|(name, _)| {
                name.is_empty()
                    || !name
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '_')
            })
        })
        .unwrap_or_default();
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let lower = base.to_ascii_lowercase();
    [".exe", ".cmd", ".bat"]
        .into_iter()
        .find_map(|extension| lower.strip_suffix(extension))
        .map_or_else(|| lower.clone(), str::to_owned)
}

/// The file `program` (a bare name: no folder, no extension) is, in the
/// folders of `path` (a `PATH` value), trying each of `extensions`. Windows
/// looks up only `.exe` itself, while editors such as VS Code install a
/// `code.cmd`. Relative entries, the empty one included, are skipped: they
/// would point into the current folder, usually the project, so a cloned
/// repository could put its own `code.cmd` there.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn find_program(program: &OsStr, path: &OsStr, extensions: &[&str]) -> Option<PathBuf> {
    let named = Path::new(program);
    if named.components().count() != 1 || named.extension().is_some() {
        return None;
    }
    std::env::split_paths(path)
        .filter(|folder| folder.is_absolute())
        .find_map(|folder| {
            extensions
                .iter()
                .map(|extension| folder.join(named).with_extension(extension))
                .find(|candidate| candidate.is_file())
        })
}

impl crate::state::App {
    /// The folder holding the chat's files, when it is on this computer: the
    /// chat's own folder if it has one, else the server's project.
    pub(crate) fn local_project(&self) -> Option<&Path> {
        if !self.files_local {
            return None;
        }
        self.thread
            .directory
            .as_deref()
            .or_else(|| self.server_info.as_ref()?.workspace.as_deref())
            .map(Path::new)
    }

    /// What a click on `link` (an encoded [`Reference`]) finds. A lookup
    /// that found the file less than [`FRESH`] ago opens it at once. An older
    /// one is checked again, on a thread of its own, with the project folder
    /// the lookup thread already resolved; the click waits for that at most
    /// [`CLICK_CHECK`].
    pub(crate) fn file_to_open(&self, link: &str) -> ClickCheck {
        let (Some(root), Some(reference)) = (self.local_project(), Reference::decode(link)) else {
            return ClickCheck::Gone;
        };
        let (fresh, known_root) = self.reply_files.fresh(root, link);
        let shown = |path: &Path, resolved_root: &Path| {
            path.strip_prefix(resolved_root).map_or_else(
                |_| path.display().to_string(),
                |relative| relative.display().to_string(),
            )
        };
        if let (Some(path), Some(resolved_root)) = (fresh, known_root.as_deref()) {
            return ClickCheck::Open(Opening {
                shown: shown(&path, resolved_root),
                path,
                line: reference.line,
            });
        }
        let root = root.to_owned();
        let written = reference.path.clone();
        #[cfg(test)]
        let delay = self
            .reply_files
            .known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .check_delay;
        let checked = within(CLICK_CHECK, move || {
            #[cfg(test)]
            std::thread::sleep(delay);
            let resolved_root = match known_root {
                Some(resolved_root) => resolved_root,
                None => resolve_root(&root)?,
            };
            let path = project_file(&root, &resolved_root, &reference)?;
            Some(Opening {
                shown: shown(&path, &resolved_root),
                path,
                line: reference.line,
            })
        });
        match checked {
            Some(Some(file)) => ClickCheck::Open(file),
            Some(None) => ClickCheck::Gone,
            None => ClickCheck::Slow(written),
        }
    }

    /// Tell the person about a GUI editor that exited with an error (one
    /// not installed, for example).
    pub(crate) fn report_failed_editors(&mut self) {
        for (editor, file) in self.reply_files.failed_editors() {
            self.notice = self.editor_failed(editor.as_deref(), &file);
        }
    }

    /// The notice for an editor that could not open `file`.
    pub(crate) fn editor_failed(&self, editor: Option<&str>, file: &str) -> String {
        match editor {
            Some(editor) => self.language.format(
                "{editor} could not open {file}",
                &[("editor", editor), ("file", file)],
            ),
            None => self
                .language
                .format("Your text editor could not open {file}", &[("file", file)]),
        }
    }
}
