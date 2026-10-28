//! The data folder the server keeps preview versions and saved tool output
//! in, apart from the stores its caller hands it.

use std::path::{Path, PathBuf};
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

/// Makes `folder`, and what is missing above it, for this user alone (0700
/// on Unix): saved output holds whole command output and MCP resources. One
/// already there that others can open (made by an older build) is narrowed
/// too, when this user may; the folders above it are left as they are.
pub(crate) async fn private_folder(folder: &Path) -> std::io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(folder).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = tokio::fs::metadata(folder).await?.permissions().mode();
        if mode & 0o077 != 0 {
            // Another user's folder is not this one's to change; the files
            // written in it are still private.
            let _ =
                tokio::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).await;
        }
    }
    Ok(())
}

/// A new file `<prefix>_<id>.txt` in `folder` (made as [`private_folder`]
/// makes it) that only this user can read (0600 on Unix), never one that
/// is already there.
pub(crate) async fn new_private_file(
    folder: &Path,
    prefix: &str,
) -> std::io::Result<(PathBuf, tokio::fs::File)> {
    private_folder(folder).await?;
    let path = folder.join(format!("{prefix}_{}.txt", uuid::Uuid::now_v7()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(&path).await?;
    Ok((path, file))
}

/// Where saved output goes when there is no data folder (no home folder to
/// find one in): a folder of this user's own in the system temp folder,
/// chosen once per process. See [`private_temp_folder_in`].
pub(crate) fn private_temp_folder() -> PathBuf {
    static FOLDER: OnceLock<PathBuf> = OnceLock::new();
    FOLDER
        .get_or_init(|| private_temp_folder_in(&std::env::temp_dir()))
        .clone()
}

/// `haarness-<uid>` in `temp`, made 0700. A shared temp folder lets anyone
/// make that name first, so one that is not this user's, is a link, or can
/// be opened by others is not used: a new `haarness-<uid>-<id>` is made
/// instead, and no one else can read or plant what is saved there.
#[cfg(unix)]
fn private_temp_folder_in(temp: &Path) -> PathBuf {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    let user = nix::unistd::geteuid().as_raw();
    let named = temp.join(format!("haarness-{user}"));
    let usable = match std::fs::DirBuilder::new().mode(0o700).create(&named) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::symlink_metadata(&named).is_ok_and(|metadata| {
                let open_to_others = metadata.mode() & 0o077 != 0;
                metadata.is_dir() && metadata.uid() == user && !open_to_others
            })
        }
        Err(_) => false,
    };
    if usable {
        return named;
    }
    let own = temp.join(format!("haarness-{user}-{}", uuid::Uuid::now_v7()));
    // Should this fail too, the first write tries again and reports why.
    let _ = std::fs::DirBuilder::new().mode(0o700).create(&own);
    own
}

/// Windows' temp folder is the user's own.
#[cfg(not(unix))]
fn private_temp_folder_in(temp: &Path) -> PathBuf {
    temp.join("haarness")
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

    #[cfg(unix)]
    mod private {
        use super::*;
        use std::collections::BTreeMap;
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        use tokio_util::sync::CancellationToken;

        /// A folder of this test's own, removed with everything in it.
        struct Scratch(PathBuf);
        impl Scratch {
            fn new() -> Self {
                let path =
                    std::env::temp_dir().join(format!("haarness-private-{}", uuid::Uuid::now_v7()));
                std::fs::create_dir(&path).unwrap();
                Self(path)
            }
        }
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        fn mode(path: &Path) -> u32 {
            std::fs::symlink_metadata(path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        }

        /// What a long tool result, a long command and a long MCP resource
        /// save is for this user alone, in a folder only they can open: one
        /// an older build made open to everyone is narrowed, one that is
        /// missing is made private.
        #[tokio::test]
        async fn saved_output_is_readable_by_its_owner_alone() {
            let scratch = Scratch::new();
            let older = scratch.0.join("older/tool-output");
            std::fs::create_dir_all(&older).unwrap();
            std::fs::set_permissions(&older, std::fs::Permissions::from_mode(0o755)).unwrap();
            let fresh = scratch.0.join("fresh/data/tool-output");
            let long = (0..3_000)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            for folder in [&older, &fresh] {
                let saved = crate::tools::truncate_output_at(long.clone(), folder)
                    .await
                    .unwrap();
                let saved = PathBuf::from(saved.metadata["outputPath"].as_str().unwrap());
                assert_eq!(mode(&saved), 0o600, "{}", saved.display());
                assert_eq!(std::fs::read_to_string(&saved).unwrap(), long);
                assert_eq!(mode(folder), 0o700, "{}", folder.display());

                let command = crate::shell::run(
                    "awk 'BEGIN {for(i=0;i<5000;i++) print i}'",
                    &scratch.0,
                    0,
                    &CancellationToken::new(),
                    folder,
                    &BTreeMap::new(),
                    None,
                )
                .await
                .unwrap();
                let saved = PathBuf::from(command.metadata["outputPath"].as_str().unwrap());
                assert_eq!(mode(&saved), 0o600, "{}", saved.display());
            }
            // The folders a missing one is made in are this user's too.
            assert_eq!(mode(&scratch.0.join("fresh")), 0o700);
        }

        #[test]
        fn without_a_data_folder_output_goes_to_a_temp_folder_of_this_user_alone() {
            let scratch = Scratch::new();
            let user = nix::unistd::geteuid().as_raw();
            let named = scratch.0.join(format!("haarness-{user}"));
            assert_eq!(private_temp_folder_in(&scratch.0), named);
            assert_eq!(mode(&named), 0o700);
            // The same folder the next time.
            assert_eq!(private_temp_folder_in(&scratch.0), named);
        }

        #[test]
        fn a_temp_folder_others_can_open_or_a_link_in_its_place_is_not_used() {
            let user = nix::unistd::geteuid().as_raw();
            // Made open to everyone, as another user (or a umask of 0)
            // would leave it.
            let open = Scratch::new();
            let named = open.0.join(format!("haarness-{user}"));
            std::fs::DirBuilder::new()
                .mode(0o777)
                .create(&named)
                .unwrap();
            std::fs::set_permissions(&named, std::fs::Permissions::from_mode(0o777)).unwrap();
            let chosen = private_temp_folder_in(&open.0);
            assert_ne!(chosen, named);
            assert!(chosen.starts_with(&open.0), "{}", chosen.display());
            assert_eq!(mode(&chosen), 0o700);
            assert_eq!(mode(&named), 0o777, "someone else's folder is left alone");

            // A link someone put there, to a folder of theirs.
            let linked = Scratch::new();
            let target = linked.0.join("theirs");
            std::fs::create_dir(&target).unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
            let named = linked.0.join(format!("haarness-{user}"));
            std::os::unix::fs::symlink(&target, &named).unwrap();
            let chosen = private_temp_folder_in(&linked.0);
            assert_ne!(chosen, named);
            assert_eq!(mode(&chosen), 0o700);
            assert!(std::fs::read_dir(&target).unwrap().next().is_none());
        }
    }
}
