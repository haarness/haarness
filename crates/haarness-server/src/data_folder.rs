//! The data folder the server keeps preview versions and saved tool output
//! in, apart from the stores its caller hands it.

use std::path::PathBuf;
use std::sync::OnceLock;

static NAMED: OnceLock<PathBuf> = OnceLock::new();

/// A test binary, this crate's own unit tests included.
fn test_binary() -> bool {
    cfg!(test) || haarness_protocol::test_binary()
}

/// Names the data folder for the rest of the process, as `--data-dir` does
/// for the CLI (`<root>/data`). The first name wins.
pub fn use_data_folder(folder: PathBuf) {
    let _ = NAMED.set(folder);
}

/// The data folder: the one named with [`use_data_folder`], else
/// `$HAARNESS_DATA_DIR/data`, else `$HAARNESS_HOME/data`, else the app's own
/// data folder. A test binary never gets the app's own: it would write into
/// the data folder of the person running the tests, so it gets one of its
/// own in the system temp folder, one per test process.
#[must_use]
pub fn data_folder() -> Option<PathBuf> {
    if let Some(folder) = NAMED.get() {
        return Some(folder.clone());
    }
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|root| root.is_absolute())
    };
    if let Some(root) = std::env::var_os("HAARNESS_DATA_DIR") {
        return Some(PathBuf::from(root).join("data"));
    }
    // HAARNESS_HOME keeps everything under one folder.
    if let Some(root) = absolute("HAARNESS_HOME") {
        return Some(root.join("data"));
    }
    if test_binary() {
        return Some(haarness_protocol::test_folder().join("data"));
    }
    directories::ProjectDirs::from("dev", "haarness", "haarness")
        .map(|project| project.data_local_dir().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The folder this crate writes previews and tool output to: a test never
    // resolves the person's own data folder.
    #[test]
    fn a_test_never_resolves_the_persons_own_data_folder() {
        assert!(test_binary());
        if std::env::var_os("HAARNESS_DATA_DIR").is_some()
            || std::env::var_os("HAARNESS_HOME").is_some()
            || NAMED.get().is_some()
        {
            return;
        }
        let folder = data_folder().unwrap();
        assert!(
            folder.starts_with(std::env::temp_dir()),
            "{}",
            folder.display()
        );
        if let Some(project) = directories::ProjectDirs::from("dev", "haarness", "haarness") {
            assert!(
                !folder.starts_with(project.data_local_dir()),
                "{}",
                folder.display()
            );
        }
        if let Some(home) = haarness_protocol::home_dir() {
            assert!(
                !folder.starts_with(home.join(".local/share")),
                "{}",
                folder.display()
            );
        }
    }
}
