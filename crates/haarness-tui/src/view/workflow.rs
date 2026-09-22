//! The durable workflow-run inspector.
//!
//! A run is a live object — phases advance, agents finish, logs arrive — so
//! this reads its snapshot out of `App` on every frame rather than freezing a
//! key/value dump at open time. The layout is a composed report: identity and
//! timing first, then whatever went wrong, then the agents doing the work,
//! then a bounded tail of the log. Long collections are summarized with their
//! true count instead of paging hundreds of rows through a dialog.

use chrono::{DateTime, Utc};
use haarness_opentui::{Color, Line, Span, attr};
use haarness_protocol::{
    WorkflowDefinition, WorkflowRun, WorkflowRunAgent, WorkflowRunAgentStatus, WorkflowRunStatus,
};
use unicode_width::UnicodeWidthStr as _;

use crate::state::{App, workflow_status, workflow_status_marker};
use crate::theme::Theme;
use crate::view::span::{line, single_line, wrap_spans};

/// Warnings and logs are tails, not archives: the newest few explain the
/// current state, and the count says how much history exists behind them.
const WARNING_TAIL: usize = 4;
const LOG_TAIL: usize = 12;

pub fn status_color(theme: &Theme, status: WorkflowRunStatus) -> Color {
    match status {
        WorkflowRunStatus::Running => theme.primary,
        WorkflowRunStatus::Completed => theme.success(),
        WorkflowRunStatus::Failed => theme.danger,
        WorkflowRunStatus::Paused | WorkflowRunStatus::Interrupted => theme.accent,
        WorkflowRunStatus::Cancelled => theme.muted,
    }
}

fn agent_status_label(status: WorkflowRunAgentStatus) -> &'static str {
    match status {
        WorkflowRunAgentStatus::Running => "running",
        WorkflowRunAgentStatus::Completed => "completed",
        WorkflowRunAgentStatus::Failed => "failed",
        WorkflowRunAgentStatus::Cancelled => "cancelled",
    }
}

fn agent_status_color(theme: &Theme, status: WorkflowRunAgentStatus) -> Color {
    match status {
        WorkflowRunAgentStatus::Running => theme.primary,
        WorkflowRunAgentStatus::Completed => theme.success(),
        WorkflowRunAgentStatus::Failed => theme.danger,
        WorkflowRunAgentStatus::Cancelled => theme.muted,
    }
}

fn agent_marker(status: WorkflowRunAgentStatus) -> &'static str {
    match status {
        WorkflowRunAgentStatus::Running => "●",
        WorkflowRunAgentStatus::Completed => "✓",
        WorkflowRunAgentStatus::Failed => "✗",
        WorkflowRunAgentStatus::Cancelled => "⊘",
    }
}

/// Wall time between two instants, or up to now while still open.
pub(crate) fn elapsed(start: DateTime<Utc>, end: Option<DateTime<Utc>>) -> String {
    let end = end.unwrap_or_else(Utc::now);
    let seconds = (end - start).num_seconds().max(0);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m {}s", seconds % 60);
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// Local wall-clock time. A workflow that ran overnight is much easier to
/// place with `02:30` than with an RFC 3339 stamp.
fn clock(time: DateTime<Utc>) -> String {
    time.with_timezone(&chrono::Local)
        .format("%H:%M")
        .to_string()
}

fn ago(time: DateTime<Utc>) -> String {
    let age = (Utc::now() - time).num_seconds().max(0);
    if age < 60 {
        "just now".into()
    } else if age < 3_600 {
        format!("{}m ago", age / 60)
    } else if age < 86_400 {
        format!("{}h ago", age / 3_600)
    } else {
        format!("{}d ago", age / 86_400)
    }
}

fn dialect(run: &WorkflowRun) -> &'static str {
    match run.definition.dialect {
        haarness_protocol::WorkflowDialect::Native => "native",
        haarness_protocol::WorkflowDialect::ClaudePortable => "claude portable",
    }
}

/// Whether a resume would replay the snapshot taken at launch or re-read the
/// file on disk. It decides what a restart actually runs, so it belongs in
/// the run's identity line rather than buried in a source field.
fn source_policy(run: &WorkflowRun) -> &'static str {
    if matches!(
        run.definition.dialect,
        haarness_protocol::WorkflowDialect::ClaudePortable
    ) || run.definition.source_path.is_none()
    {
        "original source preserved"
    } else {
        "uses installed source"
    }
}

/// An editorial section marker: a small-caps label, its count, and a hairline
/// running out to the edge of the card. It separates a report into parts
/// without spending a blank row on every heading.
fn section(title: &str, count: Option<usize>, theme: &Theme, width: usize) -> Line {
    let label = title.to_uppercase();
    let counted = count.map(|count| format!("  {count}")).unwrap_or_default();
    let used = label.width() + counted.width() + 2;
    let mut spans = vec![Span::new(label, theme.primary).with_attributes(attr::BOLD)];
    if !counted.is_empty() {
        spans.push(Span::new(counted, theme.muted));
    }
    if width > used {
        spans.push(Span::new(
            format!("  {}", "\u{2500}".repeat(width - used)),
            theme.hairline(),
        ));
    }
    Line::new(spans)
}

/// One JSON blob as a single readable line. Structured payloads belong in the
/// CLI; the dialog only has to answer "what was passed" and "what came back".
fn json_line(label: &str, value: &serde_json::Value, width: usize, theme: &Theme) -> Line {
    let text = value.to_string();
    Line::new(vec![
        Span::new(format!("{label}  "), theme.muted),
        Span::new(
            single_line(&text, width.saturating_sub(label.width() + 2)),
            theme.text,
        ),
    ])
}

fn agent_row(agent: &WorkflowRunAgent, width: usize, theme: &Theme) -> Line {
    let label = agent.label.as_deref().unwrap_or(&agent.id);
    let mut detail = vec![agent_status_label(agent.status).to_owned()];
    if let Some(phase) = agent.phase.as_deref() {
        detail.push(phase.to_owned());
    }
    if let Some(model) = agent.model.as_deref() {
        detail.push(model.to_owned());
    }
    // Elapsed time gets its own right-hand column so a fleet of agents can be
    // scanned for the slow one without reading every row.
    let duration = elapsed(agent.started_at, agent.completed_at);
    let detail = detail.join(" · ");
    let body_budget = width.saturating_sub(duration.width() + 4).max(12);
    let name_width = label.width().min(body_budget / 2).max(6);
    let name = crate::view::span::pad(&single_line(label, name_width), name_width);
    let detail = single_line(&detail, body_budget.saturating_sub(name_width + 2));
    let pad = width
        .saturating_sub(2 + name.width() + 2 + detail.width() + duration.width())
        .max(1);
    Line::new(vec![
        Span::new(
            format!("{} ", agent_marker(agent.status)),
            agent_status_color(theme, agent.status),
        ),
        Span::new(name, theme.text),
        Span::new("  ", theme.muted),
        Span::new(detail, theme.muted),
        Span::new(" ".repeat(pad), theme.muted),
        Span::new(duration, theme.muted),
    ])
}

/// The complete report for one run, at `width` content columns.
pub fn run_body(app: &App, run: &WorkflowRun, width: usize) -> Vec<Line> {
    let theme = &app.theme;
    let tone = status_color(theme, run.status);
    let mut lines = Vec::new();

    // Identity: how it is going, how long it has taken, and what would run
    // again on a resume. The dialog's own header carries the name, so the
    // report leads with state instead of repeating it.
    let state = match run.phase.as_deref() {
        Some(phase) => format!("{} · {phase}", workflow_status(run.status)),
        None => workflow_status(run.status).to_owned(),
    };
    let started = run.time.started.unwrap_or(run.time.created);
    let duration = elapsed(started, run.time.completed);
    let pad = width
        .saturating_sub(2 + state.width() + duration.width())
        .max(1);
    lines.push(Line::new(vec![
        Span::new(format!("{} ", workflow_status_marker(run.status)), tone),
        Span::new(state, tone).with_attributes(attr::BOLD),
        Span::new(" ".repeat(pad), theme.muted),
        Span::new(duration, theme.text),
    ]));
    lines.push(line(
        single_line(&run.id.to_string(), width),
        theme.hairline(),
    ));

    let mut origin = vec![
        dialect(run).to_owned(),
        format!("/{}", run.definition.invocation),
    ];
    if let Some(plugin) = run.definition.plugin_name.as_deref() {
        origin.push(format!("plugin {plugin}"));
    }
    origin.push(source_policy(run).to_owned());
    lines.push(line(single_line(&origin.join(" · "), width), theme.muted));

    let closing = if run.time.completed.is_some() {
        format!("finished {}", clock(run.time.updated))
    } else {
        format!("updated {}", ago(run.time.updated))
    };
    lines.push(line(
        single_line(&format!("started {} · {closing}", clock(started)), width),
        theme.muted,
    ));

    // Failure outranks everything below it.
    if let Some(error) = run.error.as_deref() {
        lines.push(Line::empty());
        lines.push(section("failed", None, theme, width));
        lines.extend(wrap_spans(
            &[Span::new(error.to_owned(), theme.danger)],
            width,
            "",
            "",
            theme.danger,
        ));
    }

    if let Some(args) = run.args.as_ref() {
        lines.push(Line::empty());
        lines.push(json_line("arguments", args, width, theme));
    }

    if !run.agents.is_empty() {
        lines.push(Line::empty());
        lines.push(section("agents", Some(run.agents.len()), theme, width));
        for agent in &run.agents {
            lines.push(agent_row(agent, width, theme));
            if let Some(error) = agent.error.as_deref() {
                lines.push(line(
                    format!("  {}", single_line(error, width.saturating_sub(2))),
                    theme.danger,
                ));
            }
        }
    }

    if !run.warnings.is_empty() {
        lines.push(Line::empty());
        lines.push(section("warnings", Some(run.warnings.len()), theme, width));
        let skipped = run.warnings.len().saturating_sub(WARNING_TAIL);
        if skipped > 0 {
            lines.push(line(format!("… {skipped} earlier"), theme.hairline()));
        }
        for warning in run.warnings.iter().skip(skipped) {
            let mut text = warning.message.clone();
            if let (Some(requested), Some(resolved)) = (
                warning.requested_model.as_deref(),
                warning.resolved_model.as_deref(),
            ) {
                text = format!("{text} · {requested} → {resolved}");
            }
            lines.push(line(
                format!("⚠ {}", single_line(&text, width.saturating_sub(2))),
                theme.accent,
            ));
        }
    }

    if !run.logs.is_empty() {
        lines.push(Line::empty());
        lines.push(section("logs", Some(run.logs.len()), theme, width));
        let skipped = run.logs.len().saturating_sub(LOG_TAIL);
        if skipped > 0 {
            lines.push(line(
                format!("… {skipped} earlier · haarness workflow logs shows them all"),
                theme.hairline(),
            ));
        }
        for log in run.logs.iter().skip(skipped) {
            let phase = log
                .phase
                .as_deref()
                .map_or_else(String::new, |phase| format!("{phase} · "));
            lines.push(line(
                single_line(&format!("{phase}{}", log.message), width),
                theme.muted,
            ));
        }
    }

    if let Some(result) = run.result.as_ref() {
        lines.push(Line::empty());
        lines.push(json_line("result", result, width, theme));
    }

    lines
}

/// Footer hint for the inspector: what the keys do, plus the scroll position
/// when there is more report than panel.
pub fn run_footer(shown: usize, total: usize) -> String {
    if total > shown {
        format!("{shown} of {total} rows  ·  pgup/pgdn scroll  ·  enter actions")
    } else {
        "enter actions  ·  esc close".to_owned()
    }
}

/// A definition you have not started yet. Its arguments are the reason this
/// view exists: pressing Enter on the dashboard launches immediately, and a
/// one-line description cannot tell you what the workflow expects.
pub fn definition_body(app: &App, definition: &WorkflowDefinition, width: usize) -> Vec<Line> {
    let theme = &app.theme;
    let mut lines = Vec::new();

    let dialect = match definition.dialect {
        haarness_protocol::WorkflowDialect::Native => "native",
        haarness_protocol::WorkflowDialect::ClaudePortable => "claude portable",
    };
    let invocation = format!("/{}", definition.invocation);
    let pad = width
        .saturating_sub(2 + invocation.width() + dialect.width())
        .max(1);
    lines.push(Line::new(vec![
        Span::new("◇ ", theme.primary),
        Span::new(
            single_line(&invocation, width.saturating_sub(dialect.width() + 3)),
            theme.text,
        )
        .with_attributes(attr::BOLD),
        Span::new(" ".repeat(pad), theme.muted),
        Span::new(dialect.to_owned(), theme.muted),
    ]));
    if let Some(description) = definition.meta.description.as_deref() {
        lines.extend(wrap_spans(
            &[Span::new(description.to_owned(), theme.muted)],
            width,
            "",
            "",
            theme.muted,
        ));
    }
    let mut origin = Vec::new();
    if let Some(plugin) = definition.plugin_name.as_deref() {
        origin.push(format!("plugin {plugin}"));
    }
    origin.push(definition.path.clone());
    lines.push(line(
        single_line(&origin.join(" · "), width),
        theme.hairline(),
    ));

    if let Some(when) = definition.meta.when_to_use.as_deref() {
        lines.push(Line::empty());
        lines.push(section("when to use", None, theme, width));
        lines.extend(wrap_spans(
            &[Span::new(when.to_owned(), theme.muted)],
            width,
            "",
            "",
            theme.muted,
        ));
    }

    if !definition.meta.arguments.is_empty() {
        lines.push(Line::empty());
        lines.push(section(
            "arguments",
            Some(definition.meta.arguments.len()),
            theme,
            width,
        ));
        let name_width = definition
            .meta
            .arguments
            .keys()
            .map(|name| name.width())
            .max()
            .unwrap_or(0)
            .min(width / 3)
            .max(6);
        for (name, argument) in &definition.meta.arguments {
            let kind = argument.kind.map_or("string", |kind| match kind {
                haarness_protocol::WorkflowArgumentType::String => "string",
                haarness_protocol::WorkflowArgumentType::Number => "number",
                haarness_protocol::WorkflowArgumentType::Boolean => "boolean",
            });
            let mut detail = vec![kind.to_owned()];
            match argument.default.as_ref() {
                Some(value) => detail.push(format!("default {value}")),
                None => detail.push("required".to_owned()),
            }
            if let Some(description) = argument.description.as_deref() {
                detail.push(description.to_owned());
            }
            let label = single_line(name, name_width);
            lines.push(Line::new(vec![
                Span::new(crate::view::span::pad(&label, name_width), theme.text),
                Span::new("  ", theme.muted),
                Span::new(
                    single_line(&detail.join(" · "), width.saturating_sub(name_width + 2)),
                    theme.muted,
                ),
            ]));
        }
    }

    if !definition.meta.phases.is_empty() {
        lines.push(Line::empty());
        lines.push(section(
            "phases",
            Some(definition.meta.phases.len()),
            theme,
            width,
        ));
        for phase in &definition.meta.phases {
            let detail = phase
                .detail
                .as_deref()
                .map_or_else(String::new, |detail| format!("  {detail}"));
            lines.push(line(
                single_line(&format!("{}{detail}", phase.title), width),
                theme.muted,
            ));
        }
    }

    // Lint findings explain why a definition may behave unexpectedly, and
    // Haarness already computes them at discovery time.
    if !definition.findings.is_empty() {
        lines.push(Line::empty());
        lines.push(section(
            "findings",
            Some(definition.findings.len()),
            theme,
            width,
        ));
        for finding in &definition.findings {
            lines.push(line(
                single_line(
                    &format!(
                        "line {} · {} · {}",
                        finding.line, finding.rule, finding.text
                    ),
                    width,
                ),
                theme.accent,
            ));
        }
    }

    lines
}

/// Footer hint for the definition view.
pub fn definition_footer(shown: usize, total: usize) -> String {
    if total > shown {
        format!("{shown} of {total} rows  ·  pgup/pgdn scroll  ·  enter starts it")
    } else {
        "enter starts this workflow  ·  esc close".to_owned()
    }
}

/// A file the catalog could not load. The dashboard row truncates the parse
/// error to fit; this is where you read all of it.
pub fn invalid_body(
    app: &App,
    invalid: &haarness_protocol::WorkflowInvalidDefinition,
    width: usize,
) -> Vec<Line> {
    let theme = &app.theme;
    let name = std::path::Path::new(&invalid.path).file_name().map_or_else(
        || invalid.path.clone(),
        |name| name.to_string_lossy().into(),
    );
    let mut lines = vec![
        Line::new(vec![
            Span::new("⚠ ", theme.accent),
            Span::new(single_line(&name, width.saturating_sub(2)), theme.text)
                .with_attributes(attr::BOLD),
        ]),
        line(single_line(&invalid.path, width), theme.hairline()),
    ];
    if let Some(plugin) = invalid.plugin_name.as_deref() {
        lines.push(line(format!("plugin {plugin}"), theme.muted));
    }
    if invalid.legacy {
        lines.push(line("legacy definition location", theme.muted));
    }
    lines.push(Line::empty());
    lines.push(section("error", None, theme, width));
    lines.extend(wrap_spans(
        &[Span::new(invalid.error.clone(), theme.danger)],
        width,
        "",
        "",
        theme.danger,
    ));
    lines
}

/// Footer hint for an unloadable definition: nothing to launch, only to fix.
pub fn invalid_footer(shown: usize, total: usize) -> String {
    if total > shown {
        format!("{shown} of {total} rows  ·  pgup/pgdn scroll  ·  esc close")
    } else {
        "fix the file and reopen /workflow  ·  esc close".to_owned()
    }
}
