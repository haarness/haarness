//! The program that runs an agent's shell commands.
//!
//! On macOS and Linux it is `sh`. On Windows it is Git for Windows' bash when
//! Git is installed, since the tool is `bash`, models write POSIX shell and the
//! permission reader reads POSIX shell; otherwise cmd.exe. Git's bash is found
//! from Git's own install: the `git.exe` on `PATH` (Git's `cmd`, `bin` or
//! `mingw64\bin` folder), the install folder Git's installer records in the
//! registry, then the usual install folders. A `bash.exe` found anywhere else
//! is never used: `C:\Windows\System32\bash.exe` and the `WindowsApps` launchers
//! start WSL, which runs in Linux, not in the project's folder.
//!
//! `HAARNESS_WINDOWS_SHELL` (environment) or `execution.windows_shell` (the
//! global configuration; a project's is ignored) chooses: `cmd`, `auto`, or
//! the path of a `bash.exe` to run.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, OnceLock};

/// The variable that chooses the agent shell on Windows.
pub const ENV: &str = "HAARNESS_WINDOWS_SHELL";

/// What runs an agent's command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentShell {
    /// `sh -lc` on macOS and Linux.
    Posix,
    /// Git for Windows' bash (or another MSYS or Cygwin bash named by the
    /// person), run as a login shell in the command's folder.
    GitBash(PathBuf),
    /// cmd.exe, when Windows has no bash to run.
    Cmd,
}

impl AgentShell {
    /// Whether commands are read as POSIX shell: everywhere but cmd.exe.
    #[must_use]
    pub fn posix(&self) -> bool {
        !matches!(self, Self::Cmd)
    }
}

/// The `execution.windows_shell` the runtime was last configured with.
static CONFIGURED: LazyLock<parking_lot::RwLock<Option<String>>> =
    LazyLock::new(|| parking_lot::RwLock::new(None));

/// Takes `execution.windows_shell` from a runtime's configuration (the CLI
/// leaves out a project's value).
pub fn configure(value: Option<&str>) {
    *CONFIGURED.write() = value.map(str::to_owned);
}

/// The agent shell now: the environment's choice, else the configuration's,
/// else what this computer has.
#[must_use]
pub fn current() -> AgentShell {
    if !cfg!(windows) {
        return AgentShell::Posix;
    }
    let env = std::env::var(ENV).ok();
    let configured = CONFIGURED.read().clone();
    choose(env.as_deref(), configured.as_deref(), detected)
}

/// The shell `env` (the variable), else `configured`, names; `auto` or
/// nothing named takes `detect`'s. A value naming no usable bash is said
/// once in the log and passed over.
fn choose(env: Option<&str>, configured: Option<&str>, detect: fn() -> AgentShell) -> AgentShell {
    for (source, value) in [(ENV, env), ("execution.windows_shell", configured)] {
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            continue;
        };
        if value.eq_ignore_ascii_case("auto") {
            break;
        }
        if value.eq_ignore_ascii_case("cmd") || value.eq_ignore_ascii_case("cmd.exe") {
            return AgentShell::Cmd;
        }
        let path = PathBuf::from(value.trim_matches('"'));
        match refused(&path) {
            None if path.is_file() => return AgentShell::GitBash(path),
            reason => {
                static SAID: OnceLock<()> = OnceLock::new();
                SAID.get_or_init(|| {
                    tracing::warn!(
                        source,
                        path = %path.display(),
                        reason = reason.unwrap_or("no such file"),
                        "the shell named for agent commands cannot be used; Haarness chooses one itself"
                    );
                });
            }
        }
    }
    detect()
}

/// Why `path` is not a bash Haarness runs: WSL's launchers, in the Windows
/// folder or `WindowsApps`, start Linux in another file system.
fn refused(path: &Path) -> Option<&'static str> {
    let text = path.to_string_lossy().replace('/', "\\").to_lowercase();
    let windows = std::env::var("SystemRoot")
        .or_else(|_| std::env::var("windir"))
        .unwrap_or_else(|_| r"C:\Windows".into())
        .replace('/', "\\")
        .to_lowercase();
    if text.starts_with(&format!("{}\\", windows.trim_end_matches('\\')))
        || text.contains("\\windowsapps\\")
    {
        return Some("it starts WSL, which runs in Linux, not in the project's folder");
    }
    let name = text.rsplit('\\').next().unwrap_or_default();
    if !matches!(name, "bash.exe" | "bash" | "sh.exe" | "sh") {
        return Some("it is not a bash or sh program");
    }
    None
}

/// What this computer has, looked up once.
fn detected() -> AgentShell {
    static FOUND: OnceLock<AgentShell> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            git_bash(
                std::env::var_os("PATH").as_deref(),
                &install_folders(),
                registry_folders,
            )
            .map_or(AgentShell::Cmd, AgentShell::GitBash)
        })
        .clone()
}

/// Git's bash, from the `git.exe` on `path`, else from the folders Git's
/// installer recorded (`registry`, read only when `path` has none), else
/// from `usual` install folders.
fn git_bash(
    path: Option<&std::ffi::OsStr>,
    usual: &[PathBuf],
    registry: fn() -> Vec<PathBuf>,
) -> Option<PathBuf> {
    let from_path = path
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|folder| folder.is_absolute() && folder.join("git.exe").is_file())
        .filter(|folder| refused(&folder.join("bash.exe")).is_none())
        // Git's `cmd`, `bin` or `mingw64\bin` folder: its install is at most
        // two folders up.
        .flat_map(|folder| {
            folder
                .ancestors()
                .take(3)
                .map(Path::to_path_buf)
                .collect::<Vec<_>>()
        });
    from_path
        .chain(std::iter::once_with(registry).flatten())
        .chain(usual.iter().cloned())
        .find_map(|root| bash_in(&root))
}

/// The bash of the Git install at `root`: its `bin\bash.exe`, which sets up
/// Git's environment first, else the `usr\bin\bash.exe` that one runs.
fn bash_in(root: &Path) -> Option<PathBuf> {
    [root.join("bin"), root.join("usr").join("bin")]
        .into_iter()
        .map(|folder| folder.join("bash.exe"))
        .find(|bash| bash.is_file() && refused(bash).is_none())
}

/// Where Git for Windows installs: for everyone, for one person, and Scoop's.
fn install_folders() -> Vec<PathBuf> {
    let mut folders = Vec::new();
    for (variable, rest) in [
        ("ProgramW6432", &["Git"][..]),
        ("ProgramFiles", &["Git"]),
        ("ProgramFiles(x86)", &["Git"]),
        ("LOCALAPPDATA", &["Programs", "Git"]),
        ("USERPROFILE", &["scoop", "apps", "git", "current"]),
    ] {
        if let Some(base) = std::env::var_os(variable).filter(|base| !base.is_empty()) {
            let folder = rest
                .iter()
                .fold(PathBuf::from(base), |folder, part| folder.join(part));
            if !folders.contains(&folder) {
                folders.push(folder);
            }
        }
    }
    folders
}

/// The install folders Git's installer records under
/// `HKLM\SOFTWARE\GitForWindows` and `HKCU\…` (`InstallPath`), read with
/// Windows' own `reg.exe`.
fn registry_folders() -> Vec<PathBuf> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let system = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let reg = PathBuf::from(system).join("System32").join("reg.exe");
    let mut folders = Vec::new();
    for key in [
        r"HKLM\SOFTWARE\GitForWindows",
        r"HKCU\SOFTWARE\GitForWindows",
        r"HKLM\SOFTWARE\WOW6432Node\GitForWindows",
    ] {
        let Ok(output) = std::process::Command::new(&reg)
            .args(["query", key, "/v", "InstallPath"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            continue;
        };
        folders.extend(install_path(&String::from_utf8_lossy(&output.stdout)));
    }
    folders
}

/// The `InstallPath` value of `reg query` output.
fn install_path(output: &str) -> Option<PathBuf> {
    output.lines().find_map(|line| {
        let (_, value) = line
            .trim()
            .strip_prefix("InstallPath")?
            .split_once("REG_SZ")?;
        let value = value.trim();
        (!value.is_empty()).then(|| PathBuf::from(value))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let root = std::env::temp_dir().join(format!("haarness-shell-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Empty files at `files`, written with `/` between their folders.
    fn install(root: &Path, files: &[&str]) {
        for file in files {
            let file = file
                .split('/')
                .fold(root.to_path_buf(), |path, part| path.join(part));
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, b"").unwrap();
        }
    }

    #[test]
    fn git_s_bash_is_found_from_git_on_path_its_registry_entry_or_its_usual_folder() {
        let root = scratch();
        let git = root.as_path().join("Git");
        install(&git, &["cmd/git.exe", "bin/bash.exe", "usr/bin/bash.exe"]);
        let path =
            std::env::join_paths([root.as_path().join("elsewhere"), git.join("cmd")]).unwrap();
        let none = || Vec::new();
        // Git's `cmd` folder on PATH leads to its `bin\bash.exe`.
        assert_eq!(
            git_bash(Some(&path), &[], none),
            Some(git.join("bin").join("bash.exe"))
        );
        // `mingw64\bin`, where a Git for Windows SDK puts git.exe.
        let sdk = root.as_path().join("sdk");
        install(&sdk, &["mingw64/bin/git.exe", "usr/bin/bash.exe"]);
        let path = std::env::join_paths([sdk.join("mingw64").join("bin")]).unwrap();
        assert_eq!(
            git_bash(Some(&path), &[], none),
            Some(sdk.join("usr").join("bin").join("bash.exe"))
        );
        // No git on PATH: the usual install folder, then nothing.
        assert_eq!(
            git_bash(None, std::slice::from_ref(&git), none),
            Some(git.join("bin").join("bash.exe"))
        );
        assert_eq!(git_bash(None, &[root.as_path().join("absent")], none), None);
        assert_eq!(
            install_path(
                "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\GitForWindows\r\n    InstallPath    REG_SZ    C:\\Program Files\\Git\r\n"
            ),
            Some(PathBuf::from("C:\\Program Files\\Git"))
        );
    }

    #[test]
    fn a_bash_on_path_alone_is_never_taken_nor_wsl_s_launchers() {
        let root = scratch();
        // A folder holding bash.exe but no git.exe is not Git.
        install(root.as_path(), &["tools/bash.exe"]);
        let path = std::env::join_paths([root.as_path().join("tools")]).unwrap();
        assert_eq!(git_bash(Some(&path), &[], Vec::new), None);
        for wsl in [
            r"C:\Windows\System32\bash.exe",
            r"c:/windows/system32/bash.exe",
            r"C:\Users\sam\AppData\Local\Microsoft\WindowsApps\bash.exe",
        ] {
            assert!(refused(Path::new(wsl)).is_some(), "{wsl}");
        }
        assert!(refused(Path::new(r"C:\Program Files\Git\bin\bash.exe")).is_none());
        assert!(refused(Path::new(r"C:\Program Files\Git\bin\git.exe")).is_some());
    }

    #[test]
    fn the_environment_then_the_configuration_choose_and_a_bad_choice_is_passed_over() {
        let root = scratch();
        install(root.as_path(), &["msys64/usr/bin/bash.exe"]);
        let bash = root
            .as_path()
            .join("msys64")
            .join("usr")
            .join("bin")
            .join("bash.exe");
        let bash_text = bash.display().to_string();
        let found = || AgentShell::GitBash(PathBuf::from("found"));
        assert_eq!(choose(None, None, found), found());
        assert_eq!(
            choose(Some("cmd"), Some(&bash_text), found),
            AgentShell::Cmd
        );
        assert_eq!(
            choose(None, Some(&bash_text), found),
            AgentShell::GitBash(bash.clone())
        );
        assert_eq!(choose(Some("auto"), Some("cmd"), found), found());
        assert_eq!(choose(Some(" CMD "), None, found), AgentShell::Cmd);
        assert_eq!(
            choose(Some(r"C:\Windows\System32\bash.exe"), None, found),
            found()
        );
        assert_eq!(
            choose(
                Some(
                    &root
                        .as_path()
                        .join("missing")
                        .join("bash.exe")
                        .display()
                        .to_string()
                ),
                Some("cmd"),
                found
            ),
            AgentShell::Cmd
        );
        assert!(!AgentShell::Cmd.posix());
        assert!(AgentShell::GitBash(bash).posix());
    }
}
