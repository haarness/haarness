//! Verified, offline ripgrep for every Haarness executable, agent shell and PTY.
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
};

pub const VERSION: &str = env!("HAARNESS_RG_VERSION");
pub const TARGET: &str = env!("HAARNESS_RG_TARGET");
pub const SHA256: &str = env!("HAARNESS_RG_SHA");
const NAME: &str = env!("HAARNESS_RG_NAME");
const ARCHIVE: &[u8] = include_bytes!(env!("HAARNESS_RG_ARCHIVE"));
const LICENSE: &str = concat!(
    include_str!("../../../vendor/ripgrep/COPYING"),
    "\n",
    include_str!("../../../vendor/ripgrep/LICENSE-MIT"),
    "\n",
    include_str!("../../../vendor/ripgrep/UNLICENSE")
);

/// Extract only to Haarness's private cache, never the workspace or global PATH.
/// Hash-check existing files too: a deleted or damaged cache repairs itself.
pub fn binary_path() -> io::Result<PathBuf> {
    extract(&cache_root()?)
}

fn cache_root() -> io::Result<PathBuf> {
    // An isolated `--data-dir` root keeps its cache too, as the CLI does.
    if let Some(root) = env::var_os("HAARNESS_DATA_DIR").filter(|root| !root.is_empty()) {
        return Ok(PathBuf::from(root).join("cache"));
    }
    if let Some(root) = env::var_os("HAARNESS_HOME") {
        let root = PathBuf::from(root);
        if !root.is_absolute() {
            return Err(io::Error::other("HAARNESS_HOME must be an absolute path"));
        }
        return Ok(root.join("cache"));
    }
    if let Some(root) = env::var_os("XDG_CACHE_HOME").map(PathBuf::from)
        && root.is_absolute()
    {
        return Ok(root.join("haarness"));
    }
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".cache/haarness"))
        .ok_or_else(|| io::Error::other("cannot locate the Haarness cache"))
}

fn extract(cache: &Path) -> io::Result<PathBuf> {
    let directory = cache
        .join("ripgrep")
        .join(VERSION)
        .join(TARGET)
        .join(SHA256);
    fs::create_dir_all(&directory)?;
    // The cache root can legitimately alias the legacy storage location. Reject
    // symlinks in our own subtree, where executables and extraction locks live.
    for path in directory.ancestors().take(4) {
        reject_symlink(path)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    }
    let lock_path = directory.join(".extract.lock");
    reject_symlink(&lock_path)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.lock_exclusive()?;
    let output = directory.join(NAME);
    reject_symlink(&output)?;
    if valid_binary(&output)? {
        return Ok(output);
    }
    let mut binary = Vec::new();
    flate2::read::GzDecoder::new(ARCHIVE).read_to_end(&mut binary)?;
    if hex::encode(Sha256::digest(&binary)) != SHA256 {
        return Err(io::Error::other("bundled ripgrep checksum mismatch"));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    temporary.write_all(&binary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o700))?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(&output).map_err(|error| error.error)?;
    fs::write(directory.join("RIPGREP-LICENSE.txt"), LICENSE)?;
    Ok(output)
}

fn reject_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_symlink() => Err(io::Error::other(format!(
            "refusing symlink in ripgrep cache: {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn valid_binary(path: &Path) -> io::Result<bool> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Err(io::Error::other(
            "ripgrep cache executable is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o100 == 0 {
            return Ok(false);
        }
    }
    Ok(hex::encode(Sha256::digest(fs::read(path)?)) == SHA256)
}

pub fn prepend_path(existing: Option<&OsStr>) -> io::Result<OsString> {
    path_with_binary(&binary_path()?, existing)
}

fn path_with_binary(binary: &Path, existing: Option<&OsStr>) -> io::Result<OsString> {
    let directory = binary.parent().expect("extracted binary has a directory");
    let paths = std::iter::once(directory.to_path_buf()).chain(
        existing
            .into_iter()
            .flat_map(env::split_paths)
            .filter(|path| path != directory),
    );
    env::join_paths(paths).map_err(io::Error::other)
}

/// Keep the existing login-shell behavior, restoring the bundled directory after
/// profiles run. Pass its path as an argument, never interpolate it as shell code.
pub fn shell_command(script: &str) -> io::Result<Command> {
    shell_with_binary(script, &binary_path()?)
}

fn shell_with_binary(script: &str, binary: &Path) -> io::Result<Command> {
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt as _;
        // cmd.exe parses its own command line, not C-runtime quoting: given
        // `"a b"` as one argument it saw `\"a b\"` and split it. With /s it
        // drops only the outer quotes and runs the script as written.
        let mut command = Command::new("cmd");
        command.raw_arg(format!("/d /s /c \"{script}\""));
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new("sh");
        command
            .args([
                "-lc",
                &format!("export PATH=\"$1${{PATH:+:$PATH}}\"; shift\n{script}"),
                "sh",
            ])
            .arg(binary.parent().expect("binary directory"));
        command
    };
    command.env(
        "PATH",
        path_with_binary(binary, env::var_os("PATH").as_deref())?,
    );
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_binary_runs_and_repairs_damage() {
        let cache = tempfile::tempdir().unwrap();
        let binary = extract(cache.path()).unwrap();
        let output = Command::new(&binary).arg("--version").output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("ripgrep {VERSION}")));
        fs::write(&binary, "damaged").unwrap();
        assert_eq!(extract(cache.path()).unwrap(), binary);
        assert!(valid_binary(&binary).unwrap());
        fs::remove_file(&binary).unwrap();
        assert_eq!(extract(cache.path()).unwrap(), binary);
        assert!(valid_binary(&binary).unwrap());
    }

    #[test]
    fn concurrent_first_use_has_one_verified_executable() {
        let cache = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            let tasks: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| extract(cache.path()).unwrap()))
                .collect();
            for task in tasks {
                assert!(valid_binary(&task.join().unwrap()).unwrap());
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_and_repairs_missing_execute_permission() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let cache = tempfile::tempdir().unwrap();
        let binary = extract(cache.path()).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o600)).unwrap();
        extract(cache.path()).unwrap();
        assert!(valid_binary(&binary).unwrap());
        fs::remove_file(&binary).unwrap();
        let outside = cache.path().join("outside");
        fs::write(&outside, "preserve").unwrap();
        symlink(&outside, &binary).unwrap();
        assert!(extract(cache.path()).is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "preserve");
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_finds_bundled_rg_with_spaces_and_shell_metacharacters() {
        let cache = tempfile::Builder::new()
            .prefix("haarness rg ' $() ")
            .tempdir()
            .unwrap();
        let binary = extract(cache.path()).unwrap();
        // No externally installed rg; the login shell can reset PATH again.
        let output = shell_with_binary(
            "command -v rg; rg --version; printf 'args:%s' \"$#\"",
            &binary,
        )
        .unwrap()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
        assert!(output.status.success());
        let output = String::from_utf8_lossy(&output.stdout);
        assert!(output.starts_with(binary.to_str().unwrap()), "{output}");
        assert!(output.contains(&format!("ripgrep {VERSION}")));
        assert!(output.contains("args:0"));
    }
}
