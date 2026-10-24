//! The right pane: live child agents, todos, MCP status, context/cost, and
//! recent threads — collapsible sections, visible whenever the terminal is
//! wide enough and `ctrl+b` hasn't hidden it.

use haarness_opentui::{Line, Span, attr};

use crate::state::App;
use crate::view::frame::{HitTarget, TaggedLines};
use crate::view::home::{INSET, wordmark};
use crate::view::span::{compact_count, line, single_line, task_state_style};

pub const SECTION_TASKS: usize = 0;
pub const SECTION_TODOS: usize = 1;
pub const SECTION_MCP: usize = 2;
pub const SECTION_CONTEXT: usize = 3;
pub const SECTION_RECENT: usize = 4;
const MAX_TODO_ROWS: usize = 10;

pub fn sidebar_lines(app: &App, width: usize, height: usize) -> TaggedLines {
    let theme = &app.theme;
    let mut out = TaggedLines::default();

    // The panel opens on a quiet row, the same air the transcript gets, so the
    // identity does not butt against the top edge. The row still carries the
    // panel surface, so the pane reads as one unbroken block.
    out.push(Line::empty());
    push_identity(app, &mut out, width);

    // Keep the standing objective visible above accounting details: it is the
    // reason the session is running, while usage is supporting information.
    push_goal_section(app, &mut out, width);

    section(app, &mut out, SECTION_CONTEXT, "usage", |app, out| {
        let (input_tokens, output_tokens) = app.usage_totals();
        out.push(line(
            format!(
                "  {}↑ {}↓",
                compact_count(input_tokens),
                compact_count(output_tokens)
            ),
            theme.muted,
        ));
        if let Some(pace) = app.request_pace_label() {
            out.push(line(single_line(&format!("  {pace}"), width), theme.muted));
        }
        if let Some(fill) = app.context_fill() {
            let cells = width.saturating_sub(12).clamp(6, 24);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let filled = ((fill * cells as f64).round() as usize).min(cells);
            out.push(Line::new(vec![
                Span::new("  ", theme.muted),
                Span::new("█".repeat(filled), theme.accent),
                Span::new("░".repeat(cells - filled), theme.muted),
                Span::new(format!(" {:.0}%", fill * 100.0), theme.muted),
            ]));
        }
        if let Some(cost) = app.usage_cost() {
            if cost > 0.0 {
                out.push(line(
                    if cost < 0.005 {
                        "  <$0.01".into()
                    } else {
                        format!("  ${cost:.2}")
                    },
                    theme.muted,
                ));
            }
        }
    });

    // Current todo state is hydrated independently from historical items so
    // a pending revert/reconnect cannot leave a future task list on screen.
    if app
        .todos
        .iter()
        .any(|entry| matches!(entry.status.as_str(), "pending" | "in_progress"))
    {
        section(app, &mut out, SECTION_TODOS, "todo", |_, out| {
            let mut rows = Vec::new();
            for entry in &app.todos {
                let (marker, color) = match entry.status.as_str() {
                    "completed" => ("[✓]", theme.muted),
                    "in_progress" => ("[•]", theme.accent),
                    "cancelled" => ("[–]", theme.muted),
                    _ => ("[ ]", theme.muted),
                };
                for (index, row) in textwrap::wrap(&entry.content, width.saturating_sub(7).max(1))
                    .iter()
                    .enumerate()
                {
                    rows.push((
                        if index == 0 {
                            format!("  {marker} ")
                        } else {
                            "      ".into()
                        },
                        row.to_string(),
                        color,
                    ));
                }
            }
            let hidden = rows.len().saturating_sub(MAX_TODO_ROWS);
            for (prefix, row, color) in rows.into_iter().take(MAX_TODO_ROWS) {
                out.push(Line::new(vec![
                    Span::new(prefix, color),
                    Span::new(row, color),
                ]));
            }
            if hidden > 0 {
                out.push(line(format!("  … +{hidden} more lines"), theme.muted));
            }
        });
    }

    // Child agents are actionable activity. Hide the entire section when a
    // turn has no child tasks instead of spending prime sidebar space on an
    // empty roster.
    let visible_children = app.visible_children();
    if !visible_children.is_empty() {
        section(app, &mut out, SECTION_TASKS, "agents", |app, out| {
            let mut tasks = app.visible_children();
            tasks.sort_by(|a, b| a.0.cmp(b.0));
            for (call_id, task) in tasks {
                let (marker, color) = task_state_style(&task.state, app.tick, theme);
                let (title, agent) = app.child_task_identity(call_id, task);
                let mut activity = Vec::new();
                if title != agent {
                    activity.push(agent);
                }
                if let Some(tool) = task.last_tool.as_deref() {
                    activity.push(if task.is_active() {
                        tool.to_owned()
                    } else {
                        format!("last {tool}")
                    });
                }
                // A child that made no calls says only what state it is in,
                // as the row under its delegation does.
                if task.tool_calls > 0 {
                    activity.push(
                        app.language
                            .count(task.tool_calls, "1 tool", "{count} tools"),
                    );
                }
                match task.state.as_str() {
                    "waiting_permission" => {
                        activity.push(app.language.text("approval needed").into())
                    }
                    "running" => activity.push(app.language.text("running").into()),
                    "background" => {
                        activity.push(app.language.text("running in background").into())
                    }
                    "done" => activity.push(app.language.text("completed").into()),
                    "failed" => activity.push(app.language.text("failed").into()),
                    "cancelled" | "interrupted" => {
                        activity.push(app.language.text("cancelled").into())
                    }
                    _ => {
                        if let Some(seconds) = task.elapsed_seconds() {
                            activity.push(format!("{seconds}s"));
                        }
                    }
                }
                out.push_tagged(
                    Line::new(vec![
                        Span::new(format!("  {marker} "), color),
                        Span::new(single_line(&title, width.saturating_sub(5)), theme.text),
                    ]),
                    HitTarget::TaskOpen(call_id.clone()),
                );
                out.push_tagged(
                    line(
                        format!(
                            "    {}",
                            single_line(&activity.join(" · "), width.saturating_sub(4))
                        ),
                        theme.muted,
                    ),
                    HitTarget::TaskOpen(call_id.clone()),
                );
            }
        });
    }

    // MCP servers (fetched when the sidebar opens).
    if !app.mcp_status.is_empty() {
        section(app, &mut out, SECTION_MCP, "mcp", |app, out| {
            for server in &app.mcp_status {
                let color = if server.status == "connected" {
                    theme.text
                } else {
                    theme.muted
                };
                out.push(line(
                    format!(
                        "  {} {}",
                        if server.status == "connected" {
                            "●"
                        } else {
                            "○"
                        },
                        single_line(&server.name, width.saturating_sub(5))
                    ),
                    color,
                ));
            }
        });
    }

    // Recent threads.
    let recent: Vec<_> = app
        .threads
        .iter()
        .filter(|thread| thread.id != app.thread.id && !thread.archived && !thread.is_subagent)
        .take(5)
        .cloned()
        .collect();
    if !recent.is_empty() {
        section(app, &mut out, SECTION_RECENT, "recent", |_, out| {
            for thread in &recent {
                out.push_tagged(
                    line(
                        format!(
                            "  ▸ {}",
                            single_line(&thread.title, width.saturating_sub(5))
                        ),
                        theme.text,
                    ),
                    HitTarget::ThreadOpen(thread.id),
                );
            }
        });
    }

    out.push(Line::empty());
    out.push(line(app.language.text("ctrl+b hides"), theme.muted));
    out.lines.truncate(height.saturating_sub(4));
    out.tags.retain(|(row, _)| *row < height.saturating_sub(4));
    out
}

/// The panel's masthead: the wordmark, then what this conversation is about.
/// It heads the panel rather than the transcript so the conversation itself
/// starts at the top of its own column. The durable id belongs to the band,
/// and is not repeated here.
fn push_identity(app: &App, out: &mut TaggedLines, width: usize) {
    let theme = &app.theme;
    let inset = || Span::new(" ".repeat(INSET), theme.muted);
    out.push(Line::new(vec![
        inset(),
        Span::new(wordmark(width.saturating_sub(INSET)), theme.primary).with_attributes(attr::BOLD),
    ]));
    // A child pane's pinned breadcrumb already names the thread, two columns
    // to the left. The wordmark still belongs up here; a second copy of the
    // title does not.
    let title = if app.is_child_view() {
        String::new()
    } else {
        single_line(app.thread.title.trim(), width.saturating_sub(INSET))
    };
    if !title.is_empty() {
        out.push(Line::new(vec![
            inset(),
            Span::new(title, theme.text).with_attributes(attr::BOLD),
        ]));
    }
    out.push(Line::empty());
}

fn push_goal_section(app: &App, out: &mut TaggedLines, width: usize) {
    let Some(goal) = &app.goal else {
        return;
    };
    let theme = &app.theme;
    section(app, out, SECTION_CONTEXT + 10, "goal", |_, out| {
        let rows = textwrap::wrap(&goal.goal, width.saturating_sub(4).max(1));
        for (index, row) in rows.iter().take(3).enumerate() {
            let value = if index == 2 && rows.len() > 3 {
                single_line(&format!("{row}…"), width.saturating_sub(4))
            } else {
                row.to_string()
            };
            out.push(line(format!("  {value}"), theme.text));
        }
        let tone = match goal.status {
            haarness_protocol::GoalStatus::Active => theme.primary,
            haarness_protocol::GoalStatus::Complete => theme.success(),
            haarness_protocol::GoalStatus::Blocked => theme.danger,
            haarness_protocol::GoalStatus::Paused => theme.accent,
            haarness_protocol::GoalStatus::UsageLimited
            | haarness_protocol::GoalStatus::BudgetLimited
            | haarness_protocol::GoalStatus::Cleared => theme.muted,
        };
        // The status carries why a goal stopped and what it waits for, so
        // it wraps like the objective instead of running off the pane.
        for row in textwrap::wrap(
            &crate::commands::goal_status_label(goal),
            width.saturating_sub(4).max(1),
        ) {
            out.push(line(format!("  {row}"), tone));
        }
        if let Some(budget) = goal.token_budget {
            let cells = width.saturating_sub(8).clamp(6, 24);
            let filled =
                usize::try_from(goal.tokens_used.saturating_mul(cells as u64) / budget.max(1))
                    .unwrap_or(cells)
                    .min(cells);
            out.push(Line::new(vec![
                Span::new("  ", theme.muted),
                Span::new("█".repeat(filled), tone),
                Span::new("░".repeat(cells - filled), theme.muted),
            ]));
        }
        if goal.time_used_seconds > 0 {
            out.push(line(
                format!("  {} elapsed", format_goal_time(goal.time_used_seconds)),
                theme.muted,
            ));
        }
        if !goal.criteria.is_empty() {
            let complete = goal
                .criteria
                .iter()
                .filter(|criterion| criterion.satisfied == Some(true))
                .count();
            out.push(line(
                format!("  criteria {complete}/{}", goal.criteria.len()),
                theme.muted,
            ));
        }
    });
}

pub(crate) fn format_goal_time(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        let remainder = seconds % 60;
        return if remainder == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m {remainder}s")
        };
    }
    let hours = minutes / 60;
    if hours < 24 {
        let remainder = minutes % 60;
        return if remainder == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {remainder}m")
        };
    }
    format!("{}d {}h {}m", hours / 24, hours % 24, minutes % 60)
}

/// A collapsible section: clickable header, body hidden when collapsed.
fn section(
    app: &App,
    out: &mut TaggedLines,
    index: usize,
    title: &str,
    body: impl FnOnce(&App, &mut TaggedLines),
) {
    let collapsed = app.sidebar_collapsed.contains(&index);
    let marker = if collapsed { "▸" } else { "▾" };
    out.push_tagged(
        line(
            format!("{marker} {}", app.language.text(title)),
            app.theme.primary,
        ),
        HitTarget::SidebarSection(index),
    );
    if !collapsed {
        body(app, out);
    }
    out.push(Line::empty());
}
