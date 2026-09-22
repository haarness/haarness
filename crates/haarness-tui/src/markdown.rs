//! Markdown → styled terminal lines, in the quiet editorial voice: bold
//! headings, surface-backed code, soft list bullets — no boxes, no rules
//! heavier than a hairline.

use haarness_opentui::{Line, Span, attr};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use crate::highlight::highlight_block;
use crate::theme::Theme;
use crate::view::span::wrap_spans;

pub fn render_markdown(text: &str, width: usize, theme: &Theme, indent: &str) -> Vec<Line> {
    let mut renderer = Renderer {
        theme,
        width,
        indent,
        lines: Vec::new(),
        inline: Vec::new(),
        bold: 0,
        italic: 0,
        code_block: None,
        list_stack: Vec::new(),
        link: None,
        prefix_override: None,
        quote_depth: 0,
        table: None,
    };
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(text, options) {
        renderer.event(event);
    }
    renderer.flush_inline();
    while renderer
        .lines
        .last()
        .is_some_and(|line| line.text().trim().is_empty())
    {
        renderer.lines.pop();
    }
    renderer.lines
}

struct Renderer<'a> {
    theme: &'a Theme,
    width: usize,
    indent: &'a str,
    lines: Vec<Line>,
    inline: Vec<Span>,
    bold: u32,
    italic: u32,
    code_block: Option<(String, String)>,
    list_stack: Vec<Option<u64>>,
    link: Option<String>,
    prefix_override: Option<String>,
    quote_depth: u32,
    /// Collecting table cells while inside a table.
    table: Option<TableState>,
}

#[derive(Default)]
struct TableState {
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    in_cell: bool,
}

impl Renderer<'_> {
    fn attributes(&self) -> u32 {
        let mut attributes = attr::NONE;
        if self.bold > 0 {
            attributes |= attr::BOLD;
        }
        if self.italic > 0 {
            attributes |= attr::ITALIC;
        }
        attributes
    }

    fn text_color(&self) -> haarness_opentui::Color {
        if self.quote_depth > 0 {
            self.theme.muted
        } else {
            self.theme.text
        }
    }

    fn push_inline(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.inline
            .push(Span::new(text, self.text_color()).with_attributes(self.attributes()));
    }

    fn flush_inline(&mut self) {
        if self.inline.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.inline);
        let (first, cont) = self.prefixes();
        let wrapped = wrap_spans(&spans, self.width, &first, &cont, self.theme.muted);
        self.lines.extend(wrapped);
        self.prefix_override = None;
    }

    fn prefixes(&self) -> (String, String) {
        let quote = "▎ ".repeat(self.quote_depth as usize);
        let list_indent = "  ".repeat(self.list_stack.len().saturating_sub(1));
        let base = format!("{}{quote}{list_indent}", self.indent);
        let first = self
            .prefix_override
            .clone()
            .map_or_else(|| base.clone(), |marker| format!("{base}{marker}"));
        let cont_pad = " ".repeat(first.chars().count() - base.chars().count());
        (first, format!("{base}{cont_pad}"))
    }

    /// Aligned table: header row on the surface, hairline separator, data
    /// rows padded per column (clipped to keep the table inside the width).
    fn render_table(&mut self, rows: &[Vec<String>]) {
        if rows.is_empty() {
            return;
        }
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let budget = self
            .width
            .saturating_sub(self.indent.chars().count() + columns.saturating_sub(1) * 3)
            .max(8);
        let cap = (budget / columns.max(1)).clamp(4, 40);
        let mut widths = vec![0usize; columns];
        for row in rows {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(cell.trim().chars().count().min(cap));
            }
        }
        for (row_index, row) in rows.iter().enumerate() {
            let mut spans = vec![Span::new(self.indent, self.theme.muted)];
            for (index, width) in widths.iter().enumerate() {
                let cell = row.get(index).map(String::as_str).unwrap_or("").trim();
                let clipped = crate::view::span::single_line(cell, *width);
                let pad = width.saturating_sub(clipped.chars().count());
                let color = if row_index == 0 {
                    self.theme.primary
                } else {
                    self.text_color()
                };
                spans.push(Span::new(format!("{clipped}{}", " ".repeat(pad)), color));
                if index + 1 < columns {
                    spans.push(Span::new(" │ ", self.theme.muted));
                }
            }
            self.lines.push(Line::new(spans));
            if row_index == 0 {
                let rule: String = widths
                    .iter()
                    .enumerate()
                    .map(|(index, width)| {
                        let mut piece = "─".repeat(*width);
                        if index + 1 < columns {
                            piece.push_str("─┼─");
                        }
                        piece
                    })
                    .collect();
                self.lines.push(Line::plain(
                    format!("{}{rule}", self.indent),
                    self.theme.muted,
                ));
            }
        }
        self.blank();
    }

    fn blank(&mut self) {
        // A block boundary only separates content that already exists.  In
        // particular, headings and rules at the start of a message must not
        // manufacture a leading row for the transcript reply marker to land
        // on.
        if !self.lines.is_empty() && !self.lines.last().is_some_and(|line| line.text().is_empty()) {
            self.lines.push(Line::empty());
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if let Some((_, buffer)) = &mut self.code_block {
                    buffer.push_str(&text);
                } else if let Some(table) = &mut self.table {
                    if table.in_cell {
                        if let Some(cell) = table.current_row.last_mut() {
                            cell.push_str(&text);
                        }
                    }
                } else {
                    self.push_inline(&text);
                }
            }
            Event::Code(code) => {
                if let Some(table) = &mut self.table {
                    if table.in_cell {
                        if let Some(cell) = table.current_row.last_mut() {
                            cell.push_str(&code);
                        }
                        return;
                    }
                }
                self.inline
                    .push(Span::new(format!(" {code} "), self.theme.accent).on(self.theme.surface));
            }
            // Terminal agent output relies on its line structure; treat soft
            // breaks as real breaks instead of collapsing them to spaces.
            Event::SoftBreak | Event::HardBreak => self.flush_inline(),
            Event::Rule => {
                self.blank();
                self.lines.push(Line::plain(
                    format!("{}{}", self.indent, "─".repeat(24)),
                    self.theme.muted,
                ));
                self.blank();
            }
            Event::TaskListMarker(done) => {
                let marker = if done { "☑ " } else { "☐ " };
                self.inline.push(Span::new(marker, self.theme.muted));
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { .. } => {
                self.blank();
                self.bold += 1;
            }
            Tag::Strong => self.bold += 1,
            Tag::Emphasis => self.italic += 1,
            Tag::BlockQuote(_) => {
                self.flush_inline();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.flush_inline();
                let language = match kind {
                    CodeBlockKind::Fenced(name) => name.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code_block = Some((language, String::new()));
            }
            Tag::List(start) => {
                self.flush_inline();
                self.list_stack.push(start);
            }
            Tag::Item => {
                self.flush_inline();
                let marker = match self.list_stack.last_mut() {
                    Some(Some(index)) => {
                        let marker = format!("{index}. ");
                        *index += 1;
                        marker
                    }
                    _ => "• ".into(),
                };
                self.prefix_override = Some(marker);
            }
            Tag::Link { dest_url, .. } => self.link = Some(dest_url.to_string()),
            Tag::Table(_) => {
                self.flush_inline();
                self.table = Some(TableState::default());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.current_row = Vec::new();
                }
            }
            Tag::TableCell => {
                if let Some(table) = &mut self.table {
                    table.current_row.push(String::new());
                    table.in_cell = true;
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                // Recolor the heading buffer to primary before flushing.
                for span in &mut self.inline {
                    span.fg = self.theme.primary;
                }
                self.flush_inline();
                self.bold = self.bold.saturating_sub(1);
                self.blank();
            }
            TagEnd::Paragraph => {
                self.flush_inline();
                self.blank();
            }
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::BlockQuote(_) => {
                self.flush_inline();
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.blank();
            }
            TagEnd::CodeBlock => {
                if let Some((language, body)) = self.code_block.take() {
                    let inner_width = self.width.saturating_sub(self.indent.chars().count());
                    // Whole-block highlighting keeps multi-line constructs
                    // (block comments, raw strings) correct.
                    for row_spans in highlight_block(&body, &language, self.theme) {
                        let mut spans = vec![Span::new(self.indent, self.theme.muted)];
                        let mut used = 0;
                        for span in row_spans {
                            used += span.text.chars().count();
                            spans.push(span);
                        }
                        // Pad so the block reads as one raised surface.
                        if used < inner_width {
                            spans.push(
                                Span::new(" ".repeat(inner_width - used), self.theme.text)
                                    .on(self.theme.surface),
                            );
                        }
                        self.lines.push(Line::new(spans));
                    }
                    self.blank();
                }
            }
            TagEnd::List(_) => {
                self.flush_inline();
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.blank();
                }
            }
            TagEnd::Item => self.flush_inline(),
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table {
                    table.in_cell = false;
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.current_row);
                    table.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.render_table(&table.rows);
                }
            }
            TagEnd::Link => {
                if let Some(url) = self.link.take() {
                    let shows_url = self
                        .inline
                        .last()
                        .is_some_and(|span| span.text.contains(&url));
                    if let Some(last) = self.inline.last_mut() {
                        last.attributes |= attr::UNDERLINE;
                    }
                    if !shows_url {
                        self.inline
                            .push(Span::new(format!(" ({url})"), self.theme.muted));
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::render_markdown;
    use crate::theme::Theme;
    use haarness_opentui::attr;

    fn text_of(lines: &[haarness_opentui::Line]) -> String {
        lines
            .iter()
            .map(|line| line.text() + "\n")
            .collect::<String>()
    }

    #[test]
    fn bold_and_inline_code_style_without_marker_characters() {
        let theme = Theme::editorial_dark();
        let lines = render_markdown("**bold** and `code` here", 80, &theme, "    ");
        let text = text_of(&lines);
        assert!(text.contains("bold"), "{text}");
        assert!(!text.contains("**"), "markers must not leak: {text}");
        assert!(!text.contains('`'), "{text}");
        let bold_span = lines[0]
            .spans
            .iter()
            .find(|span| span.text.contains("bold"))
            .unwrap();
        assert_eq!(bold_span.attributes & attr::BOLD, attr::BOLD);
        let code_span = lines[0]
            .spans
            .iter()
            .find(|span| span.text.contains("code"))
            .unwrap();
        assert_eq!(code_span.bg, Some(theme.surface));
    }

    #[test]
    fn fenced_code_blocks_highlight_on_a_surface() {
        let theme = Theme::editorial_dark();
        let lines = render_markdown("```rust\nlet x = 1;\n```", 60, &theme, "    ");
        let code_line = lines
            .iter()
            .find(|line| line.text().contains("let x = 1;"))
            .unwrap();
        let keyword = code_line
            .spans
            .iter()
            .find(|span| span.text.contains("let"))
            .unwrap();
        assert_eq!(keyword.fg, theme.syntax.keyword);
        assert_eq!(keyword.bg, Some(theme.surface));
        // The row is padded to a full-width block.
        assert!(code_line.width() >= 55, "{}", code_line.width());
    }

    #[test]
    fn unclosed_fenced_code_is_visible_and_highlighted_while_streaming() {
        let theme = Theme::editorial_dark();
        let lines = render_markdown("```rust\nlet answer = 42;", 60, &theme, "    ");
        let text = text_of(&lines);
        assert!(
            text.contains("let answer = 42;"),
            "partial fence vanished: {text}"
        );
        assert!(!text.contains("```"), "fence marker leaked: {text}");
        let code_line = lines
            .iter()
            .find(|line| line.text().contains("let answer = 42;"))
            .unwrap();
        let keyword = code_line
            .spans
            .iter()
            .find(|span| span.text.contains("let"))
            .unwrap();
        assert_eq!(keyword.fg, theme.syntax.keyword);
        assert_eq!(keyword.bg, Some(theme.surface));
    }

    #[test]
    fn incomplete_emphasis_stays_visible_then_completed_bold_hides_markers() {
        let theme = Theme::editorial_dark();
        let partial = render_markdown("Status: **working", 80, &theme, "  ");
        let partial_text = text_of(&partial);
        assert!(
            partial_text.contains("**working"),
            "an incomplete delimiter must remain visible: {partial_text}"
        );

        let complete = render_markdown("Status: **working**", 80, &theme, "  ");
        let complete_text = text_of(&complete);
        assert!(complete_text.contains("working"), "{complete_text}");
        assert!(
            !complete_text.contains("**"),
            "completed markers must be hidden: {complete_text}"
        );
        let bold = complete
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.text.contains("working"))
            .unwrap();
        assert_eq!(bold.attributes & attr::BOLD, attr::BOLD);
    }

    #[test]
    fn headings_lists_and_quotes_keep_the_quiet_voice() {
        let theme = Theme::editorial_dark();
        let lines = render_markdown(
            "# Title\n\n- first\n- second\n\n> aside\n\n1. one\n2. two",
            80,
            &theme,
            "  ",
        );
        let text = text_of(&lines);
        assert!(text.contains("Title"));
        assert!(!text.contains('#'));
        assert!(text.contains("• first"));
        assert!(text.contains("▎ aside"));
        assert!(text.contains("1. one"));
        assert!(text.contains("2. two"));
        let title_line = lines
            .iter()
            .find(|line| line.text().contains("Title"))
            .unwrap();
        let title_span = title_line
            .spans
            .iter()
            .find(|span| span.text.contains("Title"))
            .unwrap();
        assert_eq!(title_span.fg, theme.primary);
    }

    #[test]
    fn leading_block_markdown_does_not_create_an_empty_first_row() {
        let theme = Theme::editorial_dark();
        for markdown in ["# Heading", "---\n\nAfter rule"] {
            let lines = render_markdown(markdown, 80, &theme, "  ");
            assert!(
                lines
                    .first()
                    .is_some_and(|line| !line.text().trim().is_empty()),
                "leading block created an empty row: {lines:?}"
            );
        }
    }

    #[test]
    fn long_paragraphs_wrap_and_keep_styles() {
        let theme = Theme::editorial_dark();
        let body = format!("prefix **{}** suffix", "word ".repeat(30).trim());
        let lines = render_markdown(&body, 40, &theme, "  ");
        assert!(lines.len() > 2, "should wrap: {}", text_of(&lines));
        for line in &lines {
            assert!(line.width() <= 40, "{} > 40", line.width());
        }
        // Styled runs survive wrapping onto continuation lines.
        assert!(
            lines
                .iter()
                .skip(1)
                .flat_map(|line| &line.spans)
                .any(|span| span.attributes & attr::BOLD == attr::BOLD)
        );
    }

    #[test]
    fn links_underline_and_cite_urls() {
        let theme = Theme::editorial_dark();
        let lines = render_markdown("see [the docs](https://example.com)", 80, &theme, "  ");
        let text = text_of(&lines);
        assert!(text.contains("the docs"));
        assert!(text.contains("(https://example.com)"));
    }
}

#[cfg(test)]
mod table_tests {
    use super::render_markdown;
    use crate::theme::Theme;

    #[test]
    fn tables_render_aligned_with_a_header_rule() {
        let theme = Theme::editorial_dark();
        let lines = render_markdown(
            "| name | role |\n|---|---|\n| ada | engineer |\n| grace | admiral |",
            80,
            &theme,
            "  ",
        );
        let rows: Vec<String> = lines.iter().map(haarness_opentui::Line::text).collect();
        let header = rows.iter().find(|row| row.contains("name")).unwrap();
        assert!(header.contains('│'), "column separator: {header}");
        assert!(rows.iter().any(|row| row.contains('┼')), "header rule");
        let ada = rows.iter().find(|row| row.contains("ada")).unwrap();
        let grace = rows.iter().find(|row| row.contains("grace")).unwrap();
        assert_eq!(
            ada.find('│'),
            grace.find('│'),
            "columns align: {ada:?} vs {grace:?}"
        );
    }

    #[test]
    fn partial_table_text_survives_until_it_becomes_a_table() {
        let theme = Theme::editorial_dark();
        for partial in [
            "| name",
            "| name | role |",
            "| name | role |\n| ---",
            "| name | role |\n| --- |",
        ] {
            let rendered = render_markdown(partial, 80, &theme, "  ");
            let text = rendered
                .iter()
                .map(haarness_opentui::Line::text)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                text.contains("name"),
                "partial table lost its header: {text}"
            );
            if partial.contains("role") {
                assert!(text.contains("role"), "partial table lost a cell: {text}");
            }
        }

        let complete = render_markdown(
            "| name | role |\n| --- | --- |\n| ada | engineer |",
            80,
            &theme,
            "  ",
        );
        let rows: Vec<String> = complete.iter().map(haarness_opentui::Line::text).collect();
        let header = rows.iter().find(|row| row.contains("name")).unwrap();
        assert!(
            header.contains('│'),
            "completed table was not promoted: {header}"
        );
        assert!(rows.iter().any(|row| row.contains("ada")), "{rows:?}");
    }

    #[test]
    fn multi_line_rust_blocks_keep_comment_state() {
        let theme = Theme::editorial_dark();
        let lines = render_markdown(
            "```rust\n/* start\nstill a comment\n*/\nlet x = 1;\n```",
            60,
            &theme,
            "  ",
        );
        let inside = lines
            .iter()
            .find(|line| line.text().contains("still a comment"))
            .unwrap();
        let span = inside
            .spans
            .iter()
            .find(|span| span.text.contains("still"))
            .unwrap();
        assert_eq!(
            span.fg, theme.syntax.comment,
            "block comments survive across lines"
        );
    }
}
