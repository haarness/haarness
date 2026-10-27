//! Selector menus (commands, threads, models, agents) and info panels.

use crate::width::DisplayWidth as _;
use haarness_opentui::{Line, Span, attr};
use unicode_segmentation::UnicodeSegmentation as _;

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
        Overlay::Threads => {
            let waiting = app.waiting_chats();
            (
                "threads",
                app.filtered_threads()
                    .into_iter()
                    .map(|thread| {
                        let title = match waiting.get(&thread.id).copied().map(waiting_sign) {
                            Some(sign) => format!("{sign} {}", thread.title),
                            None => thread.title.clone(),
                        };
                        (title, short_id(&thread.id.to_string()).into())
                    })
                    .collect(),
            )
        }
        Overlay::Models => (
            "models",
            app.filtered_models()
                .into_iter()
                .map(|model| {
                    let active = model == format!("{}/{}", app.thread.provider, app.thread.model);
                    let status = if active {
                        app.language.text("active")
                    } else {
                        ""
                    };
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
                            app.language.text("active").into()
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
                let resources = app
                    .mention_suggestions
                    .iter()
                    .any(|path| crate::resource_mentions::find(app, path).is_some());
                // One order whatever matches, the masthead's: the header
                // says what `@` reaches, not which kind matched first.
                let title = if query.starts_with('/') || query.starts_with('~') || dot_path {
                    "files · local paths"
                } else if resources {
                    "files · agents · resources"
                } else {
                    "files · agents"
                };
                (
                    title,
                    app.mention_suggestions
                        .iter()
                        .map(|path| {
                            if let Some(resource) = crate::resource_mentions::find(app, path) {
                                crate::resource_mentions::row(resource)
                            } else if let Some(agent) = path.strip_prefix("agent://") {
                                (
                                    format!("@{agent}"),
                                    app.language.text("Delegate to agent").into(),
                                )
                            } else if path.ends_with('/') {
                                (path.clone(), app.language.text("Folder").into())
                            } else {
                                // A path says what it is; "File snapshot" on
                                // every row only pushed the paths apart.
                                (path.clone(), String::new())
                            }
                        })
                        .collect(),
                )
            }
        }
        Overlay::Panel { .. }
        | Overlay::Diff(_)
        | Overlay::Workflow { .. }
        | Overlay::ProviderLogin
        | Overlay::RemoteControl
        | Overlay::Chrome
        | Overlay::SideChat => {
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
        lines.push(line(
            format!("  {}", app.language.text("No matches")),
            theme.muted,
        ));
        return lines;
    }
    let visible = rows.len().min(7).min(max_height - 1);
    let start = app.selection.saturating_sub(visible - 1);
    // One column for the labels that carry a description, so a long one
    // does not shove its description out of line with the rows around it.
    let label_width = rows
        .iter()
        .skip(start)
        .take(visible)
        .filter(|(_, description)| !description.is_empty())
        .map(|(label, _)| label.width())
        .max()
        .unwrap_or_default()
        .clamp(18, (width / 2).max(18))
        .min(width.saturating_sub(8));
    for (index, (label, description)) in rows.into_iter().enumerate().skip(start).take(visible) {
        let marker = if index == app.selection { "›" } else { " " };
        let body = if description.is_empty() {
            // A bare path gets the whole row, and an over-long one keeps
            // its end: the file name is what tells two rows apart.
            format!(
                "  {marker} {}",
                leading_ellipsis(&label, width.saturating_sub(5))
            )
        } else {
            let label = single_line(&label, label_width);
            format!(
                "  {marker} {label:<label_width$} {}",
                single_line(&description, width.saturating_sub(label_width + 7))
            )
        };
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

/// What sets one model apart from the next: the id to type (whose prefix
/// also says who serves it), how much it can read, and what it costs.
/// Abilities nearly every model shares (reasoning, vision, tools) and the
/// output ceiling are noise in a list of fifty; only a missing tool ability
/// is worth a word, since agents depend on it.
pub(crate) fn model_detail(app: &App, qualified: &str, status: &str) -> String {
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
        if let Some(context) = info.context_window {
            parts.push(format!("{} context", token_limit(context)));
        }
        if !info.tools {
            parts.push("no tools".to_owned());
        }
        // A subscription model is priced at zero; "$0/0" says nothing useful.
        if let Some(pricing) = info
            .pricing
            .filter(|pricing| pricing.input > 0.0 || pricing.output > 0.0)
        {
            parts.push(format!(
                "${}/{} per MTok",
                compact_price(pricing.input),
                compact_price(pricing.output)
            ));
            if let Some(tier) = pricing.long_context {
                parts.push(format!(
                    "${}/{} over {}",
                    compact_price(tier.input),
                    compact_price(tier.output),
                    token_limit(tier.above_input_tokens)
                ));
            }
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

/// Fit `text` into `width` cells by dropping its start rather than its end.
fn leading_ellipsis(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut kept: Vec<char> = Vec::new();
    let mut used = 1;
    for character in text.chars().rev() {
        let cell = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if used + cell > width {
            break;
        }
        used += cell;
        kept.push(character);
    }
    std::iter::once('…').chain(kept.into_iter().rev()).collect()
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
    if matches!(app.overlay, Overlay::RemoteControl) {
        return Some(remote_control_dialog(app, width, max_height));
    }
    if matches!(app.overlay, Overlay::Chrome) {
        return Some(chrome_dialog(app, width, max_height));
    }

    let (title, rows): (&str, Vec<SelectorRow>) = match app.overlay {
        Overlay::Commands => (
            "Commands",
            app.filtered_slash_entries(&app.overlay_filter)
                .into_iter()
                .map(|entry| SelectorRow::new(format!("/{}", entry.name), entry.description, false))
                .collect(),
        ),
        Overlay::Threads => {
            let waiting = app.waiting_chats();
            (
                "Sessions",
                app.filtered_threads()
                    .into_iter()
                    .map(|thread| {
                        let waits = waiting.get(&thread.id).copied();
                        let row = SelectorRow::new(
                            thread.title.clone(),
                            app.session_detail(thread, waits),
                            thread.id == app.thread.id,
                        );
                        match waits.map(waiting_sign) {
                            Some(sign) => row.with_alert(sign),
                            None => row,
                        }
                    })
                    .collect(),
            )
        }
        Overlay::Models => (
            "Select model",
            app.filtered_models()
                .into_iter()
                .map(|model| {
                    let current = model == format!("{}/{}", app.thread.provider, app.thread.model);
                    let status = if current {
                        app.language.text("active")
                    } else {
                        ""
                    };
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
        } => {
            let options = app.filtered_options(entries);
            let mut rows: Vec<SelectorRow> = options
                .iter()
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
                        crate::state::OptionsKind::CloudRouter
                        | crate::state::OptionsKind::Billing
                        | crate::state::OptionsKind::InputQueue
                        | crate::state::OptionsKind::InputQueueActions
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
                    let mut row =
                        SelectorRow::new(entry.label.clone(), entry.description.clone(), current);
                    if kind == crate::state::OptionsKind::ConnectProvider {
                        row.checked = app.connect_row_connected(&entry.value);
                    }
                    row
                })
                .collect();
            if kind == crate::state::OptionsKind::ConnectProvider {
                // Shelves as in Desktop's picker: popular first, then the
                // rest, then what is not a provider. Search keeps them, so a
                // filtered list still says which shelf a match is on.
                let mut previous = None;
                for (row, entry) in rows.iter_mut().zip(&options) {
                    let group = app.connect_row_group(&entry.value);
                    if group.is_some() && group != previous {
                        row.heading = group.map(|group| app.language.text(group).to_owned());
                        row.gap = previous.is_some();
                    }
                    previous = group;
                }
            }
            (title, rows)
        }
        Overlay::None
        | Overlay::Panel { .. }
        | Overlay::Diff(_)
        | Overlay::Workflow { .. }
        | Overlay::ProviderLogin
        | Overlay::RemoteControl
        | Overlay::Chrome
        | Overlay::SideChat => {
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
    // A refused key is explained under the field it came from, not in the
    // status bar at the far edge of the screen.
    let field_error = match (&app.overlay, &app.connect.key_error) {
        (
            Overlay::Options {
                kind: crate::state::OptionsKind::ConnectApiKey,
                ..
            },
            Some(error),
        ) => wrap_panel_text(
            app.language.text(error),
            card.content_width(),
            "",
            theme.danger,
        ),
        _ => Vec::new(),
    };
    let body_budget = max_height
        .saturating_sub(if filterable { 7 } else { 6 })
        .saturating_sub(field_error.len())
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
    for row in field_error {
        out.push(card.row(row, false));
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
            for line in row.heading_lines(theme, &layout, index == start) {
                out.push(card.row(line, false));
            }
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

/// A waiting chat's sign: the approval card's ⚠ for an approval, `?` for a
/// question.
const fn waiting_sign(waiting: crate::state::Waiting) -> &'static str {
    match waiting {
        crate::state::Waiting::Approval => "⚠",
        crate::state::Waiting::Answer => "?",
    }
}

/// One choice in a picker: what it is, what it means, whether it is the
/// current one, and — for the theme picker — the palette it stands for.
struct SelectorRow {
    label: String,
    detail: String,
    current: bool,
    /// Already set up (a connected provider): a check in the gutter, so the
    /// names stay in one column.
    checked: bool,
    /// Waits for the reader (a chat with an approval or a question): this
    /// sign in the gutter, in the accent, before the current-choice dot.
    alert: Option<&'static str>,
    swatch: Option<[haarness_opentui::Color; 4]>,
    /// The shelf this row opens, and whether a blank row separates it from
    /// the shelf before.
    heading: Option<String>,
    gap: bool,
}

impl SelectorRow {
    fn new(label: impl Into<String>, detail: impl Into<String>, current: bool) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            current,
            checked: false,
            alert: None,
            swatch: None,
            heading: None,
            gap: false,
        }
    }

    const fn with_alert(mut self, sign: &'static str) -> Self {
        self.alert = Some(sign);
        self
    }

    const fn with_swatch(mut self, swatch: [haarness_opentui::Color; 4]) -> Self {
        self.swatch = Some(swatch);
        self
    }

    fn height(&self, stacked: bool) -> usize {
        usize::from(stacked && !self.detail.is_empty())
            + 1
            + usize::from(self.heading.is_some())
            + usize::from(self.heading.is_some() && self.gap)
    }

    /// The shelf label above a row that opens one, aligned with the names.
    /// At the top of the visible list the separator already sets the shelf
    /// apart, so the blank row before it is left out.
    fn heading_lines(
        &self,
        theme: &crate::theme::Theme,
        layout: &SelectorLayout,
        first_visible: bool,
    ) -> Vec<Line> {
        let Some(heading) = &self.heading else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        if self.gap && !first_visible {
            lines.push(Line::empty());
        }
        lines.push(Line::new(vec![
            Span::new(" ".repeat(layout.gutter()), theme.muted),
            Span::new(heading.clone(), theme.accent).with_attributes(attr::BOLD),
        ]));
        lines
    }

    fn lines(&self, theme: &crate::theme::Theme, layout: &SelectorLayout) -> Vec<Line> {
        let (marker, marker_color) = if let Some(sign) = self.alert {
            (sign, theme.accent)
        } else if self.current {
            ("●", theme.primary)
        } else if self.checked {
            ("✓", theme.success())
        } else {
            (" ", theme.muted)
        };
        let mut marker = Span::new(marker, marker_color);
        if self.alert.is_some() {
            marker = marker.with_attributes(attr::BOLD);
        }
        let mut primary = vec![marker, Span::new("  ", theme.muted)];
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

/// The card width that shows every command and its description on one
/// aligned row, as [`SelectorLayout`] lays them out.
pub(crate) fn commands_width(app: &App) -> usize {
    let entries = app.filtered_slash_entries("");
    let label = entries
        .iter()
        .map(|entry| entry.name.width() + 1)
        .max()
        .unwrap_or_default();
    let detail = entries
        .iter()
        .map(|entry| entry.description.width())
        .max()
        .unwrap_or_default();
    // Card padding, the marker gutter, the label, its gap, the description.
    4 + 3 + label + 2 + detail
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
        let mut spans = vec![Span::new(
            format!("{}  ", app.language.text("Key")),
            theme.muted,
        )];
        if app.overlay_filter.is_empty() {
            spans.push(Span::new(app.language.text("paste it here"), theme.muted));
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
                Span::new(format!("{}  ", app.language.text(label)), theme.muted),
                Span::new(app.language.text(placeholder), theme.muted),
                Span::new("▌", theme.primary),
            ]);
        }
        return Line::new(vec![
            Span::new(app.language.text("Type to filter"), theme.muted),
            Span::new("▌", theme.primary),
        ]);
    }
    if let Some((label, _)) = prompt {
        return Line::new(vec![
            Span::new(format!("{}  ", app.language.text(label)), theme.muted),
            Span::new(app.overlay_filter.clone(), theme.text),
            Span::new("▌", theme.primary),
        ]);
    }
    Line::new(vec![
        Span::new(format!("{}  ", app.language.text("Search")), theme.muted),
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
            app.language
                .hints(&["Enter to apply", "Ctrl+U to clear", "Escape to cancel"]),
            theme.muted,
        );
    }
    if app.overlay_filter.is_empty() {
        return line(app.language.text("Nothing to choose from"), theme.muted);
    }
    // The query is the user's own text; only the words around it translate.
    let template = app.language.text("No match for {query}");
    let (before, after) = template.split_once("{query}").unwrap_or((template, ""));
    Line::new(vec![
        Span::new(before.to_owned(), theme.muted),
        Span::new(
            format!("\u{201c}{}\u{201d}", app.overlay_filter),
            theme.text,
        ),
        Span::new(after.to_owned(), theme.muted),
        Span::new(
            format!(
                "  \u{00b7}  {}",
                app.language.hints(&["backspace to widen"])
            ),
            theme.muted,
        ),
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
    use crate::state::OptionsKind;
    let language = app.language;
    let range = |keys: &[&str]| language.range_hints(start + 1, end, total, keys);
    let kind = match &app.overlay {
        Overlay::Threads => return range(&["enter resume", "\u{2192} actions"]),
        Overlay::Models => return range(&["ctrl+f favorite", "enter select"]),
        Overlay::Options { kind, .. } => *kind,
        _ => return range(&["\u{2191}\u{2193} navigate", "enter select"]),
    };
    match kind {
        OptionsKind::Workspaces => range(&["enter actions", "type to filter"]),
        OptionsKind::WorkspaceCreateName => language.hints(&["enter continue", "esc cancel"]),
        OptionsKind::WorkspaceCreateBase => range(&["enter choose", "esc back", "type to filter"]),
        OptionsKind::WorkspaceCreateCustomBase => language.hints(&["enter check", "esc back"]),
        OptionsKind::WorkspaceActions
        | OptionsKind::WorkspaceResetConfirmation
        | OptionsKind::WorkspaceRemoveConfirmation => range(&["enter select", "esc back"]),
        OptionsKind::WorkflowDashboard => range(&["enter run / actions", "\u{2192} inspect"]),
        OptionsKind::ConnectApiKey => {
            language.hints(&["enter connect", "ctrl+u clear", "esc back"])
        }
        OptionsKind::ConnectMethod | OptionsKind::SessionActions => {
            range(&["enter choose", "esc back"])
        }
        OptionsKind::ConnectProvider => range(&["enter connect", "type to filter"]),
        _ => range(&["\u{2191}\u{2193} navigate", "enter select"]),
    }
}

/// The live run inspector. Unlike a `Panel`, whose text is frozen when it
/// opens, this rebuilds from the cached run snapshot every frame — so a
/// running workflow's phase, agents, and log tail move while you watch.
/// Footer builders differ per target: a run scrolls its report, a definition
/// advertises that Enter launches it.
type Footer = fn(crate::locale::Language, usize, usize) -> String;

fn workflow_dialog(
    app: &App,
    target: &crate::state::WorkflowTarget,
    width: usize,
    max_height: usize,
) -> TaggedLines {
    let card = ModalCard::new(app, width);
    let content = card.content_width();
    let language = app.language;
    let (title, body, footer): (String, Vec<Line>, Footer) = match target {
        crate::state::WorkflowTarget::Run(id) => match app
            .workflow_runs
            .iter()
            .find(|candidate| candidate.id == *id)
        {
            Some(run) => (
                language.format("Workflow · {name}", &[("name", &run.workflow_name)]),
                crate::view::workflow::run_body(app, run, content),
                crate::view::workflow::run_footer,
            ),
            None => (
                language.text("Workflow run").to_owned(),
                vec![line(
                    language.text("This run is no longer cached."),
                    app.theme.muted,
                )],
                crate::view::workflow::run_footer,
            ),
        },
        crate::state::WorkflowTarget::Invalid(index) => match app.workflow_invalid.get(*index) {
            Some(invalid) => (
                language.text("Invalid workflow").to_owned(),
                crate::view::workflow::invalid_body(app, invalid, content),
                crate::view::workflow::invalid_footer,
            ),
            None => (
                language.text("Invalid workflow").to_owned(),
                vec![line(
                    language.text("This file is no longer listed."),
                    app.theme.muted,
                )],
                crate::view::workflow::invalid_footer,
            ),
        },
        crate::state::WorkflowTarget::Definition(invocation) => match app
            .workflow_definitions
            .iter()
            .find(|candidate| candidate.invocation == *invocation)
        {
            Some(definition) => (
                language.format(
                    "Workflow · {name}",
                    &[("name", &format!("/{}", definition.invocation))],
                ),
                crate::view::workflow::definition_body(app, definition, content),
                crate::view::workflow::definition_footer,
            ),
            None => (
                language.text("Workflow").to_owned(),
                vec![line(
                    language.text("This definition is no longer listed."),
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
    out.push(card.footer(&footer(language, end.saturating_sub(start), total)));
    out.push(card.bottom());
    out
}

/// Rows printed between a picker's header and its choices: a management
/// view's first lines, or the hints above the API-key prompt. Each source
/// line gets one row when it fits and a wrapped continuation when it does
/// not, within a cap so the choices stay on screen.
/// The `/billing` summary: who, which plan, and what is left to spend. Rows
/// are `label  value` pairs so they line up like the rest of the panels.
fn billing_preface(
    billing: &haarness_account::billing::State,
    language: crate::locale::Language,
) -> Vec<String> {
    use haarness_account::billing::display_credits;
    if !billing.logged_in {
        return vec!["Not signed in to Haarness Cloud.".into()];
    }
    let mut lines = Vec::new();
    if let Some(error) = &billing.error {
        lines.push(format!("\u{26a0}  Billing unavailable  {error}"));
    }
    let Some(summary) = &billing.summary else {
        if billing.error.is_none() {
            lines.push("Loading your plan and credits…".into());
        }
        return lines;
    };
    let day = |value: &str| {
        chrono::DateTime::parse_from_rfc3339(value)
            .map_or_else(|_| value.to_owned(), |at| at.format("%b %-d").to_string())
    };
    let plan_code = summary.plan.trim();
    let mut plan = if plan_code.is_empty() || plan_code.eq_ignore_ascii_case("no plan") {
        // Cloud's own wording for an account that has not subscribed.
        "No plan yet".to_owned()
    } else if summary.trial {
        "Free trial".to_owned()
    } else {
        let mut letters = plan_code.chars();
        letters.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(letters).collect::<String>() + " plan"
        })
    };
    if let Some(status) = summary
        .subscription_status
        .as_deref()
        .filter(|status| !matches!(*status, "active" | "trialing"))
    {
        plan = format!("{plan} · {}", status.replace('_', " "));
    }
    if let Some(seats) = summary.seats {
        plan = format!("{plan} · {seats} seats");
    }
    if summary.billing_interval.as_deref() == Some("year") {
        plan = format!("{plan} · yearly");
    }
    if let Some(at) = &summary.period_ends_at {
        let verb = if summary.cancel_at_period_end {
            "ends"
        } else {
            "renews"
        };
        plan = format!("{plan} · {verb} {}", day(at));
    }
    lines.push(format!("Account  {}", summary.account_name));
    lines.push(format!("Plan  {plan}"));
    let mut included = format!("{} credits", display_credits(&summary.included_credits));
    if let Some(at) = &summary.resets_at {
        included = format!("{included} · resets {}", day(at));
    }
    lines.push(format!("Included AI  {included}"));
    lines.push(format!(
        "Purchased  {} credits",
        display_credits(&summary.purchased_credits)
    ));
    let held = display_credits(&summary.held_credits);
    if held != "0" {
        lines.push(format!("Reserved  {held} credits in use"));
    }
    lines.push(format!("Cloud compute  {} units", summary.cloud_units));
    if summary.out_of_credits {
        lines.push("\u{26a0}  Out of credits  add credits with /billing topup".into());
    } else if let Some(days) = summary.days_left.filter(|days| *days < 30) {
        // Whole days at this week's pace: none left is under a day, not 0.
        let left = if days == 0 {
            language
                .text("less than a day left at this pace")
                .to_owned()
        } else {
            language.count(
                days,
                "about 1 day left at this pace",
                "about {count} days left at this pace",
            )
        };
        lines.push(format!("Forecast  {left}"));
    }
    if summary.overage_opt_in {
        lines.push("Overage  on · purchased credits pay past your allowance".into());
    } else if display_credits(&summary.purchased_credits) != "0" {
        lines.push("Overage  off · purchased credits are not spent".into());
    }
    if let Some(reason) = &summary.auto_topup_failure {
        lines.push(format!(
            "Auto top-up  off · the last charge failed: {reason}"
        ));
    } else if summary.auto_topup {
        lines.push("Auto top-up  on · your saved card buys credits when you run low".into());
    }
    if summary.trial_available {
        lines.push("Free trial  available · start it with /billing open".into());
    }
    lines
}

/// The welcome line above the first-run provider picker: why the screen is
/// there, short enough to leave the list most of an 80x24 terminal.
const WELCOME: &str =
    "Haarness needs an AI model. Sign in with a subscription you have, or paste an API key.";

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
        // The first screen says why it is there, as Desktop's welcome does.
        Overlay::Options {
            kind: crate::state::OptionsKind::ConnectProvider,
            ..
        } if !app.any_provider_connected() => vec![app.language.text(WELCOME).to_owned()],
        Overlay::Options {
            kind: crate::state::OptionsKind::Billing,
            ..
        } => billing_preface(&app.billing, app.language),
        Overlay::Options {
            kind: crate::state::OptionsKind::GoalActions,
            ..
        } => crate::commands::goal_preface(app.goal.as_ref()),
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
        // details page when there is more. The welcome sentence is the one
        // line that is read whole, so it may take a narrow card's third row.
        item_rows.truncate(
            if item.plain.as_deref() == Some(app.language.text(WELCOME)) {
                3
            } else {
                2
            },
        );
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
    let language = app.language;
    let card = ModalCard::new(app, width);
    let content = card.content_width();
    let Some(login) = app.connect.login.as_ref() else {
        let mut out = TaggedLines::default();
        out.push(card.top());
        out.push(card.header(language.text("Sign in")));
        out.push(card.separator());
        out.push(card.row(
            line(language.text("No sign-in is running"), theme.muted),
            false,
        ));
        out.push(card.bottom());
        return out;
    };
    let name = [("name", login.name.as_str())];
    let title = match login.state {
        crate::connect::LoginState::Pending => language.format("Sign in to {name}", &name),
        crate::connect::LoginState::Connected => language.format("Connected {name}", &name),
        crate::connect::LoginState::Failed(_) => language.format("Could not connect {name}", &name),
    };
    let mut body: Vec<Line> = Vec::new();
    let step = |number: &str, text: &str| {
        Line::new(vec![
            Span::new(format!("{number}  "), theme.muted),
            Span::new(language.text(text).to_owned(), theme.text),
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
            // A server that stops answering the checks for a while leaves
            // the sign-in waiting; the same row says so, quietly.
            let waiting = if login.unreachable() {
                "Can't reach the server · still waiting"
            } else {
                "Waiting for you to finish in the browser…"
            };
            body.push(Line::new(vec![
                Span::new(format!("{frame} "), theme.primary),
                Span::new(
                    format!(
                        "{} {}",
                        language.text(waiting),
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
            body.push(line(
                language.format("{name} is ready to use.", &name),
                theme.text,
            ));
        }
        crate::connect::LoginState::Failed(error) => {
            // Haarness's own reasons are in the catalog; a server's or a
            // provider's are shown as they came.
            body.extend(wrap_panel_text(
                language.text(error),
                content,
                "",
                theme.danger,
            ));
            body.push(Line::empty());
            body.push(line(
                language.text("Enter to try again, or Esc to go back."),
                theme.muted,
            ));
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
    let keys: &[&str] = match login.state {
        crate::connect::LoginState::Pending if login.code.is_empty() => {
            &["c copy link", "o open browser", "esc cancel"]
        }
        crate::connect::LoginState::Pending => &["c copy code", "o open browser", "esc cancel"],
        crate::connect::LoginState::Connected => &["enter continue"],
        crate::connect::LoginState::Failed(_) => &["enter retry", "esc back"],
    };
    out.push(card.footer(&language.hints(keys)));
    out.push(card.bottom());
    out
}

/// `/remote-control`: the shared chat's QR code beside what it means, then
/// its link. The QR code is drawn in true black on white whatever the theme,
/// because phone cameras read dark-on-light codes most reliably.
fn remote_control_dialog(app: &App, width: usize, max_height: usize) -> TaggedLines {
    use crate::remote_control::Action;
    let theme = &app.theme;
    let card = ModalCard::new(app, width);
    let content = card.content_width();
    let remote = &app.remote;
    let current = remote.thread == Some(app.thread.id);
    let share = remote.share.as_ref().filter(|_| current);
    let busy = remote.busy().filter(|_| current);
    let state = share.map_or("", |share| share.state.as_str());
    let spin = crate::view::span::spinner(app.tick).to_string();
    let language = app.language;
    let heading = |glyph: String, color: haarness_opentui::Color, label: &str| {
        Line::new(vec![
            Span::new(format!("{glyph} "), color),
            Span::new(language.text(label).to_owned(), theme.text).with_attributes(attr::BOLD),
        ])
    };
    let note = |text: &str| language.text(text).to_owned();
    let mut status = Vec::new();
    let mut notes: Vec<(String, haarness_opentui::Color)> = Vec::new();
    let mut footer: &[&str] = &["esc close"];
    let live = matches!(state, "on" | "connecting");
    let explain = note("This chat keeps running here, with this computer's files and model keys.");
    match (busy, state) {
        (Some(Action::Stop), _) => status.push(heading(spin, theme.primary, "Stopping…")),
        (Some(Action::Start), _) if !live => {
            status.push(heading(spin, theme.primary, "Sharing this chat…"));
            notes.push((note("Connecting it to Haarness Cloud."), theme.muted));
        }
        _ if remote.failure.is_some() && current => {
            status.push(heading(
                "✕".into(),
                theme.danger,
                "Could not update sharing",
            ));
            notes.push((remote.failure.clone().unwrap_or_default(), theme.danger));
            footer = &["enter try again", "esc close"];
        }
        (_, "on" | "connecting") => {
            status.push(if state == "on" {
                heading("●".into(), theme.success(), "Live in Haarness Cloud")
            } else {
                heading(spin, theme.primary, "Connecting to Haarness Cloud…")
            });
            if let Some(chat) = share.and_then(|share| share.chat.as_deref()) {
                status.push(line(chat, theme.text));
            }
            if let Some(computer) = share.and_then(|share| share.computer.as_deref()) {
                status.push(line(
                    language.format("on {computer}", &[("computer", computer)]),
                    theme.muted,
                ));
            }
            notes.push((
                note("Scan the code with your phone, or open the link in any browser signed in to Haarness Cloud."),
                theme.text,
            ));
            notes.push((explain.clone(), theme.muted));
            footer = &["o open", "c copy link", "s stop sharing", "esc close"];
        }
        (_, "off") => {
            status.push(heading("○".into(), theme.muted, "Not shared"));
            notes.push((
                note("Continue this chat from your phone or any browser."),
                theme.text,
            ));
            notes.push((explain.clone(), theme.muted));
            footer = &["enter share this chat", "esc close"];
        }
        (_, "stopped") => {
            status.push(heading(
                "○".into(),
                theme.muted,
                "Sharing stopped from Haarness Cloud",
            ));
            notes.push((note("Share again for a new link."), theme.text));
            footer = &["enter share again", "esc close"];
        }
        (_, "signed_out") => {
            status.push(heading(
                "○".into(),
                theme.muted,
                "Sign in to Haarness Cloud",
            ));
            notes.push((
                note("Sign in to continue this chat from your phone or any browser."),
                theme.text,
            ));
            notes.push((explain, theme.muted));
            footer = &["enter sign in", "esc close"];
        }
        _ => status.push(heading(spin, theme.primary, "Checking…")),
    }
    if let Some(error) = share.and_then(|share| share.error.as_deref()) {
        notes.push((error.to_owned(), theme.danger));
    }
    let url = share
        .filter(|share| share.live())
        .and_then(|share| share.url.clone());
    let qr = share
        .filter(|_| live && url.is_some())
        .map(|share| qr_code_lines(&share.qr))
        .unwrap_or_default();
    let qr_width = qr.first().map_or(0, Line::width);
    // Header, separators, footer and the link take about eight rows.
    let room = max_height.saturating_sub(8);
    let side_by_side = !qr.is_empty() && content >= qr_width + 3 + 28;
    let show_qr = !qr.is_empty()
        && if side_by_side {
            qr.len() <= room
        } else {
            qr.len() + status.len() + 2 <= room
        };
    let text_width = if show_qr && side_by_side {
        content - qr_width - 3
    } else {
        content
    };
    let mut text = status;
    for (note, color) in notes {
        text.push(Line::empty());
        text.extend(wrap_panel_text(&note, text_width, "", color));
    }
    let mut body: Vec<Line> = Vec::new();
    if show_qr && side_by_side {
        // Centre the text block against the code.
        let offset = qr.len().saturating_sub(text.len()) / 2;
        for (index, code) in qr.into_iter().enumerate() {
            let words = index
                .checked_sub(offset)
                .and_then(|row| text.get(row).cloned())
                .unwrap_or_else(Line::empty);
            body.push(beside(app, width, code, words, text_width));
        }
    } else {
        body.extend(text.into_iter().map(|row| card.row(row, false)));
        if show_qr {
            body.push(card.row(Line::empty(), false));
            for code in qr {
                body.push(beside(app, width, code, Line::empty(), 0));
            }
        }
    }
    if let Some(url) = &url {
        body.push(card.row(Line::empty(), false));
        for row in wrap_panel_text(url, content, "", theme.primary) {
            body.push(card.row(row, false));
        }
    }
    body.truncate(max_height.saturating_sub(5).max(1));
    let mut out = TaggedLines::default();
    out.push(card.top());
    out.push(card.header(language.text("Remote control")));
    out.push(card.separator());
    for row in body {
        out.push(row);
    }
    out.push(card.separator());
    out.push(card.footer(&language.hints(footer)));
    out.push(card.bottom());
    out
}

/// `/chrome`: whether agents can use the person's own Chrome, and the one
/// next step while they cannot yet (set it up, add the extension, open
/// Chrome, resume it there), above the two settings people change here.
fn chrome_dialog(app: &App, width: usize, max_height: usize) -> TaggedLines {
    use crate::chrome::{LocalAction, Off, Unavailable, View};
    const ABOUT: &str = "Agents can use your own Chrome — with your sign-ins — through the Haarness for Chrome extension. Each new site asks first.";
    let theme = &app.theme;
    let language = app.language;
    let card = ModalCard::new(app, width);
    let content = card.content_width();
    let chrome = &app.chrome;
    let spin = crate::view::span::spinner(app.tick).to_string();
    let heading = |glyph: &str, color: haarness_opentui::Color, label: String| {
        Line::new(vec![
            Span::new(format!("{glyph} "), color),
            Span::new(label, theme.text).with_attributes(attr::BOLD),
        ])
    };
    // A paragraph: a blank row, then the text wrapped to the card.
    let paragraph = |body: &mut Vec<Line>, text: &str, color: haarness_opentui::Color| {
        body.push(Line::empty());
        body.extend(wrap_panel_text(text, content, "", color));
    };
    let mut body: Vec<Line> = Vec::new();
    let mut footer: Vec<&str> = Vec::new();
    match chrome.view() {
        View::Checking => {
            body.push(heading(
                &spin,
                theme.primary,
                language.text("Checking…").into(),
            ));
        }
        View::Unavailable(Unavailable::Remote) => {
            body.push(heading(
                "○",
                theme.muted,
                language.text("Not available in this chat").into(),
            ));
            paragraph(
                &mut body,
                language.text("Chrome works with projects on the computer where Chrome runs. Open this project on that computer to use it."),
                theme.text,
            );
        }
        View::Unavailable(Unavailable::Unsupported) => {
            body.push(heading(
                "○",
                theme.muted,
                language.text("This server cannot use Chrome").into(),
            ));
            paragraph(
                &mut body,
                language.text("Update Haarness, then restart this project's server."),
                theme.text,
            );
        }
        View::Unavailable(Unavailable::Failed(error)) => {
            body.push(heading(
                "✕",
                theme.danger,
                language.text("Could not reach the server").into(),
            ));
            paragraph(&mut body, error, theme.danger);
            footer.push("enter try again");
        }
        View::Off(Off::Switch) => {
            body.push(heading(
                "○",
                theme.muted,
                language.text("Agents may not use Chrome").into(),
            ));
            paragraph(&mut body, language.text(ABOUT), theme.text);
            footer.push("enter turn on");
        }
        // The switch below is on; what keeps Chrome off is elsewhere, so
        // Enter only looks again once the person has changed it.
        View::Off(Off::BrowserControl) => {
            body.push(heading(
                "○",
                theme.muted,
                language.text("Agent browser control is off").into(),
            ));
            paragraph(
                &mut body,
                language.text("Agents can't use any browser in this project, Chrome included. Turn on Agent browser control in Haarness Desktop (Settings → Browser), or set browser.policy.enabled to true in your Haarness config."),
                theme.text,
            );
            footer.push("enter check again");
        }
        View::Off(Off::Project) => {
            body.push(heading(
                "○",
                theme.muted,
                language
                    .text("This project's settings turn Chrome off")
                    .into(),
            ));
            paragraph(
                &mut body,
                language.text("Agents can still use Chrome in your other projects. To use it here, remove browser.chrome.enabled: false from this project's Haarness config."),
                theme.text,
            );
            footer.push("enter check again");
        }
        View::Connected(hosts) => {
            chrome_connected(app, hosts, content, &mut body);
            footer.push("enter refresh");
        }
        View::Elsewhere => {
            body.push(heading(
                "○",
                theme.muted,
                language.text("Not connected").into(),
            ));
            paragraph(
                &mut body,
                // Chrome on this terminal's computer does not reach the
                // project, which may run elsewhere or under another data
                // folder here; either way, setting up here would not help.
                language.text(if crate::connect::over_ssh() {
                    "You are connected over SSH. Chrome works with projects on the computer where Chrome runs."
                } else {
                    "This terminal can't set up Chrome for this project. Chrome works with projects on the computer where Chrome runs."
                }),
                theme.text,
            );
            paragraph(
                &mut body,
                &language.format(
                    "On that computer, run {command}, or open Settings → Browser → Chrome in Haarness Desktop.",
                    &[("command", crate::chrome::SETUP_COMMAND)],
                ),
                theme.muted,
            );
            footer.extend(["c copy command", "enter refresh"]);
        }
        View::LocalChecking => {
            body.push(heading(
                &spin,
                theme.primary,
                language.text("Checking this computer…").into(),
            ));
        }
        View::LocalFailed(action, error) => {
            body.push(heading(
                "✕",
                theme.danger,
                language
                    .text(if action == LocalAction::Setup {
                        "Could not set up Chrome"
                    } else {
                        "Could not check Chrome on this computer"
                    })
                    .into(),
            ));
            paragraph(&mut body, language.text(error), theme.danger);
            footer.extend(["enter try again", "c copy command"]);
        }
        View::SettingUp => {
            body.push(heading(
                &spin,
                theme.primary,
                language.text("Setting up Chrome…").into(),
            ));
            paragraph(
                &mut body,
                language.text("Letting Chrome start Haarness on this computer."),
                theme.muted,
            );
        }
        View::NotSetUp { removed } => {
            body.push(heading(
                "○",
                theme.muted,
                language.text("Not set up").into(),
            ));
            paragraph(&mut body, language.text(ABOUT), theme.text);
            match &chrome.local {
                // Setup registers nothing then; its warning says what to install.
                Some(setup) if chrome.nothing_to_add() => {
                    paragraph(
                        &mut body,
                        language.text(
                            "This build of Haarness has no Haarness for Chrome extension to load.",
                        ),
                        theme.danger,
                    );
                    for warning in &setup.warnings {
                        paragraph(&mut body, warning, theme.muted);
                    }
                }
                _ => paragraph(
                    &mut body,
                    language.text(if removed {
                        "Haarness was removed from Chrome on this computer. Press Enter to set it up again."
                    } else {
                        "Press Enter to let Chrome start Haarness on this computer."
                    }),
                    theme.muted,
                ),
            }
            footer.push("enter set up");
        }
        View::Waiting(setup) => {
            let reconnecting = matches!(
                &chrome.hosts,
                Some(Ok(hosts)) if hosts.chrome.ever_connected
            );
            if reconnecting {
                // It worked before: Chrome is closed, or the extension is off.
                body.push(heading(
                    "○",
                    theme.muted,
                    language.text("Not connected").into(),
                ));
                paragraph(
                    &mut body,
                    language.text("Open Chrome; Haarness connects within a few seconds."),
                    theme.text,
                );
            } else if chrome.nothing_to_add() {
                // Nothing will connect, so nothing spins.
                body.push(heading(
                    "○",
                    theme.muted,
                    language.text("Not set up").into(),
                ));
                chrome_steps(app, setup, content, &mut body);
            } else {
                body.push(heading(
                    &spin,
                    theme.primary,
                    language.text("Waiting for Chrome…").into(),
                ));
                chrome_steps(app, setup, content, &mut body);
            }
            for browser in setup.unsupported() {
                let note = browser
                    .note
                    .as_deref()
                    .unwrap_or_else(|| language.text("not supported"));
                paragraph(
                    &mut body,
                    &format!("{} · {note}", browser.name),
                    theme.muted,
                );
            }
            // With nothing to add, the steps above already said so in the
            // person's language; the CLI's English note would repeat it.
            for warning in setup.warnings.iter().filter(|warning| {
                !(chrome.nothing_to_add() && crate::chrome::not_included(warning))
            }) {
                paragraph(&mut body, warning, theme.muted);
            }
            if chrome.web_store_url().is_some() {
                footer.push("o open web store");
            } else if chrome.extension_path().is_some() {
                footer.push("c copy path");
            }
            footer.push("enter check again");
        }
    }
    if matches!(chrome.hosts, Some(Ok(_))) {
        chrome_settings_rows(app, content, &mut body);
    }
    // A short terminal scrolls the dialog rather than lose its last rows,
    // which hold the settings its keys change.
    let room = max_height.saturating_sub(6).max(1);
    let max_scroll = body.len().saturating_sub(room);
    app.max_overlay_scroll.set(max_scroll);
    if max_scroll > 0 {
        let start = app.overlay_scroll.min(max_scroll);
        body.drain(..start);
        body.truncate(room);
        footer.push("pgup/pgdn scroll");
    }
    footer.push("esc close");
    let mut out = TaggedLines::default();
    out.push(card.top());
    out.push(card.header(language.text("Chrome")));
    out.push(card.separator());
    for row in body {
        out.push(card.row(row, false));
    }
    out.push(card.separator());
    out.push(card.footer(&language.hints(&footer)));
    out.push(card.bottom());
    out
}

/// Which browsers are connected, whether the person paused or stopped
/// Haarness there, and what this chat has open.
fn chrome_connected(
    app: &App,
    hosts: &[crate::chrome::Host],
    content: usize,
    body: &mut Vec<Line>,
) {
    let theme = &app.theme;
    let language = app.language;
    let heading = |glyph: &str, color: haarness_opentui::Color, label: String| {
        Line::new(vec![
            Span::new(format!("{glyph} "), color),
            Span::new(label, theme.text).with_attributes(attr::BOLD),
        ])
    };
    let stopped = hosts.iter().all(|host| host.stopped);
    let paused = !stopped && hosts.iter().all(|host| host.paused || host.stopped);
    if stopped {
        body.push(heading(
            "✕",
            theme.danger,
            language.text("Stopped in Chrome").into(),
        ));
    } else if paused {
        body.push(heading(
            "◐",
            theme.accent,
            language.text("Paused in Chrome").into(),
        ));
    } else if let [host] = hosts {
        body.push(heading(
            "●",
            theme.success(),
            language.format("Connected · {browser}", &[("browser", &host.title())]),
        ));
    } else {
        body.push(heading(
            "●",
            theme.success(),
            language.format(
                "{count} browsers connected",
                &[("count", &hosts.len().to_string())],
            ),
        ));
    }
    // One browser was named above; several each get a row of their own.
    for host in hosts {
        let state = if host.stopped {
            language.text("stopped")
        } else if host.paused {
            language.text("paused")
        } else {
            ""
        };
        let version = host.extension_version.as_deref().map(|version| {
            language.format("Haarness for Chrome {version}", &[("version", version)])
        });
        let text = if hosts.len() == 1 {
            version.unwrap_or_default()
        } else {
            [Some(host.title()), version, Some(state.to_owned())]
                .into_iter()
                .flatten()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ")
        };
        if !text.is_empty() {
            body.extend(wrap_panel_text(&text, content, "  ", theme.muted));
        }
    }
    body.push(Line::empty());
    if stopped {
        body.extend(wrap_panel_text(
            language.text("You stopped Haarness in Chrome. Click Resume on the Haarness button in Chrome's toolbar to let agents back in."),
            content,
            "",
            theme.text,
        ));
        return;
    }
    if paused {
        body.extend(wrap_panel_text(
            language.text("Resume it from the Haarness button in Chrome's toolbar."),
            content,
            "",
            theme.text,
        ));
        return;
    }
    let tabs = app.chrome.thread_tabs(app.thread.id);
    let text = match tabs {
        0 => language
            .text("Try it: ask an agent to open a website in Chrome.")
            .to_owned(),
        1 => language.text("This chat has 1 tab in Chrome.").to_owned(),
        count => language.format(
            "This chat has {count} tabs in Chrome.",
            &[("count", &count.to_string())],
        ),
    };
    body.extend(wrap_panel_text(&text, content, "", theme.text));
    body.extend(wrap_panel_text(
        language
            .text("Agents work only in their own tab groups, and each new site asks you first."),
        content,
        "",
        theme.muted,
    ));
}

/// How to add Haarness for Chrome once Chrome can start Haarness: from the
/// Web Store when it is listed there, otherwise by loading the unpacked
/// folder in every browser setup registered, with the path to paste.
fn chrome_steps(app: &App, setup: &crate::chrome::Setup, content: usize, body: &mut Vec<Line>) {
    let theme = &app.theme;
    let language = app.language;
    let chrome = &app.chrome;
    let step = |body: &mut Vec<Line>, number: usize, text: &str| {
        let rows = textwrap::wrap(text, content.saturating_sub(3).max(10));
        for (index, row) in rows.into_iter().enumerate() {
            let lead = if index == 0 {
                format!("{number}  ")
            } else {
                "   ".to_owned()
            };
            body.push(Line::new(vec![
                Span::new(lead, theme.muted),
                Span::new(row.into_owned(), theme.text),
            ]));
        }
    };
    let detail = |body: &mut Vec<Line>, text: &str, color: haarness_opentui::Color| {
        body.extend(wrap_panel_text(text, content, "   ", color));
    };
    body.push(Line::empty());
    if let Some(url) = chrome.web_store_url() {
        body.extend(wrap_panel_text(
            language.text("Chrome can start Haarness. Now add Haarness for Chrome:"),
            content,
            "",
            theme.text,
        ));
        body.push(Line::empty());
        step(
            body,
            1,
            language.text("Open the Haarness for Chrome page in the Chrome Web Store:"),
        );
        detail(body, url, theme.primary);
        step(body, 2, language.text("Click Add to Chrome"));
        step(body, 3, language.text("Pin Haarness to the toolbar"));
        return;
    }
    let Some(path) = chrome.extension_path() else {
        body.extend(wrap_panel_text(
            language.text("This Haarness installation doesn't include Haarness for Chrome. Set it up from Haarness Desktop (Settings → Browser → Chrome), then press Enter here."),
            content,
            "",
            theme.danger,
        ));
        return;
    };
    body.extend(wrap_panel_text(
        language.text("Chrome can start Haarness. Now add Haarness for Chrome:"),
        content,
        "",
        theme.text,
    ));
    body.push(Line::empty());
    let page = |browser: &crate::chrome::SetupBrowser| {
        browser
            .extensions_page
            .clone()
            .unwrap_or_else(|| "chrome://extensions".into())
    };
    let mut browsers = setup.ready();
    if let Some(first) = browsers.next() {
        step(
            body,
            1,
            &language.format(
                "Open {page} in {browser}",
                &[("page", &page(first)), ("browser", &first.name)],
            ),
        );
    }
    for other in browsers {
        detail(
            body,
            &language.format(
                "or {page} in {browser}",
                &[("page", &page(other)), ("browser", &other.name)],
            ),
            theme.muted,
        );
    }
    step(
        body,
        2,
        language.text("Turn on Developer mode (keep it on)"),
    );
    // macOS's file picker takes a typed path after ⌘⇧G; GTK's and
    // Windows' after Ctrl+L.
    let keys = if cfg!(target_os = "macos") {
        "⌘⇧G"
    } else {
        "Ctrl+L"
    };
    step(
        body,
        3,
        &language.format(
            "Click Load unpacked, press {keys} and paste this path:",
            &[("keys", keys)],
        ),
    );
    detail(body, path, theme.primary);
    step(body, 4, language.text("Pin Haarness to the toolbar"));
    body.push(Line::empty());
    body.extend(wrap_panel_text(
        language.text("Already added? Open Chrome; Haarness connects within a few seconds."),
        content,
        "",
        theme.muted,
    ));
}

/// The two Chrome settings this dialog changes, with their keys. They apply
/// to every project on this computer, as Chrome itself does.
fn chrome_settings_rows(app: &App, content: usize, body: &mut Vec<Line>) {
    use crate::chrome::Change;
    let theme = &app.theme;
    let language = app.language;
    let chrome = &app.chrome;
    let Some((enabled, default_browser)) = chrome.access() else {
        return;
    };
    let saving = chrome.saving();
    let rows = [
        (
            language.text("Let agents use Chrome"),
            language.text(if enabled { "On" } else { "Off" }),
            "e",
            matches!(saving, Some(Change::Enabled(_))),
        ),
        (
            language.text("Browser for new agent tabs"),
            language.text(crate::chrome::default_browser_label(default_browser)),
            "d",
            matches!(saving, Some(Change::DefaultBrowser(_))),
        ),
    ];
    let label_width = rows
        .iter()
        .map(|(label, ..)| label.width())
        .max()
        .unwrap_or_default()
        .min(content / 2);
    // Every value either row can show, so the keys stay in one column as the
    // values change under them.
    let value_width = ["On", "Off"]
        .into_iter()
        .chain(
            crate::chrome::DEFAULT_BROWSERS
                .iter()
                .map(|browser| crate::chrome::default_browser_label(browser)),
        )
        .map(|value| language.text(value).width())
        .max()
        .unwrap_or_default()
        .min(content.saturating_sub(label_width + 6));
    body.push(Line::empty());
    for (label, value, key, busy) in rows {
        let label = single_line(label, label_width);
        let value = single_line(value, value_width);
        let mut spans = vec![
            Span::new(
                format!("{label}{}", " ".repeat(label_width - label.width() + 3)),
                theme.text,
            ),
            Span::new(value.clone(), theme.text).with_attributes(attr::BOLD),
            Span::new(
                " ".repeat(value_width.saturating_sub(value.width()) + 3),
                theme.muted,
            ),
        ];
        if busy {
            spans.push(Span::new(
                format!(
                    "{} {}",
                    crate::view::span::spinner(app.tick),
                    language.text("Saving…")
                ),
                theme.primary,
            ));
        } else {
            spans.push(Span::new(key, theme.muted));
        }
        body.push(Line::new(spans));
    }
    if default_browser == "auto" || !crate::chrome::DEFAULT_BROWSERS.contains(&default_browser) {
        body.extend(wrap_panel_text(
            language.text("Haarness browser when this chat is open in Desktop, otherwise Chrome"),
            content,
            &" ".repeat((label_width + 3).min(content / 2)),
            theme.muted,
        ));
    }
    if let Some(settings) = chrome
        .settings
        .as_ref()
        .filter(|settings| !settings.restrictions.is_empty())
    {
        body.push(Line::empty());
        body.extend(wrap_panel_text(
            language.text("Restricted by this project's settings:"),
            content,
            "",
            theme.muted,
        ));
        for restriction in &settings.restrictions {
            body.extend(wrap_panel_text(
                &format!("· {}", chrome_restriction(restriction, language)),
                content,
                "  ",
                theme.muted,
            ));
        }
    }
    if let Some(error) = &chrome.settings_error {
        body.push(Line::empty());
        body.extend(wrap_panel_text(
            language.text(error),
            content,
            "",
            theme.danger,
        ));
    }
    body.extend(wrap_panel_text(
        language.text("Chrome settings apply to every project on this computer."),
        content,
        "",
        theme.muted,
    ));
}

/// One way this project tightens Chrome, in the person's language. A
/// category or decision this client has no words for is shown as sent.
fn chrome_restriction(
    restriction: &crate::chrome::Restriction,
    language: crate::locale::Language,
) -> String {
    use crate::chrome::{Restriction, category_label, decision_label};
    let category = |value: &str| {
        category_label(value).map_or_else(|| value.to_owned(), |key| language.text(key).to_owned())
    };
    let decision = |value: &str| {
        decision_label(value).map_or_else(|| value.to_owned(), |key| language.text(key).to_owned())
    };
    match restriction {
        Restriction::Off => language
            .text("Chrome is turned off for this project.")
            .to_owned(),
        Restriction::Default {
            category: kind,
            decision: verdict,
        } => language.format(
            "{category}: {decision} by default",
            &[
                ("category", &category(kind)),
                ("decision", &decision(verdict)),
            ],
        ),
        Restriction::Site {
            origin,
            category: kind,
            decision: verdict,
        } => language.format(
            "{category} on {origin}: {decision}",
            &[
                ("category", &category(kind)),
                ("origin", origin),
                ("decision", &decision(verdict)),
            ],
        ),
        Restriction::Text(text) => text.clone(),
    }
}

/// A QR code in half blocks: each cell holds two modules, the upper one in
/// the foreground and the lower one in the background, with the two-module
/// quiet zone scanners need.
fn qr_code_lines(rows: &[String]) -> Vec<Line> {
    const DARK: haarness_opentui::Color = haarness_opentui::Color::rgb(0, 0, 0);
    const LIGHT: haarness_opentui::Color = haarness_opentui::Color::rgb(255, 255, 255);
    const QUIET: usize = 2;
    if rows.is_empty() {
        return Vec::new();
    }
    let total = rows.len() + QUIET * 2;
    let dark = |row: usize, column: usize| {
        row.checked_sub(QUIET)
            .zip(column.checked_sub(QUIET))
            .and_then(|(row, column)| rows.get(row)?.as_bytes().get(column))
            == Some(&b'1')
    };
    (0..total)
        .step_by(2)
        .map(|row| {
            let mut spans: Vec<Span> = Vec::new();
            for column in 0..total {
                let fg = if dark(row, column) { DARK } else { LIGHT };
                let bg = if dark(row + 1, column) { DARK } else { LIGHT };
                match spans.last_mut() {
                    Some(previous) if previous.fg == fg && previous.bg == Some(bg) => {
                        previous.text.push('▀');
                    }
                    _ => spans.push(Span::new("▀", fg).on(bg)),
                }
            }
            Line::new(spans)
        })
        .collect()
}

/// One card row holding a QR code line and, beside it, a line of text. The
/// code keeps its own colours, which `ModalCard::row` would paint over.
fn beside(app: &App, width: usize, code: Line, words: Line, text_width: usize) -> Line {
    let theme = &app.theme;
    let padding = 2.min(width / 2);
    let code_width = code.width();
    let mut spans = vec![Span::new(" ".repeat(padding), theme.text).on(theme.overlay)];
    spans.extend(code.spans);
    let mut used = padding + code_width;
    if text_width > 0 {
        spans.push(Span::new("   ", theme.text).on(theme.overlay));
        spans.extend(fit_line(words, text_width, theme.text, theme.overlay).spans);
        used += 3 + text_width;
    }
    if used < width {
        spans.push(Span::new(" ".repeat(width - used), theme.text).on(theme.overlay));
    }
    Line::new(spans)
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
        body.push((
            line(
                app.language.text("No information available"),
                app.theme.muted,
            ),
            None,
        ));
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
    let rows = app.language.format(
        "Rows {from}–{to} of {total}",
        &[
            ("from", &(start + 1).to_string()),
            ("to", &end.to_string()),
            ("total", &total.to_string()),
        ],
    );
    out.push(card.footer(&format!(
        "{}  \u{00b7}  {}",
        rows,
        app.language.hints(&["pgup/pgdn scroll"])
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
        line(
            app.language.text("Widen the terminal to review"),
            app.theme.accent,
        ),
        Line::empty(),
    ];
    if viewer.files.is_empty() {
        body.push(line(app.language.text("No changes"), app.theme.muted));
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
    let count = viewer.files.len().to_string();
    let files = if viewer.files.len() == 1 {
        app.language.format("{count} file", &[("count", &count)])
    } else {
        app.language.format("{count} files", &[("count", &count)])
    };
    out.push(card.footer(&format!(
        "{}  ·  {}",
        files,
        app.language.hints(&["esc close"])
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
            self.app.language.hints(&[escape_hint(self.app)])
        } else {
            String::new()
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

/// What Escape does in the open dialog, in the words its footer uses. A step
/// inside a flow goes back to the step before it; anything else closes.
fn escape_hint(app: &App) -> &'static str {
    use crate::state::OptionsKind;
    match &app.overlay {
        Overlay::Options {
            kind:
                OptionsKind::ConnectMethod
                | OptionsKind::ConnectApiKey
                | OptionsKind::GoalClearConfirmation
                | OptionsKind::MessageActions
                | OptionsKind::WorkspaceActions
                | OptionsKind::WorkspaceResetConfirmation
                | OptionsKind::WorkspaceRemoveConfirmation
                | OptionsKind::WorkspaceCreateBase
                | OptionsKind::WorkspaceCreateCustomBase
                | OptionsKind::ManagementField
                | OptionsKind::ManagementValue
                | OptionsKind::ManagementConfirm,
            ..
        } => "esc back",
        Overlay::ProviderLogin => match app.connect.login.as_ref().map(|login| &login.state) {
            Some(crate::connect::LoginState::Pending) => "esc cancel",
            Some(crate::connect::LoginState::Failed(_)) => "esc back",
            _ => "esc close",
        },
        _ => "esc close",
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

    fn billing(summary: serde_json::Value) -> haarness_account::billing::State {
        let mut base = serde_json::json!({
            "accountId": "a", "accountName": "Acme", "plan": "team", "subscriptionStatus": "active",
            "accountStatus": "active", "includedCredits": "30000", "purchasedCredits": "0", "heldCredits": "0",
            "cloudUnits": "120.00", "cloudUnitsHeld": "0.00", "cancelAtPeriodEnd": false, "overageOptIn": false,
            "billingUrl": "https://haarness.ai/app/billing", "usageUrl": "https://haarness.ai/app/billing/usage",
        });
        for (key, value) in summary.as_object().unwrap() {
            base[key] = value.clone();
        }
        haarness_account::billing::State {
            logged_in: true,
            summary: Some(serde_json::from_value(base).unwrap()),
            error: None,
        }
    }

    /// Whole days at this week's pace: under one is "less than a day", one
    /// is a day, in the person's language.
    #[test]
    fn billing_forecast_counts_whole_days() {
        use crate::locale::Language;
        for (days, language, forecast) in [
            (0, Language::English, "less than a day left at this pace"),
            (1, Language::English, "about 1 day left at this pace"),
            (2, Language::English, "about 2 days left at this pace"),
            (1, Language::German, "bei diesem Tempo noch etwa 1 Tag"),
            (5, Language::German, "bei diesem Tempo noch etwa 5 Tage"),
            (
                1,
                Language::Russian,
                "при таком темпе осталось дней: около 1",
            ),
            (
                5,
                Language::Polish,
                "przy tym tempie pozostało dni: około 5",
            ),
        ] {
            let lines =
                billing_preface(&billing(serde_json::json!({ "daysLeft": days })), language);
            assert!(
                lines.contains(&format!("Forecast  {forecast}")),
                "{days} {language:?}: {lines:?}"
            );
        }
    }

    #[test]
    fn billing_names_trial_seats_yearly_forecast_and_auto_topups() {
        let lines = billing_preface(
            &billing(serde_json::json!({
                "seats": 3, "billingInterval": "year", "daysLeft": 6, "autoTopup": true,
            })),
            crate::locale::Language::English,
        );
        assert!(
            lines.contains(&"Plan  Team plan · 3 seats · yearly".to_owned()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"Forecast  about 6 days left at this pace".to_owned()),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.starts_with("Auto top-up  on")),
            "{lines:?}"
        );

        let lines = billing_preface(
            &billing(serde_json::json!({
                "plan": "trial", "subscriptionStatus": null, "trial": true, "outOfCredits": true,
                "autoTopupFailure": "Your card was declined.",
            })),
            crate::locale::Language::English,
        );
        assert!(lines.contains(&"Plan  Free trial".to_owned()), "{lines:?}");
        assert!(
            lines.iter().any(|line| line.contains("Out of credits")),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.starts_with("Forecast")),
            "out of credits wins: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("the last charge failed: Your card was declined.")),
            "{lines:?}"
        );

        let lines = billing_preface(
            &billing(serde_json::json!({
                "plan": "No plan", "subscriptionStatus": null, "trialAvailable": true, "daysLeft": 90,
            })),
            crate::locale::Language::English,
        );
        assert!(lines.contains(&"Plan  No plan yet".to_owned()), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("Free trial  available")),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.starts_with("Forecast")),
            "a month or more is not worth a line: {lines:?}"
        );
    }
}
