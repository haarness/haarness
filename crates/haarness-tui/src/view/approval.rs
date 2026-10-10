//! The docked approval panel: a permission prompt as a visible card pinned
//! above the composer, not a hint buried in the status band.
//!
//! Same accent rail the user's own messages carry, raised on the surface
//! color. Each row earns its place by saying something the others do not:
//! what kind of thing is being asked, the one subject it applies to, and the
//! rule an "always" would persist — shown only when that rule is wider than
//! the subject itself. The chips carry their own hotkeys and are answered by
//! arrows + Enter, by the letter, or by mouse; they wrap onto more rows
//! rather than lose one off the edge. The answers that reach past this one
//! request ("always" rules, and Once + Auto) keep a two-press confirmation:
//! the chips swap to confirm/cancel while staged, and the row above them says
//! exactly what is about to be granted.

use haarness_opentui::{Line, Span, attr};
use haarness_protocol::PendingPermissionInfo;
use unicode_width::UnicodeWidthStr as _;

use crate::locale::Language;
use crate::state::{App, PermissionChoice};
use crate::theme::Theme;
use crate::view::frame::{HitTarget, TaggedLines};
use crate::view::span::single_line;

/// Context rows the panel gives a long command before eliding.
const MAX_CONTEXT_ROWS: usize = 4;
/// Rows of a proposed file change shown before eliding the rest.
const MAX_CHANGE_ROWS: usize = 8;
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

/// What kind of thing is being asked for, in words (an English catalog key).
/// The wire's `operation` is a machine rule — `bash:git push origin main` —
/// whose subject half the panel already prints verbatim on the row below, so
/// naming the category here is what stops the card from saying the same
/// string twice.
fn headline(category: &str) -> &str {
    match category {
        "bash" => "Run a shell command",
        "edit" => "Edit a file",
        "read" => "Read a file",
        "external_directory" => "Access outside the project",
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
            let previous = details["previousModel"]
                .as_str()
                .filter(|previous| *previous != model);
            let mut text = format!("Agent: {agent}\nModel: {model}");
            if let Some(previous) = previous {
                text.push_str("\nPrevious model: ");
                text.push_str(previous);
            }
            if let Some(reason) = pending.detail["reason"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
            {
                text.push_str("\nReason: ");
                text.push_str(reason);
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
fn always_rules(
    pending: &PendingPermissionInfo,
    subject: Option<&str>,
    language: Language,
) -> Vec<String> {
    // The server keys a remembered switch by chat as well as agent.
    if let Some(agent) = pending.detail["__agentSwitch"]["agent"]
        .as_str()
        .filter(|_| category(pending) == "agent_switch")
    {
        return vec![
            language
                .text("only agent {agent} in this chat")
                .replace("{agent}", agent),
        ];
    }
    let prefix = format!("{}:", category(pending));
    pending
        .always_patterns
        .iter()
        .map(|rule| rule.strip_prefix(&prefix).unwrap_or(rule).to_owned())
        .filter(|rule| Some(rule.as_str()) != subject)
        // A bare `*` is the whole category; say so rather than print a glob.
        .map(|rule| {
            if rule == "*" {
                match category(pending) {
                    "edit" => language.text("every file edit").into(),
                    "bash" => language.text("every shell command").into(),
                    "read" => language.text("every file read").into(),
                    "webfetch" | "fetch" => language.text("every URL fetch").into(),
                    other if !other.is_empty() => language
                        .text("every {kind} request")
                        .replace("{kind}", other),
                    _ => rule,
                }
            } else {
                rule
            }
        })
        .collect()
}

/// The request in one line of plain words, such as "Access outside the
/// project: /tmp/notes.txt", for the places that repeat the card: the status
/// band, the desktop banner and the toast shown while the card is hidden.
/// Never the wire's `kind:subject` rule.
pub fn summary(pending: &PendingPermissionInfo, language: Language) -> String {
    let headline = language.text(headline(category(pending)));
    match subject(pending) {
        Some(subject) => format!("{headline}: {}", subject.lines().next().unwrap_or_default()),
        None => headline.to_owned(),
    }
}

/// What a staged answer is about to grant, as an English catalog key. An
/// "always" rule lives in the server: Always lasts until that server stops
/// and covers every chat it runs, while This folder and Repository are saved
/// and outlast restarts. Once + Auto changes how this whole chat is answered.
pub fn staged_lead(choice: char, pending: &PendingPermissionInfo) -> &'static str {
    // A remembered agent switch belongs to its own chat; the rule row says so.
    let one_chat = category(pending) == "agent_switch";
    match choice {
        'u' => "allow once and switch this chat to Auto: later requests are allowed without asking",
        'r' => "always allow in this repository and its worktrees",
        'w' if one_chat => "always allow, kept across restarts",
        'w' => "always allow in every chat in this folder, kept across restarts",
        _ if one_chat => "always allow until the Haarness server stops",
        _ => "always allow in every chat until the Haarness server stops",
    }
}

/// The card, fitted to `max_rows` when the terminal is short: rows that only
/// add detail give way first (the change preview, the spacer, the "always
/// allows" row, the subject's later rows, the key hints), while the title,
/// the subject's first row, what a staged answer grants and every choice
/// stay on screen.
pub fn approval_lines(app: &App, width: usize, max_rows: usize) -> TaggedLines {
    let mut panel = TaggedLines::default();
    let Some(pending) = app.first_pending_permission() else {
        return panel;
    };
    let theme = &app.theme;
    let language = app.language;
    let focused = app.permission_focus;
    let inner = width.saturating_sub(RAIL.width() + 2).max(20);
    let staged = app.confirm_always.filter(|_| focused);

    let category = category(pending);
    let subject = subject(pending);
    let rules = always_rules(pending, subject.as_deref(), language);

    // The thing being approved, verbatim: the command for bash, the path for
    // file operations, the url for fetches. It stays on screen while an
    // answer is staged — that is precisely the moment you want to re-read
    // what you are about to grant.
    let mut subject_rows = Vec::new();
    for paragraph in subject.iter().flat_map(|subject| subject.lines()) {
        for wrapped in textwrap::wrap(paragraph, inner.saturating_sub(2).max(10)) {
            subject_rows.push(wrapped.into_owned());
        }
    }
    // An edit is approved for what it changes, not for its file name.
    let change = change_preview(pending, inner.saturating_sub(2).max(10));
    // Staged, name exactly what the answer grants. The rule itself only earns
    // a row where it reaches wider than the subject already shown; with none
    // to list, the sentence simply lands on the subject above it.
    let lead_rows = staged.map_or_else(Vec::new, |choice| {
        let lead = language.text(staged_lead(choice, pending));
        let lead = if rules.is_empty() || choice == 'u' {
            lead.to_owned()
        } else {
            format!("{lead}:")
        };
        textwrap::wrap(&lead, inner)
            .into_iter()
            .map(std::borrow::Cow::into_owned)
            .collect()
    });
    let staged_rules = match staged {
        Some('u') | None => &[][..],
        Some(_) => &rules[..],
    };
    let allows = (staged.is_none() && !rules.is_empty()).then(|| {
        let label = language.text("always allows");
        format!(
            "{label}: {}",
            single_line(&rules.join("  "), inner.saturating_sub(label.width() + 2))
        )
    });
    let chips = chip_rows(app, width, staged.is_some());
    let footer = footer(app, width.saturating_sub(RAIL.width()), staged.is_some());

    // Spend the rows beyond the essentials, most useful first.
    let essential = 1 + usize::from(!subject_rows.is_empty()) + lead_rows.len() + chips.len();
    let mut room = max_rows.saturating_sub(essential);
    let mut take = |wanted: usize| {
        let granted = wanted.min(room);
        room -= granted;
        granted
    };
    let rule_count = take(staged_rules.len().min(MAX_CONTEXT_ROWS));
    let show_footer = take(usize::from(!footer.is_empty())) == 1;
    let wanted_subject = subject_rows.len().min(MAX_CONTEXT_ROWS)
        + usize::from(subject_rows.len() > MAX_CONTEXT_ROWS);
    let subject_count = (1 + take(wanted_subject.saturating_sub(1))).min(subject_rows.len());
    let show_allows = take(usize::from(allows.is_some())) == 1;
    let show_spacer = take(1) == 1;
    // A preview needs its blank row and at least one changed line.
    let mut change_count = 0;
    if !change.is_empty() && room >= 2 {
        change_count = change.len().min(MAX_CHANGE_ROWS).min(room - 1);
        if change_count < change.len() && change_count + 2 > room {
            change_count -= 1;
        }
    }

    // Title: what kind of thing is being asked, plus the queue position when
    // prompts are stacked.
    let position = (app.pending_permissions.len() > 1).then(|| {
        format!(
            "  ·  {}",
            language
                .text("1 of {count}")
                .replace("{count}", &app.pending_permissions.len().to_string())
        )
    });
    let mut title = vec![
        Span::new("⚠ ", theme.accent)
            .on(theme.overlay)
            .with_attributes(attr::BOLD),
        Span::new(
            single_line(
                language.text(headline(category)),
                inner.saturating_sub(position.as_ref().map_or(0, |text| text.width())),
            ),
            theme.text,
        )
        .on(theme.overlay)
        .with_attributes(attr::BOLD),
    ];
    if let Some(position) = position {
        title.push(Span::new(position, theme.muted).on(theme.overlay));
    }
    push_row(&mut panel, title, width, theme, focused);

    if !subject_rows.is_empty() {
        let command = pending.detail.get("command").is_some();
        let truncated = subject_rows.len() > subject_count.min(MAX_CONTEXT_ROWS);
        // A truncated subject ends on its own "…" row, unless the terminal
        // only has room for one, which then ends in the ellipsis itself.
        let (rows, ellipsis_row) = if !truncated {
            (subject_count, false)
        } else if subject_count >= 2 {
            ((subject_count - 1).min(MAX_CONTEXT_ROWS), true)
        } else {
            (1, false)
        };
        for (index, row) in subject_rows.iter().take(rows).enumerate() {
            let prefix = if command && index == 0 { "$ " } else { "  " };
            let text = if truncated && !ellipsis_row {
                format!(
                    "{prefix}{}",
                    single_line(&format!("{row} …"), inner.saturating_sub(2))
                )
            } else {
                format!("{prefix}{row}")
            };
            push_row(
                &mut panel,
                vec![Span::new(text, theme.text).on(theme.overlay)],
                width,
                theme,
                focused,
            );
        }
        if ellipsis_row {
            push_row(
                &mut panel,
                vec![Span::new("  …", theme.muted).on(theme.overlay)],
                width,
                theme,
                focused,
            );
        }
    }

    if change_count > 0 {
        push_row(&mut panel, Vec::new(), width, theme, focused);
        let hidden = change.len() - change_count;
        for (marker, text) in change.into_iter().take(change_count) {
            let (fg, bg) = match marker {
                '+' => (theme.diff.added, theme.diff.added_bg),
                '-' => (theme.diff.removed, theme.diff.removed_bg),
                _ => (theme.muted, theme.overlay),
            };
            let row = format!("{marker} {text}");
            let pad = inner.saturating_sub(row.width());
            push_row(
                &mut panel,
                vec![
                    Span::new("  ", theme.text).on(theme.overlay),
                    Span::new(row, fg).on(bg),
                    Span::new(" ".repeat(pad), fg).on(bg),
                ],
                width,
                theme,
                focused,
            );
        }
        if hidden > 0 {
            push_row(
                &mut panel,
                vec![
                    Span::new(
                        format!(
                            "  … {}",
                            language
                                .text("{count} more lines")
                                .replace("{count}", &hidden.to_string())
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
    }

    for lead in lead_rows {
        push_row(
            &mut panel,
            vec![Span::new(lead, theme.muted).on(theme.overlay)],
            width,
            theme,
            focused,
        );
    }
    for rule in staged_rules.iter().take(rule_count) {
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
    if let Some(allows) = allows.filter(|_| show_allows) {
        push_row(
            &mut panel,
            vec![Span::new(allows, theme.muted).on(theme.overlay)],
            width,
            theme,
            focused,
        );
    }

    if show_spacer {
        push_row(&mut panel, Vec::new(), width, theme, focused);
    }
    for (line, targets) in chips {
        panel.push_ranged(line, targets);
    }
    // Staged, the two chips are already labelled Confirm and Cancel, and each
    // carries its key. A row restating them in muted text is the same
    // sentence twice.
    if show_footer {
        push_row(&mut panel, footer, width, theme, focused);
    }
    panel
}

/// The lines a file-changing request would add and remove, as `(marker,
/// text)` pairs clipped to `width`: an `edit`'s old/new text, an
/// `apply_patch` envelope's hunks, or the opening of a `write`.
fn change_preview(pending: &PendingPermissionInfo, width: usize) -> Vec<(char, String)> {
    let detail = &pending.detail;
    let text = |key: &str| detail.get(key).and_then(serde_json::Value::as_str);
    let clip = |line: &str| single_line(line.trim_end(), width.saturating_sub(2));
    if let (Some(old), Some(new)) = (text("old"), text("new")) {
        let diff = similar::TextDiff::from_lines(old, new);
        return diff
            .iter_all_changes()
            .map(|change| {
                let marker = match change.tag() {
                    similar::ChangeTag::Delete => '-',
                    similar::ChangeTag::Insert => '+',
                    similar::ChangeTag::Equal => ' ',
                };
                (marker, clip(change.value()))
            })
            .collect();
    }
    if let Some(patch) = text("input")
        .or_else(|| text("patch"))
        .filter(|patch| patch.trim_start().starts_with("*** Begin Patch"))
    {
        return patch
            .lines()
            .filter_map(|line| {
                let mut chars = line.chars();
                match chars.next() {
                    Some(marker @ ('+' | '-')) => Some((marker, clip(chars.as_str()))),
                    _ => None,
                }
            })
            .collect();
    }
    if category(pending) == "edit" {
        if let Some(content) = text("content") {
            return content.lines().map(|line| ('+', clip(line))).collect();
        }
    }
    Vec::new()
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
            (app.language.text("Confirm").to_owned(), "enter".into()),
            (app.language.text("Cancel").to_owned(), "esc".into()),
        ];
    }
    PermissionChoice::available(app)
        .iter()
        .map(|choice| {
            (
                app.language.text(choice.label()).to_owned(),
                choice.key().to_string(),
            )
        })
        .collect()
}

fn chip_width(label: &str, key: &str) -> usize {
    format!(" {label}  {key} ").width()
}

/// A clickable chip row and its column ranges.
type ChipRow = (Line, Vec<(usize, usize, HitTarget)>);

/// The chips in as many rows as the width needs, each chip its own click
/// target: a narrow terminal wraps the choices instead of cutting Reject off
/// the edge. The selection is painted only while the prompt owns the
/// keyboard, so an unfocused panel never claims the next Enter.
fn chip_rows(app: &App, width: usize, staged: bool) -> Vec<ChipRow> {
    let theme = &app.theme;
    let chips = chips(app, staged);
    let selection = app.permission_selection.min(chips.len() - 1);
    let start = || vec![Span::new(RAIL, theme.accent).on(theme.overlay)];
    let finish = |mut spans: Vec<Span>, column: usize| {
        spans.push(
            Span::new(" ".repeat(width.saturating_sub(column)), theme.text).on(theme.overlay),
        );
        Line::new(spans)
    };
    let mut rows = Vec::new();
    let mut spans = start();
    let mut targets = Vec::new();
    let mut column = RAIL.width();
    for (index, (label, key)) in chips.iter().enumerate() {
        // A label too long for the row on its own is cut rather than drawn
        // past the edge.
        let label = single_line(
            label,
            width.saturating_sub(RAIL.width() + key.width() + 4).max(1),
        );
        let chip = chip_width(&label, key);
        if column > RAIL.width() {
            if column + 2 + chip > width {
                rows.push((
                    finish(std::mem::replace(&mut spans, start()), column),
                    std::mem::take(&mut targets),
                ));
                column = RAIL.width();
            } else {
                spans.push(Span::new("  ", theme.muted).on(theme.overlay));
                column += 2;
            }
        }
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
        targets.push((column, column + chip, HitTarget::PermissionOption(index)));
        column += chip;
    }
    rows.push((finish(spans, column), targets));
    rows
}

/// Key hints, two-tone: keys in body text, what they do in muted. Only what
/// the chips cannot say for themselves — staged, they say all of it, and this
/// row goes away rather than repeat them. Hints that do not fit `width` drop
/// from the end.
fn footer(app: &App, width: usize, staged: bool) -> Vec<Span> {
    let theme = &app.theme;
    let surface = theme.overlay;
    let pairs: &[(&str, &str)] = if !app.permission_focus {
        &[("ctrl+g", "answer"), ("click", "choose")]
    } else if staged {
        &[]
    } else {
        &[("←→", "select"), ("enter", "choose"), ("esc", "reject")]
    };
    let mut count = pairs.len();
    loop {
        let mut spans = Vec::new();
        for (index, (key, action)) in pairs.iter().take(count).enumerate() {
            if index > 0 {
                spans.push(Span::new(" · ", theme.muted).on(surface));
            }
            spans.push(Span::new((*key).to_owned(), theme.text).on(surface));
            spans.push(
                Span::new(format!(" {}", app.language.text(action)), theme.muted).on(surface),
            );
        }
        if count <= 1 || spans.iter().map(Span::width).sum::<usize>() <= width {
            return spans;
        }
        count -= 1;
    }
}
