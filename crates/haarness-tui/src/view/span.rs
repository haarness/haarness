//! Line/span construction helpers shared by every view.

use haarness_opentui::{Color, Line, Span, attr};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthChar as _;

use crate::theme::Theme;
use crate::width::DisplayWidth as _;

pub fn line(text: impl Into<String>, foreground: Color) -> Line {
    Line::plain(text, foreground)
}

/// One display line, truncated to `width` with an ellipsis when content was
/// dropped mid-way. Line breaks and tabs read as a space each.
pub fn single_line(value: &str, width: usize) -> String {
    let value = value.replace(['\n', '\r', '\t'], " ");
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

/// Tab stops of code and of what people write (a reply's code block, a
/// diff's rows, a prompt): every four columns.
pub const CODE_TABS: usize = 4;
/// Tab stops of a command's output, where a terminal sets them: every eight.
pub const OUTPUT_TABS: usize = 8;

/// Cells a tab at `column` takes: up to the next of the `stops`.
pub(crate) const fn tab_cells(column: usize, stops: usize) -> usize {
    stops - column % stops
}

/// Cells a grapheme takes at `column`, a tab reaching the next of the `stops`.
pub(crate) fn code_cells(column: usize, grapheme: &str, stops: usize) -> usize {
    if grapheme == "\t" {
        tab_cells(column, stops)
    } else {
        grapheme.width()
    }
}

/// One tab as it is drawn: `cells` spaces in `template`'s style, which a
/// selection copies as the tab. The renderer gives a raw tab a width of its
/// own, not the one a view measures, so every view that keeps a text's tabs
/// draws them as these.
pub fn tab_span(template: &Span, cells: usize) -> Span {
    let mut span = template.clone();
    span.text = " ".repeat(cells);
    span.tab = true;
    span
}

/// Whether `text` holds only printable ASCII, tabs and line breaks: there,
/// once the line breaks read as spaces, each byte is a grapheme of one cell
/// but a tab, which reaches its stop. The common case reads it byte by byte.
fn plain_ascii(text: &str) -> bool {
    text.bytes()
        .all(|byte| matches!(byte, b'\t' | b'\n' | b'\r' | 0x20..=0x7e))
}

/// Cells a byte of [`plain_ascii`] text takes at `column`.
const fn ascii_cells(column: usize, byte: u8, stops: usize) -> usize {
    if byte == b'\t' {
        tab_cells(column, stops)
    } else {
        1
    }
}

/// [`single_line`] for a line whose tabs [`expand_tabs`] draws: its tabs
/// stay, each measured to the next of the `stops`.
pub fn single_code_line(value: &str, width: usize, stops: usize) -> String {
    if !value.contains('\t') {
        return single_line(value, width);
    }
    if plain_ascii(value) {
        let mut used = 0;
        let mut end = None;
        let budget = width.saturating_sub(1);
        for (index, byte) in value.bytes().enumerate() {
            let cells = ascii_cells(used, byte, stops);
            if end.is_none() && used + cells > budget {
                end = Some(index);
            }
            used += cells;
        }
        let value = value.replace(['\n', '\r'], " ");
        return match end {
            _ if used <= width => value,
            _ if width == 0 => String::new(),
            Some(end) => format!("{}…", &value[..end]),
            None => value,
        };
    }
    let value = value.replace(['\n', '\r'], " ");
    let total = value
        .graphemes(true)
        .fold(0, |used, grapheme| used + code_cells(used, grapheme, stops));
    if total <= width {
        return value;
    }
    if width == 0 {
        return String::new();
    }
    let budget = width.saturating_sub(1);
    let mut used = 0;
    let mut truncated = String::new();
    for grapheme in value.graphemes(true) {
        let cells = code_cells(used, grapheme, stops);
        if used + cells > budget {
            break;
        }
        truncated.push_str(grapheme);
        used += cells;
    }
    truncated.push('…');
    truncated
}

/// `text` as one line of code drawn from `column` in `template`'s style,
/// within `width` cells: line breaks as spaces, each tab a [`tab_span`] to
/// the next of the `stops`, cut with an ellipsis where it does not fit. Its
/// spans, and the cells they take.
pub fn code_line_spans(
    text: &str,
    width: usize,
    stops: usize,
    column: usize,
    template: &Span,
) -> (Vec<Span>, usize) {
    if !text.contains('\t') {
        let mut span = template.clone();
        span.text = single_line(text, width);
        let used = span.width();
        return (
            vec![span]
                .into_iter()
                .filter(|span| !span.text.is_empty())
                .collect(),
            used,
        );
    }
    let text = text.replace(['\n', '\r'], " ");
    // Plain ASCII is read byte by byte, each byte one grapheme.
    let ascii = plain_ascii(&text);
    let units = || -> Box<dyn Iterator<Item = &str> + '_> {
        if ascii {
            Box::new((0..text.len()).map(|index| &text[index..=index]))
        } else {
            Box::new(text.graphemes(true))
        }
    };
    let cells_at = |at: usize, unit: &str| {
        if ascii {
            ascii_cells(at, unit.as_bytes()[0], stops)
        } else {
            code_cells(at, unit, stops)
        }
    };
    let total = units().fold(0, |used, unit| used + cells_at(column + used, unit));
    let fits = total <= width;
    if !fits && width == 0 {
        return (Vec::new(), 0);
    }
    let budget = if fits { width } else { width - 1 };
    let piece = |run: &mut String| {
        let mut span = template.clone();
        span.text = std::mem::take(run);
        span
    };
    let (mut spans, mut run, mut used) = (Vec::new(), String::new(), 0);
    for grapheme in units() {
        let cells = cells_at(column + used, grapheme);
        if used + cells > budget {
            break;
        }
        if grapheme == "\t" {
            if !run.is_empty() {
                spans.push(piece(&mut run));
            }
            spans.push(tab_span(template, cells));
        } else {
            run.push_str(grapheme);
        }
        used += cells;
    }
    if !fits {
        run.push('…');
        used += 1;
    }
    if !run.is_empty() {
        spans.push(piece(&mut run));
    }
    (spans, used)
}

/// A line with each tab drawn as a [`tab_span`] up to the next of the
/// `stops`, counted from its first span; a line without tabs comes back as
/// it is.
pub fn expand_tabs(spans: Vec<Span>, stops: usize) -> Vec<Span> {
    if !spans.iter().any(|span| span.text.contains('\t')) {
        return spans;
    }
    let mut column = 0;
    let mut opened = Vec::with_capacity(spans.len() + 2);
    for mut span in spans {
        if !span.text.contains('\t') {
            column += span.width();
            opened.push(span);
            continue;
        }
        // The span, its text taken out, is the style each piece is made from.
        let text = std::mem::take(&mut span.text);
        for (index, piece) in text.split('\t').enumerate() {
            if index > 0 {
                let cells = tab_cells(column, stops);
                opened.push(tab_span(&span, cells));
                column += cells;
            }
            if !piece.is_empty() {
                let mut part = span.clone();
                piece.clone_into(&mut part.text);
                column += if piece.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
                    piece.len()
                } else {
                    part.width()
                };
                opened.push(part);
            }
        }
    }
    opened
}

/// A chat title as one display line: [`single_line`] without the escape
/// sequences an older server kept in a title, so no `]0;…` fragment shows.
/// Only for titles: a file's or a command's text keeps what it holds.
pub fn title_line(title: &str, width: usize) -> String {
    single_line(&crate::escapes::without_escapes(title), width)
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

/// A request's first-token time: one decimal under ten seconds, whole seconds
/// from there (`820` → `0.8s`, `14_200` → `14s`). Rounded to tenths first,
/// so 9.96 s reads `10s`, not `10.0s`.
pub fn seconds_label(ms: u64) -> String {
    let tenths = (ms + 50) / 100;
    if tenths < 100 {
        format!("{}.{}s", tenths / 10, tenths % 10)
    } else {
        format!("{}s", (ms + 500) / 1000)
    }
}

/// An output speed in tokens per second: one decimal under ten, whole
/// numbers from there.
pub fn speed_label(tokens_per_second: f64) -> String {
    let tenths = (tokens_per_second * 10.0).round();
    if tenths < 100.0 {
        format!("{:.1}", tenths / 10.0)
    } else {
        format!("{tokens_per_second:.0}")
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
/// run's colors and attributes. Tabs reach stops of [`CODE_TABS`].
pub fn wrap_spans(
    spans: &[Span],
    width: usize,
    first_prefix: &str,
    cont_prefix: &str,
    prefix_color: Color,
) -> Vec<Line> {
    wrap_spans_with_tabs(
        spans,
        width,
        first_prefix,
        cont_prefix,
        prefix_color,
        CODE_TABS,
    )
}

/// [`wrap_spans`] with tabs reaching the next of the `stops`, counted from
/// the start of the row they land on (as a terminal and the prompt count
/// them), each drawn as a [`tab_span`].
pub fn wrap_spans_with_tabs(
    spans: &[Span],
    width: usize,
    first_prefix: &str,
    cont_prefix: &str,
    prefix_color: Color,
    stops: usize,
) -> Vec<Line> {
    let inner = width
        .saturating_sub(first_prefix.width().max(cont_prefix.width()))
        .max(10);
    // Flatten to (character, style index) so breaks can land anywhere. A
    // tab's width depends on its row, so it is measured there.
    let mut cells: Vec<(char, usize, usize)> = Vec::new();
    for (index, span) in spans.iter().enumerate() {
        for character in span.text.chars() {
            cells.push((character, index, character.width().unwrap_or(0)));
        }
    }
    let cells_at = |cell: &(char, usize, usize), column: usize| {
        if cell.0 == '\t' {
            tab_cells(column, stops)
        } else {
            cell.2
        }
    };
    let mut lines = Vec::new();
    let mut start = 0;
    while start < cells.len() {
        let mut end = start;
        let mut used = 0;
        while end < cells.len() {
            let cells_wide = cells_at(&cells[end], used);
            if end > start && used + cells_wide > inner {
                break;
            }
            used += cells_wide;
            end += 1;
        }
        if end < cells.len() {
            // Prefer breaking at the last space (or tab) inside the window.
            if let Some(break_at) = (start..end)
                .rev()
                .find(|&index| matches!(cells[index].0, ' ' | '\t'))
            {
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
        let mut column = 0;
        for cell in &cells[start..end] {
            let (character, style, _) = *cell;
            let cells_wide = cells_at(cell, column);
            column += cells_wide;
            if character == '\t' {
                if let Some((active, text)) = current.take() {
                    line_spans.push(styled_run(&spans[active], text));
                }
                line_spans.push(tab_span(&spans[style], cells_wide));
                continue;
            }
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
        while start < cells.len() && matches!(cells[start].0, ' ' | '\t') {
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
        for (mut styled, cell_width) in style_pieces(span) {
            let end = column.saturating_add(cell_width);
            let hovered = if cell_width == 0 {
                column > from && column <= to
            } else {
                column < to && end > from
            };
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
        for (mut styled, cell_width) in style_pieces(span) {
            let end = column.saturating_add(cell_width);
            let selected = if cell_width == 0 {
                column > from && column <= to
            } else {
                column < to && end > from
            };
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

/// Make the `prefix` a row was drawn with decoration: a selection copies the
/// text after it, never the margin or the bar. The row's first span must
/// start with it, as [`wrap_spans`] draws it.
pub fn decorate_prefix(line: &mut Line, prefix: &str) {
    let Some(first) = line.spans.first_mut() else {
        return;
    };
    if prefix.is_empty() || !first.text.starts_with(prefix) {
        return;
    }
    let rest = first.text.split_off(prefix.len());
    let tail = styled_run(first, rest);
    first.selectable = false;
    if !tail.text.is_empty() {
        line.spans.insert(1, tail);
    }
}

/// Add `span` to `spans`, joined to the last one when they look alike. A tab
/// stays a span of its own, so a selection still copies it as one.
fn push_run(spans: &mut Vec<Span>, span: Span) {
    if let Some(previous) = spans.last_mut()
        && !previous.tab
        && !span.tab
        && previous.fg == span.fg
        && previous.bg == span.bg
        && previous.attributes == span.attributes
        && previous.selectable == span.selectable
        && previous.opens == span.opens
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
        selectable: template.selectable,
        tab: false,
        opens: template.opens.clone(),
    }
}

/// `span` cut into the pieces a cell style is laid over: each grapheme, or a
/// [`tab_span`] whole, with the cells each piece takes.
fn style_pieces(span: Span) -> Vec<(Span, usize)> {
    if span.tab {
        let cells = span.width();
        return vec![(span, cells)];
    }
    span.text
        .graphemes(true)
        .map(|grapheme| {
            let mut piece = span.clone();
            piece.text = grapheme.to_string();
            (piece, grapheme.width())
        })
        .collect()
}

#[cfg(test)]
mod truncation_tests {
    use super::*;
    #[test]
    fn narrow_labels_preserve_complete_graphemes() {
        for grapheme in ["👩‍💻", "👍🏽", "🇯🇵", "e\u{301}"] {
            let width = unicode_width::UnicodeWidthStr::width(grapheme);
            assert_eq!(
                single_line(&format!("{grapheme}abcd"), width + 1),
                format!("{grapheme}…")
            );
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
        assert!(line.spans.iter().all(|span| {
            !span
                .text
                .replace("👩‍💻", "")
                .contains(['👩', '\u{200d}', '💻'])
        }));
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
