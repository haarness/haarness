//! The review model behind the full-screen `/diff` viewer.
//!
//! Everything here is pure data: parse a patch (or a before/after pair) into
//! numbered rows, pair those rows for a side-by-side layout, fold the changed
//! paths into a directory tree, and expose cursor/scroll navigation over the
//! flattened result. The renderer in `view::diffview` only paints what this
//! module has already decided.

use std::collections::{BTreeMap, BTreeSet};

use haarness_protocol::{FileDiff, FileDiffStatus};
use similar::{ChangeTag, TextDiff};

/// Context-line steps cycled by `x`. `None` means the whole file.
pub const CONTEXT_STEPS: [Option<u32>; 4] = [Some(4), Some(12), Some(50), None];
/// Where the cycle starts: enough context to read a change in place without
/// pulling an entire file into the viewer.
pub const DEFAULT_CONTEXT_STEP: usize = 1;
/// Columns reserved for the file tree when it is showing.
pub const TREE_WIDTH: usize = 30;
/// Narrower than this and the patch pane cannot hold two readable columns.
pub const MIN_SPLIT_WIDTH: usize = 88;
/// Hard bound on rows kept for one file, so a generated lockfile cannot make
/// the viewer unresponsive.
const MAX_ROWS_PER_FILE: usize = 20_000;

/// Which review a viewer is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffSource {
    WorkingTree,
    Branch,
    LastTurn,
}

impl DiffSource {
    pub const fn label(self) -> &'static str {
        match self {
            Self::WorkingTree => "working tree",
            Self::Branch => "main branch",
            Self::LastTurn => "last turn",
        }
    }

    /// The `mode` value understood by `GET /v1/vcs/diff`. Last-turn reviews
    /// come from the snapshot store instead and have no Git mode.
    pub const fn query(self) -> Option<&'static str> {
        match self {
            Self::WorkingTree => Some("working"),
            Self::Branch => Some("branch"),
            Self::LastTurn => None,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "" | "git" | "working" | "worktree" => Some(Self::WorkingTree),
            "branch" => Some(Self::Branch),
            "last" | "turn" => Some(Self::LastTurn),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLayout {
    Unified,
    Split,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffFocus {
    Tree,
    Patch,
}

/// What one rendered patch row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Context,
    Added,
    Removed,
    /// The elision between two hunks.
    Gap,
}

impl RowKind {
    pub const fn is_change(self) -> bool {
        matches!(self, Self::Added | Self::Removed)
    }
}

/// One line of a patch, already numbered on the side it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: RowKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
    /// Character ranges that differ from the paired line, for the word-level
    /// emphasis that makes a one-token change readable at a glance.
    pub emphasis: Vec<CharRange>,
}

impl DiffRow {
    fn plain(kind: RowKind, old: Option<u32>, new: Option<u32>, text: impl Into<String>) -> Self {
        Self {
            kind,
            old,
            new,
            text: text.into(),
            emphasis: Vec::new(),
        }
    }

    /// The line number a comment on this row addresses: the post-image where
    /// there is one, otherwise the line that was removed.
    pub const fn line(&self) -> Option<u32> {
        match self.new {
            Some(new) => Some(new),
            None => self.old,
        }
    }
}

/// One row of the side-by-side layout: an index into a file's rows per side,
/// absent where that side has nothing there.
pub type SplitPair = (Option<usize>, Option<usize>);

/// A file's rows plus the pairing used by the side-by-side layout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileRows {
    pub rows: Vec<DiffRow>,
    pub pairs: Vec<SplitPair>,
    pub truncated: bool,
    /// A body-free explanation (binary, over budget, nothing to show).
    pub note: Option<String>,
}

// ---------------------------------------------------------------------------
// Row construction
// ---------------------------------------------------------------------------

/// Build the rows for one file. Git reviews carry a `patch`; snapshot reviews
/// carry `before`/`after` and are re-cut locally whenever context changes.
pub fn file_rows(file: &FileDiff, context: Option<u32>) -> FileRows {
    if file.binary {
        return FileRows {
            note: Some("Binary content changed".into()),
            ..FileRows::default()
        };
    }
    let mut rows = if let (Some(before), Some(after)) = (&file.before, &file.after) {
        rows_from_texts(before, after, context)
    } else if let Some(patch) = &file.patch {
        rows_from_patch(patch)
    } else if let Some(after) = &file.after {
        rows_from_texts("", after, context)
    } else if let Some(before) = &file.before {
        rows_from_texts(before, "", context)
    } else {
        Vec::new()
    };
    let truncated = rows.len() > MAX_ROWS_PER_FILE;
    if truncated {
        rows.truncate(MAX_ROWS_PER_FILE);
    }
    let note = if !rows.is_empty() {
        None
    } else if file.truncated {
        Some("Patch body exceeded the review budget; counts remain available".into())
    } else {
        Some("No patch body for this file".into())
    };
    let pairs = split_pairs(&rows);
    FileRows {
        rows,
        pairs,
        truncated: truncated || file.truncated,
        note,
    }
}

/// Rows from a pre/post image pair. `context` of `None` keeps the whole file.
pub fn rows_from_texts(before: &str, after: &str, context: Option<u32>) -> Vec<DiffRow> {
    let diff = TextDiff::from_lines(before, after);
    let mut rows = Vec::new();
    let Some(context) = context else {
        for change in diff.iter_all_changes() {
            rows.push(row_from_change(
                change.tag(),
                change.old_index(),
                change.new_index(),
                change.value(),
            ));
        }
        return apply_word_emphasis(rows);
    };
    for (index, group) in diff.grouped_ops(context as usize).iter().enumerate() {
        if index > 0 {
            rows.push(DiffRow::plain(RowKind::Gap, None, None, String::new()));
        }
        for op in group {
            for change in diff.iter_inline_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Insert => RowKind::Added,
                    ChangeTag::Delete => RowKind::Removed,
                    ChangeTag::Equal => RowKind::Context,
                };
                let mut text = String::new();
                let mut emphasis = Vec::new();
                for (marked, value) in change.iter_strings_lossy() {
                    let value = value.trim_end_matches(['\n', '\r']);
                    let start = text.chars().count();
                    text.push_str(value);
                    if marked && !value.is_empty() {
                        emphasis.push((start, text.chars().count()));
                    }
                }
                rows.push(DiffRow {
                    kind,
                    old: change.old_index().map(line_number),
                    new: change.new_index().map(line_number),
                    text,
                    emphasis,
                });
            }
        }
    }
    rows
}

/// Lines added and removed in one file, as `git diff` counts them. Whole
/// pre/post images are diffed line by line: hosts before 2026-10-17 counted
/// an edit's entire region, so unchanged lines inside it showed as both
/// removed and added. Without both bodies intact the host's numbers stand.
pub fn line_stat(file: &FileDiff) -> (u64, u64) {
    if file.binary || file.truncated || (file.before.is_none() && file.after.is_none()) {
        return (file.additions, file.deletions);
    }
    let diff = TextDiff::from_lines(
        file.before.as_deref().unwrap_or_default(),
        file.after.as_deref().unwrap_or_default(),
    );
    diff.ops()
        .iter()
        .fold((0, 0), |(additions, deletions), op| match op.tag() {
            similar::DiffTag::Equal => (additions, deletions),
            _ => (
                additions + u64::try_from(op.new_range().len()).unwrap_or(u64::MAX),
                deletions + u64::try_from(op.old_range().len()).unwrap_or(u64::MAX),
            ),
        })
}

/// Move `value` by a signed `delta`, saturating at zero and `max`.
fn shift(value: usize, delta: isize, max: usize) -> usize {
    let next = if delta >= 0 {
        value.saturating_add(delta.unsigned_abs())
    } else {
        value.saturating_sub(delta.unsigned_abs())
    };
    next.min(max)
}

/// A zero-based diff index as a one-based line number.
fn line_number(index: usize) -> u32 {
    u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX)
}

fn row_from_change(tag: ChangeTag, old: Option<usize>, new: Option<usize>, value: &str) -> DiffRow {
    let kind = match tag {
        ChangeTag::Insert => RowKind::Added,
        ChangeTag::Delete => RowKind::Removed,
        ChangeTag::Equal => RowKind::Context,
    };
    DiffRow::plain(
        kind,
        old.map(line_number),
        new.map(line_number),
        value.trim_end_matches(['\n', '\r']),
    )
}

/// Rows from a unified Git patch. Hunk headers become gaps; the numbers come
/// from the `@@` ranges so the gutter matches the file on disk.
pub fn rows_from_patch(patch: &str) -> Vec<DiffRow> {
    let mut rows: Vec<DiffRow> = Vec::new();
    let mut old = 0_u32;
    let mut new = 0_u32;
    let mut in_hunk = false;
    for line in patch.lines() {
        if line.starts_with("@@") {
            if let Some((start_old, start_new)) = parse_hunk_header(line) {
                if in_hunk {
                    rows.push(DiffRow::plain(RowKind::Gap, None, None, String::new()));
                }
                old = start_old;
                new = start_new;
                in_hunk = true;
            }
            continue;
        }
        if !in_hunk {
            // File headers: `diff --git`, `index`, `--- a/…`, `+++ b/…`,
            // mode and rename lines. None of them belong in the body.
            continue;
        }
        let Some(marker) = line.chars().next() else {
            // A bare empty line inside a hunk is an empty context line.
            rows.push(DiffRow::plain(RowKind::Context, Some(old), Some(new), ""));
            old += 1;
            new += 1;
            continue;
        };
        let body = &line[marker.len_utf8()..];
        match marker {
            '+' => {
                rows.push(DiffRow::plain(RowKind::Added, None, Some(new), body));
                new += 1;
            }
            '-' => {
                rows.push(DiffRow::plain(RowKind::Removed, Some(old), None, body));
                old += 1;
            }
            ' ' => {
                rows.push(DiffRow::plain(RowKind::Context, Some(old), Some(new), body));
                old += 1;
                new += 1;
            }
            // "\ No newline at end of file" annotates the previous row.
            '\\' => {}
            _ => {
                in_hunk = false;
            }
        }
    }
    apply_word_emphasis(rows)
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let inner = line.strip_prefix("@@")?.split("@@").next()?;
    let mut old = None;
    let mut new = None;
    for field in inner.split_whitespace() {
        let (sign, rest) = field.split_at(1);
        let start = rest.split(',').next()?.parse::<u32>().ok()?;
        match sign {
            "-" => old = Some(start.max(1)),
            "+" => new = Some(start.max(1)),
            _ => {}
        }
    }
    Some((old?, new?))
}

/// Give every removed/added pair inside a hunk its word-level emphasis. Git
/// patches arrive without it, and it is what turns "this line changed" into
/// "this token changed".
fn apply_word_emphasis(mut rows: Vec<DiffRow>) -> Vec<DiffRow> {
    let mut index = 0;
    while index < rows.len() {
        if rows[index].kind != RowKind::Removed {
            index += 1;
            continue;
        }
        let removed_start = index;
        while index < rows.len() && rows[index].kind == RowKind::Removed {
            index += 1;
        }
        let added_start = index;
        while index < rows.len() && rows[index].kind == RowKind::Added {
            index += 1;
        }
        let removed = removed_start..added_start;
        let added = added_start..index;
        // Only a one-to-one replacement has an unambiguous pairing. Anything
        // else stays whole-line, which is honest rather than misleading.
        if removed.len() != added.len() || removed.is_empty() {
            continue;
        }
        for offset in 0..removed.len() {
            let before = rows[removed.start + offset].text.clone();
            let after = rows[added.start + offset].text.clone();
            let (old_ranges, new_ranges) = word_ranges(&before, &after);
            rows[removed.start + offset].emphasis = old_ranges;
            rows[added.start + offset].emphasis = new_ranges;
        }
    }
    rows
}

/// A run of characters within a line, as `[start, end)` char offsets.
pub type CharRange = (usize, usize);

/// Character ranges that differ between two lines, per side.
fn word_ranges(before: &str, after: &str) -> (Vec<CharRange>, Vec<CharRange>) {
    let diff = TextDiff::from_words(before, after);
    let mut old_ranges = Vec::new();
    let mut new_ranges = Vec::new();
    let mut old_at = 0_usize;
    let mut new_at = 0_usize;
    for change in diff.iter_all_changes() {
        let width = change.value().chars().count();
        match change.tag() {
            ChangeTag::Equal => {
                old_at += width;
                new_at += width;
            }
            ChangeTag::Delete => {
                push_range(&mut old_ranges, old_at, old_at + width);
                old_at += width;
            }
            ChangeTag::Insert => {
                push_range(&mut new_ranges, new_at, new_at + width);
                new_at += width;
            }
        }
    }
    // Whole-line rewrites tell the reader nothing; drop the emphasis and let
    // the row colour carry the change. Shared indentation and the spaces
    // between two rewritten words do not count as retained text.
    if covers_everything(before, &old_ranges) && covers_everything(after, &new_ranges) {
        return (Vec::new(), Vec::new());
    }
    (old_ranges, new_ranges)
}

fn push_range(ranges: &mut Vec<CharRange>, start: usize, end: usize) {
    if start == end {
        return;
    }
    if let Some(last) = ranges.last_mut()
        && last.1 == start
    {
        last.1 = end;
        return;
    }
    ranges.push((start, end));
}

/// True when every character that carries meaning is inside a changed range.
fn covers_everything(text: &str, ranges: &[CharRange]) -> bool {
    text.chars()
        .enumerate()
        .filter(|(_, character)| !character.is_whitespace())
        .all(|(index, _)| {
            ranges
                .iter()
                .any(|(start, end)| index >= *start && index < *end)
        })
}

/// Pair removed rows with the added rows that replaced them, so the split
/// layout puts a change and its replacement on the same screen row.
pub fn split_pairs(rows: &[DiffRow]) -> Vec<SplitPair> {
    let mut pairs = Vec::new();
    let mut removed: Vec<usize> = Vec::new();
    let mut added: Vec<usize> = Vec::new();
    let flush = |removed: &mut Vec<usize>, added: &mut Vec<usize>, pairs: &mut Vec<SplitPair>| {
        for index in 0..removed.len().max(added.len()) {
            pairs.push((removed.get(index).copied(), added.get(index).copied()));
        }
        removed.clear();
        added.clear();
    };
    for (index, row) in rows.iter().enumerate() {
        match row.kind {
            RowKind::Removed => removed.push(index),
            RowKind::Added => added.push(index),
            RowKind::Context | RowKind::Gap => {
                flush(&mut removed, &mut added, &mut pairs);
                pairs.push((Some(index), Some(index)));
            }
        }
    }
    flush(&mut removed, &mut added, &mut pairs);
    pairs
}

// ---------------------------------------------------------------------------
// File tree
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub name: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub directory: bool,
    pub file: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTree {
    pub roots: Vec<usize>,
    pub nodes: Vec<TreeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub id: usize,
    pub depth: usize,
    pub name: String,
    pub directory: bool,
    pub file: Option<usize>,
}

impl FileTree {
    /// Fold `a/b/c.rs` paths into a directory tree, directories before files
    /// and each level sorted by name.
    pub fn build(files: &[FileDiff]) -> Self {
        let mut tree = Self::default();
        let mut directories: BTreeMap<String, usize> = BTreeMap::new();
        for (index, file) in files.iter().enumerate() {
            let segments: Vec<&str> = file
                .file
                .split('/')
                .filter(|segment| !segment.is_empty())
                .collect();
            let Some((name, parents)) = segments.split_last() else {
                continue;
            };
            let mut parent = None;
            let mut path = String::new();
            for segment in parents {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(segment);
                parent = Some(match directories.get(&path) {
                    Some(existing) => *existing,
                    None => {
                        let id = tree.push(TreeNode {
                            name: (*segment).to_owned(),
                            parent,
                            children: Vec::new(),
                            directory: true,
                            file: None,
                        });
                        directories.insert(path.clone(), id);
                        id
                    }
                });
            }
            tree.push(TreeNode {
                name: (*name).to_owned(),
                parent,
                children: Vec::new(),
                directory: false,
                file: Some(index),
            });
        }
        tree.sort();
        tree
    }

    fn push(&mut self, node: TreeNode) -> usize {
        let id = self.nodes.len();
        match node.parent {
            Some(parent) => self.nodes[parent].children.push(id),
            None => self.roots.push(id),
        }
        self.nodes.push(node);
        id
    }

    fn sort(&mut self) {
        let key = |tree: &Self, id: usize| {
            let node = &tree.nodes[id];
            (usize::from(!node.directory), node.name.clone(), id)
        };
        let mut roots = std::mem::take(&mut self.roots);
        roots.sort_by_key(|id| key(self, *id));
        self.roots = roots;
        for index in 0..self.nodes.len() {
            let mut children = std::mem::take(&mut self.nodes[index].children);
            children.sort_by_key(|id| key(self, *id));
            self.nodes[index].children = children;
        }
    }

    /// Every directory id, for "expand all" and the default open state.
    pub fn all_directories(&self) -> BTreeSet<usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.directory)
            .map(|(id, _)| id)
            .collect()
    }

    /// Visible rows in display order. Single-child directory chains collapse
    /// into one `a/b/c` row so deep trees stay narrow.
    pub fn flatten(&self, expanded: &BTreeSet<usize>) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        for root in &self.roots {
            self.visit(*root, 0, expanded, &mut rows);
        }
        rows
    }

    fn visit(&self, id: usize, depth: usize, expanded: &BTreeSet<usize>, rows: &mut Vec<TreeRow>) {
        let node = &self.nodes[id];
        if !node.directory {
            rows.push(TreeRow {
                id,
                depth,
                name: node.name.clone(),
                directory: false,
                file: node.file,
            });
            return;
        }
        let chain = self.collapsed_chain(id);
        let last = *chain.last().unwrap_or(&id);
        rows.push(TreeRow {
            id,
            depth,
            name: chain
                .iter()
                .map(|node| self.nodes[*node].name.as_str())
                .collect::<Vec<_>>()
                .join("/"),
            directory: true,
            file: None,
        });
        if expanded.contains(&id) {
            for child in &self.nodes[last].children {
                self.visit(*child, depth + 1, expanded, rows);
            }
        }
    }

    fn collapsed_chain(&self, id: usize) -> Vec<usize> {
        let mut chain = vec![id];
        let mut current = id;
        loop {
            let node = &self.nodes[current];
            if node.children.len() != 1 {
                return chain;
            }
            let child = node.children[0];
            if !self.nodes[child].directory {
                return chain;
            }
            chain.push(child);
            current = child;
        }
    }

    /// The directories that must be open for `file` to be visible.
    pub fn parents_of_file(&self, file: usize) -> BTreeSet<usize> {
        let Some(id) = self.nodes.iter().position(|node| node.file == Some(file)) else {
            return BTreeSet::new();
        };
        let mut result = BTreeSet::new();
        let mut parent = self.nodes[id].parent;
        while let Some(current) = parent {
            result.insert(current);
            parent = self.nodes[current].parent;
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

/// A queued review note. Closing the viewer hands every note to the composer
/// so the model receives the review as ordinary prompt text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffComment {
    pub file: String,
    pub line: Option<u32>,
    pub text: String,
}

/// Wording shared with the note the model already understands from Haarness's
/// review surfaces, so a comment reads the same wherever it was written.
pub fn format_comment(comment: &DiffComment) -> String {
    let range = comment
        .line
        .map_or_else(|| "this file".to_owned(), |line| format!("line {line}"));
    format!(
        "The user made the following comment regarding {range} of {}: {}",
        comment.file, comment.text
    )
}

pub fn format_comments(comments: &[DiffComment]) -> String {
    comments
        .iter()
        .map(format_comment)
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ---------------------------------------------------------------------------
// Display rows
// ---------------------------------------------------------------------------

/// One row of the patch pane, already resolved against the current layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayRow {
    /// A file's name and counts, repeated at the top of each patch.
    Header(usize),
    /// A unified body row: an index into that file's `rows`.
    Body { file: usize, row: usize },
    /// A side-by-side body row.
    Split {
        file: usize,
        left: Option<usize>,
        right: Option<usize>,
    },
    /// A body-free explanation attached to a file.
    Note { file: usize, text: String },
    /// Breathing room between files.
    Spacer,
}

impl DisplayRow {
    pub const fn file(&self) -> Option<usize> {
        match self {
            Self::Header(file)
            | Self::Body { file, .. }
            | Self::Split { file, .. }
            | Self::Note { file, .. } => Some(*file),
            Self::Spacer => None,
        }
    }

    pub const fn selectable(&self) -> bool {
        matches!(self, Self::Body { .. } | Self::Split { .. })
    }
}

// ---------------------------------------------------------------------------
// Viewer state
// ---------------------------------------------------------------------------

/// Everything the `/diff` surface needs. Derived collections (`rows`, `tree`,
/// `display`) are rebuilt only when an input changes, never per frame.
#[allow(clippy::struct_excessive_bools)] // independent view toggles, not a state machine
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffViewer {
    pub source: DiffSource,
    pub files: Vec<FileDiff>,
    pub rows: Vec<FileRows>,
    pub tree: FileTree,
    pub expanded: BTreeSet<usize>,
    pub display: Vec<DisplayRow>,
    pub context_step: usize,
    pub layout: Option<DiffLayout>,
    pub focus: DiffFocus,
    pub tree_visible: bool,
    pub single_file: bool,
    pub tree_selection: usize,
    pub active_file: usize,
    pub reviewed: BTreeSet<String>,
    pub comments: Vec<DiffComment>,
    pub cursor: Option<usize>,
    pub scroll: usize,
    pub help: bool,
    /// In-progress comment text while the footer is an input line.
    pub comment_input: Option<String>,
    /// Rows the last frame gave the patch pane; navigation pages by it.
    pub viewport: std::cell::Cell<usize>,
    /// Rows the last frame gave the tree pane.
    pub tree_viewport: std::cell::Cell<usize>,
    pub tree_scroll: usize,
    /// True while a refetch is in flight, so the header can say so.
    pub loading: bool,
    /// Changed paths the server bounded out of this review. `None` means it
    /// returned everything that changed.
    pub omitted_files: Option<u64>,
    /// Columns the patch pane had in the last frame. Split needs room, so the
    /// resolved layout is a function of this as well as the reader's choice.
    pub pane_width: std::cell::Cell<usize>,
}

impl DiffViewer {
    pub fn new(source: DiffSource, files: Vec<FileDiff>, preferences: DiffPreferences) -> Self {
        let mut viewer = Self {
            source,
            files,
            rows: Vec::new(),
            tree: FileTree::default(),
            expanded: BTreeSet::new(),
            display: Vec::new(),
            context_step: preferences.context_step.min(CONTEXT_STEPS.len() - 1),
            layout: preferences.layout,
            focus: DiffFocus::Patch,
            tree_visible: preferences.tree_visible,
            single_file: preferences.single_file,
            tree_selection: 0,
            active_file: 0,
            reviewed: BTreeSet::new(),
            comments: Vec::new(),
            cursor: None,
            scroll: 0,
            help: false,
            comment_input: None,
            viewport: std::cell::Cell::new(20),
            tree_viewport: std::cell::Cell::new(20),
            tree_scroll: 0,
            loading: false,
            omitted_files: None,
            pane_width: std::cell::Cell::new(0),
        };
        viewer.rebuild_files();
        viewer
    }

    pub const fn context(&self) -> Option<u32> {
        CONTEXT_STEPS[self.context_step]
    }

    pub fn context_label(&self) -> String {
        self.context()
            .map_or_else(|| "all".to_owned(), |value| value.to_string())
    }

    /// Replace the reviewed files, keeping the reader's place where the same
    /// path is still present.
    pub fn set_files(&mut self, files: Vec<FileDiff>) {
        let anchor = self.active_file_name();
        self.files = files;
        self.rebuild_files();
        if let Some(anchor) = anchor
            && let Some(index) = self.files.iter().position(|file| file.file == anchor)
        {
            self.focus_file(index);
        }
    }

    fn rebuild_files(&mut self) {
        let context = self.context();
        self.rows = self
            .files
            .iter()
            .map(|file| file_rows(file, context))
            .collect();
        self.tree = FileTree::build(&self.files);
        self.expanded = self.tree.all_directories();
        self.active_file = self.active_file.min(self.files.len().saturating_sub(1));
        self.reviewed
            .retain(|name| self.files.iter().any(|file| &file.file == name));
        self.rebuild_display();
        self.scroll = 0;
        self.tree_selection = 0;
        self.tree_scroll = 0;
        self.place_cursor_at_start();
    }

    /// Put the cursor on the first reviewable row and make its file active,
    /// so the tree highlight, the header marker, and the cursor all agree.
    fn place_cursor_at_start(&mut self) {
        self.cursor = None;
        match self.first_selectable() {
            Some(index) => self.set_cursor(index),
            None => self.cursor = None,
        }
    }

    /// Recompute the flattened patch pane from the current layout settings.
    pub fn rebuild_display(&mut self) {
        let split = self.layout_in_use() == DiffLayout::Split;
        let mut display = Vec::new();
        let indexes: Vec<usize> = if self.single_file {
            if self.files.is_empty() {
                Vec::new()
            } else {
                vec![self.active_file.min(self.files.len() - 1)]
            }
        } else {
            self.ordered_files()
        };
        for file in indexes {
            if !display.is_empty() {
                display.push(DisplayRow::Spacer);
            }
            display.push(DisplayRow::Header(file));
            let rows = &self.rows[file];
            if let Some(note) = &rows.note {
                display.push(DisplayRow::Note {
                    file,
                    text: note.clone(),
                });
                continue;
            }
            if split {
                for (left, right) in &rows.pairs {
                    display.push(DisplayRow::Split {
                        file,
                        left: *left,
                        right: *right,
                    });
                }
            } else {
                for row in 0..rows.rows.len() {
                    display.push(DisplayRow::Body { file, row });
                }
            }
            if rows.truncated {
                display.push(DisplayRow::Note {
                    file,
                    text: "… body truncated for review".into(),
                });
            }
        }
        self.display = display;
        if self
            .cursor
            .is_some_and(|cursor| cursor >= self.display.len())
        {
            self.cursor = self.first_selectable();
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// File order as the tree presents it, so `]`/`[` and the stacked patches
    /// agree with the sidebar.
    pub fn ordered_files(&self) -> Vec<usize> {
        let rows = self.tree.flatten(&self.tree.all_directories());
        let ordered: Vec<usize> = rows.iter().filter_map(|row| row.file).collect();
        if ordered.len() == self.files.len() {
            ordered
        } else {
            (0..self.files.len()).collect()
        }
    }

    /// The layout actually in force: the reader's choice where the pane can
    /// carry it, side-by-side by default on a wide terminal, unified below.
    pub fn layout_in_use(&self) -> DiffLayout {
        if !Self::split_available(self.pane_width.get()) {
            return DiffLayout::Unified;
        }
        self.layout.unwrap_or(DiffLayout::Split)
    }

    /// Split needs a wide pane; below the threshold the viewer silently reads
    /// as unified rather than crushing two columns together.
    pub const fn split_available(patch_width: usize) -> bool {
        patch_width >= MIN_SPLIT_WIDTH
    }

    pub fn tree_showing(&self) -> bool {
        self.tree_visible && !self.files.is_empty()
    }

    pub fn active_file_name(&self) -> Option<String> {
        self.files
            .get(self.active_file)
            .map(|file| file.file.clone())
    }

    pub fn preferences(&self) -> DiffPreferences {
        DiffPreferences {
            context_step: self.context_step,
            layout: self.layout,
            tree_visible: self.tree_visible,
            single_file: self.single_file,
        }
    }

    // -- navigation ---------------------------------------------------------

    fn first_selectable(&self) -> Option<usize> {
        self.display.iter().position(DisplayRow::selectable)
    }

    pub fn max_scroll(&self) -> usize {
        self.display
            .len()
            .saturating_sub(self.viewport.get().max(1))
    }

    pub fn cursor_row(&self) -> Option<&DiffRow> {
        let cursor = self.cursor?;
        match self.display.get(cursor)? {
            DisplayRow::Body { file, row } => self.rows.get(*file)?.rows.get(*row),
            DisplayRow::Split { file, left, right } => {
                let rows = &self.rows.get(*file)?.rows;
                right.or(*left).and_then(|index| rows.get(index))
            }
            _ => None,
        }
    }

    pub fn cursor_line(&self) -> Option<u32> {
        self.cursor_row().and_then(DiffRow::line)
    }

    pub fn cursor_file(&self) -> Option<usize> {
        self.cursor
            .and_then(|cursor| self.display.get(cursor))
            .and_then(DisplayRow::file)
    }

    /// Step the line cursor one row, skipping headers and spacers.
    pub fn move_cursor(&mut self, forward: bool) {
        let mut index = self.cursor.unwrap_or(0);
        loop {
            index = if forward {
                match index + 1 {
                    next if next < self.display.len() => next,
                    _ => return,
                }
            } else {
                match index.checked_sub(1) {
                    Some(next) => next,
                    None => return,
                }
            };
            if self.display[index].selectable() {
                self.set_cursor(index);
                return;
            }
        }
    }

    fn set_cursor(&mut self, index: usize) {
        self.cursor = Some(index);
        if let Some(file) = self.display[index].file() {
            self.active_file = file;
            self.sync_tree_to_active();
        }
        self.reveal_cursor();
    }

    /// Keep the cursor on screen with a one-row margin.
    pub fn reveal_cursor(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let viewport = self.viewport.get().max(1);
        if cursor < self.scroll {
            self.scroll = cursor.saturating_sub(1);
        } else if cursor >= self.scroll + viewport {
            self.scroll = cursor + 2 - viewport;
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }

    pub fn scroll_by(&mut self, delta: isize) {
        self.scroll = shift(self.scroll, delta, self.max_scroll());
    }

    /// Jump to the next or previous changed line, parking the top of its hunk
    /// at the top of the pane so the context leading in stays readable.
    pub fn jump_change(&mut self, forward: bool) {
        let Some(next) = self.find_change(forward) else {
            return;
        };
        self.cursor = Some(next);
        if let Some(file) = self.display[next].file() {
            self.active_file = file;
            self.sync_tree_to_active();
        }
        let anchor = self.hunk_start(next);
        let viewport = self.viewport.get().max(1);
        self.scroll = anchor.min(self.max_scroll());
        if next >= self.scroll + viewport {
            self.scroll = (next + 2).saturating_sub(viewport).min(self.max_scroll());
        }
    }

    fn find_change(&self, forward: bool) -> Option<usize> {
        let start = self.cursor.unwrap_or(0);
        let current_run = self.change_run(start);
        let indexes: Vec<usize> = if forward {
            (start + 1..self.display.len()).collect()
        } else {
            (0..start).rev().collect()
        };
        for index in indexes {
            if !self.is_change_row(index) {
                continue;
            }
            // A run of adjacent changed lines is one destination, so stepping
            // never stops twice inside the same edit.
            if current_run.is_some_and(|run| self.change_run(index) == Some(run)) {
                continue;
            }
            if !forward {
                let run_start = self.run_start(index);
                return Some(run_start);
            }
            return Some(index);
        }
        None
    }

    fn is_change_row(&self, index: usize) -> bool {
        match self.display.get(index) {
            Some(DisplayRow::Body { file, row }) => self
                .rows
                .get(*file)
                .and_then(|rows| rows.rows.get(*row))
                .is_some_and(|row| row.kind.is_change()),
            Some(DisplayRow::Split { file, left, right }) => {
                let rows = self.rows.get(*file).map(|rows| &rows.rows);
                rows.is_some_and(|rows| {
                    [left, right].iter().any(|side| {
                        side.and_then(|index| rows.get(index))
                            .is_some_and(|row| row.kind.is_change())
                    })
                })
            }
            _ => false,
        }
    }

    /// Identify the contiguous run of changed rows a row belongs to.
    fn change_run(&self, index: usize) -> Option<usize> {
        self.is_change_row(index).then(|| self.run_start(index))
    }

    fn run_start(&self, index: usize) -> usize {
        let mut start = index;
        while start > 0 && self.is_change_row(start - 1) {
            start -= 1;
        }
        start
    }

    fn hunk_start(&self, index: usize) -> usize {
        let mut start = self.run_start(index);
        while start > 0 {
            match self.display.get(start - 1) {
                Some(DisplayRow::Body { .. } | DisplayRow::Split { .. }) => {
                    if self.is_gap(start - 1) {
                        break;
                    }
                    start -= 1;
                }
                _ => break,
            }
        }
        start
    }

    fn is_gap(&self, index: usize) -> bool {
        match self.display.get(index) {
            Some(DisplayRow::Body { file, row }) => self
                .rows
                .get(*file)
                .and_then(|rows| rows.rows.get(*row))
                .is_some_and(|row| row.kind == RowKind::Gap),
            Some(DisplayRow::Split { file, left, right }) => {
                let rows = self.rows.get(*file).map(|rows| &rows.rows);
                rows.is_some_and(|rows| {
                    left.or(*right)
                        .and_then(|index| rows.get(index))
                        .is_some_and(|row| row.kind == RowKind::Gap)
                })
            }
            _ => false,
        }
    }

    /// Move to the next or previous file in tree order.
    pub fn jump_file(&mut self, forward: bool) {
        let order = self.ordered_files();
        if order.is_empty() {
            return;
        }
        let position = order
            .iter()
            .position(|file| *file == self.active_file)
            .unwrap_or(0);
        let next = if forward {
            (position + 1).min(order.len() - 1)
        } else {
            position.saturating_sub(1)
        };
        self.focus_file(order[next]);
    }

    /// Make `file` the active patch and put the cursor on its first row.
    pub fn focus_file(&mut self, file: usize) {
        if file >= self.files.len() {
            return;
        }
        self.active_file = file;
        if self.single_file {
            self.rebuild_display();
            self.scroll = 0;
            self.cursor = self.first_selectable();
            self.sync_tree_to_active();
            return;
        }
        let header = self
            .display
            .iter()
            .position(|row| matches!(row, DisplayRow::Header(index) if *index == file));
        if let Some(header) = header {
            self.scroll = header.min(self.max_scroll());
            self.cursor = self.display[header..]
                .iter()
                .position(DisplayRow::selectable)
                .map(|offset| header + offset);
        }
        self.sync_tree_to_active();
    }

    // -- tree ---------------------------------------------------------------

    pub fn tree_rows(&self) -> Vec<TreeRow> {
        self.tree.flatten(&self.expanded)
    }

    fn sync_tree_to_active(&mut self) {
        for parent in self.tree.parents_of_file(self.active_file) {
            self.expanded.insert(parent);
        }
        let rows = self.tree_rows();
        if let Some(index) = rows
            .iter()
            .position(|row| row.file == Some(self.active_file))
        {
            self.tree_selection = index;
            self.reveal_tree_row(index, rows.len());
        }
    }

    fn reveal_tree_row(&mut self, index: usize, total: usize) {
        let viewport = self.tree_viewport.get().max(1);
        if index < self.tree_scroll {
            self.tree_scroll = index;
        } else if index >= self.tree_scroll + viewport {
            self.tree_scroll = index + 1 - viewport;
        }
        self.tree_scroll = self.tree_scroll.min(total.saturating_sub(viewport));
    }

    pub fn move_tree_selection(&mut self, delta: isize) {
        let rows = self.tree_rows();
        if rows.is_empty() {
            return;
        }
        let next = shift(self.tree_selection, delta, rows.len() - 1);
        self.tree_selection = next;
        self.reveal_tree_row(next, rows.len());
    }

    /// Enter on a tree row: open the file, or fold and unfold a directory.
    pub fn activate_tree_row(&mut self) {
        let rows = self.tree_rows();
        let Some(row) = rows.get(self.tree_selection).cloned() else {
            return;
        };
        if let Some(file) = row.file {
            self.focus_file(file);
            self.focus = DiffFocus::Patch;
            return;
        }
        if self.expanded.contains(&row.id) {
            self.expanded.remove(&row.id);
        } else {
            self.expanded.insert(row.id);
        }
        let rows = self.tree_rows();
        self.tree_selection = self.tree_selection.min(rows.len().saturating_sub(1));
    }

    /// Right: open the directory, or step to its first child.
    pub fn expand_tree_row(&mut self) {
        let rows = self.tree_rows();
        let Some(row) = rows.get(self.tree_selection).cloned() else {
            return;
        };
        if !row.directory {
            return;
        }
        if self.expanded.contains(&row.id) {
            if rows
                .get(self.tree_selection + 1)
                .is_some_and(|child| child.depth > row.depth)
            {
                self.move_tree_selection(1);
            }
            return;
        }
        self.expanded.insert(row.id);
    }

    /// Left: close the directory, or step out to its parent.
    pub fn collapse_tree_row(&mut self) {
        let rows = self.tree_rows();
        let Some(row) = rows.get(self.tree_selection).cloned() else {
            return;
        };
        if row.directory && self.expanded.contains(&row.id) {
            self.expanded.remove(&row.id);
            return;
        }
        if let Some(parent) = rows[..self.tree_selection]
            .iter()
            .rposition(|candidate| candidate.depth < row.depth)
        {
            self.tree_selection = parent;
            self.reveal_tree_row(parent, rows.len());
        }
    }

    pub fn expand_all(&mut self) {
        self.expanded = self.tree.all_directories();
    }

    // -- review state -------------------------------------------------------

    /// The file `r`, `c`, and the tree act on: the highlighted tree row when
    /// the tree has focus, otherwise the patch under the cursor.
    pub fn target_file(&self) -> Option<usize> {
        match self.focus {
            DiffFocus::Tree => self
                .tree_rows()
                .get(self.tree_selection)
                .and_then(|row| row.file),
            DiffFocus::Patch => self.cursor_file().or(Some(self.active_file)),
        }
        .filter(|file| *file < self.files.len())
    }

    pub fn toggle_reviewed(&mut self) -> Option<(String, bool)> {
        let file = self.target_file()?;
        let name = self.files[file].file.clone();
        let marked = if self.reviewed.contains(&name) {
            self.reviewed.remove(&name);
            false
        } else {
            self.reviewed.insert(name.clone());
            true
        };
        Some((name, marked))
    }

    pub fn is_reviewed(&self, file: usize) -> bool {
        self.files
            .get(file)
            .is_some_and(|entry| self.reviewed.contains(&entry.file))
    }

    pub fn commented_files(&self) -> BTreeSet<&str> {
        self.comments
            .iter()
            .map(|comment| comment.file.as_str())
            .collect()
    }

    pub fn commented_lines(&self, file: &str) -> BTreeSet<u32> {
        self.comments
            .iter()
            .filter(|comment| comment.file == file)
            .filter_map(|comment| comment.line)
            .collect()
    }

    /// What `c` addresses right now: a file plus, from the patch pane, the
    /// line under the cursor.
    pub fn comment_target(&self) -> Option<(String, Option<u32>)> {
        let file = self.target_file()?;
        let name = self.files[file].file.clone();
        let line = match self.focus {
            DiffFocus::Tree => None,
            DiffFocus::Patch => self.cursor_line(),
        };
        Some((name, line))
    }

    pub fn existing_comment(&self, file: &str, line: Option<u32>) -> Option<&DiffComment> {
        self.comments
            .iter()
            .find(|comment| comment.file == file && comment.line == line)
    }

    /// Save a note. Empty text removes the note at that position.
    pub fn upsert_comment(&mut self, file: String, line: Option<u32>, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            self.comments
                .retain(|comment| !(comment.file == file && comment.line == line));
            return;
        }
        if let Some(existing) = self
            .comments
            .iter_mut()
            .find(|comment| comment.file == file && comment.line == line)
        {
            text.clone_into(&mut existing.text);
            return;
        }
        self.comments.push(DiffComment {
            file,
            line,
            text: text.to_owned(),
        });
    }

    /// Total additions and deletions across the review.
    pub fn totals(&self) -> (u64, u64) {
        self.files.iter().fold((0, 0), |(added, removed), file| {
            (added + file.additions, removed + file.deletions)
        })
    }
}

/// The viewer settings worth remembering between sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffPreferences {
    pub context_step: usize,
    pub layout: Option<DiffLayout>,
    pub tree_visible: bool,
    pub single_file: bool,
}

impl Default for DiffPreferences {
    fn default() -> Self {
        Self {
            context_step: DEFAULT_CONTEXT_STEP,
            layout: None,
            tree_visible: true,
            single_file: false,
        }
    }
}

pub const fn status_marker(status: FileDiffStatus) -> char {
    match status {
        FileDiffStatus::Added => 'A',
        FileDiffStatus::Deleted => 'D',
        FileDiffStatus::Modified => 'M',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered_lines(count: u32) -> String {
        use std::fmt::Write as _;
        let mut text = String::new();
        for line in 1..=count {
            let _ = writeln!(text, "line{line}");
        }
        text
    }

    fn file(name: &str, patch: &str) -> FileDiff {
        FileDiff {
            file: name.into(),
            status: FileDiffStatus::Modified,
            additions: 1,
            deletions: 1,
            patch: Some(patch.into()),
            before: None,
            after: None,
            binary: false,
            truncated: false,
        }
    }

    #[test]
    fn patch_rows_carry_the_line_numbers_from_the_hunk_header() {
        let patch = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -10,3 +10,3 @@\n ctx\n-old\n+new\n";
        let rows = rows_from_patch(patch);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].kind, RowKind::Context);
        assert_eq!(rows[0].old, Some(10));
        assert_eq!(rows[0].new, Some(10));
        assert_eq!(rows[1].kind, RowKind::Removed);
        assert_eq!(rows[1].old, Some(11));
        assert_eq!(rows[1].new, None);
        assert_eq!(rows[2].kind, RowKind::Added);
        assert_eq!(rows[2].new, Some(11));
    }

    #[test]
    fn a_second_hunk_opens_with_a_gap() {
        let patch = "@@ -1,1 +1,1 @@\n-a\n+b\n@@ -50,1 +50,1 @@\n-c\n+d\n";
        let rows = rows_from_patch(patch);
        assert!(rows.iter().any(|row| row.kind == RowKind::Gap), "{rows:#?}");
        let second = rows.iter().rev().find(|row| row.kind == RowKind::Removed);
        assert_eq!(second.unwrap().old, Some(50));
    }

    #[test]
    fn one_for_one_replacements_get_word_emphasis() {
        let patch = "@@ -1,1 +1,1 @@\n-let value = compute(1);\n+let value = compute(2);\n";
        let rows = rows_from_patch(patch);
        let added = rows.iter().find(|row| row.kind == RowKind::Added).unwrap();
        assert!(!added.emphasis.is_empty(), "{added:#?}");
        let emphasized: String = added
            .emphasis
            .iter()
            .map(|(start, end)| {
                added
                    .text
                    .chars()
                    .skip(*start)
                    .take(end - start)
                    .collect::<String>()
            })
            .collect();
        assert!(emphasized.contains('2'), "{emphasized:?}");
        assert!(!emphasized.contains("let"), "{emphasized:?}");
    }

    #[test]
    fn a_whole_line_rewrite_keeps_no_emphasis() {
        let patch = "@@ -1,1 +1,1 @@\n-alpha beta\n+gamma delta\n";
        let rows = rows_from_patch(patch);
        for row in &rows {
            assert!(row.emphasis.is_empty(), "{row:#?}");
        }
    }

    #[test]
    fn before_after_pairs_respect_the_context_setting() {
        let before = numbered_lines(60);
        let after = before.replace("line30\n", "LINE30\n");
        let tight = rows_from_texts(&before, &after, Some(2));
        let wide = rows_from_texts(&before, &after, None);
        assert!(tight.len() < wide.len(), "{} {}", tight.len(), wide.len());
        assert!(wide.len() >= 60);
        assert!(tight.iter().any(|row| row.text == "LINE30"));
    }

    #[test]
    fn split_pairs_put_a_replacement_beside_what_it_replaced() {
        let rows = rows_from_patch("@@ -1,3 +1,3 @@\n ctx\n-old\n+new\n");
        let pairs = split_pairs(&rows);
        assert_eq!(pairs[0], (Some(0), Some(0)));
        assert_eq!(pairs[1], (Some(1), Some(2)));
        assert_eq!(pairs.len(), 2);
    }

    #[test]
    fn split_pairs_handle_uneven_runs() {
        let rows = rows_from_patch("@@ -1,3 +1,4 @@\n-a\n-b\n+x\n+y\n+z\n");
        let pairs = split_pairs(&rows);
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[2].0, None);
        assert!(pairs[2].1.is_some());
    }

    #[test]
    fn the_tree_nests_directories_and_collapses_single_chains() {
        let files = vec![
            file("crates/a/src/one.rs", "@@ -1,1 +1,1 @@\n-a\n+b\n"),
            file("crates/a/src/two.rs", "@@ -1,1 +1,1 @@\n-a\n+b\n"),
            file("README.md", "@@ -1,1 +1,1 @@\n-a\n+b\n"),
        ];
        let tree = FileTree::build(&files);
        let rows = tree.flatten(&tree.all_directories());
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names[0], "crates/a/src", "single-child chain collapses");
        assert!(names.contains(&"one.rs"));
        assert!(names.contains(&"README.md"));
        // Directories sort before files at the same level.
        assert!(rows[0].directory);
    }

    #[test]
    fn collapsing_a_directory_hides_its_files() {
        let files = vec![file("src/one.rs", "@@ -1,1 +1,1 @@\n-a\n+b\n")];
        let tree = FileTree::build(&files);
        let open = tree.flatten(&tree.all_directories());
        let closed = tree.flatten(&BTreeSet::new());
        assert_eq!(open.len(), 2);
        assert_eq!(closed.len(), 1);
    }

    fn viewer() -> DiffViewer {
        let files = vec![
            file(
                "src/one.rs",
                "@@ -1,4 +1,4 @@\n ctx\n-old\n+new\n ctx2\n@@ -40,2 +40,2 @@\n-far\n+near\n",
            ),
            file("src/two.rs", "@@ -1,2 +1,2 @@\n-alpha\n+beta\n"),
        ];
        DiffViewer::new(DiffSource::WorkingTree, files, DiffPreferences::default())
    }

    #[test]
    fn the_cursor_starts_on_the_first_body_row_and_steps_over_chrome() {
        let mut viewer = viewer();
        viewer.viewport.set(40);
        let start = viewer.cursor.expect("a cursor");
        assert!(viewer.display[start].selectable());
        viewer.move_cursor(true);
        assert!(viewer.display[viewer.cursor.unwrap()].selectable());
        // Walking to the end never leaves the cursor on a header or spacer.
        for _ in 0..100 {
            viewer.move_cursor(true);
        }
        assert!(viewer.display[viewer.cursor.unwrap()].selectable());
    }

    #[test]
    fn change_jumps_stop_once_per_edit_and_cross_files() {
        let mut viewer = viewer();
        viewer.viewport.set(40);
        let mut stops = Vec::new();
        for _ in 0..6 {
            viewer.jump_change(true);
            stops.push(viewer.cursor_line());
        }
        // Three edits total: one.rs has two, two.rs has one.
        let files: Vec<Option<usize>> = stops
            .iter()
            .map(|_| viewer.cursor_file())
            .collect::<Vec<_>>();
        assert!(files.contains(&Some(1)), "reached the second file");
        assert!(stops.iter().any(Option::is_some));
    }

    #[test]
    fn file_jumps_follow_tree_order() {
        let mut viewer = viewer();
        viewer.viewport.set(40);
        assert_eq!(viewer.active_file, 0);
        viewer.jump_file(true);
        assert_eq!(viewer.active_file, 1);
        viewer.jump_file(true);
        assert_eq!(viewer.active_file, 1, "clamped at the last file");
        viewer.jump_file(false);
        assert_eq!(viewer.active_file, 0);
    }

    #[test]
    fn single_file_mode_shows_only_the_active_patch() {
        let mut viewer = viewer();
        viewer.viewport.set(40);
        let all = viewer.display.len();
        viewer.single_file = true;
        viewer.rebuild_display();
        assert!(viewer.display.len() < all);
        assert!(
            viewer
                .display
                .iter()
                .all(|row| row.file().is_none_or(|file| file == 0))
        );
    }

    #[test]
    fn split_layout_produces_paired_rows() {
        let mut viewer = viewer();
        viewer.pane_width.set(MIN_SPLIT_WIDTH);
        viewer.layout = Some(DiffLayout::Split);
        viewer.rebuild_display();
        assert!(
            viewer
                .display
                .iter()
                .any(|row| matches!(row, DisplayRow::Split { .. }))
        );
        // A pane too narrow for two columns silently reads as unified, so a
        // small terminal never crushes the review.
        viewer.pane_width.set(MIN_SPLIT_WIDTH - 1);
        viewer.rebuild_display();
        assert!(
            viewer
                .display
                .iter()
                .all(|row| !matches!(row, DisplayRow::Split { .. }))
        );
    }

    #[test]
    fn comments_upsert_and_remove_and_format_for_the_model() {
        let mut viewer = viewer();
        viewer.upsert_comment("src/one.rs".into(), Some(12), "tighten this");
        viewer.upsert_comment("src/two.rs".into(), None, "whole file");
        assert_eq!(viewer.comments.len(), 2);
        viewer.upsert_comment("src/one.rs".into(), Some(12), "tighten this properly");
        assert_eq!(viewer.comments.len(), 2);
        assert_eq!(viewer.comments[0].text, "tighten this properly");
        let text = format_comments(&viewer.comments);
        assert!(text.contains("line 12 of src/one.rs: tighten this properly"));
        assert!(text.contains("this file of src/two.rs: whole file"));
        viewer.upsert_comment("src/one.rs".into(), Some(12), "   ");
        assert_eq!(viewer.comments.len(), 1);
    }

    #[test]
    fn reviewed_marks_toggle_for_the_targeted_file() {
        let mut viewer = viewer();
        viewer.viewport.set(40);
        let (name, marked) = viewer.toggle_reviewed().unwrap();
        assert!(marked);
        assert!(viewer.reviewed.contains(&name));
        let (_, marked) = viewer.toggle_reviewed().unwrap();
        assert!(!marked);
        assert!(viewer.reviewed.is_empty());
    }

    #[test]
    fn context_changes_recut_snapshot_bodies() {
        let before = numbered_lines(40);
        let after = before.replace("line20\n", "LINE20\n");
        let files = vec![FileDiff {
            file: "big.txt".into(),
            status: FileDiffStatus::Modified,
            additions: 1,
            deletions: 1,
            patch: None,
            before: Some(before),
            after: Some(after),
            binary: false,
            truncated: false,
        }];
        let mut viewer = DiffViewer::new(
            DiffSource::LastTurn,
            files.clone(),
            DiffPreferences::default(),
        );
        let tight = viewer.rows[0].rows.len();
        viewer.context_step = CONTEXT_STEPS.len() - 1;
        viewer.set_files(files);
        assert!(viewer.rows[0].rows.len() > tight);
    }

    #[test]
    fn binary_and_bodyless_files_explain_themselves() {
        let binary = FileDiff {
            file: "logo.png".into(),
            status: FileDiffStatus::Modified,
            additions: 0,
            deletions: 0,
            patch: None,
            before: None,
            after: None,
            binary: true,
            truncated: false,
        };
        assert_eq!(
            file_rows(&binary, None).note.as_deref(),
            Some("Binary content changed")
        );
        let capped = FileDiff {
            binary: false,
            truncated: true,
            ..binary
        };
        assert!(
            file_rows(&capped, None)
                .note
                .unwrap()
                .contains("review budget")
        );
    }

    #[test]
    fn tree_navigation_expands_collapses_and_steps_out() {
        let mut viewer = viewer();
        viewer.tree_viewport.set(20);
        viewer.focus = DiffFocus::Tree;
        assert_eq!(viewer.tree_rows().len(), 3, "one directory, two files");
        viewer.tree_selection = 0;
        viewer.collapse_tree_row();
        assert_eq!(viewer.tree_rows().len(), 1);
        viewer.expand_tree_row();
        assert_eq!(viewer.tree_rows().len(), 3);
        viewer.move_tree_selection(2);
        viewer.collapse_tree_row();
        assert_eq!(viewer.tree_selection, 0, "stepped out to the parent");
    }

    #[test]
    fn activating_a_tree_file_row_focuses_that_patch() {
        let mut viewer = viewer();
        viewer.viewport.set(40);
        viewer.tree_viewport.set(20);
        viewer.focus = DiffFocus::Tree;
        viewer.tree_selection = 2;
        viewer.activate_tree_row();
        assert_eq!(viewer.active_file, 1);
        assert_eq!(viewer.focus, DiffFocus::Patch);
    }

    #[test]
    fn sources_round_trip_their_command_arguments() {
        assert_eq!(DiffSource::parse(""), Some(DiffSource::WorkingTree));
        assert_eq!(DiffSource::parse("branch"), Some(DiffSource::Branch));
        assert_eq!(DiffSource::parse("last"), Some(DiffSource::LastTurn));
        assert_eq!(DiffSource::parse("nonsense"), None);
        assert_eq!(DiffSource::WorkingTree.query(), Some("working"));
        assert_eq!(DiffSource::LastTurn.query(), None);
    }
}
