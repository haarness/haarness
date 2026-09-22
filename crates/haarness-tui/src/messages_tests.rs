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
        },
    ));
    app.items.push(message_item(
        6,
        &app.thread,
        ItemData::Reasoning {
            text: "private reasoning".into(),
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
