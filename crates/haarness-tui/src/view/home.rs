//! The masthead: the document head of a conversation.
//!
//! It is transcript content, not chrome. On a fresh session it sits directly
//! above the composer with the terminal empty above it, exactly the way a
//! shell banner would; once the conversation starts it simply scrolls away.
//! Nothing about the composer moves between the two states.
//!
//! Once the panel is on screen the session's identity heads that instead, and
//! this yields entirely rather than print the same two rows twice.

use crate::width::DisplayWidth as _;
use haarness_opentui::{Line, Span, attr};

use crate::state::App;
use crate::view::span::single_line;

/// Left inset shared with the composer, so the wordmark, the prompt, and the
/// band's chip all hang off one vertical.
pub const INSET: usize = 2;

pub fn masthead(app: &App, width: usize, identity_in_panel: bool) -> Vec<Line> {
    // A child agent already has a document head: its pinned breadcrumb. A
    // second one would repeat the title and the id directly beneath it.
    if app.is_child_view() || identity_in_panel {
        return Vec::new();
    }
    let theme = &app.theme;
    let home = app.items.is_empty();
    // Only the launch surface has anything worth setting against the
    // wordmark. The session's durable id is the band's job: up here it
    // competes with the title for the eye and answers a question nobody is
    // asking mid-conversation.
    let meta = if home {
        app.server_info
            .as_ref()
            .map_or_else(String::new, |info| format!("v{}", info.version))
    } else {
        String::new()
    };
    let mut lines = vec![heading(app, width, &meta)];

    if home {
        lines.push(indented(
            single_line(
                app.language.text("durable harness for coding agents"),
                width.saturating_sub(INSET),
            ),
            theme.muted,
        ));
        if !app.any_provider_connected() && !app.models_loading {
            // The one thing standing between a fresh install and a first
            // prompt, said where the eye lands first.
            let language = app.language;
            let missing = language.text("no provider connected");
            let mut spans = vec![Span::new(" ".repeat(INSET), theme.muted)];
            spans.push(Span::new(missing, theme.accent).with_attributes(attr::BOLD));
            let room = width.saturating_sub(INSET + missing.width());
            if room >= "  ·  /connect".width() {
                // The command is drawn highlighted wherever the sentence
                // puts it.
                let template = language.text("/connect to sign in or add an API key");
                let (before, rest) = template.split_once("/connect").unwrap_or(("", template));
                spans.push(Span::new("  ·  ", theme.muted));
                if room >= "  ·  /connect".width() + before.width() + rest.width() {
                    spans.push(Span::new(before.to_owned(), theme.muted));
                }
                spans.push(Span::new("/connect", theme.accent).with_attributes(attr::BOLD));
                if room >= "  ·  /connect".width() + before.width() + rest.width() {
                    spans.push(Span::new(rest.to_owned(), theme.muted));
                }
            }
            lines.push(Line::new(spans));
        } else if let Some(runtime) = app.local_only_runtime() {
            // Nobody chose this model: say where replies come from, and how to
            // get a larger one, before the first prompt rather than after a
            // slow, small answer.
            let sentence = app.language.format(
                "Replies come from {model}, which {runtime} runs on this computer.",
                &[("model", &app.thread.model), ("runtime", runtime)],
            );
            let mut spans = vec![
                Span::new(" ".repeat(INSET), theme.muted),
                Span::new(
                    single_line(&sentence, width.saturating_sub(INSET)),
                    theme.muted,
                ),
            ];
            if INSET + sentence.width() + "  ·  /connect".width() <= width {
                spans.push(Span::new("  ·  ", theme.muted));
                spans.push(Span::new("/connect", theme.accent).with_attributes(attr::BOLD));
            }
            lines.push(Line::new(spans));
        }
        if let Some(version) = &app.update_available {
            lines.push(update_line(app, width, version));
        }
        if width >= 56 {
            lines.push(Line::empty());
            lines.push(legend(app, width));
        }
    } else {
        let mut title = indented(
            single_line(&app.thread.title, width.saturating_sub(INSET)),
            theme.text,
        );
        for span in &mut title.spans {
            span.attributes |= attr::BOLD;
        }
        lines.push(title);
    }
    lines.push(Line::empty());
    lines
}

/// The wordmark, with whatever the caller wants set against it on the same
/// baseline — the build version on the launch surface, nothing in a session.
fn heading(app: &App, width: usize, meta: &str) -> Line {
    let theme = &app.theme;
    let mark = wordmark(width.saturating_sub(INSET));
    let used = INSET + mark.width();
    let mut spans = vec![
        Span::new(" ".repeat(INSET), theme.muted),
        Span::new(mark, theme.primary).with_attributes(attr::BOLD),
    ];
    if !meta.is_empty() && used + meta.width() + 3 <= width {
        spans.push(Span::new(
            " ".repeat(width - used - meta.width() - 1),
            theme.muted,
        ));
        spans.push(Span::new(meta.to_owned(), theme.muted));
    }
    Line::new(spans)
}

/// The wordmark, letter-spaced whenever the column it sits in can hold the
/// spaced form. The plain form is the fallback for a narrow terminal or panel,
/// where letter spacing would wrap.
pub fn wordmark(available: usize) -> String {
    let spaced = "haarness"
        .chars()
        .map(|character| character.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    if available >= spaced.width() {
        spaced
    } else {
        "haarness".to_owned()
    }
}

/// The three entry points useful before a session has any activity. Task
/// navigation belongs to the in-session chrome, not the pristine home.
/// Everything else lives behind `/`.
fn legend(app: &App, width: usize) -> Line {
    let theme = &app.theme;
    // `@` reaches both a file snapshot and a delegate agent, and says so.
    let context = format!(
        "{} · {}",
        app.language.text("files"),
        app.language.text("agents")
    );
    let entries = [("/", "commands"), ("@", context.as_str()), ("!", "shell")];
    let mut spans = vec![Span::new(" ".repeat(INSET), theme.muted)];
    let mut used = INSET;
    for (key, label) in entries {
        let label = app.language.text(label);
        let cost = key.width() + label.width() + 4;
        if used + cost > width {
            break;
        }
        if used > INSET {
            spans.push(Span::new("   ", theme.muted));
            used += 3;
        }
        spans.push(Span::new(key, theme.accent).with_attributes(attr::BOLD));
        spans.push(Span::new(format!(" {label}"), theme.muted));
        used += key.width() + label.width() + 1;
    }
    Line::new(spans)
}

/// A newer Haarness, and the command that installs it, where the eye lands
/// before a first prompt. There is no slash command for it: updating
/// replaces the program this screen runs in.
fn update_line(app: &App, width: usize, version: &str) -> Line {
    const COMMAND: &str = "haarness --update";
    let theme = &app.theme;
    let sentence = app
        .language
        .format("Haarness {version} is available.", &[("version", version)]);
    let mut spans = vec![
        Span::new(" ".repeat(INSET), theme.muted),
        Span::new("↑ ", theme.accent).with_attributes(attr::BOLD),
    ];
    let room = width.saturating_sub(INSET + 2);
    spans.push(Span::new(single_line(&sentence, room), theme.muted));
    // The command, where it fits whole; the sentence alone otherwise.
    if sentence.width() + "  ·  ".width() + COMMAND.width() <= room {
        spans.push(Span::new("  ·  ", theme.muted));
        spans.push(Span::new(COMMAND, theme.accent).with_attributes(attr::BOLD));
    }
    Line::new(spans)
}

fn indented(text: String, color: haarness_opentui::Color) -> Line {
    Line::new(vec![
        Span::new(" ".repeat(INSET), color),
        Span::new(text, color),
    ])
}
