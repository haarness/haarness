//! The script a shell command runs, read for the person answering its
//! permission request: approving `bash /tmp/x.sh` should not mean approving
//! a file nobody could see. Display only: nothing here adds a permission,
//! changes a pattern or decides a request.

use std::io::Read as _;
use std::path::Path;

use haarness_protocol::PermissionScript;

/// The most of a script a request carries; a longer one is cut here and
/// marked [`PermissionScript::truncated`].
pub const MAX_SCRIPT_BYTES: usize = 30_000;

/// How many of a command's script files are tried before giving up: a
/// program run by its path may be a binary (`/usr/bin/git status && bash
/// x.sh`), and the next one is still worth showing.
const MAX_TRIED: usize = 4;

/// The first script `command` runs from `cwd` that reads as text (see
/// [`super::bash::script_files`] for what counts). Nothing for a secret-named
/// file (the built-in `read` rules), a binary, a folder, a device or a
/// missing file.
#[must_use]
pub fn bash_script(command: &str, cwd: &Path) -> Option<PermissionScript> {
    super::bash::script_files(command, cwd)
        .iter()
        .take(MAX_TRIED)
        .find_map(|file| read(file))
}

/// `file` (absolute, as the command names it) read where it really is,
/// links followed.
fn read(file: &Path) -> Option<PermissionScript> {
    let shown = super::normalize_absolute(file);
    let real = file.canonicalize().ok()?;
    if super::is_secret_path(&shown) || super::is_secret_path(&real) {
        return None;
    }
    let file = open(&real).ok()?;
    // Checked on the open file: a folder or a pipe put there after the
    // path was resolved is still refused, and a pipe never blocks the open.
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_SCRIPT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let truncated = bytes.len() > MAX_SCRIPT_BYTES;
    bytes.truncate(MAX_SCRIPT_BYTES);
    if bytes.contains(&0) {
        return None;
    }
    if truncated {
        // A character the cut split in two is left out whole.
        bytes.truncate(complete_utf8(&bytes));
    }
    Some(PermissionScript {
        path: shown.to_string_lossy().into_owned(),
        content: displayed(&String::from_utf8_lossy(&bytes)),
        truncated,
    })
}

#[cfg(unix)]
fn open(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
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
/// other than tab and line feed) or that reorder what is shown
/// (bidirectional controls) as `�`, so a script cannot hide a line.
fn displayed(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    text.chars()
        .map(|character| match character {
            '\r' => '\n',
            '\t' | '\n' => character,
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => '\u{FFFD}',
            character if character.is_control() => '\u{FFFD}',
            character => character,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        let script = bash_script("bash x.sh", &fixture.0).unwrap();
        assert_eq!(script.path, fixture.0.join("x.sh").to_string_lossy());
        assert_eq!(script.content, "echo hi\n");
        assert!(!script.truncated);
        // A `cd` earlier in the line moves where it is looked for.
        fixture.write("sub/y.sh", "echo sub\n");
        let script = bash_script("cd sub && ./y.sh", &fixture.0).unwrap();
        assert_eq!(script.path, fixture.0.join("sub/y.sh").to_string_lossy());
        assert_eq!(script.content, "echo sub\n");
        assert!(bash_script("echo hi", &fixture.0).is_none());
    }

    #[test]
    fn cuts_a_long_script_and_says_so() {
        let fixture = Fixture::new();
        fixture.write("big.sh", "a".repeat(MAX_SCRIPT_BYTES + 10));
        let script = bash_script("sh big.sh", &fixture.0).unwrap();
        assert_eq!(script.content.len(), MAX_SCRIPT_BYTES);
        assert!(script.truncated);
        // Exactly at the cap is whole.
        fixture.write("full.sh", "b".repeat(MAX_SCRIPT_BYTES));
        assert!(!bash_script("sh full.sh", &fixture.0).unwrap().truncated);
        // A character the cut splits is left out, not shown as `�`.
        fixture.write(
            "wide.sh",
            format!("{}é and more", "c".repeat(MAX_SCRIPT_BYTES - 1)),
        );
        let script = bash_script("sh wide.sh", &fixture.0).unwrap();
        assert_eq!(script.content, "c".repeat(MAX_SCRIPT_BYTES - 1));
        assert!(script.truncated);
    }

    #[test]
    fn shows_nothing_for_a_missing_file_a_folder_a_binary_or_a_secret() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.0.join("folder")).unwrap();
        fixture.write("binary", [0x7f, b'E', 0, 1, 2]);
        fixture.write("prod.env", "TOKEN=1\n");
        fixture.write(".env", "TOKEN=1\n");
        for command in [
            "bash missing.sh",
            "bash folder",
            "./binary",
            "source prod.env",
            ". .env",
        ] {
            assert!(bash_script(command, &fixture.0).is_none(), "{command}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn follows_links_but_never_into_a_secret() {
        let fixture = Fixture::new();
        let target = fixture.write("real/run.sh", "echo real\n");
        std::os::unix::fs::symlink(&target, fixture.0.join("run.sh")).unwrap();
        let script = bash_script("./run.sh", &fixture.0).unwrap();
        // Named as the command names it, read where it leads.
        assert_eq!(script.path, fixture.0.join("run.sh").to_string_lossy());
        assert_eq!(script.content, "echo real\n");
        let secret = fixture.write("prod.env", "TOKEN=1\n");
        std::os::unix::fs::symlink(&secret, fixture.0.join("link.sh")).unwrap();
        assert!(bash_script("bash link.sh", &fixture.0).is_none());
        // A pipe is not read, and never blocks.
        let pipe = std::process::Command::new("mkfifo")
            .arg(fixture.0.join("pipe.sh"))
            .status();
        if pipe.is_ok_and(|status| status.success()) {
            assert!(bash_script("bash pipe.sh", &fixture.0).is_none());
        }
    }

    #[test]
    fn a_binary_program_gives_way_to_the_script_after_it() {
        let fixture = Fixture::new();
        fixture.write("tool", [0x7f, b'E', 0, 1]);
        fixture.write("x.sh", "echo second\n");
        let script = bash_script("./tool && bash x.sh", &fixture.0).unwrap();
        assert_eq!(script.content, "echo second\n");
    }

    #[test]
    fn control_characters_and_bidirectional_controls_are_shown_as_replacements() {
        let fixture = Fixture::new();
        fixture.write(
            "x.sh",
            "echo a\r\necho \u{1b}[2Kb\recho\tc\u{202E}d\u{7f}\n\u{fffe}",
        );
        let script = bash_script("bash x.sh", &fixture.0).unwrap();
        assert_eq!(
            script.content,
            "echo a\necho \u{fffd}[2Kb\necho\tc\u{fffd}d\u{fffd}\n\u{fffe}"
        );
        // Bytes that are not UTF-8 read as replacements too.
        fixture.write("latin.sh", b"echo caf\xe9\n");
        assert_eq!(
            bash_script("bash latin.sh", &fixture.0).unwrap().content,
            "echo caf\u{fffd}\n"
        );
    }
}
