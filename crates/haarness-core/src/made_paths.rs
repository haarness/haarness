//! The files and folders outside the project that each chat made, so that
//! Auto can tell a chat taking away its own scratch folder (`rm -rf
//! /tmp/bench` after its `mkdir -p /tmp/bench`) from a delete of something
//! nobody named (see `auto_review`).
//!
//! A path is noted when a call is seen to bring it into being: it was not
//! there before the call ran and is there after. It is kept for the chat at
//! the root of the conversation, so what an agent it delegated to made is
//! that chat's too, and in memory only: after a restart nothing is noted, and
//! such a delete comes to a person as before. A folder holds everything in
//! it. A path is kept as a delete reaches it, through the links of the
//! folders above it; a link the chat made is the link itself, so a delete
//! that goes through it (`rm -rf /tmp/l/`) reaches where it leads, which the
//! chat did not make. Once a call of the chat has taken a path away it is
//! forgotten, so something made there later by anyone else is not the
//! chat's.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use haarness_protocol::ThreadId;

/// How many paths are kept in all; past it the oldest is forgotten first.
const MAX_PATHS: usize = 5000;

#[derive(Default)]
struct Made {
    by_chat: HashMap<ThreadId, HashSet<PathBuf>>,
    /// Every kept path in the order it was noted, for [`MAX_PATHS`].
    order: VecDeque<(ThreadId, PathBuf)>,
}

static MADE: LazyLock<parking_lot::Mutex<Made>> = LazyLock::new(Default::default);

/// What a shell command run from `workdir` may make outside `workspace`
/// that is not there yet (see [`missing`]): call it before the command runs
/// and give what it returns to [`after_command`].
#[must_use]
pub fn before_command(command: &str, workspace: &Path, workdir: &Path) -> Vec<PathBuf> {
    missing(&crate::permissions::bash::creations(
        command, workspace, workdir,
    ))
}

/// After a shell command of the chat at `root` ran: notes what of `before`
/// it made, and forgets what the chat made that the command deleted.
pub fn after_command(
    root: ThreadId,
    command: &str,
    workspace: &Path,
    workdir: &Path,
    before: &[PathBuf],
) {
    note(root, before, workspace);
    if !any(root) {
        return;
    }
    let gone: Vec<PathBuf> =
        crate::permissions::bash::outside_deletions(command, workspace, Some(workdir))
            .into_iter()
            .filter(|target| {
                target.placed && target.effect == crate::permissions::bash::Effect::Deletes
            })
            .map(|target| PathBuf::from(target.path))
            .collect();
    forget(root, &gone);
}

/// Of the files a tool is about to write (`write`, `apply_patch`), the ones
/// it would bring into being outside the project, as [`missing`] gives them:
/// give them to [`note`] once it wrote them.
#[must_use]
pub fn before_files(files: &[PathBuf]) -> Vec<PathBuf> {
    let placed: Vec<_> = files
        .iter()
        .filter(|file| file.is_absolute())
        .map(|file| crate::permissions::bash::reached(file))
        .collect();
    missing(&placed)
}

/// Of `paths` (absolute, through the links of the folders that are there,
/// as [`crate::permissions::bash::creations`] places them), the ones a call
/// would bring into being: each one that is not there yet, as the topmost
/// folder of it that is missing (`mkdir -p /tmp/a/b` where `/tmp/a` is
/// missing makes `/tmp/a`). Call it before the call runs, and give what it
/// returns to [`note`] after.
#[must_use]
pub fn missing(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for path in paths {
        if !path.is_absolute() || std::fs::symlink_metadata(path).is_ok() {
            continue;
        }
        let mut top = path.as_path();
        while let Some(up) = top.parent() {
            if std::fs::symlink_metadata(up).is_ok() {
                break;
            }
            top = up;
        }
        if top.parent().is_some() && !found.iter().any(|known: &PathBuf| known == top) {
            found.push(top.to_path_buf());
        }
    }
    found
}

/// Notes the paths of `made` (from [`missing`]) that are there now as made
/// by the chat at `root`. Never the root folder or a folder right under it,
/// the home folder, the project or a folder above either, nor anything in
/// the project.
pub fn note(root: ThreadId, made: &[PathBuf], workspace: &Path) {
    let kept = Kept::new(workspace);
    let made: Vec<_> = made
        .iter()
        .filter(|path| std::fs::symlink_metadata(path).is_ok() && !kept.refuses(path))
        .collect();
    if made.is_empty() {
        return;
    }
    let mut store = MADE.lock();
    for path in made {
        if !store.by_chat.entry(root).or_default().insert(path.clone()) {
            continue;
        }
        store.order.push_back((root, path.clone()));
        while store.order.len() > MAX_PATHS {
            let Some((chat, oldest)) = store.order.pop_front() else {
                break;
            };
            if let Some(paths) = store.by_chat.get_mut(&chat) {
                paths.remove(&oldest);
                if paths.is_empty() {
                    store.by_chat.remove(&chat);
                }
            }
        }
    }
}

/// Whether the chat at `root` made `target` (a path as a delete reaches
/// it, every link followed) or a folder above it.
#[must_use]
pub fn holds(root: ThreadId, target: &Path) -> bool {
    let store = MADE.lock();
    let Some(paths) = store.by_chat.get(&root) else {
        return false;
    };
    target.ancestors().any(|folder| paths.contains(folder))
}

/// Forgets what the chat at `root` made at or under each of `gone` that is
/// no longer there: a path it deleted.
pub fn forget(root: ThreadId, gone: &[PathBuf]) {
    let gone: Vec<_> = gone
        .iter()
        .filter(|path| std::fs::symlink_metadata(path).is_err())
        .collect();
    if gone.is_empty() {
        return;
    }
    let mut store = MADE.lock();
    let Some(paths) = store.by_chat.get_mut(&root) else {
        return;
    };
    paths.retain(|path| !gone.iter().any(|gone| path.starts_with(gone)));
    if paths.is_empty() {
        store.by_chat.remove(&root);
    }
    store
        .order
        .retain(|(chat, path)| *chat != root || !gone.iter().any(|gone| path.starts_with(gone)));
}

/// Whether the chat at `root` has made anything that is kept.
#[must_use]
pub fn any(root: ThreadId) -> bool {
    MADE.lock().by_chat.contains_key(&root)
}

/// The folders a chat never owns: the root and the folders right under it,
/// the home folder, the project, the folders above them, and anything in
/// the project, each as written and through its links.
struct Kept {
    above: Vec<PathBuf>,
    project: Vec<PathBuf>,
}

impl Kept {
    fn new(workspace: &Path) -> Self {
        let both = |folder: &Path| {
            let mut spellings = vec![folder.to_path_buf()];
            if let Ok(real) = crate::paths::canonicalize(folder) {
                spellings.push(real);
            }
            spellings
        };
        let project = both(workspace);
        let mut above = project.clone();
        if let Some(home) = haarness_protocol::home_dir() {
            above.extend(both(&home));
        }
        Self { above, project }
    }

    fn refuses(&self, path: &Path) -> bool {
        path.components().count() < 3
            || self.above.iter().any(|folder| folder.starts_with(path))
            || self.project.iter().any(|project| path.starts_with(project))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_holds_what_it_made_below_it_and_nothing_else() {
        let root = std::env::temp_dir().join(format!("haarness-made-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let root = crate::paths::canonicalize(&root).unwrap();
        let chat = ThreadId::new();
        let other = ThreadId::new();
        let scratch = root.join("scratch");
        let made = missing(&[scratch.join("a/b"), root.join("project/inside")]);
        assert_eq!(
            made,
            vec![scratch.clone(), root.join("project/inside")],
            "the topmost missing folder of each"
        );
        std::fs::create_dir_all(scratch.join("a/b")).unwrap();
        std::fs::create_dir_all(root.join("project/inside")).unwrap();
        note(chat, &made, &workspace);
        assert!(holds(chat, &scratch));
        assert!(holds(chat, &scratch.join("a/b/c")));
        // Another chat, the folder above it, and anything in the project
        // are not the chat's.
        assert!(!holds(other, &scratch));
        assert!(!holds(chat, &root));
        assert!(!holds(chat, &root.join("project/inside")));
        // A path that was there before the call is never noted.
        assert!(missing(std::slice::from_ref(&scratch)).is_empty());
        // Nor is one the call did not make after all.
        note(other, &missing(&[root.join("never")]), &workspace);
        assert!(!any(other));
        // Taken away, it is forgotten.
        std::fs::remove_dir_all(&scratch).unwrap();
        forget(chat, std::slice::from_ref(&scratch));
        assert!(!holds(chat, &scratch.join("a")));
        assert!(!any(chat));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_root_the_home_folder_the_project_and_their_parents_are_never_noted() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-made-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&workspace).unwrap();
        let kept = Kept::new(&workspace);
        assert!(kept.refuses(Path::new("/")));
        assert!(kept.refuses(Path::new("/tmp")));
        assert!(kept.refuses(workspace.parent().unwrap()));
        assert!(kept.refuses(&workspace));
        assert!(kept.refuses(&workspace.join("src")));
        if let Some(home) = haarness_protocol::home_dir() {
            assert!(kept.refuses(&home));
            assert!(!kept.refuses(&home.join("scratch-folder")));
        }
        assert!(!kept.refuses(&std::env::temp_dir().join("haarness-made-scratch")));
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn the_paths_kept_are_bounded() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-made-{}", uuid::Uuid::now_v7()));
        let scratch = workspace.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let project = workspace.join("project");
        let chat = ThreadId::new();
        // The store is shared by every test: count only this chat's.
        let files: Vec<_> = (0..MAX_PATHS + 10)
            .map(|index| scratch.join(format!("f{index}")))
            .collect();
        for file in &files {
            std::fs::write(file, "").unwrap();
        }
        note(chat, &files, &project);
        let kept = MADE.lock().by_chat.get(&chat).map_or(0, HashSet::len);
        assert!(kept <= MAX_PATHS, "{kept}");
        assert!(MADE.lock().order.len() <= MAX_PATHS);
        assert!(!holds(chat, &files[0]), "the oldest goes first");
        assert!(holds(chat, files.last().unwrap()));
        std::fs::remove_dir_all(workspace).unwrap();
    }
}
