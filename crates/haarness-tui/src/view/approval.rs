//! The docked approval panel: a permission prompt as a visible card pinned
//! above the composer, not a hint buried in the status band.
//!
//! Same accent rail the user's own messages carry, raised on the surface
//! color. Each row earns its place by saying something the others do not:
//! what kind of thing is being asked, the one subject it applies to, and the
//! rule an "always" would persist — shown only when that rule is wider than
//! the subject itself. The chips carry their own hotkeys and are answered by
//! arrows + Enter, by the letter, or by mouse. The "always" variants keep
//! their two-press confirmation: the chips swap to confirm/cancel while
//! staged, and speak for themselves rather than being narrated again below.

use haarness_opentui::{Line, Span, attr};
use haarness_protocol::PendingPermissionInfo;
use unicode_width::UnicodeWidthStr as _;

use crate::state::{App, PermissionChoice};
use crate::theme::Theme;
use crate::view::frame::{HitTarget, TaggedLines};
use crate::view::span::single_line;

/// Context rows the panel gives a long command before eliding.
const MAX_CONTEXT_ROWS: usize = 4;
/// The same rail glyph user messages carry; here in the accent color.
const RAIL: &str = "▎ ";

/// The permission category, from the structured field when the server sends
/// one and otherwise recovered from the `kind:subject` operation string that
/// older events carry.
fn category(pending: &PendingPermissionInfo) -> &str {
    if !pending.permission.is_empty() {
        return &pending.permission;
    }
    pending
        .operation
        .split_once(':')
        .map_or("", |(kind, _)| kind)
}

/// What kind of thing is being asked for, in words. The wire's `operation` is
/// a machine rule — `bash:git push origin main` — whose subject half the panel
/// already prints verbatim on the row below, so naming the category here is
/// what stops the card from saying the same string twice.
fn headline(category: &str) -> &str {
    match category {
        "bash" => "Run a shell command",
        "edit" => "Edit a file",
        "read" => "Read a file",
        "external_directory" => "Reach outside the workspace",
        "browser" => "Use the browser",
        "browser_site" => "Allow site access",
        "multiple" => "Allow browser and file access",
        "webfetch" | "fetch" => "Fetch a URL",
        "workflow" => "Run a workflow",
        "agent_switch" => "Switch the active agent",
        "doom_loop" => "Keep repeating the same action",
        // A malformed request must still produce a card that reads as a
        // question rather than a bare warning glyph.
        "" => "Permission needed",
        // An unknown category is still better named than unnamed: the server
        // may grow one before this list does.
        other => other,
    }
}

/// The one thing being approved: the command, path, or url the server sent as
/// detail, else the raw patterns, else the operation string's subject half.
fn subject(pending: &PendingPermissionInfo) -> Option<String> {
    if category(pending) == "agent_switch" {
        let details = &pending.detail["__agentSwitch"];
        if let (Some(agent), Some(model)) = (details["agent"].as_str(), details["model"].as_str()) {
            let previous = details["previousModel"].as_str().filter(|previous| *previous != model);
            let mut text = format!("Agent: {agent}\nModel: {model}");
            if let Some(previous) = previous { text.push_str(&format!("\nPrevious model: {previous}")); }
            if let Some(reason) = pending.detail["reason"].as_str().filter(|value| !value.trim().is_empty()) {
                text.push_str(&format!("\nReason: {reason}"));
            }
            return Some(text);
        }
    }
    for key in ["command", "path", "url"] {
        if let Some(value) = pending
            .detail
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            return Some(value.to_owned());
        }
    }
    if !pending.patterns.is_empty() {
        return Some(pending.patterns.join("  "));
    }
    // An operation with no category prefix is itself the subject; dropping it
    // would leave the card with nothing to show.
    let operation = pending.operation.trim();
    Some(
        operation
            .split_once(':')
            .map_or(operation, |(_, rest)| rest.trim())
            .to_owned(),
    )
    .filter(|subject| !subject.is_empty())
}

/// The rules an "always" would persist, worth a row only where they say more
/// than the subject already did. The stored rule carries a `category:` prefix
/// that the headline has just spelled out in words, so it comes off here.
fn always_rules(pending: &PendingPermissionInfo, subject: Option<&str>) -> Vec<String> {
    if category(pending) == "agent_switch" && pending.detail["__agentSwitch"]["agent"].is_string() {
        return vec![format!("only agent {} in this session", pending.detail["__agentSwitch"]["agent"].as_str().unwrap())];
    }
    let prefix = format!("{}:", category(pending));
    pending
        .always_patterns
        .iter()
        .map(|rule| rule.strip_prefix(&prefix).unwrap_or(rule).to_owned())
        .filter(|rule| Some(rule.as_str()) != subject)
        .collect()
}

pub fn approval_lines(app: &App, width: usize) -> TaggedLines {
    let mut panel = TaggedLines::default();
    let Some(pending) = app.first_pending_permission() else {
        return panel;
    };
    let theme = &app.theme;
    let focused = app.permission_focus;
    let inner = width.saturating_sub(RAIL.width() + 2).max(20);
    let staged = app.confirm_always.filter(|_| focused);

    let category = category(pending);
    let subject = subject(pending);
    let rules = always_rules(pending, subject.as_deref());

    // Title: what kind of thing is being asked, plus the queue position when
    // prompts are stacked.
    let mut title = vec![
        Span::new("⚠ ", theme.accent)
            .on(theme.overlay)
            .with_attributes(attr::BOLD),
        Span::new(
            single_line(headline(category), inner.saturating_sub(14)),
            theme.text,
        )
        .on(theme.overlay)
        .with_attributes(attr::BOLD),
    ];
    if app.pending_permissions.len() > 1 {
        title.push(
            Span::new(
                format!("  ·  1 of {}", app.pending_permissions.len()),
                theme.muted,
            )
            .on(theme.overlay),
        );
    }
    push_row(&mut panel, title, width, theme, focused);

    // The thing being approved, verbatim: the command for bash, the path for
    // file operations, the url for fetches. It stays on screen while an
    // "always" is staged — that is precisely the moment you want to re-read
    // what you are about to grant standing permission for.
    if let Some(subject) = &subject {
        let command = pending.detail.get("command").is_some();
        let mut rows = Vec::new();
        for paragraph in subject.lines() {
            for wrapped in textwrap::wrap(paragraph, inner.saturating_sub(2).max(10)) {
                rows.push(wrapped.into_owned());
            }
        }
        let truncated = rows.len() > MAX_CONTEXT_ROWS;
        rows.truncate(MAX_CONTEXT_ROWS);
        for (index, row) in rows.iter().enumerate() {
            let prefix = match (command, index) {
                (true, 0) => "$ ",
                _ => "  ",
            };
            push_row(
                &mut panel,
                vec![Span::new(format!("{prefix}{row}"), theme.text).on(theme.overlay)],
                width,
                theme,
                focused,
            );
        }
        if truncated {
            push_row(
                &mut panel,
                vec![Span::new("  …", theme.muted).on(theme.overlay)],
                width,
                theme,
                focused,
            );
        }
    }

    if let Some(choice) = staged {
        // Confirm stage: name the scope the rule is about to enter. The rule
        // itself only earns a row where it reaches wider than the subject
        // already shown; otherwise saying so would print that string twice.
        let scope = if choice == 'r' { "this repository and its worktrees" } else if choice == 'w' {
            "this folder"
        } else {
            "this session"
        };
        // With no wider rule to list, the sentence simply lands on the
        // subject above it; the colon is what introduces one.
        let lead = if rules.is_empty() {
            format!("always allow for {scope}")
        } else {
            format!("always allow for {scope}:")
        };
        push_row(
            &mut panel,
            vec![Span::new(lead, theme.muted).on(theme.overlay)],
            width,
            theme,
            focused,
        );
        for rule in rules.iter().take(MAX_CONTEXT_ROWS) {
            push_row(
                &mut panel,
                vec![
                    Span::new(
                        format!("  {}", single_line(rule, inner.saturating_sub(2))),
                        theme.text,
                    )
                    .on(theme.overlay),
                ],
                width,
                theme,
                focused,
            );
        }
    } else if !rules.is_empty() {
        push_row(
            &mut panel,
            vec![
                Span::new(
                    format!(
                        "always allows: {}",
                        single_line(&rules.join("  "), inner.saturating_sub(15))
                    ),
                    theme.muted,
                )
                .on(theme.overlay),
            ],
            width,
            theme,
            focused,
        );
    }

    push_row(&mut panel, Vec::new(), width, theme, focused);
    panel.push_ranged(
        chip_row(app, width, staged.is_some()),
        chip_targets(app, staged.is_some()),
    );
    // Staged, the two chips are already labelled Confirm and Cancel, and each
    // carries its key. A row restating them in muted text is the same
    // sentence twice.
    let footer = footer(app, staged.is_some());
    if !footer.is_empty() {
        push_row(&mut panel, footer, width, theme, focused);
    }
    panel
}

/// One panel row: accent rail, content, surface background to the edge.
/// Unfocused rows are tagged so a click anywhere on the card gives the
/// prompt the keyboard.
fn push_row(
    panel: &mut TaggedLines,
    mut spans: Vec<Span>,
    width: usize,
    theme: &Theme,
    focused: bool,
) {
    let mut row = vec![Span::new(RAIL, theme.accent).on(theme.overlay)];
    row.append(&mut spans);
    let used: usize = row.iter().map(Span::width).sum();
    row.push(Span::new(" ".repeat(width.saturating_sub(used)), theme.text).on(theme.overlay));
    let line = Line::new(row);
    if focused {
        panel.push(line);
    } else {
        panel.push_tagged(line, HitTarget::PermissionFocus);
    }
}

/// Each chip and the key that answers it. Carrying the key on the chip is
/// what lets the footer drop a bare `y/a/w/n` that never said which letter
/// belonged to which answer.
fn chips(app: &App, staged: bool) -> Vec<(String, String)> {
    if staged {
        return vec![
            ("Confirm".into(), "enter".into()),
            ("Cancel".into(), "esc".into()),
        ];
    }
    PermissionChoice::available(app)
        .iter()
        .map(|choice| (choice.label().to_owned(), choice.key().to_string()))
        .collect()
}

fn chip_width(label: &str, key: &str) -> usize {
    format!(" {label}  {key} ").width()
}

/// The chips as one line, selection painted only while the prompt owns the
/// keyboard so an unfocused panel never claims the next Enter.
fn chip_row(app: &App, width: usize, staged: bool) -> Line {
    let theme = &app.theme;
    let chips = chips(app, staged);
    let selection = app.permission_selection.min(chips.len() - 1);
    let mut spans = vec![Span::new(RAIL, theme.accent).on(theme.overlay)];
    for (index, (label, key)) in chips.iter().enumerate() {
        let selected = app.permission_focus && index == selection;
        let background = if selected {
            theme.accent
        } else {
            theme.selected
        };
        let (foreground, attributes) = match (selected, app.permission_focus) {
            (true, _) => (theme.bg, attr::BOLD),
            (false, true) => (theme.text, attr::NONE),
            (false, false) => (theme.muted, attr::NONE),
        };
        spans.push(
            Span::new(format!(" {label}  "), foreground)
                .on(background)
                .with_attributes(attributes),
        );
        // The key rides quieter than the answer it triggers, except on the
        // selected chip, which is inverted whole.
        spans.push(
            Span::new(
                format!("{key} "),
                if selected { theme.bg } else { theme.muted },
            )
            .on(background),
        );
        spans.push(Span::new("  ", theme.muted).on(theme.overlay));
    }
    let used: usize = spans.iter().map(Span::width).sum();
    spans.push(Span::new(" ".repeat(width.saturating_sub(used)), theme.text).on(theme.overlay));
    Line::new(spans)
}

/// Column ranges matching `chip_row`, so each chip is its own click target.
fn chip_targets(app: &App, staged: bool) -> Vec<(usize, usize, HitTarget)> {
    let mut targets = Vec::new();
    let mut column = RAIL.width();
    for (index, (label, key)) in chips(app, staged).iter().enumerate() {
        let width = chip_width(label, key);
        targets.push((column, column + width, HitTarget::PermissionOption(index)));
        column += width + 2;
    }
    targets
}

/// Key hints, two-tone: keys in body text, what they do in muted. Only what
/// the chips cannot say for themselves — staged, they say all of it, and this
/// row goes away rather than repeat them.
fn footer(app: &App, staged: bool) -> Vec<Span> {
    let theme = &app.theme;
    let surface = theme.overlay;
    let pairs: &[(&str, &str)] = if !app.permission_focus {
        &[("ctrl+g", "answer"), ("click", "a button")]
    } else if staged {
        &[]
    } else {
        &[("←→", "select"), ("enter", "choose"), ("esc", "reject")]
    };
    let mut spans = Vec::new();
    for (index, (key, action)) in pairs.iter().enumerate() {
        if index > 0 {
            spans.push(Span::new(" · ", theme.muted).on(surface));
        }
        spans.push(Span::new((*key).to_owned(), theme.text).on(surface));
        if !action.is_empty() {
            spans.push(Span::new(format!(" {action}"), theme.muted).on(surface));
        }
    }
    spans
}
