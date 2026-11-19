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

/// A server whose release finishes a summary whose request was cut.
fn finishing(app: &mut App) {
    app.server_info = Some(haarness_protocol::ServerInfo {
        version: "0.1.24".into(),
        ..side_server_info(vec![])
    });
}

#[tokio::test]
async fn a_second_compaction_is_refused_here_while_the_first_runs() {
    let thread = fixture_thread();
    let item = compaction_item(&thread, "first summary");
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
    compact(&mut app, &client).await;
    assert_eq!(app.notice, crate::compaction::ALREADY_COMPACTING);
    // Said on a card: the band says `compacting` meanwhile.
    assert_eq!(
        app.notification_toast
            .as_ref()
            .map(|toast| toast.message.as_str()),
        Some(crate::compaction::ALREADY_COMPACTING)
    );
    assert_eq!(server.accepted.load(Ordering::SeqCst), 1, "not asked again");
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
    finishing(&mut app);

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
    enter(&mut app, "second message", &client).await;
    app.composer.set_text("typed meanwhile".into());
    poll_until(&mut app, "the summary never landed", |app| {
        app.compacting_since().is_none()
    })
    .await;
    app.release_held(&client).await;
    // Both come back at once, in order, ahead of the draft, with one toast.
    assert!(app.held_messages().is_empty());
    assert_eq!(
        app.composer.expanded_text(),
        "first message\n\nsecond message\n\ntyped meanwhile"
    );
    assert!(app.notification_toast.is_some(), "why it did not go out");
    assert!(turns_started(&server).is_empty());
}

/// A chat being compacted, with nothing needed from a server: the request
/// fails at once, and nothing takes its answer until the test polls.
fn compacting_offline() -> App {
    let mut app = selector_fixture();
    app.start_compaction(None, &offline_client());
    app
}

#[tokio::test]
async fn held_messages_come_back_on_esc_or_a_click_on_their_strip() {
    let mut app = compacting_offline();
    let client = offline_client();
    enter(&mut app, "first", &client).await;
    enter(&mut app, "second", &client).await;
    app.composer.set_text("draft".into());
    press_key(&mut app, KeyCode::Esc).await;
    assert!(app.held_messages().is_empty());
    assert_eq!(app.composer.expanded_text(), "first\n\nsecond\n\ndraft");

    app.composer.clear();
    enter(&mut app, "third", &client).await;
    assert!(frame_text(&app, (100, 30)).contains("Queued · 1"));
    click_hit(
        &mut app,
        &crate::view::frame::HitTarget::HeldMessages,
        &client,
    )
    .await;
    assert!(app.held_messages().is_empty());
    assert_eq!(app.composer.expanded_text(), "third");
}

#[tokio::test]
async fn leaving_the_chat_or_quitting_keeps_held_messages_as_its_draft() {
    let mut app = compacting_offline();
    let thread = app.thread.clone();
    let other = app.threads[1].clone();
    let client = offline_client();
    enter(&mut app, "first", &client).await;
    enter(&mut app, "second", &client).await;
    app.composer.set_text("draft".into());
    let (navigation, server) = navigation_client().await;
    app.switch_thread(other.clone(), &navigation).await.unwrap();
    server.await.unwrap();
    assert!(
        app.held.is_empty(),
        "nothing waits for a chat that is not open"
    );
    assert_eq!(
        app.drafts.get(&thread.id).map(|draft| draft.text.as_str()),
        Some("first\n\nsecond\n\ndraft")
    );

    // Quitting does the same for the open chat: its draft is saved as it is.
    let mut app = compacting_offline();
    enter(&mut app, "waiting at quit", &client).await;
    app.return_held();
    assert_eq!(
        crate::servers::Draft::from_app(&app).text,
        "waiting at quit"
    );
}

#[tokio::test]
async fn shell_commands_and_command_prompts_wait_for_the_compaction() {
    let mut app = compacting_offline();
    let client = offline_client();
    press_key(&mut app, KeyCode::Char('!')).await;
    app.composer.set_text("git status".into());
    press_key(&mut app, KeyCode::Enter).await;
    assert_eq!(
        app.composer.expanded_text(),
        "git status",
        "the command stays"
    );
    assert_eq!(app.notice, crate::compaction::WAIT_FOR_COMPACTION);

    app.shell_mode = false;
    app.composer.clear();
    assert!(!app.submit_prompt("/plan it".into(), &client).await.unwrap());
    assert_eq!(app.notice, crate::compaction::WAIT_FOR_COMPACTION);
}

#[tokio::test]
async fn the_wait_running_out_lets_held_messages_go_out() {
    let mut app = selector_fixture();
    finishing(&mut app);
    let thread = app.thread.clone();
    let server = serve(vec![
        (
            false,
            http(
                "524 A Timeout Occurred",
                "text/html",
                "<html>timeout</html>",
            ),
        ),
        info(),
        turn_started(&thread),
    ])
    .await;
    let client =
        crate::api::ApiClient::new(&format!("http://{}", server.address), "test-token").unwrap();
    compact(&mut app, &client).await;
    enter(&mut app, "after the wait", &client).await;
    poll_until(&mut app, "the cut never answered", |app| {
        app.compactions_waiting_for_row() == 1
    })
    .await;
    app.release_held(&client).await;
    assert_eq!(app.held_messages(), ["after the wait"], "still waiting");

    app.age_compactions(crate::api::COMPACT_TIMEOUT);
    app.poll_compactions();
    assert!(app.notification_toast.is_some(), "the cut is said");
    app.release_held(&client).await;
    assert!(app.held_messages().is_empty());
    let turns = turns_started(&server);
    assert_eq!(turns.len(), 1);
    assert!(turns[0].contains("after the wait"));
}

#[tokio::test]
async fn a_turn_another_client_starts_ends_the_wait_and_lets_held_messages_go() {
    let mut app = selector_fixture();
    finishing(&mut app);
    let thread = app.thread.clone();
    let server = serve(vec![
        (
            false,
            http(
                "524 A Timeout Occurred",
                "text/html",
                "<html>timeout</html>",
            ),
        ),
        info(),
        turn_started(&thread),
    ])
    .await;
    let client =
        crate::api::ApiClient::new(&format!("http://{}", server.address), "test-token").unwrap();
    compact(&mut app, &client).await;
    enter(&mut app, "said meanwhile", &client).await;
    poll_until(&mut app, "the cut never answered", |app| {
        app.compactions_waiting_for_row() == 1
    })
    .await;
    app.reduce(
        fixture_event("turn.started", thread.id, thread.id, serde_json::json!({})),
        &client,
    )
    .await
    .unwrap();
    assert!(app.compacting_since().is_none());
    assert_eq!(
        app.notification_toast
            .as_ref()
            .map(|toast| toast.message.as_str()),
        Some(crate::compaction::CHAT_CHANGED)
    );
    app.release_held(&client).await;
    assert!(app.held_messages().is_empty());
    assert_eq!(turns_started(&server).len(), 1);
}

/// What one failed `/compact` request leaves: a wait for the compaction's
/// row (`true`), or the failure said at once.
/// Boxed: the app it holds is large.
fn waits_after(
    response: String,
    finishes: bool,
    out_for: Duration,
) -> std::pin::Pin<Box<impl std::future::Future<Output = bool>>> {
    Box::pin(async move {
        let mut app = selector_fixture();
        if finishes {
            finishing(&mut app);
        }
        let server = serve(vec![(false, response)]).await;
        let client =
            crate::api::ApiClient::new(&format!("http://{}", server.address), "test-token")
                .unwrap();
        compact(&mut app, &client).await;
        // As if the request had been out that long when its answer came.
        app.age_compactions(out_for);
        poll_until(&mut app, "the request never answered", |app| {
            app.compactions_waiting_for_row() == 1 || app.compacting_since().is_none()
        })
        .await;
        let waits = app.compactions_waiting_for_row() == 1;
        assert_eq!(waits, app.notification_toast.is_none());
        waits
    })
}

#[tokio::test]
async fn only_answers_lost_on_the_way_from_a_server_that_finishes_them_are_waited_for() {
    let now = Duration::ZERO;
    let json = |status: &str, body: &str| http(status, "application/json", body);
    let cloudflare = || {
        http(
            "524 A Timeout Occurred",
            "text/html",
            "<html>timeout</html>",
        )
    };
    assert!(waits_after(cloudflare(), true, now).await);
    assert!(
        !waits_after(cloudflare(), false, now).await,
        "an older server's summary died with its request"
    );
    // Any 5xx a proxy says with nothing to read.
    assert!(
        waits_after(
            http(
                "500 Internal Server Error",
                "text/html",
                "<html>oops</html>"
            ),
            true,
            now,
        )
        .await
    );
    assert!(waits_after(http("503 Service Unavailable", "text/plain", ""), true, now).await);
    // A sentence is someone's answer, not a cut.
    assert!(
        !waits_after(
            http(
                "502 Bad Gateway",
                "text/plain",
                "The upstream server sent an invalid response.",
            ),
            true,
            now,
        )
        .await
    );
    // Cloud lost the computer's connection: a cut once the request had been
    // out a while; said at once, the computer was not there to take it.
    let lost = || {
        json(
            "409 Conflict",
            r#"{"error":{"code":"conflict","message":"Remote host is offline or its connection changed. Try again."}}"#,
        )
    };
    assert!(waits_after(lost(), true, Duration::from_secs(6)).await);
    assert!(!waits_after(lost(), true, now).await);
    // The computer's runtime failed the request, or the model the summary.
    assert!(
        !waits_after(
            json(
                "502 Bad Gateway",
                r#"{"error":{"code":"bad_gateway","message":"Your computer couldn't answer. Check that Haarness is open and up to date on it, then try again."}}"#,
            ),
            true,
            now,
        )
        .await
    );
    assert!(
        !waits_after(
            json(
                "502 Bad Gateway",
                r#"{"code":"provider_error","message":"provider error: overloaded"}"#,
            ),
            true,
            now,
        )
        .await
    );
    // A server on this computer that dropped the request is gone, and its
    // summary with it.
    assert!(!waits_after(String::new(), true, now).await);
}

#[tokio::test]
async fn another_clients_compaction_is_waited_for_and_said_plainly_if_it_never_lands() {
    let mut app = selector_fixture();
    let server = serve(vec![(
        false,
        http(
            "409 Conflict",
            "application/json",
            r#"{"code":"conflict","message":"conflict: this chat is already being compacted"}"#,
        ),
    )])
    .await;
    let client =
        crate::api::ApiClient::new(&format!("http://{}", server.address), "test-token").unwrap();
    compact(&mut app, &client).await;
    poll_until(&mut app, "the request never answered", |app| {
        app.compactions_waiting_for_row() == 1
    })
    .await;
    assert!(app.notification_toast.is_none());
    app.age_compactions(crate::api::COMPACT_TIMEOUT);
    app.poll_compactions();
    assert_eq!(
        app.notification_toast
            .as_ref()
            .map(|toast| toast.message.as_str()),
        Some(crate::compaction::DID_NOT_COME_BACK)
    );
}

/// A wait for chat A's cut compaction while chat B is open.
async fn waiting_in_another_chat() -> (App, Thread) {
    let mut app = selector_fixture();
    finishing(&mut app);
    let waiting = app.thread.clone();
    let server = serve(vec![(
        false,
        http(
            "524 A Timeout Occurred",
            "text/html",
            "<html>timeout</html>",
        ),
    )])
    .await;
    let client =
        crate::api::ApiClient::new(&format!("http://{}", server.address), "test-token").unwrap();
    compact(&mut app, &client).await;
    poll_until(&mut app, "the cut never answered", |app| {
        app.compactions_waiting_for_row() == 1
    })
    .await;
    let other = app.threads[1].clone();
    let (navigation, server) = navigation_client().await;
    app.switch_thread(other, &navigation).await.unwrap();
    server.await.unwrap();
    (app, waiting)
}

#[tokio::test]
async fn a_compaction_landing_while_another_chat_is_open_ends_its_wait() {
    let (mut app, waiting) = Box::pin(waiting_in_another_chat()).await;
    let landed = compaction_item(&waiting, "landed meanwhile");
    app.reduce(
        fixture_event(
            "item.created",
            waiting.id,
            waiting.id,
            serde_json::json!(landed),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.compactions_waiting_for_row(), 0);
    // Nothing is said when the wait would have run out.
    app.age_compactions(crate::api::COMPACT_TIMEOUT);
    app.poll_compactions();
    assert!(app.notification_toast.is_none());
}

#[tokio::test]
async fn a_landing_whose_row_went_unseen_ends_the_wait_once_the_chat_is_read() {
    let (mut app, waiting) = Box::pin(waiting_in_another_chat()).await;
    // Back in the chat: its history, read again, holds the compaction.
    app.thread = waiting.clone();
    app.items = vec![compaction_item(&waiting, "landed unseen")];
    app.compaction_rows_loaded();
    assert_eq!(app.compactions_waiting_for_row(), 0);
    assert!(app.compacting_since().is_none());
}

#[tokio::test]
async fn held_messages_never_come_back_as_a_shell_command() {
    let mut app = compacting_offline();
    let client = offline_client();
    enter(&mut app, "fix the failing tests", &client).await;
    press_key(&mut app, KeyCode::Char('!')).await;
    app.composer.set_text("cargo test".into());
    press_key(&mut app, KeyCode::Enter).await;
    assert!(app.shell_mode, "refused, the command stays");
    assert!(app.return_held());
    assert!(!app.shell_mode, "the prompt is prose again");
    assert_eq!(
        app.composer.expanded_text(),
        "fix the failing tests\n\n!cargo test"
    );
    assert!(!crate::servers::Draft::from_app(&app).shell);
}

#[tokio::test]
async fn a_server_restart_ends_the_wait_for_a_cut_compaction() {
    let mut app = selector_fixture();
    finishing(&mut app);
    let server = serve(vec![(
        false,
        http(
            "524 A Timeout Occurred",
            "text/html",
            "<html>timeout</html>",
        ),
    )])
    .await;
    let client =
        crate::api::ApiClient::new(&format!("http://{}", server.address), "test-token").unwrap();
    compact(&mut app, &client).await;
    poll_until(&mut app, "the cut never answered", |app| {
        app.compactions_waiting_for_row() == 1
    })
    .await;
    app.server_restarted();
    assert!(app.compacting_since().is_none());
    assert!(app.notification_toast.is_some());
}

#[tokio::test]
async fn a_summary_a_turn_voided_is_said_in_plain_words() {
    let mut app = selector_fixture();
    let server = serve(vec![(
        false,
        http(
            "409 Conflict",
            "application/json",
            r#"{"code":"conflict","message":"conflict: thread changed while the compaction summary was generated; retry compaction"}"#,
        ),
    )])
    .await;
    let client =
        crate::api::ApiClient::new(&format!("http://{}", server.address), "test-token").unwrap();
    compact(&mut app, &client).await;
    poll_until(&mut app, "the refusal never came", |app| {
        app.compacting_since().is_none()
    })
    .await;
    assert_eq!(
        app.notification_toast
            .as_ref()
            .map(|toast| toast.message.as_str()),
        Some(crate::compaction::CHAT_CHANGED)
    );
}
