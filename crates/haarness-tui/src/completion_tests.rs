//! Turn-end banners: a completion waits out its grace, a queued prompt
//! starting inside it drops the banner, stops say nothing and failures say so.
use super::*;
use crate::state::{COMPLETION_GRACE, ToastVariant, TurnEnd};
use std::time::{Duration, Instant};

fn finished(thread: ThreadId, status: &str) -> haarness_protocol::EventEnvelope {
    fixture_event(
        "turn.finished",
        thread,
        thread,
        serde_json::json!({ "status": status }),
    )
}

fn started(thread: ThreadId) -> haarness_protocol::EventEnvelope {
    let mut event = fixture_event("turn.started", thread, thread, serde_json::json!({}));
    event.turn_id = Some(TurnId::new());
    event
}

/// An unfocused reader on `thread`, whose current turn began a minute ago.
fn away(thread: &Thread) -> App {
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.terminal_focused = false;
    app.active_turn = Some(TurnId::new());
    app.turn_started = Instant::now().checked_sub(Duration::from_secs(60));
    app
}

fn toast(app: &App) -> Option<&str> {
    app.notification_toast
        .as_ref()
        .map(|toast| toast.message.as_str())
}

#[tokio::test]
async fn a_queue_of_prompts_announces_finished_once_after_its_last_turn() {
    let thread = fixture_thread();
    let mut app = away(&thread);
    let client = offline_client();

    app.reduce(finished(thread.id, "completed"), &client)
        .await
        .unwrap();
    app.release_completions(Instant::now());
    assert_eq!(app.pending_notification, None, "held through the grace");
    assert_eq!(toast(&app), None);

    // The daemon starts the next queued prompt straight away.
    app.reduce(started(thread.id), &client).await.unwrap();
    app.release_completions(Instant::now() + COMPLETION_GRACE);
    assert_eq!(app.pending_notification, None, "work is still going");
    assert_eq!(toast(&app), None);
    assert!(!app.completions.holds(thread.id));

    // The last prompt is quick, but the queue it ends took a minute.
    app.reduce(finished(thread.id, "completed"), &client)
        .await
        .unwrap();
    assert_eq!(app.pending_notification, None);
    app.release_completions(Instant::now() + COMPLETION_GRACE);
    let message = format!("Haarness finished: {}", thread.title);
    assert_eq!(app.pending_notification.take().as_deref(), Some(&*message));
    assert_eq!(toast(&app), Some(&*message));
    app.release_completions(Instant::now() + COMPLETION_GRACE * 4);
    assert_eq!(app.pending_notification, None, "announced exactly once");
}

#[tokio::test]
async fn a_stop_says_nothing_and_a_failure_says_it_failed() {
    let thread = fixture_thread();
    let mut app = away(&thread);
    let client = offline_client();

    app.reduce(finished(thread.id, "interrupted"), &client)
        .await
        .unwrap();
    app.release_completions(Instant::now() + COMPLETION_GRACE);
    assert_eq!(
        app.pending_notification, None,
        "the reader asked for the stop"
    );
    assert_eq!(toast(&app), None);

    app.active_turn = Some(TurnId::new());
    app.turn_started = Some(Instant::now());
    app.reduce(finished(thread.id, "failed"), &client)
        .await
        .unwrap();
    let message = format!("Haarness failed: {}", thread.title);
    // Failures are never taken back, so they do not wait, whatever their length.
    assert_eq!(app.pending_notification.take().as_deref(), Some(&*message));
    assert_eq!(toast(&app), Some(&*message));
    assert!(matches!(
        app.notification_toast.as_ref().map(|toast| &toast.variant),
        Some(ToastVariant::Error)
    ));
    app.release_completions(Instant::now() + COMPLETION_GRACE);
    assert_eq!(app.pending_notification, None, "never also called finished");
}

#[tokio::test]
async fn a_failure_or_stop_inside_the_grace_drops_the_held_completion() {
    let thread = fixture_thread();
    let mut app = away(&thread);
    let client = offline_client();
    for end in ["interrupted", "failed"] {
        app.active_turn = Some(TurnId::new());
        app.turn_started = Instant::now().checked_sub(Duration::from_secs(60));
        app.reduce(finished(thread.id, "completed"), &client)
            .await
            .unwrap();
        app.reduce(finished(thread.id, end), &client).await.unwrap();
        app.pending_notification = None;
        app.release_completions(Instant::now() + COMPLETION_GRACE);
        assert_eq!(app.pending_notification, None, "{end}");
    }
}

fn owing(thread: ThreadId, status: &str, background: u32) -> haarness_protocol::EventEnvelope {
    fixture_event(
        "turn.finished",
        thread,
        thread,
        serde_json::json!({ "status": status, "background": background }),
    )
}

#[tokio::test]
async fn a_turn_background_work_outlives_is_no_end_and_the_turn_its_result_starts_is() {
    let thread = fixture_thread();
    let mut app = away(&thread);
    let client = offline_client();

    // A completion still in its grace is dropped by a later turn that
    // leaves background work running, completed or failed.
    app.reduce(finished(thread.id, "completed"), &client)
        .await
        .unwrap();
    for status in ["completed", "failed"] {
        app.reduce(started(thread.id), &client).await.unwrap();
        app.reduce(owing(thread.id, status, 1), &client)
            .await
            .unwrap();
        assert!(!app.completions.holds(thread.id), "{status}");
        app.release_completions(Instant::now() + COMPLETION_GRACE);
        assert_eq!(app.pending_notification, None, "{status}: still at work");
        assert_eq!(toast(&app), None);
    }

    // The child's result starts a quick turn; the run it ends took a minute.
    app.reduce(started(thread.id), &client).await.unwrap();
    app.turn_started = Some(Instant::now());
    app.reduce(finished(thread.id, "completed"), &client)
        .await
        .unwrap();
    app.release_completions(Instant::now() + COMPLETION_GRACE);
    let message = format!("Haarness finished: {}", thread.title);
    assert_eq!(app.pending_notification.take().as_deref(), Some(&*message));
    assert_eq!(toast(&app), Some(&*message));
    assert_eq!(
        TurnEnd::from_payload(&serde_json::json!({"status": "completed", "background": 0})),
        TurnEnd::Completed
    );
    assert_eq!(
        TurnEnd::from_payload(&serde_json::json!({"status": "interrupted", "background": 2})),
        TurnEnd::Stopped
    );
}

/// The prompt a turn of `thread` starts with: a person's, or a background
/// result the runtime delivered (`synthetic`).
fn prompt(thread: ThreadId, synthetic: bool) -> haarness_protocol::EventEnvelope {
    let item = haarness_protocol::Item {
        id: haarness_protocol::ItemId::new(),
        thread_id: thread,
        turn_id: None,
        sequence: 1,
        data: haarness_protocol::ItemData::UserMessage {
            text: "next".into(),
            attachments: Vec::new(),
            queued: false,
            synthetic,
        },
        created_at: chrono::Utc::now(),
    };
    fixture_event(
        "item.created",
        thread,
        thread,
        serde_json::to_value(item).unwrap(),
    )
}

#[tokio::test]
async fn a_run_left_waiting_on_stopped_work_does_not_make_the_next_quick_turn_long() {
    let thread = fixture_thread();
    let mut app = away(&thread);
    let client = offline_client();
    app.reduce(owing(thread.id, "completed", 1), &client)
        .await
        .unwrap();
    assert!(app.completions.owes(thread.id));

    // The work was stopped while the chat was idle; the person's next prompt
    // starts a quick turn of its own.
    app.reduce(prompt(thread.id, false), &client).await.unwrap();
    assert!(!app.completions.owes(thread.id), "the run ends with it");
    app.reduce(started(thread.id), &client).await.unwrap();
    app.reduce(finished(thread.id, "completed"), &client)
        .await
        .unwrap();
    app.release_completions(Instant::now() + COMPLETION_GRACE);
    assert_eq!(app.pending_notification, None, "a quick turn stays quiet");
    assert_eq!(toast(&app), None);
}

#[tokio::test]
async fn a_delivered_result_carries_the_waiting_run_on() {
    let thread = fixture_thread();
    let mut app = away(&thread);
    let client = offline_client();
    app.reduce(owing(thread.id, "completed", 1), &client)
        .await
        .unwrap();
    // The result arrives as a runtime message starting the next turn.
    app.reduce(prompt(thread.id, true), &client).await.unwrap();
    assert!(app.completions.owes(thread.id), "the run carries on");
    app.reduce(started(thread.id), &client).await.unwrap();
    app.reduce(finished(thread.id, "completed"), &client)
        .await
        .unwrap();
    assert!(!app.completions.owes(thread.id));
    app.release_completions(Instant::now() + COMPLETION_GRACE);
    let message = format!("Haarness finished: {}", thread.title);
    assert_eq!(app.pending_notification.take().as_deref(), Some(&*message));
}

#[tokio::test]
async fn a_focused_reader_gets_no_card_for_a_long_completion() {
    let thread = fixture_thread();
    let mut app = away(&thread);
    app.terminal_focused = true;
    app.reduce(finished(thread.id, "completed"), &offline_client())
        .await
        .unwrap();
    app.release_completions(Instant::now() + COMPLETION_GRACE);
    // The event loop sends the desktop banner only while unfocused.
    assert!(app.pending_notification.is_some());
    assert_eq!(toast(&app), None, "the reply is already on screen");
}

#[tokio::test]
async fn a_background_conversation_resuming_drops_its_banner() {
    let current = fixture_thread();
    let background = Thread {
        title: "Background".into(),
        ..fixture_thread()
    };
    let mut app = App::new(
        current.clone(),
        vec![current.clone(), background.clone()],
        vec![],
        vec![],
        vec![],
    );
    let client = offline_client();
    app.reduce(finished(background.id, "completed"), &client)
        .await
        .unwrap();
    // Another conversation starting does not touch this one's banner.
    app.reduce(started(current.id), &client).await.unwrap();
    assert!(app.completions.holds(background.id));
    app.reduce(started(background.id), &client).await.unwrap();
    app.release_completions(Instant::now() + COMPLETION_GRACE);
    assert_eq!(toast(&app), None);

    app.reduce(finished(background.id, "completed"), &client)
        .await
        .unwrap();
    app.release_completions(Instant::now() + COMPLETION_GRACE);
    // Work in another conversation earns a card even for a focused reader.
    assert_eq!(toast(&app), Some("Haarness finished: Background"));
}
