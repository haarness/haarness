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

/// `existing` with the bundled rg's folder first. When rg cannot be put in
/// place (a cache folder that cannot be written, a link in it), `existing`
/// as it is, so terminals and commands still start and find any rg on it.
pub fn prepend_path(existing: Option<&OsStr>) -> io::Result<OsString> {
    path_from(binary_path(), existing)
}

fn path_from(binary: io::Result<PathBuf>, existing: Option<&OsStr>) -> io::Result<OsString> {
    match binary {
        Ok(binary) => path_with_binary(&binary, existing),
        Err(error) => {
            unavailable(&error);
            existing.map(OsStr::to_os_string).ok_or(error)
        }
    }
}

/// Says once per process that the bundled rg could not be used: a cache
/// folder that cannot be written does not mend between commands, and one
/// line is enough to explain why `rg` is the one on `PATH`, if any.
fn unavailable(error: &io::Error) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        tracing::warn!(
            %error,
            "the bundled ripgrep cannot be extracted; commands and terminals use PATH as it is"
        );
    });
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
/// When the bundled rg cannot be put in place, the command still runs, with
/// `PATH` as it is (said once in the log).
pub fn shell_command(script: &str) -> io::Result<Command> {
    shell_from(script, binary_path())
}

fn shell_from(script: &str, binary: io::Result<PathBuf>) -> io::Result<Command> {
    match binary {
        Ok(binary) => shell_with_binary(script, Some(&binary)),
        Err(error) => {
            unavailable(&error);
            shell_with_binary(script, None)
        }
    }
}

fn shell_with_binary(script: &str, binary: Option<&Path>) -> io::Result<Command> {
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
    // A `cd` goes where the script says (as a permission card reads it),
    // never through a `CDPATH`: none is inherited, and one a login profile
    // sets is unset on this first line, so an error quotes only the
    // script's own lines; a read-only one is left, and breaks nothing.
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new("sh");
        // The bundled rg's folder goes back first after the profiles ran.
        let restore = if binary.is_some() {
            "export PATH=\"$1${PATH:+:$PATH}\"; "
        } else {
            ""
        };
        command
            .args([
                "-lc",
                &format!(
                    "{restore}shift; \
                     command unset CDPATH 2>/dev/null || :\n{script}"
                ),
                "sh",
            ])
            .arg(binary.map_or_else(PathBuf::new, |binary| {
                binary.parent().expect("binary directory").to_path_buf()
            }))
            .env_remove("CDPATH");
        command
    };
    if let Some(binary) = binary {
        command.env(
            "PATH",
            path_with_binary(binary, env::var_os("PATH").as_deref())?,
        );
    }
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

    /// A cache folder that cannot be written (or a link in it) leaves the
    /// bundled rg out, and commands and terminals still start with `PATH` as
    /// it is.
    #[test]
    fn without_the_bundled_rg_commands_run_with_path_as_it_is() {
        let failed = || Err(io::Error::other("cache folder is read-only"));
        assert_eq!(
            path_from(failed(), Some(OsStr::new("/usr/bin:/bin"))).unwrap(),
            OsString::from("/usr/bin:/bin")
        );
        assert!(path_from(failed(), None).is_err());
        #[cfg(unix)]
        {
            let root = tempfile::tempdir().unwrap();
            let output = shell_from("printf 'ran:%s' \"$#\"", failed())
                .unwrap()
                .current_dir(root.path())
                .env("HOME", root.path())
                .env("PATH", "/usr/bin:/bin")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert_eq!(String::from_utf8_lossy(&output.stdout), "ran:0");
        }
    }

    /// A login shell for `script` from `folder`, with `home` as its home
    /// folder (where its `.profile` is).
    #[cfg(unix)]
    fn run_in(
        script: &str,
        folder: &Path,
        home: &Path,
        cdpath: Option<&Path>,
    ) -> std::process::Output {
        let mut command = shell_with_binary(script, Some(&folder.join("bin/rg"))).unwrap();
        command.current_dir(folder).env("HOME", home);
        if let Some(cdpath) = cdpath {
            command.env("CDPATH", cdpath);
        }
        command.output().unwrap()
    }

    /// A `cd` goes where the script says, as a permission card reads it,
    /// whatever `CDPATH` the server's environment or a login profile sets.
    #[cfg(unix)]
    #[test]
    fn cd_goes_where_the_script_says_whatever_cdpath_says() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        for folder in [root.path(), elsewhere.path()] {
            fs::create_dir_all(folder.join("sub")).unwrap();
        }
        fs::write(root.path().join("sub/run.sh"), "printf here").unwrap();
        fs::write(elsewhere.path().join("sub/run.sh"), "printf elsewhere").unwrap();
        let script = "cd sub && sh run.sh";
        let output = run_in(script, root.path(), home.path(), Some(elsewhere.path()));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "here");
        fs::write(
            home.path().join(".profile"),
            format!("export CDPATH='{}'\n", elsewhere.path().display()),
        )
        .unwrap();
        let output = run_in(script, root.path(), home.path(), None);
        assert_eq!(String::from_utf8_lossy(&output.stdout), "here");
    }

    /// The wrapper's own line stays out of what the person reads: an error
    /// quotes only the script, and a profile's read-only `CDPATH` fails
    /// nothing.
    #[cfg(unix)]
    #[test]
    fn the_wrapper_stays_out_of_errors_and_a_read_only_cdpath_breaks_nothing() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let output = run_in("echo )", root.path(), home.path(), None);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(')'), "{error}");
        assert!(
            !error.contains("CDPATH") && !error.contains("shift"),
            "{error}"
        );
        fs::write(
            home.path().join(".profile"),
            "readonly CDPATH=/nonexistent\n",
        )
        .unwrap();
        let output = run_in("printf ran", root.path(), home.path(), None);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "ran");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "");
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
            Some(&binary),
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
