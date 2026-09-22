//! Stacked diff rendering for edit/write tool results: grouped hunks, two
//! context lines, line numbers, and word-level emphasis inside changed
//! lines — the quiet way to show exactly what changed.

use haarness_opentui::{Line, Span, attr};
use similar::{ChangeTag, TextDiff};

use crate::theme::Theme;

const MAX_ROWS: usize = 200;
const CONTEXT: usize = 2;

pub fn render_diff(old: &str, new: &str, width: usize, theme: &Theme, indent: &str) -> Vec<Line> {
    let diff = TextDiff::from_lines(old, new);
    let digits = old
        .lines()
        .count()
        .max(new.lines().count())
        .to_string()
        .len()
        .max(2);
    let inner = width
        .saturating_sub(indent.chars().count() + digits + 1)
        .max(10);
    let mut lines = Vec::new();
    let mut truncated = 0_usize;
    for (group_index, group) in diff.grouped_ops(CONTEXT).iter().enumerate() {
        if group_index > 0 && lines.len() < MAX_ROWS {
            lines.push(Line::plain(
                format!("{indent}{}  ⋮", " ".repeat(digits + 1)),
                theme.muted,
            ));
        }
        for op in group {
            for change in diff.iter_inline_changes(op) {
                if lines.len() >= MAX_ROWS {
                    truncated += 1;
                    continue;
                }
                // The side the line lives on numbers it.
                let number = match change.tag() {
                    ChangeTag::Delete => change.old_index(),
                    _ => change.new_index(),
                }
                .map_or_else(
                    || " ".repeat(digits),
                    |index| format!("{:>digits$}", index + 1),
                );
                let gutter = Span::new(format!("{indent}{number} "), theme.muted);
                let row = match change.tag() {
                    ChangeTag::Insert => inline_row(gutter, '+', &change, inner, theme, true),
                    ChangeTag::Delete => inline_row(gutter, '-', &change, inner, theme, false),
                    ChangeTag::Equal => {
                        let text: String = change
                            .iter_strings_lossy()
                            .map(|(_, value)| value.into_owned())
                            .collect();
                        Line::new(vec![
                            gutter,
                            Span::new(
                                format!("  {}", clip(text.trim_end_matches('\n'), inner)),
                                theme.muted,
                            ),
                        ])
                    }
                };
                lines.push(row);
            }
        }
    }
    if truncated > 0 {
        lines.push(Line::plain(
            format!("{indent}  … +{truncated} more lines"),
            theme.muted,
        ));
    }
    lines
}

/// A changed line with its intra-line emphasized runs (the words that
/// actually differ) rendered inverse.
fn inline_row(
    gutter: Span,
    marker: char,
    change: &similar::InlineChange<'_, str>,
    inner: usize,
    theme: &Theme,
    added: bool,
) -> Line {
    let (fg, bg) = if added {
        (theme.diff.added, theme.diff.added_bg)
    } else {
        (theme.diff.removed, theme.diff.removed_bg)
    };
    let mut spans = vec![gutter, Span::new(format!("{marker} "), fg).on(bg)];
    let mut used = 2usize;
    for (emphasized, value) in change.iter_strings_lossy() {
        let value = value.trim_end_matches('\n');
        if value.is_empty() {
            continue;
        }
        let remaining = inner.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let clipped = clip(value, remaining);
        used += clipped.chars().count();
        let mut span = Span::new(clipped, fg).on(bg);
        if emphasized {
            span.attributes |= attr::INVERSE;
        }
        spans.push(span);
    }
    let pad = inner.saturating_sub(used);
    if pad > 0 {
        spans.push(Span::new(" ".repeat(pad), fg).on(bg));
    }
    Line::new(spans)
}

/// All-added rendering for freshly written files, capped short.
pub fn render_added(
    content: &str,
    width: usize,
    theme: &Theme,
    indent: &str,
    cap: usize,
) -> Vec<Line> {
    let inner = width.saturating_sub(indent.chars().count()).max(10);
    let mut lines: Vec<Line> = content
        .lines()
        .take(cap)
        .map(|row| styled_row(indent, '+', row, inner, theme, true))
        .collect();
    let total = content.lines().count();
    if total > cap {
        lines.push(Line::plain(
            format!("{indent}  … +{} more lines", total - cap),
            theme.muted,
        ));
    }
    lines
}

fn styled_row(
    indent: &str,
    marker: char,
    text: &str,
    inner: usize,
    theme: &Theme,
    added: bool,
) -> Line {
    let (fg, bg) = if added {
        (theme.diff.added, theme.diff.added_bg)
    } else {
        (theme.diff.removed, theme.diff.removed_bg)
    };
    let body = format!("{marker} {}", clip(text, inner.saturating_sub(2)));
    let pad = inner.saturating_sub(body.chars().count());
    Line::new(vec![
        Span::new(indent, theme.muted),
        Span::new(body, fg).on(bg),
        Span::new(" ".repeat(pad), fg).on(bg),
    ])
}

fn clip(text: &str, width: usize) -> String {
    crate::view::span::single_line(text, width)
}

#[cfg(test)]
mod tests {
    use super::{render_added, render_diff};
    use crate::theme::Theme;

    fn text_of(lines: &[haarness_opentui::Line]) -> Vec<String> {
        lines.iter().map(haarness_opentui::Line::text).collect()
    }

    #[test]
    fn changed_lines_carry_diff_colors_and_context_stays_muted() {
        let theme = Theme::editorial_dark();
        let old = "alpha\nbeta\ngamma\ndelta\n";
        let new = "alpha\nBETA\ngamma\ndelta\n";
        let lines = render_diff(old, new, 60, &theme, "    ");
        let rows = text_of(&lines);
        assert!(rows.iter().any(|row| row.contains("- beta")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("+ BETA")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("alpha")), "{rows:?}");
        // Line numbers ride in the gutter.
        assert!(
            rows.iter().any(|row| row.trim_start().starts_with('2')),
            "{rows:?}"
        );
        let added = lines
            .iter()
            .find(|line| line.text().contains("+ BETA"))
            .unwrap();
        let marker = added
            .spans
            .iter()
            .find(|span| span.text.contains('+'))
            .unwrap();
        assert_eq!(marker.fg, theme.diff.added);
        assert_eq!(marker.bg, Some(theme.diff.added_bg));
        // The changed word itself is emphasized (inverse).
        assert!(
            added.spans.iter().any(|span| span.text.contains("BETA")
                && span.attributes & haarness_opentui::attr::INVERSE
                    == haarness_opentui::attr::INVERSE),
            "{added:?}"
        );
    }

    #[test]
    fn distant_hunks_are_separated_and_long_diffs_truncate() {
        let theme = Theme::editorial_dark();
        let mut old = String::new();
        for n in 1..=300 {
            use std::fmt::Write as _;
            let _ = writeln!(old, "line{n}");
        }
        let mut new = old.clone();
        new = new.replace("line1\n", "LINE1\n");
        new = new.replace("line300\n", "LINE300\n");
        let lines = render_diff(&old, &new, 60, &theme, "  ");
        let rows = text_of(&lines);
        assert!(rows.iter().any(|row| row.contains('⋮')), "hunk separator");
        assert!(rows.len() <= super::MAX_ROWS + 10, "{}", rows.len());
    }

    #[test]
    fn written_files_render_as_added_with_a_cap() {
        let theme = Theme::editorial_dark();
        let mut content = String::new();
        for n in 1..=60 {
            use std::fmt::Write as _;
            let _ = writeln!(content, "row{n}");
        }
        let lines = render_added(&content, 60, &theme, "  ", 40);
        let rows = text_of(&lines);
        assert!(rows[0].contains("+ row1"));
        assert!(rows.last().unwrap().contains("+20 more lines"), "{rows:?}");
    }
}
