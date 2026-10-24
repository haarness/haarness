use super::*;
use crate::state::{OptionsKind, Overlay, OverlayChoice};

fn browser_app(count: u64) -> App {
    let thread = fixture_thread();
    let items = (1..=count)
        .map(|i| {
            message_item(
                i,
                &thread,
                ItemData::AssistantMessage {
                    text: format!(
                        "reply-{i:04}\nsecond line {i}\n{}",
                        "wrapped prose ".repeat(8)
                    ),
                },
            )
        })
        .collect();
    App::new(thread.clone(), vec![thread], items, vec![], vec![])
}
fn options(app: &App) -> Vec<crate::state::OptionEntry> {
    let Overlay::Options { ref entries, .. } = app.overlay else {
        panic!("no options")
    };
    app.filtered_options(entries)
}
async fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input::handle_key(app, KeyEvent::new(code, modifiers), &offline_client())
        .await
        .unwrap();
}

#[test]
fn messages_search_full_text_unicode_and_hide_internal_records() {
    let mut app = browser_app(3);
    app.items.push(message_item(
        4,
        &app.thread,
        ItemData::UserMessage {
            text: "First line\nÉLÈVE 日本語 🔎 second line".into(),
            attachments: vec![],
            queued: false,
            synthetic: false,
            command_line: None,
        },
    ));
    app.items.push(message_item(
        5,
        &app.thread,
        ItemData::UserMessage {
            text: "internal hidden".into(),
            attachments: vec![],
            queued: false,
            synthetic: true,
            command_line: None,
        },
    ));
    app.items.push(message_item(
        6,
        &app.thread,
        ItemData::Reasoning {
            text: "private reasoning".into(),
            duration_ms: None,
        },
    ));
    app.items.push(message_item(
        7,
        &app.thread,
        ItemData::AssistantMessage { text: "  ".into() },
    ));
    app.open_messages(Some("élève 日本語"));
    assert_eq!(options(&app).len(), 1);
    assert!(options(&app)[0].label.starts_with("You"));
    app.overlay_filter.clear();
    assert_eq!(options(&app).len(), 4);
    assert!(options(&app)[1].description.starts_with("reply-0003"));
}

#[tokio::test]
async fn messages_shortcut_actions_escape_and_draft_preserve_input() {
    let mut app = browser_app(6);
    app.keybindings.insert("messages".into(), "alt+m".into());
    app.composer.set_text("unsent draft".into());
    app.attachments.push("image.png".into());
    key(&mut app, KeyCode::Char('m'), KeyModifiers::ALT).await;
    app.overlay_filter = "reply-0002".into();
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(matches!(
        app.overlay,
        Overlay::Options {
            kind: OptionsKind::MessageActions,
            ..
        }
    ));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert_eq!(app.overlay_filter, "reply-0002");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    let value = options(&app)
        .into_iter()
        .find(|entry| entry.value.starts_with("draft:"))
        .unwrap()
        .value;
    app.execute_message_action(&value, &offline_client())
        .await
        .unwrap();
    assert!(
        app.composer
            .text()
            .starts_with("unsent draft\n\nreply-0002")
    );
    assert_eq!(app.attachments, vec!["image.png"]);
    assert!(app.active_turn.is_none());
    assert_eq!(app.items.len(), 6);
}

#[tokio::test]
async fn messages_jump_uses_actual_wrapped_rows_and_survives_live_output() {
    for size in [(100, 30), (48, 18)] {
        let mut app = browser_app(30);
        app.lines(size);
        let id = app.items[4].id;
        app.execute_message_action(&format!("jump:{}/{id}", app.thread.id), &offline_client())
            .await
            .unwrap();
        let text = app
            .lines(size)
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("reply-0005"),
            "jump failed at {size:?}: {text}"
        );
        assert!(!text.contains("reply-0030"));
        app.items.push(message_item(
            31,
            &app.thread,
            ItemData::AssistantMessage {
                text: "new live response".into(),
            },
        ));
        let text = app
            .lines(size)
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("reply-0005"),
            "incoming output moved the reader: {text}"
        );
        app.scroll_to_bottom();
        assert!(
            app.lines(size)
                .iter()
                .any(|line| line.text().contains("new live response"))
        );
    }
}

#[tokio::test]
async fn messages_stale_actions_cannot_cross_sessions_or_use_removed_items() {
    let mut app = browser_app(4);
    let id = app.items[0].id;
    let old = format!("{}/{id}", app.thread.id);
    app.thread.id = ThreadId::new();
    for action in ["jump", "copy", "read", "draft", "fork"] {
        assert!(
            app.execute_message_action(&format!("{action}:{old}"), &offline_client())
                .await
                .is_err()
        );
    }
    app.open_messages(None);
    assert!(options(&app).is_empty());
    let new = format!("{}/{id}", app.thread.id);
    assert!(app.open_message_actions(&new).is_err());
    assert!(app.pending_message_jump.get().is_none());
    assert!(app.composer.is_empty());
}

#[test]
fn messages_live_refresh_keeps_selected_identity_and_bounds_large_lists() {
    let mut app = browser_app(1000);
    app.open_messages(Some("reply"));
    app.selection = 700;
    let selected = options(&app)[app.selection].value.clone();
    app.items.push(message_item(
        1001,
        &app.thread,
        ItemData::AssistantMessage {
            text: "reply newest".into(),
        },
    ));
    app.refresh_open_messages();
    assert_eq!(options(&app)[app.selection].value, selected);
    assert_eq!(app.selection, 701);
    for size in [(120, 40), (48, 18)] {
        let lines = app.lines(size);
        assert_eq!(lines.len(), size.1 as usize);
        let hits = app.hit_rows.borrow();
        let menu = hits
            .iter()
            .filter_map(|hit| {
                if let crate::view::frame::HitTarget::MenuRow(index) = hit.target {
                    Some(index)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert!(menu.contains(&701));
        assert!(menu.len() < 50);
    }
}

#[tokio::test]
async fn messages_child_views_allow_reading_but_reject_draft_and_fork() {
    let mut app = browser_app(3);
    app.thread.is_subagent = true;
    app.root_thread_id = ThreadId::new();
    let value = format!("{}/{}", app.thread.id, app.items[0].id);
    app.open_message_actions(&value).unwrap();
    assert!(
        !options(&app)
            .iter()
            .any(|entry| entry.value.starts_with("fork:") || entry.value.starts_with("draft:"))
    );
    for action in ["draft", "fork"] {
        assert!(
            app.execute_message_action(&format!("{action}:{value}"), &offline_client())
                .await
                .is_err()
        );
    }
    app.execute_choice(
        OverlayChoice::Option(OptionsKind::MessageActions, format!("read:{value}")),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(matches!(app.overlay, Overlay::Panel { .. }));
}

/// A conversation of prompts and long replies, with records the message keys
/// pass over: a reply that is only tool calls, an attachment-only prompt and
/// an internal prompt.
fn conversation_app() -> App {
    let thread = fixture_thread();
    let mut items = Vec::new();
    let user = |text: &str, attachments: Vec<String>, synthetic: bool| ItemData::UserMessage {
        text: text.into(),
        attachments,
        queued: false,
        synthetic,
        command_line: None,
    };
    for turn in 1..=6_u64 {
        let base = turn * 10;
        items.push(message_item(
            base,
            &thread,
            user(&format!("prompt-{turn}"), vec![], false),
        ));
        items.push(message_item(
            base + 1,
            &thread,
            ItemData::AssistantMessage {
                text: format!("reply-{turn}\n{}", "a long answer line\n".repeat(30)),
            },
        ));
        if turn == 3 {
            items.push(message_item(
                base + 2,
                &thread,
                ItemData::AssistantMessage { text: " ".into() },
            ));
            items.push(message_item(
                base + 3,
                &thread,
                user("", vec!["shot.png".into()], false),
            ));
            items.push(message_item(
                base + 4,
                &thread,
                user("internal", vec![], true),
            ));
        }
    }
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.keybindings = crate::keybindings::ACTIONS
        .iter()
        .map(|(action, _, key)| ((*action).to_owned(), (*key).to_owned()))
        .collect();
    app
}

const SIZE: (u32, u32) = (100, 30);

/// The first line of the transcript window, by what it says.
fn top_line(app: &App) -> String {
    let lines = app.lines(SIZE);
    let top = app
        .last_transcript_len
        .get()
        .saturating_sub(app.effective_viewport_offset() + app.transcript_rows.get());
    let rows = app.transcript_messages.borrow();
    let id = rows.iter().find(|(_, row)| *row == top).map(|(id, _)| *id);
    drop(rows);
    let Some(id) = id else {
        return format!(
            "row {top}: {}",
            lines.iter().map(Line::text).collect::<Vec<_>>().join("\n")
        );
    };
    let item = app.items.iter().find(|item| item.id == id).unwrap();
    crate::messages::message_text(item)
        .unwrap()
        .1
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn message_keys_step_by_message_and_by_prompt_from_the_top_row() {
    let mut app = conversation_app();
    app.lines(SIZE);
    let alt = KeyModifiers::ALT;
    let alt_shift = KeyModifiers::ALT | KeyModifiers::SHIFT;
    // From the newest output, the previous message is the one that starts
    // above the window.
    key(&mut app, KeyCode::Up, alt).await;
    assert_eq!(top_line(&app), "reply-6");
    key(&mut app, KeyCode::Up, alt).await;
    assert_eq!(top_line(&app), "prompt-6");
    key(&mut app, KeyCode::Up, alt).await;
    assert_eq!(top_line(&app), "reply-5");
    key(&mut app, KeyCode::Down, alt).await;
    assert_eq!(top_line(&app), "prompt-6");
    // By prompt: replies, a reply that is only tool calls, an attachment-only
    // prompt and an internal prompt are passed over.
    key(&mut app, KeyCode::Up, alt_shift).await;
    assert_eq!(top_line(&app), "prompt-5");
    key(&mut app, KeyCode::Up, alt_shift).await;
    assert_eq!(top_line(&app), "prompt-4");
    key(&mut app, KeyCode::Up, alt_shift).await;
    assert_eq!(top_line(&app), "prompt-3");
    key(&mut app, KeyCode::Down, alt_shift).await;
    assert_eq!(top_line(&app), "prompt-4");
    key(&mut app, KeyCode::Up, alt).await;
    assert_eq!(top_line(&app), "reply-3");
    key(&mut app, KeyCode::Down, alt).await;
    assert_eq!(top_line(&app), "prompt-4");
    // First message is the start of the conversation; last prompt the newest.
    key(&mut app, KeyCode::Home, alt).await;
    app.lines(SIZE);
    assert_eq!(
        app.effective_viewport_offset(),
        app.max_viewport_offset.get()
    );
    key(&mut app, KeyCode::Up, alt).await;
    app.lines(SIZE);
    assert_eq!(
        app.effective_viewport_offset(),
        app.max_viewport_offset.get()
    );
    key(&mut app, KeyCode::Down, alt).await;
    assert_eq!(top_line(&app), "prompt-1");
    key(&mut app, KeyCode::End, alt).await;
    assert_eq!(top_line(&app), "prompt-6");
    // Past the last message the view moves on by a page, to the newest output.
    for _ in 0..4 {
        key(&mut app, KeyCode::Down, alt).await;
        app.lines(SIZE);
    }
    assert_eq!(app.effective_viewport_offset(), 0);
}

#[tokio::test]
async fn message_keys_leave_the_draft_word_motion_and_a_waiting_card_alone() {
    let mut app = conversation_app();
    app.lines(SIZE);
    app.composer.set_text("draft one two".into());
    let draft = app.composer.text().to_owned();
    key(&mut app, KeyCode::Up, KeyModifiers::ALT).await;
    assert_eq!(top_line(&app), "reply-6");
    assert_eq!(app.composer.text(), draft);
    key(&mut app, KeyCode::Left, KeyModifiers::ALT).await;
    app.composer.insert_char('X');
    assert_eq!(app.composer.text(), "draft one Xtwo");
    // An approval card that holds the keyboard keeps it, staged answer and
    // all, while the transcript moves under it.
    app.composer.clear();
    app.pending_permissions
        .push(pending_permission_fixture(&app.thread, "nav"));
    app.permission_focus = true;
    key(&mut app, KeyCode::Up, KeyModifiers::ALT).await;
    assert_eq!(top_line(&app), "prompt-6");
    assert!(app.permission_focus);
    assert_eq!(app.pending_permissions.len(), 1);
    assert!(app.composer.is_empty());
    // So does a question: the choice and the custom answer stay as they were.
    app.permission_focus = false;
    app.pending_permissions.clear();
    app.pending_questions
        .push(haarness_protocol::PendingQuestionInfo {
            question_id: "nav-question".into(),
            prompt: "Which one?".into(),
            choices: vec!["red".into(), "blue".into()],
            multiple: false,
            custom: true,
            thread_id: app.thread.id,
            root_thread_id: Some(app.thread.id),
            turn_id: None,
            call_id: None,
            created_at: chrono::Utc::now(),
        });
    app.question_selection = 1;
    key(&mut app, KeyCode::Down, KeyModifiers::ALT).await;
    assert_eq!(top_line(&app), "reply-6");
    assert_eq!(app.question_selection, 1);
    assert_eq!(app.pending_questions.len(), 1);
    app.pending_questions.clear();
    // A rebound key moves the same way; the old one does nothing now.
    app.keybindings
        .insert("messages_previous".into(), "ctrl+pageup".into());
    key(&mut app, KeyCode::Up, KeyModifiers::ALT).await;
    assert_eq!(top_line(&app), "reply-6");
    key(&mut app, KeyCode::PageUp, KeyModifiers::CONTROL).await;
    assert_eq!(top_line(&app), "prompt-6");
}
