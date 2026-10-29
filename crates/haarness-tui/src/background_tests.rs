//! Shell commands in the background: rows from reports and checkpoints, the
//! move key, the output view, `/tasks`, the sidebar, the band and the
//! session list, against the server's contract.
use super::*;
use crate::background::fixture_info;
use haarness_protocol::{ShellCommandInfo, ShellCommandOutput, ShellCommandState};
use std::sync::{Arc, Mutex};

/// An app on `thread` whose server keeps commands in the background.
fn app_on(thread: &Thread, items: Vec<Item>) -> App {
    let mut app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    app.server_info = Some(side_server_info(vec![
        haarness_protocol::BACKGROUND_COMMANDS_CAPABILITY.into(),
    ]));
    app
}

/// A `bash` call, and its answer when `result` names one: the output and
/// the command behind it (`metadata.command`).
fn bash(
    thread: &Thread,
    turn: TurnId,
    call: &str,
    command: &str,
    result: Option<(&str, &str, bool)>,
) -> Vec<Item> {
    let mut items = vec![message_item_for_turn(
        1,
        thread,
        turn,
        ItemData::ToolCall {
            call_id: call.into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": command }),
            provider_metadata: serde_json::Value::Null,
        },
    )];
    if let Some((output, id, background)) = result {
        items.push(message_item_for_turn(
            2,
            thread,
            turn,
            ItemData::ToolResult {
                call_id: call.into(),
                name: "bash".into(),
                output: output.into(),
                is_error: false,
                metadata: serde_json::json!({
                    "command": { "id": id, "background": background, "startedAt": Utc::now() }
                }),
            },
        ));
    }
    items
}

fn info_for(
    thread: &Thread,
    turn: TurnId,
    id: &str,
    call: &str,
    background: bool,
) -> ShellCommandInfo {
    let mut info = fixture_info(
        id,
        thread.id,
        call,
        "npm run dev",
        background,
        ShellCommandState::Running,
    );
    info.turn_id = turn;
    info
}

fn updated(thread: &Thread, info: &ShellCommandInfo) -> haarness_protocol::EventEnvelope {
    fixture_event(
        "command.updated",
        info.thread_id,
        thread.id,
        serde_json::to_value(info).unwrap(),
    )
}

fn frame_text(app: &App, size: (u32, u32)) -> Vec<String> {
    app.lines(size).iter().map(line_text).collect()
}

fn row_with<'a>(rows: &'a [String], text: &str) -> &'a str {
    rows.iter()
        .find(|row| row.contains(text))
        .unwrap_or_else(|| panic!("no row with {text:?}:\n{}", rows.join("\n")))
}

/// A server that answers each request with `answer(request line, body)` and
/// keeps every request it saw (its line and body).
async fn recording_server(
    answer: impl Fn(&str, &str) -> (&'static str, String) + Send + Sync + 'static,
) -> (crate::api::ApiClient, Arc<Mutex<Vec<(String, String)>>>) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            let answer = answer.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut chunk = [0_u8; 4096];
                let mut expected = None;
                while expected.is_none_or(|length| request.len() < length) {
                    let read = socket.read(&mut chunk).await.unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if expected.is_none()
                        && let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        expected = Some(end + 4 + length);
                    }
                }
                let text = String::from_utf8_lossy(&request).into_owned();
                let line = text.lines().next().unwrap_or_default().to_owned();
                let body = text
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body.to_owned())
                    .unwrap_or_default();
                let (status, reply) = answer(&line, &body);
                log.lock().unwrap().push((line, body));
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    (
        crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap(),
        seen,
    )
}

fn requested(seen: &Arc<Mutex<Vec<(String, String)>>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|(line, _)| line.clone())
        .collect()
}

#[tokio::test]
async fn a_command_in_the_background_reads_running_then_how_it_ended() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    let items = bash(
        &thread,
        turn,
        "call-1",
        "npm run dev",
        Some(("Started in the background as c1.", "c1", true)),
    );
    let mut app = app_on(&thread, items);
    let client = offline_client();
    let mut info = info_for(&thread, turn, "c1", "call-1", true);
    info.started_at = Utc::now() - chrono::Duration::seconds(72);
    app.reduce(updated(&thread, &info), &client).await.unwrap();

    let rows = frame_text(&app, (100, 30));
    let row = row_with(&rows, "bash npm run dev");
    assert!(row.contains("in background · 1m 12s"), "{row}");
    // The agent's note in the call's answer is never shown.
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("Started in the background")),
        "{}",
        rows.join("\n")
    );
    // The row breathes like a running task.
    let lines = app.lines((100, 30));
    let marker = lines
        .iter()
        .find(|line| line_text(line).contains("bash npm run dev"))
        .unwrap()
        .spans
        .first()
        .unwrap()
        .clone();
    assert_eq!(marker.fg, app.theme.accent);

    for (state, exit, ending, glyph) in [
        (
            ShellCommandState::Exited,
            Some(0),
            "in background · completed",
            '✓',
        ),
        (
            ShellCommandState::Exited,
            Some(2),
            "in background · exit 2",
            '✗',
        ),
        (
            ShellCommandState::Stopping,
            None,
            "in background · stopping",
            '⠋',
        ),
        (
            ShellCommandState::Stopped,
            None,
            "in background · stopped",
            '⊘',
        ),
        (
            ShellCommandState::TimedOut,
            None,
            "in background · ran out of time",
            '✗',
        ),
        (
            ShellCommandState::Interrupted,
            None,
            "in background · cut off by a restart",
            '⊘',
        ),
    ] {
        info.state = state;
        info.exit_code = exit;
        app.reduce(updated(&thread, &info), &client).await.unwrap();
        app.tick = 0;
        let rows = frame_text(&app, (100, 30));
        let row = row_with(&rows, "bash npm run dev");
        assert!(row.contains(ending), "{state:?}: {row}");
        assert!(row.contains(glyph), "{state:?}: {row}");
    }
}

#[test]
fn a_restart_leaves_the_rows_end_in_the_chats_checkpoints() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    let mut items = bash(
        &thread,
        turn,
        "call-1",
        "cargo watch",
        Some(("Started in the background as c9.", "c9", true)),
    );
    let checkpoint = |sequence, state, exit_code| {
        message_item_for_turn(
            sequence,
            &thread,
            turn,
            ItemData::CommandState {
                call_id: "call-1".into(),
                command_id: "c9".into(),
                state,
                exit_code,
                signal: None,
                tail: "watching…".into(),
                started_at: Utc::now(),
                ended_at: None,
                moved_by: Some(haarness_protocol::ShellCommandMovedBy::Agent),
            },
        )
    };
    items.push(checkpoint(3, ShellCommandState::Running, None));
    items.push(checkpoint(4, ShellCommandState::Interrupted, None));
    // No report: the server restarted and forgot it.
    let app = app_on(&thread, items.clone());
    let rows = frame_text(&app, (100, 30));
    let row = row_with(&rows, "bash cargo watch");
    assert!(
        row.contains("in background · cut off by a restart"),
        "{row}"
    );
    assert!(row.contains('⊘'), "{row}");

    items.pop();
    items.push(checkpoint(4, ShellCommandState::Exited, Some(0)));
    let app = app_on(&thread, items);
    let rows = frame_text(&app, (100, 30));
    assert!(row_with(&rows, "bash cargo watch").contains("in background · completed"));
}

#[test]
fn a_command_sent_to_the_background_reads_so_before_its_report_comes() {
    let thread = fixture_thread();
    let items = bash(
        &thread,
        TurnId::new(),
        "call-1",
        "npm test",
        Some(("partial output", "c3", true)),
    );
    let app = app_on(&thread, items);
    let rows = frame_text(&app, (100, 30));
    let row = row_with(&rows, "bash npm test");
    assert!(row.contains("in background"), "{row}");
    assert!(!rows.iter().any(|row| row.contains("partial output")));
}

#[test]
fn rows_keep_their_state_at_40_columns_and_read_in_arabic() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    let items = bash(
        &thread,
        turn,
        "call-1",
        "npm run dev -- --port 5173 --host 0.0.0.0",
        Some(("ok", "c1", true)),
    );
    let mut app = app_on(&thread, items);
    let mut info = info_for(&thread, turn, "c1", "call-1", true);
    info.started_at = Utc::now() - chrono::Duration::seconds(72);
    let conversation = app.conversation_id();
    app.shell_commands.update(info.clone(), conversation);

    let lines = app.lines((40, 20));
    let row = lines
        .iter()
        .find(|line| line_text(line).contains("bash npm"))
        .expect("row at 40 columns");
    assert!(row.width() <= 40, "{:?}", line_text(row));
    assert!(
        line_text(row).trim_end().ends_with("in background"),
        "{}",
        line_text(row)
    );

    info.state = ShellCommandState::Exited;
    info.exit_code = Some(2);
    app.shell_commands.update(info, conversation);
    let lines = app.lines((40, 20));
    let row = lines
        .iter()
        .find(|line| line_text(line).contains("bash npm"))
        .unwrap();
    assert!(row.width() <= 40);
    assert!(
        line_text(row).trim_end().ends_with("exit 2"),
        "{}",
        line_text(row)
    );

    app.language = crate::locale::Language::Arabic;
    let rows = frame_text(&app, (100, 30));
    let row = row_with(&rows, "bash npm");
    assert!(row.contains("في الخلفية · رمز الخروج 2"), "{row}");
}

#[tokio::test]
async fn a_foreground_command_names_the_move_key_after_a_few_seconds() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    let items = bash(&thread, turn, "call-1", "cargo build", None);
    let mut app = app_on(&thread, items);
    app.active_turn = Some(turn);
    let conversation = app.conversation_id();
    let mut info = info_for(&thread, turn, "c1", "call-1", false);
    info.command = "cargo build".into();
    app.shell_commands.update(info.clone(), conversation);
    let hint = "ctrl+x sends to background";
    // Too soon: a quick command never flashes it.
    assert!(
        !frame_text(&app, (100, 30))
            .iter()
            .any(|row| row.contains(hint))
    );

    info.started_at = Utc::now() - chrono::Duration::seconds(4);
    app.shell_commands.update(info.clone(), conversation);
    let rows = frame_text(&app, (100, 30));
    assert!(row_with(&rows, "bash cargo build").contains(hint));
    // The running row opens its output.
    let _ = app.lines((100, 30));
    assert!(
        app.hit_rows
            .borrow()
            .iter()
            .any(|hit| hit.target == crate::view::frame::HitTarget::CommandOpen("c1".into()))
    );

    // Rebound, the hint names the person's key; unbound, it says nothing.
    app.keybindings
        .insert(crate::background::BACKGROUND_ACTION.into(), "alt+b".into());
    assert!(
        row_with(&frame_text(&app, (100, 30)), "bash cargo build")
            .contains("alt+b sends to background")
    );
    app.keybindings
        .insert(crate::background::BACKGROUND_ACTION.into(), "none".into());
    assert!(
        !frame_text(&app, (100, 30))
            .iter()
            .any(|row| row.contains("sends to background"))
    );
    app.keybindings
        .insert(crate::background::BACKGROUND_ACTION.into(), "ctrl+x".into());

    // A person's own `!` command never moves.
    let mut shell = info.clone();
    shell.call_id = "shell-1".into();
    let shell_items = bash(&thread, turn, "shell-1", "cargo build", None);
    let mut own = app_on(&thread, shell_items);
    own.shell_commands.update(shell, conversation);
    assert!(
        !frame_text(&own, (100, 30))
            .iter()
            .any(|row| row.contains(hint))
    );

    // Narrow, the hint goes before anything else does.
    let rows = frame_text(&app, (40, 20));
    assert!(!rows.iter().any(|row| row.contains(hint)));
    assert!(row_with(&rows, "bash cargo build").width() <= 40);
}

#[test]
fn ctrl_x_is_the_background_key_and_ctrl_b_stays_the_sidebar() {
    let defaults: std::collections::BTreeMap<String, String> = crate::keybindings::ACTIONS
        .iter()
        .map(|(action, _, key)| ((*action).to_owned(), (*key).to_owned()))
        .collect();
    crate::keybindings::validate(&defaults).unwrap();
    assert_eq!(
        crate::keybindings::resolve(
            &defaults,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)
        ),
        Some("background")
    );
    assert_eq!(
        crate::keybindings::resolve(
            &defaults,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)
        ),
        None
    );
}

#[tokio::test]
async fn ctrl_x_sends_subagents_and_commands_to_the_background_and_keeps_the_draft() {
    let thread = fixture_thread();
    let moved = serde_json::to_string(&vec![info_for(
        &thread,
        TurnId::new(),
        "c1",
        "call-1",
        true,
    )])
    .unwrap();
    let (client, seen) = recording_server(move |line, _| {
        if line.contains("/commands/background") {
            ("200 OK", moved.clone())
        } else {
            ("200 OK", "[]".into())
        }
    })
    .await;
    let mut app = app_on(&thread, vec![]);
    app.composer.set_text("keep this draft".into());
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.composer.text(), "keep this draft");
    let lines = requested(&seen);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with(&format!("POST /v1/threads/{}/background ", thread.id))),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with(&format!(
            "POST /v1/threads/{}/commands/background ",
            thread.id
        ))),
        "{lines:?}"
    );
    assert_eq!(
        app.notice,
        "Command moved to background · its result will return here"
    );

    // An older server moves subagents only, as before.
    let (client, seen) = recording_server(|_, _| ("200 OK", "[]".into())).await;
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.execute_command("/background", &client).await.unwrap();
    let lines = requested(&seen);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("/background"));
    assert_eq!(app.notice, "No matching foreground subagent is running");
}

#[tokio::test]
async fn background_with_a_command_id_moves_that_command_or_says_why_not() {
    let thread = fixture_thread();
    let (client, seen) = recording_server(|line, _| {
        if line.contains("commandId=c7") {
            (
                "409 Conflict",
                serde_json::json!({"code": "conflict", "message": "it ended", "retryable": false})
                    .to_string(),
            )
        } else {
            ("200 OK", "[]".into())
        }
    })
    .await;
    let mut app = app_on(&thread, vec![]);
    app.execute_command("/background c7", &client)
        .await
        .unwrap();
    // The server's own words say why.
    assert_eq!(app.notice, "it ended");
    assert!(
        requested(&seen)
            .iter()
            .all(|line| !line.contains("childThreadId")),
        "a command id is not a child's"
    );
    app.execute_command("/background c8", &client)
        .await
        .unwrap();
    assert_eq!(app.notice, "Nothing is running in the foreground");
}

#[tokio::test]
async fn this_client_says_it_follows_background_work_and_scripts_do_not() {
    let thread = fixture_thread();
    let turn = serde_json::to_string(&fixture_running_turn(thread.id)).unwrap();
    let (client, seen) = recording_server(move |line, _| {
        if line.contains("/input") {
            (
                "200 OK",
                serde_json::json!({"queued": true, "requestId": haarness_protocol::RequestId::new()})
                    .to_string(),
            )
        } else {
            ("200 OK", turn.clone())
        }
    })
    .await;
    client
        .start_turn(thread.id, haarness_protocol::StartTurnRequest::default())
        .await
        .unwrap();
    let following = client.clone().follows_background();
    following
        .start_turn(thread.id, haarness_protocol::StartTurnRequest::default())
        .await
        .unwrap();
    let _ = following
        .queue_input(thread.id, haarness_protocol::QueueInputRequest::default())
        .await;
    let bodies: Vec<serde_json::Value> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, body)| serde_json::from_str(body).unwrap())
        .collect();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[0].get("followsBackground"), None);
    assert_eq!(bodies[1]["followsBackground"], true);
    assert_eq!(bodies[2]["followsBackground"], true);
}

fn page(text: &str, start: u64, settled: bool, state: ShellCommandState) -> ShellCommandOutput {
    ShellCommandOutput {
        text: text.into(),
        start,
        cursor: start + text.len() as u64,
        size: start + text.len() as u64,
        more: false,
        settled,
        state,
        exit_code: settled.then_some(0),
    }
}

#[tokio::test]
async fn clicking_a_row_opens_its_output_full_screen_and_esc_comes_back() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    let items = bash(
        &thread,
        turn,
        "call-1",
        "npm run dev",
        Some(("ok", "c1", true)),
    );
    let mut app = app_on(&thread, items);
    let conversation = app.conversation_id();
    app.shell_commands
        .update(info_for(&thread, turn, "c1", "call-1", true), conversation);
    let client = offline_client();
    let lines = app.lines((100, 30));
    let row = lines
        .iter()
        .position(|line| line_text(line).contains("bash npm run dev"))
        .unwrap();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column: 6,
                row: u16::try_from(row).unwrap(),
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
    }
    assert!(matches!(app.overlay, crate::state::Overlay::CommandOutput));
    app.command_view.as_mut().unwrap().apply(page(
        "\u{1b}[32m> vite dev\u{1b}[0m\n  ready in 312ms\nbuilding 10%\rbuilding 100%\n",
        0,
        false,
        ShellCommandState::Running,
    ));
    let rows = frame_text(&app, (100, 30));
    assert!(rows[0].starts_with(" bash  npm run dev"), "{}", rows[0]);
    assert!(
        rows[0].trim_end().ends_with("in background · 0s"),
        "{}",
        rows[0]
    );
    assert!(rows.iter().any(|row| row == "  > vite dev"), "{rows:#?}");
    assert!(rows.iter().any(|row| row == "    ready in 312ms"));
    assert!(rows.iter().any(|row| row == "  building 100%"));
    let footer = rows.last().unwrap();
    assert!(
        footer.contains("esc back") && footer.contains("s stop"),
        "{footer}"
    );
    assert!(
        !footer.contains("end follows"),
        "following already: {footer}"
    );
    // Narrow, whole hints go, scrolling first; the way out and Stop stay.
    let narrow = frame_text(&app, (40, 12));
    assert_eq!(narrow.last().unwrap().trim_end(), "  esc back  ·  s stop");
    assert!(narrow[0].starts_with(" bash  npm run dev"), "{}", narrow[0]);
    // A click on the view leaves it open.
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column: 10,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
    }
    assert!(matches!(app.overlay, crate::state::Overlay::CommandOutput));

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(app.overlay, crate::state::Overlay::None));
    // Back in the chat, the view's reads stop.
    app.poll_command_output(&client);
    assert!(app.command_view.is_none());
}

#[tokio::test]
async fn the_output_view_scrolls_follows_and_stops_its_command() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    let mut info = info_for(&thread, turn, "c1", "call-1", true);
    let stopping = {
        let mut stopping = info.clone();
        stopping.state = ShellCommandState::Stopping;
        serde_json::to_string(&stopping).unwrap()
    };
    let (client, seen) = recording_server(move |line, _| {
        if line.contains("/stop") {
            ("200 OK", stopping.clone())
        } else {
            ("200 OK", "{}".into())
        }
    })
    .await;
    let mut app = app_on(&thread, vec![]);
    let conversation = app.conversation_id();
    app.shell_commands.update(info.clone(), conversation);
    app.open_command_output("c1", None);
    let text = (1..=200)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    app.command_view
        .as_mut()
        .unwrap()
        .apply(page(&text, 0, false, ShellCommandState::Running));
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

    let rows = frame_text(&app, (80, 20));
    assert!(
        rows.iter().any(|row| row == "  line 200"),
        "follows the end"
    );
    crate::input::handle_key(&mut app, key(KeyCode::PageUp), &client)
        .await
        .unwrap();
    let rows = frame_text(&app, (80, 20));
    assert!(!rows.iter().any(|row| row == "  line 200"));
    assert!(rows.last().unwrap().contains("end follows"));
    // More output does not move a view scrolled back.
    let first = rows[2].clone();
    app.command_view.as_mut().unwrap().apply(page(
        "line 201\n",
        text.len() as u64,
        false,
        ShellCommandState::Running,
    ));
    assert_eq!(frame_text(&app, (80, 20))[2], first);
    crate::input::handle_key(&mut app, key(KeyCode::Char('g')), &client)
        .await
        .unwrap();
    assert_eq!(frame_text(&app, (80, 20))[2], "  line 1");
    crate::input::handle_key(&mut app, key(KeyCode::End), &client)
        .await
        .unwrap();
    assert!(
        frame_text(&app, (80, 20))
            .iter()
            .any(|row| row == "  line 201")
    );

    crate::input::handle_key(&mut app, key(KeyCode::Char('s')), &client)
        .await
        .unwrap();
    assert!(requested(&seen).iter().any(|line| {
        line.starts_with(&format!("POST /v1/threads/{}/commands/c1/stop ", thread.id))
    }));
    let rows = frame_text(&app, (80, 20));
    assert!(
        rows[0].trim_end().ends_with("in background · stopping"),
        "{}",
        rows[0]
    );
    assert!(!rows.last().unwrap().contains("s stop"));
    // It keeps reading until the command has settled.
    assert!(app.command_view.as_ref().unwrap().wanted);
    info.state = ShellCommandState::Stopped;
    app.reduce(updated(&thread, &info), &client).await.unwrap();
    app.command_view.as_mut().unwrap().apply(page(
        "bye\n",
        text.len() as u64 + 9,
        true,
        ShellCommandState::Stopped,
    ));
    let rows = frame_text(&app, (80, 20));
    assert!(
        rows[0].trim_end().ends_with("in background · stopped"),
        "{}",
        rows[0]
    );
    assert!(rows.iter().any(|row| row == "  bye"));
    assert!(app.command_view.as_ref().unwrap().settled);
}

#[tokio::test]
async fn the_output_view_reads_the_end_first_then_on_from_it() {
    let thread = fixture_thread();
    // The end comes first: 4 KiB were printed before it.
    let first = page("…tail\n", 4096, false, ShellCommandState::Running);
    let resume = first.cursor;
    let tail = serde_json::to_string(&first).unwrap();
    let next =
        serde_json::to_string(&page("next\n", resume, true, ShellCommandState::Exited)).unwrap();
    let (client, seen) = recording_server(move |line, _| {
        if line.contains("tail=true") {
            ("200 OK", tail.clone())
        } else {
            ("200 OK", next.clone())
        }
    })
    .await;
    let mut app = app_on(&thread, vec![]);
    app.open_command_output("c1", Some(&client));
    for _ in 0..200 {
        app.poll_command_output(&client);
        if app.command_view.as_ref().is_some_and(|view| view.settled) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if let Some(view) = app.command_view.as_mut() {
            view.wanted |= view.loaded;
        }
    }
    let lines = requested(&seen);
    assert!(lines[0].contains(&format!("/v1/threads/{}/commands/c1/output?", thread.id)));
    assert!(lines[0].contains("tail=true"), "{lines:?}");
    assert!(lines[1].contains(&format!("cursor={resume}")), "{lines:?}");
    let rows = frame_text(&app, (80, 20));
    assert!(
        rows.iter()
            .any(|row| row.contains("earlier output isn't shown here"))
    );
    assert!(rows.iter().any(|row| row == "  …tail") && rows.iter().any(|row| row == "  next"));
}

#[tokio::test]
async fn a_command_the_server_forgot_shows_its_kept_end() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    let mut items = bash(&thread, turn, "call-1", "make", Some(("note", "c5", true)));
    items.push(message_item_for_turn(
        3,
        &thread,
        turn,
        ItemData::CommandState {
            call_id: "call-1".into(),
            command_id: "c5".into(),
            state: ShellCommandState::Exited,
            exit_code: Some(0),
            signal: None,
            tail: "built 12 targets\n".into(),
            started_at: Utc::now(),
            ended_at: Some(Utc::now()),
            moved_by: None,
        },
    ));
    let (client, _) = recording_server(|_, _| {
        (
            "404 Not Found",
            serde_json::json!({"code": "not_found", "message": "not found", "retryable": false})
                .to_string(),
        )
    })
    .await;
    let mut app = app_on(&thread, items);
    app.open_command_output("c5", Some(&client));
    for _ in 0..200 {
        app.poll_command_output(&client);
        if app.command_view.as_ref().is_some_and(|view| view.settled) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let rows = frame_text(&app, (80, 20));
    assert!(rows[0].starts_with(" bash  make"), "{}", rows[0]);
    assert!(
        rows.iter()
            .any(|row| row.contains("only the end of its output is kept"))
    );
    assert!(
        rows.iter().any(|row| row == "  built 12 targets"),
        "{rows:#?}"
    );
}

#[tokio::test]
async fn tasks_lists_child_agents_then_commands_and_enter_opens_a_commands_output() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    let mut app = app_on(&thread, vec![]);
    let conversation = app.conversation_id();
    let mut server = info_for(&thread, turn, "c1", "call-1", true);
    server.started_at = Utc::now() - chrono::Duration::seconds(65);
    app.shell_commands.update(server, conversation);
    let mut quick = info_for(&thread, turn, "c2", "call-2", false);
    quick.state = ShellCommandState::Exited;
    quick.exit_code = Some(0);
    quick.ended_at = Some(Utc::now());
    app.shell_commands.update(quick, conversation);
    let client = offline_client();
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    let crate::state::Overlay::Options { title, entries, .. } = &app.overlay else {
        panic!("tasks did not open");
    };
    assert_eq!(title, "tasks");
    // An ended foreground command is in the transcript already.
    assert_eq!(
        entries.len(),
        1,
        "{:?}",
        entries.iter().map(|e| &e.label).collect::<Vec<_>>()
    );
    assert_eq!(entries[0].label, "npm run dev");
    assert_eq!(
        entries[0].description,
        "in background · 1m 5s · enter opens its output"
    );
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(app.overlay, crate::state::Overlay::CommandOutput));
    assert_eq!(app.command_view.as_ref().unwrap().id, "c1");

    // Nothing at all: say so.
    let mut empty = app_on(&thread, vec![]);
    empty.open_tasks_overlay();
    assert!(matches!(empty.overlay, crate::state::Overlay::None));
    assert_eq!(empty.notice, "No child agents or commands");
}

#[tokio::test]
async fn the_sidebar_lists_commands_in_the_background_only_while_there_are_some() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    let mut app = app_on(&thread, vec![]);
    let section = |app: &App| {
        crate::view::sidebar::sidebar_lines(app, 40, 60)
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
    };
    assert!(!section(&app).iter().any(|row| row.contains("background")));
    let conversation = app.conversation_id();
    let mut info = info_for(&thread, turn, "c1", "call-1", true);
    info.started_at = Utc::now() - chrono::Duration::seconds(130);
    app.shell_commands.update(info.clone(), conversation);
    // A foreground command is not background work.
    app.shell_commands
        .update(info_for(&thread, turn, "c2", "call-2", false), conversation);
    let rows = section(&app);
    let header = rows
        .iter()
        .position(|row| row == "▾ background")
        .unwrap_or_else(|| panic!("{rows:#?}"));
    assert!(
        rows[header + 1].ends_with("npm run dev"),
        "{}",
        rows[header + 1]
    );
    assert_eq!(rows[header + 2], "    2m 10s");
    assert_eq!(rows[header + 3], "");
    let tagged = crate::view::sidebar::sidebar_lines(&app, 40, 60);
    assert!(tagged.tags.iter().any(|(row, target)| *row == header + 1
        && *target == crate::view::frame::HitTarget::CommandOpen("c1".into())));

    info.state = ShellCommandState::Exited;
    info.exit_code = Some(0);
    info.ended_at = Some(Utc::now());
    app.shell_commands.update(info, conversation);
    let rows = section(&app);
    assert!(rows.iter().any(|row| row == "    completed"), "{rows:#?}");

    app.language = crate::locale::Language::Arabic;
    assert!(section(&app).iter().any(|row| row == "▾ الخلفية"));
}

#[tokio::test]
async fn the_idle_band_counts_commands_in_the_background_and_esc_esc_stops_them() {
    let thread = fixture_thread();
    let mut app = app_on(&thread, vec![]);
    let band = |app: &App| {
        crate::view::composer::composer_lines(app, 100)
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let conversation = app.conversation_id();
    app.notice = "Ready".into();
    app.shell_commands.update(
        info_for(&thread, TurnId::new(), "c1", "call-1", true),
        conversation,
    );
    assert!(
        band(&app).contains("Ready   1 command in background"),
        "{}",
        band(&app)
    );
    app.notice.clear();
    app.shell_commands.update(
        info_for(&thread, TurnId::new(), "c2", "call-2", true),
        conversation,
    );
    assert!(
        band(&app).contains("2 commands in background"),
        "{}",
        band(&app)
    );
    app.language = crate::locale::Language::Arabic;
    assert!(band(&app).contains("أوامر في الخلفية: 2"), "{}", band(&app));
    app.language = crate::locale::Language::English;

    // With nothing but them running, Esc Esc stops the chat's work, them
    // included (a stop that answers steering would leave them), and never
    // quits.
    let (client, seen) = recording_server(|_, _| ("200 OK", "{}".into())).await;
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    // It says which work the second press stops.
    assert_eq!(
        app.notice,
        "Press Esc again to stop 2 commands in background"
    );
    let action = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(action, crate::input::AppAction::Continue));
    assert_eq!(app.notice, "Stopping session work…");
    let stop = format!("POST /v1/threads/{}/interrupt ", thread.id);
    for _ in 0..200 {
        if requested(&seen).iter().any(|line| line.starts_with(&stop)) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        requested(&seen).iter().any(|line| line.starts_with(&stop)),
        "{:?}",
        requested(&seen)
    );
}

#[tokio::test]
async fn the_session_list_says_running_in_background_for_a_chat_whose_only_work_is_there() {
    let current = fixture_thread();
    let other = Thread {
        title: "Dev server".into(),
        ..fixture_thread()
    };
    let mut app = App::new(
        current.clone(),
        vec![current.clone(), other.clone()],
        vec![],
        vec![],
        vec![],
    );
    app.server_info = Some(side_server_info(vec![
        haarness_protocol::BACKGROUND_COMMANDS_CAPABILITY.into(),
    ]));
    let client = offline_client();
    assert!(
        !app.session_detail(&other, None)
            .contains("running in background")
    );
    let info = info_for(&other, TurnId::new(), "c1", "call-1", true);
    app.reduce(updated(&other, &info), &client).await.unwrap();
    assert!(
        app.session_detail(&other, None)
            .starts_with("running in background"),
        "{}",
        app.session_detail(&other, None)
    );
    // Its own turn is work, which says more.
    app.live_threads.entry(other.id).or_default();
    assert!(app.session_detail(&other, None).starts_with("working"));
    app.live_threads.remove(&other.id);
    // Another chat's command is dropped once it ends.
    let mut ended = info.clone();
    ended.state = ShellCommandState::Exited;
    app.reduce(updated(&other, &ended), &client).await.unwrap();
    assert!(app.shell_commands.get("c1").is_none());
    assert!(
        !app.session_detail(&other, None)
            .contains("running in background")
    );
}

#[tokio::test]
async fn output_events_ask_the_open_view_to_read_on() {
    let thread = fixture_thread();
    let mut app = app_on(&thread, vec![]);
    let conversation = app.conversation_id();
    app.shell_commands.update(
        info_for(&thread, TurnId::new(), "c1", "call-1", true),
        conversation,
    );
    app.open_command_output("c1", None);
    let view = app.command_view.as_mut().unwrap();
    view.apply(page("one\n", 0, false, ShellCommandState::Running));
    view.wanted = false;
    let client = offline_client();
    app.reduce(
        fixture_event(
            "command.output",
            thread.id,
            thread.id,
            serde_json::json!({ "id": "c1", "size": 40 }),
        ),
        &client,
    )
    .await
    .unwrap();
    assert!(app.command_view.as_ref().unwrap().wanted);
    assert_eq!(app.shell_commands.get("c1").unwrap().output_bytes, 40);
}

#[test]
fn hydrating_replaces_the_conversations_list_and_keeps_other_chats_running_ones() {
    let thread = fixture_thread();
    let other = fixture_thread();
    let mut commands = crate::background::ShellCommands::default();
    let turn = TurnId::new();
    commands.update(info_for(&thread, turn, "stale", "call-0", true), thread.id);
    commands.update(
        info_for(&other, turn, "elsewhere", "call-9", true),
        thread.id,
    );
    commands.update(info_for(&other, turn, "gone", "call-8", true), thread.id);
    commands.hydrate(
        thread.id,
        Some(vec![info_for(&thread, turn, "c1", "call-1", true)]),
        Some(vec![info_for(&other, turn, "elsewhere", "call-9", true)]),
    );
    assert!(
        commands.get("stale").is_none(),
        "the server's list is the whole list"
    );
    assert!(commands.get("c1").is_some());
    assert!(commands.get("elsewhere").is_some());
    assert!(
        commands.get("gone").is_none(),
        "it ended while nothing said so"
    );
    assert_eq!(commands.running_in_background(other.id), 1);
}

#[test]
fn bash_output_and_bash_stop_rows_name_the_command_and_leftovers_are_said() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    let mut items = bash(
        &thread,
        turn,
        "call-1",
        "npm run dev",
        Some(("Started.", "c1", true)),
    );
    let call = |sequence, call: &str, name: &str, arguments| {
        message_item_for_turn(
            sequence,
            &thread,
            turn,
            ItemData::ToolCall {
                call_id: call.into(),
                name: name.into(),
                arguments,
                provider_metadata: serde_json::Value::Null,
            },
        )
    };
    items.push(call(
        3,
        "call-2",
        "bash_output",
        serde_json::json!({"id": "c1", "until": "match", "pattern": "ready on \\d+"}),
    ));
    items.push(call(
        4,
        "call-3",
        "bash_stop",
        serde_json::json!({"id": "c1"}),
    ));
    items.push(call(
        5,
        "call-4",
        "bash",
        serde_json::json!({"command": "sleep 300 & echo started"}),
    ));
    items.push(message_item_for_turn(
        6,
        &thread,
        turn,
        ItemData::ToolResult {
            call_id: "call-4".into(),
            name: "bash".into(),
            output: "exit: 0\nstarted".into(),
            is_error: false,
            metadata: serde_json::json!({
                "exit": 0,
                "leftovers": true,
                "command": {"id": "c4", "background": false, "startedAt": Utc::now()},
            }),
        },
    ));
    // Known only from the transcript: the server forgot it.
    let app = app_on(&thread, items);
    let rows = frame_text(&app, (100, 30));
    assert!(
        rows.iter()
            .any(|row| row.contains("bash_output npm run dev · ready on \\d+")),
        "{}",
        rows.join("\n")
    );
    assert!(rows.iter().any(|row| row.contains("bash_stop npm run dev")));
    assert!(
        row_with(&rows, "bash sleep 300 & echo started")
            .contains("·  stopped what it left running")
    );
}

/// A batch call whose second step runs `command`, with the batch's answer
/// naming that step's command `id` when `answered`.
fn batch(thread: &Thread, turn: TurnId, command: &str, id: &str, answered: bool) -> Vec<Item> {
    let mut items = vec![message_item_for_turn(
        1,
        thread,
        turn,
        ItemData::ToolCall {
            call_id: "call-b".into(),
            name: "batch".into(),
            arguments: serde_json::json!({"tool_calls": [
                {"tool": "read", "parameters": {"path": "README.md"}},
                {"tool": "bash", "parameters": {"command": command, "run_in_background": true}},
            ]}),
            provider_metadata: serde_json::Value::Null,
        },
    )];
    if answered {
        items.push(message_item_for_turn(
            2,
            thread,
            turn,
            ItemData::ToolResult {
                call_id: "call-b".into(),
                name: "batch".into(),
                output: "{}".into(),
                is_error: false,
                metadata: serde_json::json!({"results": [
                    {"tool": "read", "metadata": {}},
                    {"tool": "bash", "metadata": {
                        "command": {"id": id, "background": true, "startedAt": Utc::now()}
                    }},
                ]}),
            },
        ));
    }
    items
}

#[tokio::test]
async fn a_shell_step_of_a_batch_reads_in_background_under_the_batch_row() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    let mut app = app_on(&thread, batch(&thread, turn, "npm run dev", "c1", true));
    let conversation = app.conversation_id();
    // The step runs as call `<batch call>:2`.
    let mut info = info_for(&thread, turn, "c1", "call-b:2", true);
    info.started_at = Utc::now() - chrono::Duration::seconds(65);
    app.shell_commands.update(info.clone(), conversation);
    let rows = frame_text(&app, (100, 30));
    let step = row_with(&rows, "bash npm run dev");
    assert!(step.contains("in background · 1m 5s"), "{step}");
    let _ = app.lines((100, 30));
    assert!(
        app.hit_rows
            .borrow()
            .iter()
            .any(|hit| hit.target == crate::view::frame::HitTarget::CommandOpen("c1".into()))
    );
    // /tasks and the sidebar list it too.
    assert_eq!(app.command_task_entries()[0].label, "npm run dev");
    assert_eq!(app.background_commands().len(), 1);

    // Once the server forgot it, the chat's checkpoint says how it ended.
    let mut items = batch(&thread, turn, "npm run dev", "c1", true);
    items.push(message_item_for_turn(
        3,
        &thread,
        turn,
        ItemData::CommandState {
            call_id: "call-b:2".into(),
            command_id: "c1".into(),
            state: ShellCommandState::Exited,
            exit_code: Some(0),
            signal: None,
            tail: String::new(),
            started_at: Utc::now(),
            ended_at: Some(Utc::now()),
            moved_by: None,
        },
    ));
    let app = app_on(&thread, items);
    let rows = frame_text(&app, (100, 30));
    assert!(row_with(&rows, "bash npm run dev").contains("in background · completed"));
}

#[test]
fn a_checkpoint_the_chats_list_left_out_is_not_live() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    let mut items = bash(
        &thread,
        turn,
        "call-1",
        "npm run dev",
        Some(("ok", "c1", true)),
    );
    items.push(message_item_for_turn(
        3,
        &thread,
        turn,
        ItemData::CommandState {
            call_id: "call-1".into(),
            command_id: "c1".into(),
            state: ShellCommandState::Running,
            exit_code: None,
            signal: None,
            tail: "copied\n".into(),
            started_at: Utc::now() - chrono::Duration::seconds(30),
            ended_at: None,
            moved_by: None,
        },
    ));
    let mut app = app_on(&thread, items);
    // Before the list came, the checkpoint is all there is.
    let rows = frame_text(&app, (100, 30));
    assert!(row_with(&rows, "bash npm run dev").contains("in background · 30s"));
    // A forked chat copies the checkpoint, but its list has no such command.
    let conversation = app.conversation_id();
    app.shell_commands
        .hydrate(conversation, Some(Vec::new()), Some(Vec::new()));
    app.tick = 3;
    let rows = frame_text(&app, (100, 30));
    let row = row_with(&rows, "bash npm run dev");
    assert!(row.trim_end().ends_with("in background"), "no timer: {row}");
    assert!(row.contains('○'), "no spinner: {row}");
    app.open_command_output("c1", None);
    let rows = frame_text(&app, (80, 16));
    assert!(
        !rows.last().unwrap().contains("s stop"),
        "no stop: {rows:#?}"
    );
    assert!(!rows[0].contains("running"), "{}", rows[0]);
}

#[tokio::test]
async fn only_a_404_or_410_says_a_command_is_gone_and_the_view_keeps_what_it_read() {
    let thread = fixture_thread();
    let answers = Arc::new(Mutex::new(vec![
        ("409 Conflict", "this workspace is asleep"),
        ("400 Bad Request", "relay refused"),
        ("410 Gone", "no such command"),
    ]));
    let queue = answers.clone();
    let (client, seen) = recording_server(move |_, _| {
        let (status, message) = queue.lock().unwrap().remove(0);
        (
            status,
            serde_json::json!({"code": "x", "message": message, "retryable": true}).to_string(),
        )
    })
    .await;
    let mut app = app_on(&thread, vec![]);
    let conversation = app.conversation_id();
    app.shell_commands.update(
        info_for(&thread, TurnId::new(), "c1", "call-1", true),
        conversation,
    );
    app.open_command_output("c1", None);
    app.command_view.as_mut().unwrap().apply(page(
        "kept line\n",
        0,
        false,
        ShellCommandState::Running,
    ));
    // A relay's 409, then a 400: the view keeps its output and reads again.
    for _ in 0..2 {
        let before = requested(&seen).len();
        app.command_view.as_mut().unwrap().wanted = true;
        app.poll_command_output(&client);
        for _ in 0..200 {
            app.poll_command_output(&client);
            if requested(&seen).len() > before
                && app.command_view.as_ref().is_some_and(|view| !view.wanted)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        app.poll_command_output(&client);
        let view = app.command_view.as_ref().unwrap();
        assert!(!view.settled && !view.only_kept);
        assert!(
            frame_text(&app, (80, 16))
                .iter()
                .any(|row| row == "  kept line")
        );
        assert!(app.shell_commands.get("c1").is_some());
    }
    // A 410: no more comes, what was read stays, and the row is not live.
    app.command_view.as_mut().unwrap().wanted = true;
    for _ in 0..200 {
        app.poll_command_output(&client);
        if app.command_view.as_ref().is_some_and(|view| view.settled) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let rows = frame_text(&app, (80, 16));
    assert!(rows.iter().any(|row| row == "  kept line"), "{rows:#?}");
    assert!(!rows.last().unwrap().contains("s stop"));
    assert!(app.shell_commands.get("c1").is_none());
}

#[tokio::test]
async fn a_stop_the_server_cannot_find_settles_the_view_and_a_relays_refusal_does_not() {
    let thread = fixture_thread();
    let answers = Arc::new(Mutex::new(vec!["409 Conflict", "404 Not Found"]));
    let queue = answers.clone();
    let (client, _) = recording_server(move |_, _| {
        let status = queue.lock().unwrap().remove(0);
        (
            status,
            serde_json::json!({"code": "x", "message": "no", "retryable": false}).to_string(),
        )
    })
    .await;
    let mut app = app_on(&thread, vec![]);
    let conversation = app.conversation_id();
    app.shell_commands.update(
        info_for(&thread, TurnId::new(), "c1", "call-1", true),
        conversation,
    );
    app.open_command_output("c1", None);
    app.command_view.as_mut().unwrap().apply(page(
        "serving\n",
        0,
        false,
        ShellCommandState::Running,
    ));
    let key = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(app.command_output_key(key, &client).await.is_err());
    assert!(!app.command_view.as_ref().unwrap().settled);
    assert!(app.shell_commands.get("c1").is_some());
    app.command_output_key(key, &client).await.unwrap();
    assert!(app.command_view.as_ref().unwrap().settled);
    assert!(app.shell_commands.get("c1").is_none());
    let rows = frame_text(&app, (80, 16));
    assert!(rows.iter().any(|row| row == "  serving"));
    assert!(!rows.last().unwrap().contains("s stop"));
}

#[test]
fn bash_output_and_bash_stop_never_show_a_raw_id() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    // The id is named only in a batch's answer for its shell step.
    let mut items = batch(&thread, turn, "cargo watch -x test", "cmd-7f3a", true);
    let call = |sequence, call: &str, name: &str, arguments| {
        message_item_for_turn(
            sequence,
            &thread,
            turn,
            ItemData::ToolCall {
                call_id: call.into(),
                name: name.into(),
                arguments,
                provider_metadata: serde_json::Value::Null,
            },
        )
    };
    items.push(call(
        3,
        "call-2",
        "bash_output",
        serde_json::json!({"id": "cmd-7f3a"}),
    ));
    items.push(call(
        4,
        "call-3",
        "bash_stop",
        serde_json::json!({"id": "cmd-unknown"}),
    ));
    let app = app_on(&thread, items);
    let rows = frame_text(&app, (100, 30));
    let text = rows.join("\n");
    assert!(text.contains("bash_output cargo watch -x test"), "{text}");
    assert!(
        !text.contains("cmd-7f3a") && !text.contains("cmd-unknown"),
        "{text}"
    );
    assert!(rows.iter().any(|row| row.trim_end().ends_with("bash_stop")));
}

#[test]
fn the_view_says_it_waits_only_once_its_first_read_is_late_and_a_narrow_band_keeps_the_count() {
    let thread = fixture_thread();
    let mut app = app_on(&thread, vec![]);
    let conversation = app.conversation_id();
    app.shell_commands.update(
        info_for(&thread, TurnId::new(), "c1", "call-1", true),
        conversation,
    );
    app.open_command_output("c1", None);
    let waiting = |app: &App| {
        frame_text(app, (80, 16))
            .iter()
            .any(|row| row.contains("Waiting for output…"))
    };
    assert!(!waiting(&app), "nothing flashes before the first read");
    app.command_view.as_mut().unwrap().opened = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap();
    assert!(waiting(&app));
    app.close_overlay();

    // At 40 columns an older notice gives its place to the count; a fresh
    // one, the answer to what was just done, keeps it.
    let band = |app: &App| {
        crate::view::composer::composer_lines(app, 40)
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n")
    };
    app.notice = "Ready".into();
    app.notice_shown = Some((
        "Ready".into(),
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(10))
            .unwrap(),
    ));
    assert!(
        band(&app).contains("1 command in background"),
        "{}",
        band(&app)
    );
    app.notice = "Copied selection".into();
    app.notice_shown = Some(("Copied selection".into(), std::time::Instant::now()));
    let narrow = band(&app);
    assert!(
        narrow.contains("Copied select") && !narrow.contains("1 command"),
        "{narrow}"
    );
}

#[tokio::test]
async fn progress_without_newlines_keeps_the_view_moving_past_what_it_keeps() {
    let thread = fixture_thread();
    let mut app = app_on(&thread, vec![]);
    let conversation = app.conversation_id();
    app.shell_commands.update(
        info_for(&thread, TurnId::new(), "c1", "call-1", true),
        conversation,
    );
    app.open_command_output("c1", None);
    // `\r` progress only, in pages of about 540 KiB: well past the 2 MiB the
    // view keeps, every page must still show its newest line.
    let mut cursor = 0;
    for page_index in 0..8_u64 {
        let text = (0..30_000_u64).fold(String::new(), |mut text, step| {
            use std::fmt::Write as _;
            let _ = write!(text, "progress {:08}\r", page_index * 30_000 + step);
            text
        });
        let size = cursor + text.len() as u64;
        app.command_view
            .as_mut()
            .unwrap()
            .apply(ShellCommandOutput {
                start: cursor,
                cursor: size,
                size,
                text,
                more: false,
                settled: false,
                state: ShellCommandState::Running,
                exit_code: None,
            });
        cursor = size;
        let newest = format!("  progress {:08}", page_index * 30_000 + 29_999);
        assert!(
            frame_text(&app, (80, 16))
                .iter()
                .any(|row| row.trim_end() == newest),
            "page {page_index}: {:#?}",
            frame_text(&app, (80, 16))
        );
    }

    // Fallen further behind than it keeps, the view reads from the end.
    let (client, seen) = recording_server(|_, _| {
        (
            "200 OK",
            serde_json::to_string(&page(
                "progress 99999999\r",
                9_000_000,
                false,
                ShellCommandState::Running,
            ))
            .unwrap(),
        )
    })
    .await;
    let view = app.command_view.as_mut().unwrap();
    view.apply(ShellCommandOutput {
        start: cursor,
        cursor: cursor + 4,
        size: cursor + 3 * 1024 * 1024,
        text: "tail".into(),
        more: true,
        settled: false,
        state: ShellCommandState::Running,
        exit_code: None,
    });
    for _ in 0..200 {
        app.poll_command_output(&client);
        if frame_text(&app, (80, 16))
            .iter()
            .any(|row| row.trim_end() == "  progress 99999999")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let lines = requested(&seen);
    assert!(
        lines[0].contains("tail=true") && !lines[0].contains("cursor="),
        "{lines:?}"
    );
    let rows = frame_text(&app, (80, 16));
    assert!(
        rows.iter()
            .any(|row| row.contains("earlier output isn't shown here")),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.trim_end() == "  progress 99999999"),
        "{rows:#?}"
    );
}

#[tokio::test]
async fn ctrl_c_twice_still_quits_while_commands_run_in_the_background() {
    let thread = fixture_thread();
    let (client, seen) = recording_server(|_, _| ("200 OK", "{}".into())).await;
    let mut app = app_on(&thread, vec![]);
    let conversation = app.conversation_id();
    app.shell_commands.update(
        info_for(&thread, TurnId::new(), "c1", "call-1", true),
        conversation,
    );
    let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    crate::input::handle_key(&mut app, key, &client)
        .await
        .unwrap();
    assert_eq!(app.notice, "Press Ctrl+C again to quit");
    let action = crate::input::handle_key(&mut app, key, &client)
        .await
        .unwrap();
    assert!(matches!(action, crate::input::AppAction::Quit));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(requested(&seen).is_empty(), "the server keeps the commands");

    // One command alone: the Esc prompt names it.
    let mut app = app_on(&thread, vec![]);
    app.shell_commands.update(
        info_for(&thread, TurnId::new(), "c1", "call-1", true),
        conversation,
    );
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    crate::input::handle_key(&mut app, esc, &client)
        .await
        .unwrap();
    assert_eq!(
        app.notice,
        "Press Esc again to stop 1 command in background"
    );
    app.language = crate::locale::Language::Arabic;
    app.escape_interrupt = None;
    crate::input::handle_key(&mut app, esc, &client)
        .await
        .unwrap();
    assert_eq!(app.notice, "اضغط Esc مرة أخرى لإيقاف أمر واحد في الخلفية");
}

#[tokio::test]
async fn a_session_stop_while_the_main_reply_runs_still_answers_steering() {
    let root = fixture_thread();
    let child = Thread {
        parent_id: Some(root.id),
        is_subagent: true,
        ..fixture_thread()
    };
    let (client, seen) = recording_server(|_, _| ("200 OK", "{}".into())).await;
    let mut app = App::new(
        root.clone(),
        vec![root.clone(), child.clone()],
        vec![],
        vec![],
        vec![],
    );
    // A child still works; the main reply runs, though not on this view.
    app.live_threads.entry(child.id).or_default();
    app.root_active_turn = Some(TurnId::new());
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    crate::input::handle_key(&mut app, esc, &client)
        .await
        .unwrap();
    crate::input::handle_key(&mut app, esc, &client)
        .await
        .unwrap();
    let stop = format!("POST /v1/threads/{}/interrupt?resume=true ", root.id);
    for _ in 0..200 {
        if requested(&seen).iter().any(|line| line.starts_with(&stop)) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        requested(&seen).iter().any(|line| line.starts_with(&stop)),
        "{:?}",
        requested(&seen)
    );
}

#[tokio::test]
async fn background_from_a_childs_view_promotes_that_childs_own_subagents_too() {
    let root = fixture_thread();
    let child = Thread {
        parent_id: Some(root.id),
        is_subagent: true,
        ..fixture_thread()
    };
    let (client, seen) = recording_server(|_, _| ("200 OK", "[]".into())).await;
    let mut app = app_on(&child, vec![]);
    app.threads = vec![root.clone(), child.clone()];
    app.parent_stack = vec![root.clone()];
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    let lines = requested(&seen);
    for expected in [
        format!("POST /v1/threads/{}/background ", child.id),
        format!("POST /v1/threads/{}/background ", root.id),
        format!("POST /v1/threads/{}/commands/background ", root.id),
    ] {
        assert!(
            lines.iter().any(|line| line.starts_with(&expected)),
            "{expected} missing: {lines:?}"
        );
    }
}

#[tokio::test]
async fn ctrl_x_at_the_chats_cap_says_the_servers_words() {
    let thread = fixture_thread();
    let (client, _) = recording_server(|line, _| {
        if line.contains("/commands/background") {
            (
                "409 Conflict",
                serde_json::json!({"code": "conflict", "message": "This chat runs 16 commands in the background, as many as it may", "retryable": false})
                    .to_string(),
            )
        } else {
            ("200 OK", "[]".into())
        }
    })
    .await;
    let mut app = app_on(&thread, vec![]);
    app.composer.set_text("draft".into());
    let action = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
        &client,
    )
    .await;
    assert!(action.is_ok(), "no error card");
    assert_eq!(
        app.notice,
        "This chat runs 16 commands in the background, as many as it may"
    );
    assert_eq!(app.composer.text(), "draft");
}

#[test]
fn a_person_stopping_a_foreground_command_reads_stopped_and_leftovers_running_say_so() {
    let thread = fixture_thread();
    let turn = TurnId::new();
    let call = |sequence, call: &str, command: &str| {
        message_item_for_turn(
            sequence,
            &thread,
            turn,
            ItemData::ToolCall {
                call_id: call.into(),
                name: "bash".into(),
                arguments: serde_json::json!({ "command": command }),
                provider_metadata: serde_json::Value::Null,
            },
        )
    };
    let result = |sequence, call: &str, output: &str, metadata| {
        message_item_for_turn(
            sequence,
            &thread,
            turn,
            ItemData::ToolResult {
                call_id: call.into(),
                name: "bash".into(),
                output: output.into(),
                is_error: false,
                metadata,
            },
        )
    };
    let items = vec![
        call(1, "call-1", "npm test"),
        result(
            2,
            "call-1",
            "exit: 130\nrunning…",
            serde_json::json!({"exit": 130, "stoppedBy": "person",
                "command": {"id": "c1", "background": false, "startedAt": Utc::now()}}),
        ),
        call(3, "call-2", "setsid server &"),
        result(
            4,
            "call-2",
            "exit: 0\n",
            serde_json::json!({"exit": 0, "leftovers": true, "leftoversRunning": true,
                "command": {"id": "c2", "background": false, "startedAt": Utc::now()}}),
        ),
    ];
    let app = app_on(&thread, items);
    let rows = frame_text(&app, (100, 30));
    let stopped = row_with(&rows, "bash npm test");
    assert!(
        stopped.contains('⊘') && stopped.contains("stopped"),
        "{stopped}"
    );
    assert!(!stopped.contains("exit 130"), "{stopped}");
    assert!(
        row_with(&rows, "bash setsid server &").contains("·  left processes running"),
        "{rows:#?}"
    );
}

#[test]
fn a_progress_line_read_part_way_shows_as_a_terminal_leaves_it() {
    let thread = fixture_thread();
    let mut app = app_on(&thread, vec![]);
    app.open_command_output("c1", None);
    // The page ends in the middle of a rewrite of the line.
    app.command_view.as_mut().unwrap().apply(page(
        "progress 00000041\rprogress 00000042\rprogress 0000",
        0,
        false,
        ShellCommandState::Running,
    ));
    let rows = frame_text(&app, (80, 16));
    assert!(
        rows.iter()
            .any(|row| row.trim_end() == "  progress 00000042"),
        "{rows:#?}"
    );
    app.command_view.as_mut().unwrap().apply(page(
        "0043\rdone\n",
        49,
        false,
        ShellCommandState::Running,
    ));
    let rows = frame_text(&app, (80, 16));
    assert!(
        rows.iter()
            .any(|row| row.trim_end() == "  doneress 00000043"),
        "{rows:#?}"
    );
}
