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

/// The variable that names the base configuration folder.
pub const BASE_CONFIG_DIR_ENV: &str = "HAARNESS_BASE_CONFIG_DIR";

/// The base configuration folder, `HAARNESS_BASE_CONFIG_DIR`: a read-only
/// layer that a team or device management ships below the user's global
/// configuration. `None` when the variable is unset or blank, so Haarness
/// loads exactly as before; a relative value is read from the folder
/// Haarness starts in, which the servers it starts share.
#[must_use]
pub fn base_config_dir() -> Option<PathBuf> {
    base_config_dir_from(std::env::var_os(BASE_CONFIG_DIR_ENV))
}

/// [`base_config_dir`] for a value of the variable.
#[must_use]
pub fn base_config_dir_from(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let value = value?;
    let path = match value.to_str() {
        Some(text) => PathBuf::from(text.trim()),
        None => PathBuf::from(value),
    };
    if path.as_os_str().is_empty() {
        return None;
    }
    std::path::absolute(&path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_config_dir_is_unset_for_blank_values_and_absolute_otherwise() {
        assert_eq!(base_config_dir_from(None), None);
        assert_eq!(base_config_dir_from(Some("".into())), None);
        assert_eq!(base_config_dir_from(Some("  ".into())), None);
        let absolute = std::env::temp_dir().join("team-base");
        assert_eq!(
            base_config_dir_from(Some(format!(" {} ", absolute.display()).into())),
            Some(absolute)
        );
        let relative = base_config_dir_from(Some("team".into())).unwrap();
        assert!(relative.is_absolute());
        assert!(relative.ends_with("team"));
    }
}
