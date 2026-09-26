//! Line/span construction helpers shared by every view.

use haarness_opentui::{Color, Line, Span, attr};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthChar as _;
use unicode_width::UnicodeWidthStr as _;

use crate::theme::Theme;

pub fn line(text: impl Into<String>, foreground: Color) -> Line {
    Line::plain(text, foreground)
}

/// One display line, truncated to `width` with an ellipsis when content was
/// dropped mid-way.
pub fn single_line(value: &str, width: usize) -> String {
    let value = value.replace(['\n', '\r'], " ");
    if value.width() <= width {
        return value;
    }
    if width == 0 {
        return String::new();
    }
    let budget = width.saturating_sub(1);
    let mut used = 0;
    let mut truncated = String::new();
    for grapheme in value.graphemes(true) {
        let cells = grapheme.width();
        if used + cells > budget {
            break;
        }
        truncated.push_str(grapheme);
        used += cells;
    }
    truncated.push('…');
    truncated
}

/// Compact token counts: `950` → `950`, `12400` → `12.4k`.
#[allow(clippy::cast_precision_loss)] // display-only rounding
pub fn compact_count(value: u64) -> String {
    if value < 1000 {
        value.to_string()
    } else {
        let thousands = value as f64 / 1000.0;
        if thousands >= 100.0 {
            format!("{thousands:.0}k")
        } else {
            format!("{thousands:.1}k")
        }
    }
}

/// One attached file: an inverse badge in the attention color followed by the
/// file's name. The badge is the same chip shape the status band uses, so
/// "this is a labelled object" reads the same way everywhere.
pub fn media_chip(badge: &str, name: &str, theme: &Theme) -> Vec<Span> {
    vec![
        Span::new(format!(" {badge} "), theme.bg)
            .on(theme.accent)
            .with_attributes(attr::BOLD),
        Span::new(format!(" {name}"), theme.text),
    ]
}

pub fn pad(value: &str, width: usize) -> String {
    let length = value.width();
    format!("{value}{}", " ".repeat(width.saturating_sub(length)))
}

/// Braille working spinner, advanced by the render tick.
pub fn spinner(tick: u64) -> char {
    const FRAMES: [char; 6] = [
        '\u{280b}', '\u{2819}', '\u{2838}', '\u{2834}', '\u{2826}', '\u{2807}',
    ];
    FRAMES[usize::try_from(tick % 6).unwrap_or(0)]
}

/// One lifecycle language for child agents everywhere they appear. A task
/// launch returning successfully does not mean its child has finished, so
/// callers must pass the durable child state rather than the tool result.
/// Active foreground and background work share the animated working marker;
/// the hollow circle is reserved for work that is actually queued.
pub fn task_state_style(state: &str, tick: u64, theme: &Theme) -> (char, Color) {
    match state {
        "done" => ('✓', theme.success()),
        "failed" => ('✗', theme.danger),
        "cancelled" | "interrupted" => ('⊘', theme.muted),
        "waiting_permission" => ('⚠', theme.accent),
        "running" | "background" => (spinner(tick), theme.accent),
        _ => ('○', theme.muted),
    }
}

pub fn short_id(value: &str) -> &str {
    // UUIDv7 values created together share their leading timestamp bytes, so
    // the first eight characters are visually identical. The random suffix
    // is the useful compact discriminator for session and item IDs.
    value.get(value.len().saturating_sub(8)..).unwrap_or(value)
}

/// Wrap styled runs to `width` display columns, prefixing the first line with
/// `first_prefix` and continuations with `cont_prefix` (same color). Styling
/// survives the wrap: each output line carries spans grouped by their source
/// run's colors and attributes.
pub fn wrap_spans(
    spans: &[Span],
    width: usize,
    first_prefix: &str,
    cont_prefix: &str,
    prefix_color: Color,
) -> Vec<Line> {
    let inner = width
        .saturating_sub(first_prefix.width().max(cont_prefix.width()))
        .max(10);
    // Flatten to (character, style index) so breaks can land anywhere.
    let mut cells: Vec<(char, usize, usize)> = Vec::new();
    for (index, span) in spans.iter().enumerate() {
        for character in span.text.chars() {
            cells.push((character, index, character.width().unwrap_or(0)));
        }
    }
    let mut lines = Vec::new();
    let mut start = 0;
    while start < cells.len() {
        let mut end = start;
        let mut used = 0;
        while end < cells.len() {
            let cells_wide = cells[end].2;
            if end > start && used + cells_wide > inner {
                break;
            }
            used += cells_wide;
            end += 1;
        }
        if end < cells.len() {
            // Prefer breaking at the last space inside the window.
            if let Some(break_at) = (start..end).rev().find(|&index| cells[index].0 == ' ') {
                if break_at > start {
                    end = break_at;
                }
            }
        }
        let prefix = if lines.is_empty() {
            first_prefix
        } else {
            cont_prefix
        };
        let mut line_spans = vec![Span::new(prefix, prefix_color)];
        let mut current: Option<(usize, String)> = None;
        for &(character, style, _) in &cells[start..end] {
            match &mut current {
                Some((active, text)) if *active == style => text.push(character),
                _ => {
                    if let Some((active, text)) = current.take() {
                        line_spans.push(styled_run(&spans[active], text));
                    }
                    current = Some((style, character.to_string()));
                }
            }
        }
        if let Some((active, text)) = current.take() {
            line_spans.push(styled_run(&spans[active], text));
        }
        lines.push(Line::new(line_spans));
        start = end;
        // Consume the whitespace the break landed on.
        while start < cells.len() && cells[start].0 == ' ' {
            start += 1;
        }
    }
    if lines.is_empty() {
        lines.push(Line::plain(first_prefix, prefix_color));
    }
    lines
}

/// Paint an application selection over existing display cells. Haarness uses
/// the theme's primary color for focused rows and selection, rather than a
/// panel shade that can be identical to the ordinary surface. Wide glyphs are
/// treated as indivisible cells so styling never splits their UTF-8 bytes.
pub fn highlight_range(line: &mut Line, from: usize, to: usize, theme: &Theme) {
    if from >= to {
        return;
    }
    style_cells(line, from, to, theme, false, false);
}

/// Full-width active row shared by every picker and autocomplete surface.
pub fn selected_row(line: &mut Line, width: usize, theme: &Theme) {
    style_cells(line, 0, width, theme, true, true);
}

/// Full-width hover/focus treatment shared by all row-tagged affordances.
pub fn interactive_region(line: &mut Line, from: usize, to: usize, theme: &Theme) {
    style_cells(line, from, to, theme, true, false);
}

/// Restrained hover treatment for a read-only logical item in a dialog. It
/// preserves the item's label/metadata hierarchy and only raises the block's
/// background, rather than making informational content look selectable.
pub fn dialog_item_region(line: &mut Line, from: usize, to: usize, theme: &Theme) {
    let mut rebuilt: Vec<Span> = Vec::with_capacity(line.spans.len());
    let mut column = 0usize;
    for span in line.spans.drain(..) {
        for grapheme in span.text.graphemes(true) {
            let cell_width = grapheme.width();
            let end = column.saturating_add(cell_width);
            let hovered = if cell_width == 0 {
                column > from && column <= to
            } else {
                column < to && end > from
            };
            let mut styled = span.clone();
            styled.text = grapheme.to_string();
            if hovered {
                styled.bg = Some(theme.selected);
            }
            push_run(&mut rebuilt, styled);
            column = end;
        }
    }
    line.spans = rebuilt;
}

fn style_cells(
    line: &mut Line,
    from: usize,
    to: usize,
    theme: &Theme,
    pad_to_end: bool,
    bold: bool,
) {
    let mut rebuilt: Vec<Span> = Vec::with_capacity(line.spans.len() + 2);
    let mut column = 0usize;
    for span in line.spans.drain(..) {
        for grapheme in span.text.graphemes(true) {
            let cell_width = grapheme.width();
            let end = column.saturating_add(cell_width);
            let selected = if cell_width == 0 {
                column > from && column <= to
            } else {
                column < to && end > from
            };
            let mut styled = span.clone();
            styled.text = grapheme.to_string();
            if selected {
                styled.bg = Some(theme.primary);
                styled.fg = theme.bg;
                if bold {
                    styled.attributes |= attr::BOLD;
                }
            }
            push_run(&mut rebuilt, styled);
            column = end;
        }
    }
    if pad_to_end && column < to {
        if column < from {
            rebuilt.push(Span::new(" ".repeat(from - column), theme.text));
            column = from;
        }
        if column < to {
            let mut padding = Span::new(" ".repeat(to - column), theme.bg).on(theme.primary);
            if bold {
                padding.attributes |= attr::BOLD;
            }
            rebuilt.push(padding);
        }
    }
    line.spans = rebuilt;
}

fn push_run(spans: &mut Vec<Span>, span: Span) {
    if let Some(previous) = spans.last_mut()
        && previous.fg == span.fg
        && previous.bg == span.bg
        && previous.attributes == span.attributes
    {
        previous.text.push_str(&span.text);
    } else {
        spans.push(span);
    }
}

fn styled_run(template: &Span, text: String) -> Span {
    Span {
        text,
        fg: template.fg,
        bg: template.bg,
        attributes: template.attributes,
    }
}

#[cfg(test)]
mod truncation_tests {
    use super::*;
    #[test]
    fn narrow_labels_preserve_complete_graphemes() {
        for grapheme in ["👩‍💻", "👍🏽", "🇯🇵", "e\u{301}"] {
            let width = unicode_width::UnicodeWidthStr::width(grapheme);
            assert_eq!(single_line(&format!("{grapheme}abcd"), width + 1), format!("{grapheme}…"));
            assert_eq!(single_line(&format!("{grapheme}abcd"), width), "…");
        }
        assert_eq!(single_line("日本語", 3), "日…");
        assert_eq!(single_line("hello", 0), "");
        assert_eq!(single_line("a\nb", 3), "a b");
    }
}

#[cfg(test)]
mod selection_grapheme_tests {
    use super::*;

    #[test]
    fn selection_and_hover_keep_graphemes_in_one_styled_run() {
        let theme = Theme::default();
        let label = format!("{}…", "👩‍💻".repeat(25));
        let mut line = Line::plain(&label, theme.text);
        selected_row(&mut line, 54, &theme);
        assert_eq!(line.width(), 54);
        assert!(line.spans.iter().all(|span| !span.text.replace("👩‍💻", "").contains(['👩', '\u{200d}', '💻'])));
        let mut renderer = haarness_opentui::Renderer::new(60, 4, true).unwrap();
        assert!(renderer.snapshot_lines(&[line]).contains(&label));
        for paint in [highlight_range, interactive_region, dialog_item_region] {
            let mut line = Line::plain("a👩‍💻b", theme.text);
            paint(&mut line, 1, 2, &theme);
            assert_eq!(line.text(), "a👩‍💻b");
            assert!(line.spans.iter().any(|span| span.text.contains("👩‍💻")));
            assert!(renderer.snapshot_lines(&[line]).contains("a👩‍💻b"));
        }
    }
}
