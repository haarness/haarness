//! A chat's own working folder inside the server's project folder
//! ([`haarness_protocol::Thread::directory`]): how one is named, made and
//! found again, without ever following a link out of the project.

use std::path::{Path, PathBuf};

use crate::{CoreError, CoreResult};

/// The parts of a folder named relative to the project folder: `/`-separated,
/// not absolute, with no empty, `.` or `..` part, and no `\`, `:` or NUL (a
/// name that means something else on another system).
pub fn relative_parts(requested: &str) -> CoreResult<Vec<&str>> {
    if requested.is_empty() {
        return Err(CoreError::Invalid("A chat folder needs a name.".into()));
    }
    if requested.starts_with('/') || Path::new(requested).is_absolute() {
        return Err(CoreError::Invalid(format!(
            "A chat folder is named relative to the project folder, not as an absolute path: {requested}"
        )));
    }
    let parts = requested.split('/').collect::<Vec<_>>();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err(CoreError::Invalid(format!(
            "A chat folder cannot have an empty, `.` or `..` part: {requested}"
        )));
    }
    if parts.iter().any(|part| part.contains(['\\', ':', '\0'])) {
        return Err(CoreError::Invalid(format!(
            "A chat folder's name cannot contain `\\`, `:` or NUL: {requested}"
        )));
    }
    Ok(parts)
}

/// Make the folder `parts` names inside `root` (`mkdir -p`), one part at a
/// time from the canonical root, and return it canonical. A part that is a
/// link must lead to a folder inside `root`; nothing is created through one
/// that leads out, and the folder cannot be `root` itself.
pub fn create_inside(root: &Path, parts: &[&str]) -> CoreResult<PathBuf> {
    let root = canonical_root(root)?;
    let mut current = root.clone();
    for part in parts {
        let next = current.join(part);
        match std::fs::create_dir(&next) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(CoreError::Invalid(format!(
                    "Cannot create the chat folder {}: {error}",
                    next.display()
                )));
            }
        }
        current = inside(&root, &next)?;
    }
    if current == root {
        return Err(CoreError::Invalid(
            "A chat folder cannot be the project folder itself.".into(),
        ));
    }
    Ok(current)
}

/// The existing folder `requested` names inside `root`, canonical: relative
/// to it (as [`relative_parts`] allows) or absolute, as a thread reports its
/// folder. It cannot be `root` itself.
pub fn existing_inside(root: &Path, requested: &str) -> CoreResult<PathBuf> {
    let root = canonical_root(root)?;
    let path = Path::new(requested);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        relative_parts(requested)?
            .iter()
            .fold(root.clone(), |path, part| path.join(part))
    };
    let folder = inside(&root, &candidate)?;
    if folder == root {
        return Err(CoreError::Invalid(
            "A chat folder cannot be the project folder itself.".into(),
        ));
    }
    Ok(folder)
}

/// The working folder of a chat whose recorded folder is `directory`: as
/// recorded while it is a folder inside `root`, made again (an empty one)
/// when someone deleted it, refused when it now leads out of `root`.
pub fn reopen(root: &Path, directory: &str) -> CoreResult<PathBuf> {
    let canonical = canonical_root(root)?;
    let path = Path::new(directory);
    if std::fs::symlink_metadata(path).is_ok() {
        return existing_inside(&canonical, directory);
    }
    let relative = path
        .strip_prefix(&canonical)
        .ok()
        .and_then(Path::to_str)
        .filter(|relative| !relative.is_empty())
        .ok_or_else(|| {
            CoreError::Invalid(format!(
                "This chat's folder {directory} is not in the project folder {}.",
                canonical.display()
            ))
        })?
        .replace(std::path::MAIN_SEPARATOR, "/");
    create_inside(&canonical, &relative_parts(&relative)?)
}

fn canonical_root(root: &Path) -> CoreResult<PathBuf> {
    root.canonicalize().map_err(|error| {
        CoreError::Invalid(format!(
            "Cannot open the project folder {}: {error}",
            root.display()
        ))
    })
}

/// `path` canonical, when it is a folder inside the canonical `root`.
fn inside(root: &Path, path: &Path) -> CoreResult<PathBuf> {
    let canonical = path.canonicalize().map_err(|error| {
        CoreError::Invalid(format!(
            "Cannot open the chat folder {}: {error}",
            path.display()
        ))
    })?;
    if !canonical.starts_with(root) {
        return Err(CoreError::Invalid(format!(
            "The chat folder {} leads out of the project folder.",
            path.display()
        )));
    }
    if !canonical.is_dir() {
        return Err(CoreError::Invalid(format!(
            "The chat folder {} is not a folder.",
            path.display()
        )));
    }
    Ok(canonical)
}

/// `parts` with `-2`, `-3`, … after the last part, for the `n`th choice
/// (the first is `parts` as they are).
#[must_use]
pub fn numbered(parts: &[&str], n: usize) -> Vec<String> {
    let mut owned = parts
        .iter()
        .map(|part| (*part).to_owned())
        .collect::<Vec<_>>();
    if n > 1
        && let Some(last) = owned.last_mut()
    {
        *last = format!("{last}-{n}");
    }
    owned
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Root(PathBuf);
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn root() -> Root {
        let path =
            std::env::temp_dir().join(format!("haarness-chat-folders-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(path.join("project")).unwrap();
        Root(path)
    }

    #[test]
    fn a_folder_name_is_relative_and_plain() {
        assert_eq!(
            relative_parts("2026-10-24/fix-the-tests").unwrap(),
            ["2026-10-24", "fix-the-tests"]
        );
        for refused in [
            "", "/etc", "a//b", "a/", "./a", "a/./b", "../a", "a/../b", "a\\b", "c:x", "a\0b",
        ] {
            assert!(
                matches!(relative_parts(refused), Err(CoreError::Invalid(_))),
                "{refused:?}"
            );
        }
    }

    #[test]
    fn folders_are_made_inside_and_found_again() {
        let root = root();
        let project = root.0.join("project");
        let made = create_inside(&project, &["2026-10-24", "first"]).unwrap();
        assert_eq!(
            made,
            project.canonicalize().unwrap().join("2026-10-24/first")
        );
        assert!(made.is_dir());
        // Making it again is the same folder.
        assert_eq!(
            create_inside(&project, &["2026-10-24", "first"]).unwrap(),
            made
        );
        assert_eq!(existing_inside(&project, "2026-10-24/first").unwrap(), made);
        assert_eq!(
            existing_inside(&project, made.to_str().unwrap()).unwrap(),
            made
        );
        assert!(existing_inside(&project, "2026-10-24/missing").is_err());
        assert!(existing_inside(&project, root.0.to_str().unwrap()).is_err());
        assert!(existing_inside(&project, project.to_str().unwrap()).is_err());
        // Deleted by someone, it is made again, empty.
        std::fs::remove_dir_all(&made).unwrap();
        assert_eq!(reopen(&project, made.to_str().unwrap()).unwrap(), made);
        assert!(made.is_dir());
        assert!(reopen(&project, root.0.join("elsewhere").to_str().unwrap()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_never_leads_a_new_folder_out_of_the_project() {
        let root = root();
        let project = root.0.join("project");
        let outside = root.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, project.join("out")).unwrap();
        std::os::unix::fs::symlink(".", project.join("here")).unwrap();
        std::fs::create_dir_all(project.join("real")).unwrap();
        std::os::unix::fs::symlink("real", project.join("alias")).unwrap();
        assert!(matches!(
            create_inside(&project, &["out", "chat"]),
            Err(CoreError::Invalid(_))
        ));
        assert!(!outside.join("chat").exists(), "nothing made outside");
        assert!(
            create_inside(&project, &["here"]).is_err(),
            "the project itself"
        );
        assert!(existing_inside(&project, "out").is_err());
        assert!(reopen(&project, project.join("out").to_str().unwrap()).is_err());
        // A link to a folder inside is followed to it.
        assert_eq!(
            create_inside(&project, &["alias", "chat"]).unwrap(),
            project.canonicalize().unwrap().join("real/chat")
        );
    }

    #[test]
    fn later_choices_number_the_last_part() {
        assert_eq!(numbered(&["day", "chat"], 1), ["day", "chat"]);
        assert_eq!(numbered(&["day", "chat"], 3), ["day", "chat-3"]);
    }
}
