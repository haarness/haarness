use std::path::PathBuf;

/// The user's home folder: `HOME`, or `USERPROFILE` where Windows sets that
/// instead (PowerShell and cmd set no `HOME`, so user-level configuration,
/// instructions and `~` were skipped there).
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|home| !home.is_empty())
        .map(PathBuf::from)
}
