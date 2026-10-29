//! The full-screen output of one shell command, opened from its row, the
//! sidebar or `/tasks`.
//!
//! A header names the command and says where it is (`in background · 1m
//! 12s`, then how it ended), the output follows its end as it grows unless
//! scrolled back, and one row of key hints closes the view. It only reads:
//! `s` stops a running command, and the view keeps reading until all of its
//! output is in.

use crate::width::DisplayWidth as _;
use haarness_opentui::{Line, Span, attr};

use crate::background::{CommandView, command_line, view_can_stop, view_status};
use crate::state::App;
use crate::view::frame::{HitRegion, HitTarget};
use crate::view::span::{single_line, task_state_style};

/// Build the whole frame for the command `view` shows.
pub fn frame(
    app: &App,
    view: &CommandView,
    width: usize,
    height: usize,
) -> (Vec<Line>, Vec<HitRegion>) {
    let theme = &app.theme;
    let mut lines = vec![
        header(app, view, width),
        Line::plain("─".repeat(width), theme.hairline()),
    ];
    let bottom = vec![Line::empty(), footer(app, view, width)];
    let body_height = height.saturating_sub(lines.len() + bottom.len());
    let mut body = Vec::new();
    let note = |text: &str| {
        Line::new(vec![
            Span::new("  ", theme.muted),
            Span::new(single_line(text, width.saturating_sub(4)), theme.muted),
        ])
    };
    if view.only_kept {
        body.push(note(
            app.language.text("only the end of its output is kept"),
        ));
    } else if view.trimmed {
        body.push(note(app.language.text("earlier output is no longer kept")));
    }
    let rows = view.shown_rows(width.saturating_sub(4));
    if rows.is_empty() && body.is_empty() {
        body.push(note(app.language.text(if view.settled {
            "No output"
        } else {
            "Waiting for output…"
        })));
    }
    let lead = body.len();
    let total = lead + rows.len();
    view.rows.set(total);
    view.page.set(body_height);
    let last = total.saturating_sub(body_height);
    let start = view.top.map_or(last, |top| top.min(last));
    for index in start..total.min(start + body_height) {
        if index < lead {
            lines.push(body[index].clone());
        } else {
            lines.push(Line::new(vec![
                Span::new("  ", theme.text),
                Span::new(rows[index - lead].clone(), theme.text),
            ]));
        }
    }
    lines.resize_with(height.saturating_sub(bottom.len()), Line::empty);
    lines.extend(bottom);
    // The view is a place, not a card over the chat: a click on it leaves
    // it open, and a drag still selects its text.
    let surface = (0..height)
        .map(|row| HitRegion {
            row,
            start: 0,
            end: width,
            target: HitTarget::DialogSurface,
            paint_hover: false,
        })
        .collect();
    (lines, surface)
}

/// ` bash  npm run dev            in background · 1m 12s `.
fn header(app: &App, view: &CommandView, width: usize) -> Line {
    let theme = &app.theme;
    let (status, state) = view_status(app, view);
    let (_, tone) = task_state_style(state, app.tick, theme);
    let status = if status.is_empty() {
        String::new()
    } else {
        format!(" {status} ")
    };
    let title = " bash ";
    let command = app.shell_commands.get(&view.id).map_or_else(
        || command_line(&view.command),
        |info| command_line(&info.command),
    );
    let room = width.saturating_sub(title.width() + status.width() + 1);
    let command = single_line(&format!(" {command}"), room);
    let pad = width.saturating_sub(title.width() + command.width() + status.width());
    Line::new(vec![
        Span::new(title, theme.primary).with_attributes(attr::BOLD),
        Span::new(command, theme.text),
        Span::new(" ".repeat(pad), theme.muted),
        Span::new(status, tone),
    ])
}

/// What the keys do: back, scroll, follow the end again once scrolled
/// back, and stop while the command runs. A narrow view drops whole hints,
/// scrolling first, never half of one.
fn footer(app: &App, view: &CommandView, width: usize) -> Line {
    let theme = &app.theme;
    let language = app.language;
    // In the order shown, each with the rank it keeps its place by.
    let mut hints = vec![
        (language.text("esc back"), 0),
        (language.text("pgup/pgdn scroll"), 3),
    ];
    if view.top.is_some() {
        hints.push((language.text("end follows"), 2));
    }
    if view_can_stop(app, view) {
        hints.push((language.text("s stop"), 1));
    }
    let separator = "  \u{00b7}  ";
    let room = width.saturating_sub(2);
    let fits = |hints: &[(&str, usize)]| {
        hints.iter().map(|(hint, _)| hint.width()).sum::<usize>()
            + separator.width() * hints.len().saturating_sub(1)
            <= room
    };
    while hints.len() > 1 && !fits(&hints) {
        let last = hints
            .iter()
            .enumerate()
            .max_by_key(|(_, (_, rank))| *rank)
            .map_or(0, |(index, _)| index);
        hints.remove(last);
    }
    let text = hints
        .iter()
        .map(|(hint, _)| *hint)
        .collect::<Vec<_>>()
        .join(separator);
    Line::new(vec![
        Span::new("  ", theme.muted),
        Span::new(single_line(&text, room), theme.muted),
    ])
}
