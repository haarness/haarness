//! Paths as the tools open and compare them.
//!
//! On Windows a canonical path carries the `\\?\` prefix, which `dunce`
//! drops only while the path is at most 260 characters long: a file deep in a
//! project (`node_modules`, a Java package, a long user folder) came back as
//! `\\?\C:\…` while the project folder stayed `C:\…`, so every check that the
//! file is inside the project said no, and the tools refused it as outside.
//! [`canonicalize`] gives every path the plain form; Rust's own file calls add
//! the prefix back where a long path needs it.

use std::io;
use std::path::{Path, PathBuf};

/// `path` with links followed, `.` and `..` folded and names spelled as on
/// disk, in the plain form on Windows whatever its length (see the module
/// documentation). Elsewhere it is [`dunce::canonicalize`].
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let path = dunce::canonicalize(path)?;
    #[cfg(windows)]
    if let Some(plain) = plain(&path) {
        return Ok(plain);
    }
    Ok(path)
}

/// The plain form of a verbatim disk path (`\\?\C:\…` as `C:\…`) when every
/// name in it means the same without the prefix: none ends in a dot or a
/// space, holds a character Windows reserves, or is a device name such as
/// `CON` or `COM1`. Its length does not matter (see [`canonicalize`]).
#[cfg(any(windows, test))]
fn plain(path: &Path) -> Option<PathBuf> {
    let text = path.to_str()?;
    let rest = text.strip_prefix(r"\\?\")?;
    let mut parts = rest.split('\\');
    let drive = parts.next()?;
    if drive.len() != 2 || !drive.ends_with(':') || !drive.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    parts
        .filter(|name| !name.is_empty())
        .all(plain_name)
        .then(|| PathBuf::from(rest))
}

#[cfg(any(windows, test))]
fn plain_name(name: &str) -> bool {
    const RESERVED: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    if name == "." || name == ".." || name.ends_with(['.', ' ']) {
        return false;
    }
    if name.chars().any(|c| c < ' ' || RESERVED.contains(&c)) {
        return false;
    }
    // A device name stays one with an extension or trailing dots and spaces
    // before it (`con.txt`, `COM1 .log`).
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    !device
}

/// A path as a model on Windows writes it, in the spelling Windows reads: a
/// Git Bash, Cygwin or WSL drive path (`/c/Users/…`, `/cygdrive/c/…`,
/// `/mnt/c/…`, `/C:/…`) as the drive path it names (`C:/Users/…`), and a path
/// wrapped in double quotes without them (Windows allows no `"` in a name).
/// `None` when it is already plain. Elsewhere such a path names a folder of
/// its own and stays as written.
#[must_use]
pub fn windows_spelling(value: &str) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    windows_spelling_of(value)
}

fn windows_spelling_of(value: &str) -> Option<String> {
    let unquoted = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .filter(|inner| !inner.is_empty() && !inner.contains('"'));
    let text = unquoted.unwrap_or(value);
    let drive = |rest: &str| -> Option<String> {
        let mut chars = rest.chars();
        let letter = chars.next().filter(char::is_ascii_alphabetic)?;
        let after = chars.as_str();
        let after = after.strip_prefix(':').unwrap_or(after);
        if !(after.is_empty() || after.starts_with('/')) {
            return None;
        }
        Some(format!(
            "{}:/{}",
            letter.to_ascii_uppercase(),
            after.trim_start_matches('/')
        ))
    };
    let mapped = text
        .strip_prefix("/cygdrive/")
        .or_else(|| text.strip_prefix("/mnt/"))
        .and_then(drive)
        .or_else(|| text.strip_prefix('/').and_then(drive));
    match (mapped, unquoted) {
        (Some(mapped), _) => Some(mapped),
        (None, Some(inner)) => Some(inner.to_owned()),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_verbatim_disk_path_takes_the_plain_form_and_a_doubtful_one_keeps_its_prefix() {
        let deep = format!(
            r"\\?\C:\Users\sam\proj\{}\settings.json",
            "module_with_a_long_name\\".repeat(12)
        );
        assert!(deep.len() > 260);
        assert_eq!(plain(Path::new(&deep)), Some(PathBuf::from(&deep[4..])));
        assert_eq!(plain(Path::new(r"\\?\D:\")), Some(PathBuf::from(r"D:\")));
        for kept in [
            r"\\?\C:\proj\name.",
            r"\\?\C:\proj\name ",
            r"\\?\C:\proj\con.txt",
            r"\\?\C:\proj\COM1 .log",
            r"\\?\C:\proj\a/b",
            r"\\?\UNC\server\share\file",
            r"\\?\C:\proj\..\x",
            r"C:\proj\plain",
        ] {
            assert_eq!(plain(Path::new(kept)), None, "{kept}");
        }
        for allowed in [
            r"\\?\C:\proj\COM0",
            r"\\?\C:\proj\console.txt",
            r"\\?\C:\proj\.git",
        ] {
            assert!(plain(Path::new(allowed)).is_some(), "{allowed}");
        }
    }

    #[test]
    fn windows_spellings_of_drive_paths_and_quoted_paths() {
        for (given, read) in [
            ("/c/Users/sam/proj/a.py", "C:/Users/sam/proj/a.py"),
            ("/C:/Users/sam/proj", "C:/Users/sam/proj"),
            ("/cygdrive/d/work", "D:/work"),
            ("/mnt/c/Users/sam", "C:/Users/sam"),
            ("/c", "C:/"),
            ("\"src/inventory/core.py\"", "src/inventory/core.py"),
            ("\"/c/Users/sam\"", "C:/Users/sam"),
        ] {
            assert_eq!(windows_spelling_of(given).as_deref(), Some(read), "{given}");
        }
        for kept in [
            "src/a.py",
            "/usr/lib",
            "/cc/x",
            "/mnt/data/x",
            "C:\\proj",
            "\"\"",
            "\"a\"b\"",
        ] {
            assert_eq!(windows_spelling_of(kept), None, "{kept}");
        }
    }
}
