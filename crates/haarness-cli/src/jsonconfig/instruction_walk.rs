//! The paths an `instructions` glob names. They are found as the `glob` crate
//! finds them, the same paths in the same order, but within bounds: `glob`
//! walks a `**` into every linked folder with no record of where it has
//! been, so a link back up the tree (`notes/up -> ..`) kept a configuration
//! load walking for good. Here a `**` never enters a linked folder (a linked
//! file still matches, and a linked folder the pattern names, or that a
//! one-level wildcard such as `*` matches, is entered as before), it reads no
//! folder more than [`LEVELS`] below the pattern's last literal folder, and a
//! walk looks at no more than [`ENTRIES`] folder entries.

use glob::{MatchOptions, Pattern};
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// How many folders below the pattern's last literal folder a `**` reads.
pub(super) const LEVELS: usize = 8;

/// How many folder entries one walk looks at before it stops.
pub(super) const ENTRIES: usize = 100_000;

/// Why a walk ended before it looked everywhere its pattern reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Cut {
    /// A `**` met a folder more than [`LEVELS`] below the last literal one.
    Deep,
    /// The walk looked at [`ENTRIES`] entries.
    Large,
}

/// What a walk found.
#[derive(Debug, Default)]
pub(super) struct Found {
    /// Every path the pattern matches, folders included, in `glob`'s order.
    pub(super) paths: Vec<PathBuf>,
    /// The pattern's last literal folder, where its wildcards start.
    pub(super) base: PathBuf,
    /// Why the walk ended early, when it did.
    pub(super) cut: Option<Cut>,
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
    /// How many components below where the walk started.
    level: usize,
}

impl Entry {
    fn named(path: PathBuf, level: usize) -> Self {
        let folder = fs::metadata(&path).is_ok_and(|metadata| metadata.is_dir());
        Self {
            path,
            folder,
            link: false,
            level,
        }
    }
}

/// Paths still to look at: each with the part it meets next, or `None` once
/// it matched the whole pattern.
type Todo = Vec<(Entry, Option<usize>)>;

struct Walk {
    parts: Vec<Part>,
    /// How many leading parts name a folder literally.
    base: usize,
    /// [`LEVELS`] and [`ENTRIES`], smaller in tests.
    levels: usize,
    entries: usize,
    looked: usize,
    cut: Option<Cut>,
}

/// The paths `pattern` matches (see the module comment). A pattern `glob`
/// refuses matches nothing, as before.
pub(super) fn walk(pattern: &str) -> Found {
    walk_within(pattern, LEVELS, ENTRIES)
}

pub(super) fn walk_within(pattern: &str, levels: usize, entries: usize) -> Found {
    if Pattern::new(pattern).is_err() {
        return Found::default();
    }
    let root_len = root_len(pattern);
    let root = &pattern[..root_len];
    if verbatim(root) {
        return Found::default();
    }
    let texts: Vec<&str> = pattern[root_len..]
        .split_terminator(std::path::is_separator)
        .collect();
    let mut parts = Vec::new();
    for text in &texts {
        let Ok(compiled) = Pattern::new(text) else {
            return Found::default();
        };
        parts.push(Part {
            pattern: compiled,
            plain: !text.contains(['*', '?', '[']),
            recursive: *text == "**",
        });
    }
    if root_len == pattern.len() {
        parts.push(Part {
            pattern: Pattern::default(),
            plain: true,
            recursive: false,
        });
    }
    let start = PathBuf::from(if root.is_empty() { "." } else { root });
    let literal: Vec<String> = texts.iter().map_while(|text| literal(text)).collect();
    let mut found = Found {
        base: literal
            .iter()
            .fold(start.clone(), |path, name| path.join(name)),
        ..Found::default()
    };
    let require_folder = pattern.ends_with(std::path::is_separator);
    let mut walk = Walk {
        parts,
        base: literal.len(),
        levels,
        entries,
        looked: 0,
        cut: None,
    };
    let mut todo = Todo::new();
    if !walk.parts.is_empty() {
        walk.fill(&mut todo, 0, &Entry::named(start, 0));
    }
    let last = walk.parts.len().saturating_sub(1);
    while let Some((entry, at)) = todo.pop() {
        if walk.cut == Some(Cut::Large) {
            break;
        }
        let Some(mut at) = at else {
            if !require_folder || entry.folder {
                found.paths.push(entry.path);
            }
            continue;
        };
        if walk.parts[at].recursive {
            let mut next = at;
            while next < last && walk.parts[next + 1].recursive {
                next += 1;
            }
            if entry.folder {
                walk.descend(&mut todo, next, &entry);
                if next == last {
                    found.paths.push(entry.path);
                    continue;
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
        if walk.parts[at]
            .pattern
            .matches_with(name, MatchOptions::new())
        {
            if at == last {
                if !require_folder || entry.folder {
                    found.paths.push(entry.path);
                }
            } else {
                walk.fill(&mut todo, at + 1, &entry);
            }
        }
    }
    found.cut = walk.cut;
    found
}

impl Walk {
    /// A `**` going into `entry`'s folder: never a linked one, and none more
    /// than `levels` below the last literal folder.
    fn descend(&mut self, todo: &mut Todo, at: usize, entry: &Entry) {
        if entry.link {
            return;
        }
        if entry.level.saturating_sub(self.base) > self.levels {
            self.cut.get_or_insert(Cut::Deep);
            return;
        }
        self.fill(todo, at, entry);
    }

    /// Queues what part `at` finds from `entry`, as `glob` does.
    fn fill(&mut self, todo: &mut Todo, at: usize, entry: &Entry) {
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
                self.add(todo, at, Entry::named(next, entry.level + 1));
            }
            return;
        }
        if !entry.folder {
            return;
        }
        if self.looked >= self.entries {
            self.cut = Some(Cut::Large);
            return;
        }
        // A listing with an entry that cannot be read gives nothing.
        let Ok(children) = fs::read_dir(&entry.path).and_then(|listing| {
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
                        let entry = Entry {
                            path,
                            folder,
                            link,
                            level: entry.level + 1,
                        };
                        (entry, name)
                    })
                })
                .collect::<std::io::Result<Vec<(Entry, OsString)>>>()
        }) else {
            return;
        };
        self.looked += children.len();
        let mut children = children;
        // Popped from the end, so in name order.
        children.sort_by(|first, second| second.1.cmp(&first.1));
        todo.extend(children.into_iter().map(|(child, _)| (child, Some(at))));
        // A part that starts with `.` also matches `.` and `..`.
        if part.pattern.as_str().starts_with('.') {
            for special in [".", ".."] {
                if self.parts[at]
                    .pattern
                    .matches_with(special, MatchOptions::new())
                {
                    self.add(
                        todo,
                        at,
                        Entry::named(entry.path.join(special), entry.level + 1),
                    );
                }
            }
        }
    }

    fn add(&mut self, todo: &mut Todo, at: usize, next: Entry) {
        if at + 1 == self.parts.len() {
            todo.push((next, None));
        } else {
            self.fill(todo, at + 1, &next);
        }
    }
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

/// The name a component stands for when it has no wildcard, written plainly
/// or escaped the way `glob::Pattern::escape` escapes (`[[]`, `[*]`).
fn literal(text: &str) -> Option<String> {
    let mut name = String::new();
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        match character {
            '*' | '?' => return None,
            '[' => match (characters.next(), characters.next()) {
                (Some(inner), Some(']')) if inner != '!' => name.push(inner),
                _ => return None,
            },
            character => name.push(character),
        }
    }
    Some(name)
}
