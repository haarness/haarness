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

/// Whether this process is a test binary that cargo built
/// (`target/<profile>/deps/<crate>-<hash>`). Such a run must never write into
/// the folders of the person running it (Upstream PR #1145): where Haarness
/// would otherwise fall back to the home's data or cache folder, a test
/// binary gets [`test_folder`] instead.
#[must_use]
pub fn test_binary() -> bool {
    std::env::current_exe().ok().is_some_and(|executable| {
        executable.parent().and_then(std::path::Path::file_name) == Some("deps".as_ref())
    })
}

/// A test binary's own folder for what Haarness keeps in the home: one per
/// process under the system temp folder, so tests of separate runs never see
/// each other's files.
#[must_use]
pub fn test_folder() -> PathBuf {
    std::env::temp_dir()
        .join("haarness-test-data")
        .join(std::process::id().to_string())
}

/// The variable that names the base configuration folder.
pub const BASE_CONFIG_DIR_ENV: &str = "HAARNESS_BASE_CONFIG_DIR";

/// The base configuration folder, `HAARNESS_BASE_CONFIG_DIR`: a read-only
/// layer that a team or device management ships below the user's global
/// configuration. `None` when the variable is unset or blank, so Haarness
/// loads exactly as before, and when it is not an absolute path: an
/// administrator's folder, it must mean the same folder for every process,
/// whatever folder each starts in ([`base_config_dir_problem`] says why it is
/// ignored).
#[must_use]
pub fn base_config_dir() -> Option<PathBuf> {
    base_config_dir_from(std::env::var_os(BASE_CONFIG_DIR_ENV))
}

/// [`base_config_dir`] for a value of the variable, without a trailing
/// separator or `.` parts.
#[must_use]
pub fn base_config_dir_from(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let path = written(value?);
    (path.is_absolute()).then(|| path.components().collect())
}

/// Why a set `HAARNESS_BASE_CONFIG_DIR` is ignored, in one line: it is not an
/// absolute path. `None` when it is unset, blank or absolute.
#[must_use]
pub fn base_config_dir_problem() -> Option<String> {
    let path = written(std::env::var_os(BASE_CONFIG_DIR_ENV)?);
    (!path.as_os_str().is_empty() && !path.is_absolute()).then(|| {
        format!(
            "{BASE_CONFIG_DIR_ENV} must be an absolute path; ignoring \"{}\"",
            path.display()
        )
    })
}

fn written(value: std::ffi::OsString) -> PathBuf {
    match value.to_str() {
        Some(text) => PathBuf::from(text.trim()),
        None => PathBuf::from(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_config_dir_is_unset_for_blank_and_relative_values() {
        assert_eq!(base_config_dir_from(None), None);
        assert_eq!(base_config_dir_from(Some("".into())), None);
        assert_eq!(base_config_dir_from(Some("  ".into())), None);
        assert_eq!(base_config_dir_from(Some("team".into())), None);
        assert_eq!(base_config_dir_from(Some("./team".into())), None);
        let absolute = std::env::temp_dir().join("team-base");
        assert_eq!(
            base_config_dir_from(Some(format!(" {}/ ", absolute.display()).into())),
            Some(absolute.components().collect())
        );
        assert_eq!(
            base_config_dir_from(Some(format!("{}/./", absolute.display()).into())),
            Some(absolute.components().collect())
        );
    }
}
