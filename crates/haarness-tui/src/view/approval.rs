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

use crate::width::DisplayWidth as _;
use haarness_opentui::{Line, Span, attr};
use haarness_protocol::PendingPermissionInfo;

use crate::locale::Language;
use crate::state::{App, PermissionChoice, SubjectFit};
use crate::theme::Theme;
use crate::view::frame::{HitTarget, TaggedLines};
use crate::view::span::single_line;

/// Rows the collapsed panel gives a long command; the rest scrolls or
/// expands into view.
const MAX_CONTEXT_ROWS: usize = 4;
/// A Chrome request is answered for the exact call it shows: its site lines
/// and then the call's own script, address or text, so it gets more room.
const MAX_CHROME_ROWS: usize = 10;
/// An argument longer than this is cut before it is wrapped; the card shows
/// far less of it anyway.
const MAX_ARGUMENT_CHARS: usize = 4000;
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
fn headline(pending: &PendingPermissionInfo) -> &str {
    if pending.explicit {
        return "Allow in Chrome";
    }
    match category(pending) {
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

/// One Chrome site gate in words. The server asks with a remember pattern,
/// `chrome-<category>:<thread id>:<origin>` (prefixed `browser_site:` when it
/// rides in a combined request), and the person should read what it lets the
/// agent do, not the rule.
fn chrome_access(pattern: &str, language: Language) -> Option<String> {
    let rest = pattern.strip_prefix("browser_site:").unwrap_or(pattern);
    let (category, rest) = rest.strip_prefix("chrome-")?.split_once(':')?;
    // The origin carries a colon of its own; the chat id never does.
    let origin = if rest.starts_with("http://") || rest.starts_with("https://") {
        rest
    } else {
        rest.split_once(':')?.1
    };
    let key = match category {
        "browsing" => "Use {origin} in Chrome",
        "downloads" => "Send downloads from {origin} in Chrome to the agent",
        "uploads" => "Upload files to {origin} in Chrome",
        "scripts" => "Run scripts on {origin} in Chrome",
        _ => return None,
    };
    Some(language.format(key, &[("origin", origin)]))
}

/// The call's own arguments, one `name: value` row each: the address it
/// opens, the script it runs, the text it types, the files it uploads. "Allow
/// once" approves exactly this call, so it is shown as sent, minus the
/// broker's `__browser` plumbing and the tab and frame ids, which name
/// nothing a person can check (the site lines say where).
fn chrome_arguments(detail: &serde_json::Value) -> Vec<String> {
    /// What a person reads first, ahead of the rest in the order sent.
    const FIRST: [&str; 4] = ["url", "script", "text", "paths"];
    let Some(arguments) = detail.as_object() else {
        return Vec::new();
    };
    let mut names: Vec<&String> = arguments
        .keys()
        .filter(|name| !matches!(name.as_str(), "__browser" | "tabID" | "frameID"))
        .collect();
    names.sort_by_key(|name| {
        FIRST
            .iter()
            .position(|first| first == name)
            .unwrap_or(FIRST.len())
    });
    let mut rows = Vec::new();
    for name in names {
        let value = match &arguments[name] {
            serde_json::Value::Null => continue,
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(items) if items.iter().all(serde_json::Value::is_string) => {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            }
            other => other.to_string(),
        };
        let mut value = value.trim().to_owned();
        if let Some((cut, _)) = value.char_indices().nth(MAX_ARGUMENT_CHARS) {
            value.truncate(cut);
            value.push_str(" …");
        }
        // A script keeps its own lines, under its name; blank ones would
        // only spend the card's rows.
        let mut lines = value.lines().filter(|line| !line.trim().is_empty());
        rows.push(format!("{name}: {}", lines.next().unwrap_or_default()));
        rows.extend(lines.take(MAX_CHROME_ROWS).map(|line| format!("  {line}")));
    }
    rows
}

/// What a Chrome request lets the agent do, one line per site and kind of
/// access, followed by anything else it asks alongside (a file it would
/// upload) and then the call itself.
fn chrome_subject(pending: &PendingPermissionInfo, language: Language) -> Option<String> {
    let mut lines = Vec::new();
    let mut other = Vec::new();
    for pattern in &pending.patterns {
        // A navigation asks for the site and its `www.` twin together, so
        // one answer covers both; the twin is the same site to a person.
        let bare = pattern.replacen("://www.", "://", 1);
        if bare != *pattern && pending.patterns.contains(&bare) {
            continue;
        }
        if let Some(line) = chrome_access(pattern, language) {
            if !lines.contains(&line) {
                lines.push(line);
            }
        } else if !pattern.starts_with("browser:") && !pattern.starts_with("browser_site:") {
            other.push(pattern.clone());
        }
    }
    if lines.is_empty() {
        return None;
    }
    lines.extend(other);
    lines.extend(chrome_arguments(&pending.detail));
    Some(lines.join("\n"))
}

/// The one thing being approved: the command, path, or url the server sent as
/// detail, else the raw patterns, else the operation string's subject half.
fn subject(pending: &PendingPermissionInfo, language: Language) -> Option<String> {
    if pending.explicit
        && let Some(subject) = chrome_subject(pending, language)
    {
        return Some(subject);
    }
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
            // A path that a link or `..` takes elsewhere is also asked about
            // where it leads; name that file, since it is what gets opened.
            let mut text = value.to_owned();
            if key == "path" {
                for target in link_targets(pending) {
                    text.push('\n');
                    text.push_str(&language.text("Leads to {path}").replace("{path}", target));
                }
            }
            return Some(text);
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

/// Files a read or edit is asked about besides the path the tool names: where
/// a link or `..` in that path leads.
fn link_targets(pending: &PendingPermissionInfo) -> impl Iterator<Item = &str> {
    let path = pending
        .detail
        .get("path")
        .and_then(serde_json::Value::as_str);
    let file = matches!(category(pending), "read" | "edit");
    pending
        .patterns
        .iter()
        .map(String::as_str)
        .filter(move |pattern| file && path.is_some_and(|path| path != *pattern))
}

/// The rules an "always" would persist, worth a row only where they say more
/// than the subject already did. The stored rule carries a `category:` prefix
/// that the headline has just spelled out in words, so it comes off here.
fn always_rules(
    pending: &PendingPermissionInfo,
    subject: Option<&str>,
    language: Language,
) -> Vec<String> {
    // Allowing a Chrome site in this chat covers exactly the lines shown;
    // the chip says so, and the stored rule is plumbing.
    if pending.explicit {
        return Vec::new();
    }
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
        .filter(|rule| Some(rule.as_str()) != subject.and_then(|subject| subject.lines().next()))
        // A link's target already has its own subject row.
        .filter(|rule| !link_targets(pending).any(|target| target == rule))
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
    let headline = language.text(headline(pending));
    match subject(pending, language) {
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
/// stay on screen. A subject longer than its rows scrolls (↑↓, page keys)
/// and expands (ctrl+e) inside the card, so the whole command can be read
/// before it is answered.
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

    let subject = subject(pending, language);
    let rules = always_rules(pending, subject.as_deref(), language);
    let subject_cap = if pending.explicit {
        MAX_CHROME_ROWS
    } else {
        MAX_CONTEXT_ROWS
    };

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
    let (scrolled, expanded) = app.permission_scroll_of(&pending.permission_id);
    // Expanded, the subject may take every row the card can spare.
    let cap = if expanded {
        subject_rows.len().max(subject_cap)
    } else {
        subject_cap
    };

    // Spend the rows beyond the essentials, most useful first.
    let essential = 1 + usize::from(!subject_rows.is_empty()) + lead_rows.len() + chips.len();
    let mut room = max_rows.saturating_sub(essential);
    let rule_count = staged_rules.len().min(MAX_CONTEXT_ROWS).min(room);
    room -= rule_count;
    let footer_rows = usize::from(!footer.is_empty());
    // A subject longer than the rows it gets scrolls, and a row under it says
    // which part is on screen and which keys move it. That row comes before
    // the footer: the chips name their own keys, but nothing else says the
    // command goes on past what is shown.
    let overflows = subject_rows.len() > cap
        || subject_rows.len().saturating_sub(1) > room.saturating_sub(footer_rows);
    let show_scroll =
        !subject_rows.is_empty() && (overflows || expanded) && spend(&mut room, 1) == 1;
    let show_footer = spend(&mut room, footer_rows) == 1;
    let subject_count = if subject_rows.is_empty() {
        0
    } else {
        1 + spend(&mut room, subject_rows.len().min(cap) - 1)
    };
    let offset = if show_scroll {
        scrolled.min(subject_rows.len() - subject_count)
    } else {
        0
    };
    let fit = SubjectFit {
        max_offset: if show_scroll {
            subject_rows.len() - subject_count
        } else {
            0
        },
        shown: subject_count,
        // Collapsed at its cap with rows to spare: expanding shows more.
        expandable: show_scroll
            && !expanded
            && subject_count < subject_rows.len()
            && subject_count == subject_cap
            && room > 0,
    };
    app.permission_fit.set(fit);
    let show_allows = spend(&mut room, usize::from(allows.is_some())) == 1;
    let show_spacer = spend(&mut room, 1) == 1;
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
                language.text(headline(pending)),
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
        // With no row to say how to scroll, a subject cut short ends in an
        // ellipsis on the one row the terminal has for it.
        let truncated = !show_scroll && subject_count < subject_rows.len();
        for (index, row) in subject_rows
            .iter()
            .enumerate()
            .skip(offset)
            .take(subject_count)
        {
            let prefix = if command && index == 0 { "$ " } else { "  " };
            let text = if truncated {
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
        if show_scroll {
            let hidden = subject_count < subject_rows.len();
            let mut spans = vec![Span::new("  ", theme.muted).on(theme.overlay)];
            spans.extend(scroll_hint(
                app,
                hidden.then(|| (offset + 1, offset + subject_count, subject_rows.len())),
                fit.expandable,
                expanded,
                inner.saturating_sub(2),
            ));
            push_row(&mut panel, spans, width, theme, focused);
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
/// Give up to `wanted` of the card's spare rows; what is left stays in `room`.
fn spend(room: &mut usize, wanted: usize) -> usize {
    let granted = wanted.min(*room);
    *room -= granted;
    granted
}

/// The row under a subject that scrolls: which of its rows are on screen,
/// and, while the card has the keyboard, the keys that move it. Expanding is
/// offered only where it shows more, and an expanded subject always says how
/// to collapse. Hints that do not fit `width` drop from the end.
fn scroll_hint(
    app: &App,
    window: Option<(usize, usize, usize)>,
    expandable: bool,
    expanded: bool,
    width: usize,
) -> Vec<Span> {
    let theme = &app.theme;
    let surface = theme.overlay;
    let language = app.language;
    let mut pairs = Vec::new();
    if app.permission_focus {
        if window.is_some() {
            pairs.push(("↑↓", "scroll"));
        }
        if expanded {
            pairs.push(("ctrl+e", "collapse"));
        } else if expandable {
            pairs.push(("ctrl+e", "expand"));
        }
    }
    let position = window.map(|(from, to, total)| {
        single_line(
            &language.format(
                "lines {from}–{to} of {total}",
                &[
                    ("from", &from.to_string()),
                    ("to", &to.to_string()),
                    ("total", &total.to_string()),
                ],
            ),
            width,
        )
    });
    let mut count = pairs.len();
    loop {
        let mut spans = Vec::new();
        if let Some(position) = &position {
            spans.push(Span::new(position.clone(), theme.muted).on(surface));
        }
        for (key, action) in pairs.iter().take(count) {
            if !spans.is_empty() {
                spans.push(Span::new(" · ", theme.muted).on(surface));
            }
            spans.push(Span::new((*key).to_owned(), theme.text).on(surface));
            spans.push(Span::new(format!(" {}", language.text(action)), theme.muted).on(surface));
        }
        if count == 0 || spans.iter().map(Span::width).sum::<usize>() <= width {
            return spans;
        }
        count -= 1;
    }
}

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
    let explicit = app
        .first_pending_permission()
        .is_some_and(|pending| pending.explicit);
    PermissionChoice::available(app)
        .iter()
        .map(|choice| {
            (
                app.language.text(choice.label_for(explicit)).to_owned(),
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
    let explicit = app
        .first_pending_permission()
        .is_some_and(|pending| pending.explicit);
    let pairs: &[(&str, &str)] = if !app.permission_focus {
        &[("ctrl+g", "answer"), ("click", "choose")]
    } else if staged {
        &[]
    } else if explicit {
        // Escape answers the same as the Deny chip beside it.
        &[("←→", "select"), ("enter", "choose"), ("esc", "deny")]
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
