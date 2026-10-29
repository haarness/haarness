//! Frame composition: header, transcript window, menus, composer.

pub mod approval;
pub mod command_output;
pub mod composer;
pub mod diffview;
pub mod frame;
pub mod home;
pub mod menu;
pub mod side_chat;
pub mod sidebar;
pub mod span;
pub mod transcript;
pub mod workflow;

use crate::width::DisplayWidth as _;
use haarness_opentui::{Line, Span, attr};
use unicode_width::UnicodeWidthChar as _;

use crate::state::{App, Overlay, ToastVariant};
use crate::view::span::{compact_count, short_id, single_line};

/// Prose wider than this stops feeling like an application.
const MAX_CONTENT_WIDTH: usize = 100;
/// The sidebar's columns, separator included.
const SIDEBAR_WIDTH: usize = 42;
/// The narrowest terminal that docks the sidebar beside the conversation.
const SIDEBAR_DOCK_WIDTH: usize = 110;

fn goal_header_tone(app: &App, goal: &haarness_protocol::GoalState) -> haarness_opentui::Color {
    match goal.status {
        haarness_protocol::GoalStatus::Active => app.theme.primary,
        haarness_protocol::GoalStatus::Complete => app.theme.success(),
        haarness_protocol::GoalStatus::Blocked => app.theme.danger,
        haarness_protocol::GoalStatus::Paused => app.theme.accent,
        haarness_protocol::GoalStatus::UsageLimited
        | haarness_protocol::GoalStatus::BudgetLimited
        | haarness_protocol::GoalStatus::Cleared => app.theme.muted,
    }
}

fn goal_header_status(goal: &haarness_protocol::GoalState) -> &'static str {
    match goal.status {
        haarness_protocol::GoalStatus::Active => "pursuing",
        haarness_protocol::GoalStatus::Paused => "paused",
        haarness_protocol::GoalStatus::Blocked => "blocked",
        haarness_protocol::GoalStatus::UsageLimited => "usage limited",
        haarness_protocol::GoalStatus::BudgetLimited => "budget reached",
        haarness_protocol::GoalStatus::Complete => "achieved",
        haarness_protocol::GoalStatus::Cleared => "cleared",
    }
}

fn goal_header_usage(goal: &haarness_protocol::GoalState) -> Option<String> {
    match goal.token_budget {
        Some(budget) => Some(format!(
            "{} / {} tokens",
            compact_count(goal.tokens_used),
            compact_count(budget)
        )),
        None if goal.tokens_used > 0 => Some(format!("{} tokens", compact_count(goal.tokens_used))),
        None => None,
    }
}

fn goal_header_line(app: &App, width: usize) -> Line {
    let Some(goal) = &app.goal else {
        return Line::empty();
    };
    let tone = goal_header_tone(app, goal);
    let status = goal_header_status(goal);
    let usage = goal_header_usage(goal);
    let marker = " ◆ ";
    let full_right = usage
        .as_ref()
        .map_or_else(|| status.to_owned(), |usage| format!("{status} · {usage}"));
    let full_right = format!("  {full_right} ");
    let compact_right = format!("  {status} ");
    let show_usage = usage.is_some() && marker.width() + 12 + full_right.width() <= width;
    let right = if show_usage {
        full_right
    } else {
        compact_right
    };
    let objective_width = width.saturating_sub(marker.width() + right.width());
    let objective = single_line(&goal.goal, objective_width);
    let pad = width.saturating_sub(marker.width() + objective.width() + right.width());
    let mut spans = vec![
        Span::new(marker, tone),
        Span::new(objective, app.theme.text),
        Span::new(" ".repeat(pad), app.theme.muted),
        Span::new(format!("  {status}"), tone).with_attributes(attr::BOLD),
    ];
    if show_usage {
        if let Some(usage) = usage {
            spans.push(Span::new(format!(" · {usage} "), app.theme.muted));
        } else {
            spans.push(Span::new(" ", app.theme.muted));
        }
    } else {
        spans.push(Span::new(" ", app.theme.muted));
    }
    Line::new(spans)
}

/// Rows pinned above the conversation. Ordinary sessions pin nothing at all:
/// identity lives in the masthead at the top of the transcript and in the
/// band on the last row, so the top edge of the terminal is content. Only a
/// standing goal or a child-agent view earns a permanent row up here.
fn header_rows(app: &App, width: usize) -> (Vec<Line>, Vec<frame::HitRegion>) {
    let Some(parent) = app.parent_stack.last() else {
        if app.goal.is_some() {
            return (vec![goal_header_line(app, width)], Vec::new());
        }
        return (Vec::new(), Vec::new());
    };
    let theme = &app.theme;
    let sibling_navigation = has_navigable_siblings(app);
    let mut controls = vec![(" ↑ parent ", frame::HitTarget::ChildParent)];
    if sibling_navigation {
        controls.extend([
            (" ← previous ", frame::HitTarget::ChildPrevious),
            (" next → ", frame::HitTarget::ChildNext),
        ]);
    }
    let controls_width = controls
        .iter()
        .map(|(label, _)| label.width())
        .sum::<usize>();
    let same_row = width >= controls_width.saturating_add(32);
    let breadcrumb_width = if same_row {
        width.saturating_sub(controls_width)
    } else {
        width
    };
    // Keep the child discriminator visible even when a long parent/task path
    // is truncated. Sibling agents commonly share the same generated title;
    // hiding the id made a successful Previous/Next click look like a no-op.
    let identity = format!("  #{}", short_id(&app.thread.id.to_string()));
    let lead = "  ";
    let crumb = format!("{} › ", single_line(&parent.title, 24));
    let path_width = breadcrumb_width.saturating_sub(lead.width() + identity.width());
    let crumb = single_line(&crumb, path_width);
    let title = single_line(
        &app.thread.title,
        path_width.saturating_sub(crumb.width()).max(1),
    );
    let breadcrumb_cells = lead.width() + crumb.width() + title.width() + identity.width();
    let mut first = vec![
        Span::new(lead, theme.muted),
        Span::new(crumb, theme.muted),
        Span::new(title, theme.text).with_attributes(attr::BOLD),
        Span::new(identity, theme.muted),
    ];
    if breadcrumb_cells < breadcrumb_width {
        first.push(Span::new(
            " ".repeat(breadcrumb_width - breadcrumb_cells),
            theme.muted,
        ));
    }

    let control_row = usize::from(!same_row);
    let mut cursor = if same_row { breadcrumb_width } else { 0 };
    let mut navigation = Vec::new();
    if same_row {
        navigation.append(&mut first);
    } else {
        navigation.push(Span::new(String::new(), theme.muted));
    }
    let mut hits = Vec::with_capacity(controls.len());
    for (label, target) in controls {
        let start = cursor;
        cursor += label.width();
        navigation.push(Span::new(label, theme.primary).with_attributes(attr::BOLD));
        hits.push(frame::HitRegion {
            row: control_row,
            start,
            end: cursor.min(width),
            target,
            paint_hover: true,
        });
    }
    if cursor < width {
        navigation.push(Span::new(" ".repeat(width - cursor), theme.muted));
    }

    let mut lines = if same_row {
        vec![Line::new(navigation)]
    } else {
        vec![Line::new(first), Line::new(navigation)]
    };
    if app.goal.is_some() {
        lines.push(goal_header_line(app, width));
    }
    (lines, hits)
}

/// Whether Left/Right currently have a real child-session destination. Both
/// the clickable header and keyboard-hint footer use this so they never
/// advertise sibling navigation for a lone or stale child thread.
fn has_navigable_siblings(app: &App) -> bool {
    let mut sibling_ids = app
        .children
        .values()
        .map(|task| task.child_thread_id)
        .collect::<Vec<_>>();
    sibling_ids.sort_by_key(ToString::to_string);
    sibling_ids.dedup();
    sibling_ids.len() > 1 && sibling_ids.contains(&app.thread.id)
}

impl App {
    /// Build the full frame and record cell-bounded click regions for the
    /// mouse handler.
    pub fn lines(&self, size: (u32, u32)) -> Vec<Line> {
        let width = size.0.max(1) as usize;
        let height = size.1.max(1) as usize;
        self.frame_width.set(width);
        self.copy_panes.borrow_mut().clear();
        if let Some(terminal) = &self.terminal {
            let lines = crate::pty_view::frame(terminal, self.theme, self.language, width, height);
            return self.finish_frame(lines, Vec::new(), height);
        }
        // Review is a place, not a dialog: it takes the whole terminal so a
        // file tree, a patch, and its key hints all fit without competing
        // with the conversation behind them.
        if let Overlay::Diff(ref viewer) = self.overlay
            && width >= 40
            && height >= 12
        {
            let (lines, hits) = diffview::frame(self, viewer, width, height);
            return self.finish_frame(lines, hits, height);
        }
        // A command's output is a place too, at any size.
        if matches!(self.overlay, Overlay::CommandOutput)
            && let Some(view) = &self.command_view
        {
            let (mut lines, mut hits) = command_output::frame(self, view, width, height);
            apply_toast(self, &mut lines, &mut hits, width, height);
            return self.finish_frame(lines, hits, height);
        }
        // The side chat is a place too, at any size: its box must stay usable.
        if matches!(self.overlay, Overlay::SideChat) {
            let (mut lines, mut hits) = side_chat::frame(self, width, height);
            apply_toast(self, &mut lines, &mut hits, width, height);
            return self.finish_frame(lines, hits, height);
        }
        if width < 40 || height < 12 {
            self.transcript_messages.borrow_mut().clear();
            let mut hits = Vec::new();
            let mut lines = compact_frame(self, width, height, &mut hits);
            apply_dialog(self, &mut lines, &mut hits, width, height);
            // As in the full frame, the approval rows are the attention
            // surface, and a question at the end of the conversation is in
            // view: a toast repeating either would cover it.
            let question_shown =
                self.first_pending_question().is_some() && self.effective_viewport_offset() == 0;
            let toast_repeats_card = matches!(self.overlay, Overlay::None)
                && self.notification_toast.as_ref().is_some_and(|toast| {
                    (toast.approval && self.first_pending_permission().is_some())
                        || (toast.question && question_shown)
                });
            if !toast_repeats_card {
                apply_toast(self, &mut lines, &mut hits, width, height);
            }
            return self.finish_frame(lines, hits, height);
        }
        let sidebar_width = self.sidebar_width(width);
        let overlay_width = self.sidebar_overlay_width(width);
        let main_width = width - sidebar_width;
        let content = main_width.min(MAX_CONTENT_WIDTH);
        self.plan_inline
            .set(sidebar_width == 0 && overlay_width == 0);
        let (mut lines, mut hits) = header_rows(self, width);
        let header_height = lines.len();
        // The masthead opens the transcript rather than pinning itself above
        // it: a launch surface on an empty session, scrollback once the
        // conversation starts, and never a second copy of the band's data.
        // With a panel on screen the identity heads that instead, and the
        // transcript opens on the conversation itself.
        let mut body = frame::TaggedLines::default();
        body.extend(home::masthead(self, content, sidebar_width > 0));
        body.append(transcript::transcript_lines(self, content));
        let composer = composer::composer_lines(self, width);
        // Blocking prompts and queued follow-ups dock directly above the
        // composer: the approval card is the loudest thing on screen while a
        // tool waits, and the strip keeps unheard messages out of the
        // conversation until the agent picks them up. On a short terminal
        // the card trims its detail rather than push the prompt off screen,
        // leaving a transcript row and the gutter above the prompt.
        let queued = composer::queued_strip(self, width);
        let approval = approval::approval_lines(
            self,
            width,
            height.saturating_sub(lines.len() + composer.len() + queued.len() + 2),
        );
        let card_shown = !approval.is_empty();
        let docked = card_shown || !queued.is_empty();
        let mut bottom = frame::TaggedLines::default();
        if matches!(self.overlay, Overlay::None) {
            // Header, full composer, and the newest transcript row have fixed
            // priority. Bound autocomplete to the remaining rows instead of
            // appending an eight-row menu and truncating the composer footer.
            let menu_height = height
                .saturating_sub(lines.len() + composer.len() + approval.len() + queued.len() + 1);
            bottom.append(menu::menu_lines(self, width, menu_height));
        }
        bottom.append(approval);
        bottom.append(queued);
        // A docked panel gets the same one-row gutter the transcript has, so
        // the prompt never reads as the panel's last row. It is the first
        // decoration kept when rows are scarce, and still yields rather than
        // cost the transcript its only row.
        if docked && height.saturating_sub(lines.len() + bottom.len() + composer.len()) >= 2 {
            bottom.push(Line::empty());
        }
        bottom.extend(composer);
        // Root conversations need the same visual boundary that child panes
        // already carry in their footer. Keep the gutter outside the
        // transcript so scrollback never treats decoration as conversation
        // content, but yield it when doing so would leave no transcript row
        // on a compact regular frame.
        if height.saturating_sub(lines.len() + bottom.len()) >= 2 {
            bottom.lines.insert(0, Line::empty());
            for (row, _) in &mut bottom.tags {
                *row += 1;
            }
            for entry in &mut bottom.ranged {
                entry.0 += 1;
            }
        }
        // The region opens on a quiet row, the mirror of the gutter above the
        // composer: the masthead, a pinned breadcrumb, and the sidebar's first
        // heading all need air between them and the terminal's top edge. It is
        // part of the frame rather than the document, so scrolling the
        // transcript never consumes it — but like the composer's gutter it
        // yields rather than cost a compact frame its only transcript row.
        let region_height = height.saturating_sub(lines.len() + bottom.len());
        let gutter = usize::from(region_height >= 2);
        let transcript_height = region_height - gutter;
        if self.items.is_empty() && body.lines.len() < transcript_height {
            // A launch surface, not an empty top-anchored document: the
            // masthead settles onto the composer the way a shell banner does,
            // and the composer itself never moves when the session starts.
            let padding = transcript_height - body.lines.len();
            let mut settled = vec![Line::empty(); padding];
            settled.append(&mut body.lines);
            body.lines = settled;
            for (row, _) in &mut body.tags {
                *row += padding;
            }
            for entry in &mut body.ranged {
                entry.0 += padding;
            }
        }

        // Windowed scrollback: `viewport_offset` counts lines from the bottom
        // and clamps to the transcript length.
        let max_offset = body.lines.len().saturating_sub(transcript_height);
        self.max_viewport_offset.set(max_offset);
        self.transcript_rows.set(transcript_height);
        let previous_offset = self.effective_viewport_offset();
        let previous_len = self.last_transcript_len.replace(body.lines.len());
        // Once the reader leaves the bottom, preserve the document-relative
        // end of their window. Streaming is append-only at the protocol level
        // but not at the rendered-line level: closing a Markdown link, table,
        // or fence can make the live block shorter before later deltas grow it
        // again. Account for both directions so those reflows never drag the
        // paused transcript.
        let jump = self
            .pending_message_jump
            .take()
            .filter(|(thread, _)| *thread == self.thread.id)
            .and_then(|(_, id)| {
                body.messages
                    .iter()
                    .find(|(message, _)| *message == id)
                    .map(|(_, row)| *row)
            });
        self.transcript_messages.replace(body.messages.clone());
        let offset = if let Some(row) = jump {
            body.lines.len().saturating_sub(row + transcript_height)
        } else if previous_offset == 0 || previous_len == 0 {
            previous_offset
        } else if body.lines.len() >= previous_len {
            previous_offset.saturating_add(body.lines.len() - previous_len)
        } else {
            previous_offset.saturating_sub(previous_len - body.lines.len())
        }
        .min(max_offset);
        self.set_effective_viewport_offset(offset);
        let end = body.lines.len() - offset;
        let start = end.saturating_sub(transcript_height);
        let window: Vec<Line> = body
            .lines
            .into_iter()
            .skip(start)
            .take(end - start)
            .collect();

        for _ in 0..gutter {
            lines.push(Line::empty());
        }
        // Top-anchor: content reads top-down like a document; empty space
        // sits between the content and the composer, never above the
        // conversation. Overflow still windows from the bottom (sticky).
        let window_base = lines.len();
        let window_len = window.len();
        lines.extend(window);
        let transcript_end = main_width - overlay_width;
        for (row, target) in body.tags {
            if row >= start && row < end {
                hits.push(frame::HitRegion {
                    row: window_base + (row - start),
                    start: 0,
                    end: transcript_end,
                    target,
                    paint_hover: false,
                });
            }
        }
        for (row, from, to, target) in body.ranged {
            if row >= start && row < end {
                hits.push(frame::HitRegion {
                    row: window_base + (row - start),
                    start: from.min(transcript_end),
                    end: to.min(transcript_end),
                    target,
                    paint_hover: false,
                });
            }
        }
        for _ in 0..transcript_height.saturating_sub(window_len) {
            lines.push(Line::empty());
        }
        // Right sidebar shares the transcript region: docked beside it, or
        // drawn over its right edge when the terminal is too narrow.
        let panel_width = sidebar_width.max(overlay_width);
        if panel_width > 0 {
            // The region starts at the quiet row so the panel surface reaches
            // the top edge unbroken; the sidebar's own leading blank keeps its
            // first heading on the same baseline as the first transcript row.
            let region_top = header_height;
            let region_end = region_top + region_height;
            let panel_start = width - panel_width;
            let sidebar = sidebar::sidebar_lines(self, panel_width.saturating_sub(2), height);
            let mut sidebar_rows = sidebar.lines.into_iter();
            // The divider belongs to the pane, not its content. Merge one
            // sidebar row (possibly empty) into every row of the region so
            // the rule never breaks around sparse sections.
            self.copy_panes.borrow_mut().push(frame::CopyPane {
                rows: region_top..region_end,
                columns: panel_start..width,
                side: false,
            });
            for row in region_top..region_end {
                let side_line = sidebar_rows.next().unwrap_or_else(Line::empty);
                if let Some(target) = lines.get_mut(row) {
                    // Over the conversation, the panel is in front: what it
                    // covers recedes, as behind a dialog.
                    if overlay_width > 0 {
                        for span in &mut target.spans {
                            span.fg = self.theme.muted;
                        }
                    }
                    merge_columns(target, side_line, panel_start, panel_width, &self.theme);
                }
            }
            for (row, target) in sidebar.tags {
                let absolute = region_top + row;
                if absolute < region_end {
                    hits.push(frame::HitRegion {
                        row: absolute,
                        start: panel_start,
                        end: width,
                        target,
                        paint_hover: true,
                    });
                }
            }
        }
        let bottom_base = lines.len();
        for (row, target) in &bottom.tags {
            hits.push(frame::HitRegion {
                row: bottom_base + row,
                start: 0,
                end: width,
                target: target.clone(),
                // The approval card is a multi-row surface whose whole-card
                // focus affordance must not flash on every pointer pass;
                // its chips carry their own hover feedback.
                paint_hover: !matches!(target, frame::HitTarget::PermissionFocus),
            });
        }
        for (row, from, to, target) in &bottom.ranged {
            hits.push(frame::HitRegion {
                row: bottom_base + row,
                start: *from,
                end: (*to).min(width),
                target: target.clone(),
                paint_hover: true,
            });
        }
        lines.extend(bottom.lines);
        apply_dialog(self, &mut lines, &mut hits, width, height);
        // The approval card is the attention surface whenever it is on
        // screen; a toast repeating it would only cover the conversation (or,
        // on a short terminal, the card itself).
        // So is a question shown in the conversation: it waits at the end of
        // it, or its choices are in view.
        let question_shown = self.first_pending_question().is_some()
            && (offset == 0
                || hits
                    .iter()
                    .any(|hit| matches!(hit.target, frame::HitTarget::QuestionChoice(_))));
        let toast_repeats_card = matches!(self.overlay, Overlay::None)
            && self.notification_toast.as_ref().is_some_and(|toast| {
                (toast.approval && card_shown) || (toast.question && question_shown)
            });
        if !toast_repeats_card {
            apply_toast(self, &mut lines, &mut hits, width, height);
        }
        self.finish_frame(lines, hits, height)
    }

    fn finish_frame(
        &self,
        mut lines: Vec<Line>,
        mut hits: Vec<frame::HitRegion>,
        height: usize,
    ) -> Vec<Line> {
        lines.resize_with(height, Line::empty);
        lines.truncate(height);
        hits.retain(|hit| hit.row < height);
        if self.selection_range.is_none()
            && let Some((column, row)) = self.hovered_cell
            && let Some(hit) = hits.iter().rev().find(|hit| {
                hit.row == usize::from(row)
                    && usize::from(column) >= hit.start
                    && usize::from(column) < hit.end
            })
            && hit.paint_hover
        {
            match &hit.target {
                // A dialog surface exists solely to prevent click-through and
                // must never make inert card chrome look interactive. On a
                // menu row, mouse movement already changes selection; when
                // that scrolls a picker, this cell can now belong to another
                // item, and painting it again would highlight both the
                // selected and stale row.
                frame::HitTarget::DialogSurface | frame::HitTarget::MenuRow(_) => {}
                frame::HitTarget::DialogItem(_) => {
                    let target = hit.target.clone();
                    for region in hits
                        .iter()
                        .filter(|region| region.paint_hover && region.target == target)
                    {
                        if let Some(line) = lines.get_mut(region.row) {
                            span::dialog_item_region(line, region.start, region.end, &self.theme);
                        }
                    }
                }
                _ => {
                    // A logical item can occupy several terminal rows (for
                    // example, a sidebar agent title plus its activity line).
                    // Paint every tagged row together so hover never implies
                    // that the continuation row is a separate action.
                    let target = hit.target.clone();
                    for region in hits
                        .iter()
                        .filter(|region| region.paint_hover && region.target == target)
                    {
                        if let Some(line) = lines.get_mut(region.row) {
                            span::interactive_region(line, region.start, region.end, &self.theme);
                        }
                    }
                }
            }
        }
        *self.hit_rows.borrow_mut() = hits;
        *self.last_frame.borrow_mut() = frame::CopyFrame::new(&lines, self.copy_panes.take());
        // Paint exactly what a release copies: the pane the drag started in,
        // without decoration.
        for (row, columns) in self.selection_runs() {
            if let Some(line) = lines.get_mut(row) {
                span::highlight_range(line, columns.start, columns.end, &self.theme);
            }
        }
        lines
    }

    /// Sidebar columns (including its separator) when it fits and is open.
    pub fn sidebar_width(&self, width: usize) -> usize {
        if self.sidebar_open && width >= SIDEBAR_DOCK_WIDTH && !self.is_home() {
            SIDEBAR_WIDTH
        } else {
            0
        }
    }

    /// Sidebar columns drawn over the conversation's right edge on a
    /// terminal too narrow to dock it.
    pub fn sidebar_overlay_width(&self, width: usize) -> usize {
        if self.sidebar_overlay && width < SIDEBAR_DOCK_WIDTH && !self.is_home() {
            SIDEBAR_WIDTH.min(width)
        } else {
            0
        }
    }

    /// Ctrl+B: show or hide the sidebar. Wide enough, it docks beside the
    /// conversation, and the choice is kept for the next launch; narrower,
    /// it opens over the conversation until hidden.
    pub fn toggle_sidebar(&mut self) {
        let docks = self.frame_width.get() >= SIDEBAR_DOCK_WIDTH;
        let shown = if docks {
            self.sidebar_open
        } else {
            self.sidebar_overlay
        };
        self.sidebar_open = !shown;
        self.sidebar_overlay = !shown;
        if docks {
            crate::config::save_sidebar(self.sidebar_open);
        }
    }

    /// Ctrl+T: the next thinking display (Compact, Show, Hide), kept for the
    /// next launch.
    pub fn cycle_thinking(&mut self) {
        self.thinking = self.thinking.next();
        crate::config::save_thinking(self.thinking);
        self.notice = self.language.format(
            "Thinking display: {mode}",
            &[("mode", self.thinking.label(self.language))],
        );
    }

    fn is_home(&self) -> bool {
        self.items.is_empty()
            && self.todos.is_empty()
            && self.children.is_empty()
            && self.preview.is_empty()
            && self.reasoning_preview.is_empty()
            && self.active_turn.is_none()
            && self.pending_permissions.is_empty()
            && self.pending_questions.is_empty()
    }

    /// Open or close the fold nearest the prompt among those on screen (a
    /// tool's output, a long prompt, a thought), as a click on it would.
    pub fn toggle_nearest_fold(&mut self) {
        let nearest = self
            .hit_rows
            .borrow()
            .iter()
            .filter_map(|hit| match hit.target {
                frame::HitTarget::ToolToggle(item) => Some((hit.row, item)),
                _ => None,
            })
            .max_by_key(|(row, _)| *row)
            .map(|(_, item)| item);
        match nearest {
            Some(item) => {
                if !self.expanded.remove(&item) {
                    self.expanded.insert(item);
                }
            }
            // While a card waits the band keeps its summary; a notice set
            // now would stay in its place.
            None if self.pending_permissions.is_empty() => {
                self.notice = "Nothing to expand on screen".into();
            }
            None => {}
        }
    }

    /// The click target at a screen cell, resolved against the last frame.
    pub fn hit_at(&self, row: usize, column: usize) -> Option<frame::HitTarget> {
        self.hit_rows
            .borrow()
            .iter()
            .rev()
            .find(|hit| hit.row == row && column >= hit.start && column < hit.end)
            .map(|hit| hit.target.clone())
    }
}

fn compact_frame(
    app: &App,
    width: usize,
    height: usize,
    hits: &mut Vec<frame::HitRegion>,
) -> Vec<Line> {
    let mut lines = vec![Line::empty(); height];
    app.max_viewport_offset.set(0);
    if height == 0 {
        return lines;
    }
    // The keyboard may already answer a waiting request here, so it is never
    // hidden: its subject, its answers, and the hint in the status row.
    let card_owns_keys = app.permission_focus && app.first_pending_permission().is_some();
    // Chrome goes first, the conversation last: the masthead, sidebar and
    // band's segments are gone, and every row above the prompt (and the
    // approval rows) reads the newest of the conversation, scrollable.
    let card = app.first_pending_permission().is_some();
    let reserved = match (height >= 3, card) {
        (false, _) => height,
        (true, false) => 3,
        (true, true) if height >= 5 => 5,
        (true, true) => 4,
    };
    let room = height.saturating_sub(reserved);
    if app.is_home() || room == 0 {
        lines[0] = Line::plain(single_line("haarness", width), app.theme.primary);
    } else {
        let body = transcript::transcript_lines(app, width);
        let max_offset = body.lines.len().saturating_sub(room);
        app.max_viewport_offset.set(max_offset);
        app.transcript_rows.set(room);
        let offset = app.effective_viewport_offset().min(max_offset);
        app.set_effective_viewport_offset(offset);
        let end = body.lines.len() - offset;
        let start = end.saturating_sub(room);
        for (row, line) in body.lines[start..end].iter().enumerate() {
            lines[row] = line.clone();
        }
        for (row, target) in body.tags {
            if row >= start && row < end {
                hits.push(frame::HitRegion {
                    row: row - start,
                    start: 0,
                    end: width,
                    target,
                    paint_hover: false,
                });
            }
        }
    }
    let notice = app.permission_notice();
    if height >= 3 {
        if !app.is_child_view() {
            let prompt = if app.composer.is_empty() && card_owns_keys {
                app.language
                    .text("Answer the approval above · ctrl+g to type")
            } else if app.composer.is_empty() {
                app.language.text(composer::placeholder(app))
            } else {
                app.composer.text()
            };
            let tone = if card_owns_keys {
                app.theme.muted
            } else {
                app.theme.text
            };
            lines[height - 3] = Line::plain(single_line(prompt, width), tone);
        }
        lines[height - 2] = Line::plain("─".repeat(width), app.theme.hairline());
        let status = if let Some(notice) = &notice {
            notice.as_str()
        } else if app.active_turn.is_some() {
            "working · ctrl+c twice to interrupt"
        } else {
            app.notice.as_str()
        };
        let status = single_line(status, width);
        let padding = width.saturating_sub(status.width());
        lines[height - 1] = Line::new(vec![
            Span::new(status, app.theme.muted).on(app.theme.surface),
            Span::new(" ".repeat(padding), app.theme.muted).on(app.theme.surface),
        ]);
    }
    if let Some(card) = approval::compact_lines(app, width) {
        // Above the prompt when there is room; on a tinier screen the answers
        // win over the title and then the prompt, and the status row still
        // names the request.
        let answers_row = height.saturating_sub(4);
        if height >= 5 {
            lines[height - 5] = card.request;
        }
        lines[answers_row] = card.answers;
        for (start, end, target) in card.targets {
            hits.push(frame::HitRegion {
                row: answers_row,
                start,
                end,
                target,
                paint_hover: true,
            });
        }
    }
    lines
}

fn apply_toast(
    app: &App,
    lines: &mut Vec<Line>,
    hits: &mut Vec<frame::HitRegion>,
    width: usize,
    height: usize,
) {
    let Some(toast) = &app.notification_toast else {
        return;
    };
    let border = match toast.variant {
        ToastVariant::Success => app.theme.success(),
        ToastVariant::Warning => app.theme.accent,
        ToastVariant::Error => app.theme.danger,
    };
    let max_width = 60.min(width.saturating_sub(6)).max(20.min(width));
    let longest = toast
        .message
        .lines()
        .map(haarness_opentui::display_width)
        .chain(toast.title.as_deref().map(haarness_opentui::display_width))
        .max()
        .unwrap_or(0);
    let card_width = (longest + 6).clamp(20.min(max_width), max_width);
    let inner = card_width.saturating_sub(6).max(8);
    let mut content = Vec::new();
    if let Some(title) = &toast.title {
        content.push((title.clone(), true));
    }
    for paragraph in toast.message.lines() {
        for row in textwrap::wrap(paragraph, inner) {
            content.push((row.into_owned(), false));
        }
    }
    let mut card = vec![Line::new(vec![
        Span::new("━".repeat(card_width), border).on(app.theme.overlay),
    ])];
    for (text, bold) in content {
        card.push(toast_row(&text, bold, card_width, app));
    }
    card.push(toast_row("", false, card_width, app));
    let top = 2.min(height.saturating_sub(card.len()));
    let left = width.saturating_sub(card_width + 2);
    // The margin right of the card is cleared too: a row of text running
    // under the toast must not leave a stray glyph or two beside it.
    let margin = width.saturating_sub(left + card_width);
    lines.resize_with(height, Line::empty);
    app.copy_panes.borrow_mut().push(frame::CopyPane {
        rows: top..(top + card.len()).min(height),
        columns: left..width,
        side: false,
    });
    for (offset, mut row) in card.into_iter().enumerate() {
        if top + offset >= height {
            break;
        }
        row.spans
            .push(Span::new(" ".repeat(margin), app.theme.muted));
        let base = std::mem::take(&mut lines[top + offset]);
        lines[top + offset] = overlay_row(base, row, left, card_width + margin, width, app);
        hits.push(frame::HitRegion {
            row: top + offset,
            start: left,
            end: left + card_width,
            target: frame::HitTarget::ToastDismiss,
            paint_hover: true,
        });
    }
}

/// Toast rows are the same raised panel as a dialog: a variant-colored cap
/// above, surface fill beneath, and no drawn edges.
fn toast_row(text: &str, bold: bool, width: usize, app: &App) -> Line {
    let content = single_line(text, width.saturating_sub(4));
    let padding = width.saturating_sub(content.width() + 4);
    let attributes = if bold { attr::BOLD } else { attr::NONE };
    Line::new(vec![
        Span::new("  ", app.theme.text).on(app.theme.overlay),
        Span::new(content, app.theme.text)
            .with_attributes(attributes)
            .on(app.theme.overlay),
        Span::new(" ".repeat(padding + 2), app.theme.text).on(app.theme.overlay),
    ])
}

fn apply_dialog(
    app: &App,
    lines: &mut Vec<Line>,
    hits: &mut Vec<frame::HitRegion>,
    width: usize,
    height: usize,
) {
    if matches!(app.overlay, Overlay::None) {
        return;
    }
    lines.resize_with(height, Line::empty);
    lines.truncate(height);
    for line in lines.iter_mut() {
        for span in &mut line.spans {
            span.attributes |= attr::DIM;
        }
    }
    // Cards claim the room a wide terminal offers: a picker that stays 72
    // columns wide on a 160-column screen reads as timid, and every extra
    // column goes to the descriptions beside each choice.
    let preferred = match app.overlay {
        Overlay::Models | Overlay::Diff(_) | Overlay::Workflow { .. } => 96,
        Overlay::Panel { .. } => 88,
        // The command list (/help) reads each description whole where the
        // terminal has the room, measured over every command so that
        // filtering never changes its width.
        Overlay::Commands => menu::commands_width(app).max(84),
        _ => 84,
    };
    // Below 80 columns a one-cell margin only shows a sliver of dimmed
    // transcript glyphs beside the card; take the full width instead.
    let horizontal_margin = if width >= 80 { 4 } else { 0 };
    let available_width = width.saturating_sub(horizontal_margin).max(1);
    let card_width = preferred.min(available_width).max(32.min(width)).min(width);
    let vertical_margin = if height >= 9 { 2 } else { 0 };
    let available_height = height.saturating_sub(vertical_margin).max(1);
    // Reports are read, not picked from: give the workflow inspector and the
    // informational panels the room a picker does not need.
    let report = matches!(
        app.overlay,
        Overlay::Panel { .. }
            | Overlay::Diff(_)
            | Overlay::Workflow { .. }
            | Overlay::ProviderLogin
            | Overlay::RemoteControl
            | Overlay::Chrome
            | Overlay::SideChat
    );
    // A tall terminal should show more of the list, not the same fifteen rows
    // with empty space above and below the card.
    let max_height = if report {
        height.saturating_sub(4).clamp(7, 44).min(available_height)
    } else {
        (height * 3 / 4).clamp(7, 26).min(available_height)
    };
    let Some(card) = menu::dialog_lines(app, card_width, max_height) else {
        return;
    };
    let card_height = card.lines.len().min(height.saturating_sub(2));
    let left = width.saturating_sub(card_width) / 2;
    // A list being filtered keeps its top edge where it opened: it shortens
    // from the bottom instead of jumping to stay centred.
    let anchored = if app.overlay.is_filterable() {
        // Which dialog this is: a list by its kind, an options dialog by its
        // title (one kind opens many pages).
        let key = match &app.overlay {
            Overlay::Options { title, .. } => title.clone(),
            Overlay::Commands => "commands".to_owned(),
            Overlay::Threads => "sessions".to_owned(),
            Overlay::Models => "models".to_owned(),
            Overlay::Agents => "agents".to_owned(),
            Overlay::Themes => "themes".to_owned(),
            _ => String::new(),
        };
        let mut anchor = app.dialog_anchor.borrow_mut();
        let tallest = match anchor.as_ref() {
            Some((open, screen, tallest)) if *open == key && *screen == height => {
                (*tallest).max(card_height)
            }
            _ => card_height,
        };
        *anchor = Some((key, height, tallest));
        tallest
    } else {
        *app.dialog_anchor.borrow_mut() = None;
        card_height
    };
    let top = height.saturating_sub(anchored) / 2;
    hits.clear();
    // A margin of a few cells shows only fragments of the dimmed screen
    // beside the card — the last digit of the version, half a word — so it
    // stays blank; a wide margin still shows the chat behind.
    let sliver = left < 6;
    app.copy_panes.borrow_mut().push(frame::CopyPane {
        rows: top..top + card_height,
        columns: left..left + card_width,
        side: false,
    });
    for (offset, card_line) in card.lines.into_iter().take(card_height).enumerate() {
        let row = top + offset;
        let base = if sliver {
            Line::empty()
        } else {
            std::mem::take(&mut lines[row])
        };
        lines[row] = overlay_row(base, card_line, left, card_width, width, app);
    }
    // Broad surface targets sit below specific rows in z-order. Hit testing
    // resolves regions back-to-front, so menu rows and toasts remain clickable
    // wherever they overlap the card.
    for row in top..top + card_height {
        hits.push(frame::HitRegion {
            row,
            start: left,
            end: left + card_width,
            target: frame::HitTarget::DialogSurface,
            paint_hover: false,
        });
    }
    for (row, target) in card.tags {
        if row < card_height {
            hits.push(frame::HitRegion {
                row: top + row,
                start: left,
                end: left + card_width,
                target,
                paint_hover: true,
            });
        }
    }
}

fn overlay_row(
    base: Line,
    card: Line,
    left: usize,
    card_width: usize,
    screen_width: usize,
    app: &App,
) -> Line {
    let mut spans = slice_spans(&base, 0, left);
    let prefix_width: usize = spans.iter().map(Span::width).sum();
    if prefix_width < left {
        spans.push(Span::new(" ".repeat(left - prefix_width), app.theme.muted));
    }
    spans.extend(card.spans);
    let right = left + card_width;
    spans.extend(slice_spans(&base, right, screen_width));
    let used: usize = spans.iter().map(Span::width).sum();
    if used < screen_width {
        spans.push(Span::new(" ".repeat(screen_width - used), app.theme.muted));
    }
    Line::new(spans)
}

fn slice_spans(line: &Line, from: usize, to: usize) -> Vec<Span> {
    let mut output = Vec::new();
    let mut cell = 0;
    for span in &line.spans {
        let mut text = String::new();
        let mut included = false;
        for character in span.text.chars() {
            let cells = character.width().unwrap_or(0);
            let next = cell + cells;
            let intersects = if cells == 0 {
                included
            } else {
                next > from && cell < to
            };
            if intersects {
                text.push(character);
            }
            included = intersects;
            cell = next;
        }
        if !text.is_empty() {
            let mut clipped = span.clone();
            clipped.text = text;
            output.push(clipped);
        }
    }
    output
}

/// Pad `target` to `main_width` display columns and append a full-height
/// sidebar panel. Haarness distinguishes the pane with one continuous
/// background rectangle; a box-drawing separator can appear broken across
/// terminal fonts, wrapped rows, and mixed glyph widths.
fn merge_columns(
    target: &mut Line,
    side: Line,
    main_width: usize,
    sidebar_width: usize,
    theme: &crate::theme::Theme,
) {
    use unicode_width::UnicodeWidthChar as _;

    // A long or wide-glyph main row must never push the sidebar panel to a
    // different column. Clip spans while preserving their styles, then pad
    // to the exact main-region width.
    let mut remaining = main_width;
    let mut fitted = Vec::new();
    for mut span in std::mem::take(&mut target.spans) {
        if remaining == 0 {
            break;
        }
        let mut text = String::new();
        for character in span.text.chars() {
            let cells = character.width().unwrap_or(0);
            if cells > remaining {
                break;
            }
            text.push(character);
            remaining = remaining.saturating_sub(cells);
        }
        if !text.is_empty() {
            span.text = text;
            fitted.push(span);
        }
    }
    target.spans = fitted;
    if remaining > 0 {
        target
            .spans
            .push(Span::new(" ".repeat(remaining), theme.muted));
    }
    let panel_padding = sidebar_width.min(2);
    target.spans.push(
        Span::new(" ".repeat(panel_padding), theme.surface)
            .on(theme.surface)
            .unselectable(),
    );
    let mut panel_remaining = sidebar_width.saturating_sub(panel_padding);
    for mut span in side.spans {
        if panel_remaining == 0 {
            break;
        }
        let mut text = String::new();
        for character in span.text.chars() {
            let cells = character.width().unwrap_or(0);
            if cells > panel_remaining {
                break;
            }
            text.push(character);
            panel_remaining = panel_remaining.saturating_sub(cells);
        }
        if !text.is_empty() {
            span.text = text;
            span.bg.get_or_insert(theme.surface);
            target.spans.push(span);
        }
    }
    if panel_remaining > 0 {
        target
            .spans
            .push(Span::new(" ".repeat(panel_remaining), theme.surface).on(theme.surface));
    }
}
