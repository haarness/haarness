//! The paths a glob pattern matches, found as the `glob` crate finds them
//! (the same paths, in the same order) except where `glob` would never end.
//! `glob` walks a `**` into every linked folder with no record of where it
//! has been, so a link back up the tree (`handbook/team/up -> ..`) keeps it
//! walking for good.
//!
//! Here a `**` follows a linked folder, as `glob` does, unless the link leads
//! back into a folder the walk is inside: one that holds the link, or one the
//! walk came through to reach it. With [`Links::Once`] it also never reads a
//! folder again for the same `**` of the pattern after reading it by another
//! path (what a `**` finds below a folder depends only on the folder, while
//! another `**` of the pattern looks for something else). A `**` does not go into a
//! version control folder (`.git`, `.hg`, …). A linked folder the pattern
//! names, or that a one-level wildcard such as `*` matches, is read as
//! before (the pattern bounds how deep that goes), and a linked file matches
//! as before. A walk can also end after a budget of folder entries, keeping
//! what it has already listed, or when its caller says so; [`Walk::cut`]
//! says which.

use glob::{MatchOptions, Pattern, PatternError};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Folders a `**` leaves alone: a version control system's own records.
const VERSION_CONTROL: [&str; 7] = [".git", ".hg", ".svn", ".bzr", ".jj", "_darcs", "CVS"];

/// Which linked folders a `**` goes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Links {
    /// Every one that does not lead back into the walk, so a folder is
    /// listed by each path that reaches it.
    Unlooped,
    /// Those, less any whose folder the walk has already read (by its real
    /// path) for the same `**`, so each folder is read once for each, by the
    /// first path that reaches it.
    Once,
}

/// Why a walk ended before it read everything its pattern reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    /// It read as many folder entries as [`Walk::entries`] allows; what it
    /// had listed by then is still matched.
    Large,
    /// The caller's [`Walk::stop_when`] said to stop.
    Stopped,
}

/// One component of the pattern.
struct Part {
    pattern: Pattern,
    /// No wildcard as `glob` reads it: the entry is looked up, not listed.
    plain: bool,
    /// `**`.
    recursive: bool,
}

/// A path met on the way.
struct Entry {
    path: PathBuf,
    folder: bool,
    /// A link, as its folder listing said; a path the pattern names is
    /// looked up through any link and never counts as one.
    link: bool,
    /// Where it really is, when known without asking the file system.
    real: Option<PathBuf>,
}

impl Entry {
    fn named(path: PathBuf) -> Self {
        let folder = fs::metadata(&path).is_ok_and(|metadata| metadata.is_dir());
        Self {
            path,
            folder,
            link: false,
            real: None,
        }
    }
}

/// A walk of one pattern; an iterator over the paths it matches, folders
/// included.
pub struct Walk {
    parts: Vec<Part>,
    /// Where the walk starts, until the first [`Iterator::next`].
    start: Option<Entry>,
    require_folder: bool,
    entries: usize,
    links: Links,
    stop: Option<Box<dyn Fn() -> bool + Send>>,
    looked: usize,
    cut: Option<Cut>,
    /// Paths still to look at: each with the part it meets next, or `None`
    /// once it matched the whole pattern.
    todo: Vec<(Entry, Option<usize>)>,
    /// Where each folder a link check asked about really is.
    real: HashMap<PathBuf, Option<PathBuf>>,
    /// The real paths of the folders read, each with the part it was read
    /// for, for [`Links::Once`].
    read: HashSet<(PathBuf, usize)>,
}

impl Walk {
    /// A walk of `pattern`, or the error `glob::glob` gives for it.
    pub fn new(pattern: &str) -> Result<Self, PatternError> {
        Pattern::new(pattern)?;
        let root_len = root_len(pattern);
        let root = &pattern[..root_len];
        let mut parts = Vec::new();
        if !verbatim(root) {
            for text in pattern[root_len..].split_terminator(std::path::is_separator) {
                parts.push(Part {
                    pattern: Pattern::new(text)?,
                    plain: !text.contains(['*', '?', '[']),
                    recursive: text == "**",
                });
            }
            if root_len == pattern.len() {
                parts.push(Part {
                    pattern: Pattern::default(),
                    plain: true,
                    recursive: false,
                });
            }
        }
        Ok(Self {
            parts,
            start: Some(Entry::named(PathBuf::from(if root.is_empty() {
                "."
            } else {
                root
            }))),
            require_folder: pattern.ends_with(std::path::is_separator),
            entries: usize::MAX,
            links: Links::Unlooped,
            stop: None,
            looked: 0,
            cut: None,
            todo: Vec::new(),
            real: HashMap::new(),
            read: HashSet::new(),
        })
    }

    /// Reads no more folders once `entries` folder entries have been read
    /// ([`Cut::Large`]).
    #[must_use]
    pub fn entries(mut self, entries: usize) -> Self {
        self.entries = entries;
        self
    }

    #[must_use]
    pub fn links(mut self, links: Links) -> Self {
        self.links = links;
        self
    }

    /// Ends before reading another folder once `stop` says so
    /// ([`Cut::Stopped`]).
    #[must_use]
    pub fn stop_when(mut self, stop: impl Fn() -> bool + Send + 'static) -> Self {
        self.stop = Some(Box::new(stop));
        self
    }

    /// Why the walk ended early, when it did.
    pub fn cut(&self) -> Option<Cut> {
        self.cut
    }

    /// A `**` going into `entry`'s folder, unless it is one the walk leaves.
    fn descend(&mut self, at: usize, entry: &Entry) {
        if entry
            .path
            .file_name()
            .is_some_and(|name| VERSION_CONTROL.iter().any(|folder| name == *folder))
            || (entry.link && self.leads_back(&entry.path))
            || (self.links == Links::Once
                && real_path(entry).is_some_and(|real| self.read.contains(&(real, at))))
        {
            return;
        }
        self.fill(at, entry);
    }

    /// Whether the folder the link at `link` leads to holds the link, or a
    /// folder the walk came through to reach it.
    fn leads_back(&mut self, link: &Path) -> bool {
        let Ok(target) = fs::canonicalize(link) else {
            return true;
        };
        link.ancestors().skip(1).any(|folder| {
            self.real
                .entry(folder.to_path_buf())
                .or_insert_with(|| fs::canonicalize(folder).ok())
                .as_ref()
                .is_some_and(|real| real.starts_with(&target))
        })
    }

    /// Queues what part `at` finds from `entry`, as `glob` does.
    fn fill(&mut self, at: usize, entry: &Entry) {
        let part = &self.parts[at];
        let here = entry.path == Path::new(".");
        if part.plain {
            let name = part.pattern.as_str();
            let special = name == "." || name == "..";
            let next = if here {
                PathBuf::from(name)
            } else {
                entry.path.join(name)
            };
            if (special && entry.folder)
                || (!special
                    && (fs::metadata(&next).is_ok() || fs::symlink_metadata(&next).is_ok()))
            {
                self.add(at, Entry::named(next));
            }
            return;
        }
        if !entry.folder {
            return;
        }
        if self.stop.as_ref().is_some_and(|stop| stop()) {
            self.cut = Some(Cut::Stopped);
            return;
        }
        if self.looked >= self.entries {
            self.cut = Some(Cut::Large);
            return;
        }
        let real = match self.links {
            Links::Once => real_path(entry),
            Links::Unlooped => None,
        };
        // A listing with an entry that cannot be read gives nothing.
        let Ok(mut children) = fs::read_dir(&entry.path).and_then(|listing| {
            listing
                .map(|child| {
                    child.map(|child| {
                        let name = child.file_name();
                        let path = if here {
                            PathBuf::from(&name)
                        } else {
                            child.path()
                        };
                        let kind = child.file_type().ok();
                        let link = kind.is_some_and(|kind| kind.is_symlink());
                        let folder = match kind {
                            Some(kind) if !link => kind.is_dir(),
                            _ => fs::metadata(&path).is_ok_and(|metadata| metadata.is_dir()),
                        };
                        let real = real.as_ref().filter(|_| !link).map(|real| real.join(&name));
                        let entry = Entry {
                            path,
                            folder,
                            link,
                            real,
                        };
                        (entry, name)
                    })
                })
                .collect::<std::io::Result<Vec<(Entry, OsString)>>>()
        }) else {
            return;
        };
        if let Some(real) = real {
            self.read.insert((real, at));
        }
        self.looked += children.len();
        // Taken from the end, so in name order.
        children.sort_by(|first, second| second.1.cmp(&first.1));
        self.todo
            .extend(children.into_iter().map(|(child, _)| (child, Some(at))));
        // A part that starts with `.` also matches `.` and `..`.
        if self.parts[at].pattern.as_str().starts_with('.') {
            for special in [".", ".."] {
                if self.parts[at]
                    .pattern
                    .matches_with(special, MatchOptions::new())
                {
                    self.add(at, Entry::named(entry.path.join(special)));
                }
            }
        }
    }

    fn add(&mut self, at: usize, next: Entry) {
        if at + 1 == self.parts.len() {
            self.todo.push((next, None));
        } else {
            self.fill(at + 1, &next);
        }
    }
}

impl Iterator for Walk {
    type Item = PathBuf;

    fn next(&mut self) -> Option<PathBuf> {
        if let Some(start) = self.start.take()
            && !self.parts.is_empty()
        {
            self.fill(0, &start);
        }
        let last = self.parts.len().saturating_sub(1);
        loop {
            if self.cut == Some(Cut::Stopped) {
                return None;
            }
            let (entry, at) = self.todo.pop()?;
            let Some(mut at) = at else {
                if !self.require_folder || entry.folder {
                    return Some(entry.path);
                }
                continue;
            };
            if self.parts[at].recursive {
                let mut next = at;
                while next < last && self.parts[next + 1].recursive {
                    next += 1;
                }
                if entry.folder {
                    self.descend(next, &entry);
                    if next == last {
                        return Some(entry.path);
                    }
                    at = next + 1;
                } else if next == last {
                    continue;
                } else {
                    at = next + 1;
                }
            }
            let Some(name) = entry.path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if self.parts[at]
                .pattern
                .matches_with(name, MatchOptions::new())
            {
                if at == last {
                    if !self.require_folder || entry.folder {
                        return Some(entry.path);
                    }
                } else {
                    self.fill(at + 1, &entry);
                }
            }
        }
    }
}

/// Where `entry` really is.
fn real_path(entry: &Entry) -> Option<PathBuf> {
    entry
        .real
        .clone()
        .or_else(|| fs::canonicalize(&entry.path).ok())
}

/// How much of `pattern` is its root (`/`, or a drive on Windows), as
/// `glob` measures it.
fn root_len(pattern: &str) -> usize {
    let mut components = Path::new(pattern).components().peekable();
    while matches!(
        components.peek(),
        Some(Component::Prefix(_) | Component::RootDir)
    ) {
        components.next();
    }
    let rest: PathBuf = components.map(Component::as_os_str).collect();
    let whole: PathBuf = Path::new(pattern).iter().collect();
    whole
        .to_string_lossy()
        .len()
        .saturating_sub(rest.to_string_lossy().len())
}

/// A Windows root `glob` matches nothing under: a verbatim one other than a
/// drive's (`\\?\UNC\…`).
#[cfg(windows)]
fn verbatim(root: &str) -> bool {
    match Path::new(root).components().next() {
        Some(Component::Prefix(prefix)) => {
            prefix.kind().is_verbatim()
                && !matches!(prefix.kind(), std::path::Prefix::VerbatimDisk(_))
        }
        _ => false,
    }
}

#[cfg(not(windows))]
fn verbatim(_root: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str, files: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "haarness-bounded-glob-{name}-{}",
            uuid::Uuid::now_v7()
        ));
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, file).unwrap();
        }
        root
    }

    fn globbed(pattern: &str) -> Vec<PathBuf> {
        glob::glob(pattern)
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    }

    fn walked(pattern: &str, links: Links) -> Vec<PathBuf> {
        Walk::new(pattern).unwrap().links(links).collect()
    }

    #[test]
    fn an_ordinary_tree_gives_what_glob_gives_in_its_order() {
        let root = tree(
            "ordinary",
            &[
                "handbook [x]*?/RULES.md",
                "handbook [x]*?/a.md",
                "handbook [x]*?/abc.md",
                "handbook [x]*?/aXc.md",
                "handbook [x]*?/b.txt",
                "handbook [x]*?/c.md",
                "handbook [x]*?/.hidden.md",
                "handbook [x]*?/.config/x.md",
                "handbook [x]*?/style/tone.md",
                "handbook [x]*?/style/b.md",
                "handbook [x]*?/style/guide/review.md",
                "handbook [x]*?/b/style/x.md",
                "handbook [x]*?/team/RULES.md",
                "handbook [x]*?/team/oncall/RULES.md",
                "handbook [x]*?/1/2/3/4/5/6/7/8/9/10/RULES.md",
            ],
        );
        let handbook = root.join("handbook [x]*?");
        fs::create_dir_all(handbook.join("empty")).unwrap();
        let escaped = Pattern::escape(&handbook.to_string_lossy());
        for pattern in [
            "**/*.md",
            "style/*.md",
            "*/RULES.md",
            "**/style/*.md",
            "**/RULES.md",
            "**/guide/**/*.md",
            ".*",
            "**",
            "**/",
            "*",
            "a?c.md",
            "[ab]*.md",
            "[!a]*.md",
            "**/.config/*.md",
            "style/**",
            "missing/*.md",
            "RULES.md",
            "1/**/RULES.md",
        ] {
            let pattern = format!("{escaped}/{pattern}");
            for links in [Links::Unlooped, Links::Once] {
                let mut walk = Walk::new(&pattern).unwrap().links(links);
                assert_eq!(
                    walk.by_ref().collect::<Vec<_>>(),
                    globbed(&pattern),
                    "{pattern}"
                );
                assert_eq!(walk.cut(), None, "{pattern}");
            }
        }
        // A pattern glob refuses is refused the same way.
        let refused = format!("{escaped}/a***");
        assert_eq!(
            Walk::new(&refused).err().map(|error| error.to_string()),
            glob::glob(&refused).err().map(|error| error.to_string())
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn linked_folders_that_do_not_lead_back_are_followed() {
        let root = tree(
            "links",
            &[
                "project/src/main.rs",
                "project/docs/guide.md",
                "shared/lib.rs",
            ],
        );
        let project = root.join("project");
        std::os::unix::fs::symlink("../docs", project.join("src/docs")).unwrap();
        std::os::unix::fs::symlink("../shared", project.join("shared")).unwrap();
        std::os::unix::fs::symlink("../shared/lib.rs", project.join("lib.rs")).unwrap();
        for pattern in ["**/*", "**/*.md", "src/**", "*/*.rs"] {
            let pattern = format!("{}/{pattern}", project.display());
            assert_eq!(
                walked(&pattern, Links::Unlooped),
                globbed(&pattern),
                "{pattern}"
            );
        }
        // Read once: a folder another path already read is not read again,
        // while a linked folder elsewhere is.
        let pattern = format!("{}/**/*.*", project.display());
        assert_eq!(
            walked(&pattern, Links::Once),
            [
                project.join("docs/guide.md"),
                project.join("lib.rs"),
                project.join("shared/lib.rs"),
                project.join("src/main.rs"),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Runs `walk` on a thread of its own, so a walk that never ends fails
    /// the test instead of holding it.
    fn ends(walk: Walk) -> Vec<PathBuf> {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(walk.collect::<Vec<_>>());
        });
        receiver
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the walk ends")
    }

    #[cfg(unix)]
    #[test]
    fn links_back_up_the_tree_end_the_walk_instead_of_looping() {
        let root = tree("loops", &["top.md", "a/note.md", "p/x.md", "q/y.md"]);
        std::os::unix::fs::symlink("..", root.join("a/up")).unwrap();
        std::os::unix::fs::symlink(".", root.join("a/self")).unwrap();
        // Two links that lead to each other's folder, and one above the walk.
        std::os::unix::fs::symlink("../q", root.join("p/to-q")).unwrap();
        std::os::unix::fs::symlink("../p", root.join("q/to-p")).unwrap();
        std::os::unix::fs::symlink(root.parent().unwrap(), root.join("a/above")).unwrap();
        let pattern = format!("{}/**/*.md", root.display());
        // Each folder by each path that does not go round.
        assert_eq!(
            ends(Walk::new(&pattern).unwrap()),
            [
                root.join("a/note.md"),
                root.join("p/to-q/y.md"),
                root.join("p/x.md"),
                root.join("q/to-p/x.md"),
                root.join("q/y.md"),
                root.join("top.md"),
            ]
        );
        // Each folder once.
        assert_eq!(
            ends(Walk::new(&pattern).unwrap().links(Links::Once)),
            [
                root.join("a/note.md"),
                root.join("p/to-q/y.md"),
                root.join("p/x.md"),
                root.join("top.md"),
            ]
        );
        // With no match at all, it ends just the same.
        let none = format!("{}/**/*.nothing", root.display());
        assert!(ends(Walk::new(&none).unwrap()).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_read_through_a_link_is_read_again_for_another_double_star() {
        let root = tree(
            "two-stars",
            &["handbook/teams/web/RULES.md", "handbook/RULES.md"],
        );
        let handbook = root.join("handbook");
        // Read first through the link, for the first `**`.
        std::os::unix::fs::symlink("teams/web", handbook.join("aaa-web")).unwrap();
        let pattern = format!("{}/**/teams/**/RULES.md", handbook.display());
        assert_eq!(walked(&pattern, Links::Once), globbed(&pattern));
        assert_eq!(
            walked(&pattern, Links::Once),
            [handbook.join("teams/web/RULES.md")]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_double_star_leaves_version_control_folders_alone() {
        let root = tree(
            "vcs",
            &[
                ".git/RULES.md",
                "team/.hg/RULES.md",
                "team/RULES.md",
                "RULES.md",
            ],
        );
        let pattern = format!("{}/**/RULES.md", root.display());
        for links in [Links::Unlooped, Links::Once] {
            assert_eq!(
                walked(&pattern, links),
                [root.join("RULES.md"), root.join("team/RULES.md")]
            );
        }
        // Named, it is read as any folder.
        let named = format!("{}/.git/*.md", root.display());
        assert_eq!(walked(&named, Links::Once), [root.join(".git/RULES.md")]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rules_are_found_beside_a_large_folder() {
        let root = tree(
            "budget",
            &["RULES.md", "team/RULES.md", "team/oncall/RULES.md"],
        );
        fs::create_dir_all(root.join(".cache")).unwrap();
        for index in 0..11_000 {
            fs::write(root.join(format!(".cache/{index:05}.bin")), "").unwrap();
        }
        let pattern = format!("{}/**/RULES.md", root.display());
        let all = [
            root.join("RULES.md"),
            root.join("team/RULES.md"),
            root.join("team/oncall/RULES.md"),
        ];
        // A budget well above the large folder, as a safety net.
        let mut walk = Walk::new(&pattern)
            .unwrap()
            .links(Links::Once)
            .entries(100_000);
        assert_eq!(walk.by_ref().collect::<Vec<_>>(), all);
        assert_eq!(walk.cut(), None);
        // A budget the large folder spends: what was already listed still
        // matches, and the walk says it was cut.
        let mut walk = Walk::new(&pattern)
            .unwrap()
            .links(Links::Once)
            .entries(5_000);
        assert_eq!(walk.by_ref().collect::<Vec<_>>(), all[..1]);
        assert_eq!(walk.cut(), Some(Cut::Large));
        let mut stopped = Walk::new(&pattern).unwrap().stop_when(|| true);
        assert_eq!(stopped.by_ref().count(), 0);
        assert_eq!(stopped.cut(), Some(Cut::Stopped));
        fs::remove_dir_all(root).unwrap();
    }
}
