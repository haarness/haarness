//! Selector menus (commands, threads, models, agents) and info panels.

use haarness_opentui::{Line, Span, attr};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use crate::state::{App, Overlay};
use crate::view::frame::{HitTarget, TaggedLines};
use crate::view::span::{line, pad, selected_row, short_id, single_line};

pub fn menu_lines(app: &App, width: usize, max_height: usize) -> TaggedLines {
    if max_height == 0 {
        return TaggedLines::default();
    }
    let theme = &app.theme;
    let (title, rows): (&str, Vec<(String, String)>) = match app.overlay {
        Overlay::Commands => (
            "commands",
            app.filtered_slash_entries(&app.overlay_filter)
                .into_iter()
                .map(|entry| (format!("/{}", entry.name), entry.description))
                .collect(),
        ),
        Overlay::Threads => (
            "threads",
            app.filtered_threads()
                .into_iter()
                .map(|thread| {
                    (
                        thread.title.clone(),
                        short_id(&thread.id.to_string()).into(),
                    )
                })
                .collect(),
        ),
        Overlay::Models => (
            "models",
            app.filtered_models()
                .into_iter()
                .map(|model| {
                    let active = model == format!("{}/{}", app.thread.provider, app.thread.model);
                    let status = if active { "active" } else { "" };
                    let detail = model_detail(app, &model, status);
                    (app.model_label(&model), detail)
                })
                .collect(),
        ),
        Overlay::Agents => (
            "agents",
            app.filtered_agents()
                .into_iter()
                .map(|agent| (agent.id.clone(), agent.description.clone()))
                .collect(),
        ),
        Overlay::Options {
            ref title,
            ref entries,
            ..
        } => (
            title.as_str(),
            app.filtered_options(entries)
                .into_iter()
                .map(|entry| (entry.label, entry.description))
                .collect(),
        ),
        Overlay::Themes => (
            "themes",
            app.filtered_themes()
                .into_iter()
                .map(|(name, variant, _)| {
                    let active = (name.as_str(), variant.as_str()) == app.theme_baseline();
                    (
                        format!("{name} · {variant}"),
                        if active {
                            "active".into()
                        } else {
                            String::new()
                        },
                    )
                })
                .collect(),
        ),
        Overlay::None => {
            if app.autocomplete_hidden {
                return TaggedLines::default();
            }
            if let Some(commands) = app.slash_suggestions() {
                (
                    "commands",
                    commands
                        .into_iter()
                        .map(|entry| (format!("/{}", entry.name), entry.description))
                        .collect(),
                )
            } else {
                let Some((_, query)) = app.composer.mention_query() else {
                    return TaggedLines::default();
                };
                let dot_path = query == "."
                    || query == ".."
                    || query.starts_with("./")
                    || query.starts_with("../");
                let title = if query.starts_with('/') || query.starts_with('~') || dot_path {
                    "files · local paths"
                } else if app
                    .mention_suggestions
                    .iter()
                    .any(|path| path.starts_with("agent://"))
                {
                    "agents · files"
                } else {
                    "files · agents"
                };
                (
                    title,
                    app.mention_suggestions
                        .iter()
                        .map(|path| {
                            if let Some(agent) = path.strip_prefix("agent://") {
                                (format!("@{agent}"), "Delegate to agent".into())
                            } else {
                                (path.clone(), "File snapshot".into())
                            }
                        })
                        .collect(),
                )
            }
        }
        Overlay::Panel { .. }
        | Overlay::Diff(_)
        | Overlay::Workflow { .. }
        | Overlay::ProviderLogin => {
            return TaggedLines::default();
        }
    };
    let mut lines = TaggedLines::default();
    // The menu is the only raised surface near the composer, so it reads as a
    // popover over the page. Its caption belongs to that block, not the page.
    let caption = format!(
        "  {}{}",
        app.language.text(title),
        if app.overlay_filter.is_empty() {
            String::new()
        } else {
            format!("  {}", app.overlay_filter)
        }
    );
    lines.push(Line::new(vec![
        Span::new(pad(&caption, width), theme.muted).on(theme.overlay),
    ]));
    if max_height == 1 {
        return lines;
    }
    if rows.is_empty() {
        lines.push(line("  No matches", theme.muted));
        return lines;
    }
    let visible = rows.len().min(7).min(max_height - 1);
    let start = app.selection.saturating_sub(visible - 1);
    for (index, (label, description)) in rows.into_iter().enumerate().skip(start).take(visible) {
        let marker = if index == app.selection { "›" } else { " " };
        let label_width = 18.min(width.saturating_sub(8));
        let body = format!(
            "  {marker} {label:<label_width$} {}",
            single_line(&description, width.saturating_sub(label_width + 7))
        );
        let mut row = line(
            pad(&body, width),
            if index == app.selection {
                theme.bg
            } else {
                theme.muted
            },
        );
        if index == app.selection {
            selected_row(&mut row, width, theme);
        } else {
            for span in &mut row.spans {
                span.bg = Some(theme.overlay);
            }
        }
        lines.push_tagged(row, HitTarget::MenuRow(index));
    }
    lines
}

fn model_detail(app: &App, qualified: &str, status: &str) -> String {
    let Some((provider_id, model_id)) = qualified.split_once('/') else {
        return status.to_owned();
    };
    let info = app
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .and_then(|provider| provider.models.iter().find(|model| model.id == model_id));
    let mut parts = vec![haarness_protocol::model_display_id(qualified)];
    if !status.is_empty() {
        parts.push(status.to_owned());
    }
    if let Some(info) = info {
        let mut capabilities = Vec::new();
        if info.reasoning {
            capabilities.push("reasoning");
        }
        if info.images {
            capabilities.push("vision");
        }
        if info.video {
            capabilities.push("video");
        }
        if info.tools {
            capabilities.push("tools");
        }
        if !capabilities.is_empty() {
            parts.push(capabilities.join("+"));
        }
        if let Some(context) = info.context_window {
            parts.push(format!("{} ctx", token_limit(context)));
        }
        if let Some(output) = info.max_output_tokens {
            parts.push(format!("{} out", token_limit(output)));
        }
        if let Some(pricing) = info.pricing {
            parts.push(format!(
                "${}/{} MTok",
                compact_price(pricing.input),
                compact_price(pricing.output)
            ));
        }
    }
    parts.join(" · ")
}

fn token_limit(value: u64) -> String {
    if value >= 1_000_000 {
        compact_ratio(value, 1_000_000, 2, "M")
    } else if value >= 1_000 {
        compact_ratio(value, 1_000, 1, "K")
    } else {
        value.to_string()
    }
}

fn compact_ratio(value: u64, divisor: u64, precision: u32, suffix: &str) -> String {
    let factor = 10_u128.pow(precision);
    let scaled = (u128::from(value) * factor + u128::from(divisor / 2)) / u128::from(divisor);
    let whole = scaled / factor;
    let fraction = scaled % factor;
    if fraction == 0 {
        return format!("{whole}{suffix}");
    }
    let precision = usize::try_from(precision).unwrap_or(0);
    let mut fraction = format!("{fraction:0precision$}");
    while fraction.ends_with('0') {
        fraction.pop();
    }
    format!("{whole}.{fraction}{suffix}")
}

fn compact_decimal(value: f64, precision: usize, suffix: &str) -> String {
    let mut value = format!("{value:.precision$}");
    while value.ends_with('0') {
        value.pop();
    }
    if value.ends_with('.') {
        value.pop();
    }
    format!("{value}{suffix}")
}

fn compact_price(value: f64) -> String {
    compact_decimal(value, 3, "")
}

/// Center-card content for explicit dialogs. Autocomplete remains attached to
/// the composer; commands/models/threads/agents/themes and info panels use
/// this richer modal surface.
pub fn dialog_lines(app: &App, width: usize, max_height: usize) -> Option<TaggedLines> {
    if matches!(app.overlay, Overlay::None) {
        return None;
    }
    if let Overlay::Panel {
        ref title,
        ref lines,
    } = app.overlay
    {
        return Some(panel_dialog(app, title, lines, width, max_height));
    }
    if let Overlay::Diff(ref viewer) = app.overlay {
        return Some(diff_dialog(app, viewer, width, max_height));
    }
    if let Overlay::Workflow { ref target } = app.overlay {
        return Some(workflow_dialog(app, target, width, max_height));
    }
    if matches!(app.overlay, Overlay::ProviderLogin) {
        return Some(login_dialog(app, width, max_height));
    }

    let (title, rows): (&str, Vec<SelectorRow>) = match app.overlay {
        Overlay::Commands => (
            "Commands",
            app.filtered_slash_entries(&app.overlay_filter)
                .into_iter()
                .map(|entry| SelectorRow::new(format!("/{}", entry.name), entry.description, false))
                .collect(),
        ),
        Overlay::Threads => (
            "Sessions",
            app.filtered_threads()
                .into_iter()
                .map(|thread| {
                    SelectorRow::new(
                        thread.title.clone(),
                        app.session_detail(thread),
                        thread.id == app.thread.id,
                    )
                })
                .collect(),
        ),
        Overlay::Models => (
            "Select model",
            app.filtered_models()
                .into_iter()
                .map(|model| {
                    let current = model == format!("{}/{}", app.thread.provider, app.thread.model);
                    let status = if current { "active" } else { "" };
                    SelectorRow::new(
                        format!(
                            "{}{}",
                            if app.model_favorites.contains(&model) {
                                "★ "
                            } else {
                                ""
                            },
                            app.model_label(&model)
                        ),
                        model_detail(app, &model, status),
                        current,
                    )
                })
                .collect(),
        ),
        Overlay::Agents => (
            "Select agent",
            app.filtered_agents()
                .into_iter()
                .map(|agent| {
                    SelectorRow::new(
                        agent.name.clone(),
                        agent.description.clone(),
                        agent.id == app.agent(),
                    )
                })
                .collect(),
        ),
        Overlay::Themes => (
            "Select theme",
            app.filtered_themes()
                .into_iter()
                .map(|(name, variant, theme)| {
                    let current = (name.as_str(), variant.as_str()) == app.theme_baseline();
                    // A palette is the thing being chosen, so show it rather
                    // than only naming it: the page, its type, and the two
                    // colours the interface actually leans on.
                    SelectorRow::new(name, variant, current).with_swatch([
                        theme.bg,
                        theme.text,
                        theme.primary,
                        theme.accent,
                    ])
                })
                .collect(),
        ),
        Overlay::Options {
            ref title,
            ref entries,
            kind,
        } => (
            title,
            app.filtered_options(entries)
                .into_iter()
                .map(|entry| {
                    let current = match kind {
                        crate::state::OptionsKind::ReasoningEffort => {
                            entry.value
                                == if app.thread.ultracode {
                                    "ultracode"
                                } else {
                                    app.reasoning_effort.as_deref().unwrap_or("off")
                                }
                        }
                        crate::state::OptionsKind::Language => entry.value == app.language.code(),
                        crate::state::OptionsKind::Tasks => {
                            entry.value == app.thread.id.to_string()
                        }
                        crate::state::OptionsKind::Timeline => app
                            .thread
                            .revert
                            .as_ref()
                            .is_some_and(|state| entry.value == state.sequence.to_string()),
                        crate::state::OptionsKind::Workspaces => app
                            .workspace_catalog
                            .as_ref()
                            .and_then(|catalog| catalog.current_workspace_id.as_deref())
                            .is_some_and(|current| entry.value == format!("workspace:{current}")),
                        crate::state::OptionsKind::InputQueue
                        | crate::state::OptionsKind::InputQueueActions
                        | crate::state::OptionsKind::InputQueueEdit
                        | crate::state::OptionsKind::Management
                        | crate::state::OptionsKind::ManagementField
                        | crate::state::OptionsKind::ManagementValue
                        | crate::state::OptionsKind::ManagementConfirm
                        | crate::state::OptionsKind::ConnectProvider
                        | crate::state::OptionsKind::ConnectMethod
                        | crate::state::OptionsKind::ConnectApiKey
                        | crate::state::OptionsKind::ForkMessages
                        | crate::state::OptionsKind::Messages
                        | crate::state::OptionsKind::MessageActions
                        | crate::state::OptionsKind::WorkspaceActions
                        | crate::state::OptionsKind::WorkspaceCreateName
                        | crate::state::OptionsKind::WorkspaceCreateBase
                        | crate::state::OptionsKind::WorkspaceCreateCustomBase
                        | crate::state::OptionsKind::WorkspaceResetConfirmation
                        | crate::state::OptionsKind::WorkspaceRemoveConfirmation
                        | crate::state::OptionsKind::Tools
                        | crate::state::OptionsKind::SessionActions
                        | crate::state::OptionsKind::WorkflowDashboard
                        | crate::state::OptionsKind::WorkflowRunActions
                        | crate::state::OptionsKind::GoalActions
                        | crate::state::OptionsKind::GoalClearConfirmation
                        | crate::state::OptionsKind::GoalCompletion => false,
                    };
                    SelectorRow::new(entry.label, entry.description, current)
                })
                .collect(),
        ),
        Overlay::None
        | Overlay::Panel { .. }
        | Overlay::Diff(_)
        | Overlay::Workflow { .. }
        | Overlay::ProviderLogin => {
            unreachable!()
        }
    };

    app.max_overlay_scroll.set(0);
    let theme = &app.theme;
    let card = ModalCard::new(app, width);
    let filterable = app.overlay.is_filterable();
    // A view's own words go above its choices, so a status or a hint is read
    // in place instead of hiding behind a row called "Details".
    let preface = preface_rows(app, card.content_width(), max_height / 3);
    let body_budget = max_height
        .saturating_sub(if filterable { 7 } else { 6 })
        .saturating_sub(if preface.is_empty() {
            0
        } else {
            preface.len() + 1
        })
        .max(1);
    let selection = app.selection.min(rows.len().saturating_sub(1));
    let layout = SelectorLayout::measure(&rows, card.content_width());
    let (start, end) = selector_window(&rows, selection, body_budget, layout.stacked);

    let mut out = TaggedLines::default();
    out.push(card.top());
    out.push(card.header(app.language.text(title)));
    if filterable {
        out.push(card.row(
            scroll_input_line(search_row(app), card.content_width()),
            false,
        ));
    }
    out.push(card.separator());
    if !preface.is_empty() {
        for row in preface {
            out.push(card.row(row, false));
        }
        out.push(card.separator());
    }
    if rows.is_empty() {
        out.push(card.row(empty_row(app), false));
    } else {
        for (index, row) in rows.iter().enumerate().skip(start).take(end - start) {
            let selected = index == selection;
            for line in row.lines(theme, &layout) {
                out.push_tagged(card.row(line, selected), HitTarget::MenuRow(index));
            }
        }
    }
    out.push(card.separator());
    out.push(card.footer(&selector_footer(app, start, end, rows.len())));
    out.push(card.bottom());
    Some(out)
}

/// One choice in a picker: what it is, what it means, whether it is the
/// current one, and — for the theme picker — the palette it stands for.
struct SelectorRow {
    label: String,
    detail: String,
    current: bool,
    swatch: Option<[haarness_opentui::Color; 4]>,
}

impl SelectorRow {
    fn new(label: impl Into<String>, detail: impl Into<String>, current: bool) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            current,
            swatch: None,
        }
    }

    const fn with_swatch(mut self, swatch: [haarness_opentui::Color; 4]) -> Self {
        self.swatch = Some(swatch);
        self
    }

    fn height(&self, stacked: bool) -> usize {
        usize::from(stacked && !self.detail.is_empty()) + 1
    }

    fn lines(&self, theme: &crate::theme::Theme, layout: &SelectorLayout) -> Vec<Line> {
        let marker = if self.current { "●" } else { " " };
        let marker_color = if self.current {
            theme.primary
        } else {
            theme.muted
        };
        let mut primary = vec![
            Span::new(marker, marker_color),
            Span::new("  ", theme.muted),
        ];
        if let Some(swatch) = self.swatch {
            for color in swatch {
                primary.push(Span::new("█", color));
            }
            primary.push(Span::new("  ", theme.muted));
        }
        let label = single_line(&self.label, layout.label_width);
        primary.push(Span::new(label.clone(), theme.text).with_attributes(attr::BOLD));

        if layout.stacked {
            let mut lines = vec![Line::new(primary)];
            if !self.detail.is_empty() {
                let detail = single_line(&self.detail, layout.content_width.saturating_sub(4));
                lines.push(Line::new(vec![
                    Span::new("   ", theme.muted),
                    Span::new(detail, theme.muted),
                ]));
            }
            return lines;
        }
        // One row, two aligned columns: the labels line up, so the eye scans
        // the list vertically instead of following a ragged edge.
        if !self.detail.is_empty() {
            let pad = layout.label_width.saturating_sub(label.width()) + 2;
            let used = layout.gutter() + layout.label_width + 2;
            let detail = single_line(&self.detail, layout.content_width.saturating_sub(used));
            if !detail.is_empty() {
                primary.push(Span::new(" ".repeat(pad), theme.muted));
                primary.push(Span::new(detail, theme.muted));
            }
        }
        vec![Line::new(primary)]
    }
}

/// How a picker's rows are laid out at this width: one aligned row per choice
/// wherever the description has room, stacked only when it does not.
struct SelectorLayout {
    content_width: usize,
    label_width: usize,
    stacked: bool,
    swatch: bool,
}

impl SelectorLayout {
    /// Columns before the label: the current-choice dot, its gap, and any
    /// palette chips.
    const fn gutter(&self) -> usize {
        if self.swatch { 3 + 4 + 2 } else { 3 }
    }

    fn measure(rows: &[SelectorRow], content_width: usize) -> Self {
        let swatch = rows.iter().any(|row| row.swatch.is_some());
        let mut layout = Self {
            content_width,
            label_width: content_width,
            stacked: true,
            swatch,
        };
        let widest = rows
            .iter()
            .map(|row| row.label.width())
            .max()
            .unwrap_or_default();
        // Labels take what they need up to two fifths of the card; whatever
        // is left has to be enough for a description to be worth reading.
        let cap = (content_width * 2 / 5).max(8);
        let label_width = widest.clamp(1, cap);
        let remaining = content_width.saturating_sub(layout.gutter() + label_width + 2);
        // Layout must not jump when an asynchronously refreshed provider adds
        // or removes model metadata. Choose density from the available columns,
        // not the current detail lengths: wide cards stay aligned and truncate,
        // narrow cards consistently stack their descriptions.
        let has_detail = rows.iter().any(|row| !row.detail.is_empty());
        layout.stacked = has_detail && remaining < 32;
        layout.label_width = if layout.stacked {
            content_width.saturating_sub(layout.gutter())
        } else {
            label_width
        };
        layout
    }
}

/// The filter line. Empty reads as an invitation, not as the word "Search"
/// with a cursor stuck to it.
/// Selector fields edit at the end of their text. Keep that insertion point
/// visible, retaining complete graphemes and styles when the beginning scrolls.
fn scroll_input_line(line: Line, width: usize) -> Line {
    if line.width() <= width {
        return line;
    }
    if width == 0 {
        return Line::empty();
    }
    let budget = width.saturating_sub(1);
    let mut used = 0;
    let mut suffix = Vec::new();
    'outer: for span in line.spans.iter().rev() {
        for grapheme in span.text.graphemes(true).rev() {
            let cells = grapheme.width();
            if used + cells > budget {
                break 'outer;
            }
            used += cells;
            let mut part = span.clone();
            part.text = grapheme.into();
            suffix.push(part);
        }
    }
    suffix.reverse();
    if width == 1 {
        let mut cursor = line.spans.last().unwrap().clone();
        cursor.text = "▌".into();
        return Line::new(vec![cursor]);
    }
    let mut prefix = line.spans[0].clone();
    prefix.text = "…".into();
    suffix.insert(0, prefix);
    Line::new(suffix)
}

fn search_row(app: &App) -> Line {
    let theme = &app.theme;
    if matches!(
        app.overlay,
        Overlay::Options {
            kind: crate::state::OptionsKind::ConnectApiKey,
            ..
        }
    ) {
        let mut spans = vec![Span::new("Key  ", theme.muted)];
        if app.overlay_filter.is_empty() {
            spans.push(Span::new("paste it here", theme.muted));
        } else {
            spans.push(Span::new(
                "•".repeat(app.overlay_filter.chars().count().min(40)),
                theme.text,
            ));
        }
        spans.push(Span::new("▌", theme.primary));
        return Line::new(spans);
    }
    if matches!(
        app.overlay,
        Overlay::Options {
            kind: crate::state::OptionsKind::ManagementField,
            ..
        }
    ) {
        return Line::new(vec![
            Span::new(
                if app.management_secret() {
                    "•".repeat(app.overlay_filter.chars().count().min(40))
                } else {
                    app.overlay_filter.replace('\n', " ↵ ")
                },
                theme.text,
            ),
            Span::new("▌", theme.primary),
        ]);
    }
    let prompt = match app.overlay {
        Overlay::Options {
            kind: crate::state::OptionsKind::InputQueueEdit,
            ..
        } => Some(("Message", "Edit queued message")),
        Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceCreateName,
            ..
        } => Some(("Name", "Type a worktree name")),
        Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceCreateCustomBase,
            ..
        } => Some(("Base ref", "branch, tag, or commit SHA")),
        _ => None,
    };
    if app.overlay_filter.is_empty() {
        if let Some((label, placeholder)) = prompt {
            return Line::new(vec![
                Span::new(format!("{label}  "), theme.muted),
                Span::new(placeholder, theme.muted),
                Span::new("▌", theme.primary),
            ]);
        }
        return Line::new(vec![
            Span::new("Type to filter", theme.muted),
            Span::new("▌", theme.primary),
        ]);
    }
    if let Some((label, _)) = prompt {
        return Line::new(vec![
            Span::new(format!("{label}  "), theme.muted),
            Span::new(app.overlay_filter.clone(), theme.text),
            Span::new("▌", theme.primary),
        ]);
    }
    Line::new(vec![
        Span::new("Search  ", theme.muted),
        Span::new(app.overlay_filter.clone(), theme.text),
        Span::new("▌", theme.primary),
    ])
}

fn empty_row(app: &App) -> Line {
    let theme = &app.theme;
    if matches!(
        app.overlay,
        Overlay::Options {
            kind: crate::state::OptionsKind::ManagementField,
            ..
        }
    ) {
        return line(
            "Enter to apply · Ctrl+U to clear · Escape to cancel",
            theme.muted,
        );
    }
    if app.overlay_filter.is_empty() {
        return line("Nothing to choose from", theme.muted);
    }
    Line::new(vec![
        Span::new("No match for ", theme.muted),
        Span::new(
            format!("\u{201c}{}\u{201d}", app.overlay_filter),
            theme.text,
        ),
        Span::new("  \u{00b7}  backspace to widen", theme.muted),
    ])
}

/// Keep the selected item visible while bounding the list by rendered rows,
/// not just item count (normal-width entries can occupy two rows).
fn selector_window(
    rows: &[SelectorRow],
    selection: usize,
    budget: usize,
    stacked: bool,
) -> (usize, usize) {
    if rows.is_empty() {
        return (0, 0);
    }
    let selection = selection.min(rows.len() - 1);
    let mut start = selection;
    let mut end = selection + 1;
    let mut used = rows[selection].height(stacked);
    let before_target = budget / 2;
    while start > 0 && used < before_target {
        let height = rows[start - 1].height(stacked);
        if used + height > budget {
            break;
        }
        start -= 1;
        used += height;
    }
    while end < rows.len() {
        let height = rows[end].height(stacked);
        if used + height > budget {
            break;
        }
        end += 1;
        used += height;
    }
    while start > 0 {
        let height = rows[start - 1].height(stacked);
        if used + height > budget {
            break;
        }
        start -= 1;
        used += height;
    }
    (start, end)
}

fn selector_footer(app: &App, start: usize, end: usize, total: usize) -> String {
    let range = if total == 0 {
        "0 of 0".to_owned()
    } else {
        format!("{}\u{2013}{end} of {total}", start + 1)
    };
    if matches!(app.overlay, Overlay::Threads) {
        format!("{range}  \u{00b7}  enter resume  \u{00b7}  \u{2192} actions")
    } else if matches!(app.overlay, Overlay::Models) {
        format!("{range}  ·  Ctrl+F favorite  ·  Enter select")
    } else if matches!(
        app.overlay,
        Overlay::Options {
            kind: crate::state::OptionsKind::Workspaces,
            ..
        }
    ) {
        format!("{range}  \u{00b7}  enter actions  \u{00b7}  type to filter")
    } else if matches!(
        app.overlay,
        Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceCreateName,
            ..
        }
    ) {
        "enter continue  ·  esc cancel".into()
    } else if matches!(
        app.overlay,
        Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceCreateBase,
            ..
        }
    ) {
        format!("{range}  ·  enter choose  ·  esc back  ·  type to filter")
    } else if matches!(
        app.overlay,
        Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceCreateCustomBase,
            ..
        }
    ) {
        "enter check  ·  esc back".into()
    } else if matches!(
        app.overlay,
        Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceActions
                | crate::state::OptionsKind::WorkspaceResetConfirmation
                | crate::state::OptionsKind::WorkspaceRemoveConfirmation,
            ..
        }
    ) {
        format!("{range}  \u{00b7}  enter select  \u{00b7}  esc back")
    } else if matches!(
        app.overlay,
        Overlay::Options {
            kind: crate::state::OptionsKind::WorkflowDashboard,
            ..
        }
    ) {
        format!("{range}  \u{00b7}  enter run / actions  \u{00b7}  \u{2192} inspect")
    } else if matches!(
        app.overlay,
        Overlay::Options {
            kind: crate::state::OptionsKind::ConnectApiKey,
            ..
        }
    ) {
        "enter connect  ·  ctrl+u clear  ·  esc back".into()
    } else if matches!(
        app.overlay,
        Overlay::Options {
            kind: crate::state::OptionsKind::ConnectMethod,
            ..
        }
    ) {
        format!("{range}  ·  enter choose  ·  esc back")
    } else if matches!(
        app.overlay,
        Overlay::Options {
            kind: crate::state::OptionsKind::ConnectProvider,
            ..
        }
    ) {
        format!("{range}  ·  enter connect  ·  type to filter")
    } else {
        format!("{range}  \u{00b7}  \u{2191}\u{2193} navigate  \u{00b7}  enter select")
    }
}

/// The live run inspector. Unlike a `Panel`, whose text is frozen when it
/// opens, this rebuilds from the cached run snapshot every frame — so a
/// running workflow's phase, agents, and log tail move while you watch.
/// Footer builders differ per target: a run scrolls its report, a definition
/// advertises that Enter launches it.
type Footer = fn(usize, usize) -> String;

fn workflow_dialog(
    app: &App,
    target: &crate::state::WorkflowTarget,
    width: usize,
    max_height: usize,
) -> TaggedLines {
    let card = ModalCard::new(app, width);
    let content = card.content_width();
    let (title, body, footer): (String, Vec<Line>, Footer) = match target {
        crate::state::WorkflowTarget::Run(id) => match app
            .workflow_runs
            .iter()
            .find(|candidate| candidate.id == *id)
        {
            Some(run) => (
                format!("Workflow · {}", run.workflow_name),
                crate::view::workflow::run_body(app, run, content),
                crate::view::workflow::run_footer,
            ),
            None => (
                "Workflow run".to_owned(),
                vec![line("This run is no longer cached.", app.theme.muted)],
                crate::view::workflow::run_footer,
            ),
        },
        crate::state::WorkflowTarget::Invalid(index) => match app.workflow_invalid.get(*index) {
            Some(invalid) => (
                "Invalid workflow".to_owned(),
                crate::view::workflow::invalid_body(app, invalid, content),
                crate::view::workflow::invalid_footer,
            ),
            None => (
                "Invalid workflow".to_owned(),
                vec![line("This file is no longer listed.", app.theme.muted)],
                crate::view::workflow::invalid_footer,
            ),
        },
        crate::state::WorkflowTarget::Definition(invocation) => match app
            .workflow_definitions
            .iter()
            .find(|candidate| candidate.invocation == *invocation)
        {
            Some(definition) => (
                format!("Workflow · /{}", definition.invocation),
                crate::view::workflow::definition_body(app, definition, content),
                crate::view::workflow::definition_footer,
            ),
            None => (
                "Workflow".to_owned(),
                vec![line(
                    "This definition is no longer listed.",
                    app.theme.muted,
                )],
                crate::view::workflow::definition_footer,
            ),
        },
    };

    let total = body.len();
    let cap = max_height.saturating_sub(6).max(1);
    let max_scroll = total.saturating_sub(cap);
    app.max_overlay_scroll.set(max_scroll);
    let start = app.overlay_scroll.min(max_scroll);
    let end = (start + cap).min(total);
    let mut out = TaggedLines::default();
    out.push(card.top());
    out.push(card.header(&title));
    out.push(card.separator());
    for row in body.into_iter().skip(start).take(end - start) {
        out.push(card.row(row, false));
    }
    out.push(card.separator());
    out.push(card.footer(&footer(end.saturating_sub(start), total)));
    out.push(card.bottom());
    out
}

/// Rows printed between a picker's header and its choices: a management
/// view's first lines, or the hints above the API-key prompt. Each source
/// line gets one row when it fits and a wrapped continuation when it does
/// not, within a cap so the choices stay on screen.
fn preface_rows(app: &App, width: usize, cap: usize) -> Vec<Line> {
    let lines: Vec<String> = match app.overlay {
        Overlay::Options {
            kind: crate::state::OptionsKind::Management,
            ..
        } => app
            .management
            .view
            .lines
            .iter()
            .take(crate::management::PREFACE_LINES)
            .flat_map(|line| line.lines().map(str::to_owned))
            .collect(),
        Overlay::Options {
            kind: crate::state::OptionsKind::ConnectApiKey,
            ..
        } => app.connect_key_hints(),
        _ => Vec::new(),
    };
    if lines.is_empty() || cap == 0 {
        return Vec::new();
    }
    let items: Vec<PanelItem> = lines.iter().map(|value| PanelItem::parse(value)).collect();
    let label_column = items
        .iter()
        .filter(|item| item.plain.is_none())
        .map(|item| item.marker_width() + item.label.width())
        .max()
        .unwrap_or_default()
        .min(width * 2 / 5)
        .max(1);
    let mut rows = Vec::new();
    for item in &items {
        let mut item_rows = item.lines(app, width, label_column);
        // Two rows per line is plenty for a hint; the full text lives on the
        // details page when there is more.
        item_rows.truncate(2);
        rows.extend(item_rows);
        if rows.len() >= cap {
            rows.truncate(cap);
            break;
        }
    }
    rows
}

/// The sign-in wait screen: where to go, what to type, and whether the
/// server has seen the browser come back yet.
fn login_dialog(app: &App, width: usize, max_height: usize) -> TaggedLines {
    let theme = &app.theme;
    let card = ModalCard::new(app, width);
    let content = card.content_width();
    let Some(login) = app.connect.login.as_ref() else {
        let mut out = TaggedLines::default();
        out.push(card.top());
        out.push(card.header("Sign in"));
        out.push(card.separator());
        out.push(card.row(line("No sign-in is running", theme.muted), false));
        out.push(card.bottom());
        return out;
    };
    let title = match login.state {
        crate::connect::LoginState::Pending => format!("Sign in to {}", login.name),
        crate::connect::LoginState::Connected => format!("Connected {}", login.name),
        crate::connect::LoginState::Failed(_) => format!("Could not connect {}", login.name),
    };
    let mut body: Vec<Line> = Vec::new();
    let step = |number: &str, text: &str| {
        Line::new(vec![
            Span::new(format!("{number}  "), theme.muted),
            Span::new(text.to_owned(), theme.text),
        ])
    };
    match &login.state {
        crate::connect::LoginState::Pending => {
            body.push(step(
                "1",
                if login.opened {
                    "Your browser opened the sign-in page. If not, open:"
                } else {
                    "Open this page in a browser:"
                },
            ));
            body.extend(wrap_panel_text(&login.url, content, "   ", theme.primary));
            if !login.code.is_empty() {
                body.push(Line::empty());
                body.push(step("2", "Enter this code when asked:"));
                body.push(Line::new(vec![
                    Span::new("   ", theme.muted),
                    Span::new(login.code.clone(), theme.accent).with_attributes(attr::BOLD),
                ]));
            }
            body.push(Line::empty());
            let spinner = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let frame = spinner[usize::try_from((app.tick / 2) % 10).unwrap_or(0)];
            let waited = login.started.elapsed().as_secs();
            body.push(Line::new(vec![
                Span::new(format!("{frame} "), theme.primary),
                Span::new(
                    format!(
                        "Waiting for you to finish in the browser… {}",
                        if waited >= 60 {
                            format!("{}m {:02}s", waited / 60, waited % 60)
                        } else {
                            format!("{waited}s")
                        }
                    ),
                    theme.muted,
                ),
            ]));
        }
        crate::connect::LoginState::Connected => {
            body.push(line(format!("{} is ready to use.", login.name), theme.text));
        }
        crate::connect::LoginState::Failed(error) => {
            body.extend(wrap_panel_text(error, content, "", theme.danger));
            body.push(Line::empty());
            body.push(line("Enter to try again, or Esc to go back.", theme.muted));
        }
    }
    let cap = max_height.saturating_sub(6).max(1);
    body.truncate(cap);
    let mut out = TaggedLines::default();
    out.push(card.top());
    out.push(card.header(&title));
    out.push(card.separator());
    for row in body {
        out.push(card.row(row, false));
    }
    out.push(card.separator());
    out.push(card.footer(match login.state {
        crate::connect::LoginState::Pending if login.code.is_empty() => {
            "c copy link  ·  o open browser  ·  esc cancel"
        }
        crate::connect::LoginState::Pending => "c copy code  ·  o open browser  ·  esc cancel",
        crate::connect::LoginState::Connected => "enter continue",
        crate::connect::LoginState::Failed(_) => "enter retry  ·  esc back",
    }));
    out.push(card.bottom());
    out
}

fn panel_dialog(
    app: &App,
    title: &str,
    values: &[String],
    width: usize,
    max_height: usize,
) -> TaggedLines {
    let card = ModalCard::new(app, width);
    let content = card.content_width();
    let items: Vec<PanelItem> = values.iter().map(|value| PanelItem::parse(value)).collect();
    // One label column across the whole panel, so status reads as a table
    // rather than as a stack of paragraphs.
    let label_column = items
        .iter()
        .filter(|item| item.plain.is_none())
        .map(|item| item.marker_width() + item.label.width())
        .max()
        .unwrap_or_default()
        .min(content * 2 / 5)
        .max(1);
    let mut body = Vec::new();
    for (item_index, item) in items.iter().enumerate() {
        let rows = item.lines(app, content, label_column);
        let tag = item.marker.is_some().then_some(item_index);
        body.extend(rows.into_iter().map(|row| (row, tag)));
    }
    if body.is_empty() {
        body.push((line("No information available", app.theme.muted), None));
    }
    let total = body.len();
    let cap = max_height.saturating_sub(6).max(1);
    let max_scroll = total.saturating_sub(cap);
    app.max_overlay_scroll.set(max_scroll);
    let start = app.overlay_scroll.min(max_scroll);
    let end = (start + cap).min(total);
    let mut out = TaggedLines::default();
    out.push(card.top());
    out.push(card.header(app.language.text(title)));
    out.push(card.separator());
    for (row, item) in body.into_iter().skip(start).take(end - start) {
        let row = card.row(row, false);
        if let Some(item) = item {
            out.push_tagged(row, HitTarget::DialogItem(item));
        } else {
            out.push(row);
        }
    }
    out.push(card.separator());
    out.push(card.footer(&format!(
        "Rows {}\u{2013}{end} of {total}  \u{00b7}  pgup/pgdn scroll",
        start + 1,
    )));
    out.push(card.bottom());
    out
}

/// One line of an informational panel, read as a status marker, a label, and
/// the metadata behind it. Anything that is not that shape stays prose.
struct PanelItem {
    marker: Option<String>,
    label: String,
    description: String,
    plain: Option<String>,
}

impl PanelItem {
    fn parse(value: &str) -> Self {
        let prose = |value: &str| Self {
            marker: None,
            label: String::new(),
            description: String::new(),
            plain: Some(value.to_owned()),
        };
        let fields = value
            .split("  ")
            .map(str::trim)
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();
        if fields.len() < 2 {
            return prose(value);
        }
        let marker = matches!(
            fields[0],
            "\u{25cf}"
                | "\u{25cb}"
                | "\u{25d0}"
                | "\u{2713}"
                | "\u{2717}"
                | "\u{26a0}"
                | "\u{2298}"
        )
        .then(|| fields[0].to_owned());
        let label_index = usize::from(marker.is_some());
        let Some(label) = fields.get(label_index) else {
            return prose(value);
        };
        // The caller may already have written its own separators. Drop those
        // so rejoining cannot produce "running  ·  ·  ·  http://…".
        let description = fields
            .iter()
            .skip(label_index + 1)
            .copied()
            .filter(|field| *field != "\u{00b7}")
            .collect::<Vec<_>>()
            .join("  \u{00b7}  ");
        Self {
            marker,
            label: (*label).to_owned(),
            description,
            plain: None,
        }
    }

    fn marker_width(&self) -> usize {
        self.marker.as_ref().map_or(0, |marker| marker.width() + 2)
    }

    fn lines(&self, app: &App, width: usize, label_column: usize) -> Vec<Line> {
        let theme = &app.theme;
        if let Some(plain) = &self.plain {
            return wrap_panel_text(plain, width, "", theme.text);
        }
        let mut primary = Vec::new();
        if let Some(marker) = &self.marker {
            primary.push(Span::new(marker.clone(), theme.primary));
            primary.push(Span::new("  ", theme.muted));
        }
        let label_room = label_column.saturating_sub(self.marker_width()).max(1);
        let label = single_line(&self.label, label_room);
        primary.push(Span::new(label.clone(), theme.text).with_attributes(attr::BOLD));
        if self.description.is_empty() {
            return vec![Line::new(primary)];
        }
        let used = self.marker_width() + label.width();
        let remaining = width.saturating_sub(label_column + 2);
        // Keep the pair on one row when the metadata fits; only a genuinely
        // long value earns a wrapped continuation.
        if self.description.width() <= remaining && remaining >= 12 {
            primary.push(Span::new(
                " ".repeat(label_column.saturating_sub(used) + 2),
                theme.muted,
            ));
            primary.push(Span::new(self.description.clone(), theme.muted));
            return vec![Line::new(primary)];
        }
        let mut rows = vec![Line::new(primary)];
        rows.extend(wrap_panel_text(
            &self.description,
            width,
            "    ",
            theme.muted,
        ));
        rows
    }
}

/// Compact fallback for terminals too small for the full review surface
/// (under 40x12). It lists what changed and says where the real viewer is,
/// rather than crushing a file tree and a patch into six rows.
fn diff_dialog(
    app: &App,
    viewer: &crate::diffview::DiffViewer,
    width: usize,
    max_height: usize,
) -> TaggedLines {
    let card = ModalCard::new(app, width);
    let content_width = card.content_width();
    let mut body = vec![
        line("Widen the terminal to review", app.theme.accent),
        Line::empty(),
    ];
    if viewer.files.is_empty() {
        body.push(line("No changes", app.theme.muted));
    }
    for file in &viewer.files {
        let counts = format!("+{} −{}", file.additions, file.deletions);
        let name = crate::view::span::single_line(
            &file.file,
            content_width.saturating_sub(counts.chars().count() + 2),
        );
        let pad = content_width.saturating_sub(name.chars().count() + counts.chars().count());
        body.push(Line::new(vec![
            Span::new(name, app.theme.text),
            Span::new(" ".repeat(pad), app.theme.muted),
            Span::new(format!("+{}", file.additions), app.theme.diff.added),
            Span::new(format!(" −{}", file.deletions), app.theme.diff.removed),
        ]));
    }
    let total = body.len();
    let cap = max_height.saturating_sub(6).max(1);
    let max_scroll = total.saturating_sub(cap);
    app.max_overlay_scroll.set(max_scroll);
    let start = app.overlay_scroll.min(max_scroll);
    let end = (start + cap).min(total);
    let mut out = TaggedLines::default();
    out.push(card.top());
    out.push(card.header(&format!("Diff · {}", viewer.source.label())));
    out.push(card.separator());
    for row in body.into_iter().skip(start).take(end.saturating_sub(start)) {
        out.push(card.row(row, false));
    }
    out.push(card.separator());
    out.push(card.footer(&format!(
        "{} file{}  ·  esc closes",
        viewer.files.len(),
        if viewer.files.len() == 1 { "" } else { "s" },
    )));
    out.push(card.bottom());
    out
}

fn wrap_panel_text(
    value: &str,
    width: usize,
    prefix: &str,
    color: haarness_opentui::Color,
) -> Vec<Line> {
    let wrap_width = width.saturating_sub(prefix.width()).max(1);
    let mut rows = Vec::new();
    for paragraph in value.lines() {
        if paragraph.is_empty() {
            rows.push(line(prefix, color));
            continue;
        }
        for wrapped in textwrap::wrap(paragraph, wrap_width) {
            rows.push(line(format!("{prefix}{wrapped}"), color));
        }
    }
    if rows.is_empty() {
        rows.push(line(prefix, color));
    }
    rows
}

/// Reusable visible card shell for every explicit overlay. Composer-attached
/// autocomplete intentionally does not use this surface.
struct ModalCard<'a> {
    app: &'a App,
    width: usize,
}

impl<'a> ModalCard<'a> {
    const fn new(app: &'a App, width: usize) -> Self {
        Self { app, width }
    }

    fn content_width(&self) -> usize {
        self.width.saturating_sub(4)
    }

    /// The card is a solid raised panel, not a drawn box: one accent cap
    /// defines its top edge and the surface fill does the rest. Box-drawing
    /// corners belong to the terminal era this interface is trying to leave.
    fn top(&self) -> Line {
        Line::new(vec![
            Span::new("━".repeat(self.width), self.app.theme.primary).on(self.app.theme.overlay),
        ])
    }

    fn header(&self, title: &str) -> Line {
        let hint = if self.content_width() >= 24 {
            if matches!(
                self.app.overlay,
                Overlay::Options {
                    kind: crate::state::OptionsKind::GoalClearConfirmation,
                    ..
                }
            ) {
                "esc back"
            } else {
                "esc close"
            }
        } else {
            ""
        };
        let title = single_line(title, self.content_width().saturating_sub(hint.width() + 2));
        let fill = self
            .content_width()
            .saturating_sub(title.width() + hint.width());
        self.row(
            Line::new(vec![
                Span::new(title, self.app.theme.text).with_attributes(attr::BOLD),
                Span::new(" ".repeat(fill), self.app.theme.muted),
                Span::new(hint, self.app.theme.muted),
            ]),
            false,
        )
    }

    /// An inset hairline, the same structural language as the composer rule.
    fn separator(&self) -> Line {
        self.row(
            line("─".repeat(self.content_width()), self.app.theme.hairline()),
            false,
        )
    }

    fn row(&self, content: Line, selected: bool) -> Line {
        if self.width == 0 {
            return Line::empty();
        }
        let padding = 2.min(self.width / 2);
        let mut content = fit_line(
            content,
            self.width.saturating_sub(padding * 2),
            self.app.theme.text,
            self.app.theme.overlay,
        );
        content.spans.insert(
            0,
            Span::new(" ".repeat(padding), self.app.theme.text).on(self.app.theme.overlay),
        );
        content
            .spans
            .push(Span::new(" ".repeat(padding), self.app.theme.text).on(self.app.theme.overlay));
        if selected {
            selected_row(&mut content, self.width, &self.app.theme);
        }
        content
    }

    fn footer(&self, value: &str) -> Line {
        self.row(
            line(
                single_line(value, self.content_width()),
                self.app.theme.muted,
            ),
            false,
        )
    }

    fn bottom(&self) -> Line {
        self.row(Line::empty(), false)
    }
}

fn fit_line(
    line: Line,
    width: usize,
    color: haarness_opentui::Color,
    background: haarness_opentui::Color,
) -> Line {
    let mut spans: Vec<Span> = Vec::new();
    let mut used = 0;
    'outer: for span in line.spans {
        for grapheme in span.text.graphemes(true) {
            let character_width = grapheme.width();
            if used + character_width > width {
                break 'outer;
            }
            let mut character_span = span.clone();
            character_span.text = grapheme.into();
            character_span.bg = Some(background);
            if let Some(previous) = spans.last_mut()
                && previous.fg == character_span.fg
                && previous.bg == character_span.bg
                && previous.attributes == character_span.attributes
            {
                previous.text.push_str(grapheme);
            } else {
                spans.push(character_span);
            }
            used += character_width;
        }
    }
    if used < width {
        spans.push(Span::new(" ".repeat(width - used), color).on(background));
    }
    Line::new(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_inputs_keep_the_cursor_and_whole_unicode_suffix_visible() {
        for width in [1, 12, 20, 80] {
            let value = format!("Search  {}👨‍👩‍👧‍👦🇯🇵-END▌", "東京".repeat(80));
            let input = Line::plain(value, haarness_opentui::Color::rgb(255, 255, 255));
            let rendered = scroll_input_line(input, width);
            assert!(
                rendered.width() <= width,
                "width={width}: {} ({})",
                rendered.text(),
                rendered.width()
            );
            assert!(rendered.text().ends_with('▌'));
            if width >= 12 {
                assert!(rendered.text().ends_with("-END▌"));
            }
            let fitted = fit_line(
                rendered,
                width,
                haarness_opentui::Color::rgb(255, 255, 255),
                haarness_opentui::Color::rgb(0, 0, 0),
            );
            assert!(fitted.text().trim_end().ends_with('▌'));
        }
    }

    #[test]
    fn model_limits_are_compact_without_losing_useful_precision() {
        assert_eq!(token_limit(1_050_000), "1.05M");
        assert_eq!(token_limit(1_048_576), "1.05M");
        assert_eq!(token_limit(128_000), "128K");
        assert_eq!(token_limit(8192), "8.2K");
    }
}
