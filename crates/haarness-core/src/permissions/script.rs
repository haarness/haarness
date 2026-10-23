//! The script a shell command runs, read for the person answering its
//! permission request: approving `bash /tmp/x.sh` should not mean approving
//! a file nobody could see. Display only: nothing here adds a permission,
//! changes a pattern or decides a request.

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use super::bash::{ScriptFile, Shebang};
use haarness_protocol::{PermissionScript, ScriptWithheld};
use sha2::{Digest as _, Sha256};

/// The most of a script a request carries; a longer one is cut here and
/// marked [`PermissionScript::truncated`].
pub const MAX_SCRIPT_BYTES: usize = 30_000;

/// How long reading a script for a prompt may take: a hung network or FUSE
/// mount gives no script rather than a stalled prompt.
pub const READ_TIMEOUT: Duration = Duration::from_millis(300);

/// How long reading it again before the command runs may take: longer, as
/// a slow disk would otherwise refuse a command the person approved.
pub const RECHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// How many of a command's script files are tried before giving up: a
/// program run by its path may be a binary (`/usr/bin/git status && bash
/// x.sh`), and the next one is still worth showing.
const MAX_TRIED: usize = 4;

/// Files that hold credentials or export them (keys, tokens, shell
/// profiles), by name: never shown, whatever the rules allow, since the
/// request travels to every client that shows it.
const CREDENTIAL_NAMES: &[&str] = &[
    ".netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    ".envrc",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
];
/// Private key files, by the start of their name (`id_rsa`, `id_ed25519.old`).
const KEY_PREFIXES: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];
/// Key and certificate stores, by extension.
const KEY_EXTENSIONS: &[&str] = &["pem", "key", "p12", "pfx"];

/// A script read for a prompt, and what it was read from.
#[derive(Debug, Clone)]
pub struct Shown {
    pub script: PermissionScript,
    /// Checked again when the person answers: see [`Seal::holds`]. None
    /// for a script the command writes before it runs it.
    pub seal: Option<Seal>,
}

/// What a shown script was read from. The person may answer minutes later,
/// and approves what the card showed, not what the file says by then.
#[derive(Debug, Clone)]
pub struct Seal {
    /// The file as the command names it.
    file: PathBuf,
    /// Where it led.
    real: PathBuf,
    /// The bytes read, up to one past the cap, hashed.
    digest: [u8; 32],
}

impl Seal {
    /// Whether the command still runs what the card showed: the file it
    /// names leads to the same place and reads the same, read again off the
    /// async workers within [`RECHECK_TIMEOUT`]. A file gone, changed, moved
    /// behind its link or too slow to read again does not hold.
    pub async fn holds(&self) -> bool {
        let seal = self.clone();
        let check = tokio::task::spawn_blocking(move || {
            seal.file.canonicalize().is_ok_and(|real| real == seal.real)
                && read_bytes(&seal.real).is_some_and(|bytes| digest(&bytes) == seal.digest)
        });
        matches!(
            tokio::time::timeout(RECHECK_TIMEOUT, check).await,
            Ok(Ok(true))
        )
    }
}

/// [`find`] off the async workers, within [`READ_TIMEOUT`]: nothing when the
/// disk is too slow, so a stalled mount never holds up a prompt.
pub async fn for_prompt(
    command: String,
    cwd: PathBuf,
    readable: impl Fn(&Path) -> bool + Send + 'static,
) -> Option<Shown> {
    tracing::debug!("reading the script a command runs for its permission prompt");
    let read = tokio::task::spawn_blocking(move || find(&command, &cwd, readable));
    tokio::time::timeout(READ_TIMEOUT, read).await.ok()?.ok()?
}

/// The first script `command` runs from `cwd` that reads as text (see
/// [`super::bash::script_files`] for what counts) and that `readable` lets
/// this chat read. Nothing for a secret-named file (the built-in `read`
/// rules), a credential file ([`is_credential`]), a binary, a folder, a
/// device, a missing file, one a link leads into `/proc` or `/dev`, or one
/// run by its path whose `#!` line runs another file
/// ([`runs_another_file`]).
#[must_use]
pub fn find(command: &str, cwd: &Path, readable: impl Fn(&Path) -> bool) -> Option<Shown> {
    super::bash::script_files(command, cwd)
        .iter()
        .take(MAX_TRIED)
        .find_map(|file| match file.withheld {
            Some(reason) => Some(withheld(&file.path, reason)),
            None => read(file, &readable),
        })
}

/// A script whose text may not be what runs (the command writes it first,
/// or an earlier step may change it): nothing of it is read or shown, only
/// its path, and there is nothing to check again.
fn withheld(file: &Path, reason: ScriptWithheld) -> Shown {
    Shown {
        script: PermissionScript {
            path: shown_path(&named(file)),
            content: String::new(),
            truncated: false,
            withheld: Some(reason),
        },
        seal: None,
    }
}

/// Whether `path` names a file that holds or exports credentials: a private
/// key (`id_rsa*`, `*.pem`, `*.key`, `*.p12`), `.aws/credentials`,
/// `.netrc`, `.git-credentials`, `.npmrc`, `.pypirc`, `.envrc`, or a shell's
/// rc or profile file.
#[must_use]
pub fn is_credential(path: &Path) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    let name = name.to_string_lossy().to_ascii_lowercase();
    let extension = Path::new(&name)
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned());
    CREDENTIAL_NAMES.contains(&name.as_str())
        || KEY_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        || extension.is_some_and(|extension| KEY_EXTENSIONS.contains(&extension.as_str()))
        || (name == "credentials"
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|folder| folder.eq_ignore_ascii_case(".aws")))
}

/// A script (its path absolute, as the command names it) read where it
/// really is, links followed. Its own command's outputs may lead to it
/// through a link: it is written before it runs then.
fn read(script: &ScriptFile, readable: &impl Fn(&Path) -> bool) -> Option<Shown> {
    let file = script.path.as_path();
    // There the server's process answers, not the shell's.
    if through_system_folder(file) {
        return None;
    }
    let shown = named(file);
    let real = file.canonicalize().ok()?;
    // Swapped for a link there since the walk above.
    if super::bash::in_system_folder(&real) {
        return None;
    }
    if [&shown, &real]
        .iter()
        .any(|path| super::is_secret_path(path) || is_credential(path) || !readable(path))
    {
        return None;
    }
    if script
        .outputs
        .iter()
        .any(|output| through_system_folder(output))
    {
        return Some(withheld(file, ScriptWithheld::Changeable));
    }
    if script.outputs.iter().any(|output| same_file(output, &real)) {
        return Some(withheld(file, ScriptWithheld::Written));
    }
    let mut bytes = read_bytes(&real)?;
    let another = match script.shebang {
        Shebang::Comment => false,
        Shebang::Followed => runs_another_file(&bytes),
        Shebang::Perl => perl_runs_another_program(&bytes),
    };
    if another || declares_another_encoding(&bytes) {
        return None;
    }
    let seal = Seal {
        file: file.to_path_buf(),
        real,
        digest: digest(&bytes),
    };
    let truncated = bytes.len() > MAX_SCRIPT_BYTES;
    bytes.truncate(MAX_SCRIPT_BYTES);
    if bytes.contains(&0) {
        return None;
    }
    if truncated {
        // A character the cut split in two is left out whole.
        bytes.truncate(complete_utf8(&bytes));
    }
    let content = displayed(&String::from_utf8_lossy(&bytes));
    // An empty file has nothing to read on the card.
    if content.trim_end_matches('\n').is_empty() {
        return None;
    }
    Some(Shown {
        script: PermissionScript {
            path: shown_path(&shown),
            content,
            truncated,
            withheld: None,
        },
        seal: Some(seal),
    })
}

/// The file a command names, as the card names it: with its `..` taken
/// where the folder before it really is, as the system takes it when it
/// opens the file (`lnk/../run.sh` is `run.sh` beside the folder `lnk`
/// leads to), and its own name as written.
fn named(file: &Path) -> PathBuf {
    let lexical = super::normalize_absolute(file);
    if !file.components().any(|part| part == Component::ParentDir) {
        return lexical;
    }
    file.parent()
        .and_then(|folder| folder.canonicalize().ok())
        .zip(file.file_name())
        .map_or(lexical, |(folder, name)| folder.join(name))
}

/// The `#!` line of `bytes`, without the `#!`, if it has one.
fn shebang_line(bytes: &[u8]) -> Option<std::borrow::Cow<'_, str>> {
    let line = bytes.strip_prefix(b"#!")?;
    let line = line.split(|&byte| byte == b'\n').next().unwrap_or_default();
    Some(String::from_utf8_lossy(line))
}

/// Whether `perl` runs another program instead of the script: one its
/// first line names with `#!` without saying `perl` or `indir`, found as
/// perl finds it (after a UTF-8 byte order mark, any ASCII white space but
/// a line feed, and one `:`), the line ending at a line feed, form feed,
/// vertical tab or carriage return. Real Perl scripts say `perl`, so the
/// program is not judged.
fn perl_runs_another_program(bytes: &[u8]) -> bool {
    let start = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let at = start
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\x0c' | b'\x0b'))
        .unwrap_or(start.len());
    let mut start = &start[at..];
    if start.starts_with(b":") && !start.starts_with(b"::") {
        start = &start[1..];
    }
    let Some(line) = start.strip_prefix(b"#!") else {
        return false;
    };
    let line = line
        .split(|byte| matches!(byte, b'\n' | b'\x0c' | b'\x0b' | b'\r'))
        .next()
        .unwrap_or_default();
    let line = String::from_utf8_lossy(line);
    !line.contains("perl") && !line.contains("indir")
}

/// Whether a script's `#!` line, followed, runs another file too: an
/// interpreter that is not a rooted path, is under `/proc` or `/dev`, or is
/// itself a script (Linux follows its `#!` line in turn), or an argument
/// that is not an option or has a `/` (`#!/bin/sh /x/evil.sh`,
/// `#!/bin/sh evil.sh`, `#!/usr/bin/perl -I/x`), the script's own path
/// coming after them. `#!/usr/bin/env NAME` looks the program up by name.
/// Under `#!/usr/bin/env -S …`, a known shell's or interpreter's arguments
/// are read the same way (`-S sh /x/evil.sh`); another program may have a
/// subcommand and options, a path only as an option's value (`-S deno run
/// --allow-read=/tmp`, not `-S awk -f evil.awk`).
fn runs_another_file(bytes: &[u8]) -> bool {
    let Some(line) = shebang_line(bytes) else {
        return false;
    };
    let mut words = line.split([' ', '\t']).filter(|word| !word.is_empty());
    let Some(interpreter) = words.next() else {
        return false;
    };
    let mut arguments: Vec<&str> = words.collect();
    if !interpreter.starts_with('/')
        || super::bash::in_system_folder(Path::new(interpreter))
        || is_script(Path::new(interpreter))
    {
        return true;
    }
    if Path::new(interpreter).file_name() == Some("env".as_ref()) {
        let split = arguments.first().and_then(|first| {
            first
                .strip_prefix("--split-string")
                .map(|rest| rest.trim_start_matches('='))
                .or_else(|| first.strip_prefix("-S"))
        });
        match (split, arguments.first()) {
            (Some(attached), _) => {
                // The words `-S` splits: assignments, the program, its
                // arguments. Quotes, escapes and `${NAME}`, which it reads
                // its own way, and options before the program are not read.
                let words: Vec<&str> = std::iter::once(attached)
                    .filter(|word| !word.is_empty())
                    .chain(arguments[1..].iter().copied())
                    .collect();
                if words
                    .iter()
                    .any(|word| word.contains(['\'', '"', '\\', '$']))
                {
                    return true;
                }
                let Some(at) = words.iter().position(|word| !word.contains('=')) else {
                    return false;
                };
                let (program, rest) = (words[at], &words[at + 1..]);
                if program.starts_with('-') || program.contains('/') {
                    return true;
                }
                if super::bash::interpreter(&super::bash::program_name(program)).is_none() {
                    // Another program's own arguments: a subcommand first
                    // (`deno run`, `uv run`), then options, a path only as
                    // an option's value (`--allow-read=/tmp`).
                    return rest.iter().enumerate().any(|(index, word)| {
                        index > 0 && !word.starts_with('-')
                            || word
                                .split_once('=')
                                .map_or(*word, |(name, _)| name)
                                .contains('/')
                    });
                }
                arguments = rest.to_vec();
            }
            // The program it looks up by name.
            (None, Some(name)) if !name.starts_with('-') && !name.contains('/') => {
                arguments.remove(0);
            }
            _ => {}
        }
    }
    arguments
        .iter()
        .any(|argument| !argument.starts_with('-') || argument.contains('/'))
}

/// Whether the interpreter a `#!` line names is a script itself (its own
/// first bytes `#!`), or cannot be told. A missing one runs nothing.
fn is_script(interpreter: &Path) -> bool {
    let Ok(real) = interpreter.canonicalize() else {
        return false;
    };
    if super::bash::in_system_folder(&real) {
        return true;
    }
    if !std::fs::metadata(&real).is_ok_and(|meta| meta.is_file()) {
        return false;
    }
    let mut start = [0u8; 2];
    match open(&real).and_then(|mut file| file.read_exact(&mut start)) {
        Ok(()) => &start == b"#!",
        Err(error) => error.kind() != std::io::ErrorKind::UnexpectedEof,
    }
}

/// Whether one of the file's first two lines declares a source encoding
/// (PEP 263, `# -*- coding: … -*-`) that may turn bytes into other text:
/// Python decodes the file by it, so it may run lines the card cannot show
/// (`# coding: utf-7` makes `+AAo-` a line break). UTF-8, ASCII and the
/// ASCII-compatible single-byte encodings ([`keeps_lines`]) cannot.
fn declares_another_encoding(bytes: &[u8]) -> bool {
    let mut rest = bytes;
    for _ in 0..2 {
        let end = rest
            .iter()
            .position(|byte| matches!(byte, b'\n' | b'\r'))
            .unwrap_or(rest.len());
        let line = String::from_utf8_lossy(&rest[..end]);
        if coding(&line).is_some_and(|encoding| !keeps_lines(encoding)) {
            return true;
        }
        rest = &rest[end..];
        rest = rest
            .strip_prefix(b"\r\n")
            .or_else(|| rest.get(1..))
            .unwrap_or_default();
    }
    false
}

/// Whether an encoding, by Python's names for it, reads ASCII as ASCII and
/// every other byte as one character: UTF-8 (`utf-8-…` too), ASCII
/// (`us-ascii`), Latin-1 (`latin1`, `iso8859-1`, `iso-latin-1`, `l1`), and
/// the other ISO 8859 and Windows code pages (`iso-8859-15`, `cp1250`,
/// `windows-1252`, `latin-2`). Not UTF-7, UTF-16 or UTF-32, the escape
/// codecs or the multi-byte ones (`shift_jis`).
fn keeps_lines(encoding: &str) -> bool {
    let name = encoding.to_ascii_lowercase().replace('_', "-");
    let number = |text: Option<&str>, low: u32, high: u32| {
        text.and_then(|text| text.parse::<u32>().ok())
            .is_some_and(|number| (low..=high).contains(&number))
    };
    matches!(
        name.as_str(),
        "utf-8"
            | "utf8"
            | "ascii"
            | "us-ascii"
            | "latin-1"
            | "latin1"
            | "iso-8859-1"
            | "iso8859-1"
            | "iso-latin-1"
            | "l1"
    ) || ["utf-8-", "latin-1-", "iso-8859-1-", "iso-latin-1-"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || number(
            name.strip_prefix("iso-8859-")
                .or_else(|| name.strip_prefix("iso8859-"))
                .filter(|number| *number != "12"),
            1,
            16,
        )
        || number(
            name.strip_prefix("cp")
                .or_else(|| name.strip_prefix("windows-")),
            1250,
            1258,
        )
        || number(
            name.strip_prefix("latin-")
                .or_else(|| name.strip_prefix("latin")),
            1,
            10,
        )
}

/// The encoding a line declares: `^[ \t\f]*#.*?coding[:=][ \t]*([-\w.]+)`.
fn coding(line: &str) -> Option<&str> {
    let mut rest = line
        .trim_start_matches([' ', '\t', '\x0c'])
        .strip_prefix('#')?;
    loop {
        rest = &rest[rest.find("coding")? + "coding".len()..];
        let Some(value) = rest.strip_prefix([':', '=']) else {
            continue;
        };
        let value = value.trim_start_matches([' ', '\t']);
        let end = value
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .unwrap_or(value.len());
        if end > 0 {
            return Some(&value[..end]);
        }
    }
}

/// Whether `path` (absolute) leads, as written or through links, into
/// `/proc` or `/dev`, where the server's process answers instead of the
/// shell's (`/proc/self/cwd`, `/dev/fd/3`).
fn through_system_folder(path: &Path) -> bool {
    let mut resolved = PathBuf::from("/");
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let push = |rest: &mut Vec<std::ffi::OsString>, path: &Path| {
        for part in path.components().rev() {
            match part {
                Component::Normal(name) => rest.push(name.to_owned()),
                Component::ParentDir => rest.push("..".into()),
                _ => {}
            }
        }
    };
    push(&mut rest, path);
    let mut links = 0;
    while let Some(part) = rest.pop() {
        if part == ".." {
            resolved.pop();
            continue;
        }
        let next = resolved.join(&part);
        if super::bash::in_system_folder(&next) {
            return true;
        }
        let is_link =
            std::fs::symlink_metadata(&next).is_ok_and(|meta| meta.file_type().is_symlink());
        // Past the system's own limit the file cannot be opened anyway.
        if !is_link || links >= 40 {
            resolved = next;
            continue;
        }
        links += 1;
        let Ok(target) = std::fs::read_link(&next) else {
            return false;
        };
        if target.has_root() {
            resolved = PathBuf::from("/");
        }
        push(&mut rest, &target);
    }
    false
}

/// Whether `path` leads to the file at `real`, through links of either
/// kind.
fn same_file(path: &Path, real: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let (Ok(one), Ok(other)) = (std::fs::metadata(path), std::fs::metadata(real)) else {
            return false;
        };
        one.dev() == other.dev() && one.ino() == other.ino()
    }
    #[cfg(not(unix))]
    path.canonicalize().is_ok_and(|path| path == real)
}

/// A path as the card shows it: [`displayed`], with its tabs and line
/// breaks as `�` too.
fn shown_path(path: &Path) -> String {
    displayed(&path.to_string_lossy()).replace(['\n', '\t'], "\u{FFFD}")
}

/// The bytes of the regular file at `real` (a resolved path), up to one
/// past the cap. A pipe or device is never opened, and the open file is
/// checked again in case one was put there since.
fn read_bytes(real: &Path) -> Option<Vec<u8>> {
    #[cfg(all(test, not(windows)))]
    tests::wait_if_slow(real);
    if !std::fs::metadata(real).ok()?.is_file() {
        return None;
    }
    let file = open(real).ok()?;
    if !file.metadata().ok()?.is_file() || !opened_at(&file, real) {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_SCRIPT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// The resolved path opened without following a link: a link put there
/// after it was resolved cannot lead past the checks above. macOS (11 and
/// later) refuses a link at any step; elsewhere the last step is refused
/// and [`opened_at`] catches a folder swapped for a link. A pipe never
/// blocks the open, and a terminal never becomes the server's.
#[cfg(unix)]
fn open(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    #[cfg(target_vendor = "apple")]
    let links = nix::libc::O_NOFOLLOW_ANY;
    #[cfg(not(target_vendor = "apple"))]
    let links = nix::libc::O_NOFOLLOW;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY | links)
        .open(path)
}

#[cfg(not(unix))]
fn open(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// Whether the open file is the one at `real`: on Linux its own path, as
/// the system names it, must be `real`, so a folder swapped for a link
/// between resolving and opening is refused. Elsewhere [`open`] already
/// refuses that (macOS), or nothing more can be told.
#[cfg(target_os = "linux")]
fn opened_at(file: &std::fs::File, real: &Path) -> bool {
    use std::os::fd::AsRawFd as _;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .ok()
        .is_none_or(|opened| opened == real)
}

#[cfg(not(target_os = "linux"))]
fn opened_at(_file: &std::fs::File, _real: &Path) -> bool {
    true
}

/// How many bytes of `bytes` end on a whole UTF-8 character: a sequence
/// that the end cuts short is dropped.
fn complete_utf8(bytes: &[u8]) -> usize {
    let start = bytes
        .iter()
        .rposition(|byte| byte & 0xC0 != 0x80)
        .filter(|start| bytes.len() - start <= 4);
    let Some(start) = start else {
        return bytes.len();
    };
    let wanted = match bytes[start] {
        byte if byte & 0x80 == 0 => 1,
        byte if byte & 0xE0 == 0xC0 => 2,
        byte if byte & 0xF0 == 0xE0 => 3,
        byte if byte & 0xF8 == 0xF0 => 4,
        _ => return bytes.len(),
    };
    if bytes.len() - start < wanted {
        start
    } else {
        bytes.len()
    }
}

/// The text as a person may safely read it anywhere: line breaks as `\n`,
/// and the characters a terminal would take as commands (control characters
/// other than tab and line feed), that reorder what is shown (bidirectional
/// controls and marks) or that break a line where nothing shows it (the
/// Unicode line and paragraph separators) as `�`, so a script cannot hide a
/// line. The path gets the same, with its tabs and line breaks too.
fn displayed(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    text.chars()
        .map(|character| match character {
            '\r' => '\n',
            '\t' | '\n' => character,
            '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{061C}'
            | '\u{2028}'
            | '\u{2029}' => '\u{FFFD}',
            character if character.is_control() => '\u{FFFD}',
            character => character,
        })
        .collect()
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    /// Files a test makes slow to read, as a hung network mount is.
    static SLOW: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

    pub(super) fn wait_if_slow(path: &Path) {
        if SLOW.lock().unwrap().iter().any(|slow| slow == path) {
            std::thread::sleep(RECHECK_TIMEOUT + Duration::from_millis(500));
        }
    }

    fn bash_script(
        command: &str,
        cwd: &Path,
        readable: impl Fn(&Path) -> bool,
    ) -> Option<PermissionScript> {
        find(command, cwd, readable).map(|shown| shown.script)
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("haarness-script-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn write(&self, name: &str, contents: impl AsRef<[u8]>) -> PathBuf {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, contents).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_the_script_a_command_runs_from_its_folder() {
        let fixture = Fixture::new();
        fixture.write("x.sh", "echo hi\n");
        let script = bash_script("bash x.sh", &fixture.0, |_| true).unwrap();
        assert_eq!(Path::new(&script.path), fixture.0.join("x.sh"));
        assert_eq!(script.content, "echo hi\n");
        assert!(!script.truncated);
        // A `cd` earlier in the line moves where it is looked for.
        fixture.write("sub/y.sh", "echo sub\n");
        let script = bash_script("cd sub && ./y.sh", &fixture.0, |_| true).unwrap();
        assert_eq!(Path::new(&script.path), fixture.0.join("sub").join("y.sh"));
        assert_eq!(script.content, "echo sub\n");
        assert!(bash_script("echo hi", &fixture.0, |_| true).is_none());
    }

    #[test]
    fn cuts_a_long_script_and_says_so() {
        let fixture = Fixture::new();
        fixture.write("big.sh", "a".repeat(MAX_SCRIPT_BYTES + 10));
        let script = bash_script("sh big.sh", &fixture.0, |_| true).unwrap();
        assert_eq!(script.content.len(), MAX_SCRIPT_BYTES);
        assert!(script.truncated);
        // Exactly at the cap is whole.
        fixture.write("full.sh", "b".repeat(MAX_SCRIPT_BYTES));
        assert!(
            !bash_script("sh full.sh", &fixture.0, |_| true)
                .unwrap()
                .truncated
        );
        // A character the cut splits is left out, not shown as `�`.
        fixture.write(
            "wide.sh",
            format!("{}é and more", "c".repeat(MAX_SCRIPT_BYTES - 1)),
        );
        let script = bash_script("sh wide.sh", &fixture.0, |_| true).unwrap();
        assert_eq!(script.content, "c".repeat(MAX_SCRIPT_BYTES - 1));
        assert!(script.truncated);
    }

    #[test]
    fn shows_nothing_for_a_missing_or_empty_file_a_folder_a_binary_or_a_secret() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.0.join("folder")).unwrap();
        fixture.write("binary", [0x7f, b'E', 0, 1, 2]);
        fixture.write("empty.sh", "");
        fixture.write("blank.sh", "\n\n");
        fixture.write("prod.env", "TOKEN=1\n");
        fixture.write(".env", "TOKEN=1\n");
        for command in [
            "bash missing.sh",
            "bash folder",
            "./binary",
            "sh empty.sh",
            "sh blank.sh",
            "source ./prod.env",
            ". ./.env",
        ] {
            assert!(
                bash_script(command, &fixture.0, |_| true).is_none(),
                "{command}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn follows_links_but_never_into_a_secret() {
        let fixture = Fixture::new();
        let target = fixture.write("real/run.sh", "echo real\n");
        std::os::unix::fs::symlink(&target, fixture.0.join("run.sh")).unwrap();
        let script = bash_script("./run.sh", &fixture.0, |_| true).unwrap();
        // Named as the command names it, read where it leads.
        assert_eq!(Path::new(&script.path), fixture.0.join("run.sh"));
        assert_eq!(script.content, "echo real\n");
        let secret = fixture.write("prod.env", "TOKEN=1\n");
        std::os::unix::fs::symlink(&secret, fixture.0.join("link.sh")).unwrap();
        assert!(bash_script("bash link.sh", &fixture.0, |_| true).is_none());
        // A pipe is not read, and never blocks.
        let pipe = std::process::Command::new("mkfifo")
            .arg(fixture.0.join("pipe.sh"))
            .status();
        if pipe.is_ok_and(|status| status.success()) {
            assert!(bash_script("bash pipe.sh", &fixture.0, |_| true).is_none());
        }
    }

    /// `cd lnk/../realdir`: the shell takes the `..` off the folder's name
    /// and runs the project's `realdir/run.sh`, while the file read through
    /// the link is the other folder's. Nothing is shown.
    #[cfg(unix)]
    #[test]
    fn a_cd_through_a_linked_folder_and_dot_dot_shows_nothing() {
        let fixture = Fixture::new();
        let project = fixture.0.join("project");
        fixture.write("project/realdir/run.sh", "echo project\n");
        fixture.write("elsewhere/realdir/run.sh", "echo elsewhere\n");
        std::fs::create_dir_all(fixture.0.join("elsewhere/inner")).unwrap();
        std::os::unix::fs::symlink(fixture.0.join("elsewhere/inner"), project.join("lnk")).unwrap();
        let command = "cd lnk/../realdir && sh run.sh";
        let ran = std::process::Command::new("sh")
            .args(["-c", command])
            .current_dir(&project)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&ran.stdout), "project\n");
        assert!(bash_script(command, &project, |_| true).is_none());
        let script = bash_script("cd realdir && sh run.sh", &project, |_| true).unwrap();
        assert_eq!(script.content, "echo project\n");
    }

    /// `bash lnk/../run.sh` runs `run.sh` beside the folder `lnk` leads to,
    /// as the system takes a path's `..`: the card names that file.
    #[cfg(unix)]
    #[test]
    fn a_script_path_through_a_link_and_dot_dot_is_named_where_it_leads() {
        let fixture = Fixture::new();
        let project = fixture.0.join("project");
        fixture.write("project/run.sh", "echo project\n");
        let elsewhere = fixture.write("elsewhere/run.sh", "echo elsewhere\n");
        std::fs::create_dir_all(fixture.0.join("elsewhere/inner")).unwrap();
        std::os::unix::fs::symlink(fixture.0.join("elsewhere/inner"), project.join("lnk")).unwrap();
        for command in ["sh lnk/../run.sh", "cd lnk && sh ../run.sh"] {
            let ran = std::process::Command::new("sh")
                .args(["-c", command])
                .current_dir(&project)
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&ran.stdout), "elsewhere\n");
            let script = bash_script(command, &project, |_| true).unwrap();
            assert_eq!(Path::new(&script.path), elsewhere, "{command}");
            assert_eq!(script.content, "echo elsewhere\n", "{command}");
        }
        // Withheld, it is named there too.
        let script = bash_script("make; sh lnk/../run.sh", &project, |_| true).unwrap();
        assert_eq!(Path::new(&script.path), elsewhere);
        assert!(script.withheld.is_some());
        // Without a link, `..` is what it always was.
        let script = bash_script("sh ../project/run.sh", &project, |_| true).unwrap();
        assert_eq!(Path::new(&script.path), project.join("run.sh"));
    }

    /// `./run.sh > log`, where `log` leads to `run.sh`: the shell empties the
    /// script before it runs it, so its text is not shown.
    #[cfg(unix)]
    #[test]
    fn output_sent_to_the_script_through_a_link_withholds_it() {
        let fixture = Fixture::new();
        let script = fixture.write("run.sh", "echo old\n");
        std::os::unix::fs::symlink(&script, fixture.0.join("log")).unwrap();
        std::fs::hard_link(&script, fixture.0.join("hard.log")).unwrap();
        for command in ["./run.sh > log", "sh run.sh 2>> hard.log"] {
            let shown = bash_script(command, &fixture.0, |_| true).unwrap();
            assert_eq!(shown.withheld, Some(ScriptWithheld::Written), "{command}");
            assert_eq!(shown.content, "", "{command}");
        }
        // Output to another file leaves it shown.
        fixture.write("other.log", "");
        let shown = bash_script("sh run.sh > other.log 2>&1", &fixture.0, |_| true).unwrap();
        assert_eq!(shown.content, "echo old\n");
    }

    /// `#!/bin/sh /x/evil.sh` runs `evil.sh` with the script after it: a
    /// script run by its path whose `#!` line names another file is not
    /// shown. Run by an interpreter, its `#!` line is a comment.
    #[cfg(unix)]
    #[test]
    fn a_shebang_that_runs_another_file_shows_nothing() {
        use std::os::unix::fs::PermissionsExt as _;
        let fixture = Fixture::new();
        let evil = fixture.write("evil.sh", "echo evil\n");
        let write = |first: &str| {
            let script = fixture.write("run.sh", format!("{first}\necho body\n"));
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        write(&format!("#!/bin/sh {}", evil.display()));
        let ran = std::process::Command::new("./run.sh")
            .current_dir(&fixture.0)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&ran.stdout), "evil\n");
        for first in [
            format!("#!/bin/sh {}", evil.display()),
            "#!/bin/sh evil.sh".into(),
            "#!/usr/bin/perl -I/opt/lib".into(),
            "#!/usr/bin/env /opt/evil".into(),
            "#!/usr/bin/env node /opt/evil.js".into(),
            "#!evil".into(),
        ] {
            write(&first);
            for command in ["./run.sh", "sudo ./run.sh"] {
                assert!(
                    bash_script(command, &fixture.0, |_| true).is_none(),
                    "{first}"
                );
            }
            // Most interpreters read that line as a comment, also run by
            // their own path.
            for command in ["sh run.sh", "bash run.sh", "/bin/bash run.sh"] {
                let shown = bash_script(command, &fixture.0, |_| true).unwrap();
                assert!(shown.content.ends_with("echo body\n"), "{first}: {command}");
            }
        }
        for first in [
            "#!/bin/sh",
            "#! /bin/bash -eu",
            "#!/usr/bin/env bash",
            "#!/usr/bin/env -S bash -e",
            "#!/usr/bin/env -S deno run --allow-read=/tmp",
            "#!/usr/bin/env -S uv run --script",
            "#!/usr/bin/env -S NODE_ENV=production node",
            "#!/usr/bin/awk -f",
            "echo no shebang",
        ] {
            write(first);
            let shown = bash_script("./run.sh", &fixture.0, |_| true);
            assert!(
                shown.is_some_and(|shown| shown.content.ends_with("echo body\n")),
                "{first}"
            );
        }
        // Under `env -S`, a known shell's or interpreter's arguments count;
        // another program may have a subcommand and options, a path only
        // as an option's value. What `-S` reads its own way (quotes,
        // escapes, `$`) and options before the program are not read.
        for first in [
            "#!/usr/bin/env -S sh /opt/evil.sh",
            "#!/usr/bin/env -S node /opt/evil.js",
            "#!/usr/bin/env -S /opt/evil",
            "#!/usr/bin/env -Ssh evil.sh",
            "#!/usr/bin/env -S awk -f evil.awk",
            "#!/usr/bin/env -S deno run /opt/evil.ts",
            "#!/usr/bin/env -S deno -I/opt run",
            "#!/usr/bin/env -S -i bash",
            "#!/usr/bin/env -S X=1 -C /opt bash",
            "#!/usr/bin/env -S 'sh' evil.sh",
            "#!/usr/bin/env -S bash \"$X\"",
            "#!/usr/bin/env -S sh\\ evil.sh",
            "#!/usr/bin/env -S bash ${X}",
        ] {
            write(first);
            assert!(
                bash_script("./run.sh", &fixture.0, |_| true).is_none(),
                "{first}"
            );
        }
        // An interpreter that is a script itself is followed in turn.
        let nested = fixture.write("interpreter", "#!/bin/sh\nexec sh \"$@\"\n");
        write(&format!("#!{}", nested.display()));
        assert!(bash_script("./run.sh", &fixture.0, |_| true).is_none());
    }

    /// `perl` runs the program a `#!` line names instead of the script,
    /// unless the line says `perl`: `perl x.pl` with `#!/bin/sh evil.sh`
    /// runs `evil.sh`.
    #[cfg(unix)]
    #[test]
    fn perl_follows_a_shebang_that_does_not_say_perl() {
        let fixture = Fixture::new();
        fixture.write("evil.sh", "echo evil\n");
        fixture.write("x.pl", "#!/bin/sh evil.sh\nprint \"body\\n\";\n");
        let perl = std::process::Command::new("perl")
            .arg("x.pl")
            .current_dir(&fixture.0)
            .output();
        if let Ok(ran) = perl {
            assert_eq!(String::from_utf8_lossy(&ran.stdout), "evil\n");
        }
        for command in ["perl x.pl", "/usr/bin/perl -w x.pl"] {
            assert!(
                bash_script(command, &fixture.0, |_| true).is_none(),
                "{command}"
            );
        }
        // Found as perl finds it, a `#!` line that does not say `perl` shows
        // nothing, whatever program it names.
        for first in [
            "#!/bin/sh",
            "#!/bin/sh -c",
            "#!/opt/homebrew/bin/python3 -m",
            " #!/bin/sh evil.sh",
            "\t#!/bin/sh evil.sh",
            "\r#!/bin/sh evil.sh",
            ":#!/bin/sh evil.sh",
            "\u{FEFF}#!/bin/sh evil.sh",
            "#!/bin/sh evil.sh\x0cperl",
            "#!/bin/sh evil.sh\x0bperl",
            "#!/bin/sh evil.sh\rperl",
            "\x0c#!/bin/sh evil.sh",
            "\x0b#!/bin/sh evil.sh",
            " \x0c#!/bin/sh evil.sh",
            "\r\x0c\t#!/bin/sh evil.sh",
        ] {
            fixture.write("x.pl", format!("{first}\nprint \"body\\n\";\n"));
            assert!(
                bash_script("perl x.pl", &fixture.0, |_| true).is_none(),
                "{first:?}"
            );
        }
        // Saying `perl`, the line is perl's own; a shell ignores it.
        for (first, command) in [
            ("#!/usr/bin/perl -w", "perl x.pl"),
            ("#!/usr/bin/env perl", "perl x.pl"),
            ("#!/bin/sh evil.sh perl", "perl x.pl"),
            ("::#!/bin/sh evil.sh", "perl x.pl"),
            (": #!/bin/sh evil.sh", "perl x.pl"),
            ("use strict;", "perl x.pl"),
            ("#!/bin/sh evil.sh", "sh x.pl"),
            ("#!/bin/sh evil.sh", "python3 x.pl"),
        ] {
            fixture.write("x.pl", format!("{first}\nprint \"body\\n\";\n"));
            let shown = bash_script(command, &fixture.0, |_| true);
            assert!(
                shown.is_some_and(|shown| shown.content.starts_with(first)),
                "{first}: {command}"
            );
        }
    }

    /// Python decodes a file by the encoding its first two lines declare
    /// (PEP 263): under `utf-7`, `+AAo-` is a line break the card would not
    /// show, so such a file shows nothing.
    #[test]
    fn a_declared_encoding_other_than_utf8_or_latin1_shows_nothing() {
        let fixture = Fixture::new();
        for start in [
            "# coding: utf-7\n",
            "#!/usr/bin/env python3\n# -*- coding: utf_7 -*-\n",
            "# vim: set fileencoding=unicode_escape :\n",
            "\x0c# coding=UTF-16\n",
            "\r\n# coding: shift_jis\r\n",
            "# coding: raw_unicode_escape\n",
            "# coding: utf-32\n",
            "# coding: euc-jp\n",
            "# coding: cp932\n",
            "# coding: utf8-weird\n",
        ] {
            fixture.write("x.py", format!("{start}print(1) +AAo-print(2)\n"));
            assert!(
                bash_script("python3 x.py", &fixture.0, |_| true).is_none(),
                "{start:?}"
            );
        }
        // Past the first kilobyte, still on the first two lines.
        let long = format!("#{}\n# coding: utf-7\n", "x".repeat(4000));
        let late = format!("# {} coding: utf-7\n", " ".repeat(4000));
        for start in [long, late] {
            fixture.write("x.py", format!("{start}print(1)\n"));
            assert!(bash_script("python3 x.py", &fixture.0, |_| true).is_none());
        }
        for start in [
            "# -*- coding: utf-8 -*-\n",
            "# -*- coding: utf-8-sig -*-\n",
            "# coding: latin1\n",
            "# coding: iso8859-1\n",
            "# coding: iso-latin-1\n",
            "# coding: l1\n",
            "# coding: us-ascii\n",
            "# coding: iso-8859-15\n",
            "# coding: iso8859_2\n",
            "# coding: cp1250\n",
            "# coding: windows-1252\n",
            "# coding: latin-2\n",
            "# coding: Latin9\n",
            "# coding: UTF8\n",
            "# coding: latin-1\n",
            "# coding=iso-8859-1\n",
            "# coding: cp1252\n",
            "# coding: ascii\n",
            "# no declaration\n",
            // On the third line it declares nothing.
            "#\n#\n# coding: utf-7\n",
            "x = 1  # coding: utf-7\n",
        ] {
            fixture.write("x.py", format!("{start}print(1)\n"));
            assert!(
                bash_script("python3 x.py", &fixture.0, |_| true).is_some(),
                "{start:?}"
            );
        }
    }

    /// A link into `/dev` (or `/proc`) leads where the server's process
    /// answers, not the shell's: the script is not shown, and output sent
    /// through one withholds it.
    #[cfg(unix)]
    #[test]
    fn a_link_into_dev_or_proc_shows_nothing() {
        let fixture = Fixture::new();
        fixture.write("run.sh", "echo body\n");
        std::os::unix::fs::symlink("/dev", fixture.0.join("devices")).unwrap();
        std::os::unix::fs::symlink(fixture.0.join("devices"), fixture.0.join("again")).unwrap();
        std::os::unix::fs::symlink("/dev/null", fixture.0.join("out.log")).unwrap();
        assert!(through_system_folder(&fixture.0.join("again/fd/3")));
        assert!(through_system_folder(Path::new("/proc/self/cwd/run.sh")));
        assert!(!through_system_folder(&fixture.0.join("run.sh")));
        for command in ["sh devices/fd/3", "sh again/fd/3"] {
            assert!(
                bash_script(command, &fixture.0, |_| true).is_none(),
                "{command}"
            );
        }
        let shown = bash_script("sh run.sh > out.log", &fixture.0, |_| true).unwrap();
        assert_eq!(shown.withheld, Some(ScriptWithheld::Changeable));
        assert_eq!(shown.content, "");
    }

    /// A vim modeline names an encoding the way PEP 263 does; a harmless
    /// one keeps a shell script shown.
    #[test]
    fn a_modeline_with_a_single_byte_encoding_keeps_a_shell_script_shown() {
        let fixture = Fixture::new();
        fixture.write(
            "run.sh",
            "#!/bin/sh\n# vim: set fileencoding=latin1 :\necho hi\n",
        );
        let shown = bash_script("sh run.sh", &fixture.0, |_| true).unwrap();
        assert!(shown.content.ends_with("echo hi\n"));
        fixture.write(
            "run.sh",
            "#!/bin/sh\n# vim: set fileencoding=utf-16 :\necho hi\n",
        );
        assert!(bash_script("sh run.sh", &fixture.0, |_| true).is_none());
    }

    #[test]
    fn never_shows_a_credential_or_what_the_chat_may_not_read() {
        let fixture = Fixture::new();
        for name in [
            ".ssh/id_rsa",
            ".ssh/id_ed25519",
            "certs/server.pem",
            "certs/tls.KEY",
            "certs/client.p12",
            ".aws/credentials",
            ".netrc",
            ".git-credentials",
            ".npmrc",
            ".pypirc",
            ".envrc",
            ".zshrc",
            ".bashrc",
            ".bash_profile",
            ".profile",
            ".zprofile",
            ".zshenv",
        ] {
            fixture.write(name, "export TOKEN=fixture-only\n");
            assert!(is_credential(&fixture.0.join(name)), "{name}");
            assert!(
                bash_script(&format!("source ./{name}"), &fixture.0, |_| true).is_none(),
                "{name}"
            );
        }
        // A link with a harmless name to one is not shown either.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(fixture.0.join(".envrc"), fixture.0.join("setup.sh"))
                .unwrap();
            assert!(bash_script("source ./setup.sh", &fixture.0, |_| true).is_none());
        }
        for name in [
            "credentials",
            "aws/credentials",
            "keys.txt",
            "deploy.sh",
            "profile.sh",
        ] {
            assert!(!is_credential(&fixture.0.join(name)), "{name}");
        }
        // What the chat may not read (a rule's deny or ask) is not shown,
        // judged on the path named and on where it leads.
        fixture.write("private/run.sh", "echo private\n");
        let script = bash_script("bash private/run.sh", &fixture.0, |_| true);
        assert_eq!(script.unwrap().content, "echo private\n");
        let private = fixture.0.join("private");
        let readable = |path: &Path| !path.starts_with(&private);
        assert!(bash_script("bash private/run.sh", &fixture.0, readable).is_none());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(fixture.0.join("private/run.sh"), fixture.0.join("run.sh"))
                .unwrap();
            assert!(bash_script("./run.sh", &fixture.0, readable).is_none());
        }
    }

    #[tokio::test]
    async fn a_slow_disk_gives_no_script_and_does_not_hold_up_the_prompt() {
        let fixture = Fixture::new();
        let slow = fixture.write("slow.sh", "echo slow\n");
        SLOW.lock().unwrap().push(slow.clone());
        let started = std::time::Instant::now();
        let shown = for_prompt("bash slow.sh".into(), fixture.0.clone(), |_| true).await;
        assert!(shown.is_none());
        assert!(started.elapsed() < Duration::from_millis(1500));
        fixture.write("quick.sh", "echo quick\n");
        let shown = for_prompt("bash quick.sh".into(), fixture.0.clone(), |_| true)
            .await
            .unwrap();
        assert_eq!(shown.script.content, "echo quick\n");
    }

    #[tokio::test]
    async fn the_seal_holds_only_while_the_file_reads_as_it_was_shown() {
        let fixture = Fixture::new();
        let file = fixture.write("x.sh", "echo shown\n");
        let shown = for_prompt("bash x.sh".into(), fixture.0.clone(), |_| true)
            .await
            .unwrap();
        assert!(shown.seal.as_ref().unwrap().holds().await);
        // Changed, even back to the same length.
        std::fs::write(&file, "echo other\n").unwrap();
        assert!(!shown.seal.as_ref().unwrap().holds().await);
        std::fs::write(&file, "echo shown\n").unwrap();
        assert!(shown.seal.as_ref().unwrap().holds().await);
        // The rest of a cut file was never shown; a cut file made whole is
        // another file.
        let long = fixture.write("long.sh", "a".repeat(MAX_SCRIPT_BYTES + 10));
        let cut = for_prompt("sh long.sh".into(), fixture.0.clone(), |_| true)
            .await
            .unwrap();
        std::fs::write(&long, "a".repeat(MAX_SCRIPT_BYTES + 20)).unwrap();
        assert!(cut.seal.as_ref().unwrap().holds().await);
        std::fs::write(&long, "a".repeat(MAX_SCRIPT_BYTES)).unwrap();
        assert!(!cut.seal.as_ref().unwrap().holds().await);
        // Gone, or too slow to read again.
        SLOW.lock().unwrap().push(file.clone());
        assert!(!shown.seal.as_ref().unwrap().holds().await);
        std::fs::remove_file(&file).unwrap();
        assert!(!shown.seal.as_ref().unwrap().holds().await);
        // A link moved to another file with the same text.
        #[cfg(unix)]
        {
            let first = fixture.write("one/run.sh", "echo same\n");
            fixture.write("two/run.sh", "echo same\n");
            let link = fixture.0.join("run.sh");
            std::os::unix::fs::symlink(&first, &link).unwrap();
            let shown = for_prompt("./run.sh".into(), fixture.0.clone(), |_| true)
                .await
                .unwrap();
            assert!(shown.seal.as_ref().unwrap().holds().await);
            std::fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink(fixture.0.join("two/run.sh"), &link).unwrap();
            assert!(!shown.seal.as_ref().unwrap().holds().await);
        }
    }

    /// A folder swapped for a link between resolving and opening: the
    /// file is not read where the link leads.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    #[test]
    fn a_folder_swapped_for_a_link_after_resolving_is_not_read() {
        let fixture = Fixture::new();
        let real = fixture.write("a/run.sh", "echo resolved\n");
        fixture.write("b/run.sh", "echo elsewhere\n");
        assert!(read_bytes(&real).is_some());
        std::fs::rename(fixture.0.join("a"), fixture.0.join("old")).unwrap();
        std::os::unix::fs::symlink(fixture.0.join("b"), fixture.0.join("a")).unwrap();
        assert!(read_bytes(&real).is_none());
    }

    #[test]
    fn a_script_the_command_writes_first_shows_its_path_only() {
        let fixture = Fixture::new();
        fixture.write("gen.sh", "echo OLD\n");
        let shown = find(
            "cat > gen.sh <<'EOF'\necho NEW\nEOF\nbash gen.sh",
            &fixture.0,
            |_| true,
        )
        .unwrap();
        assert_eq!(shown.script.withheld, Some(ScriptWithheld::Written));
        assert_eq!(shown.script.content, "");
        assert_eq!(Path::new(&shown.script.path), fixture.0.join("gen.sh"));
        // Nothing to check again: what runs was never shown.
        assert!(shown.seal.is_none());
        // A file that does not exist yet (a download) is named too.
        let shown = find(
            "curl -fsSL -o fresh.sh https://example.test/fresh.sh && bash fresh.sh",
            &fixture.0,
            |_| true,
        )
        .unwrap();
        assert_eq!(shown.script.withheld, Some(ScriptWithheld::Written));
        assert_eq!(Path::new(&shown.script.path), fixture.0.join("fresh.sh"));
        // After a step that may change any file, the text is withheld too.
        let shown = find("make && bash gen.sh", &fixture.0, |_| true).unwrap();
        assert_eq!(shown.script.withheld, Some(ScriptWithheld::Changeable));
        assert_eq!(shown.script.content, "");
        assert!(shown.seal.is_none());
        // Read as it is when every step before it is a trusted one.
        let shown = find("chmod +x gen.sh && bash gen.sh", &fixture.0, |_| true).unwrap();
        assert_eq!(shown.script.withheld, None);
        assert_eq!(shown.script.content, "echo OLD\n");
        assert!(shown.seal.is_some());
    }

    #[test]
    fn a_binary_program_gives_way_to_the_script_after_it() {
        let fixture = Fixture::new();
        fixture.write("tool", [0x7f, b'E', 0, 1]);
        fixture.write("x.sh", "echo second\n");
        // The program may change any file, so the script after it is named
        // with no text.
        let script = bash_script("./tool && bash x.sh", &fixture.0, |_| true).unwrap();
        assert_eq!(script.withheld, Some(ScriptWithheld::Changeable));
        assert_eq!(Path::new(&script.path), fixture.0.join("x.sh"));
    }

    #[test]
    fn control_characters_and_bidirectional_controls_are_shown_as_replacements() {
        let fixture = Fixture::new();
        fixture.write(
            "x.sh",
            "echo a\r\necho \u{1b}[2Kb\recho\tc\u{202E}d\u{7f}\n\u{fffe}",
        );
        let script = bash_script("bash x.sh", &fixture.0, |_| true).unwrap();
        assert_eq!(
            script.content,
            "echo a\necho \u{fffd}[2Kb\necho\tc\u{fffd}d\u{fffd}\n\u{fffe}"
        );
        // Direction marks and Unicode line breaks too.
        fixture.write(
            "marks.sh",
            "echo a\u{200e}b\u{200f}c\u{61c}d\u{2028}e\u{2029}f\n",
        );
        assert_eq!(
            bash_script("bash marks.sh", &fixture.0, |_| true)
                .unwrap()
                .content,
            "echo a\u{fffd}b\u{fffd}c\u{fffd}d\u{fffd}e\u{fffd}f\n"
        );
        // And the path, line breaks and tabs included (names NTFS refuses).
        #[cfg(unix)]
        {
            let odd = "odd\u{202e}\nname\t.sh";
            fixture.write(odd, "echo odd\n");
            let script = bash_script(&format!("bash '{odd}'"), &fixture.0, |_| true).unwrap();
            assert!(
                script.path.ends_with("odd\u{fffd}\u{fffd}name\u{fffd}.sh"),
                "{}",
                script.path
            );
        }
        // Bytes that are not UTF-8 read as replacements too.
        fixture.write("latin.sh", b"echo caf\xe9\n");
        assert_eq!(
            bash_script("bash latin.sh", &fixture.0, |_| true)
                .unwrap()
                .content,
            "echo caf\u{fffd}\n"
        );
    }
}

/// On Windows the card names no script (see `bash::script_files`).
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn no_script_is_read() {
        let folder = std::env::temp_dir().join(format!("haarness-script-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("x.py"), "print(1)\n").unwrap();
        std::fs::write(folder.join("run.sh"), "echo hi\n").unwrap();
        for command in ["python x.py", "python 'x.py'", "bash run.sh", "./run.sh"] {
            assert!(find(command, &folder, |_| true).is_none(), "{command}");
        }
        std::fs::remove_dir_all(folder).unwrap();
    }
}
