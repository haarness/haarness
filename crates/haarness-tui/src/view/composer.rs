//! The composer: a prompt that floats on the page, a hairline that carries
//! the system's pulse, and one raised instrument band pinned to the last row.
//!
//! There is deliberately no box, rail, or panel around the input. Structure
//! comes from two horizontal edges instead: the rule directly under what you
//! type (which breathes while a turn runs, and goes solid when the turn is
//! blocked on you) and the band beneath it that holds identity and state.

use crate::width::DisplayWidth as _;
use haarness_opentui::{Color, Line, Span, attr};
use haarness_protocol::ItemData;
use unicode_width::UnicodeWidthChar as _;

use crate::state::{App, InterruptTarget, ServerRestart};
use crate::theme::Theme;
use crate::view::frame::{HitTarget, TaggedLines};
use crate::view::span::{compact_count, single_line};

const MAX_INPUT_ROWS: usize = 6;
/// Left inset of everything in the composer. The band is the only full-bleed
/// row; the prompt itself keeps a margin so it reads as page content.
const INSET: usize = 2;

/// Messages the waiting strip names in full before collapsing into a count.
const MAX_QUEUED_ROWS: usize = 3;
/// The rail every strip row hangs from: the glyph the user's own prompts
/// carry, at column zero so the strip's text shares the prompt's column.
const RAIL: &str = "▎ ";

/// Follow-ups waiting on the agent, docked directly above the prompt. Two
/// kinds, kept apart by header and rail colour: steering the running work has
/// been handed but has not read yet, and queued messages that wait outside
/// the conversation for the current work to finish. Neither is conversation
/// content yet: steering takes its durable place in the transcript when a
/// `QueueConsumed` snapshot lands, a queued message when it runs.
///
/// Queued rows are the controls: the header and `+N more` open the full
/// list, a numbered row opens that message's actions, and the editing line
/// cancels an edit in progress.
pub fn queued_strip(app: &App, width: usize) -> TaggedLines {
    let theme = &app.theme;
    let language = app.language;
    let mut strip = TaggedLines::default();

    let steers: Vec<&str> = app
        .pending_follow_ups()
        .into_iter()
        .filter_map(|item| match &item.data {
            ItemData::UserMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if !steers.is_empty() {
        let rail = theme.primary;
        strip.push(strip_row(
            rail,
            header(
                language.text("Steering"),
                format!(
                    " · {} — {}",
                    steers.len(),
                    language.text("joins at the next step")
                ),
                theme.primary,
                theme,
            ),
            theme.surface,
            width,
            theme,
        ));
        for (index, text) in steers.iter().take(MAX_QUEUED_ROWS).enumerate() {
            strip.push(numbered_row(
                index + 1,
                &crate::input_queue::queued_label(text),
                rail,
                false,
                width,
                theme,
            ));
        }
        if let Some(more) = more_row(steers.len(), rail, width, theme) {
            strip.push(more);
        }
    }

    let queued = &app.input_queue.items;
    let editing = app.queue_edit.as_ref();
    if queued.is_empty() && editing.is_none() {
        return strip;
    }
    // Working, the queue simply waits its turn. Idle with messages left, it
    // stopped on purpose and needs a person: that, and editing one of its
    // messages, are the states that take the attention colour.
    let (label, detail, tone, rail) = if editing.is_some() {
        (
            language.text("Editing"),
            format!(" — {}", language.text("Enter saves it in place")),
            theme.accent,
            theme.accent,
        )
    } else if app.active_turn.is_some() {
        (
            language.text("Queued"),
            format!(" · {}", queued.len()),
            theme.text,
            theme.muted,
        )
    } else {
        (
            language.text("Queue paused"),
            format!(" — {}", language.text("send when ready")),
            theme.accent,
            theme.accent,
        )
    };
    strip.push_tagged(
        strip_row(
            rail,
            header(label, detail, tone, theme),
            theme.surface,
            width,
            theme,
        ),
        HitTarget::QueueList,
    );
    for (index, item) in queued.iter().take(MAX_QUEUED_ROWS).enumerate() {
        let edited = editing.is_some_and(|edit| edit.id == item.id);
        strip.push_tagged(
            numbered_row(
                index + 1,
                &crate::input_queue::queued_label(&item.text),
                rail,
                edited,
                width,
                theme,
            ),
            HitTarget::QueueItem(item.id),
        );
    }
    if let Some(more) = more_row(queued.len(), rail, width, theme) {
        strip.push_tagged(more, HitTarget::QueueList);
    }
    if let Some(edit) = editing {
        // A refused save replaces the hint: with a turn running, the band
        // has no room for notices, so this line is where the reason lands.
        let (text, color) = edit.conflict.as_deref().map_or(
            (
                language.text("Editing queued message · Enter saves · Click to cancel"),
                theme.accent,
            ),
            |reason| (language.text(reason), theme.danger),
        );
        strip.push_tagged(
            strip_row(
                rail,
                vec![Span::new(text.to_owned(), color)],
                theme.surface,
                width,
                theme,
            ),
            HitTarget::QueueEditCancel,
        );
    }
    strip
}

fn header(label: &str, detail: String, tone: Color, theme: &Theme) -> Vec<Span> {
    vec![
        Span::new(label.to_owned(), tone).with_attributes(attr::BOLD),
        Span::new(detail, theme.muted),
    ]
}

/// One message, numbered in delivery order and cut to a single line. The
/// message being edited is raised on the selection background.
fn numbered_row(
    number: usize,
    text: &str,
    rail: Color,
    edited: bool,
    width: usize,
    theme: &Theme,
) -> Line {
    let number = format!("{number}. ");
    let room = width.saturating_sub(RAIL.width() + number.width() + 1);
    let mut body = Span::new(single_line(text, room), theme.text);
    if edited {
        body = body.with_attributes(attr::BOLD);
    }
    strip_row(
        if edited { theme.accent } else { rail },
        vec![Span::new(number, theme.muted), body],
        if edited {
            theme.selected
        } else {
            theme.surface
        },
        width,
        theme,
    )
}

fn more_row(count: usize, rail: Color, width: usize, theme: &Theme) -> Option<Line> {
    (count > MAX_QUEUED_ROWS).then(|| {
        strip_row(
            rail,
            vec![Span::new(
                format!("+{} more", count - MAX_QUEUED_ROWS),
                theme.muted,
            )],
            theme.surface,
            width,
            theme,
        )
    })
}

/// Rail, content, and a raised background carried to the frame edge.
fn strip_row(
    rail: Color,
    content: Vec<Span>,
    background: Color,
    width: usize,
    theme: &Theme,
) -> Line {
    let mut spans = vec![Span::new(RAIL, rail)];
    spans.extend(content);
    let mut line = clamp(spans, width);
    let used: usize = line.spans.iter().map(Span::width).sum();
    if used < width {
        line.spans
            .push(Span::new(" ".repeat(width - used), theme.text));
    }
    for span in &mut line.spans {
        span.bg.get_or_insert(background);
    }
    line
}

pub fn composer_lines(app: &App, width: usize) -> Vec<Line> {
    if app.is_child_view() {
        return vec![activity_rule(app, width), status_band(app, width)];
    }
    let mut rows = Vec::new();
    if let Some(row) = attachment_row(app, width) {
        rows.push(row);
    }
    rows.extend(input_rows(app, width));
    rows.push(activity_rule(app, width));
    rows.push(status_band(app, width));
    rows
}

/// Attached media, named rather than counted: the chips are the only thing
/// that tells you what a bare Enter is about to send.
fn attachment_row(app: &App, width: usize) -> Option<Line> {
    if app.attachments.is_empty() {
        return None;
    }
    let theme = &app.theme;
    let mut spans = vec![Span::new(" ".repeat(INSET), theme.muted)];
    for (index, source) in app.attachments.iter().take(3).enumerate() {
        // A queued message opened for editing can carry uploaded data URLs
        // as well as local paths; name both the way the transcript does.
        let (badge, name) = crate::media::attachment_display(source);
        if index > 0 {
            spans.push(Span::new("   ", theme.muted));
        }
        spans.extend(crate::view::span::media_chip(badge, &name, theme));
    }
    let more = app
        .attachments
        .len()
        .saturating_sub(3.min(app.attachments.len()));
    if more > 0 {
        spans.push(Span::new(format!("   +{more} more"), theme.muted));
    }
    Some(clamp(spans, width))
}

/// The hairline under the prompt is the product's heartbeat. Idle it is a
/// quiet rule; during a turn it breathes in the agent color; when the turn is
/// blocked on a human it stops moving and goes solid, which is impossible to
/// mistake for progress.
fn activity_rule(app: &App, width: usize) -> Line {
    let theme = &app.theme;
    let (glyph, color) = rule_state(app, theme);
    Line::new(vec![Span::new(glyph.to_string().repeat(width), color)])
}

fn rule_state(app: &App, theme: &Theme) -> (char, Color) {
    if app.connection_lost.is_some() {
        return ('━', theme.pulse(theme.danger, breath(app.tick)));
    }
    if !app.pending_permissions.is_empty() || !app.pending_questions.is_empty() {
        return ('━', theme.accent);
    }
    if app.active_turn.is_some() {
        return ('━', theme.pulse(theme.primary, breath(app.tick)));
    }
    ('─', theme.hairline())
}

/// A heartbeat over ~2.4s at the render tick: a quick rise and a long fall,
/// eased so the crest holds for a frame instead of snapping.
///
/// This used to be a symmetric triangle, which is why it was easy to miss. The
/// eye reports *rate of change*, not absolute difference — a slow linear fade
/// is exactly the signal peripheral vision adapts to and stops noticing, so
/// widening the range would not have helped. Concentrating the same swing into
/// a short attack gives it a transient to catch instead.
fn breath(tick: u64) -> f64 {
    const PERIOD: u64 = 12;
    const ATTACK: u64 = 4;
    let phase = tick % PERIOD;
    #[allow(clippy::cast_precision_loss)] // small counters, display only
    let ramp = if phase < ATTACK {
        phase as f64 / ATTACK as f64
    } else {
        1.0 - (phase - ATTACK) as f64 / (PERIOD - ATTACK) as f64
    };
    // Smoothstep: rounds the corners of the ramp so the beat reads as a swell
    // rather than a sawtooth, without softening the attack enough to lose it.
    ramp * ramp * (3.0 - 2.0 * ramp)
}

/// The instrument band: who is answering and on what, then whatever the
/// session is doing right now. One raised strip on the terminal's last row is
/// the whole of Haarness's persistent chrome.
fn status_band(app: &App, width: usize) -> Line {
    let theme = &app.theme;
    let identity = left_variants(app);
    let (state, essential) = right_segments(app, width);
    // Detail yields before identity: trim the state text down to what the
    // session cannot do without, keeping the model on screen. Only once even
    // that will not fit does identity start giving way — and then state wins,
    // because on a 40-column terminal "how do I stop this" outranks "on what
    // model".
    let mut chosen = None;
    'detail: for (left_index, left) in identity.iter().enumerate() {
        for (right_index, right) in state.iter().enumerate().take(essential + 1) {
            if measure(left) + measure(right) + 3 <= width {
                chosen = Some((left_index, right_index));
                break 'detail;
            }
        }
    }
    if chosen.is_none() {
        'identity: for (right_index, right) in state.iter().enumerate().skip(essential + 1) {
            for (left_index, left) in identity.iter().enumerate() {
                if measure(left) + measure(right) + 3 <= width {
                    chosen = Some((left_index, right_index));
                    break 'identity;
                }
            }
        }
    }
    let (left_index, right_index) = chosen.unwrap_or((identity.len() - 1, state.len()));
    let mut left = identity.into_iter().nth(left_index).unwrap_or_default();
    let right = state.into_iter().nth(right_index).unwrap_or_default();
    let mut left_width = measure(&left);
    let right_width = measure(&right);
    if let Some(place) = place_segment(app) {
        let place_width = place.width() + 2;
        if left_width + place_width + right_width + 5 <= width {
            left.push(Span::new(format!("  {place}"), theme.muted).on(theme.surface));
            left_width += place_width;
        }
    }
    // Permission mode uses spare space without displacing location, queue or
    // stop hints. It names what it is: an agent can be called "review" too.
    // `--yolo` answers this TUI's requests whatever the chat's own mode is.
    let mode = format!(
        "  {}",
        app.language.text(if app.yolo {
            "approvals: --yolo"
        } else if app.auto_permissions {
            "approvals: auto"
        } else {
            "approvals: review"
        })
    );
    let mode_width = mode.width();
    if left_width + right_width + mode_width + 3 <= width {
        left.push(
            Span::new(
                mode,
                if app.yolo || app.auto_permissions {
                    theme.accent
                } else {
                    theme.muted
                },
            )
            .on(theme.surface),
        );
        left_width += mode_width;
    }
    let mut spans = left;
    if !right.is_empty() {
        let gap = width.saturating_sub(left_width + right_width + 1);
        spans.push(Span::new(" ".repeat(gap), theme.muted).on(theme.surface));
        spans.extend(right);
        spans.push(Span::new(" ", theme.muted).on(theme.surface));
    }
    band(spans, width, theme)
}

fn measure(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// Who is answering and on what, richest first. The chip alone is the floor:
/// it is the one thing the band never gives up.
fn left_variants(app: &App) -> Vec<Vec<Span>> {
    let theme = &app.theme;
    let child = app.is_child_view();
    // Modes take the chip over: it is the one place that always answers
    // "what will this keystroke do?".
    let label = if app.permission_focus && !app.pending_permissions.is_empty() {
        app.language.text("approve").to_owned()
    } else if child {
        app.language.text("child").to_owned()
    } else if app.shell_mode {
        app.language.text("shell").to_owned()
    } else {
        app.agent().to_owned()
    };
    let chip = Span::new(format!(" {label} "), theme.bg)
        .on(chip_tone(app))
        .with_attributes(attr::BOLD);
    if child {
        let lifecycle = |compact| {
            let (text, color) = child_lifecycle(app, compact);
            vec![
                chip.clone(),
                Span::new("  ", theme.muted).on(theme.surface),
                Span::new(text, color).on(theme.surface),
            ]
        };
        return vec![lifecycle(false), lifecycle(true), vec![chip]];
    }
    // With nothing connected, the configured default is a model nobody can
    // use yet; naming it would read as if the session were ready. An empty
    // catalog only means it has not loaded, so the model stays named then.
    let model = if app.providers.is_empty() || app.any_provider_connected() {
        vec![
            chip.clone(),
            Span::new(
                format!(
                    "  {}/",
                    haarness_protocol::provider_display_id(&app.thread.provider)
                ),
                theme.muted,
            )
            .on(theme.surface),
            Span::new(
                app.model_label(&format!("{}/{}", app.thread.provider, app.thread.model)),
                theme.text,
            )
            .on(theme.surface),
        ]
    } else {
        vec![
            chip.clone(),
            Span::new("  no model", theme.muted).on(theme.surface),
        ]
    };
    if let Some(custom) = app
        .custom_status_line
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return vec![
            vec![
                chip.clone(),
                Span::new("  ", theme.muted).on(theme.surface),
                Span::new(single_line(custom, 160), theme.text).on(theme.surface),
            ],
            model,
            vec![chip],
        ];
    }
    let mut full = model.clone();
    if let Some(billing) = &app.billing.summary {
        full.push(
            Span::new(
                if app.billing.error.is_some() {
                    "  Cloud credits unavailable · /billing".to_owned()
                } else {
                    format!(
                        "  Cloud {} included · {} purchased · /billing",
                        haarness_account::billing::display_credits(&billing.included_credits),
                        haarness_account::billing::display_credits(&billing.purchased_credits)
                    )
                },
                theme.muted,
            )
            .on(theme.surface),
        );
    }
    for (badge, color) in badges(app) {
        full.push(Span::new("  ", theme.muted).on(theme.surface));
        full.push(
            Span::new(badge, color)
                .on(theme.surface)
                .with_attributes(attr::BOLD),
        );
    }
    if full.len() == model.len() {
        return vec![full, vec![chip]];
    }
    vec![full, model, vec![chip]]
}

/// Where this session is working, short enough to live beside the model. The
/// full path belongs to the masthead; the band only needs the discriminator.
fn place_segment(app: &App) -> Option<String> {
    if app.is_child_view() {
        return None;
    }
    let info = app.server_info.as_ref()?;
    let place = info
        .workspace
        .as_deref()
        .and_then(|workspace| workspace.rsplit('/').next())
        .unwrap_or_default();
    match (place, info.git_branch.as_deref()) {
        ("", None) => None,
        ("", Some(branch)) => Some(branch.to_owned()),
        (place, None) => Some(place.to_owned()),
        (place, Some(branch)) => Some(format!("{place}  {branch}")),
    }
}

fn chip_tone(app: &App) -> Color {
    let theme = &app.theme;
    if app.connection_lost.is_some() {
        theme.danger
    } else if app.shell_mode
        || !app.pending_permissions.is_empty()
        || !app.pending_questions.is_empty()
    {
        theme.accent
    } else if app.is_child_view() {
        theme.user
    } else {
        theme.primary
    }
}

/// Session preferences worth a permanent marker. Ordinary defaults earn none:
/// a band that always says `default` teaches the eye to skip it.
fn badges(app: &App) -> Vec<(String, Color)> {
    let theme = &app.theme;
    let mut badges = Vec::new();
    if app.thread.ultracode {
        badges.push(("ultracode".to_owned(), theme.accent));
    } else if let Some(effort) = &app.reasoning_effort {
        badges.push((effort.clone(), theme.muted));
    }
    if app.thinking == crate::state::ThinkingMode::Hide {
        badges.push((app.language.text("thinking_hidden").to_owned(), theme.muted));
    }
    if app.remote.shared(app.thread.id) {
        badges.push(("remote".to_owned(), theme.success()));
    }
    badges
}

/// Right-hand state, most to least verbose. The band renders the first
/// variant that fits, so nothing important is ever pushed off the edge by an
/// optional detail.
fn right_segments(app: &App, width: usize) -> (Vec<Vec<Span>>, usize) {
    let theme = &app.theme;
    let plain = |text: String, color: Color| vec![Span::new(text, color).on(theme.surface)];
    // Everything but a running turn is detail all the way down: its last
    // variant is the only essential one.
    let detail = |variants: Vec<Vec<Span>>| {
        let last = variants.len().saturating_sub(1);
        (variants, last)
    };

    if app.following_stopped {
        return detail(vec![
            plain(
                app.language
                    .text(
                        "not following the server · trying again in a minute, or restart Haarness",
                    )
                    .to_owned(),
                theme.danger,
            ),
            plain(
                app.language.text("not following the server").to_owned(),
                theme.danger,
            ),
        ]);
    }
    if let Some(attempt) = app.connection_lost {
        let reconnecting = app.language.text("reconnecting").to_owned();
        // Counting attempts forever explains nothing: once a few have
        // failed, say what is going on and what this client does about it.
        let state = match app.server_restart {
            Some(ServerRestart::Starting) => app.language.text("server stopped · restarting it"),
            Some(ServerRestart::Failed) => app
                .language
                .text("server stopped · restart failed · retrying"),
            None if attempt >= 3 => app.language.text("server unreachable · still retrying"),
            None => {
                return detail(vec![
                    plain(format!("reconnecting · attempt {attempt}"), theme.danger),
                    plain(reconnecting, theme.danger),
                ]);
            }
        };
        return detail(vec![
            plain(state.to_owned(), theme.danger),
            plain(reconnecting, theme.danger),
        ]);
    }
    if !app.pending_permissions.is_empty() {
        let notice = single_line(&app.notice, width / 2);
        return detail(vec![
            vec![
                Span::new(notice, theme.accent)
                    .on(theme.surface)
                    .with_attributes(attr::BOLD),
            ],
            vec![
                Span::new(app.language.text("approval needed"), theme.accent)
                    .on(theme.surface)
                    .with_attributes(attr::BOLD),
            ],
        ]);
    }
    if app.active_turn.is_some() {
        return running_segments(app, theme);
    }
    let background = app.root_descendant_active_count();
    if background > 0 {
        let activity = if background == 1 {
            "1 child turn running".to_owned()
        } else {
            format!("{background} child turns running")
        };
        let target = InterruptTarget::Session(app.root_thread_id);
        let hint = if app
            .ctrl_c_action
            .as_ref()
            .is_some_and(|(candidate, armed)| {
                *candidate == target && armed.elapsed() <= std::time::Duration::from_secs(5)
            }) {
            "ctrl+c again to stop"
        } else if app
            .escape_interrupt
            .as_ref()
            .is_some_and(|(candidate, armed)| {
                *candidate == target && armed.elapsed() <= std::time::Duration::from_secs(5)
            })
        {
            "esc again to stop"
        } else {
            // Unarmed, the band says nothing about stopping: the first
            // ctrl+c or esc press is the moment to explain the second.
            ""
        };
        if hint.is_empty() {
            return detail(vec![plain(activity, theme.primary)]);
        }
        return detail(vec![
            plain(format!("{activity}   {hint}"), theme.primary),
            plain(activity, theme.primary),
        ]);
    }
    if app.effective_viewport_offset() > 0 {
        return detail(vec![
            plain("scrolled · end to resume".to_owned(), theme.muted),
            plain("scrolled".to_owned(), theme.muted),
        ]);
    }
    // Feedback and ambient state share the slot: the notice answers what you
    // just did, the counters answer what the session has cost. Neither one is
    // worth hiding the other, so they only compete for room.
    let notice = app.language.text(app.notice.trim()).to_owned();
    let (input_tokens, output_tokens) = app.usage_totals();
    let usage = (input_tokens + output_tokens > 0).then(|| {
        let fill = app
            .context_fill()
            .filter(|fill| *fill >= 0.01)
            .map_or_else(String::new, |fill| {
                format!("{:.0}% ctx   ", (fill * 100.0).clamp(0.0, 100.0))
            });
        let cost = app.usage_cost().map_or_else(String::new, |cost| {
            if cost <= 0.0 {
                String::new()
            } else if cost < 0.005 {
                "   <$0.01".to_owned()
            } else {
                format!("   ${cost:.2}")
            }
        });
        let flow = format!(
            "{}↑ {}↓",
            compact_count(input_tokens),
            compact_count(output_tokens)
        );
        (format!("{fill}{flow}{cost}"), format!("{fill}{flow}"), flow)
    });
    let brief = |text: &str| single_line(text, (width / 3).max(12));
    match (notice.is_empty(), usage) {
        (false, Some((full, medium, flow))) => detail(vec![
            plain(format!("{notice}   {full}"), theme.muted),
            plain(format!("{notice}   {medium}"), theme.muted),
            plain(format!("{notice}   {flow}"), theme.muted),
            plain(notice.clone(), theme.muted),
            plain(brief(&notice), theme.muted),
        ]),
        (false, None) => detail(vec![
            plain(notice.clone(), theme.muted),
            plain(brief(&notice), theme.muted),
        ]),
        (true, Some((full, medium, flow))) => detail(vec![
            plain(full, theme.muted),
            plain(medium, theme.muted),
            plain(flow, theme.muted),
        ]),
        // Place already sits in the identity half, so the quietest possible
        // band just carries the build.
        (true, None) => detail(vec![plain(
            app.server_info
                .as_ref()
                .map_or_else(String::new, |info| format!("v{}", info.version)),
            theme.muted,
        )]),
    }
}

/// A running turn narrates what it is actually doing (from `turn.status`),
/// never a stale free-text notice. Its interrupt escape hatch is the last
/// thing dropped; optional queue text yields first.
fn running_segments(app: &App, theme: &Theme) -> (Vec<Vec<Span>>, usize) {
    let phase = phase_text(app);
    let interrupt = interrupt_hint(app);
    let queued = app.pending_follow_ups().len();
    let verbose_queue = match queued {
        0 => String::new(),
        1 => "   follow-up steer".to_owned(),
        count => format!("   {count} follow-ups steering"),
    };
    let compact_queue = match queued {
        0 => String::new(),
        1 => "   steer".to_owned(),
        count => format!("   {count} steering"),
    };
    let build = |show_phase: bool, queue: &str, recovery: &str, hint: &str| {
        let mut spans = Vec::new();
        if show_phase {
            spans.push(
                Span::new(phase.clone(), theme.primary)
                    .on(theme.surface)
                    .with_attributes(attr::BOLD),
            );
        }
        if !queue.is_empty() {
            spans.push(Span::new(queue.to_owned(), theme.muted).on(theme.surface));
        }
        if !recovery.is_empty() {
            spans.push(
                Span::new(
                    format!("{}{}", if spans.is_empty() { "" } else { "   " }, recovery),
                    theme.muted,
                )
                .on(theme.surface),
            );
        }
        if !hint.is_empty() {
            spans.push(
                Span::new(
                    format!("{}{}", if spans.is_empty() { "" } else { "   " }, hint),
                    theme.muted,
                )
                .on(theme.surface),
            );
        }
        spans
    };
    let brief = if interrupt == "ctrl+c twice to interrupt" {
        "ctrl+c twice"
    } else {
        interrupt
    };
    if app.effective_viewport_offset() > 0 {
        return (
            vec![
                build(true, &verbose_queue, "scrolled · end to resume", interrupt),
                build(true, &compact_queue, "scrolled · end to resume", interrupt),
                build(true, "", "scrolled · end to resume", interrupt),
                build(false, "", "scrolled · end", brief),
                build(false, "", "scrolled", brief),
                build(true, "", "", brief),
                build(true, "", "", ""),
            ],
            2,
        );
    }
    (
        vec![
            build(true, &verbose_queue, "", interrupt),
            build(true, &compact_queue, "", interrupt),
            build(true, "", "", interrupt),
            build(true, "", "", brief),
            build(true, "", "", ""),
        ],
        2,
    )
}

fn interrupt_hint(app: &App) -> &'static str {
    if app
        .phase
        .as_ref()
        .is_some_and(|phase| phase.state == "stopping")
    {
        return "interrupt requested";
    }
    // Unarmed, there is no hint: the band shows what the session is doing,
    // and the first ctrl+c or esc press explains the second one.
    app.active_turn.map_or("", |turn| {
        if app
            .ctrl_c_action
            .as_ref()
            .is_some_and(|(candidate, armed)| {
                *candidate == InterruptTarget::Turn(turn)
                    && armed.elapsed() <= std::time::Duration::from_secs(5)
            })
        {
            "ctrl+c again to interrupt"
        } else if app
            .escape_interrupt
            .as_ref()
            .is_some_and(|(candidate, armed)| {
                *candidate == InterruptTarget::Turn(turn)
                    && armed.elapsed() <= std::time::Duration::from_secs(5)
            })
        {
            "esc again to interrupt"
        } else {
            ""
        }
    })
}

/// Render the lifecycle of the child task backing the currently viewed
/// thread. Only genuinely live states receive a timer; terminal states never
/// derive elapsed time from `started_at`, so their display cannot keep ticking.
fn child_lifecycle(app: &App, compact: bool) -> (String, Color) {
    let task = app
        .children
        .values()
        .find(|task| task.child_thread_id == app.thread.id);
    let Some(task) = task else {
        return if app.active_turn.is_some() {
            (app.language.text("running").into(), app.theme.primary)
        } else {
            (
                if compact {
                    "unknown".into()
                } else {
                    "status unavailable".into()
                },
                app.theme.muted,
            )
        };
    };
    let elapsed = || {
        task.elapsed_seconds()
            .map_or_else(String::new, |seconds| format!(" {seconds}s"))
    };
    match task.state.as_str() {
        "running" if compact => (app.language.text("running").into(), app.theme.primary),
        "running" => (
            format!("{}{}", app.language.text("running"), elapsed()),
            app.theme.primary,
        ),
        "waiting_permission" if compact => (app.language.text("waiting").into(), app.theme.accent),
        "waiting_permission" => (
            format!("{}{}", app.language.text("waiting for approval"), elapsed()),
            app.theme.accent,
        ),
        "done" => (app.language.text("completed").into(), app.theme.success()),
        "failed" => (app.language.text("failed").into(), app.theme.danger),
        "cancelled" | "interrupted" => (app.language.text("cancelled").into(), app.theme.muted),
        state => (state.to_owned(), app.theme.muted),
    }
}

/// Human phrase for the turn's current phase, with per-phase elapsed time.
fn phase_text(app: &App) -> String {
    let turn_elapsed = app
        .turn_started
        .map_or(0, |started| started.elapsed().as_secs());
    let Some(phase) = &app.phase else {
        return format!("{}   {turn_elapsed}s", app.language.text("thinking"));
    };
    let elapsed = phase.elapsed_seconds();
    match phase.state.as_str() {
        "responding" => format!("{}   {turn_elapsed}s", app.language.text("writing")),
        "tool" => {
            let tool = phase.tool.as_deref().unwrap_or("tool");
            let agents = if app.is_child_view() {
                0
            } else {
                app.running_children()
            };
            match agents {
                0 => format!("{tool}   {elapsed}s"),
                1 => format!("{tool} + 1 agent   {elapsed}s"),
                count => format!("{tool} + {count} agents   {elapsed}s"),
            }
        }
        "waiting_permission" => app.language.text("waiting for approval").into(),
        "waiting_question" => app.language.text("waiting for your answer").into(),
        "child_tasks" => {
            let running = app.running_children();
            if running > 0 {
                format!("{running} child agents   {elapsed}s   ctrl+o opens")
            } else {
                format!("child agents   {elapsed}s")
            }
        }
        "compacting" => format!("compacting   {elapsed}s"),
        "retrying" => format!("recovering incomplete response   {elapsed}s"),
        "stopping" => app.language.text("stopping").into(),
        "done" => app.language.text("finishing").into(),
        _ => format!("{}   {turn_elapsed}s", app.language.text("thinking")),
    }
}

/// Multiline input rows: logical lines soft-wrap to the frame width (long
/// prompts stay fully visible while typing), windowed to `MAX_INPUT_ROWS`
/// around the cursor, with the cursor cell on the selection background.
fn input_rows(app: &App, width: usize) -> Vec<Line> {
    let theme = &app.theme;
    // While the approval card owns the keyboard the prompt draws no live
    // cursor: keys answer the card until one is clearly typing, or ctrl+g
    // hands the keyboard back.
    let card_owns_keys = app.permission_focus && !app.pending_permissions.is_empty();
    if app.composer.is_empty() && card_owns_keys {
        // Wrapped rather than cut: the way back to typing is its last words.
        let hint = app
            .language
            .text("Answer the approval above · ctrl+g to type");
        return textwrap::wrap(hint, width.saturating_sub(INSET * 2).max(10))
            .into_iter()
            .map(|row| {
                clamp(
                    vec![
                        Span::new(" ".repeat(INSET), theme.muted),
                        Span::new(row.into_owned(), theme.muted),
                    ],
                    width,
                )
            })
            .collect();
    }
    if app.composer.is_empty() {
        let placeholder = if app.shell_mode {
            "shell command…"
        } else if app.attachments.is_empty() {
            "Ask anything…"
        } else {
            "Add a question, or Enter to send media"
        };
        let mut spans = vec![Span::new(" ".repeat(INSET), theme.muted)];
        if app.shell_mode {
            spans.push(Span::new("! ", theme.accent).with_attributes(attr::BOLD));
        }
        let placeholder = app.language.text(placeholder);
        if app.language.is_rtl() {
            // A bidi-aware terminal reverses a right-to-left placeholder in
            // place, which would carry a cursor drawn on its first letter to
            // the middle of the row. The cursor gets a cell of its own.
            spans.push(cursor_span(" ".to_owned(), app));
            spans.push(Span::new(placeholder.to_owned(), theme.muted));
            return vec![clamp(spans, width)];
        }
        let mut characters = placeholder.chars();
        let head = characters
            .next()
            .map_or_else(|| " ".to_owned(), String::from);
        spans.push(cursor_span(head, app));
        spans.push(Span::new(characters.as_str().to_owned(), theme.muted));
        return vec![clamp(spans, width)];
    }
    let text = app.composer.text();
    let cursor = app.composer.cursor();
    let inner_width = width.saturating_sub(INSET * 2 + 2).max(10);

    // Flatten logical rows into display-cell-wrapped rows, tracking the cursor
    // as a UTF-8 boundary inside its segment.
    let mut display: Vec<(String, Option<usize>)> = Vec::new();
    let mut byte_offset = 0;
    for row in text.split('\n') {
        let row_start = byte_offset;
        byte_offset += row.len() + 1;
        let cursor_here = cursor >= row_start && cursor <= row_start + row.len();
        let cursor_byte = if cursor_here {
            (cursor - row_start).min(row.len())
        } else {
            0
        };
        if row.is_empty() {
            display.push((String::new(), cursor_here.then_some(0)));
            continue;
        }
        let mut start = 0;
        let mut cells = 0;
        for (index, character) in row.char_indices() {
            let character_width = character.width().unwrap_or(0);
            if index > start && cells + character_width > inner_width {
                let segment = row[start..index].to_owned();
                let holds_cursor = cursor_here && cursor_byte >= start && cursor_byte < index;
                display.push((segment, holds_cursor.then(|| cursor_byte - start)));
                start = index;
                cells = 0;
            }
            cells += character_width;
        }
        {
            let end = row.len();
            let segment = row[start..end].to_owned();
            let holds_cursor = cursor_here && cursor_byte >= start && cursor_byte <= end;
            display.push((segment, holds_cursor.then(|| cursor_byte - start)));
        }
    }

    let cursor_display = display
        .iter()
        .position(|(_, cursor)| cursor.is_some())
        .unwrap_or(0);
    let visible = display.len().min(MAX_INPUT_ROWS);
    let start = (cursor_display + 1)
        .saturating_sub(visible)
        .min(display.len().saturating_sub(visible));

    let mut lines = Vec::new();
    for (index, (row, cursor_column)) in display.iter().enumerate() {
        if index < start || index >= start + visible {
            continue;
        }
        let mut spans = vec![Span::new(" ".repeat(INSET), theme.muted)];
        if app.shell_mode && index == start {
            spans.push(Span::new("! ", theme.accent).with_attributes(attr::BOLD));
        }
        if let Some(column) = cursor_column {
            let column = (*column).min(row.len());
            let before = row[..column].to_owned();
            let mut tail = row[column..].chars();
            let mut at = tail
                .next()
                .map_or_else(|| " ".to_owned(), |value| value.to_string());
            if at == "\t" {
                at = " ".into();
            }
            let after: String = tail.collect();
            spans.push(Span::new(before, theme.text));
            if card_owns_keys {
                spans.push(Span::new(at, theme.text));
            } else {
                spans.push(cursor_span(at, app));
            }
            spans.push(Span::new(after, theme.text));
        } else {
            spans.push(Span::new(row.clone(), theme.text));
        }
        lines.push(clamp(spans, width));
    }
    lines
}

/// A cursor must remain unmistakable on every theme and at end-of-line,
/// where the cell is a styled space rather than a visible glyph.
fn cursor_span(text: impl Into<String>, app: &App) -> Span {
    Span::new(text, app.theme.bg)
        .on(app.theme.primary)
        .with_attributes(attr::BOLD)
}

/// Trim styled runs to `width` display cells without splitting a wide glyph.
fn clamp(content: Vec<Span>, width: usize) -> Line {
    let mut remaining = width;
    let mut spans = Vec::with_capacity(content.len());
    for mut span in content {
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
            remaining -= cells;
        }
        if !text.is_empty() {
            span.text = text;
            spans.push(span);
        }
    }
    Line::new(spans)
}

/// One continuous raised strip, filled to the frame edge so the band reads as
/// a physical edge of the application rather than a line of text.
fn band(content: Vec<Span>, width: usize, theme: &Theme) -> Line {
    let mut line = clamp(content, width);
    let used: usize = line.spans.iter().map(Span::width).sum();
    if used < width {
        line.spans
            .push(Span::new(" ".repeat(width - used), theme.muted).on(theme.surface));
    }
    for span in &mut line.spans {
        span.bg.get_or_insert(theme.surface);
    }
    line
}
