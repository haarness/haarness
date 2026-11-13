//! The files and folders outside the project that each chat made, so that
//! Auto can tell a chat taking away its own scratch folder (`rm -rf
//! /tmp/bench` after its `mkdir -p /tmp/bench`) from a delete of something
//! nobody named (see `auto_review`).
//!
//! A path is noted when a call is seen to bring it into being: it was not
//! there before the call ran, it is there after, and the system says it was
//! born no earlier than the call started. A tree moved there keeps the birth
//! time it had, so the person's folder moved into place is never the chat's;
//! where the system keeps no birth time, nothing is noted. Each path is kept
//! with what it is (its device, inode and birth time): once something else
//! stands there, because the chat or the person or another program deleted
//! and made it again, the note no longer holds and is dropped. What a delete
//! would take away is looked at whole before the note covers it: anything in
//! it born before the call that made the folder was moved in from somewhere
//! else, and then the folder is not the chat's to take away unasked, nor is
//! one too large to look through.
//!
//! Notes are kept for the chat at the root of the conversation, so what an
//! agent it delegated to made is that chat's too, and in memory only: after
//! a restart nothing is noted, and such a delete comes to a person as
//! before. A folder holds everything in it. A path is kept as a delete
//! reaches it, through the links of the folders above it; a link the chat
//! made is the link itself, so a delete that goes through it (`rm -rf
//! /tmp/l/`) reaches where it leads, which the chat did not make.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use haarness_protocol::ThreadId;

/// How many paths are kept in all; past it the oldest is forgotten first.
const MAX_PATHS: usize = 5000;

/// How many files and folders [`holds`] looks at in what a delete would
/// take away; past it the delete is not the chat's own.
const MAX_LOOKED_AT: usize = 10_000;

/// What stood at a noted path when it was noted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    born: SystemTime,
}

/// A noted path: what stood there, and since when the call that made it
/// ran (to the second).
#[derive(Clone, Copy, Debug)]
struct Noted {
    identity: Identity,
    since: SystemTime,
}

/// What is at `path` now, the path itself (a link is the link), when the
/// system says when it was born.
fn identity(path: &Path) -> Option<Identity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path).ok()?;
        Some(Identity {
            device: metadata.dev(),
            inode: metadata.ino(),
            born: metadata.created().ok()?,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// The paths each chat made, with at most `cap` of them in all.
struct Store {
    by_chat: HashMap<ThreadId, HashMap<PathBuf, Noted>>,
    /// Every kept path in the order it was noted, for `cap`.
    order: VecDeque<(ThreadId, PathBuf)>,
    cap: usize,
}

impl Store {
    fn new(cap: usize) -> Self {
        Self {
            by_chat: HashMap::new(),
            order: VecDeque::new(),
            cap,
        }
    }

    fn insert(&mut self, root: ThreadId, path: PathBuf, noted: Noted) {
        let paths = self.by_chat.entry(root).or_default();
        if paths.insert(path.clone(), noted).is_some() {
            return;
        }
        self.order.push_back((root, path));
        while self.order.len() > self.cap {
            let Some((chat, oldest)) = self.order.pop_front() else {
                break;
            };
            self.remove(chat, |path| path == oldest);
        }
    }

    /// Drops what the chat at `root` holds for which `drop` says so.
    fn remove(&mut self, root: ThreadId, drop: impl Fn(&Path) -> bool) {
        let Some(paths) = self.by_chat.get_mut(&root) else {
            return;
        };
        let before = paths.len();
        paths.retain(|path, _| !drop(path));
        if paths.len() == before {
            return;
        }
        if paths.is_empty() {
            self.by_chat.remove(&root);
        }
        self.order
            .retain(|(chat, path)| *chat != root || !drop(path));
    }

    /// Since when the note covering `target` (the path itself or a folder
    /// above it) holds, when `root`'s chat made one and the same thing still
    /// stands there; a note it no longer matches is dropped.
    fn covering(&mut self, root: ThreadId, target: &Path) -> Option<SystemTime> {
        let paths = self.by_chat.get(&root)?;
        let noted: Vec<(PathBuf, Noted)> = target
            .ancestors()
            .filter_map(|folder| {
                paths
                    .get(folder)
                    .map(|noted| (folder.to_path_buf(), *noted))
            })
            .collect();
        for (folder, noted) in noted {
            if identity(&folder) == Some(noted.identity) {
                return Some(noted.since);
            }
            self.remove(root, |path| path == folder);
        }
        None
    }
}

static MADE: LazyLock<parking_lot::Mutex<Store>> =
    LazyLock::new(|| parking_lot::Mutex::new(Store::new(MAX_PATHS)));

/// What a call may bring into being, looked at before it runs: give it to
/// [`note`] (or [`after_command`]) once the call is done.
#[derive(Debug, Clone)]
pub struct Before {
    /// When the call started.
    start: SystemTime,
    /// The paths it may make that are not there yet (see [`missing`]).
    paths: Vec<PathBuf>,
}

impl Before {
    fn now(paths: Vec<PathBuf>) -> Self {
        Self {
            start: SystemTime::now(),
            paths,
        }
    }

    /// The paths the call may make.
    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
}

/// What a shell command run from `workdir` may make outside `workspace`
/// that is not there yet: call it before the command runs and give what it
/// returns to [`after_command`].
#[must_use]
pub fn before_command(command: &str, workspace: &Path, workdir: &Path) -> Before {
    Before::now(missing(&crate::permissions::bash::creations(
        command, workspace, workdir,
    )))
}

/// After a shell command of the chat at `root` ran: notes what of `before`
/// it made, and forgets what the chat made that the command deleted.
pub fn after_command(
    root: ThreadId,
    command: &str,
    workspace: &Path,
    workdir: &Path,
    before: &Before,
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
/// it would bring into being, as [`missing`] gives them: give them to
/// [`note`] once it wrote them.
#[must_use]
pub fn before_files(files: &[PathBuf]) -> Before {
    let placed: Vec<_> = files
        .iter()
        .filter(|file| file.is_absolute())
        .map(|file| crate::permissions::bash::reached(file))
        .collect();
    Before::now(missing(&placed))
}

/// Of `paths` (absolute, through the links of the folders that are there,
/// as [`crate::permissions::bash::creations`] places them), the ones a call
/// would bring into being: each one that is not there yet, as the topmost
/// folder of it that is missing (`mkdir -p /tmp/a/b` where `/tmp/a` is
/// missing makes `/tmp/a`).
fn missing(paths: &[PathBuf]) -> Vec<PathBuf> {
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

/// Notes the paths of `before` that are there now and were born since the
/// call started as made by the chat at `root`. Never the root folder or a
/// folder right under it, the home folder, the project or a folder above
/// either, nor anything in the project.
pub fn note(root: ThreadId, before: &Before, workspace: &Path) {
    if before.paths.is_empty() {
        return;
    }
    // Birth times may be kept to the second.
    let since = before
        .start
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(SystemTime::UNIX_EPOCH, |elapsed| {
            SystemTime::UNIX_EPOCH + Duration::from_secs(elapsed.as_secs())
        });
    let kept = Kept::new(workspace);
    let made: Vec<_> = before
        .paths
        .iter()
        .filter(|path| !kept.refuses(path))
        .filter_map(|path| {
            identity(path)
                .filter(|identity| identity.born >= since)
                .map(|identity| (path.clone(), Noted { identity, since }))
        })
        .collect();
    let mut store = MADE.lock();
    for (path, noted) in made {
        store.insert(root, path, noted);
    }
}

/// Whether the chat at `root` made `target` (a path as a delete reaches
/// it, every link followed) or a folder above it, the same thing still
/// stands there, and all a delete of `target` takes away was born since the
/// call that made it.
#[must_use]
pub fn holds(root: ThreadId, target: &Path) -> bool {
    let Some(since) = MADE.lock().covering(root, target) else {
        return false;
    };
    born_since(target, since, MAX_LOOKED_AT)
}

/// Whether everything at and under `path`, links not followed, was born no
/// earlier than `since` and no later than now, and last changed no earlier
/// than `since`: within `limit` entries, each with a birth time the system
/// keeps. A move from another disk is a copy born on arrival, but `mv`,
/// `cp -p` and `tar -x` keep the time it was last changed. It may look
/// through a lot of entries: call it off the async workers.
fn born_since(path: &Path, since: SystemTime, limit: usize) -> bool {
    let now = SystemTime::now();
    let mut stack = vec![path.to_path_buf()];
    let mut looked = 0;
    while let Some(path) = stack.pop() {
        looked += 1;
        if looked > limit {
            return false;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            // Gone already: nothing there to take away.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return false,
        };
        if metadata
            .created()
            .map_or(true, |born| born < since || born > now)
            || metadata.modified().map_or(true, |changed| changed < since)
        {
            return false;
        }
        if metadata.is_dir() {
            let Ok(entries) = std::fs::read_dir(&path) else {
                return false;
            };
            for entry in entries {
                let Ok(entry) = entry else {
                    return false;
                };
                stack.push(entry.path());
            }
        }
    }
    true
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
    MADE.lock()
        .remove(root, |path| gone.iter().any(|gone| path.starts_with(gone)));
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

    /// Whether the system keeps birth times where tests make their files:
    /// without them nothing is ever noted.
    fn births(folder: &Path) -> bool {
        identity(folder).is_some()
    }

    #[test]
    fn a_chat_holds_what_it_made_below_it_and_nothing_else() {
        let root = std::env::temp_dir().join(format!("haarness-made-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let root = crate::paths::canonicalize(&root).unwrap();
        let chat = ThreadId::new();
        let other = ThreadId::new();
        let scratch = root.join("scratch");
        let before = Before::now(missing(&[scratch.join("a/b"), root.join("project/inside")]));
        assert_eq!(
            before.paths,
            vec![scratch.clone(), root.join("project/inside")],
            "the topmost missing folder of each"
        );
        std::fs::create_dir_all(scratch.join("a/b")).unwrap();
        std::fs::create_dir_all(root.join("project/inside")).unwrap();
        note(chat, &before, &workspace);
        if !births(&scratch) {
            // No birth time, no note.
            assert!(!any(chat));
            std::fs::remove_dir_all(root).unwrap();
            return;
        }
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
        note(
            other,
            &Before::now(missing(&[root.join("never")])),
            &workspace,
        );
        assert!(!any(other));
        // Taken away, it is forgotten.
        std::fs::remove_dir_all(&scratch).unwrap();
        forget(chat, std::slice::from_ref(&scratch));
        assert!(!holds(chat, &scratch.join("a")));
        assert!(!any(chat));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn what_was_born_before_the_call_or_stands_there_since_is_not_the_chat_s() {
        let root = std::env::temp_dir().join(format!("haarness-born-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let root = crate::paths::canonicalize(&root).unwrap();
        let chat = ThreadId::new();
        // The person's folder, born before the call, moved where the call
        // then "makes" it: never the chat's.
        let theirs = root.join("theirs");
        std::fs::create_dir_all(&theirs).unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        let moved = root.join("moved");
        let before = Before::now(missing(std::slice::from_ref(&moved)));
        assert_eq!(before.paths, vec![moved.clone()]);
        std::fs::rename(&theirs, &moved).unwrap();
        note(chat, &before, &workspace);
        assert!(!holds(chat, &moved));
        // The chat's own folder, deleted and made again by someone else:
        // the note no longer holds, and is dropped.
        let own = root.join("own");
        let before = Before::now(missing(std::slice::from_ref(&own)));
        std::fs::create_dir(&own).unwrap();
        note(chat, &before, &workspace);
        if births(&own) {
            assert!(holds(chat, &own));
            std::fs::remove_dir(&own).unwrap();
            std::fs::create_dir(root.join("elsewhere")).unwrap();
            std::fs::create_dir(&own).unwrap();
            std::fs::write(own.join("person.txt"), "x").unwrap();
            assert!(!holds(chat, &own.join("person.txt")));
            assert!(!any(chat));
        } else {
            assert!(!any(chat));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_folder_holding_what_was_moved_in_or_too_much_to_look_through_is_not_the_chat_s() {
        let root = std::env::temp_dir().join(format!("haarness-moved-in-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        let root = crate::paths::canonicalize(&root).unwrap();
        let theirs = root.join("theirs");
        std::fs::create_dir_all(theirs.join("deep")).unwrap();
        std::fs::write(theirs.join("deep/notes.txt"), "x").unwrap();
        if !births(&theirs) {
            std::fs::remove_dir_all(root).unwrap();
            return;
        }
        std::thread::sleep(Duration::from_millis(1100));
        let chat = ThreadId::new();
        let own = root.join("box");
        let before = Before::now(missing(std::slice::from_ref(&own)));
        std::fs::create_dir(&own).unwrap();
        std::fs::write(own.join("made.txt"), "x").unwrap();
        note(chat, &before, &workspace);
        assert!(holds(chat, &own));
        // A later call moves the person's older folder in.
        std::fs::rename(&theirs, own.join("theirs")).unwrap();
        assert!(!holds(chat, &own));
        assert!(!holds(chat, &own.join("theirs/deep/notes.txt")));
        // What the chat made beside it is still its own.
        assert!(holds(chat, &own.join("made.txt")));
        // A file given an older time (`mv` from another disk, `cp -p`,
        // `tar -x`) is not the chat's to take away either.
        let kept = own.join("kept-time.txt");
        std::fs::write(&kept, "x").unwrap();
        assert!(holds(chat, &kept));
        std::fs::File::options()
            .write(true)
            .open(&kept)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000))
            .unwrap();
        assert!(!holds(chat, &kept));
        // Nor is one born later than now, as a clock set wrong may make it.
        #[cfg(target_os = "macos")]
        {
            use std::os::macos::fs::FileTimesExt;
            let later = own.join("later.txt");
            std::fs::write(&later, "x").unwrap();
            let future = SystemTime::now() + Duration::from_secs(3600);
            std::fs::File::options()
                .write(true)
                .open(&later)
                .unwrap()
                .set_times(
                    std::fs::FileTimes::new()
                        .set_modified(future)
                        .set_created(future),
                )
                .unwrap();
            assert!(!holds(chat, &later));
        }
        // Past the limit of what is looked at, nothing is.
        let since = SystemTime::UNIX_EPOCH;
        assert!(born_since(&own.join("made.txt"), since, 1));
        assert!(!born_since(&own, since, 2));
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
        let folder = std::env::temp_dir().join(format!("haarness-made-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&folder).unwrap();
        let Some(identity) = identity(&folder) else {
            std::fs::remove_dir_all(folder).unwrap();
            return;
        };
        let identity = Noted {
            identity,
            since: SystemTime::UNIX_EPOCH,
        };
        let mut store = Store::new(3);
        let (one, two) = (ThreadId::new(), ThreadId::new());
        for index in 0..5 {
            let chat = if index % 2 == 0 { one } else { two };
            store.insert(chat, folder.join(format!("f{index}")), identity);
        }
        assert_eq!(store.order.len(), 3);
        let kept: usize = store.by_chat.values().map(HashMap::len).sum();
        assert_eq!(kept, 3);
        // The oldest go first, whichever chat made them.
        assert!(!store.by_chat[&one].contains_key(&folder.join("f0")));
        assert!(
            !store
                .by_chat
                .get(&two)
                .is_some_and(|paths| paths.contains_key(&folder.join("f1")))
        );
        assert!(store.by_chat[&one].contains_key(&folder.join("f4")));
        std::fs::remove_dir_all(folder).unwrap();
    }
}
