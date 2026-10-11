//! The full-screen review surface behind `/diff`.
//!
//! A header naming the source, a file tree on the left, stacked (or
//! side-by-side) patches on the right, and one footer of live key hints.
//! Every decision about *what* to show already happened in `crate::diffview`;
//! this module only turns those rows into styled cells.

use crate::width::DisplayWidth as _;
use haarness_opentui::{Line, Span, attr};

use crate::diffview::{
    CharRange, DiffFocus, DiffRow, DiffViewer, DisplayRow, RowKind, TreeRow, status_marker,
};
use crate::state::App;
use crate::theme::Theme;
use crate::view::frame::{HitRegion, HitTarget};
use crate::view::span::single_line;

/// Rows spent on chrome: header, its rule, the footer rule, and the footer.
const CHROME_ROWS: usize = 4;
/// Below this the tree costs more than it gives back.
const MIN_TREE_WIDTH: usize = 76;
/// Widest line-number gutter before numbers stop earning their columns.
const MAX_DIGITS: usize = 5;
/// Shown while the review is still on its way from the server.
const LOADING_MESSAGE: &str = " Loading review…";

/// Build the whole frame for the review viewer.
pub fn frame(
    app: &App,
    viewer: &DiffViewer,
    width: usize,
    height: usize,
) -> (Vec<Line>, Vec<HitRegion>) {
    let theme = &app.theme;
    let body_height = height.saturating_sub(CHROME_ROWS).max(1);
    let tree_width = tree_width(viewer, width);
    let patch_width = width.saturating_sub(tree_width + usize::from(tree_width > 0));
    viewer.viewport.set(body_height);
    viewer.tree_viewport.set(body_height);

    let mut lines = Vec::with_capacity(height);
    let mut hits = Vec::new();
    lines.push(header(app, viewer, width));
    lines.push(rule(theme, width));

    if viewer.help {
        lines.extend(help_body(theme, width, body_height));
    } else {
        let tree = if tree_width > 0 {
            tree_rows(app, viewer, tree_width, body_height, &mut hits)
        } else {
            Vec::new()
        };
        let patch = patch_rows(app, viewer, patch_width, body_height, &mut hits, tree_width);
        for row in 0..body_height {
            let mut spans = Vec::new();
            if tree_width > 0 {
                spans.extend(
                    tree.get(row)
                        .cloned()
                        .unwrap_or_else(|| blank(tree_width, theme))
                        .spans,
                );
                spans.push(Span::new("│", theme.hairline()));
            }
            spans.extend(
                patch
                    .get(row)
                    .cloned()
                    .unwrap_or_else(|| blank(patch_width, theme))
                    .spans,
            );
            lines.push(Line::new(spans));
        }
    }

    lines.push(rule(theme, width));
    lines.push(footer(app, viewer, width));
    // Tree/patch hit rows are recorded relative to the body; shift them past
    // the header and its rule.
    for hit in &mut hits {
        hit.row += 2;
    }
    (lines, hits)
}

fn tree_width(viewer: &DiffViewer, width: usize) -> usize {
    if !viewer.tree_showing() || viewer.help || width < MIN_TREE_WIDTH {
        return 0;
    }
    crate::diffview::TREE_WIDTH.min(width / 3)
}

fn blank(width: usize, theme: &Theme) -> Line {
    Line::new(vec![Span::new(" ".repeat(width), theme.muted)])
}

fn rule(theme: &Theme, width: usize) -> Line {
    Line::plain("─".repeat(width), theme.hairline())
}

// ---------------------------------------------------------------------------
// Header and footer
// ---------------------------------------------------------------------------

fn header(app: &App, viewer: &DiffViewer, width: usize) -> Line {
    let theme = &app.theme;
    let (added, removed) = viewer.totals();
    let mut right = Vec::new();
    if viewer.loading {
        right.push("loading…".to_owned());
    }
    if let Some(omitted) = viewer.omitted_files {
        // The server bounded the review. Say so where the file count is, so
        // the reader never takes a short list for a small change.
        right.push(format!("{omitted} not shown"));
    }
    if let Some(line) = viewer.cursor_line() {
        right.push(format!("line {line}"));
    }
    if !viewer.comments.is_empty() {
        right.push(format!(
            "{} comment{}",
            viewer.comments.len(),
            if viewer.comments.len() == 1 { "" } else { "s" }
        ));
    }
    if !viewer.reviewed.is_empty() {
        right.push(format!("{} reviewed", viewer.reviewed.len()));
    }
    right.push(format!("context {}", viewer.context_label()));
    right.push(format!(
        "{} file{}",
        viewer.files.len(),
        if viewer.files.len() == 1 { "" } else { "s" }
    ));
    let right = right.join(" · ");
    let counts = format!("+{added} −{removed}");
    let left = format!(" Diff  {}  ", viewer.source.label());
    let used = left.width() + counts.width() + 2 + right.width() + 1;
    let pad = width.saturating_sub(used);
    let mut spans = vec![
        Span::new(" Diff ", theme.primary).with_attributes(attr::BOLD),
        Span::new(format!(" {}  ", viewer.source.label()), theme.text),
        Span::new(format!("+{added}"), theme.diff.added),
        Span::new(format!(" −{removed}"), theme.diff.removed),
    ];
    if pad > 0 {
        spans.push(Span::new(" ".repeat(pad), theme.muted));
        spans.push(Span::new(format!("{right} "), theme.muted));
    }
    Line::new(spans)
}

fn footer(app: &App, viewer: &DiffViewer, width: usize) -> Line {
    let theme = &app.theme;
    if let Some(draft) = &viewer.comment_input {
        let target = viewer
            .comment_target()
            .map(|(file, line)| match line {
                Some(line) => format!("{file}:{line}"),
                None => file,
            })
            .unwrap_or_default();
        let label = format!(" comment {target} ▸ ");
        let body = single_line(draft, width.saturating_sub(label.width() + 2));
        return Line::new(vec![
            Span::new(label, theme.accent).with_attributes(attr::BOLD),
            Span::new(body, theme.text),
            Span::new("▏", theme.primary),
        ]);
    }
    let hints: &[(&str, &str)] = if viewer.help {
        &[("esc", "back")]
    } else if viewer.focus == DiffFocus::Tree {
        &[
            ("tab", "patch"),
            ("↵", "open"),
            ("→←", "fold"),
            ("a", "expand all"),
            ("r", "reviewed"),
            ("c", "comment"),
            ("?", "keys"),
            ("q", "close"),
        ]
    } else {
        &[
            ("tab", "tree"),
            ("n/p", "change"),
            ("]/[", "file"),
            ("v", "split"),
            ("x", "context"),
            ("r", "reviewed"),
            ("c", "comment"),
            ("s", "source"),
            ("?", "keys"),
            ("q", "close"),
        ]
    };
    let mut spans = vec![Span::new(" ", theme.muted)];
    let mut used = 1;
    for (index, (key, label)) in hints.iter().enumerate() {
        let separator = if index == 0 { "" } else { "  " };
        let cost = separator.width() + key.width() + 1 + label.width();
        if used + cost + 1 > width {
            break;
        }
        used += cost;
        if !separator.is_empty() {
            spans.push(Span::new(separator, theme.muted));
        }
        spans.push(Span::new((*key).to_owned(), theme.text).with_attributes(attr::BOLD));
        spans.push(Span::new(format!(" {label}"), theme.muted));
    }
    Line::new(spans)
}

// ---------------------------------------------------------------------------
// File tree
// ---------------------------------------------------------------------------

fn tree_rows(
    app: &App,
    viewer: &DiffViewer,
    width: usize,
    height: usize,
    hits: &mut Vec<HitRegion>,
) -> Vec<Line> {
    let theme = &app.theme;
    let rows = viewer.tree_rows();
    if rows.is_empty() {
        return vec![Line::new(vec![
            Span::new(" No files", theme.muted),
            Span::new(" ".repeat(width.saturating_sub(9)), theme.muted),
        ])];
    }
    let commented = viewer.commented_files();
    let start = viewer.tree_scroll.min(rows.len().saturating_sub(1));
    let mut lines = Vec::new();
    for (offset, row) in rows.iter().skip(start).take(height).enumerate() {
        let index = start + offset;
        let focused = viewer.focus == DiffFocus::Tree && index == viewer.tree_selection;
        let active = row.file.is_some_and(|file| file == viewer.active_file);
        let reviewed = row.file.is_some_and(|file| viewer.is_reviewed(file));
        let has_comment = row
            .file
            .and_then(|file| viewer.files.get(file))
            .is_some_and(|file| commented.contains(file.file.as_str()));
        lines.push(tree_row(
            app,
            viewer,
            row,
            &rows,
            index,
            width,
            TreeRowState {
                focused,
                active,
                reviewed,
                has_comment,
            },
        ));
        hits.push(HitRegion {
            row: offset,
            start: 0,
            end: width,
            target: HitTarget::DiffTreeRow(index),
            paint_hover: true,
        });
    }
    lines
}

/// How one tree row should read: the four independent marks the sidebar
/// carries, kept together so the row painter takes one argument for them.
#[allow(clippy::struct_excessive_bools)]
struct TreeRowState {
    focused: bool,
    active: bool,
    reviewed: bool,
    has_comment: bool,
}

fn tree_row(
    app: &App,
    viewer: &DiffViewer,
    row: &TreeRow,
    rows: &[TreeRow],
    index: usize,
    width: usize,
    state: TreeRowState,
) -> Line {
    let TreeRowState {
        focused,
        active,
        reviewed,
        has_comment,
    } = state;
    let theme = &app.theme;
    let prefix = tree_prefix(rows, index, row, viewer);
    let status = row.file.map_or_else(String::new, |file| {
        let marker = viewer
            .files
            .get(file)
            .map_or('?', |file| status_marker(file.status));
        format!(
            "{}{}{marker}",
            if has_comment { '*' } else { ' ' },
            if reviewed { '✓' } else { ' ' }
        )
    });
    let name_width = width
        .saturating_sub(prefix.width() + status.width() + 2)
        .max(1);
    let name = single_line(&row.name, name_width);
    let pad = width.saturating_sub(prefix.width() + name.width() + status.width() + 2);
    let (foreground, background) = if focused {
        (theme.bg, Some(theme.primary))
    } else if active {
        (theme.primary, None)
    } else if reviewed || row.directory {
        (theme.muted, None)
    } else {
        (theme.text, None)
    };
    let paint = |text: String, color| {
        let mut span = Span::new(text, color);
        if let Some(background) = background {
            span = span.on(background);
        }
        span
    };
    Line::new(vec![
        paint(
            format!(" {prefix}"),
            if focused { theme.bg } else { theme.hairline() },
        ),
        paint(name, foreground),
        paint(" ".repeat(pad), foreground),
        paint(status, if focused { theme.bg } else { theme.muted }),
        paint(" ".to_owned(), theme.muted),
    ])
}

/// `│  ├─ ▾ ` scaffolding, so a nested tree reads without indentation guesswork.
fn tree_prefix(rows: &[TreeRow], index: usize, row: &TreeRow, viewer: &DiffViewer) -> String {
    let mut prefix = String::new();
    for depth in 0..row.depth {
        if has_later_sibling(rows, index, depth) {
            prefix.push_str("│  ");
        } else {
            prefix.push_str("   ");
        }
    }
    if index == 0 && row.depth == 0 {
        prefix.push(' ');
    } else if has_later_sibling(rows, index, row.depth) {
        prefix.push_str("├─ ");
    } else {
        prefix.push_str("└─ ");
    }
    if row.directory {
        prefix.push_str(if viewer.expanded.contains(&row.id) {
            "▾ "
        } else {
            "▸ "
        });
    }
    prefix
}

fn has_later_sibling(rows: &[TreeRow], index: usize, depth: usize) -> bool {
    rows.iter()
        .skip(index + 1)
        .find(|row| row.depth <= depth)
        .is_some_and(|row| row.depth == depth)
}

// ---------------------------------------------------------------------------
// Patch pane
// ---------------------------------------------------------------------------

fn patch_rows(
    app: &App,
    viewer: &DiffViewer,
    width: usize,
    height: usize,
    hits: &mut Vec<HitRegion>,
    tree_width: usize,
) -> Vec<Line> {
    let theme = &app.theme;
    if viewer.files.is_empty() {
        // The viewer opens before its review arrives, so an empty pane means
        // "still loading" as often as it means "nothing changed".
        let message = if viewer.loading {
            LOADING_MESSAGE
        } else {
            " No changes"
        };
        return vec![Line::new(vec![
            Span::new(message, theme.muted),
            Span::new(
                " ".repeat(width.saturating_sub(message.width())),
                theme.muted,
            ),
        ])];
    }
    let start = viewer.scroll.min(viewer.display.len().saturating_sub(1));
    let mut lines = Vec::new();
    for (offset, row) in viewer.display.iter().skip(start).take(height).enumerate() {
        let index = start + offset;
        let cursor = viewer.cursor == Some(index) && viewer.focus == DiffFocus::Patch;
        lines.push(match row {
            DisplayRow::Header(file) => file_header(app, viewer, *file, width),
            DisplayRow::Spacer => blank(width, theme),
            DisplayRow::Note { text, .. } => Line::new(vec![
                Span::new(
                    format!("  {}", single_line(text, width.saturating_sub(3))),
                    theme.accent,
                ),
                Span::new(
                    " ".repeat(
                        width
                            .saturating_sub(2 + single_line(text, width.saturating_sub(3)).width()),
                    ),
                    theme.muted,
                ),
            ]),
            DisplayRow::Body { file, row } => unified_row(app, viewer, *file, *row, width, cursor),
            DisplayRow::Split { file, left, right } => {
                split_row(app, viewer, *file, *left, *right, width, cursor)
            }
        });
        hits.push(HitRegion {
            row: offset,
            start: tree_width + usize::from(tree_width > 0),
            end: tree_width + usize::from(tree_width > 0) + width,
            target: HitTarget::DiffBodyRow(index),
            paint_hover: false,
        });
    }
    lines
}

fn file_header(app: &App, viewer: &DiffViewer, file: usize, width: usize) -> Line {
    let theme = &app.theme;
    let Some(entry) = viewer.files.get(file) else {
        return blank(width, theme);
    };
    let reviewed = viewer.is_reviewed(file);
    let active = file == viewer.active_file;
    let counts = format!("+{} −{}", entry.additions, entry.deletions);
    let status = match entry.status {
        haarness_protocol::FileDiffStatus::Added => "added",
        haarness_protocol::FileDiffStatus::Deleted => "deleted",
        haarness_protocol::FileDiffStatus::Modified => "modified",
    };
    let tail = if reviewed {
        format!("{status} · reviewed  ")
    } else {
        format!("{status}  ")
    };
    let name_width = width.saturating_sub(counts.width() + tail.width() + 6);
    let name = single_line(&entry.file, name_width.max(8));
    let pad = width.saturating_sub(3 + name.width() + counts.width() + 2 + tail.width());
    let name_color = if reviewed { theme.muted } else { theme.text };
    let surface = theme.surface;
    Line::new(vec![
        Span::new(if active { " ▎ " } else { "   " }, theme.primary).on(surface),
        Span::new(name, name_color)
            .with_attributes(attr::BOLD)
            .on(surface),
        Span::new(" ".repeat(pad), theme.muted).on(surface),
        Span::new(
            format!("+{}", entry.additions),
            if reviewed {
                theme.muted
            } else {
                theme.diff.added
            },
        )
        .on(surface),
        Span::new(
            format!(" −{}", entry.deletions),
            if reviewed {
                theme.muted
            } else {
                theme.diff.removed
            },
        )
        .on(surface),
        Span::new(format!("  {tail}"), theme.muted).on(surface),
    ])
}

/// Widest line number in a file, so the gutter is exactly as wide as it needs.
fn digits(viewer: &DiffViewer, file: usize) -> usize {
    let widest = viewer.rows.get(file).map_or(0, |rows| {
        rows.rows
            .iter()
            .filter_map(|row| row.new.or(row.old))
            .max()
            .unwrap_or(0)
    });
    widest.to_string().len().clamp(2, MAX_DIGITS)
}

struct RowStyle {
    background: Option<haarness_opentui::Color>,
    sign: char,
    sign_color: haarness_opentui::Color,
    gutter_background: Option<haarness_opentui::Color>,
}

fn row_style(theme: &Theme, kind: RowKind, cursor: bool, commented: bool) -> RowStyle {
    let (mut background, sign, sign_color) = match kind {
        RowKind::Added => (Some(theme.diff.added_bg), '+', theme.diff.added),
        RowKind::Removed => (Some(theme.diff.removed_bg), '-', theme.diff.removed),
        RowKind::Context | RowKind::Gap => (None, ' ', theme.muted),
    };
    if commented {
        background = Some(Theme::mix(
            background.unwrap_or(theme.bg),
            theme.primary,
            0.14,
        ));
    }
    if cursor {
        background = Some(Theme::mix(
            background.unwrap_or(theme.bg),
            theme.primary,
            0.30,
        ));
    }
    let gutter_background = background.map(|color| Theme::mix(color, theme.bg, 0.35));
    RowStyle {
        background,
        sign,
        sign_color,
        gutter_background,
    }
}

fn paint(
    text: impl Into<String>,
    color: haarness_opentui::Color,
    background: Option<haarness_opentui::Color>,
) -> Span {
    let span = Span::new(text, color);
    match background {
        Some(background) => span.on(background),
        None => span,
    }
}

fn unified_row(
    app: &App,
    viewer: &DiffViewer,
    file: usize,
    index: usize,
    width: usize,
    cursor: bool,
) -> Line {
    let theme = &app.theme;
    let Some(row) = viewer.rows.get(file).and_then(|rows| rows.rows.get(index)) else {
        return blank(width, theme);
    };
    let digits = digits(viewer, file);
    if row.kind == RowKind::Gap {
        let gutter = " ".repeat(digits * 2 + 3);
        let body = format!("{gutter}⋮");
        return Line::new(vec![
            Span::new(body.clone(), theme.hairline()),
            Span::new(" ".repeat(width.saturating_sub(body.width())), theme.muted),
        ]);
    }
    let commented = is_commented(viewer, file, row);
    let style = row_style(theme, row.kind, cursor, commented);
    let marker = if commented { '●' } else { ' ' };
    let gutter = format!(
        "{marker}{} {} ",
        number(row.old, digits),
        number(row.new, digits)
    );
    let content_width = width.saturating_sub(gutter.width() + 2);
    let mut spans = vec![
        paint(gutter, theme.muted, style.gutter_background),
        paint(
            format!("{} ", style.sign),
            style.sign_color,
            style.background,
        ),
    ];
    let (content, used) = content_spans(app, viewer, file, row, content_width, &style);
    spans.extend(content);
    let pad = width.saturating_sub(digits * 2 + 3 + 2 + used);
    if pad > 0 {
        spans.push(paint(" ".repeat(pad), theme.muted, style.background));
    }
    Line::new(spans)
}

fn split_row(
    app: &App,
    viewer: &DiffViewer,
    file: usize,
    left: Option<usize>,
    right: Option<usize>,
    width: usize,
    cursor: bool,
) -> Line {
    let theme = &app.theme;
    let rows = viewer.rows.get(file).map(|rows| &rows.rows);
    let gap = left
        .or(right)
        .and_then(|index| rows.and_then(|rows| rows.get(index)))
        .is_some_and(|row| row.kind == RowKind::Gap);
    if gap {
        let body = "  ⋮";
        return Line::new(vec![
            Span::new(body, theme.hairline()),
            Span::new(" ".repeat(width.saturating_sub(body.width())), theme.muted),
        ]);
    }
    let side_width = width.saturating_sub(1) / 2;
    let digits = digits(viewer, file);
    let mut spans = side_spans(app, viewer, file, left, side_width, digits, cursor, true);
    spans.push(Span::new("│", theme.hairline()));
    spans.extend(side_spans(
        app,
        viewer,
        file,
        right,
        width.saturating_sub(side_width + 1),
        digits,
        cursor,
        false,
    ));
    Line::new(spans)
}

#[allow(clippy::too_many_arguments)]
fn side_spans(
    app: &App,
    viewer: &DiffViewer,
    file: usize,
    index: Option<usize>,
    width: usize,
    digits: usize,
    cursor: bool,
    old_side: bool,
) -> Vec<Span> {
    let theme = &app.theme;
    let Some(row) =
        index.and_then(|index| viewer.rows.get(file).and_then(|rows| rows.rows.get(index)))
    else {
        // The other side inserted or deleted here; an empty band reads as
        // "nothing on this side" without pretending a line exists.
        let background = if cursor {
            Some(Theme::mix(theme.bg, theme.primary, 0.30))
        } else {
            Some(Theme::mix(theme.bg, theme.muted, 0.10))
        };
        return vec![paint(" ".repeat(width), theme.muted, background)];
    };
    // In split view the left column is the pre-image and the right the post.
    if old_side && row.kind == RowKind::Added || !old_side && row.kind == RowKind::Removed {
        let background = if cursor {
            Some(Theme::mix(theme.bg, theme.primary, 0.30))
        } else {
            Some(Theme::mix(theme.bg, theme.muted, 0.10))
        };
        return vec![paint(" ".repeat(width), theme.muted, background)];
    }
    let commented = is_commented(viewer, file, row);
    let style = row_style(theme, row.kind, cursor, commented);
    let marker = if commented { '●' } else { ' ' };
    let number_text = number(if old_side { row.old } else { row.new }, digits);
    let gutter = format!("{marker}{number_text} ");
    let content_width = width.saturating_sub(gutter.width() + 2);
    let mut spans = vec![
        paint(gutter, theme.muted, style.gutter_background),
        paint(
            format!("{} ", style.sign),
            style.sign_color,
            style.background,
        ),
    ];
    let (content, used) = content_spans(app, viewer, file, row, content_width, &style);
    spans.extend(content);
    let pad = width.saturating_sub(digits + 2 + 2 + used);
    if pad > 0 {
        spans.push(paint(" ".repeat(pad), theme.muted, style.background));
    }
    spans
}

fn number(value: Option<u32>, digits: usize) -> String {
    value.map_or_else(|| " ".repeat(digits), |value| format!("{value:>digits$}"))
}

fn is_commented(viewer: &DiffViewer, file: usize, row: &DiffRow) -> bool {
    let Some(name) = viewer.files.get(file).map(|file| file.file.as_str()) else {
        return false;
    };
    row.line()
        .is_some_and(|line| viewer.commented_lines(name).contains(&line))
}

/// The code itself: syntax-highlighted, word-emphasis applied, clipped to the
/// pane. Reviewed files go plain so the eye skips them.
fn content_spans(
    app: &App,
    viewer: &DiffViewer,
    file: usize,
    row: &DiffRow,
    width: usize,
    style: &RowStyle,
) -> (Vec<Span>, usize) {
    let theme = &app.theme;
    let text = single_line(&row.text, width);
    if text.is_empty() {
        return (Vec::new(), 0);
    }
    let reviewed = viewer.is_reviewed(file);
    let language = viewer
        .files
        .get(file)
        .and_then(|file| file.file.rsplit_once('.'))
        .map(|(_, extension)| extension.to_owned())
        .unwrap_or_default();
    let base: Vec<Span> = if reviewed || language.is_empty() {
        vec![Span::new(text.clone(), theme.muted)]
    } else {
        crate::highlight::highlight_block_transparent(&text, &language, theme)
            .into_iter()
            .next()
            .unwrap_or_else(|| vec![Span::new(text.clone(), theme.text)])
    };
    let emphasized = emphasize(base, &row.emphasis, theme, style);
    let used: usize = emphasized.iter().map(Span::width).sum();
    (emphasized, used)
}

/// Split the highlighted spans at the emphasis boundaries and give the
/// changed runs a stronger fill, so the eye lands on the token that moved.
fn emphasize(spans: Vec<Span>, ranges: &[CharRange], theme: &Theme, style: &RowStyle) -> Vec<Span> {
    let apply = |span: Span, marked: bool| {
        let background = if marked {
            Some(Theme::mix(
                style.background.unwrap_or(theme.bg),
                style.sign_color,
                0.35,
            ))
        } else {
            style.background
        };
        match background {
            Some(background) => span.on(background),
            None => span,
        }
    };
    if ranges.is_empty() {
        return spans.into_iter().map(|span| apply(span, false)).collect();
    }
    let marked = |index: usize| {
        ranges
            .iter()
            .any(|(start, end)| index >= *start && index < *end)
    };
    let mut result = Vec::new();
    let mut column = 0_usize;
    for span in spans {
        let mut run = String::new();
        let mut run_marked = None::<bool>;
        for character in span.text.chars() {
            let is_marked = marked(column);
            if run_marked.is_some_and(|value| value != is_marked) {
                result.push(apply(
                    Span::new(std::mem::take(&mut run), span.fg).with_attributes(span.attributes),
                    run_marked.unwrap_or(false),
                ));
            }
            run_marked = Some(is_marked);
            run.push(character);
            column += 1;
        }
        if !run.is_empty() {
            result.push(apply(
                Span::new(run, span.fg).with_attributes(span.attributes),
                run_marked.unwrap_or(false),
            ));
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

const HELP: &[(&str, &str, &str)] = &[
    (
        "q / esc",
        "Close viewer",
        "Queued comments go to the composer",
    ),
    (
        "tab",
        "Switch focus",
        "Move between the file tree and the patches",
    ),
    (
        "j / k / ↓ ↑",
        "Move cursor",
        "Step the line cursor, revealing it as needed",
    ),
    (
        "n / p",
        "Next / previous change",
        "Jump between edits, not hunks",
    ),
    (
        "] / [",
        "Next / previous file",
        "Select the next changed file",
    ),
    (
        "↵",
        "Open item",
        "Open the file, or fold and unfold a folder",
    ),
    (
        "→ / ←",
        "Expand / collapse",
        "Open the folder, or step out to its parent",
    ),
    (
        "a",
        "Expand all folders",
        "Open every folder in the file tree",
    ),
    ("t", "Toggle file tree", "Show or hide the sidebar"),
    (
        "o",
        "Toggle patches",
        "Show one selected patch or every patch",
    ),
    (
        "v",
        "Toggle view",
        "Switch between split and unified layout",
    ),
    (
        "x",
        "Cycle context",
        "Context lines per change: 4, 12, 50, all",
    ),
    ("r", "Mark reviewed", "Dim a file you have finished reading"),
    ("c", "Comment", "Queue a note for your next message"),
    (
        "s",
        "Switch source",
        "Working tree, main branch, or last turn",
    ),
    (
        "g / G",
        "Top / bottom",
        "Jump to the start or end of the review",
    ),
    (
        "pgup / pgdn",
        "Page",
        "Scroll a screen; ctrl+f/b and ctrl+d/u also work",
    ),
    ("?", "This list", "Every review shortcut"),
];

fn help_body(theme: &Theme, width: usize, height: usize) -> Vec<Line> {
    let mut lines = vec![
        Line::new(vec![
            Span::new("  Review shortcuts", theme.text).with_attributes(attr::BOLD),
        ]),
        Line::empty(),
    ];
    for (key, action, description) in HELP {
        if lines.len() >= height {
            break;
        }
        let key_column: usize = 14;
        let action_column: usize = 24;
        let key_pad = key_column.saturating_sub(key.width());
        let action_pad = action_column.saturating_sub(action.width());
        let description = single_line(
            description,
            width.saturating_sub(key_column + action_column + 4),
        );
        lines.push(Line::new(vec![
            Span::new("  ", theme.muted),
            Span::new((*key).to_owned(), theme.primary).with_attributes(attr::BOLD),
            Span::new(" ".repeat(key_pad), theme.muted),
            Span::new((*action).to_owned(), theme.text),
            Span::new(" ".repeat(action_pad), theme.muted),
            Span::new(description, theme.muted),
        ]));
    }
    while lines.len() < height {
        lines.push(Line::empty());
    }
    lines.truncate(height);
    lines
}
