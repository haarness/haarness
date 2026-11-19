//! `/compact` runs off the event loop: the command returns at once, the band
//! says `compacting` until the summary lands, a refusal is said plainly, a
//! request a proxy cut waits for the compaction row instead of failing, and a
//! message sent meanwhile waits until the compaction is over.
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// A server that answers its `n`th connection with `responses[n]`: a status
/// line and body, held until `release` gives a permit when `held`. `requests`
/// holds what each was sent, in the order they were accepted.
struct Server {
    address: std::net::SocketAddr,
    release: Arc<tokio::sync::Semaphore>,
    accepted: Arc<AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
}

fn http(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn serve(responses: Vec<(bool, String)>) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let accepted = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (gate, count, seen) = (
        Arc::clone(&release),
        Arc::clone(&accepted),
        Arc::clone(&requests),
    );
    tokio::spawn(async move {
        for (held, response) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let index = count.fetch_add(1, Ordering::SeqCst);
            seen.lock().unwrap().push(String::new());
            let (gate, seen) = (Arc::clone(&gate), Arc::clone(&seen));
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    let read = socket.read(&mut chunk).await.unwrap();
                    request.extend_from_slice(&chunk[..read]);
                    let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                        if read == 0 {
                            break;
                        }
                        continue;
                    };
                    let length = String::from_utf8_lossy(&request[..end])
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length || read == 0 {
                        break;
                    }
                }
                seen.lock().unwrap()[index] = String::from_utf8_lossy(&request).into_owned();
                if held {
                    gate.acquire().await.unwrap().forget();
                }
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    Server {
        address,
        release,
        accepted,
        requests,
    }
}

fn compaction_item(thread: &Thread, summary: &str) -> Item {
    Item {
        id: ItemId::new(),
        thread_id: thread.id,
        turn_id: None,
        sequence: 2,
        data: ItemData::Compaction {
            summary: summary.into(),
            files_touched: Vec::new(),
            outstanding: Vec::new(),
            todo_checkpoint: None,
            tail_start_sequence: None,
            kept_sequences: Vec::new(),
            tool_output_chars: None,
        },
        created_at: Utc::now(),
    }
}

/// `/compact`, which must hand back the keyboard at once.
async fn compact(app: &mut App, client: &crate::api::ApiClient) {
    tokio::time::timeout(
        Duration::from_secs(1),
        app.execute_command("/compact", client),
    )
    .await
    .expect("/compact returns at once")
    .unwrap();
}

/// Poll the way the event loop's tick does until `done`.
async fn poll_until(app: &mut App, label: &str, done: impl Fn(&App) -> bool) {
    for _ in 0..500 {
        app.poll_compactions();
        if done(app) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{label}");
}

async fn accepted(server: &Server, count: usize) {
    for _ in 0..500 {
        if server.accepted.load(Ordering::SeqCst) >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the server never got request {count}");
}

fn band(app: &App) -> String {
    let lines = app.lines((100, 20));
    line_text(lines.last().unwrap())
}

#[tokio::test]
async fn a_slow_compaction_leaves_the_tui_usable_and_ends_with_its_row() {
    let thread = fixture_thread();
    let item = compaction_item(&thread, "real model summary");
    let server = serve(vec![(
        true,
        http(
            "200 OK",
            "application/json",
            &serde_json::to_string(&item).unwrap(),
        ),
    )])
    .await;
    let client =
        crate::api::ApiClient::new(&format!("http://{}", server.address), "test-token").unwrap();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);

    compact(&mut app, &client).await;
    accepted(&server, 1).await;
    app.poll_compactions();
    assert!(band(&app).contains("compacting   "), "{}", band(&app));
    // The prompt is the person's meanwhile.
    app.composer.set_text("next, add docs".into());
    assert_eq!(app.composer.expanded_text(), "next, add docs");

    server.release.add_permits(1);
    poll_until(&mut app, "the summary never landed", |app| {
        app.compacting_since().is_none()
    })
    .await;
    assert_eq!(app.notice, "Compaction checkpoint created");
    assert!(matches!(
        app.items.last().map(|item| &item.data),
        Some(ItemData::Compaction { summary, .. }) if summary == "real model summary"
    ));
    assert!(!band(&app).contains("compacting"));
    assert!(app.notification_toast.is_none());
}

#[tokio::test]
async fn a_second_compaction_shows_the_refusal_plainly_while_the_first_runs_on() {
    let thread = fixture_thread();
    let item = compaction_item(&thread, "first summary");
    let server = serve(vec![
        (
            true,
            http("200 OK", "application/json", &serde_json::to_string(&item).unwrap()),
        ),
        (
            false,
            http(
                "409 Conflict",
                "application/json",
                r#"{"code":"conflict","message":"conflict: this chat is already being compacted","retryable":true}"#,
            ),
        ),
    ])
    .await;
    let client =
        crate::api::ApiClient::new(&format!("http://{}", server.address), "test-token").unwrap();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);

    compact(&mut app, &client).await;
    accepted(&server, 1).await;
    compact(&mut app, &client).await;
    poll_until(&mut app, "the refusal was never shown", |app| {
        app.notification_toast.is_some()
    })
    .await;
    let toast = app.notification_toast.as_ref().unwrap();
    assert_eq!(toast.message, "this chat is already being compacted");
    assert!(band(&app).contains("compacting"), "the first one runs on");

    server.release.add_permits(1);
    poll_until(&mut app, "the first summary never landed", |app| {
        app.compacting_since().is_none()
    })
    .await;
    assert_eq!(app.notice, "Compaction checkpoint created");
}

#[tokio::test]
async fn a_request_a_proxy_cut_waits_for_the_compaction_row() {
    let thread = fixture_thread();
    let cut = || {
        (
            false,
            http(
                "524 A Timeout Occurred",
                "text/html",
                "<html>timeout</html>",
            ),
        )
    };
    let server = serve(vec![cut(), cut()]).await;
    let client =
        crate::api::ApiClient::new(&format!("http://{}", server.address), "test-token").unwrap();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);

    // The server goes on summarizing; the compaction's row says it landed.
    compact(&mut app, &client).await;
    poll_until(&mut app, "the cut request never answered", |app| {
        app.compactions_waiting_for_row() == 1
    })
    .await;
    assert!(app.notification_toast.is_none(), "no failure yet");
    assert!(band(&app).contains("compacting"));
    let landed = compaction_item(&thread, "landed after the cut");
    app.reduce(
        fixture_event(
            "item.created",
            thread.id,
            thread.id,
            serde_json::json!(landed),
        ),
        &client,
    )
    .await
    .unwrap();
    app.poll_compactions();
    assert!(app.compacting_since().is_none());
    assert!(app.notification_toast.is_none());
    assert!(
        app.items
            .iter()
            .any(|item| matches!(&item.data, ItemData::Compaction { summary, .. } if summary == "landed after the cut"))
    );

    // When no compaction ever comes, the failure is said once the wait is over.
    compact(&mut app, &client).await;
    poll_until(&mut app, "the cut request never answered", |app| {
        app.compactions_waiting_for_row() == 1
    })
    .await;
    app.age_compactions(crate::api::COMPACT_TIMEOUT);
    app.poll_compactions();
    assert!(app.compacting_since().is_none());
    assert!(
        app.notification_toast.is_some(),
        "the cut is said in the end"
    );
}

/// What the server says of itself: no input settings, so Enter steers.
fn info() -> (bool, String) {
    (
        false,
        http(
            "200 OK",
            "application/json",
            &serde_json::to_string(&side_server_info(vec![])).unwrap(),
        ),
    )
}

fn turn_started(thread: &Thread) -> (bool, String) {
    (
        false,
        http(
            "200 OK",
            "application/json",
            &serde_json::to_string(&fixture_running_turn(thread.id)).unwrap(),
        ),
    )
}

async fn enter(app: &mut App, text: &str, client: &crate::api::ApiClient) {
    app.composer.set_text(text.into());
    crate::input::handle_key(
        app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        client,
    )
    .await
    .unwrap();
}

/// The requests that started a turn, as they were sent.
fn turns_started(server: &Server) -> Vec<String> {
    server
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.starts_with("POST ") && request.contains("/turns "))
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_message_sent_while_compacting_waits_and_goes_out_once_after_the_row() {
    let mut app = selector_fixture();
    let thread = app.thread.clone();
    let item = compaction_item(&thread, "summary first");
    let server = serve(vec![
        (
            true,
            http(
                "200 OK",
                "application/json",
                &serde_json::to_string(&item).unwrap(),
            ),
        ),
        info(),
        turn_started(&thread),
    ])
    .await;
    let client =
        crate::api::ApiClient::new(&format!("http://{}", server.address), "test-token").unwrap();

    compact(&mut app, &client).await;
    accepted(&server, 1).await;
    enter(&mut app, "next, add docs", &client).await;
    assert!(app.composer.is_empty(), "the message left the prompt");
    assert_eq!(app.held_messages(), ["next, add docs"]);
    let frame = frame_text(&app, (100, 24));
    assert!(frame.contains("Queued · 1"), "{frame}");
    assert!(frame.contains("next, add docs"), "{frame}");
    // Nothing goes out while the summary is written, however often the tick comes.
    for _ in 0..3 {
        app.poll_compactions();
        app.release_held(&client).await;
    }
    assert_eq!(server.accepted.load(Ordering::SeqCst), 1);
    assert!(app.active_turn.is_none());

    server.release.add_permits(1);
    poll_until(&mut app, "the summary never landed", |app| {
        app.compacting_since().is_none()
    })
    .await;
    app.release_held(&client).await;
    app.release_held(&client).await;
    assert!(app.held_messages().is_empty());
    assert!(app.active_turn.is_some(), "the message started its turn");
    let turns = turns_started(&server);
    assert_eq!(turns.len(), 1, "sent once: {turns:?}");
    assert!(turns[0].contains("next, add docs"));
    assert!(!frame_text(&app, (100, 24)).contains("Queued · 1"));
}

#[tokio::test]
async fn a_refused_compaction_lets_the_waiting_message_go_out() {
    let mut app = selector_fixture();
    let thread = app.thread.clone();
    let server = serve(vec![
        (
            true,
            http(
                "400 Bad Request",
                "application/json",
                r#"{"code":"invalid_request","message":"invalid request: nothing to compact"}"#,
            ),
        ),
        info(),
        turn_started(&thread),
    ])
    .await;
    let client =
        crate::api::ApiClient::new(&format!("http://{}", server.address), "test-token").unwrap();

    compact(&mut app, &client).await;
    accepted(&server, 1).await;
    enter(&mut app, "carry on", &client).await;
    app.release_held(&client).await;
    assert!(turns_started(&server).is_empty(), "it waits for the answer");
    assert_eq!(app.held_messages(), ["carry on"]);
    server.release.add_permits(1);
    poll_until(&mut app, "the refusal never came", |app| {
        app.compacting_since().is_none()
    })
    .await;
    assert_eq!(
        app.notification_toast
            .as_ref()
            .map(|toast| toast.message.as_str()),
        Some("nothing to compact")
    );
    app.release_held(&client).await;
    assert!(app.held_messages().is_empty());
    let turns = turns_started(&server);
    assert_eq!(turns.len(), 1);
    assert!(turns[0].contains("carry on"));
}

#[tokio::test]
async fn a_waiting_message_that_cannot_go_out_comes_back_ahead_of_the_draft() {
    let mut app = selector_fixture();
    let thread = app.thread.clone();
    let item = compaction_item(&thread, "summary first");
    let server = serve(vec![
        (
            false,
            http(
                "200 OK",
                "application/json",
                &serde_json::to_string(&item).unwrap(),
            ),
        ),
        (
            false,
            http(
                "500 Internal Server Error",
                "text/plain",
                "Internal Server Error",
            ),
        ),
    ])
    .await;
    let client =
        crate::api::ApiClient::new(&format!("http://{}", server.address), "test-token").unwrap();

    compact(&mut app, &client).await;
    // Held before the answer is taken.
    enter(&mut app, "first message", &client).await;
    app.composer.set_text("typed meanwhile".into());
    poll_until(&mut app, "the summary never landed", |app| {
        app.compacting_since().is_none()
    })
    .await;
    app.release_held(&client).await;
    assert!(app.held_messages().is_empty());
    assert_eq!(
        app.composer.expanded_text(),
        "first message\n\ntyped meanwhile"
    );
    assert!(app.notification_toast.is_some(), "why it did not go out");
    assert!(turns_started(&server).is_empty());
}
