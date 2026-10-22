//! The full-screen side chat behind `/btw`.
//!
//! A header saying what the view is, the thread in the transcript's own
//! style (the question as a prompt block, the answer as a reply), and a
//! message box with one row of key hints. Like Review it is a place rather
//! than a dialog, so a long answer has the whole terminal.

use crate::width::DisplayWidth as _;
use haarness_opentui::{Line, Span, attr};

use crate::markdown::render_markdown;
use crate::side_chat::{Thread, TurnState};
use crate::state::App;
use crate::view::frame::HitRegion;
use crate::view::span::{single_line, spinner};
use crate::view::transcript::{mark_reply, user_block};

/// Build the whole frame for the side chat.
pub fn frame(app: &App, width: usize, height: usize) -> (Vec<Line>, Vec<HitRegion>) {
    let theme = &app.theme;
    let thread = app.side.shown();
    let busy = thread.is_some_and(Thread::busy);
    let mut lines = vec![
        header(app, width),
        Line::plain("─".repeat(width), theme.hairline()),
    ];
    let placeholder = if thread.is_some_and(|thread| !thread.turns.is_empty()) {
        "Ask a follow-up"
    } else {
        "Ask a side question"
    };
    let mut bottom = vec![Line::empty()];
    bottom.extend(super::composer::editor_rows(
        app,
        &app.side.composer,
        app.language.text(placeholder),
        None,
        true,
        width,
    ));
    bottom.push(super::composer::pulse_rule(app, width, busy));
    bottom.push(footer(app, busy, width));

    let content = width.min(super::MAX_CONTENT_WIDTH);
    let body = body(app, thread, content);
    let body_height = height.saturating_sub(lines.len() + bottom.len());
    let max = body.len().saturating_sub(body_height);
    app.side.max_scroll.set(max);
    app.side.page.set(body_height);
    let end = body.len() - app.side.scroll.min(max);
    let start = end.saturating_sub(body_height);
    // A short thread sits under the header, as the transcript's does once
    // the conversation has started.
    lines.extend(body[start..end].iter().cloned());
    lines.resize_with(height.saturating_sub(bottom.len()), Line::empty);
    lines.extend(bottom);
    (lines, Vec::new())
}

/// What the view is. "esc back" is in the hints below, once.
fn header(app: &App, width: usize) -> Line {
    let theme = &app.theme;
    let detail = format!(
        " {} · {}",
        app.language.text("Side chat"),
        app.language.text("reads this chat, never changes it")
    );
    let title = " /btw ";
    let detail = single_line(&detail, width.saturating_sub(title.width() + 1));
    Line::new(vec![
        Span::new(title, theme.primary).with_attributes(attr::BOLD),
        Span::new(detail, theme.muted),
    ])
}

/// The key hints, or the side chat's last notice while it is fresh, and
/// "Answering" while an answer is being written.
fn footer(app: &App, busy: bool, width: usize) -> Line {
    let theme = &app.theme;
    let language = app.language;
    let right = busy.then(|| format!("{} {} ", spinner(app.tick), language.text("Answering")));
    let room = width.saturating_sub(right.as_ref().map_or(0, |right| right.width()) + 3);
    let left = if app.notice.trim().is_empty() {
        let thread = app.side.shown();
        let mut hints = vec![language.text("enter send")];
        if busy {
            hints.push(language.text("ctrl+c stop"));
        }
        if thread.is_some_and(|thread| !thread.turns.is_empty()) {
            hints.push(language.text("/clear new thread"));
        }
        if thread.and_then(Thread::last_answer).is_some() {
            hints.push(language.text("/copy copies the answer"));
        }
        hints.push(language.text("esc back"));
        (hints.join("  \u{00b7}  "), theme.muted)
    } else {
        (app.notice.clone(), theme.text)
    };
    let text = single_line(&left.0, room);
    let mut spans = vec![
        Span::new("  ", theme.muted),
        Span::new(text.clone(), left.1),
    ];
    if let Some(right) = right {
        let pad = width.saturating_sub(2 + text.width() + right.width());
        spans.push(Span::new(" ".repeat(pad), theme.muted));
        spans.push(Span::new(right, theme.primary));
    }
    Line::new(spans)
}

fn body(app: &App, thread: Option<&Thread>, width: usize) -> Vec<Line> {
    let theme = &app.theme;
    let language = app.language;
    let mut lines = vec![Line::empty()];
    let Some(thread) = thread.filter(|thread| !thread.turns.is_empty()) else {
        lines.push(Line::new(vec![
            Span::new("  ", theme.text),
            Span::new(language.text("Ask a side question"), theme.text).with_attributes(attr::BOLD),
        ]));
        lines.push(Line::empty());
        let detail = language.text(
            "Reads this chat without adding to it or interrupting it, and can't run tools. Follow-ups keep the thread going.",
        );
        for row in textwrap::wrap(detail, width.saturating_sub(4).max(10)) {
            lines.push(Line::new(vec![
                Span::new("  ", theme.muted),
                Span::new(row.into_owned(), theme.muted),
            ]));
        }
        return lines;
    };
    // A server from before side chats answers each question without the
    // ones before it; say so once, quietly.
    let alone = app.server_info.as_ref().is_some_and(|info| {
        !info
            .capabilities
            .iter()
            .any(|capability| capability == haarness_protocol::BTW_STREAM_CAPABILITY)
    });
    if alone {
        lines.push(Line::new(vec![
            Span::new("  ", theme.muted),
            Span::new(
                language.text("This server answers each question on its own"),
                theme.muted,
            ),
        ]));
        lines.push(Line::empty());
    }
    for (index, turn) in thread.turns.iter().enumerate() {
        if index > 0 {
            lines.push(Line::empty());
        }
        lines.extend(user_block(
            &turn.question,
            width,
            theme,
            app.language,
            false,
        ));
        lines.push(Line::empty());
        if !turn.answer.is_empty() {
            let mut answer = render_markdown(&turn.answer, width, theme, "  ");
            mark_reply(&mut answer, theme);
            lines.extend(answer);
        }
        let status = match &turn.state {
            TurnState::Pending if turn.answer.is_empty() => Some((
                format!("{} {}", spinner(app.tick), language.text("Thinking…")),
                theme.muted,
            )),
            TurnState::Error(message) => Some((message.clone(), theme.danger)),
            TurnState::Stopped => Some((language.text("Stopped").to_owned(), theme.muted)),
            TurnState::Pending | TurnState::Done => None,
        };
        if let Some((text, color)) = status {
            if !turn.answer.is_empty() {
                lines.push(Line::empty());
            }
            for row in textwrap::wrap(&text, width.saturating_sub(4).max(10)) {
                lines.push(Line::new(vec![
                    Span::new("  ", color),
                    Span::new(row.into_owned(), color),
                ]));
            }
        }
    }
    lines
}
