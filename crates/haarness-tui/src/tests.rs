use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use haarness_opentui::{Color, Line, attr};
use haarness_protocol::{Item, ItemData, ItemId, ProviderInfo, Thread, ThreadId, TurnId};

use crate::commands::COMMANDS;
use crate::state::{App, default_connected_model};
use crate::theme::Theme;
use crate::width::DisplayWidth as _;

type HitPredicate = fn(&crate::view::frame::HitTarget) -> bool;

fn line_text(line: &Line) -> String {
    line.text()
}

fn cell_style(line: &Line, column: usize) -> Option<(Color, Option<Color>, u32)> {
    let mut start = 0;
    for span in &line.spans {
        let end = start + span.width();
        if column >= start && column < end {
            return Some((span.fg, span.bg, span.attributes));
        }
        start = end;
    }
    None
}

fn assert_menu_focus(app: &App, family: &str) {
    let lines = app.lines((100, 40));
    let hits = app.hit_rows.borrow();
    let focused = hits
        .iter()
        .find(|hit| {
            matches!(hit.target, crate::view::frame::HitTarget::MenuRow(index) if index == app.selection)
        })
        .cloned()
        .unwrap_or_else(|| panic!("{family}: selected menu row is not visible"));
    let ordinary = hits
        .iter()
        .find(|hit| {
            matches!(hit.target, crate::view::frame::HitTarget::MenuRow(index) if index != app.selection)
        })
        .cloned()
        .unwrap_or_else(|| panic!("{family}: fixture needs at least two menu rows"));
    drop(hits);

    assert!(
        lines[focused.row].width() >= focused.end,
        "{family}: focused row must paint its complete hit width"
    );
    for column in focused.start..focused.end {
        let (foreground, background, attributes) = cell_style(&lines[focused.row], column)
            .unwrap_or_else(|| panic!("{family}: no rendered cell at column {column}"));
        assert_eq!(
            background,
            Some(app.theme.primary),
            "{family}: focused cell {column} has no primary background"
        );
        assert_eq!(
            foreground, app.theme.bg,
            "{family}: focused cell {column} has no contrast foreground"
        );
        assert_ne!(
            attributes & attr::BOLD,
            0,
            "{family}: focused cell {column} is not bold"
        );
    }
    for column in ordinary.start..ordinary.end.min(lines[ordinary.row].width()) {
        let (_, background, _) = cell_style(&lines[ordinary.row], column)
            .unwrap_or_else(|| panic!("{family}: no ordinary cell at column {column}"));
        assert_ne!(
            background,
            Some(app.theme.primary),
            "{family}: an unfocused menu cell also looks focused"
        );
    }
}

fn assert_hit_focus(lines: &[Line], hit: &crate::view::frame::HitRegion, app: &App, family: &str) {
    let visible_end = hit.end.min(lines[hit.row].width());
    assert!(
        visible_end > hit.start,
        "{family}: hit region has no visible cells"
    );
    for column in hit.start..visible_end {
        let (foreground, background, _) = cell_style(&lines[hit.row], column)
            .unwrap_or_else(|| panic!("{family}: no rendered cell at column {column}"));
        assert_eq!(
            background,
            Some(app.theme.primary),
            "{family}: hovered cell {column} has no primary background"
        );
        assert_eq!(
            foreground, app.theme.bg,
            "{family}: hovered cell {column} has no contrast foreground"
        );
    }
}

fn fixture_thread() -> Thread {
    Thread {
        schedule: None,
        id: ThreadId::new(),
        title: "Test".into(),
        parent_id: None,
        is_subagent: false,
        fork_origin: None,
        archived: false,
        agent: "build".into(),
        provider: "openai".into(),
        model: "gpt-5.4".into(),
        ultracode: false,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        revert: None,
    }
}

fn fixture_workspace_catalog() -> haarness_protocol::WorkspaceListResponse {
    let now = Utc::now();
    let project = haarness_protocol::ProjectInfo {
        id: "project-1".into(),
        name: "haarness".into(),
        root: "/repo/haarness".into(),
        git: true,
        created_at: now,
    };
    let local = haarness_protocol::WorkspaceInfo {
        id: "workspace-local".into(),
        project_id: project.id.clone(),
        name: "haarness".into(),
        path: "/repo/haarness".into(),
        kind: haarness_protocol::WorkspaceKind::Local,
        managed: false,
        branch: Some("main".into()),
        from_ref: None,
        base_commit: None,
        created_at: now,
        last_opened_at: now,
        exists: true,
    };
    let managed = haarness_protocol::WorkspaceInfo {
        id: "workspace-review".into(),
        project_id: project.id.clone(),
        name: "review".into(),
        path: "/repo/worktrees/review".into(),
        kind: haarness_protocol::WorkspaceKind::GitWorktree,
        managed: true,
        branch: Some("haarness/review".into()),
        from_ref: Some("origin/main".into()),
        base_commit: Some("0123456789abcdef".into()),
        created_at: now,
        last_opened_at: now,
        exists: true,
    };
    let missing = haarness_protocol::WorkspaceInfo {
        id: "workspace-missing".into(),
        project_id: project.id.clone(),
        name: "stale".into(),
        path: "/repo/worktrees/stale".into(),
        kind: haarness_protocol::WorkspaceKind::GitWorktree,
        managed: true,
        branch: Some("haarness/stale".into()),
        from_ref: Some("main".into()),
        base_commit: Some("fedcba9876543210".into()),
        created_at: now,
        last_opened_at: now,
        exists: false,
    };
    haarness_protocol::WorkspaceListResponse {
        projects: vec![project],
        workspaces: vec![local, managed, missing],
        current_workspace_id: Some("workspace-local".into()),
    }
}

fn fixture_workspace_refs() -> haarness_protocol::WorkspaceRefsResponse {
    use haarness_protocol::{WorkspaceRefInfo, WorkspaceRefKind};

    haarness_protocol::WorkspaceRefsResponse {
        default_ref: "refs/heads/main".into(),
        default_commit: "1111111111111111111111111111111111111111".into(),
        refs: vec![
            WorkspaceRefInfo {
                name: "main".into(),
                reference: "refs/heads/main".into(),
                kind: WorkspaceRefKind::LocalBranch,
                commit: "1111111111111111111111111111111111111111".into(),
                current: true,
            },
            WorkspaceRefInfo {
                name: "feature/local".into(),
                reference: "refs/heads/feature/local".into(),
                kind: WorkspaceRefKind::LocalBranch,
                commit: "2222222222222222222222222222222222222222".into(),
                current: false,
            },
            WorkspaceRefInfo {
                name: "origin/main".into(),
                reference: "refs/remotes/origin/main".into(),
                kind: WorkspaceRefKind::RemoteBranch,
                commit: "3333333333333333333333333333333333333333".into(),
                current: false,
            },
            WorkspaceRefInfo {
                name: "v1.0.0".into(),
                reference: "refs/tags/v1.0.0".into(),
                kind: WorkspaceRefKind::Tag,
                commit: "4444444444444444444444444444444444444444".into(),
                current: false,
            },
        ],
    }
}

fn fixture_goal(status: haarness_protocol::GoalStatus) -> haarness_protocol::GoalState {
    let now = Utc::now();
    haarness_protocol::GoalState {
        goal_id: "goal-fixture".into(),
        goal: "Ship a verified release".into(),
        status,
        tokens_used: 120,
        token_budget: Some(1_000),
        token_allowance: None,
        time_used_seconds: 90,
        turns_used: 2,
        created_at: now,
        updated_at: now,
        last_verdict: None,
        last_reason: None,
        paused_reason: None,
        awaiting_user_input: false,
        consecutive_parse_failures: 0,
        consecutive_blocker_turns: 0,
        last_blocker_reason: None,
        last_blocker_turn: None,
        subgoals: vec![],
        criteria: vec![],
        needs_criteria: false,
    }
}

fn selector_fixture() -> App {
    let thread = fixture_thread();
    let other = Thread {
        title: "Another thread".into(),
        ..fixture_thread()
    };
    let models = ["gpt-5.4", "gpt-5.4-mini"]
        .into_iter()
        .map(|id| haarness_protocol::ModelInfo {
            id: id.into(),
            display_name: id.into(),
            reasoning: true,
            reasoning_efforts: ["low", "medium", "high"].map(str::to_owned).to_vec(),
            images: true,
            video: false,
            tools: true,
            context_window: Some(128_000),
            max_output_tokens: Some(16_000),
            pricing: None,
        })
        .collect();
    let provider = ProviderInfo {
        id: "openai".into(),
        name: "OpenAI".into(),
        authenticated: true,
        models,
    };
    let agents = ["build", "plan"]
        .into_iter()
        .map(|id| haarness_protocol::AgentProfile {
            id: id.into(),
            name: id.into(),
            description: format!("{id} agent"),
            hidden: false,
            max_subagents: 4,
            mode: Some("primary".into()),
            model: None,
        })
        .collect();
    App::new(
        thread.clone(),
        vec![thread, other],
        vec![],
        vec![provider],
        agents,
    )
}

#[tokio::test]
async fn btw_slash_command_executes_real_http_path_and_renders_result() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let thread = fixture_thread();
    let btw_body = serde_json::json!({
        "answer": "The worker verified the release.",
        "inputTokens": 11,
        "outputTokens": 6,
    })
    .to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for body in [btw_body] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
                let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            requests.push(String::from_utf8(request).unwrap());
        }
        requests
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);

    app.execute_command("/btw what did the worker verify?", &client)
        .await
        .unwrap();
    assert!(app.notice.contains("transcript unchanged"));
    let crate::state::Overlay::Panel { title, lines } = &app.overlay else {
        panic!("btw command did not render an isolated answer panel");
    };
    assert!(title.contains("isolated read-only"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], "The worker verified the release.");

    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with(&format!("POST /v1/threads/{}/btw ", thread.id)));
    let request: haarness_protocol::BtwRequest =
        serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(request.question, "what did the worker verify?");
}

#[tokio::test]
async fn bare_compact_requests_a_server_generated_summary_over_real_http() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let thread = fixture_thread();
    let compacted = Item {
        id: ItemId::new(),
        thread_id: thread.id,
        turn_id: None,
        sequence: 1,
        data: ItemData::Compaction {
            summary: "real model summary".into(),
            files_touched: Vec::new(),
            outstanding: Vec::new(),
            todo_checkpoint: None,
            tail_start_sequence: None,
        },
        created_at: Utc::now(),
    };
    let responses = [
        serde_json::to_string(&compacted).unwrap(),
        serde_json::to_string(&vec![compacted.clone()]).unwrap(),
    ];
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for body in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
                let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            requests.push(String::from_utf8(request).unwrap());
        }
        requests
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);

    app.execute_command("/compact", &client).await.unwrap();

    assert_eq!(app.notice, "Compaction checkpoint created");
    assert!(matches!(
        app.items.last().map(|item| &item.data),
        Some(ItemData::Compaction { summary, .. }) if summary == "real model summary"
    ));
    let requests = server.await.unwrap();
    assert!(requests[0].starts_with("POST /v1/threads/"));
    assert!(requests[0].contains("/compact "));
    assert!(requests[0].contains(r#""summary":null"#));
}

#[tokio::test]
async fn bare_fork_offers_only_assistant_message_boundaries() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.items = vec![
        message_item(
            1,
            &thread,
            ItemData::UserMessage {
                text: "first prompt".into(),
                attachments: Vec::new(),
                queued: false,
                synthetic: false,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::AssistantMessage {
                text: "first reply".into(),
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::UserMessage {
                text: "latest prompt".into(),
                attachments: vec!["/tmp/input.png".into()],
                queued: false,
                synthetic: false,
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::AssistantMessage {
                text: "latest reply".into(),
            },
        ),
    ];

    app.execute_command("/fork", &offline_client())
        .await
        .unwrap();

    let crate::state::Overlay::Options { kind, entries, .. } = &app.overlay else {
        panic!("fork picker did not open");
    };
    assert!(matches!(kind, crate::state::OptionsKind::ForkMessages));
    assert_eq!(entries[0].value, "current");
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[1].label, "latest reply");
    assert_eq!(entries[2].label, "first reply");
    assert_eq!(entries[2].value, app.items[1].id.to_string());
    // A stale or fabricated choice cannot restore the removed user-prompt path.
    let user_id = app.items[0].id.to_string();
    let error = app
        .execute_choice(
            crate::state::OverlayChoice::Option(crate::state::OptionsKind::ForkMessages, user_id),
            &offline_client(),
        )
        .await
        .err()
        .expect("user-message choices must be rejected before any HTTP request");
    assert!(
        error
            .to_string()
            .contains("fork message is no longer available")
    );
}

#[tokio::test]
async fn tool_catalog_and_lazy_reset_use_real_http_and_render_effective_state() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let thread = fixture_thread();
    let catalog = serde_json::json!({
        "tools": [{
            "id": "session_manage",
            "description": "Manage durable sessions",
            "group": "sessions",
            "groupTitle": "Sessions",
            "source": "native",
            "essential": false,
            "enabled": true,
            "advertised": false,
            "loaded": false,
            "estimatedTokens": 123
        }],
        "lazyMode": "auto",
        "lazyActive": true,
        "estimatedTotalTokens": 123,
        "estimatedAdvertisedTokens": 40,
        "estimatedSavedTokens": 83
    })
    .to_string();
    let mut disabled_catalog: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    disabled_catalog["tools"][0]["enabled"] = false.into();
    let disabled_catalog = disabled_catalog.to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for body in [
            catalog,
            disabled_catalog.clone(),
            disabled_catalog.clone(),
            disabled_catalog.clone(),
            disabled_catalog,
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
                let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            requests.push(String::from_utf8(request).unwrap());
        }
        requests
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);

    app.execute_command("/tools", &client).await.unwrap();
    let crate::state::Overlay::Options { title, entries, .. } = &app.overlay else {
        panic!("tool catalog did not open");
    };
    assert!(title.contains("lazy auto active"));
    assert_eq!(entries.len(), 1);
    assert!(entries[0].label.contains("session_manage"));
    assert!(entries[0].description.contains("deferred"));

    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::Tools,
            "disable:session_manage".into(),
        ),
        &client,
    )
    .await
    .unwrap();
    assert!(app.notice.contains("Disabled tool session_manage"));
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        panic!("updated tool catalog did not remain open");
    };
    assert!(entries[0].description.contains("disabled"));

    app.execute_command("/tools lazy reset", &client)
        .await
        .unwrap();
    assert!(app.notice.contains("configured/default mode: auto"));
    let requests = server.await.unwrap();
    assert!(requests[0].starts_with(&format!("GET /v1/tools?threadId={} ", thread.id)));
    assert!(requests[1].starts_with("POST /v1/tools/session_manage "));
    assert!(requests[1].contains(r#"{"enabled":false}"#));
    assert!(requests[2].starts_with(&format!("GET /v1/tools?threadId={} ", thread.id)));
    assert!(requests[3].starts_with("POST /v1/tools/lazy "));
    assert!(requests[3].contains(r#"{"mode":null}"#));
    assert!(requests[4].starts_with(&format!("GET /v1/tools?threadId={} ", thread.id)));
}

#[tokio::test]
async fn initial_goal_hydration_restores_the_existing_pursuit() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let thread = fixture_thread();
    let goal = fixture_goal(haarness_protocol::GoalStatus::Active);
    let body = serde_json::json!({"state": goal, "message": "Goal is active"}).to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);

    crate::hydrate_initial_goal(&mut app, &client).await;

    assert_eq!(
        app.goal.as_ref().map(|state| &state.goal_id),
        Some(&goal.goal_id)
    );
    assert_eq!(
        app.goal.as_ref().map(|state| state.status),
        Some(haarness_protocol::GoalStatus::Active)
    );
    assert!(!app.goal_refresh_needed);
    assert_eq!(app.goal_snapshots.get(&thread.id), Some(&app.goal));
    let request = server.await.unwrap();
    assert!(request.starts_with(&format!("GET /v1/threads/{}/goal ", thread.id)));
}

#[tokio::test]
async fn ctrl_u_and_ctrl_w_edit_a_dialog_filter_like_the_prompt() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Models);
    let client = offline_client();
    let press = |code, modifiers| KeyEvent::new(code, modifiers);
    for character in "gpt mini".chars() {
        crate::input::handle_key(
            &mut app,
            press(KeyCode::Char(character), KeyModifiers::NONE),
            &client,
        )
        .await
        .unwrap();
    }
    assert_eq!(app.overlay_filter, "gpt mini");
    crate::input::handle_key(
        &mut app,
        press(KeyCode::Char('w'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.overlay_filter, "gpt ");
    crate::input::handle_key(
        &mut app,
        press(KeyCode::Char('u'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.overlay_filter, "");
    assert!(matches!(app.overlay, crate::state::Overlay::Models));
}

#[test]
fn a_share_link_on_this_computer_says_nobody_else_can_open_it() {
    use crate::management::{SHARE_ONLY_HERE, share_links};
    let view = |lines: &[&str]| haarness_protocol::ManagementView {
        title: "Share".into(),
        lines: lines.iter().map(|line| (*line).to_owned()).collect(),
        ..haarness_protocol::ManagementView::default()
    };
    let mut first = view(&["Publishes a snapshot.", "Review it.", "/s/abc"]);
    share_links(&mut first, "http://127.0.0.1:57524", "view");
    assert_eq!(
        first.lines,
        [
            "Publishes a snapshot.",
            "Review it.",
            SHARE_ONLY_HERE,
            "http://127.0.0.1:57524/s/abc"
        ]
    );
    let mut created = view(&["/s/abc", "Expires tomorrow"]);
    share_links(&mut created, "http://localhost:4096", "create");
    assert_eq!(
        created.lines.last().map(String::as_str),
        Some(SHARE_ONLY_HERE)
    );
    assert_eq!(created.lines[0], "http://localhost:4096/s/abc");
    let mut reachable = view(&["Publishes a snapshot.", "/s/abc"]);
    share_links(&mut reachable, "https://haarness.example.com", "view");
    assert!(!reachable.lines.iter().any(|line| line == SHARE_ONLY_HERE));
    let mut preview = view(&["You: hello"]);
    share_links(&mut preview, "http://127.0.0.1:1", "preview");
    assert_eq!(preview.lines, ["You: hello"]);
}

#[test]
fn only_pages_inside_a_management_view_offer_a_way_back() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let entries = |app: &App| match &app.overlay {
        crate::state::Overlay::Options { entries, .. } => entries.clone(),
        _ => panic!("management view did not open"),
    };
    app.management = crate::management::Management {
        resource: "servers".into(),
        view: haarness_protocol::ManagementView {
            title: "Servers".into(),
            ..haarness_protocol::ManagementView::default()
        },
        root_title: Some("Servers".into()),
        ..crate::management::Management::default()
    };
    app.show_management();
    assert!(entries(&app).iter().all(|entry| entry.value != "back"));

    app.management.view.title = "Server · fixture".into();
    app.show_management();
    let back = entries(&app)
        .into_iter()
        .find(|entry| entry.value == "back")
        .unwrap();
    assert_eq!(
        (back.label.as_str(), back.description.as_str()),
        ("Back", "Return to Servers")
    );

    // Opened straight onto an inner page, the list it belongs to is unknown.
    app.management.root_title = None;
    app.show_management();
    let back = entries(&app)
        .into_iter()
        .find(|entry| entry.value == "back")
        .unwrap();
    assert_eq!(back.description, "Return to the list");
}

#[test]
fn insights_takes_open_like_the_cli_and_stats_says_what_it_takes() {
    use crate::commands::analytics_query;
    let (query, open) = analytics_query("--days 7 --open", true).unwrap();
    assert_eq!(query, [("days".to_owned(), "7".to_owned())]);
    assert!(open);
    assert!(!analytics_query("--open --no-open", true).unwrap().1);
    assert!(!analytics_query("", true).unwrap().1);
    let error = analytics_query("--open", false).unwrap_err().to_string();
    assert!(error.starts_with("Unknown option --open;"), "{error}");
    assert!(!error.contains("or --open"), "{error}");
    let error = analytics_query("--bogus", true).unwrap_err().to_string();
    assert!(error.ends_with("--limit N or --open"), "{error}");
}

#[test]
fn a_paused_goal_says_in_full_what_it_waits_for() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "working on it".into(),
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let mut goal = fixture_goal(haarness_protocol::GoalStatus::Paused);
    goal.paused_reason = Some("waiting for you to agree on its completion criteria".into());
    app.goal = Some(goal);
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");

    let sidebar = words(
        &crate::view::sidebar::sidebar_lines(&app, 40, 40)
            .lines
            .iter()
            .map(|line| line_text(line) + "\n")
            .collect::<String>(),
    );
    assert!(
        sidebar.contains("paused · waiting for you to agree on its completion criteria"),
        "{sidebar}"
    );

    app.open_goal_overlay();
    let frame = app.lines((100, 30));
    let dialog = words(&frame.iter().map(line_text).collect::<Vec<_>>().join("\n"));
    assert!(
        dialog.contains("Paused · waiting for you to agree on its completion criteria"),
        "{dialog}"
    );
    assert!(dialog.contains("Goal paused"), "{dialog}");
    assert!(!dialog.contains("criteria…"), "{dialog}");

    // A goal the person paused themselves needs no reason.
    if let Some(goal) = app.goal.as_mut() {
        goal.paused_reason = Some("user-paused".into());
    }
    assert_eq!(
        crate::commands::goal_status_label(app.goal.as_ref().unwrap()),
        "paused"
    );
}

#[tokio::test]
async fn goal_management_exposes_refresh_and_makes_clear_cancel_safe() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.goal = Some(fixture_goal(haarness_protocol::GoalStatus::Active));
    app.open_goal_overlay();

    let crate::state::Overlay::Options { kind, entries, .. } = &app.overlay else {
        panic!("goal management overlay did not open");
    };
    assert!(matches!(kind, crate::state::OptionsKind::GoalActions));
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.value.as_str())
            .collect::<Vec<_>>(),
        ["current", "edit", "status", "pause", "clear"]
    );

    app.execute_choice(
        crate::state::OverlayChoice::Option(crate::state::OptionsKind::GoalActions, "clear".into()),
        &offline_client(),
    )
    .await
    .unwrap();
    let crate::state::Overlay::Options { kind, entries, .. } = &app.overlay else {
        panic!("clear confirmation did not open");
    };
    assert!(matches!(
        kind,
        crate::state::OptionsKind::GoalClearConfirmation
    ));
    assert_eq!(app.selection, 0);
    assert_eq!(entries[0].value, "cancel");
    assert_eq!(entries[1].value, "confirm");
    assert!(matches!(
        app.selected_overlay_choice(),
        Some(crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::GoalClearConfirmation,
            ref value
        )) if value == "cancel"
    ));

    let viewport = app.viewport_offset;
    crate::input::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.selection, 1);
    assert_eq!(
        app.viewport_offset, viewport,
        "modal wheel must not scroll chat"
    );
    crate::input::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.selection, 0);

    for character in "clear".chars() {
        crate::input::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &offline_client(),
        )
        .await
        .unwrap();
    }
    assert!(app.overlay_filter.is_empty());
    assert_eq!(
        app.selection, 0,
        "typing must not displace the safe default"
    );

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::GoalActions,
            ..
        }
    ));
    app.execute_choice(
        crate::state::OverlayChoice::Option(crate::state::OptionsKind::GoalActions, "clear".into()),
        &offline_client(),
    )
    .await
    .unwrap();

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(app.goal.is_some(), "default Enter must not clear the goal");
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::GoalActions,
            ..
        }
    ));

    app.goal = Some(fixture_goal(haarness_protocol::GoalStatus::Paused));
    app.open_goal_overlay();
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        unreachable!();
    };
    assert!(entries.iter().any(|entry| entry.value == "resume"));
    assert!(!entries.iter().any(|entry| entry.value == "pause"));

    app.goal = Some(fixture_goal(haarness_protocol::GoalStatus::Blocked));
    app.open_goal_overlay();
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        unreachable!();
    };
    assert!(!entries.iter().any(|entry| entry.value == "resume"));
    assert!(!entries.iter().any(|entry| entry.value == "pause"));

    app.open_goal_overlay();
    let error = app
        .execute_choice(
            crate::state::OverlayChoice::Option(
                crate::state::OptionsKind::GoalActions,
                "status".into(),
            ),
            &offline_client(),
        )
        .await;
    assert!(error.is_err());
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::GoalActions,
            ..
        }
    ));
}

#[tokio::test]
async fn explicit_goal_completion_uses_fallback_copy_and_defaults_to_dismiss() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let thread = fixture_thread();
    let goal = fixture_goal(haarness_protocol::GoalStatus::Complete);
    let response_body = serde_json::to_string(&haarness_protocol::GoalActionResponse {
        state: Some(goal),
        goal: Some("Ship a verified release".into()),
        message: "Goal achieved".into(),
    })
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);

    app.execute_command("/goal complete", &client)
        .await
        .unwrap();

    let request = server.await.unwrap();
    assert!(request.starts_with(&format!("POST /v1/threads/{}/goal ", thread.id)));
    assert!(request.contains(r#"{"action":"complete"}"#));
    assert_eq!(app.selection, 3);
    let crate::state::Overlay::Options { kind, entries, .. } = &app.overlay else {
        panic!("completion overlay did not open");
    };
    assert!(matches!(kind, crate::state::OptionsKind::GoalCompletion));
    assert_eq!(entries[3].value, "dismiss");
    assert!(matches!(
        app.selected_overlay_choice(),
        Some(crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::GoalCompletion,
            ref value
        )) if value == "dismiss"
    ));
    for character in "clear".chars() {
        crate::input::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &offline_client(),
        )
        .await
        .unwrap();
    }
    assert!(app.overlay_filter.is_empty());
    assert_eq!(app.selection, 3, "typing must preserve Dismiss as default");
}

#[tokio::test]
async fn failed_goal_hydration_preserves_cached_state_and_schedules_retry() {
    let thread = fixture_thread();
    let goal = fixture_goal(haarness_protocol::GoalStatus::Active);
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.install_goal_snapshot(Some(goal.clone()));

    assert!(!crate::hydrate_goal_snapshot(&mut app, &offline_client()).await);
    assert_eq!(app.goal, Some(goal));
    assert!(app.goal_refresh_needed);
}

#[tokio::test]
async fn completed_goal_events_fallback_on_missing_or_partial_copy_and_deduplicate_replay() {
    for completion in [None, Some(serde_json::json!({"title": "Partial"}))] {
        let thread = fixture_thread();
        let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
        let mut payload = serde_json::json!({
            "state": fixture_goal(haarness_protocol::GoalStatus::Complete),
            "verdict": "done"
        });
        if let Some(completion) = completion {
            payload["completion"] = completion;
        }

        app.reduce(
            fixture_event("goal.updated", thread.id, thread.id, payload.clone()),
            &offline_client(),
        )
        .await
        .unwrap();
        let crate::state::Overlay::Options { kind, entries, .. } = &app.overlay else {
            panic!("fallback completion overlay did not open");
        };
        assert!(matches!(kind, crate::state::OptionsKind::GoalCompletion));
        assert_eq!(entries[3].label, "Dismiss");
        assert_eq!(app.selection, 3);

        crate::input::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &offline_client(),
        )
        .await
        .unwrap();
        assert!(matches!(app.overlay, crate::state::Overlay::None));
        assert_eq!(app.last_goal_completion.as_deref(), Some("goal-fixture"));

        app.reduce(
            fixture_event("goal.updated", thread.id, thread.id, payload),
            &offline_client(),
        )
        .await
        .unwrap();
        assert!(
            matches!(app.overlay, crate::state::Overlay::None),
            "replayed completion must not reopen a dismissed dialog"
        );
    }
}

#[tokio::test]
async fn final_goal_accounting_event_preserves_model_authored_completion_copy() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.active_turn = Some(haarness_protocol::TurnId::new());
    let completion = haarness_protocol::GoalCompletion {
        title: "Et ensuite ?".into(),
        edit: haarness_protocol::GoalCompletionOption {
            label: "Affiner".into(),
            description: "Modifier cet objectif".into(),
        },
        new: haarness_protocol::GoalCompletionOption {
            label: "Nouveau".into(),
            description: "Commencer un autre objectif".into(),
        },
        clear: haarness_protocol::GoalCompletionOption {
            label: "Effacer".into(),
            description: "Supprimer le suivi".into(),
        },
        dismiss: haarness_protocol::GoalCompletionOption {
            label: "Fermer".into(),
            description: "Ne rien changer".into(),
        },
    };
    let state = fixture_goal(haarness_protocol::GoalStatus::Complete);
    app.reduce(
        fixture_event(
            "goal.updated",
            thread.id,
            thread.id,
            serde_json::json!({
                "state": state,
                "verdict": "done",
                "completion": completion
            }),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    // Runtime accounting arrives after the completion event and intentionally
    // carries no model-authored dialog copy.
    app.reduce(
        fixture_event(
            "goal.updated",
            thread.id,
            thread.id,
            serde_json::json!({
                "state": fixture_goal(haarness_protocol::GoalStatus::Complete),
                "verdict": "done"
            }),
        ),
        &offline_client(),
    )
    .await
    .unwrap();

    app.active_turn = None;
    app.reveal_goal_completion();
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        panic!("localized completion did not open");
    };
    assert_eq!(entries[0].label, "Affiner");
    assert_eq!(entries[3].label, "Fermer");
}

#[tokio::test]
async fn localized_goal_replay_refreshes_an_open_snapshot_fallback() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.install_goal_snapshot(Some(fixture_goal(haarness_protocol::GoalStatus::Complete)));
    app.reveal_goal_completion();
    app.selection = 1;

    app.reduce(
        fixture_event(
            "goal.updated",
            thread.id,
            thread.id,
            serde_json::json!({
                "state": fixture_goal(haarness_protocol::GoalStatus::Complete),
                "verdict": "done",
                "completion": {
                    "title": "Que faire ensuite ?",
                    "edit": {"label": "Modifier", "description": "Affiner"},
                    "new": {"label": "Nouveau", "description": "Recommencer"},
                    "clear": {"label": "Effacer", "description": "Supprimer"},
                    "dismiss": {"label": "Fermer", "description": "Conserver"}
                }
            }),
        ),
        &offline_client(),
    )
    .await
    .unwrap();

    let crate::state::Overlay::Options { title, entries, .. } = &app.overlay else {
        panic!("completion dialog closed during localized replay");
    };
    assert_eq!(title, "Que faire ensuite ?");
    assert_eq!(entries[0].label, "Modifier");
    assert_eq!(entries[3].label, "Fermer");
    assert_eq!(app.selection, 1);
}

#[test]
fn goal_status_and_elapsed_labels_match_haarness() {
    assert!(
        crate::commands::goal_status_label(&fixture_goal(haarness_protocol::GoalStatus::Active))
            .starts_with("pursuing")
    );
    assert!(
        crate::commands::goal_status_label(&fixture_goal(haarness_protocol::GoalStatus::Complete))
            .starts_with("achieved")
    );
    for (seconds, expected) in [
        (59, "59s"),
        (60, "1m"),
        (61, "1m 1s"),
        (3_600, "1h"),
        (3_660, "1h 1m"),
        (90_060, "1d 1h 1m"),
    ] {
        assert_eq!(crate::view::sidebar::format_goal_time(seconds), expected);
    }
}

#[test]
fn sidebar_prioritizes_and_bounds_the_standing_goal() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let mut goal = fixture_goal(haarness_protocol::GoalStatus::Active);
    goal.goal = "Ship a carefully verified release across every supported provider and client with enough additional detail to require substantially more than three narrow sidebar rows".into();
    app.install_goal_snapshot(Some(goal));

    let rendered = crate::view::sidebar::sidebar_lines(&app, 28, 40)
        .lines
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.find("▾ goal").unwrap() < rendered.find("▾ usage").unwrap());
    assert!(
        rendered.contains('…'),
        "long goals need an explicit truncation cue: {rendered}"
    );
}

#[tokio::test]
async fn completed_goal_snapshots_rebuild_fallback_and_reveal_once_idle() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.active_turn = Some(haarness_protocol::TurnId::new());
    app.install_goal_snapshot(Some(fixture_goal(haarness_protocol::GoalStatus::Complete)));
    app.reveal_goal_completion();
    assert!(
        matches!(app.overlay, crate::state::Overlay::None),
        "a running turn keeps completion deferred"
    );

    app.active_turn = None;
    app.reveal_goal_completion();
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::GoalCompletion,
            ..
        }
    ));
    assert_eq!(app.selection, 3);
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();

    app.install_goal_snapshot(Some(fixture_goal(haarness_protocol::GoalStatus::Complete)));
    app.reveal_goal_completion();
    assert!(
        matches!(app.overlay, crate::state::Overlay::None),
        "the same completed goal must stay dismissed after snapshot replay"
    );
}

#[test]
fn layout_has_distinct_editorial_sections() {
    let thread = fixture_thread();
    let app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let lines = app.lines((80, 24));
    assert!(
        lines
            .iter()
            .any(|line| line_text(line).contains("h a a r n e s s"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line_text(line).contains("Ask anything"))
    );
    assert_eq!(lines.len(), 24);
}

#[test]
fn slash_commands_filter_and_render_as_an_interactive_menu() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.composer.set_text("/mo".into());
    let commands = app.slash_suggestions().unwrap();
    assert_eq!(commands.first().unwrap().name, "models");
    let lines = app.lines((80, 24));
    assert!(lines.iter().any(|line| line_text(line).contains("/models")));
    let selected = app.theme.primary;
    assert!(
        lines
            .iter()
            .any(|line| { line.spans.iter().any(|span| span.bg == Some(selected)) })
    );
}

#[test]
fn every_selector_family_uses_the_shared_full_width_focus_row() {
    let mut app = selector_fixture();
    let overlays = [
        ("commands overlay", crate::state::Overlay::Commands),
        ("threads overlay", crate::state::Overlay::Threads),
        ("models overlay", crate::state::Overlay::Models),
        ("agents overlay", crate::state::Overlay::Agents),
        ("themes overlay", crate::state::Overlay::Themes),
        (
            "options overlay",
            crate::state::Overlay::Options {
                title: "reasoning".into(),
                kind: crate::state::OptionsKind::ReasoningEffort,
                entries: vec![
                    crate::state::OptionEntry {
                        label: "high".into(),
                        description: "Deep thinking".into(),
                        value: "high".into(),
                    },
                    crate::state::OptionEntry {
                        label: "minimal".into(),
                        description: "Fastest".into(),
                        value: "minimal".into(),
                    },
                ],
            },
        ),
    ];
    for (family, overlay) in overlays {
        app.open_overlay(overlay);
        assert_menu_focus(&app, family);
    }

    app.close_overlay();
    app.composer.set_text("/".into());
    assert_menu_focus(&app, "slash autocomplete");

    app.composer.set_text("@src".into());
    app.mention_suggestions = vec!["src/lib.rs".into(), "src/main.rs".into()];
    app.selection = 0;
    assert_menu_focus(&app, "mention autocomplete");
}

#[test]
fn long_mention_paths_keep_their_file_name_visible() {
    let mut app = selector_fixture();
    let root = format!("/var/folders/{}/ws", "x".repeat(90));
    let long = format!("{root}/dropped image.png");
    app.composer.set_text(format!("@{root}/drop"));
    app.mention_suggestions = vec![long, "agent://explore".into()];
    app.selection = 0;
    let text = frame_text(&app, (80, 30));
    let row = text
        .lines()
        .find(|line| line.contains('›'))
        .unwrap_or_else(|| panic!("no highlighted mention row:\n{text}"));
    assert!(
        row.contains("dropped image.png"),
        "file name clipped away: {row}"
    );
    assert!(row.contains('…'), "{row}");
    assert!(text.contains("Delegate to agent"), "{text}");
}

#[test]
fn selectable_rows_remain_visibly_distinct_in_every_theme() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Commands);
    for (name, variant, theme) in app.theme_variants() {
        app.theme = theme;
        assert_menu_focus(&app, &format!("{name} {variant}"));
    }
}

#[tokio::test]
async fn keyboard_and_mouse_move_menu_focus_before_mouse_activation() {
    let mut app = selector_fixture();
    let client = offline_client();
    app.composer.set_text("/".into());

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.selection, 1);
    assert_menu_focus(&app, "keyboard focus");

    let model_index = app
        .slash_suggestions()
        .unwrap()
        .iter()
        .position(|entry| entry.name == "models")
        .expect("/models suggestion");
    let model_row = app
        .hit_rows
        .borrow()
        .iter()
        .find(|hit| {
            matches!(hit.target, crate::view::frame::HitTarget::MenuRow(index) if index == model_index)
        })
        .cloned()
        .expect("/models row");
    crate::input::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: u16::try_from(model_row.start + 2).unwrap(),
            row: u16::try_from(model_row.row).unwrap(),
            modifiers: KeyModifiers::NONE,
        },
        &client,
    )
    .await
    .unwrap();
    assert_eq!(
        app.selection, model_index,
        "mouse motion moves focus to /models"
    );
    assert_eq!(app.composer.text(), "/", "hover never activates a row");
    assert_menu_focus(&app, "mouse focus");

    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column: u16::try_from(model_row.start + 2).unwrap(),
                row: u16::try_from(model_row.row).unwrap(),
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
    }
    assert_eq!(app.composer.text(), "/models ");
}

#[tokio::test]
async fn enter_activates_the_selected_partial_slash_command() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.composer.set_text("/mod".into());

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();

    assert!(
        matches!(
            &app.overlay,
            crate::state::Overlay::Options {
                kind: crate::state::OptionsKind::ConnectProvider,
                ..
            }
        ),
        "Enter must activate /models and open the connect picker when no models are available"
    );
    assert!(app.composer.is_empty());
}

#[test]
fn exact_slash_command_is_highlighted_and_outranks_a_description_match() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.composer.set_text("/workflow".into());

    let suggestions = app.slash_suggestions().expect("slash suggestions");
    assert_eq!(
        suggestions[0].name, "workflow",
        "the highlighted row must agree with exact Enter dispatch"
    );
    assert_eq!(
        crate::input::selected_slash_command(&app).as_deref(),
        Some("/workflow")
    );

    app.composer.set_text("/workf".into());
    assert_eq!(
        app.slash_suggestions().unwrap()[0].name,
        "workflow",
        "name prefixes outrank description-only matches"
    );
}

#[test]
fn command_catalog_has_core_haarness_style_actions() {
    let names = COMMANDS.iter().map(|entry| entry.name).collect::<Vec<_>>();
    assert!(names.contains(&"input-mode"));
    assert!(!names.contains(&"settings"));
    for expected in [
        "new",
        "sessions",
        "models",
        "providers",
        "agents",
        "plan",
        "review",
        "compact",
        "diff",
        "undo",
        "redo",
        "timeline",
        "ultracode",
        "workflow",
        "mcp",
        "skills",
        "status",
        "background",
        "help",
        "exit",
    ] {
        assert!(names.contains(&expected), "missing /{expected}");
    }
    let sessions = COMMANDS
        .iter()
        .find(|entry| entry.name == "sessions")
        .expect("canonical /sessions command");
    assert!(sessions.aliases.contains(&"resume"));
    assert!(sessions.aliases.contains(&"threads"));
    let workflow = COMMANDS
        .iter()
        .find(|entry| entry.name == "workflow")
        .expect("canonical /workflow command");
    assert!(workflow.aliases.contains(&"workflows"));
    assert!(workflow.aliases.contains(&"wf"));
}

#[test]
fn default_model_comes_from_a_connected_provider() {
    let providers = vec![
        ProviderInfo {
            id: "anthropic".into(),
            name: "Anthropic".into(),
            authenticated: true,
            models: vec![haarness_protocol::ModelInfo {
                id: "claude-test".into(),
                display_name: "Claude Test".into(),
                reasoning: true,
                reasoning_efforts: ["low", "medium", "high"].map(str::to_owned).to_vec(),
                images: true,
                video: false,
                tools: true,
                context_window: None,
                max_output_tokens: None,
                pricing: None,
            }],
        },
        ProviderInfo {
            id: "openai".into(),
            name: "OpenAI".into(),
            authenticated: false,
            models: vec![haarness_protocol::ModelInfo {
                id: "gpt-test".into(),
                display_name: "GPT Test".into(),
                reasoning: true,
                reasoning_efforts: ["low", "medium", "high"].map(str::to_owned).to_vec(),
                images: true,
                video: false,
                tools: true,
                context_window: None,
                max_output_tokens: None,
                pricing: None,
            }],
        },
    ];

    assert_eq!(
        default_connected_model(&providers),
        Some(("anthropic".into(), "claude-test".into()))
    );
}

#[test]
fn completed_tools_do_not_remain_stuck_on_running() {
    let thread = fixture_thread();
    let call_id = "call_1".to_string();
    let items = vec![
        Item {
            id: ItemId::new(),
            thread_id: thread.id,
            turn_id: None,
            sequence: 1,
            data: ItemData::ToolCall {
                call_id: call_id.clone(),
                name: "list".into(),
                arguments: serde_json::json!({"path": "."}),
                provider_metadata: serde_json::Value::Null,
            },
            created_at: Utc::now(),
        },
        Item {
            id: ItemId::new(),
            thread_id: thread.id,
            turn_id: None,
            sequence: 2,
            data: ItemData::ToolResult {
                call_id,
                name: "list".into(),
                output: "crates".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
            created_at: Utc::now(),
        },
    ];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let lines = app.lines((80, 24));

    let row = lines
        .iter()
        .find(|line| line_text(line).contains("● list"))
        .expect("tool row rendered");
    assert!(
        !line_text(row).contains('…'),
        "completed tools must not show the running ellipsis: {}",
        line_text(row)
    );
    let marker = row.spans.first().unwrap();
    assert_eq!(
        marker.fg, app.theme.muted,
        "completed tools settle to the muted state"
    );
}

fn message_item(sequence: u64, thread: &Thread, data: ItemData) -> Item {
    Item {
        id: ItemId::new(),
        thread_id: thread.id,
        turn_id: None,
        sequence,
        data,
        created_at: Utc::now(),
    }
}

fn message_item_for_turn(sequence: u64, thread: &Thread, turn_id: TurnId, data: ItemData) -> Item {
    let mut item = message_item(sequence, thread, data);
    item.turn_id = Some(turn_id);
    item
}

#[test]
fn transcript_reads_top_down_with_the_composer_pinned_at_the_bottom() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "short answer".into(),
        },
    )];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let lines = app.lines((80, 24));
    let content_row = lines
        .iter()
        .position(|line| line_text(line).contains("short answer"))
        .unwrap();
    // Content reads like a document: right under the header, not floating
    // above the composer with a wall of blank rows over it.
    assert!(
        content_row <= 4,
        "expected top-anchored content, found it at row {content_row}"
    );
    let composer_row = lines
        .iter()
        .position(|line| line_text(line).contains("Ask anything"))
        .unwrap();
    assert!(composer_row >= 20, "composer stays pinned to the bottom");
}

#[test]
fn root_composer_has_a_gutter_after_a_full_transcript_window() {
    let thread = fixture_thread();
    let items = (1..=30)
        .map(|sequence| {
            message_item(
                sequence,
                &thread,
                ItemData::AssistantMessage {
                    text: format!("response {sequence}"),
                },
            )
        })
        .collect();
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let lines = app.lines((80, 24));
    let last = lines.len() - 1;

    assert!(line_text(&lines[last]).contains("build"), "band");
    assert!(
        line_text(&lines[last - 1])
            .chars()
            .all(|character| character == '\u{2500}'),
        "rule"
    );
    assert!(
        line_text(&lines[last - 2]).contains("Ask anything"),
        "prompt"
    );
    assert!(
        line_text(&lines[last - 3]).trim().is_empty(),
        "a deliberate empty row separates transcript content from the composer"
    );
    assert!(
        line_text(&lines[last - 4]).contains("response 30"),
        "the gutter must not become transcript content or hide the latest reply"
    );
}

#[test]
fn root_composer_gutter_yields_before_clipping_a_compact_composer() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "newest reply".into(),
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.attachments.push("screenshot.png".into());
    app.composer
        .set_text("one\ntwo\nthree\nfour\nfive\nsix".into());

    let lines = app.lines((40, 12));
    assert_eq!(lines.len(), 12);
    assert!(
        lines
            .iter()
            .any(|line| line_text(line).contains("screenshot.png")),
        "attachment chips survive a compact frame"
    );
    for row in ["one", "six"] {
        assert!(
            lines.iter().any(|line| line_text(line).trim() == row),
            "every windowed input row stays visible: {row}"
        );
    }
    let last = lines.len() - 1;
    assert!(
        line_text(&lines[last]).contains("build"),
        "the band survives"
    );
    assert!(
        line_text(&lines[last - 1])
            .chars()
            .all(|character| character == '\u{2500}'),
        "so does the rule"
    );
    assert!(
        lines
            .iter()
            .any(|line| line_text(line).contains("newest reply")),
        "the newest reply is never pushed off a compact frame"
    );
}

#[test]
fn compact_autocomplete_reserves_the_complete_composer_and_latest_reply() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "newest reply".into(),
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.composer.set_text("/".into());

    let compact = app.lines((40, 12));
    assert!(
        compact
            .iter()
            .any(|line| line_text(line).contains("newest reply")),
        "autocomplete must leave the newest transcript row visible"
    );
    let last = compact.len() - 1;
    assert!(
        line_text(&compact[last]).contains("build"),
        "autocomplete must not truncate the band: {compact:?}"
    );
    assert!(
        line_text(&compact[last - 1])
            .chars()
            .all(|character| character == '\u{2500}'),
        "nor the rule"
    );
    assert!(
        line_text(&compact[last - 2]).contains('/'),
        "nor the prompt itself"
    );
    assert!(
        app.hit_rows
            .borrow()
            .iter()
            .any(|hit| matches!(hit.target, crate::view::frame::HitTarget::MenuRow(_))),
        "the menu still gets the rows that remain"
    );

    let regular = app.lines((80, 24));
    assert!(line_text(regular.last().unwrap()).contains("GPT-5.4"));
    assert_eq!(
        app.hit_rows
            .borrow()
            .iter()
            .filter(|hit| matches!(hit.target, crate::view::frame::HitTarget::MenuRow(_)))
            .count(),
        7,
        "normal-height autocomplete keeps its seven-choice capacity"
    );
}

#[test]
fn scrollback_offset_reveals_earlier_content() {
    let thread = fixture_thread();
    let items = (1..=40)
        .map(|sequence| {
            message_item(
                sequence,
                &thread,
                ItemData::AssistantMessage {
                    text: format!("marker-{sequence}"),
                },
            )
        })
        .collect();
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let bottom = app.lines((80, 24));
    assert!(
        bottom
            .iter()
            .any(|line| line_text(line).contains("marker-40"))
    );
    assert!(
        !bottom
            .iter()
            .any(|line| line_text(line).contains("marker-1 "))
    );

    app.scroll(1000);
    let limit = app.max_viewport_offset.get();
    assert_eq!(app.viewport_offset, limit, "scroll state clamps at the top");
    app.scroll(1000);
    assert_eq!(
        app.viewport_offset, limit,
        "repeated wheel-up cannot overshoot"
    );
    let top = app.lines((80, 24));
    assert!(top.iter().any(|line| line_text(line).contains("marker-1")));
    assert!(!top.iter().any(|line| line_text(line).contains("marker-40")));
    assert!(
        top.iter()
            .any(|line| line_text(line).contains("scrolled · end to resume")),
        "scroll indicator should be visible while scrolled"
    );
    app.scroll(-3);
    assert_eq!(
        app.viewport_offset,
        limit.saturating_sub(3),
        "one wheel-down step moves immediately after reaching the limit"
    );

    app.scroll_to_bottom();
    let back = app.lines((80, 24));
    assert!(
        back.iter()
            .any(|line| line_text(line).contains("marker-40"))
    );
}

#[test]
fn live_rows_do_not_move_a_manually_scrolled_view() {
    let thread = fixture_thread();
    let items = (1..=30)
        .map(|sequence| {
            message_item(
                sequence,
                &thread,
                ItemData::AssistantMessage {
                    text: format!("anchor-{sequence}"),
                },
            )
        })
        .collect();
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let _ = app.lines((80, 24));
    app.scroll(1);
    let before = app
        .lines((80, 24))
        .into_iter()
        .map(|line| line_text(&line))
        .filter(|line| line.contains("anchor-"))
        .collect::<Vec<_>>();
    app.preview = "a new streamed row".into();
    let after = app
        .lines((80, 24))
        .into_iter()
        .map(|line| line_text(&line))
        .filter(|line| line.contains("anchor-"))
        .collect::<Vec<_>>();
    assert_eq!(
        before, after,
        "even a one-line upward gesture suspends stream following"
    );
    app.scroll(-1_000_000);
    assert_eq!(
        app.effective_viewport_offset(),
        0,
        "scrolling back to the bottom resumes following"
    );
    app.preview.push_str("\nand keeps following");
    assert!(
        app.lines((80, 24))
            .iter()
            .any(|line| line_text(line).contains("and keeps following"))
    );
}

#[test]
fn active_stream_keeps_the_scroll_recovery_and_interrupt_hints_visible() {
    let thread = fixture_thread();
    let items = (1..=30)
        .map(|sequence| {
            message_item(
                sequence,
                &thread,
                ItemData::AssistantMessage {
                    text: format!("status-anchor-{sequence}"),
                },
            )
        })
        .collect();
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.active_turn = Some(haarness_protocol::TurnId::new());
    let _ = app.lines((100, 30));
    app.scroll(1);

    for width in [40, 100] {
        let status = app
            .lines((width, 30))
            .into_iter()
            .find(|line| line_text(line).contains("scrolled"))
            .unwrap_or_else(|| panic!("{width}-column active band lost its scroll hint"));
        let text = line_text(&status);
        assert!(
            text.contains("end") && !text.contains("ctrl+c"),
            "{width}-column active band keeps the scroll hint and stays quiet about stopping until asked: {text}"
        );
    }
}

#[test]
fn streaming_markdown_reflow_does_not_move_a_manually_scrolled_view() {
    let thread = fixture_thread();
    let items = (1..=40)
        .map(|sequence| {
            message_item(
                sequence,
                &thread,
                ItemData::AssistantMessage {
                    text: format!("stable-anchor-{sequence}"),
                },
            )
        })
        .collect();
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.preview = concat!(
        "[release notes](https://example.com/a/very/long/streamed/link/",
        "whose/unclosed/destination/wraps/across/several/terminal/rows",
    )
    .into();
    let _ = app.lines((40, 18));
    app.scroll(10);

    let visible_anchors = |app: &App| {
        app.lines((40, 18))
            .into_iter()
            .map(|line| line_text(&line))
            .filter(|line| line.contains("stable-anchor-"))
            .collect::<Vec<_>>()
    };
    let before = visible_anchors(&app);

    // This is append-only provider output, but closing the Markdown link
    // replaces its visible destination with the short label and makes the
    // rendered live block shorter. A paused transcript must keep its exact
    // durable rows rather than moving with that reflow.
    app.preview.push(')');
    let after_link_closes = visible_anchors(&app);
    assert_eq!(
        before, after_link_closes,
        "stream reflow preserves the paused transcript window"
    );

    app.preview
        .push_str("\n\nThe stream keeps adding rows after the reflow.");
    let after_more_output = visible_anchors(&app);
    assert_eq!(
        before, after_more_output,
        "later stream growth keeps the same paused transcript window"
    );
}

#[test]
fn new_item_kinds_render_in_the_transcript() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::Todo {
                items: vec![
                    haarness_protocol::TodoEntry {
                        content: "write tests".into(),
                        status: "in_progress".into(),
                        priority: "high".into(),
                    },
                    haarness_protocol::TodoEntry {
                        content: "ship".into(),
                        status: "pending".into(),
                        priority: "medium".into(),
                    },
                ],
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::Question {
                question_id: "q1".into(),
                prompt: "Which color?".into(),
                choices: vec!["red".into(), "blue".into()],
                multiple: false,
                custom: true,
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::PermissionRequest {
                permission_id: "p1".into(),
                operation: "bash:rm -rf target".into(),
                detail: serde_json::json!({"command": "rm -rf target"}),
                permission: "bash".into(),
                patterns: vec!["rm -rf target".into()],
                always_patterns: vec!["bash:rm *".into()],
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::Compaction {
                summary: "Earlier exploration summarized".into(),
                files_touched: vec![],
                outstanding: vec![],
                todo_checkpoint: None,
                tail_start_sequence: None,
            },
        ),
        message_item(
            5,
            &thread,
            ItemData::Plan {
                markdown: "Step one: look around".into(),
            },
        ),
        message_item(
            6,
            &thread,
            ItemData::Review {
                findings: vec![haarness_protocol::ReviewFinding {
                    severity: "high".into(),
                    file: Some("src/lib.rs".into()),
                    line: Some(42),
                    explanation: "off-by-one".into(),
                }],
            },
        ),
        message_item(
            7,
            &thread,
            ItemData::McpAction {
                server: "files".into(),
                action: "connected".into(),
                detail: serde_json::Value::Null,
            },
        ),
    ];
    let mut app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    // Answered (non-pending) prompts stay out of a replayed transcript even
    // though their durable request items remain in history.
    let text: String = app
        .lines((100, 40))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(
        !text.contains("? Which color?"),
        "answered questions are hidden after replay: {text}"
    );
    assert!(
        !text.contains("⚠ bash:rm -rf target"),
        "answered permission prompts are hidden"
    );
    // Pending prompts still render as interactive rows.
    app.pending_questions
        .push(haarness_protocol::PendingQuestionInfo {
            question_id: "q1".into(),
            prompt: "Which color?".into(),
            choices: vec!["red".into(), "blue".into()],
            multiple: false,
            custom: true,
            thread_id: thread.id,
            root_thread_id: Some(thread.id),
            turn_id: None,
            created_at: chrono::Utc::now(),
        });
    app.pending_permissions
        .push(haarness_protocol::PendingPermissionInfo {
            allow_always: None,
            repository_scope: None,
            permission_id: "p1".into(),
            operation: "bash:rm -rf target".into(),
            detail: serde_json::json!({"command": "rm -rf target"}),
            permission: "bash".into(),
            patterns: vec!["rm -rf target".into()],
            always_patterns: vec!["bash:rm *".into()],
            thread_id: thread.id,
            root_thread_id: Some(thread.id),
            turn_id: None,
            agent: None,
            created_at: chrono::Utc::now(),
        });
    let text: String = app
        .lines((100, 40))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(!text.contains("◐ write tests"));
    assert!(!text.contains("☐ ship"));
    assert!(text.contains("? Which color?"));
    assert!(text.contains("1. red"));
    assert!(text.contains("⚠ Run a shell command"));
    assert!(text.contains("$ rm -rf target"));
    assert!(text.contains("── compacted · Earlier exploration summarized"));
    assert!(text.contains("plan"));
    assert!(text.contains("Step one: look around"));
    assert!(text.contains("src/lib.rs:42"));
    assert!(text.contains("◆ files · connected"));
}

#[test]
fn multi_line_tool_output_collapses_without_preview_or_line_count() {
    let thread = fixture_thread();
    let mut output = String::new();
    for row in 1..=20 {
        use std::fmt::Write as _;
        let _ = writeln!(output, "row-{row}");
    }
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "c1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "ls"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolResult {
                call_id: "c1".into(),
                name: "bash".into(),
                output,
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let text: String = app
        .lines((100, 45))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(text.contains("● bash ls"), "compact tool row: {text}");
    assert!(
        !text.contains("20 lines"),
        "collapsed tool rows do not advertise output line counts: {text}"
    );
    assert!(
        !text.contains("row-1"),
        "no head: two lines of a long dump are never the part you wanted: {text}"
    );
    assert!(
        !text.contains("more lines"),
        "collapsed output does not show an overflow note: {text}"
    );
}

#[test]
fn exploratory_results_are_compact_and_expand_beside_their_call() {
    let thread = fixture_thread();
    // Providers can emit all calls first and all results second. The UI must
    // pair them semantically instead of reproducing that wire order.
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "read-1".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": "README.md"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolCall {
                call_id: "list-1".into(),
                name: "list".into(),
                arguments: serde_json::json!({"path": "crates"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::ToolResult {
                call_id: "read-1".into(),
                name: "read".into(),
                output: "README BODY\nsecond line".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::ToolResult {
                call_id: "list-1".into(),
                name: "list".into(),
                output: "haarness-core\nhaarness-tui".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ];
    let read_result = items[2].id;
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.sidebar_open = false;
    let compact = app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(compact.contains("read README.md"), "{compact}");
    assert!(compact.contains("list crates"), "{compact}");
    assert!(!compact.contains("2 lines"), "{compact}");
    assert!(
        !compact.contains("README BODY"),
        "routine bodies stay folded"
    );
    assert!(
        !compact.contains("haarness-core"),
        "routine bodies stay folded"
    );

    app.expanded.insert(read_result);
    let expanded = app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    let call = expanded.find("read README.md").unwrap();
    let body = expanded.find("README BODY").unwrap();
    let next_call = expanded.find("list crates").unwrap();
    assert!(
        call < body && body < next_call,
        "expanded result stays with its call instead of a detached result batch: {expanded}"
    );
}

#[test]
fn repeated_provider_call_ids_pair_with_the_result_from_the_same_turn() {
    let thread = fixture_thread();
    let first_turn = TurnId::new();
    let second_turn = TurnId::new();
    let mut items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "echo-call-1".into(),
                name: "skill".into(),
                arguments: serde_json::json!({"name": "old-skill"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolResult {
                call_id: "echo-call-1".into(),
                name: "skill".into(),
                output: "OLD RESULT".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::ToolCall {
                call_id: "echo-call-1".into(),
                name: "skill".into(),
                arguments: serde_json::json!({"name": "new-skill"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::ToolResult {
                call_id: "echo-call-1".into(),
                name: "skill".into(),
                output: "NEW RESULT".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ];
    items[0].turn_id = Some(first_turn);
    items[1].turn_id = Some(first_turn);
    items[2].turn_id = Some(second_turn);
    items[3].turn_id = Some(second_turn);
    let second_result = items[3].id;

    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.sidebar_open = false;
    app.expanded.insert(second_result);
    let rendered = app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("skill old-skill"), "{rendered}");
    assert!(rendered.contains("skill new-skill"), "{rendered}");
    assert!(rendered.contains("NEW RESULT"), "{rendered}");
    assert!(
        !rendered.contains("OLD RESULT"),
        "expanding the later row must not reveal an earlier turn's result: {rendered}"
    );
}

#[test]
fn transcript_spacing_separates_semantic_groups_without_splitting_activity_blocks() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::UserMessage {
                text: "Inspect this".into(),
                attachments: Vec::new(),
                queued: false,
                synthetic: false,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolCall {
                call_id: "read-spacing".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": "README.md"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::ToolResult {
                call_id: "read-spacing".into(),
                name: "read".into(),
                output: "read body".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::ToolCall {
                call_id: "bash-spacing".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "printf rows"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            5,
            &thread,
            ItemData::ToolResult {
                call_id: "bash-spacing".into(),
                name: "bash".into(),
                output: "result one\nresult two".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            6,
            &thread,
            ItemData::AssistantMessage {
                text: "Summary:\n\n- first point\n- second point".into(),
            },
        ),
        message_item(
            7,
            &thread,
            ItemData::Todo {
                items: vec![haarness_protocol::TodoEntry {
                    content: "write tests".into(),
                    status: "in_progress".into(),
                    priority: "high".into(),
                }],
            },
        ),
        message_item(
            8,
            &thread,
            ItemData::ToolCall {
                call_id: "task-spacing-a".into(),
                name: "task".into(),
                arguments: serde_json::json!({"prompt": "audit alpha"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            9,
            &thread,
            ItemData::ToolCall {
                call_id: "task-spacing-b".into(),
                name: "task".into(),
                arguments: serde_json::json!({"prompt": "audit beta"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
    ];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let rows = crate::view::transcript::transcript_lines(&app, 100)
        .lines
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>();
    let position = |needle: &str| {
        rows.iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle:?} in {rows:?}"))
    };

    let user = position("Inspect this");
    let read = position("read README.md");
    let bash = position("bash printf rows");
    let summary = position("Summary:");
    let first_point = position("first point");
    let second_point = position("second point");
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("# Todos") || row.contains("write tests"))
    );
    let task_a = position("task audit alpha");
    let task_b = position("task audit beta");

    assert_eq!(read, user + 2, "one row separates user and tools");
    assert_eq!(bash, read + 1, "consecutive tools remain a dense group");
    assert!(!rows[read].contains("1 line"), "{rows:?}");
    assert!(!rows[bash].contains("2 lines"), "{rows:?}");
    assert_eq!(summary, bash + 2, "one row precedes assistant prose");
    assert_eq!(
        second_point,
        first_point + 1,
        "Markdown list items remain contiguous"
    );
    assert_eq!(task_a, second_point + 2, "one row precedes the task roster");
    assert_eq!(task_b, task_a + 1, "task rows remain a dense group");
    assert!(!rows.first().is_some_and(|row| row.trim().is_empty()));
    assert!(!rows.last().is_some_and(|row| row.trim().is_empty()));
    assert!(
        !rows
            .windows(2)
            .any(|pair| pair.iter().all(|row| row.trim().is_empty())),
        "semantic boundaries never accumulate doubled blank rows: {rows:?}"
    );
}

#[test]
fn short_transcript_window_keeps_single_separators_without_wasting_rows() {
    let thread = fixture_thread();
    let items = (1..=5)
        .map(|sequence| {
            message_item(
                sequence,
                &thread,
                ItemData::AssistantMessage {
                    text: format!("response {sequence}"),
                },
            )
        })
        .collect();
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let frame = app.lines((60, 12));
    let transcript_window = &frame[2..9];
    assert!(
        transcript_window
            .iter()
            .any(|line| line_text(line).contains("response 5")),
        "the newest semantic block stays visible"
    );
    assert!(
        !transcript_window
            .windows(2)
            .any(|pair| pair.iter().all(|line| line_text(line).trim().is_empty())),
        "a short transcript cannot spend two adjacent rows on separators"
    );
}

#[test]
fn usage_totals_reach_the_composer_label() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::Usage {
                prompt: None,
                input_tokens: 12_400,
                output_tokens: 950,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: None,
                estimated: false,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::Usage {
                prompt: None,
                input_tokens: 600,
                output_tokens: 50,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: None,
                estimated: false,
            },
        ),
    ];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    assert_eq!(app.usage_totals(), (13_000, 1000));
    app.notice.clear();
    let text: String = app
        .lines((100, 24))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(text.contains("13.0k↑ 1.0k↓"), "{text}");

    // Feedback and the counters share the band's state half rather than
    // hiding each other.
    app.notice = "Reasoning off".into();
    let together: String = app
        .lines((100, 24))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(together.contains("Reasoning off"), "{together}");
    assert!(together.contains("13.0k↑ 1.0k↓"), "{together}");

    // On a terminal with no room for both, the answer to what you just did
    // wins.
    let narrow: String = app
        .lines((52, 24))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(narrow.contains("Reasoning off"), "{narrow}");
    assert!(!narrow.contains("13.0k↑ 1.0k↓"), "{narrow}");
}

#[test]
fn compact_count_formats_are_stable() {
    use crate::view::span::compact_count;
    assert_eq!(compact_count(950), "950");
    assert_eq!(compact_count(12_400), "12.4k");
    assert_eq!(compact_count(1_000_000), "1000k");
}

#[test]
fn live_reasoning_preview_shows_every_returned_line() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.reasoning_preview = String::new();
    for row in 1..=6 {
        use std::fmt::Write as _;
        let _ = writeln!(app.reasoning_preview, "thought-{row}");
    }
    let text: String = app
        .lines((100, 24))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(
        !text.contains("thinking"),
        "reasoning has no extra chrome: {text}"
    );
    assert!(text.contains("thought-6"));
    assert!(
        text.contains("thought-1"),
        "provider-visible live reasoning is shown in full by default: {text}"
    );
}

#[test]
fn durable_reasoning_is_plain_muted_text_without_disclosure_chrome() {
    let thread = fixture_thread();
    let reasoning = message_item(
        1,
        &thread,
        ItemData::Reasoning {
            text: (1..=6)
                .map(|row| format!("thought-{row}"))
                .collect::<Vec<_>>()
                .join("\n"),
        },
    );
    let mut app = App::new(
        thread.clone(),
        vec![thread],
        vec![reasoning],
        vec![],
        vec![],
    );
    app.sidebar_open = false;

    let expanded: String = app
        .lines((100, 24))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(expanded.contains("thought-1"), "{expanded}");
    assert!(expanded.contains("thought-6"), "{expanded}");
    assert!(!expanded.contains("thinking"), "{expanded}");
    assert!(!expanded.contains("click to"), "{expanded}");
}

/// Reasoning used to hang two columns to the right of everything else, which
/// read as an unexplained indent and wrapped it to a narrower measure than the
/// prose around it.
#[test]
fn reasoning_shares_the_conversation_text_column() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::Reasoning {
                text: "reasoned ".repeat(40).trim().into(),
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::AssistantMessage {
                text: "answered ".repeat(40).trim().into(),
            },
        ),
    ];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.sidebar_open = false;

    let lines = app.lines((60, 40));
    let rows = |needle: &str| -> Vec<String> {
        lines
            .iter()
            .map(line_text)
            .filter(|text| text.contains(needle))
            .collect()
    };
    let reasoned = rows("reasoned");
    let answered = rows("answered");
    assert!(reasoned.len() > 1, "reasoning should wrap: {reasoned:?}");
    assert!(answered.len() > 1, "prose should wrap: {answered:?}");

    // The assistant's opening row carries the `▸` marker in place of the
    // inset, so its continuation rows are what share the text column.
    for text in reasoned.iter().chain(answered.iter().skip(1)) {
        assert_eq!(
            text.chars().count() - text.trim_start().chars().count(),
            2,
            "one text column: {text:?}"
        );
    }
    let measure = |rows: &[String]| {
        rows.iter()
            .map(|text| text.trim_end().chars().count())
            .max()
            .unwrap_or_default()
    };
    assert_eq!(
        measure(&reasoned),
        measure(&answered),
        "reasoning and prose wrap to one measure"
    );
}

#[test]
fn assistant_replies_carry_the_identity_marker() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::AssistantMessage {
                text: "crates".into(),
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::AssistantMessage {
                text: "one\ntwo continuation".into(),
            },
        ),
    ];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let lines = app.lines((80, 24));
    let marked: Vec<&Line> = lines
        .iter()
        .filter(|line| line_text(line).starts_with("▸ "))
        .collect();
    assert_eq!(marked.len(), 2, "one marker per reply, first line only");
    assert!(line_text(marked[0]).contains("crates"));
    let anchor = marked[0].spans.first().unwrap();
    assert_eq!(anchor.fg, app.theme.primary);
    assert!(
        lines
            .iter()
            .any(|line| line_text(line).starts_with("  two continuation")),
        "continuation lines stay unmarked"
    );
}

#[test]
fn assistant_reply_starting_with_heading_keeps_marker_beside_heading() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "# Security audit findings\n\nScope reviewed: providers".into(),
        },
    )];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let lines = app.lines((90, 30));
    let marked: Vec<String> = lines
        .iter()
        .map(line_text)
        .filter(|text| text.trim_start().starts_with('▸'))
        .collect();

    assert_eq!(
        marked.len(),
        1,
        "one marker per assistant reply: {marked:?}"
    );
    assert_eq!(marked[0], "▸ Security audit findings");
}

#[test]
fn streamed_assistant_heading_keeps_marker_beside_heading() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.preview = "# Security audit findings\n\nScope reviewed: providers".into();

    let lines = app.lines((90, 30));
    let marked: Vec<String> = lines
        .iter()
        .map(line_text)
        .filter(|text| text.trim_start().starts_with('▸'))
        .collect();

    assert_eq!(marked.len(), 1, "one marker per live reply: {marked:?}");
    assert_eq!(marked[0], "▸ Security audit findings");
}

#[test]
fn renamed_builtin_theme_restores_saved_variants_and_respects_custom_overrides() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    assert!(!app.themes.iter().any(|entry| entry.name == "opencode"));
    for variant in ["dark", "light"] {
        assert!(app.apply_theme("classic", variant));
        let expected = app.theme;
        assert!(app.apply_theme("opencode", variant));
        assert_eq!(app.theme_name, "classic");
        assert_eq!(app.theme_variant, variant);
        assert_eq!(app.theme, expected);
    }
    let custom = crate::theme::Theme::editorial_light();
    app.themes.push(crate::theme::ThemeEntry {
        name: "opencode".into(),
        dark: Some(custom),
        light: None,
    });
    assert!(app.apply_theme("opencode", "dark"));
    assert_eq!(app.theme_name, "opencode");
    assert_eq!(app.theme, custom);
    assert!(!app.apply_theme("opencode", "light"));
}

#[test]
fn theme_picker_previews_the_highlighted_row_and_escape_restores() {
    let mut app = selector_fixture();
    let start = (app.theme_name.clone(), app.theme_variant.clone());
    let original = app.theme;

    app.open_overlay(crate::state::Overlay::Themes);
    assert_eq!(
        (app.theme_name.clone(), app.theme_variant.clone()),
        start,
        "opening the picker lands on the active theme and changes nothing"
    );

    app.move_selection(3);
    assert_ne!(
        app.theme, original,
        "browsing the list repaints the interface"
    );
    let previewed = (app.theme_name.clone(), app.theme_variant.clone());
    assert_eq!(
        app.theme_baseline(),
        (start.0.as_str(), start.1.as_str()),
        "the active marker keeps pointing at what escape would keep"
    );
    let frame = app.lines((100, 30));
    assert!(
        frame.iter().any(|line| line
            .spans
            .iter()
            .any(|span| span.bg == Some(app.theme.surface))),
        "the rendered frame uses the previewed palette"
    );

    app.close_overlay();
    assert_eq!(
        (app.theme_name.clone(), app.theme_variant.clone()),
        start,
        "escape abandons the preview"
    );
    assert_eq!(app.theme, original);

    // Enter captures the choice before the overlay closes, so the restore
    // cannot undo a committed selection.
    app.open_overlay(crate::state::Overlay::Themes);
    app.move_selection(3);
    let choice = app.selected_overlay_choice().expect("a highlighted theme");
    app.close_overlay();
    let crate::state::OverlayChoice::Theme(name, variant) = choice else {
        panic!("the themes overlay must yield a theme choice");
    };
    assert!(app.apply_theme(&name, &variant));
    assert_eq!(
        (app.theme_name.clone(), app.theme_variant.clone()),
        previewed
    );
}

#[test]
fn theme_switching_applies_variants_and_invalidates_renders() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let generation = app.render_generation;
    assert!(app.apply_theme("editorial", "light"));
    assert_eq!(app.theme, Theme::editorial_light());
    assert_eq!(app.theme_variant, "light");
    assert!(app.render_generation > generation, "caches must invalidate");
    assert!(!app.apply_theme("missing", "dark"));
    assert_eq!(app.theme_variant, "light", "failed switches change nothing");

    app.themes
        .push(crate::theme::parse_theme_file("custom", "[dark]\nbg = \"#102030\"\n").unwrap());
    assert!(app.apply_theme("custom", "dark"));
    assert_eq!(app.theme.bg, Color::rgb(16, 32, 48));
}

#[test]
fn explicit_themes_paint_their_background_in_limited_color_terminals() {
    use crate::config::BackgroundMode;
    let support = haarness_opentui::ColorSupport {
        truecolor: false,
        ansi256: true,
    };
    let mut app = selector_fixture();
    assert_eq!(app.theme_name, "haarness");
    assert_eq!(app.theme, Theme::haarness_dark());
    for selected in [false, true] {
        app.theme_selected = selected;
        for preview in [false, true] {
            if preview {
                app.open_overlay(crate::state::Overlay::Themes);
            }
            assert!(app.apply_theme("haarness", "light"));
            let mode = BackgroundMode::Auto
                .for_theme_choice(app.theme_selected || app.theme_preview_restore.is_some());
            app.set_terminal_background(crate::uses_terminal_background(mode, support));
            assert_eq!(app.terminal_background, !selected && !preview);
            if selected || preview {
                assert_eq!(
                    crate::page_background(app.theme.bg, mode, support),
                    Theme::haarness_light().bg
                );
                assert_eq!(app.theme.surface, Theme::haarness_light().surface);
                assert_eq!(
                    crate::initial_theme_variant(false, Some("dark"), Some("light")),
                    "light"
                );
            }
            if preview {
                app.close_overlay();
            }
        }
    }
    assert_eq!(
        BackgroundMode::Terminal.for_theme_choice(true),
        BackgroundMode::Terminal
    );
    assert_eq!(
        BackgroundMode::Theme.for_theme_choice(false),
        BackgroundMode::Theme
    );
}

#[test]
fn assistant_markdown_renders_without_literal_markers() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "**bold** and `code`\n\n```rust\nfn main() {}\n```".into(),
        },
    )];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let lines = app.lines((90, 30));
    let text: String = lines.iter().map(|line| line_text(line) + "\n").collect();
    assert!(text.contains("bold"), "{text}");
    assert!(!text.contains("**"), "{text}");
    assert!(text.contains("fn main"), "{text}");
    let code_line = lines
        .iter()
        .find(|line| line_text(line).contains("fn main"))
        .unwrap();
    assert!(
        code_line
            .spans
            .iter()
            .any(|span| span.bg == Some(app.theme.surface)),
        "code renders on the surface background"
    );
}

#[test]
fn edit_results_are_collapsed_then_expand_to_a_diff_block() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "c1".into(),
                name: "edit".into(),
                arguments: serde_json::json!({"path":"a.rs","old":"let x = 1;","new":"let x = 2;"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolResult {
                call_id: "c1".into(),
                name: "edit".into(),
                output: "updated a.rs".into(),
                is_error: false,
                metadata: serde_json::json!({
                    "diffs": [{
                        "file": "a.rs",
                        "status": "modified",
                        "additions": 1,
                        "deletions": 1,
                        "before": "let x = 1;\n",
                        "after": "let x = 2;\n"
                    }]
                }),
            },
        ),
    ];
    let result_id = items[1].id;
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let collapsed = app.lines((90, 30));
    let collapsed_text = collapsed
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        collapsed_text.contains("edit a.rs  ·  +1 −1"),
        "{collapsed_text}"
    );
    assert!(!collapsed_text.contains("- let x = 1;"), "{collapsed_text}");
    assert!(!collapsed_text.contains("+ let x = 2;"), "{collapsed_text}");

    app.expanded.insert(result_id);
    let lines = app.lines((90, 30));
    let text: String = lines.iter().map(|line| line_text(line) + "\n").collect();
    assert!(text.contains("- let x = 1;"), "{text}");
    assert!(text.contains("+ let x = 2;"), "{text}");
    let added = lines
        .iter()
        .find(|line| line_text(line).contains("+ let x = 2;"))
        .unwrap();
    assert!(
        added
            .spans
            .iter()
            .any(|span| span.bg == Some(app.theme.diff.added_bg))
    );
}

#[test]
fn home_is_a_launch_masthead_settled_onto_the_composer() {
    let thread = fixture_thread();
    let other = Thread {
        id: ThreadId::new(),
        title: "Earlier work".into(),
        ..fixture_thread()
    };
    let mut app = App::new(thread.clone(), vec![thread, other], vec![], vec![], vec![]);
    app.server_info = Some(haarness_protocol::ServerInfo {
        name: "haarness".into(),
        version: "0.1.0".into(),
        schema_version: "1".into(),
        server_instance_id: uuid::Uuid::now_v7(),
        event_cursor: 0,
        capabilities: vec![],
        workspace: Some("/Users/alex/work/haarness".into()),
        git_branch: Some("main".into()),
        default_provider: None,
        default_model: None,
        default_agent: None,
        project_trusted: Some(false),
    });
    let lines = app.lines((100, 30));
    let text: String = lines.iter().map(|line| line_text(line) + "\n").collect();

    assert!(text.contains("h a a r n e s s"), "{text}");
    assert!(text.contains("durable harness for coding agents"), "{text}");
    assert!(text.contains("/ commands"), "{text}");
    assert!(!text.contains("ctrl+o tasks"), "{text}");
    assert!(text.contains("Ask anything"), "{text}");
    assert!(text.contains("v0.1.0"), "{text}");
    assert!(!text.contains("recent"), "{text}");
    assert!(!text.contains("Earlier work"), "{text}");

    // Everything above the launch block is empty: the masthead settles onto
    // the composer instead of floating in the middle of the terminal.
    let masthead = lines
        .iter()
        .position(|line| line_text(line).contains("h a a r n e s s"))
        .expect("masthead");
    assert!(masthead > 12, "the launch block is bottom-weighted");
    assert!(
        lines[..masthead]
            .iter()
            .all(|line| line_text(line).trim().is_empty()),
        "{text}"
    );

    // The composer sits exactly where it will stay once the session starts.
    let band = lines.last().unwrap();
    assert!(line_text(band).contains("build"), "{}", line_text(band));
    assert!(
        line_text(band).contains("haarness  main"),
        "{}",
        line_text(band)
    );
    let rule = &lines[lines.len() - 2];
    assert_eq!(rule.width(), 100);
    assert!(
        line_text(rule)
            .chars()
            .all(|character| character == '\u{2500}')
    );
}

#[tokio::test]
async fn tab_cycles_agents_forward_and_shift_tab_cycles_backward() {
    let mut app = selector_fixture();
    let mut review = app.agents[0].clone();
    review.id = "review".into();
    review.name = "review".into();
    app.agents.push(review);
    let client = offline_client();

    assert_eq!(app.agent(), "build");
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.agent(), "plan");

    // Terminals normally encode Shift+Tab as BackTab.
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.agent(), "build");
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.agent(), "review");

    // Preserve compatibility with terminals reporting Tab plus Shift.
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.agent(), "plan");
}

#[test]
fn launch_is_fresh_unless_continue_or_session_is_explicit() {
    let first = fixture_thread();
    let second = Thread {
        id: ThreadId::new(),
        title: "Second".into(),
        ..fixture_thread()
    };
    let threads = vec![first.clone(), second.clone()];

    assert!(
        crate::choose_initial_thread(&threads, None, Some(second.id.0), false).is_none(),
        "plain launch creates a new thread/home screen"
    );
    assert_eq!(
        crate::choose_initial_thread(&threads, None, Some(second.id.0), true)
            .map(|thread| thread.id),
        Some(second.id),
        "--continue restores the saved root"
    );
    assert_eq!(
        crate::choose_initial_thread(&threads, Some(first.id.0), Some(second.id.0), false)
            .map(|thread| thread.id),
        Some(first.id),
        "--thread/--session wins without requiring --continue"
    );
}

#[test]
fn user_prompts_render_as_surface_blocks() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::UserMessage {
            text: "make it good".into(),
            attachments: Vec::new(),
            queued: false,
            synthetic: false,
        },
    )];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let lines = app.lines((80, 24));
    let block = lines
        .iter()
        .find(|line| line_text(line).contains("make it good"))
        .unwrap();
    let surface = app.theme.surface;
    assert!(
        block
            .spans
            .iter()
            .filter(|span| !span.text.trim().is_empty() || span.text.len() > 2)
            .any(|span| span.bg == Some(surface)),
        "prompt blocks sit on the surface background"
    );
    assert!(line_text(block).contains("▎"));
}

#[test]
fn status_band_spans_the_full_width_on_surface() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "conversation".into(),
        },
    )];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let lines = app.lines((90, 24));
    let band = lines.last().unwrap();
    assert_eq!(band.width(), 90, "the band is the frame's bottom edge");
    let surface = app.theme.surface;
    let primary = app.theme.primary;
    assert!(
        band.spans
            .iter()
            .all(|span| span.bg == Some(surface) || span.bg == Some(primary)),
        "every band cell is raised: {band:?}"
    );
    assert_eq!(
        cell_style(band, 1).and_then(|(_, background, _)| background),
        Some(primary),
        "the agent chip anchors the left edge"
    );
    assert!(line_text(band).contains("build"));
    assert!(line_text(band).contains("GPT-5.4"));
    assert!(
        lines[..lines.len() - 1].iter().all(|line| line
            .spans
            .iter()
            .all(|span| span.bg != Some(surface))
            || !line_text(line).contains("haarness")),
        "the top of the frame is content, not a second filled bar"
    );
}

/// With a panel on screen the session's identity heads it, and the transcript
/// opens on the conversation instead of repeating the same two rows.
#[test]
fn the_panel_heads_the_session_and_the_transcript_does_not_repeat_it() {
    let mut thread = fixture_thread();
    thread.title = "General Assistance".into();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "conversation".into(),
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    app.sidebar_open = true;

    let lines = app.lines((140, 20));
    let rows: Vec<String> = lines.iter().map(line_text).collect();
    let wordmark = rows
        .iter()
        .position(|row| row.contains("h a a r n e s s"))
        .expect("the panel carries the wordmark");
    assert!(
        rows[wordmark].contains("General Assistance")
            || rows[wordmark + 1].contains("General Assistance"),
        "the title sits directly under the wordmark: {rows:?}"
    );
    // Both sit in the panel's column, right of the transcript's own width.
    let main_width = 140 - app.sidebar_width(140);
    for (row, needle) in [(wordmark, "h a a r n e s s"), (wordmark + 1, "General")] {
        let column = rows[row].find(needle).expect(needle);
        assert!(
            column >= main_width,
            "identity belongs to the panel, not the transcript: column {column} of {:?}",
            rows[row]
        );
    }
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("h a a r n e s s"))
            .count(),
        1,
        "the wordmark is printed once: {rows:?}"
    );
    assert!(
        rows.iter()
            .filter(|row| row.contains("General Assistance"))
            .count()
            == 1,
        "so is the title: {rows:?}"
    );
    let frame = rows.join("\n");
    assert!(
        !frame.contains(crate::view::span::short_id(&thread.id.to_string())),
        "the durable id is not on screen twice over: {frame}"
    );

    // Hiding the panel must not lose the session's identity with it.
    app.sidebar_open = false;
    let hidden: String = app
        .lines((140, 20))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(hidden.contains("h a a r n e s s"), "{hidden}");
    assert!(hidden.contains("General Assistance"), "{hidden}");
}

/// Without a panel the transcript still carries the identity, but never the
/// durable id: that belongs to the band.
#[test]
fn masthead_names_the_session_without_repeating_its_id() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "conversation".into(),
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    app.server_info = Some(haarness_protocol::ServerInfo {
        name: "haarness".into(),
        version: "0.1.0".into(),
        schema_version: "1".into(),
        server_instance_id: uuid::Uuid::now_v7(),
        event_cursor: 0,
        capabilities: vec![],
        workspace: Some("/Users/alex/work/haarness".into()),
        git_branch: Some("main".into()),
        default_provider: None,
        default_model: None,
        default_agent: None,
        project_trusted: Some(true),
    });

    let lines = app.lines((90, 24));
    // The frame opens on a quiet row so the masthead never butts against the
    // top edge of the terminal.
    assert!(
        line_text(&lines[0]).trim().is_empty(),
        "{:?}",
        line_text(&lines[0])
    );
    let wordmark = lines[1].clone();
    let text = line_text(&wordmark);
    assert!(text.contains("h a a r n e s s"), "{text}");
    assert_eq!(wordmark.spans[1].fg, app.theme.primary);
    assert_ne!(wordmark.spans[1].attributes & attr::BOLD, 0);

    let frame: String = lines.iter().map(|line| line_text(line) + "\n").collect();
    assert!(
        !frame.contains(&format!(
            "#{}",
            crate::view::span::short_id(&thread.id.to_string())
        )),
        "the durable id is the band's job, not the masthead's: {frame}"
    );

    let title = lines[2].clone();
    assert!(
        line_text(&title).contains("Test"),
        "{:?}",
        line_text(&title)
    );
    assert_eq!(title.spans[1].fg, app.theme.text);
    assert_ne!(title.spans[1].attributes & attr::BOLD, 0);

    // Place lives in the band, once, instead of being repeated in a top bar.
    assert!(line_text(lines.last().unwrap()).contains("haarness  main"));
}

/// The quiet row at the top belongs to the frame, not the document, so a
/// scrolled conversation still gets air above it rather than butting against
/// the terminal's top edge.
#[test]
fn the_top_gutter_survives_a_scrolled_transcript() {
    let thread = fixture_thread();
    let items = (1..=40)
        .map(|sequence| {
            message_item(
                sequence,
                &thread,
                ItemData::AssistantMessage {
                    text: format!("reply-{sequence}"),
                },
            )
        })
        .collect();
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.sidebar_open = false;

    let lines = app.lines((80, 24));
    assert!(
        line_text(&lines[0]).trim().is_empty(),
        "the frame opens on a quiet row: {:?}",
        line_text(&lines[0])
    );
    // The window sits at the newest reply, so the masthead — the document's
    // own first rows — has scrolled past. The gutter is still there.
    assert!(
        lines
            .iter()
            .any(|line| line_text(line).contains("reply-40")),
        "a long conversation windows to the bottom"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line_text(line).contains("h a a r n e s s")),
        "the top of the document has scrolled away"
    );
}

#[test]
fn goal_header_leads_with_the_objective_and_right_aligns_compact_progress() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "conversation".into(),
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let mut goal = fixture_goal(haarness_protocol::GoalStatus::Active);
    goal.tokens_used = 12_400;
    goal.token_budget = Some(100_000);
    app.install_goal_snapshot(Some(goal));

    let row = app.lines((90, 24))[0].clone();
    let text = line_text(&row);
    assert!(
        text.trim_start().starts_with("◆ Ship a verified release"),
        "{text}"
    );
    assert!(text.ends_with("pursuing · 12.4k / 100k tokens "), "{text}");
    assert!(
        !text.contains("goal ·"),
        "the header repeats its own label: {text}"
    );
    assert_eq!(row.width(), 90);
    assert_eq!(row.spans[0].fg, app.theme.primary);
    assert_eq!(row.spans.last().unwrap().fg, app.theme.muted);
}

#[test]
fn narrow_frames_keep_the_goal_row_and_a_truncated_session_identity() {
    let mut thread = fixture_thread();
    thread.title = "A long session title that cannot fit in a narrow terminal".into();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "conversation".into(),
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    let mut goal = fixture_goal(haarness_protocol::GoalStatus::Active);
    goal.goal = "Ship a carefully verified release across every client".into();
    goal.tokens_used = 12_400;
    app.install_goal_snapshot(Some(goal));

    let lines = app.lines((40, 16));
    let goal = line_text(&lines[0]);
    assert_eq!(lines[0].width(), 40, "{goal}");
    assert!(goal.contains("◆ Ship"), "{goal}");
    assert!(goal.ends_with("pursuing "), "{goal}");

    // A narrow column still affords the letter-spaced wordmark; only the
    // session's own title has to give way.
    let wordmark = line_text(&lines[2]);
    assert!(wordmark.contains("h a a r n e s s"), "{wordmark}");
    let title = line_text(&lines[3]);
    assert!(title.contains('…'), "the title should truncate: {title}");
    let frame: String = lines.iter().map(|line| line_text(line) + "\n").collect();
    assert!(
        !frame.contains(crate::view::span::short_id(&thread.id.to_string())),
        "a narrow frame spends its cells on the title, not the id: {frame}"
    );
}

#[test]
fn a_pinned_goal_keeps_a_quiet_row_before_the_masthead() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::UserMessage {
            text: "advise UX improvements".into(),
            attachments: vec![],
            queued: false,
            synthetic: false,
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.install_goal_snapshot(Some(fixture_goal(haarness_protocol::GoalStatus::Active)));

    let lines = app.lines((90, 18));
    assert!(line_text(&lines[0]).contains("pursuing"));
    assert!(line_text(&lines[1]).trim().is_empty());
    assert!(line_text(&lines[2]).contains("h a a r n e s s"));
    assert!(
        lines
            .iter()
            .any(|line| line_text(line).contains("advise UX improvements"))
    );
}

#[test]
fn prose_respects_the_content_width_cap_on_wide_terminals() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "word ".repeat(80),
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.sidebar_open = false;
    let lines = app.lines((180, 30));
    for line in lines.iter().skip(2).take(20) {
        let text = line_text(line);
        if text.contains("word") {
            assert!(
                text.chars().count() <= 102,
                "capped at ~100 columns, got {}",
                text.chars().count()
            );
        }
    }
}

#[test]
fn reasoning_command_opens_the_effort_modal() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Options {
        title: "reasoning".into(),
        kind: crate::state::OptionsKind::ReasoningEffort,
        entries: vec![
            crate::state::OptionEntry {
                label: "high".into(),
                description: "Deep thinking".into(),
                value: "high".into(),
            },
            crate::state::OptionEntry {
                label: "minimal".into(),
                description: "Fastest".into(),
                value: "minimal".into(),
            },
        ],
    });
    let text: String = app
        .lines((90, 24))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(text.contains("reasoning"), "{text}");
    assert!(text.contains("high"), "{text}");
    app.overlay_filter = "mini".into();
    let choice = app.selected_overlay_choice();
    assert!(matches!(
        choice,
        Some(crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::ReasoningEffort,
            value
        )) if value == "minimal"
    ));
    app.apply_option(crate::state::OptionsKind::ReasoningEffort, "high");
    assert_eq!(app.reasoning_effort.as_deref(), Some("high"));
    app.apply_option(crate::state::OptionsKind::ReasoningEffort, "ultracode");
    assert!(app.thread.ultracode);
    assert!(app.reasoning_effort.is_none());
    app.apply_option(crate::state::OptionsKind::ReasoningEffort, "off");
    assert!(!app.thread.ultracode);
    assert!(app.reasoning_effort.is_none());
}

#[tokio::test]
async fn reasoning_picker_exposes_ultracode_as_a_first_class_mode() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.execute_command("/reasoning", &offline_client())
        .await
        .unwrap();
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        panic!("/reasoning must open the options picker");
    };
    let ultracode = entries
        .iter()
        .find(|entry| entry.value == "ultracode")
        .expect("Ultracode option");
    assert!(ultracode.description.contains("strongest compatible"));
    assert!(!entries.iter().any(|entry| entry.value == "xhigh"));
    assert!(!entries.iter().any(|entry| entry.value == "max"));
}

#[tokio::test]
async fn reasoning_commands_use_each_models_advertised_domain() {
    let mut app = selector_fixture();
    app.providers[0].models[0].reasoning_efforts = vec!["none".into(), "xhigh".into()];
    app.execute_command("/reasoning xhigh", &offline_client())
        .await
        .unwrap();
    assert_eq!(app.reasoning_effort.as_deref(), Some("xhigh"));
    app.execute_command("/reasoning max", &offline_client())
        .await
        .unwrap();
    assert_eq!(app.reasoning_effort.as_deref(), Some("xhigh"));
    // The model's name as the picker shows it, and what it does offer,
    // instead of the raw id and the command just typed.
    assert_eq!(
        app.notice,
        "GPT-5.4 does not support max reasoning · it offers none or xhigh"
    );
    app.execute_command("/reasoning", &offline_client())
        .await
        .unwrap();
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        panic!()
    };
    assert_eq!(
        entries.iter().map(|e| e.value.as_str()).collect::<Vec<_>>(),
        ["off", "none", "xhigh", "ultracode", "display"]
    );
    app.thread.model = "gpt-5.4-mini".into();
    app.normalize_reasoning();
    assert!(app.reasoning_effort.is_none());
    app.providers[0].models[1].reasoning = false;
    app.execute_command("/reasoning high", &offline_client())
        .await
        .unwrap();
    assert!(app.reasoning_effort.is_none());
}

#[tokio::test]
async fn interactive_agent_selection_excludes_hidden_and_subagent_profiles() {
    let mut app = selector_fixture();
    let mut internal = app.agents[0].clone();
    internal.id = "compaction".into();
    internal.hidden = true;
    app.agents.push(internal.clone());
    internal.id = "worker".into();
    internal.hidden = false;
    internal.mode = Some("subagent".into());
    app.agents.push(internal.clone());
    internal.id = "both".into();
    internal.mode = Some("all".into());
    app.agents.push(internal);
    assert_eq!(
        app.filtered_agents()
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        ["build", "plan", "both"]
    );
    app.cycle_agent_reverse();
    assert_eq!(app.agent(), "both");
    app.cycle_agent_reverse();
    assert_eq!(app.agent(), "plan");
    app.cycle_agent();
    assert_eq!(app.agent(), "both");
    app.cycle_agent();
    assert_eq!(app.agent(), "build");
    app.set_agent("worker", &offline_client()).await.unwrap();
    assert_eq!(app.agent(), "build");
    assert!(app.notice.contains("Unknown agent"));
    app.agents.clear();
    app.cycle_agent();
    app.cycle_agent_reverse();
}

#[tokio::test]
async fn reasoning_picker_focus_and_marker_follow_the_durable_ultracode_mode() {
    let mut thread = fixture_thread();
    thread.ultracode = true;
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.reasoning_effort = None;

    app.execute_command("/reasoning", &offline_client())
        .await
        .unwrap();
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        panic!("/reasoning must open the options picker");
    };
    let ultracode_index = app
        .filtered_options(entries)
        .iter()
        .position(|entry| entry.value == "ultracode")
        .expect("Ultracode option");
    assert_eq!(
        app.selection, ultracode_index,
        "the durable mode owns initial focus"
    );

    let rendered = app
        .lines((90, 24))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>();
    let active = rendered
        .iter()
        .find(|line| line.contains("ultracode"))
        .expect("rendered Ultracode row");
    assert!(
        active.contains('●'),
        "Ultracode must carry the active marker: {active}"
    );
}

/// Identity guard: the default theme must stay byte-identical to the original
/// hardcoded editorial palette. The tmux e2e test asserts the same background
/// RGB (15,14,13) end to end.
#[test]
fn editorial_theme_matches_the_original_palette() {
    let theme = Theme::editorial_dark();
    assert_eq!(theme.bg, Color::rgb(15, 14, 13));
    assert_eq!(theme.surface, Color::rgb(27, 25, 23));
    assert_eq!(theme.overlay, Color::rgb(27, 25, 23));
    assert_eq!(theme.selected, Color::rgb(37, 57, 52));
    assert_eq!(theme.text, Color::rgb(231, 225, 213));
    assert_eq!(theme.muted, Color::rgb(126, 120, 111));
    assert_eq!(theme.primary, Color::rgb(62, 184, 166));
    assert_eq!(theme.accent, Color::rgb(211, 133, 82));
    assert_eq!(theme.user, Color::rgb(166, 205, 194));
    assert_eq!(theme.danger, Color::rgb(224, 104, 99));
    assert_eq!(haarness_opentui::DEFAULT_BACKGROUND, theme.bg);
}

// ---------------------------------------------------------------------------
// P0: child-agent liveness, cross-thread permission routing, steering UI.
// ---------------------------------------------------------------------------

fn fixture_event(
    event_type: &str,
    thread_id: ThreadId,
    root_thread_id: ThreadId,
    payload: serde_json::Value,
) -> haarness_protocol::EventEnvelope {
    haarness_protocol::EventEnvelope {
        schema_version: haarness_protocol::SCHEMA_VERSION.into(),
        server_instance_id: uuid::Uuid::now_v7(),
        event_id: 1,
        event_type: event_type.into(),
        thread_id: Some(thread_id),
        root_thread_id: Some(root_thread_id),
        turn_id: None,
        item_id: None,
        payload,
        created_at: Utc::now(),
    }
}

fn fixture_running_turn(thread_id: ThreadId) -> haarness_protocol::Turn {
    haarness_protocol::Turn {
        id: TurnId::new(),
        thread_id,
        request_id: haarness_protocol::RequestId::new(),
        status: haarness_protocol::TurnStatus::Running,
        agent: "build".into(),
        provider: "openai".into(),
        model: "gpt-5.4".into(),
        ultracode: false,
        error: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[tokio::test]
async fn thread_update_synchronizes_a_server_side_agent_handoff() {
    let mut app = selector_fixture();
    assert_eq!(app.agent(), "build");
    let mut updated = app.thread.clone();
    updated.agent = "plan".into();

    app.reduce(
        fixture_event(
            "thread.updated",
            updated.id,
            updated.id,
            serde_json::to_value(&updated).unwrap(),
        ),
        &offline_client(),
    )
    .await
    .unwrap();

    assert_eq!(app.thread.agent, "plan");
    assert_eq!(app.agent(), "plan");
}

#[tokio::test]
async fn generated_title_updates_the_live_header_and_thread_list_without_finishing_the_turn() {
    let mut app = selector_fixture();
    app.sidebar_open = true;
    let turn = TurnId::new();
    app.active_turn = Some(turn);
    let mut updated = app.thread.clone();
    updated.title = "Security review".into();
    app.reduce(
        fixture_event(
            "thread.updated",
            updated.id,
            updated.id,
            serde_json::to_value(&updated).unwrap(),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.thread.title, "Security review");
    assert_eq!(
        app.threads
            .iter()
            .find(|thread| thread.id == updated.id)
            .unwrap()
            .title,
        "Security review"
    );
    assert_eq!(app.active_turn, Some(turn));
    assert!(
        app.lines((140, 30))
            .iter()
            .map(line_text)
            .any(|line| line.contains("Security review"))
    );
}

fn offline_client() -> crate::api::ApiClient {
    crate::api::ApiClient::new("http://127.0.0.1:1", "test-token").unwrap()
}

fn started_workspace_mutation(action: crate::input::AppAction) -> crate::input::WorkspaceMutation {
    let crate::input::AppAction::StartWorkspaceMutation(operation) = action else {
        panic!("workspace action did not schedule a background mutation");
    };
    operation
}

fn started_workspace_ref_operation(
    action: crate::input::AppAction,
) -> crate::input::WorkspaceRefOperation {
    let crate::input::AppAction::StartWorkspaceRefOperation(operation) = action else {
        panic!("workspace action did not schedule a background ref operation");
    };
    operation
}

fn finish_fixture_workspace_ref_load(
    app: &mut App,
    operation: crate::input::WorkspaceRefOperation,
) {
    assert!(matches!(
        operation,
        crate::input::WorkspaceRefOperation::Load
    ));
    let action =
        app.finish_workspace_ref_operation(crate::commands::WorkspaceRefOperationCompletion {
            operation,
            result: Ok(crate::commands::WorkspaceRefOperationResult::Loaded(
                fixture_workspace_refs(),
            )),
        });
    assert!(matches!(action, crate::input::AppAction::Continue));
}

async fn read_workspace_test_request(socket: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt as _;

    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    let mut expected_length = None;
    loop {
        let read = socket.read(&mut chunk).await.unwrap();
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if expected_length.is_none()
            && let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
        {
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            expected_length = Some(header_end + 4 + content_length);
        }
        if expected_length.is_some_and(|length| request.len() >= length) {
            break;
        }
    }
    String::from_utf8(request).unwrap()
}

async fn write_workspace_test_response(socket: &mut tokio::net::TcpStream, body: &str) {
    use tokio::io::AsyncWriteExt as _;

    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    socket.write_all(response.as_bytes()).await.unwrap();
}

async fn workspace_http_status_client(
    responses: Vec<(u16, String)>,
    local_switching: bool,
) -> (crate::api::ApiClient, tokio::task::JoinHandle<Vec<String>>) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            let mut expected_length = None;
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if expected_length.is_none()
                    && let Some(header_end) =
                        request.windows(4).position(|part| part == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    expected_length = Some(header_end + 4 + content_length);
                }
                if expected_length.is_some_and(|length| request.len() >= length) {
                    break;
                }
            }
            let reason = if status >= 400 {
                "Request Failed"
            } else {
                "OK"
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            requests.push(String::from_utf8(request).unwrap());
        }
        requests
    });
    let mut client =
        crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    if local_switching {
        client = client.with_endpoint_resolver(std::sync::Arc::new(|| None));
    }
    (client, server)
}

async fn workspace_http_client(
    responses: Vec<String>,
    local_switching: bool,
) -> (crate::api::ApiClient, tokio::task::JoinHandle<Vec<String>>) {
    workspace_http_status_client(
        responses
            .into_iter()
            .map(|response| (200, response))
            .collect(),
        local_switching,
    )
    .await
}

#[tokio::test]
async fn bare_workspace_alias_opens_the_real_searchable_dashboard() {
    let catalog = fixture_workspace_catalog();
    let (client, server) =
        workspace_http_client(vec![serde_json::to_string(&catalog).unwrap()], false).await;
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);

    let action = app.execute_command("/workspaces", &client).await.unwrap();
    assert!(matches!(action, crate::input::AppAction::Continue));
    let crate::state::Overlay::Options {
        kind,
        entries,
        title,
    } = &app.overlay
    else {
        panic!("bare workspace alias did not open a selector modal");
    };
    assert!(matches!(kind, crate::state::OptionsKind::Workspaces));
    assert_eq!(title, "Projects and worktrees");
    assert_eq!(entries[0].value, "create");
    assert_eq!(app.selection, 1, "the current workspace should be focused");
    let request = server.await.unwrap().pop().unwrap();
    assert!(request.starts_with("GET /v1/workspaces "), "{request}");
}

#[tokio::test]
async fn workspace_dashboard_renders_states_and_mouse_opens_contextual_actions() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.workspace_local_switching = true;
    app.open_workspace_dashboard(fixture_workspace_catalog());

    let rendered = app
        .lines((110, 34))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    for expected in [
        "Create worktree",
        "local checkout",
        "managed worktree",
        "current",
        "missing",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected}: {rendered}"
        );
    }

    // The managed workspace is the third modal row (create, current, review).
    let hit = app
        .hit_rows
        .borrow()
        .iter()
        .find(|hit| matches!(hit.target, crate::view::frame::HitTarget::MenuRow(2)))
        .cloned()
        .expect("managed workspace row is clickable");
    let column = u16::try_from(hit.start + 2).unwrap();
    let row = u16::try_from(hit.row).unwrap();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
            &offline_client(),
        )
        .await
        .unwrap();
    }
    let crate::state::Overlay::Options { kind, entries, .. } = &app.overlay else {
        panic!("workspace row did not open its action modal");
    };
    assert!(matches!(kind, crate::state::OptionsKind::WorkspaceActions));
    assert_eq!(app.selection, 1, "open is the primary safe action");
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.value.as_str())
            .collect::<Vec<_>>(),
        ["info", "open", "reset", "remove", "back"]
    );

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::Workspaces,
            ..
        }
    ));

    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::Workspaces,
            "workspace:workspace-local".into(),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        unreachable!();
    };
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.value.as_str())
            .collect::<Vec<_>>(),
        ["info", "back"],
        "the current unmanaged checkout must not expose switch/reset/remove"
    );
    assert!(entries[0].description.contains("local checkout"));
    assert!(entries[0].description.contains("current"));
    app.reopen_workspace_dashboard();

    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::Workspaces,
            "workspace:workspace-missing".into(),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        unreachable!();
    };
    let values = entries
        .iter()
        .map(|entry| entry.value.as_str())
        .collect::<Vec<_>>();
    assert!(values.contains(&"remove"));
    assert!(!values.contains(&"open"));
    assert!(!values.contains(&"reset"));
}

#[tokio::test]
async fn missing_workspace_cleanup_never_offers_or_requests_branch_deletion() {
    let catalog = fixture_workspace_catalog();
    let missing = catalog.workspaces[2].clone();
    let error = serde_json::json!({
        "code": "workspace_conflict",
        "message": "stale registration is busy",
        "retryable": false,
    })
    .to_string();
    let (client, server) = workspace_http_status_client(vec![(409, error)], false).await;
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.open_workspace_dashboard(catalog);
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::Workspaces,
            format!("workspace:{}", missing.id),
        ),
        &client,
    )
    .await
    .unwrap();
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkspaceActions,
            "remove".into(),
        ),
        &client,
    )
    .await
    .unwrap();

    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        panic!("missing workspace confirmation was not opened");
    };
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.value.as_str())
            .collect::<Vec<_>>(),
        ["cancel", "remove-stale"]
    );
    assert!(entries[1].description.contains("keep every Git branch"));
    assert_eq!(app.selection, 0);

    // Even a synthetic/stale choice cannot bypass the hidden-action gate.
    let blocked = app
        .execute_choice(
            crate::state::OverlayChoice::Option(
                crate::state::OptionsKind::WorkspaceRemoveConfirmation,
                "remove-branch".into(),
            ),
            &client,
        )
        .await
        .unwrap();
    assert!(matches!(blocked, crate::input::AppAction::Continue));
    assert!(app.workspace_mutation.is_none());
    assert!(app.notice.contains("always keeps its Git branch"));

    let action = app
        .execute_choice(
            crate::state::OverlayChoice::Option(
                crate::state::OptionsKind::WorkspaceRemoveConfirmation,
                "remove-stale".into(),
            ),
            &client,
        )
        .await
        .unwrap();
    let operation = started_workspace_mutation(action);
    let crate::input::WorkspaceMutation::Cleanup {
        request, selection, ..
    } = &operation
    else {
        panic!("stale cleanup did not schedule the cleanup endpoint");
    };
    assert!(request.confirm);
    assert_eq!(*selection, 1);

    let completion = crate::commands::run_workspace_mutation(client.clone(), operation).await;
    assert!(app.finish_workspace_mutation(completion, false).is_none());
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        panic!("failed stale cleanup did not restore confirmation");
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(app.selection, 1);
    assert!(app.notice.contains("stale registration is busy"));

    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].starts_with("POST /v1/workspaces/workspace-missing/cleanup "),
        "{}",
        requests[0]
    );
    let body = requests[0].split_once("\r\n\r\n").unwrap().1;
    assert_eq!(body, r#"{"confirm":true}"#);
}

#[tokio::test]
async fn non_git_workspace_dashboard_never_submits_create() {
    let mut catalog = fixture_workspace_catalog();
    catalog.projects[0].git = false;
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.workspace_local_switching = true;
    app.open_workspace_dashboard(catalog);
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        unreachable!();
    };
    assert_eq!(entries[0].value, "create-unavailable");
    assert!(entries[0].description.contains("not a Git repository"));

    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::Workspaces,
            "create-unavailable".into(),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.notice, "Worktree creation requires a Git project");
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::Workspaces,
            ..
        }
    ));
}

#[tokio::test]
async fn workspace_ref_picker_loads_categories_and_mouse_custom_resolves_canonical_ref() {
    let refs = fixture_workspace_refs();
    let resolved = haarness_protocol::WorkspaceResolveRefResponse {
        reference: "refs/tags/release-candidate".into(),
        commit: "6666666666666666666666666666666666666666".into(),
    };
    let (client, server) = workspace_http_client(
        vec![
            serde_json::to_string(&refs).unwrap(),
            serde_json::to_string(&resolved).unwrap(),
        ],
        false,
    )
    .await;
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.open_workspace_dashboard(fixture_workspace_catalog());
    app.execute_choice(
        crate::state::OverlayChoice::Option(crate::state::OptionsKind::Workspaces, "create".into()),
        &client,
    )
    .await
    .unwrap();
    app.overlay_filter = "release workspace".into();

    let started = std::time::Instant::now();
    let load = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_millis(100),
        "name submission blocked on ref discovery"
    );
    let load = started_workspace_ref_operation(load);
    assert!(matches!(load, crate::input::WorkspaceRefOperation::Load));
    assert!(matches!(app.overlay, crate::state::Overlay::Panel { .. }));
    let completion = crate::commands::run_workspace_ref_operation(client.clone(), load).await;
    assert!(matches!(
        app.finish_workspace_ref_operation(completion),
        crate::input::AppAction::Continue
    ));

    let crate::state::Overlay::Options { kind, entries, .. } = &app.overlay else {
        panic!("ref discovery did not open the base picker");
    };
    assert!(matches!(
        kind,
        crate::state::OptionsKind::WorkspaceCreateBase
    ));
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.label.as_str())
            .collect::<Vec<_>>(),
        [
            "Default · main",
            "Local · feature/local",
            "Remote · origin/main",
            "Tag · v1.0.0",
            "Custom · Type a branch, tag, or commit…",
        ]
    );

    // Render once to register real row hit targets, then click Custom through
    // the same mouse path users exercise in the terminal.
    let _ = app.lines((110, 34));
    let custom_index = entries.len() - 1;
    let hit = app
        .hit_rows
        .borrow()
        .iter()
        .find(|hit| {
            matches!(
                hit.target,
                crate::view::frame::HitTarget::MenuRow(index) if index == custom_index
            )
        })
        .cloned()
        .expect("Custom ref row is clickable");
    let column = u16::try_from(hit.start + 2).unwrap();
    let row = u16::try_from(hit.row).unwrap();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
    }
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceCreateCustomBase,
            ..
        }
    ));

    app.overlay_filter = "release-candidate".into();
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(
        app.selected_overlay_choice(),
        Some(crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkspaceCreateBase,
            ref value,
        )) if value == "custom"
    ));
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkspaceCreateBase,
            "custom".into(),
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.overlay_filter, "release-candidate");

    let validation = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    let completion = crate::commands::run_workspace_ref_operation(
        client.clone(),
        started_workspace_ref_operation(validation),
    )
    .await;
    let action = app.finish_workspace_ref_operation(completion);
    let crate::input::WorkspaceMutation::Create { request, retry } =
        started_workspace_mutation(action)
    else {
        panic!("resolved custom ref did not schedule create");
    };
    assert_eq!(
        request.from_ref.as_deref(),
        Some("refs/tags/release-candidate")
    );
    assert!(matches!(
        retry,
        crate::input::WorkspaceCreateRetry::Custom { ref input }
            if input == "release-candidate"
    ));

    let requests = server.await.unwrap();
    assert!(requests[0].starts_with("GET /v1/workspaces/refs "));
    assert!(requests[1].starts_with("POST /v1/workspaces/resolve-ref "));
    assert!(
        requests[1].contains(r#""reference":"release-candidate""#),
        "{}",
        requests[1]
    );
}

#[test]
fn workspace_ref_picker_labels_a_detached_default_as_current_commit() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let mut refs = fixture_workspace_refs();
    refs.default_ref = refs.default_commit.clone();
    for reference in &mut refs.refs {
        reference.current = false;
    }
    app.workspace_create_name = Some("detached-base".into());
    app.workspace_refs = Some(refs);
    app.reopen_workspace_create_base_picker(None);

    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        panic!("detached ref picker was not opened");
    };
    assert!(entries[0].label.starts_with("Default · 111111"));
    assert!(
        entries[0]
            .description
            .starts_with("Current commit · 111111"),
        "{}",
        entries[0].description
    );
}

#[tokio::test]
async fn workspace_ref_failures_keep_default_and_custom_and_restore_exact_typed_ref() {
    let errors = vec![
        (
            500,
            serde_json::json!({
                "code": "workspace_refs_failed",
                "message": "could not enumerate refs",
                "retryable": true,
            })
            .to_string(),
        ),
        (
            422,
            serde_json::json!({
                "code": "workspace_ref_invalid",
                "message": "unknown revision",
                "retryable": false,
            })
            .to_string(),
        ),
    ];
    let (client, server) = workspace_http_status_client(errors, false).await;
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.open_workspace_dashboard(fixture_workspace_catalog());
    app.execute_choice(
        crate::state::OverlayChoice::Option(crate::state::OptionsKind::Workspaces, "create".into()),
        &client,
    )
    .await
    .unwrap();
    app.overlay_filter = "failure-retry".into();
    let load = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    let completion = crate::commands::run_workspace_ref_operation(
        client.clone(),
        started_workspace_ref_operation(load),
    )
    .await;
    assert!(matches!(
        app.finish_workspace_ref_operation(completion),
        crate::input::AppAction::Continue
    ));
    let crate::state::Overlay::Options { kind, entries, .. } = &app.overlay else {
        panic!("failed ref list did not restore picker");
    };
    assert!(matches!(
        kind,
        crate::state::OptionsKind::WorkspaceCreateBase
    ));
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.value.as_str())
            .collect::<Vec<_>>(),
        ["default", "custom"]
    );
    assert_eq!(app.selection, 0);
    assert!(matches!(
        app.selected_overlay_choice(),
        Some(crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkspaceCreateBase,
            ref value,
        )) if value == "default"
    ));
    assert!(app.notice.contains("Default and Custom"), "{}", app.notice);
    assert!(app.notification_toast.is_some());

    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkspaceCreateBase,
            "custom".into(),
        ),
        &client,
    )
    .await
    .unwrap();
    app.overlay_filter = "missing/ref".into();
    let validation = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    let completion = crate::commands::run_workspace_ref_operation(
        client.clone(),
        started_workspace_ref_operation(validation),
    )
    .await;
    assert!(matches!(
        app.finish_workspace_ref_operation(completion),
        crate::input::AppAction::Continue
    ));
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceCreateCustomBase,
            ..
        }
    ));
    assert_eq!(app.workspace_create_name.as_deref(), Some("failure-retry"));
    assert_eq!(app.overlay_filter, "missing/ref");
    assert!(app.notice.contains("missing/ref"), "{}", app.notice);
    assert!(
        app.workspace_mutation.is_none(),
        "validation must precede create"
    );

    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2, "resolve failure must not POST create");
    assert!(requests[0].starts_with("GET /v1/workspaces/refs "));
    assert!(requests[1].starts_with("POST /v1/workspaces/resolve-ref "));
    assert!(
        requests[1].contains(r#""reference":"missing/ref""#),
        "{}",
        requests[1]
    );
}

#[tokio::test]
async fn workspace_command_initializes_local_switching_capability_from_client() {
    let catalog = fixture_workspace_catalog();
    let workspace = catalog.workspaces[1].clone();
    let (client, server) =
        workspace_http_client(vec![serde_json::to_string(&catalog).unwrap()], true).await;
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    assert!(!app.workspace_local_switching);

    app.execute_command("/workspace", &client).await.unwrap();
    assert!(app.workspace_local_switching);
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::Workspaces,
            format!("workspace:{}", workspace.id),
        ),
        &client,
    )
    .await
    .unwrap();
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        panic!("workspace action picker was not opened");
    };
    assert_eq!(app.selection, 1);
    assert!(entries.iter().any(|entry| entry.value == "open"));
    assert!(server.await.unwrap()[0].starts_with("GET /v1/workspaces "));
}

#[tokio::test]
async fn workspace_create_modal_validates_prompts_posts_and_switches_locally() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let catalog = fixture_workspace_catalog();
    let mut created = catalog.workspaces[1].clone();
    created.id = "workspace-new".into();
    created.name = "review-two".into();
    created.path = "/repo/worktrees/review-two".into();
    created.branch = Some("haarness/review-two".into());
    let operation = haarness_protocol::WorkspaceOperationResponse {
        workspace: created.clone(),
        message: format!("created worktree {}", created.path),
    };
    let (client, server) =
        workspace_http_client(vec![serde_json::to_string(&operation).unwrap()], true).await;
    app.open_workspace_dashboard(catalog);
    app.execute_choice(
        crate::state::OverlayChoice::Option(crate::state::OptionsKind::Workspaces, "create".into()),
        &client,
    )
    .await
    .unwrap();

    let empty = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(empty, crate::input::AppAction::Continue));
    assert_eq!(app.notice, "Worktree name is required");
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceCreateName,
            ..
        }
    ));
    for character in "review-two".chars() {
        crate::input::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &client,
        )
        .await
        .unwrap();
    }
    let load = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    let load = started_workspace_ref_operation(load);
    assert!(app.workspace_ref_operation.is_some());
    assert!(matches!(app.overlay, crate::state::Overlay::Panel { .. }));
    finish_fixture_workspace_ref_load(&mut app, load);
    let base_prompt = app
        .lines((100, 28))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    for expected in [
        "Default · main",
        "Local · feature/local",
        "Remote · origin/main",
        "Tag · v1.0.0",
        "Custom · Type a branch",
    ] {
        assert!(
            base_prompt.contains(expected),
            "missing {expected}: {base_prompt}"
        );
    }

    // Escape returns to the prior field without losing what was typed.
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.overlay_filter, "review-two");
    let reload = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    finish_fixture_workspace_ref_load(&mut app, started_workspace_ref_operation(reload));
    for character in "origin/main".chars() {
        crate::input::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &client,
        )
        .await
        .unwrap();
    }
    let action = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    let operation = started_workspace_mutation(action);
    assert!(app.workspace_mutation.is_some());
    assert!(matches!(app.overlay, crate::state::Overlay::Panel { .. }));
    let completion = crate::commands::run_workspace_mutation(client.clone(), operation).await;
    let switched = app
        .finish_workspace_mutation(completion, true)
        .expect("local create switches workspace");
    assert_eq!(switched, std::path::Path::new(&created.path));
    let request = server.await.unwrap().pop().unwrap();
    assert!(request.starts_with("POST /v1/workspaces "), "{request}");
    assert!(request.contains(r#""name":"review-two""#), "{request}");
    assert!(
        request.contains(r#""fromRef":"refs/remotes/origin/main""#),
        "{request}"
    );
}

#[tokio::test]
async fn attached_client_create_refreshes_without_exposing_a_server_local_path() {
    let mut catalog = fixture_workspace_catalog();
    let mut created = catalog.workspaces[1].clone();
    created.id = "workspace-remote".into();
    created.name = "remote-review".into();
    created.path = "/server/private/worktrees/remote-review".into();
    let operation = haarness_protocol::WorkspaceOperationResponse {
        workspace: created.clone(),
        message: format!("created worktree {}", created.path),
    };
    catalog.workspaces.push(created.clone());
    let (client, server) = workspace_http_client(
        vec![
            serde_json::to_string(&operation).unwrap(),
            serde_json::to_string(&catalog).unwrap(),
        ],
        false,
    )
    .await;
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.open_workspace_dashboard(fixture_workspace_catalog());
    app.execute_choice(
        crate::state::OverlayChoice::Option(crate::state::OptionsKind::Workspaces, "create".into()),
        &client,
    )
    .await
    .unwrap();
    app.overlay_filter = "remote-review".into();
    let load = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    finish_fixture_workspace_ref_load(&mut app, started_workspace_ref_operation(load));
    let action = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();

    let progress = app
        .lines((100, 28))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(progress.contains("from main"), "{progress}");
    let operation = started_workspace_mutation(action);
    let completion = crate::commands::run_workspace_mutation(client.clone(), operation).await;
    assert!(app.finish_workspace_mutation(completion, false).is_none());
    assert!(
        app.notice
            .contains("switching is unavailable on attached clients")
    );
    assert!(!app.notice.contains(&created.path), "{}", app.notice);
    assert!(!app.notice.contains("relaunch"), "{}", app.notice);
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::Workspaces,
            ..
        }
    ));
    let requests = server.await.unwrap();
    assert!(requests[0].starts_with("POST /v1/workspaces "));
    assert!(!requests[0].contains("\"fromRef\""), "{}", requests[0]);
    assert!(requests[1].starts_with("GET /v1/workspaces "));
}

#[tokio::test]
async fn delayed_workspace_mutation_keeps_input_and_rendering_live_until_completion() {
    let mut catalog = fixture_workspace_catalog();
    let mut created = catalog.workspaces[1].clone();
    created.id = "workspace-delayed".into();
    created.name = "delayed".into();
    created.path = "/server/worktrees/delayed".into();
    let operation = haarness_protocol::WorkspaceOperationResponse {
        workspace: created.clone(),
        message: format!("created worktree {}", created.path),
    };
    catalog.workspaces.push(created);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let operation_body = serde_json::to_string(&operation).unwrap();
    let catalog_body = serde_json::to_string(&catalog).unwrap();
    let server = tokio::spawn(async move {
        let (mut create_socket, _) = listener.accept().await.unwrap();
        let create_request = read_workspace_test_request(&mut create_socket).await;
        seen_tx.send(()).unwrap();
        release_rx.await.unwrap();
        write_workspace_test_response(&mut create_socket, &operation_body).await;
        let (mut list_socket, _) = listener.accept().await.unwrap();
        let list_request = read_workspace_test_request(&mut list_socket).await;
        write_workspace_test_response(&mut list_socket, &catalog_body).await;
        vec![create_request, list_request]
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.open_workspace_dashboard(fixture_workspace_catalog());
    app.execute_choice(
        crate::state::OverlayChoice::Option(crate::state::OptionsKind::Workspaces, "create".into()),
        &client,
    )
    .await
    .unwrap();
    app.overlay_filter = "delayed".into();
    let load = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    finish_fixture_workspace_ref_load(&mut app, started_workspace_ref_operation(load));

    let started = std::time::Instant::now();
    let action = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_millis(100),
        "input waited for a Git-backed HTTP request"
    );
    let operation = started_workspace_mutation(action);
    let worker = tokio::spawn(crate::commands::run_workspace_mutation(
        client.clone(),
        operation,
    ));
    seen_rx.await.unwrap();
    assert!(
        !worker.is_finished(),
        "server gate should keep mutation pending"
    );

    let rendered = app
        .lines((100, 28))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Creating worktree"), "{rendered}");
    let key_started = std::time::Instant::now();
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(key_started.elapsed() < std::time::Duration::from_millis(100));
    assert!(app.workspace_mutation.is_some());
    assert!(matches!(app.overlay, crate::state::Overlay::Panel { .. }));
    assert!(app.notice.contains("operation is still running"));
    assert!(app.notice.contains("disabled"));

    release_tx.send(()).unwrap();
    let completion = worker.await.unwrap();
    assert!(app.finish_workspace_mutation(completion, false).is_none());
    assert!(app.workspace_mutation.is_none());
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::Workspaces,
            ..
        }
    ));
    let requests = server.await.unwrap();
    assert!(requests[0].starts_with("POST /v1/workspaces "));
    assert!(requests[1].starts_with("GET /v1/workspaces "));
}

#[tokio::test]
async fn workspace_open_action_posts_and_switches_the_local_tui() {
    let catalog = fixture_workspace_catalog();
    let workspace = catalog.workspaces[1].clone();
    let operation = haarness_protocol::WorkspaceOperationResponse {
        workspace: workspace.clone(),
        message: "opened workspace".into(),
    };
    let (client, server) =
        workspace_http_client(vec![serde_json::to_string(&operation).unwrap()], true).await;
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.workspace_local_switching = true;
    app.open_workspace_dashboard(catalog);
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::Workspaces,
            format!("workspace:{}", workspace.id),
        ),
        &client,
    )
    .await
    .unwrap();

    let action = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    let operation = started_workspace_mutation(action);
    let completion = crate::commands::run_workspace_mutation(client.clone(), operation).await;
    let switched = app
        .finish_workspace_mutation(completion, true)
        .expect("local open switches workspace");
    assert_eq!(switched, std::path::Path::new(&workspace.path));
    let request = server.await.unwrap().pop().unwrap();
    assert!(
        request.starts_with("POST /v1/workspaces/workspace-review/open "),
        "{request}"
    );
}

#[tokio::test]
async fn attached_client_is_not_offered_an_unusable_workspace_open_action() {
    let catalog = fixture_workspace_catalog();
    let workspace = catalog.workspaces[1].clone();
    let client = offline_client();
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    assert!(!app.workspace_local_switching);
    app.open_workspace_dashboard(catalog);
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::Workspaces,
            format!("workspace:{}", workspace.id),
        ),
        &client,
    )
    .await
    .unwrap();
    let crate::state::Overlay::Options { kind, entries, .. } = &app.overlay else {
        panic!("workspace row did not open its action modal");
    };
    assert!(matches!(kind, crate::state::OptionsKind::WorkspaceActions));
    assert_eq!(app.selection, 0, "attached clients stay on the info row");
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.value.as_str())
            .collect::<Vec<_>>(),
        ["info", "reset", "remove", "back"]
    );
    assert!(entries.iter().all(|entry| entry.value != "open"));

    let action = app
        .execute_command("/workspace open workspace-review", &client)
        .await
        .unwrap();
    assert!(matches!(action, crate::input::AppAction::Continue));
    assert!(
        app.notice
            .contains("switching is unavailable on attached clients")
    );
    assert!(!app.notice.contains(&workspace.path), "{}", app.notice);
    assert!(app.workspace_mutation.is_none());
}

#[tokio::test]
async fn workspace_4xx_errors_restore_exact_modal_context_and_surface_feedback() {
    let errors = [
        (409, "workspace is busy"),
        (422, "base ref is invalid"),
        (409, "worktree has changes"),
        (409, "worktree server is active"),
    ]
    .into_iter()
    .map(|(status, message)| {
        (
            status,
            serde_json::json!({
                "code": "workspace_conflict",
                "message": message,
                "retryable": false,
            })
            .to_string(),
        )
    })
    .collect();
    let (client, server) = workspace_http_status_client(errors, false).await;
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.workspace_local_switching = true;
    app.open_workspace_dashboard(fixture_workspace_catalog());

    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::Workspaces,
            "workspace:workspace-review".into(),
        ),
        &client,
    )
    .await
    .unwrap();
    let action = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    let operation = started_workspace_mutation(action);
    let completion = crate::commands::run_workspace_mutation(client.clone(), operation).await;
    assert!(app.finish_workspace_mutation(completion, false).is_none());
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceActions,
            ..
        }
    ));
    assert_eq!(app.selection, 1, "failed open should stay focused on Open");
    assert!(app.notice.contains("workspace is busy"), "{}", app.notice);

    app.reopen_workspace_dashboard();
    app.execute_choice(
        crate::state::OverlayChoice::Option(crate::state::OptionsKind::Workspaces, "create".into()),
        &client,
    )
    .await
    .unwrap();
    app.overlay_filter = "retry-name".into();
    let load = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    finish_fixture_workspace_ref_load(&mut app, started_workspace_ref_operation(load));
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkspaceCreateBase,
            "custom".into(),
        ),
        &client,
    )
    .await
    .unwrap();
    app.overlay_filter = "bad/ref".into();
    let validation = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    let validation = started_workspace_ref_operation(validation);
    let action =
        app.finish_workspace_ref_operation(crate::commands::WorkspaceRefOperationCompletion {
            operation: validation,
            result: Ok(crate::commands::WorkspaceRefOperationResult::Resolved(
                haarness_protocol::WorkspaceResolveRefResponse {
                    reference: "refs/heads/bad/ref".into(),
                    commit: "5555555555555555555555555555555555555555".into(),
                },
            )),
        });
    let operation = started_workspace_mutation(action);
    let completion = crate::commands::run_workspace_mutation(client.clone(), operation).await;
    assert!(app.finish_workspace_mutation(completion, false).is_none());
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceCreateCustomBase,
            ..
        }
    ));
    assert_eq!(app.workspace_create_name.as_deref(), Some("retry-name"));
    assert_eq!(app.overlay_filter, "bad/ref");
    assert!(app.notice.contains("base ref is invalid"), "{}", app.notice);

    app.reopen_workspace_dashboard();
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::Workspaces,
            "workspace:workspace-review".into(),
        ),
        &client,
    )
    .await
    .unwrap();
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkspaceActions,
            "reset".into(),
        ),
        &client,
    )
    .await
    .unwrap();
    let action = app
        .execute_choice(
            crate::state::OverlayChoice::Option(
                crate::state::OptionsKind::WorkspaceResetConfirmation,
                "reset-clean".into(),
            ),
            &client,
        )
        .await
        .unwrap();
    let operation = started_workspace_mutation(action);
    let completion = crate::commands::run_workspace_mutation(client.clone(), operation).await;
    assert!(app.finish_workspace_mutation(completion, false).is_none());
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceResetConfirmation,
            ..
        }
    ));
    assert_eq!(app.selection, 3);
    assert!(
        app.notice.contains("worktree has changes"),
        "{}",
        app.notice
    );

    app.reopen_workspace_actions();
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkspaceActions,
            "remove".into(),
        ),
        &client,
    )
    .await
    .unwrap();
    let action = app
        .execute_choice(
            crate::state::OverlayChoice::Option(
                crate::state::OptionsKind::WorkspaceRemoveConfirmation,
                "remove-force-branch".into(),
            ),
            &client,
        )
        .await
        .unwrap();
    let operation = started_workspace_mutation(action);
    let completion = crate::commands::run_workspace_mutation(client.clone(), operation).await;
    assert!(app.finish_workspace_mutation(completion, false).is_none());
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceRemoveConfirmation,
            ..
        }
    ));
    assert_eq!(app.selection, 4);
    assert!(app.notice.contains("server is active"), "{}", app.notice);
    assert!(app.notification_toast.is_some());

    let requests = server.await.unwrap();
    assert!(requests[0].starts_with("POST /v1/workspaces/workspace-review/open "));
    assert!(requests[1].starts_with("POST /v1/workspaces "));
    assert!(requests[2].starts_with("POST /v1/workspaces/workspace-review/reset "));
    assert!(requests[3].starts_with("DELETE /v1/workspaces/workspace-review "));
}

#[tokio::test]
async fn workspace_reset_and_remove_require_safe_confirmations_and_send_exact_flags() {
    let catalog = fixture_workspace_catalog();
    let workspace = catalog.workspaces[1].clone();
    let reset = haarness_protocol::WorkspaceOperationResponse {
        workspace: workspace.clone(),
        message: "reset worktree".into(),
    };
    let mut removed_workspace = workspace.clone();
    removed_workspace.exists = false;
    let removed = haarness_protocol::WorkspaceOperationResponse {
        workspace: removed_workspace,
        message: "removed worktree".into(),
    };
    let mut after_remove = catalog.clone();
    after_remove
        .workspaces
        .retain(|candidate| candidate.id != workspace.id);
    let (client, server) = workspace_http_client(
        vec![
            serde_json::to_string(&reset).unwrap(),
            serde_json::to_string(&catalog).unwrap(),
            serde_json::to_string(&removed).unwrap(),
            serde_json::to_string(&after_remove).unwrap(),
        ],
        false,
    )
    .await;
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.open_workspace_dashboard(catalog);
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::Workspaces,
            format!("workspace:{}", workspace.id),
        ),
        &client,
    )
    .await
    .unwrap();
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkspaceActions,
            "reset".into(),
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.selection, 0, "reset must default to Cancel");
    assert!(matches!(
        app.selected_overlay_choice(),
        Some(crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkspaceResetConfirmation,
            ref value
        )) if value == "cancel"
    ));
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::WorkspaceActions,
            ..
        }
    ));
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkspaceActions,
            "reset".into(),
        ),
        &client,
    )
    .await
    .unwrap();
    let action = app
        .execute_choice(
            crate::state::OverlayChoice::Option(
                crate::state::OptionsKind::WorkspaceResetConfirmation,
                "reset-clean".into(),
            ),
            &client,
        )
        .await
        .unwrap();
    let operation = started_workspace_mutation(action);
    let completion = crate::commands::run_workspace_mutation(client.clone(), operation).await;
    assert!(app.finish_workspace_mutation(completion, false).is_none());
    assert_eq!(
        app.notice,
        "Reset worktree “review” to origin/main (0123456789ab)"
    );

    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::Workspaces,
            format!("workspace:{}", workspace.id),
        ),
        &client,
    )
    .await
    .unwrap();
    app.execute_choice(
        crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkspaceActions,
            "remove".into(),
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.selection, 0, "remove must default to Cancel");
    let action = app
        .execute_choice(
            crate::state::OverlayChoice::Option(
                crate::state::OptionsKind::WorkspaceRemoveConfirmation,
                "remove-force-branch".into(),
            ),
            &client,
        )
        .await
        .unwrap();
    let operation = started_workspace_mutation(action);
    let completion = crate::commands::run_workspace_mutation(client.clone(), operation).await;
    assert!(app.finish_workspace_mutation(completion, false).is_none());
    assert_eq!(
        app.notice,
        "Removed worktree “review” · retained branch haarness/review"
    );

    let requests = server.await.unwrap();
    assert!(
        requests[0].starts_with("POST /v1/workspaces/workspace-review/reset "),
        "{}",
        requests[0]
    );
    assert!(requests[0].contains(r#""confirm":true"#), "{}", requests[0]);
    assert!(
        requests[0].contains(r#""discardChanges":true"#),
        "{}",
        requests[0]
    );
    assert!(
        requests[0].contains(r#""removeUntracked":true"#),
        "{}",
        requests[0]
    );
    assert!(requests[1].starts_with("GET /v1/workspaces "));
    assert!(
        requests[2].starts_with("DELETE /v1/workspaces/workspace-review "),
        "{}",
        requests[2]
    );
    assert!(requests[2].contains(r#""confirm":true"#), "{}", requests[2]);
    assert!(
        requests[2].contains(r#""discardChanges":true"#),
        "{}",
        requests[2]
    );
    assert!(
        requests[2].contains(r#""deleteBranch":true"#),
        "{}",
        requests[2]
    );
    assert!(requests[3].starts_with("GET /v1/workspaces "));
}

async fn question_reply_client() -> (crate::api::ApiClient, tokio::task::JoinHandle<String>) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        let mut expected_length = None;
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if expected_length.is_none()
                && let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
            {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                expected_length = Some(header_end + 4 + content_length);
            }
            if expected_length.is_some_and(|length| request.len() >= length) {
                break;
            }
        }
        socket
            .write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        String::from_utf8(request).unwrap()
    });
    (
        crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap(),
        server,
    )
}

async fn navigation_client() -> (crate::api::ApiClient, tokio::task::JoinHandle<Vec<String>>) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        // switch_thread hydrates six independent REST resources.
        for _ in 0..6 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            let request_line = request.lines().next().unwrap_or_default().to_owned();
            let body = if request_line.contains("/goal ") {
                r#"{"goal":null}"#
            } else if request_line.contains("/active ") {
                "null"
            } else {
                "[]"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            requests.push(request_line);
        }
        requests
    });
    (
        crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap(),
        server,
    )
}

#[tokio::test]
async fn task_updates_feed_live_child_rows() {
    let thread = fixture_thread();
    let child_thread = ThreadId::new();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::ToolCall {
            call_id: "call-1".into(),
            name: "task".into(),
            arguments: serde_json::json!({"prompt": "audit the API", "agent": "general"}),
            provider_metadata: serde_json::Value::Null,
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    let client = offline_client();
    app.reduce(
        fixture_event(
            "task.updated",
            thread.id,
            thread.id,
            serde_json::json!({
                "callId": "call-1",
                "childThreadId": child_thread,
                "childTurnId": haarness_protocol::TurnId::new(),
                "state": "running",
                "toolCalls": 7,
                "lastTool": "grep",
                "startedAt": Utc::now(),
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.children.len(), 1);
    let sidebar: Vec<String> = crate::view::sidebar::sidebar_lines(&app, 38, 40)
        .lines
        .iter()
        .map(line_text)
        .collect();
    let title_row = sidebar
        .iter()
        .position(|line| line.contains("audit the API"))
        .expect("task prompt is the child identity");
    assert!(
        !sidebar[title_row].contains("grep"),
        "the current tool must not replace the child identity: {sidebar:?}"
    );
    assert!(
        sidebar
            .get(title_row + 1)
            .is_some_and(|line| line.contains("general · grep · 7 tools")),
        "agent and current tool are secondary live activity: {sidebar:?}"
    );

    app.open_tasks_overlay();
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        panic!("child picker did not open");
    };
    assert_eq!(entries[0].label, "audit the API");
    assert!(
        entries[0].description.starts_with("running")
            && entries[0].description.contains("activity: grep")
            && entries[0].description.contains("running"),
        "the picker keeps activity secondary to identity: {:?}",
        entries[0].description
    );
}

#[tokio::test]
async fn task_updates_cannot_create_phantom_agent_rows() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let client = offline_client();
    for payload in [
        serde_json::json!({
            "callId": "not-a-task-call",
            "childThreadId": ThreadId::new(),
            "state": "running"
        }),
        serde_json::json!({
            "callId": "also-not-a-task-call",
            "childThreadId": "not-a-thread-id",
            "state": "running"
        }),
    ] {
        app.reduce(
            fixture_event("task.updated", thread.id, thread.id, payload),
            &client,
        )
        .await
        .unwrap();
    }
    assert!(
        app.children.is_empty(),
        "only durable task calls may create child-agent rows"
    );
}

#[tokio::test]
async fn durable_background_task_state_wins_over_the_launch_acknowledgement() {
    let thread = fixture_thread();
    let child_thread = ThreadId::new();
    let child_turn = haarness_protocol::TurnId::new();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "call-background-child".into(),
                name: "task".into(),
                arguments: serde_json::json!({"prompt": "inspect in background"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::TaskLink {
                call_id: "call-background-child".into(),
                child_thread_id: child_thread,
            },
        ),
    ];
    let mut app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    let client = offline_client();
    for item in [
        message_item(
            3,
            &thread,
            ItemData::TaskState {
                call_id: "call-background-child".into(),
                child_thread_id: child_thread,
                child_turn_id: child_turn,
                state: "background".into(),
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::ToolResult {
                call_id: "call-background-child".into(),
                name: "task".into(),
                output: "<task state=\"running\">detached</task>".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ] {
        app.reduce(
            fixture_event(
                "item.created",
                thread.id,
                thread.id,
                serde_json::json!(item),
            ),
            &client,
        )
        .await
        .unwrap();
    }
    // Child progress reports instantaneous activity as `running`. It must not
    // erase the parent's durable background execution mode.
    app.reduce(
        fixture_event(
            "task.updated",
            thread.id,
            thread.id,
            serde_json::json!({
                "callId": "call-background-child",
                "childThreadId": child_thread,
                "childTurnId": child_turn,
                "state": "running",
                "toolCalls": 9,
                "lastTool": "grep",
                "startedAt": Utc::now(),
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    let task = &app.children["call-background-child"];
    assert_eq!(task.state, "background");
    assert_eq!(task.child_turn_id, Some(child_turn));
    assert_eq!(task.tool_calls, 9);
    assert_eq!(task.last_tool.as_deref(), Some("grep"));
    assert!(task.elapsed_seconds().is_some());
    app.tick = 0;
    let rows: Vec<String> = app
        .lines((100, 30))
        .into_iter()
        .map(|line| line_text(&line))
        .collect();
    let index = rows
        .iter()
        .position(|line| line.contains("task inspect in background"))
        .expect("background task activity row");
    let background_row = rows[index].clone();
    assert!(
        background_row.contains(crate::view::span::spinner(0)) && !background_row.contains('○'),
        "active background work must use the working marker: {background_row}"
    );
    // The detail line under the row carries the lane and the live activity.
    assert!(
        rows[index + 1].contains("↳ running in background · grep"),
        "{}",
        rows[index + 1]
    );
    let background_sidebar = crate::view::sidebar::sidebar_lines(&app, 72, 40)
        .lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        background_sidebar.contains(crate::view::span::spinner(0))
            && background_sidebar.contains("running in background"),
        "sidebar must use the same active lifecycle: {background_sidebar}"
    );
    app.tick = 1;
    let next_frame = app
        .lines((100, 30))
        .into_iter()
        .map(|line| line_text(&line))
        .find(|line| line.contains("task inspect in background"))
        .expect("background task activity row after tick");
    assert!(
        next_frame.contains(crate::view::span::spinner(1))
            && !next_frame.contains(crate::view::span::spinner(0)),
        "active task marker must animate between render ticks: {next_frame}"
    );

    let continued = message_item(
        5,
        &thread,
        ItemData::TaskLink {
            call_id: "call-background-update".into(),
            child_thread_id: child_thread,
        },
    );
    app.reduce(
        fixture_event(
            "item.created",
            thread.id,
            thread.id,
            serde_json::json!(continued),
        ),
        &client,
    )
    .await
    .unwrap();

    let done = message_item(
        6,
        &thread,
        ItemData::TaskState {
            call_id: "call-background-child".into(),
            child_thread_id: child_thread,
            child_turn_id: child_turn,
            state: "done".into(),
        },
    );
    app.reduce(
        fixture_event(
            "item.created",
            thread.id,
            thread.id,
            serde_json::json!(done),
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.children["call-background-child"].state, "done");
    assert_eq!(
        app.children.len(),
        1,
        "a task_id continuation updates the durable child instead of adding an agent row"
    );
    assert_eq!(
        app.children["call-background-child"].elapsed_seconds(),
        None
    );
    let rows: Vec<String> = app
        .lines((100, 30))
        .into_iter()
        .map(|line| line_text(&line))
        .collect();
    let index = rows
        .iter()
        .position(|line| line.contains("task inspect in background"))
        .expect("completed task activity row");
    assert!(
        rows[index].contains('✓') && rows[index + 1].trim_start().starts_with('└'),
        "the transcript and sidebar share the completed lifecycle: {} / {}",
        rows[index],
        rows[index + 1]
    );
}

#[tokio::test]
async fn declared_background_mode_survives_live_progress_and_permission_round_trip() {
    let thread = fixture_thread();
    let child_thread = ThreadId::new();
    let child_turn = haarness_protocol::TurnId::new();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::ToolCall {
            call_id: "call-declared-background".into(),
            name: "task".into(),
            arguments: serde_json::json!({
                "prompt": "inspect independently",
                "background": true
            }),
            provider_metadata: serde_json::Value::Null,
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    let client = offline_client();

    for (state, expected) in [
        ("running", "background"),
        ("waiting_permission", "waiting_permission"),
        ("running", "background"),
    ] {
        app.reduce(
            fixture_event(
                "task.updated",
                thread.id,
                thread.id,
                serde_json::json!({
                    "callId": "call-declared-background",
                    "childThreadId": child_thread,
                    "childTurnId": child_turn,
                    "state": state,
                    "toolCalls": 1,
                    "lastTool": "read",
                    "startedAt": Utc::now(),
                }),
            ),
            &client,
        )
        .await
        .unwrap();
        assert_eq!(app.children["call-declared-background"].state, expected);
    }

    let rendered = crate::view::sidebar::sidebar_lines(&app, 52, 40)
        .lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("running in background"),
        "background mode must remain explicit after live progress: {rendered}"
    );

    app.reduce(
        fixture_event(
            "task.updated",
            thread.id,
            thread.id,
            serde_json::json!({
                "callId": "call-declared-background",
                "childThreadId": child_thread,
                "childTurnId": child_turn,
                "state": "waiting_permission",
                "toolCalls": 2,
                "lastTool": "bash",
                "startedAt": Utc::now(),
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    app.reduce(
        fixture_event(
            "permission.requested",
            child_thread,
            thread.id,
            serde_json::json!({
                "permissionId": "background-permission",
                "operation": "bash:sleep 8",
                "permission": "bash",
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    app.reduce(
        fixture_event(
            "permission.resolved",
            child_thread,
            thread.id,
            serde_json::json!({
                "permissionId": "background-permission",
                "allowed": true,
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(
        app.children["call-declared-background"].state, "background",
        "permission resolution must not leave a stale approval marker or erase background mode"
    );
}

#[tokio::test]
async fn foreground_child_permission_resolution_immediately_restores_running_state() {
    let thread = fixture_thread();
    let child_thread = ThreadId::new();
    let child_turn = TurnId::new();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::ToolCall {
            call_id: "call-foreground-permission".into(),
            name: "task".into(),
            arguments: serde_json::json!({"prompt": "inspect"}),
            provider_metadata: serde_json::Value::Null,
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    let client = offline_client();

    app.reduce(
        fixture_event(
            "task.updated",
            thread.id,
            thread.id,
            serde_json::json!({
                "callId": "call-foreground-permission",
                "childThreadId": child_thread,
                "childTurnId": child_turn,
                "state": "waiting_permission",
                "toolCalls": 1,
                "lastTool": "bash",
                "startedAt": Utc::now(),
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    app.reduce(
        fixture_event(
            "permission.requested",
            child_thread,
            thread.id,
            serde_json::json!({
                "permissionId": "foreground-permission",
                "operation": "bash:sleep 8",
                "permission": "bash",
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(
        app.children["call-foreground-permission"].state,
        "waiting_permission"
    );

    app.reduce(
        fixture_event(
            "permission.resolved",
            child_thread,
            thread.id,
            serde_json::json!({
                "permissionId": "foreground-permission",
                "allowed": true,
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(
        app.children["call-foreground-permission"].state, "running",
        "resolved prompt must stop rendering as approval before the next progress event"
    );
}

#[tokio::test]
async fn foreground_continuation_does_not_inherit_an_old_background_run() {
    let thread = fixture_thread();
    let child_thread = ThreadId::new();
    let old_turn = TurnId::new();
    let new_turn = TurnId::new();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "call-old-background".into(),
                name: "task".into(),
                arguments: serde_json::json!({"prompt": "old work", "background": true}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::TaskLink {
                call_id: "call-old-background".into(),
                child_thread_id: child_thread,
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::TaskState {
                call_id: "call-old-background".into(),
                child_thread_id: child_thread,
                child_turn_id: old_turn,
                state: "background".into(),
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::TaskState {
                call_id: "call-old-background".into(),
                child_thread_id: child_thread,
                child_turn_id: old_turn,
                state: "done".into(),
            },
        ),
        message_item(
            5,
            &thread,
            ItemData::ToolCall {
                call_id: "call-new-foreground".into(),
                name: "task".into(),
                arguments: serde_json::json!({
                    "prompt": "continue with a result needed immediately",
                    "task_id": child_thread
                }),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            6,
            &thread,
            ItemData::TaskLink {
                call_id: "call-new-foreground".into(),
                child_thread_id: child_thread,
            },
        ),
    ];
    let mut app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    let client = offline_client();

    app.reduce(
        fixture_event(
            "task.updated",
            thread.id,
            thread.id,
            serde_json::json!({
                "callId": "call-new-foreground",
                "childThreadId": child_thread,
                "childTurnId": new_turn,
                "state": "running",
                "startedAt": Utc::now(),
            }),
        ),
        &client,
    )
    .await
    .unwrap();

    assert_eq!(app.children["call-old-background"].state, "running");
}

#[test]
fn agent_roster_is_one_row_per_child_and_scoped_to_the_latest_turn() {
    let thread = fixture_thread();
    let child_thread = ThreadId::new();
    let first_turn = TurnId::new();
    let second_turn = TurnId::new();
    let third_turn = TurnId::new();
    let first_two_turns = vec![
        message_item_for_turn(
            1,
            &thread,
            first_turn,
            ItemData::ToolCall {
                call_id: "call-original".into(),
                name: "task".into(),
                arguments: serde_json::json!({
                    "prompt": "Perform the accessibility audit",
                    "agent": "review"
                }),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item_for_turn(
            2,
            &thread,
            first_turn,
            ItemData::TaskLink {
                call_id: "call-original".into(),
                child_thread_id: child_thread,
            },
        ),
        message_item_for_turn(
            3,
            &thread,
            first_turn,
            ItemData::ToolResult {
                call_id: "call-original".into(),
                name: "task".into(),
                output: "finished".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
        message_item_for_turn(
            4,
            &thread,
            second_turn,
            ItemData::ToolCall {
                call_id: "call-continuation".into(),
                name: "task".into(),
                arguments: serde_json::json!({
                    "prompt": "Please finish now",
                    "agent": "review",
                    "task_id": child_thread
                }),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item_for_turn(
            5,
            &thread,
            second_turn,
            ItemData::TaskLink {
                call_id: "call-continuation".into(),
                child_thread_id: child_thread,
            },
        ),
        message_item_for_turn(
            6,
            &thread,
            second_turn,
            ItemData::ToolResult {
                call_id: "call-continuation".into(),
                name: "task".into(),
                output: "finished".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ];

    let continued = App::new(
        thread.clone(),
        vec![thread.clone()],
        first_two_turns.clone(),
        vec![],
        vec![],
    );
    assert_eq!(
        continued.children.len(),
        1,
        "continuing one child must not create a second agent row"
    );
    assert_eq!(continued.visible_children().len(), 1);
    let continued_sidebar = crate::view::sidebar::sidebar_lines(&continued, 48, 40)
        .lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        continued_sidebar.contains("Perform the accessibility audit"),
        "the durable child keeps its original identity: {continued_sidebar}"
    );
    assert!(
        !continued_sidebar.contains("Please finish now"),
        "a continuation prompt is activity, not a new agent: {continued_sidebar}"
    );

    let mut later_items = first_two_turns;
    later_items.push(message_item_for_turn(
        7,
        &thread,
        third_turn,
        ItemData::UserMessage {
            text: "Give me the final roadmap".into(),
            attachments: Vec::new(),
            queued: false,
            synthetic: false,
        },
    ));
    later_items.push(message_item_for_turn(
        8,
        &thread,
        first_turn,
        ItemData::TaskState {
            call_id: "call-original".into(),
            child_thread_id: child_thread,
            child_turn_id: TurnId::new(),
            state: "done".into(),
        },
    ));
    let mut later = App::new(thread.clone(), vec![thread], later_items, vec![], vec![]);
    assert_eq!(
        later.children.len(),
        1,
        "the durable child remains available as history"
    );
    assert!(
        later.visible_children().is_empty(),
        "completed agents from older turns must leave the live roster"
    );
    let later_sidebar = crate::view::sidebar::sidebar_lines(&later, 48, 40)
        .lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!later_sidebar.contains("agents"), "{later_sidebar}");

    later
        .children
        .get_mut("call-original")
        .expect("canonical child row")
        .state = "background".into();
    assert_eq!(
        later.visible_children().len(),
        1,
        "an older child remains visible while it is genuinely active"
    );
}

#[test]
fn background_completion_does_not_hide_the_completed_agent_roster() {
    let thread = fixture_thread();
    let launch = TurnId::new();
    let child = ThreadId::new();
    let mut items = vec![
        message_item_for_turn(
            1,
            &thread,
            launch,
            ItemData::UserMessage {
                text: "Check this project".into(),
                attachments: vec![],
                queued: false,
                synthetic: false,
            },
        ),
        message_item_for_turn(
            2,
            &thread,
            launch,
            ItemData::ToolCall {
                call_id: "child".into(),
                name: "task".into(),
                arguments: serde_json::json!({"prompt":"Check this project", "agent":"general"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item_for_turn(
            3,
            &thread,
            launch,
            ItemData::TaskLink {
                call_id: "child".into(),
                child_thread_id: child,
            },
        ),
        message_item_for_turn(
            4,
            &thread,
            launch,
            ItemData::ToolResult {
                call_id: "child".into(),
                name: "task".into(),
                output: "finished".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
        // A background task may finish after the original turn ends, starting
        // a synthetic continuation. It is still the same user interaction.
        message_item_for_turn(
            5,
            &thread,
            TurnId::new(),
            ItemData::UserMessage {
                text: "<task>completed</task>".into(),
                attachments: vec![],
                queued: false,
                synthetic: true,
            },
        ),
    ];
    let app = App::new(
        thread.clone(),
        vec![thread.clone()],
        items.clone(),
        vec![],
        vec![],
    );
    assert_eq!(app.visible_children().len(), 1);
    items.push(message_item_for_turn(
        6,
        &thread,
        TurnId::new(),
        ItemData::UserMessage {
            text: "Now do something else".into(),
            attachments: vec![],
            queued: false,
            synthetic: false,
        },
    ));
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    assert!(app.visible_children().is_empty());
}

#[test]
fn completed_child_elapsed_time_stops_rendering() {
    let started_at = Utc::now() - chrono::Duration::seconds(10);
    let mut task = crate::state::ChildTask {
        child_thread_id: ThreadId::new(),
        child_turn_id: None,
        invoked_turn_id: None,
        state: "running".into(),
        tool_calls: 2,
        last_tool: Some("read".into()),
        started_at: Some(started_at),
    };
    assert!(task.elapsed_seconds().is_some());
    task.state = "done".into();
    assert_eq!(task.elapsed_seconds(), None);
    task.state = "failed".into();
    assert_eq!(task.elapsed_seconds(), None);
}

#[test]
fn durable_child_links_derive_running_and_terminal_lifecycle_from_task_results() {
    let thread = fixture_thread();
    let child_thread = ThreadId::new();
    let mut items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "call-durable-child".into(),
                name: "task".into(),
                arguments: serde_json::json!({
                    "prompt": "inspect the durable child lifecycle",
                    "agent": "plan"
                }),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::TaskLink {
                call_id: "call-durable-child".into(),
                child_thread_id: child_thread,
            },
        ),
    ];
    let running = App::new(
        thread.clone(),
        vec![thread.clone()],
        items.clone(),
        vec![],
        vec![],
    );
    assert_eq!(
        running.children["call-durable-child"].state, "running",
        "a link without a paired result is a live child, not a completed one"
    );

    items.push(message_item(
        3,
        &thread,
        ItemData::ToolResult {
            call_id: "call-durable-child".into(),
            name: "task".into(),
            output: "finished".into(),
            is_error: false,
            metadata: serde_json::Value::Null,
        },
    ));
    let completed = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let task = &completed.children["call-durable-child"];
    assert_eq!(task.state, "done");
    assert_eq!(task.elapsed_seconds(), None, "terminal time is frozen");
    let sidebar: Vec<String> = crate::view::sidebar::sidebar_lines(&completed, 48, 40)
        .lines
        .iter()
        .map(line_text)
        .collect();
    assert!(
        sidebar
            .iter()
            .any(|line| line.contains("✓ inspect the durable child lifecycle")),
        "the completed row preserves the real child title: {sidebar:?}"
    );
    assert!(
        sidebar.iter().any(|line| line.contains("completed")),
        "the completed lifecycle is explicit: {sidebar:?}"
    );
}

#[test]
fn completed_task_roster_follows_the_assistant_narrative() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "call-task-order".into(),
                name: "task".into(),
                arguments: serde_json::json!({"prompt": "audit memory safety"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::AssistantMessage {
                text: "Started the safety agents.".into(),
            },
        ),
    ];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let text = app
        .lines((100, 30))
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
    let narrative = text
        .iter()
        .position(|line| line.contains("Started the safety agents"))
        .expect("assistant narrative");
    let task = text
        .iter()
        .position(|line| line.contains("task audit memory safety"))
        .expect("task roster");
    assert!(
        narrative < task,
        "narrative should introduce the task roster"
    );
}

#[test]
fn same_round_tool_activity_follows_the_assistant_narrative() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "call-bash-order".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "git status --short"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::AssistantMessage {
                text: "I’ll inspect the repository first.".into(),
            },
        ),
    ];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let text = app
        .lines((100, 30))
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
    let narrative = text
        .iter()
        .position(|line| line.contains("I’ll inspect the repository first"))
        .expect("assistant narrative");
    let bash = text
        .iter()
        .position(|line| line.contains("bash git status --short"))
        .expect("bash activity");
    assert!(
        narrative < bash,
        "narrative should introduce tools: {text:?}"
    );
}

#[test]
fn rejected_task_without_a_child_link_is_terminal_not_spinning() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "call-invalid-task".into(),
                name: "task".into(),
                arguments: serde_json::json!({"prompt": "audit server security"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolResult {
                call_id: "call-invalid-task".into(),
                name: "task".into(),
                output: "invalid request: task.task_id must be a child thread ID".into(),
                is_error: true,
                metadata: serde_json::Value::Null,
            },
        ),
    ];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let row = app
        .lines((100, 30))
        .into_iter()
        .map(|line| line_text(&line))
        .find(|line| line.contains("task audit server security"))
        .expect("task activity row");
    assert!(row.contains("failed"), "task row should be terminal: {row}");
    assert!(
        row.contains('✗'),
        "failed task uses the failure marker: {row}"
    );
}

#[test]
fn completed_task_roster_stays_before_post_task_work_and_final_answer() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "call-task-failed".into(),
                name: "task".into(),
                arguments: serde_json::json!({"prompt": "audit workflow security"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolResult {
                call_id: "call-task-failed".into(),
                name: "task".into(),
                output: "Provider rejected the request (cyber_policy)".into(),
                is_error: true,
                metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::ToolCall {
                call_id: "call-read-after-task".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": "README.md"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::ToolResult {
                call_id: "call-read-after-task".into(),
                name: "read".into(),
                output: "read completed".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            5,
            &thread,
            ItemData::AssistantMessage {
                text: "Final security assessment.".into(),
            },
        ),
    ];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let text = app
        .lines((100, 30))
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
    let position = |needle: &str| {
        text.iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle:?} in {text:?}"))
    };
    let task = position("task audit workflow security");
    let read = position("read README.md");
    let final_answer = position("Final security assessment");
    assert!(
        task < read && read < final_answer,
        "completed task activity must not be moved below later parent work: {text:?}"
    );
}

#[test]
fn successful_task_response_lives_in_its_child_instead_of_cluttering_the_parent() {
    let thread = fixture_thread();
    let child = ThreadId::new();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "call-task-compact".into(),
                name: "task".into(),
                arguments: serde_json::json!({"prompt": "audit the provider boundary"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::TaskLink {
                call_id: "call-task-compact".into(),
                child_thread_id: child,
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::ToolResult {
                call_id: "call-task-compact".into(),
                name: "task".into(),
                output: "CHILD-ONLY FINDING\nmore evidence\nfinal recommendation".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::AssistantMessage {
                text: "Parent synthesis.".into(),
            },
        ),
    ];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let text = app
        .lines((100, 30))
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("task audit the provider boundary"), "{text}");
    assert!(text.contains("└ "), "{text}");
    assert!(text.contains("Parent synthesis"), "{text}");
    assert!(
        !text.contains("CHILD-ONLY FINDING"),
        "the child transcript, not the parent, owns successful task output: {text}"
    );
}

#[tokio::test]
async fn child_thread_permission_surfaces_in_the_root_view() {
    let thread = fixture_thread();
    let child = ThreadId::new();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let client = offline_client();
    app.reduce(
        fixture_event(
            "permission.requested",
            child,
            thread.id,
            serde_json::json!({
                "permissionId": "perm-1",
                "operation": "bash:sleep 10",
                "detail": {"command": "sleep 10"},
                "agent": "general",
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.pending_permissions.len(), 1);
    assert_eq!(app.pending_permissions[0].thread_id, child);
    assert!(
        app.permission_focus,
        "an idle composer hands focus to the prompt"
    );
    assert_eq!(app.notice, "Run a shell command: sleep 10");
    assert!(app.pending_notification.is_some());
    // The card on screen is the attention surface; no toast repeats it.
    let frame = app.lines((100, 30));
    let rendered = frame.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert!(!rendered.contains("needs approval"), "{rendered}");
    // Where a dialog hides the card, the toast stands in for it.
    app.open_overlay(crate::state::Overlay::Commands);
    let frame = app.lines((100, 30));
    app.close_overlay();
    let rendered = frame.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert!(rendered.contains("needs approval"), "{rendered}");
    let toast = frame
        .iter()
        .find(|line| line_text(line).contains("needs approval"))
        .expect("top-right toast row");
    assert!(
        toast
            .spans
            .iter()
            .any(|span| span.bg == Some(app.theme.overlay)),
        "the toast body is a raised panel"
    );
    assert!(
        frame.iter().any(|line| line.spans.iter().any(|span| {
            span.fg == app.theme.accent
                && span.text.starts_with('━')
                && span.bg == Some(app.theme.overlay)
        })),
        "an approval toast is capped in the attention color"
    );

    // The key handler removes an answered prompt optimistically, before the
    // matching SSE resolution arrives. That path must dismiss the toast too.
    app.permission_answered("perm-1");
    assert!(app.pending_permissions.is_empty());
    assert!(!app.permission_focus);
    assert!(
        app.notification_toast.is_none(),
        "answered approval cannot keep obscuring the active UI"
    );
    assert!(app.pending_notification.is_none());

    // A later resolution event is idempotent.
    app.reduce(
        fixture_event(
            "permission.resolved",
            child,
            thread.id,
            serde_json::json!({
                "permissionId": "perm-1",
                "resolution": "allowed",
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    assert!(app.pending_permissions.is_empty());
    assert!(!app.permission_focus);
    assert!(app.notification_toast.is_none());
    assert!(app.pending_notification.is_none());
}

#[tokio::test]
async fn parallel_child_questions_queue_and_resolve_one_at_a_time() {
    let thread = fixture_thread();
    let first_child = ThreadId::new();
    let second_child = ThreadId::new();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let client = offline_client();

    app.reduce(
        fixture_event(
            "question.requested",
            first_child,
            thread.id,
            serde_json::json!({
                "questionId": "question-alpha",
                "prompt": "Choose alpha",
                "choices": ["a1", "a2"],
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    app.reduce(
        fixture_event(
            "question.requested",
            second_child,
            thread.id,
            serde_json::json!({
                "questionId": "question-beta",
                "prompt": "Choose beta",
                "choices": ["b1", "b2"],
            }),
        ),
        &client,
    )
    .await
    .unwrap();

    assert_eq!(app.pending_questions.len(), 2);
    assert_eq!(app.pending_questions[0].question_id, "question-alpha");
    assert_eq!(app.pending_questions[0].thread_id, first_child);
    assert_eq!(app.pending_questions[1].thread_id, second_child);
    assert_eq!(app.question_choice(1).as_deref(), Some("a2"));
    let first = crate::view::transcript::transcript_lines(&app, 100);
    let first_text = first
        .lines
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(first_text.contains("Choose alpha"), "{first_text}");
    assert!(
        first_text.contains("1 of 2 questions pending"),
        "{first_text}"
    );
    assert!(!first_text.contains("Choose beta"), "{first_text}");
    assert_eq!(
        first
            .tags
            .iter()
            .filter(|(_, target)| matches!(
                target,
                crate::view::frame::HitTarget::QuestionChoice(_)
            ))
            .count(),
        2,
        "only the first question's choices are interactive"
    );

    app.reduce(
        fixture_event(
            "question.resolved",
            first_child,
            thread.id,
            serde_json::json!({
                "questionId": "question-alpha",
                "resolution": "answered",
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.pending_questions.len(), 1);
    assert_eq!(app.pending_questions[0].question_id, "question-beta");
    assert_eq!(app.question_choice(0).as_deref(), Some("b1"));
    assert_eq!(app.notice, "Choose beta");
    let second = crate::view::transcript::transcript_lines(&app, 100);
    let second_text = second
        .lines
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(second_text.contains("Choose beta"), "{second_text}");
    assert!(!second_text.contains("Choose alpha"), "{second_text}");
}

#[tokio::test]
async fn answered_durable_question_disappears_live_and_after_reconnect() {
    let thread = fixture_thread();
    let item = message_item(
        1,
        &thread,
        ItemData::Question {
            question_id: "durable-question".into(),
            prompt: "Approve these criteria?".into(),
            choices: vec!["Keep the first".into(), "Keep the second".into()],
            multiple: true,
            custom: true,
        },
    );
    let mut app = App::new(
        thread.clone(),
        vec![thread.clone()],
        vec![item.clone()],
        vec![],
        vec![],
    );
    let client = offline_client();

    app.reduce(
        fixture_event(
            "question.requested",
            thread.id,
            thread.id,
            serde_json::json!({
                "questionId": "durable-question",
                "prompt": "Approve these criteria?",
                "choices": ["Keep the first", "Keep the second"],
                "multiple": true,
                "custom": true,
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    let pending = crate::view::transcript::transcript_lines(&app, 100);
    let pending_text = pending
        .lines
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        pending_text.contains("Approve these criteria?"),
        "{pending_text}"
    );
    assert!(
        pending
            .tags
            .iter()
            .any(|(_, target)| matches!(target, crate::view::frame::HitTarget::QuestionChoice(_)))
    );

    // The live resolution event removes both the prompt and its interactive
    // hit targets even though the durable Question item remains in `items`.
    app.reduce(
        fixture_event(
            "question.resolved",
            thread.id,
            thread.id,
            serde_json::json!({
                "questionId": "durable-question",
                "resolution": "answered",
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    let resolved = crate::view::transcript::transcript_lines(&app, 100);
    let resolved_text = resolved
        .lines
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !resolved_text.contains("Approve these criteria?"),
        "resolved question stayed in the live transcript: {resolved_text}"
    );
    assert!(
        !resolved
            .tags
            .iter()
            .any(|(_, target)| matches!(target, crate::view::frame::HitTarget::QuestionChoice(_)))
    );

    // Reconnect/replay starts from durable items plus the server's current
    // pending queue. An empty queue must not resurrect an answered request.
    let mut replayed = App::new(thread.clone(), vec![thread], vec![item], vec![], vec![]);
    replayed.hydrate_questions(vec![]);
    let replayed_text = crate::view::transcript::transcript_lines(&replayed, 100)
        .lines
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !replayed_text.contains("Approve these criteria?"),
        "answered question resurrected after reconnect: {replayed_text}"
    );
}

#[tokio::test]
async fn question_arrows_wrap_highlight_and_enter_submits_the_focused_choice() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.reduce(
        fixture_event(
            "question.requested",
            thread.id,
            thread.id,
            serde_json::json!({
                "questionId": "question-keyboard",
                "prompt": "Choose with arrows",
                "choices": ["first", "second", "third"],
                "custom": true,
            }),
        ),
        &offline_client(),
    )
    .await
    .unwrap();

    let focused = crate::view::transcript::transcript_lines(&app, 80);
    let first_row = focused
        .lines
        .iter()
        .find(|row| row.text().contains("1. first"))
        .expect("first question choice");
    assert!(
        first_row
            .spans
            .iter()
            .any(|span| span.bg == Some(app.theme.primary)),
        "the initial choice must have a visible focus row"
    );

    let original_agent = app.agent_index;
    let original_sidebar = app.sidebar_open;
    for key in [
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
    ] {
        crate::input::handle_key(&mut app, key, &offline_client())
            .await
            .unwrap();
    }
    assert_eq!(app.agent_index, original_agent);
    assert_eq!(app.sidebar_open, original_sidebar);
    assert!(matches!(app.overlay, crate::state::Overlay::None));

    for character in "jack".chars() {
        crate::input::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &offline_client(),
        )
        .await
        .unwrap();
    }
    assert_eq!(app.composer.text(), "jack");
    assert_eq!(app.question_selection, 0);
    let custom = crate::view::transcript::transcript_lines(&app, 80);
    assert!(
        custom.lines.iter().all(|row| row
            .spans
            .iter()
            .all(|span| span.bg != Some(app.theme.primary))),
        "a custom answer draft must not leave a conflicting choice highlighted"
    );
    app.composer.clear();

    // Haarness wraps at both ends instead of leaking arrows into composer
    // history. Moving selection is entirely local and performs no request.
    for (key, expected) in [(KeyCode::Up, 2), (KeyCode::Down, 0), (KeyCode::Down, 1)] {
        crate::input::handle_key(
            &mut app,
            KeyEvent::new(key, KeyModifiers::NONE),
            &offline_client(),
        )
        .await
        .unwrap();
        assert_eq!(app.question_selection, expected);
        assert!(app.composer.is_empty());
    }

    let second_focused = crate::view::transcript::transcript_lines(&app, 80);
    let second_row = second_focused
        .lines
        .iter()
        .find(|row| row.text().contains("2. second"))
        .expect("second question choice");
    assert!(
        second_row
            .spans
            .iter()
            .any(|span| span.bg == Some(app.theme.primary)),
        "arrow navigation must move the visible focus row"
    );

    // A dialog raised above the question owns arrows and Escape. The question
    // remains focused on the same answer after the dialog closes.
    app.open_overlay(crate::state::Overlay::Options {
        title: "foreground dialog".into(),
        kind: crate::state::OptionsKind::ReasoningEffort,
        entries: vec![
            crate::state::OptionEntry {
                label: "one".into(),
                description: String::new(),
                value: "one".into(),
            },
            crate::state::OptionEntry {
                label: "two".into(),
                description: String::new(),
                value: "two".into(),
            },
        ],
    });
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.selection, 1);
    assert_eq!(app.question_selection, 1);
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(matches!(app.overlay, crate::state::Overlay::None));
    assert_eq!(app.pending_questions.len(), 1);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        let mut expected_length = None;
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if expected_length.is_none()
                && let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
            {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                expected_length = Some(header_end + 4 + content_length);
            }
            if expected_length.is_some_and(|length| request.len() >= length) {
                break;
            }
        }
        socket
            .write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        String::from_utf8(request).unwrap()
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();

    let request = server.await.unwrap();
    assert!(request.starts_with("POST /v1/questions/question-keyboard "));
    assert!(request.contains(r#"{"answer":"second"}"#), "{request}");
    assert!(app.pending_questions.is_empty());
    assert_eq!(app.question_selection, 0);
}

#[tokio::test]
async fn custom_false_single_question_rejects_text_and_submits_keyboard_choice() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    for character in "preserved draft".chars() {
        app.composer.insert_char(character);
    }
    app.reduce(
        fixture_event(
            "question.requested",
            thread.id,
            thread.id,
            serde_json::json!({
                "questionId": "closed-keyboard-question",
                "prompt": "Choose exactly one",
                "choices": ["first", "second"],
                "multiple": false,
                "custom": false,
            }),
        ),
        &offline_client(),
    )
    .await
    .unwrap();

    for key in [
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
    ] {
        crate::input::handle_key(&mut app, key, &offline_client())
            .await
            .unwrap();
    }
    assert_eq!(app.composer.text(), "preserved draft");

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.question_selection, 1);
    let rendered = crate::view::transcript::transcript_lines(&app, 80);
    let second = rendered
        .lines
        .iter()
        .find(|row| row.text().contains("2. second"))
        .expect("second closed choice");
    assert!(
        second
            .spans
            .iter()
            .any(|span| span.bg == Some(app.theme.primary)),
        "a pre-existing composer draft must not hide closed-choice focus"
    );

    let (client, server) = question_reply_client().await;
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    let request = server.await.unwrap();
    assert!(
        request.contains(r#"{"answer":"second"}"#),
        "closed question submitted the wrong answer: {request}"
    );
    assert_eq!(app.composer.text(), "preserved draft");
    assert!(app.pending_questions.is_empty());
}

#[tokio::test]
async fn custom_false_single_question_still_submits_mouse_choice() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.reduce(
        fixture_event(
            "question.requested",
            thread.id,
            thread.id,
            serde_json::json!({
                "questionId": "closed-mouse-question",
                "prompt": "Choose exactly one",
                "choices": ["first", "second"],
                "multiple": false,
                "custom": false,
            }),
        ),
        &offline_client(),
    )
    .await
    .unwrap();

    let rendered = crate::view::transcript::transcript_lines(&app, 80);
    let second_row = rendered
        .tags
        .iter()
        .find(|(_, target)| matches!(target, crate::view::frame::HitTarget::QuestionChoice(1)))
        .map(|(row, _)| *row)
        .expect("second closed choice hit row");
    app.hit_rows.replace(
        rendered
            .tags
            .iter()
            .map(|(row, target)| crate::view::frame::HitRegion {
                row: *row,
                start: 0,
                end: 80,
                target: target.clone(),
                paint_hover: false,
            })
            .collect(),
    );

    let (client, server) = question_reply_client().await;
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column: 2,
                row: u16::try_from(second_row).unwrap(),
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
    }
    let request = server.await.unwrap();
    assert!(
        request.contains(r#"{"answer":"second"}"#),
        "mouse submitted the wrong closed choice: {request}"
    );
    assert!(app.pending_questions.is_empty());
}

#[tokio::test]
async fn criteria_question_supports_mouse_keyboard_multi_select_and_custom_text() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.reduce(
        fixture_event(
            "question.requested",
            thread.id,
            thread.id,
            serde_json::json!({
                "questionId": "criteria-question",
                "prompt": "Which criteria should be tracked?",
                "choices": ["Tests pass", "Clippy is clean", "Tmux is verified"],
                "multiple": true,
                "custom": true
            }),
        ),
        &offline_client(),
    )
    .await
    .unwrap();

    let rendered = crate::view::transcript::transcript_lines(&app, 100);
    assert!(rendered.lines.iter().any(|line| {
        line.text()
            .contains("space toggle · enter confirm · type custom")
    }));
    let second = rendered
        .tags
        .iter()
        .find(|(_, target)| matches!(target, crate::view::frame::HitTarget::QuestionChoice(1)))
        .map(|(row, _)| *row)
        .expect("second criterion hit row");
    app.hit_rows.replace(
        rendered
            .tags
            .iter()
            .map(|(row, target)| crate::view::frame::HitRegion {
                row: *row,
                start: 0,
                end: 100,
                target: target.clone(),
                paint_hover: false,
            })
            .collect(),
    );
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column: 2,
                row: u16::try_from(second).unwrap(),
                modifiers: KeyModifiers::NONE,
            },
            &offline_client(),
        )
        .await
        .unwrap();
    }
    assert_eq!(app.question_selection, 1);
    assert_eq!(
        app.question_selected.iter().copied().collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        app.pending_questions.len(),
        1,
        "click toggles without submitting"
    );

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(
        app.question_selected.iter().copied().collect::<Vec<_>>(),
        [0, 1]
    );
    for character in "Manual smoke proof".chars() {
        crate::input::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &offline_client(),
        )
        .await
        .unwrap();
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        let mut expected_length = None;
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if expected_length.is_none()
                && let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
            {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                expected_length = Some(header_end + 4 + content_length);
            }
            if expected_length.is_some_and(|length| request.len() >= length) {
                break;
            }
        }
        socket
            .write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        String::from_utf8(request).unwrap()
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();

    let request = server.await.unwrap();
    let body = request.split_once("\r\n\r\n").unwrap().1;
    let envelope: serde_json::Value = serde_json::from_str(body).unwrap();
    let answers: Vec<String> = serde_json::from_str(envelope["answer"].as_str().unwrap()).unwrap();
    assert_eq!(
        answers,
        ["Tests pass", "Clippy is clean", "Manual smoke proof"]
    );
    assert!(app.pending_questions.is_empty());
    assert!(app.question_selected.is_empty());
}

#[tokio::test]
async fn empty_multi_select_submits_an_empty_array_for_server_fallback() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.reduce(
        fixture_event(
            "question.requested",
            thread.id,
            thread.id,
            serde_json::json!({
                "questionId": "criteria-empty",
                "prompt": "Approve criteria",
                "choices": ["Tests pass", "Clippy is clean"],
                "multiple": true,
                "custom": true
            }),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            request.extend_from_slice(&chunk[..read]);
            if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + length {
                    break;
                }
            }
        }
        socket
            .write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        String::from_utf8(request).unwrap()
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    let request = server.await.unwrap();
    assert!(request.contains(r#"{"answer":"[]"}"#), "{request}");
    assert!(app.pending_questions.is_empty());
}

#[test]
fn question_hydration_filters_the_current_root_and_preserves_oldest_first() {
    let thread = fixture_thread();
    let foreign_root = ThreadId::new();
    let child = ThreadId::new();
    let now = Utc::now();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.hydrate_questions(vec![
        haarness_protocol::PendingQuestionInfo {
            question_id: "newer".into(),
            prompt: "Newer".into(),
            choices: vec![],
            multiple: false,
            custom: true,
            thread_id: child,
            root_thread_id: Some(thread.id),
            turn_id: None,
            created_at: now,
        },
        haarness_protocol::PendingQuestionInfo {
            question_id: "foreign".into(),
            prompt: "Foreign".into(),
            choices: vec![],
            multiple: false,
            custom: true,
            thread_id: ThreadId::new(),
            root_thread_id: Some(foreign_root),
            turn_id: None,
            created_at: now - chrono::Duration::seconds(2),
        },
        haarness_protocol::PendingQuestionInfo {
            question_id: "older".into(),
            prompt: "Older".into(),
            choices: vec!["yes".into()],
            multiple: false,
            custom: true,
            thread_id: child,
            root_thread_id: Some(thread.id),
            turn_id: None,
            created_at: now - chrono::Duration::seconds(1),
        },
    ]);

    assert_eq!(
        app.pending_questions
            .iter()
            .map(|question| question.question_id.as_str())
            .collect::<Vec<_>>(),
        vec!["older", "newer"]
    );
    assert_eq!(app.notice, "Older");
}

#[tokio::test]
async fn typing_leaves_permission_focus_off_until_ctrl_g() {
    let thread = fixture_thread();
    let child = ThreadId::new();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.composer.set_text("always run tests before".into());
    let client = offline_client();
    app.reduce(
        fixture_event(
            "permission.requested",
            child,
            thread.id,
            serde_json::json!({
                "permissionId": "perm-2",
                "operation": "bash:true",
                "detail": {},
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    assert!(
        !app.permission_focus,
        "a non-empty composer keeps focus: typing 'always' must not answer"
    );
    assert!(app.notice.contains("ctrl+g"));
}

fn pending_permission_fixture(
    thread: &Thread,
    id: &str,
) -> haarness_protocol::PendingPermissionInfo {
    haarness_protocol::PendingPermissionInfo {
        allow_always: None,
        repository_scope: None,
        permission_id: id.into(),
        operation: "bash:git push origin main".into(),
        detail: serde_json::json!({"command": "git push origin main"}),
        permission: "bash".into(),
        patterns: vec!["git push origin main".into()],
        always_patterns: vec!["bash:git push *".into()],
        thread_id: thread.id,
        root_thread_id: Some(thread.id),
        turn_id: None,
        agent: None,
        created_at: Utc::now(),
    }
}

#[test]
fn agent_switch_approval_discloses_destination_model_and_scope() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let mut pending = pending_permission_fixture(&thread, "switch");
    pending.permission = "agent_switch".into();
    pending.operation = "agent_switch:opaque".into();
    pending.patterns = vec!["opaque".into()];
    pending.always_patterns = vec!["agent_switch:opaque".into()];
    pending.detail = serde_json::json!({"reason":"Review the changes", "__agentSwitch":{
        "agent":"review", "model":"fixture/next", "previousModel":"fixture/previous",
    }});
    app.pending_permissions = vec![pending];
    app.permission_focus = true;
    app.confirm_always = Some('a');
    for width in [55, 120] {
        let frame = frame_text(&app, (width, 45));
        for text in [
            "Agent: review",
            "Model: fixture/next",
            "Previous model: fixture/previous",
            "only agent review in this chat",
        ] {
            assert!(frame.contains(text), "missing {text}: {frame}");
        }
        assert!(!frame.contains("opaque"));
    }
}

async fn press_key(app: &mut App, code: KeyCode) {
    crate::input::handle_key(
        app,
        KeyEvent::new(code, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
}

fn frame_text(app: &App, size: (u32, u32)) -> String {
    app.lines(size)
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect()
}

#[test]
fn cloud_billing_balances_and_panel_fit_narrow_terminals() {
    let mut app = selector_fixture();
    assert!(!frame_text(&app, (160, 46)).contains(" · /billing"));
    app.billing = serde_json::from_value(serde_json::json!({
        "loggedIn":true,"error":null,"summary":{
            "accountId":"test-account","accountName":"Test workspace","plan":"power","accountStatus":"active",
            "subscriptionStatus":"active","includedCredits":"100.123456","purchasedCredits":"500.000000","heldCredits":"1.500000",
            "cloudUnits":"2.00","cloudUnitsHeld":"0.10","resetsAt":"2026-10-27T00:00:00Z","periodEndsAt":"2026-10-27T00:00:00Z",
            "cancelAtPeriodEnd":false,"overageOptIn":false,"billingUrl":"https://app.haarness.ai/app/billing","usageUrl":"https://app.haarness.ai/app/billing/usage"
        }
    })).unwrap();
    assert!(frame_text(&app, (160, 46)).contains("100.12 included · 500 purchased"));
    for width in [160, 100, 80] {
        app.open_billing();
        let lines = app.lines((width, 46));
        assert!(lines.iter().all(|line| line.width() <= width as usize));
        let text = frame_text(&app, (width, 46));
        assert!(text.contains("Power plan · renews Oct 27"), "{text}");
        assert!(text.contains("100.12 credits · resets Oct 27"), "{text}");
        assert!(text.contains("Purchased"));
        assert!(text.contains("1.5 credits in use"), "{text}");
        // The next step is a row to choose, not a URL or a subcommand.
        assert!(text.contains("Add credits"), "{text}");
        assert!(text.contains("Change plan"), "{text}");
        assert!(!text.contains("https://"), "{text}");
    }
    app.billing.logged_in = false;
    app.open_billing();
    let text = frame_text(&app, (100, 46));
    assert!(text.contains("Sign in to Haarness"), "{text}");
    app.billing.logged_in = true;
    app.close_overlay();
    app.billing.error = Some("Offline".into());
    app.overlay = crate::state::Overlay::None;
    assert!(frame_text(&app, (160, 46)).contains("Cloud credits unavailable"));
}

#[test]
fn approval_panel_docks_the_full_request_above_the_composer() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.pending_permissions
        .push(pending_permission_fixture(&thread, "perm-panel"));
    app.permission_focus = true;
    let text = frame_text(&app, (100, 40));
    // The headline names the kind of thing; the row beneath is the only place
    // the command itself appears.
    assert!(text.contains("⚠ Run a shell command"), "{text}");
    assert!(text.contains("$ git push origin main"), "{text}");
    assert_eq!(
        text.matches("git push origin main").count(),
        1,
        "the command is printed once: {text}"
    );
    // The persisted rule drops the category prefix the headline just spelled
    // out, and earns its row by reaching wider than the command.
    assert!(text.contains("always allows: git push *"), "{text}");
    assert!(!text.contains("bash:"), "{text}");
    for chip in ["Allow once", "Always", "This folder", "Reject"] {
        assert!(text.contains(chip), "missing chip {chip}: {text}");
    }
    // Each chip carries its own key, so the footer never has to list them.
    for key in ["Allow once  y", "Reject  n"] {
        assert!(text.contains(key), "missing hotkey {key}: {text}");
    }
    assert!(!text.contains("y/a/w/n"), "{text}");
    assert!(text.contains("←→ select"), "{text}");

    // Unfocused, the panel stays visible but hands its hint to ctrl+g.
    app.permission_focus = false;
    let text = frame_text(&app, (100, 40));
    assert!(text.contains("⚠ Run a shell command"), "{text}");
    assert!(text.contains("ctrl+g"), "{text}");
}

/// A request whose "always" rule is exactly what is already on screen — every
/// `external_directory` ask — must not print that path a second time.
#[test]
fn an_always_rule_identical_to_the_subject_is_not_repeated() {
    let thread = fixture_thread();
    let path = "/Users/alex/.cargo/registry/src/index.crates.io-1949cf8/reqwest-0.13.4/src/*";
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.pending_permissions
        .push(haarness_protocol::PendingPermissionInfo {
            allow_always: None,
            repository_scope: None,
            permission_id: "perm-external".into(),
            operation: format!("external_directory:{path}"),
            detail: serde_json::json!({}),
            permission: "external_directory".into(),
            patterns: vec![path.into()],
            always_patterns: vec![format!("external_directory:{path}")],
            thread_id: thread.id,
            root_thread_id: Some(thread.id),
            turn_id: None,
            agent: None,
            created_at: Utc::now(),
        });
    app.permission_focus = true;

    for staged in [None, Some('a')] {
        app.confirm_always = staged;
        let text = frame_text(&app, (120, 40));
        assert!(text.contains("⚠ Access outside the project"), "{text}");
        assert_eq!(
            text.matches(path).count(),
            1,
            "the path belongs on screen once, staged={staged:?}: {text}"
        );
        assert!(
            !text.contains("external_directory:"),
            "the rule prefix is what the headline replaced: {text}"
        );
    }

    // Staged, the chips already read Confirm and Cancel; nothing restates them.
    let text = frame_text(&app, (120, 40));
    // Plain Always lives in the server, for every chat it runs, not in this
    // chat alone.
    assert!(
        text.contains("always allow in every chat until the Haarness server stops"),
        "{text}"
    );
    assert_eq!(text.matches("Confirm").count(), 1, "{text}");
    assert_eq!(text.matches("Cancel").count(), 1, "{text}");
}

#[tokio::test]
async fn arrows_and_enter_answer_the_docked_approval() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let (client, server) = workspace_http_status_client(vec![(200, "{}".into())], false).await;
    app.reduce(
        fixture_event(
            "permission.requested",
            thread.id,
            thread.id,
            serde_json::json!({
                "permissionId": "perm-arrows",
                "operation": "bash:git push origin main",
                "detail": {"command": "git push origin main"},
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    assert!(app.permission_focus);
    // Wrap left from the first chip to Reject, then confirm it.
    press_key(&mut app, KeyCode::Left).await;
    assert_eq!(app.permission_selection, 4, "new Auto chip");
    press_key(&mut app, KeyCode::Left).await;
    assert_eq!(app.permission_selection, 3);
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(app.pending_permissions.is_empty());
    assert!(!app.permission_focus);
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn enter_on_always_stages_the_confirm_chips_before_committing() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.pending_permissions
        .push(pending_permission_fixture(&thread, "perm-always"));
    app.permission_focus = true;
    let (client, server) = workspace_http_status_client(vec![(200, "{}".into())], false).await;

    press_key(&mut app, KeyCode::Right).await;
    assert_eq!(app.permission_selection, 1, "Always chip");
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.confirm_always, Some('a'));
    let text = frame_text(&app, (100, 40));
    assert!(
        text.contains("always allow in every chat until the Haarness server stops:"),
        "{text}"
    );
    assert!(text.contains("git push *"), "{text}");
    // The command stays on screen while the rule is staged: this is the
    // moment you want to re-read what you are granting standing access to.
    assert!(text.contains("$ git push origin main"), "{text}");
    assert!(text.contains("Confirm"), "{text}");
    assert!(text.contains("Cancel"), "{text}");

    // Esc backs out of the stage without answering the prompt.
    press_key(&mut app, KeyCode::Esc).await;
    assert_eq!(app.confirm_always, None);
    assert_eq!(app.pending_permissions.len(), 1);

    // Stage again and commit with the Confirm chip.
    press_key(&mut app, KeyCode::Right).await;
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.confirm_always, Some('a'));
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.confirm_always, None);
    assert!(app.pending_permissions.is_empty());
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn clicking_a_chip_answers_the_approval() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.pending_permissions
        .push(pending_permission_fixture(&thread, "perm-click"));
    let (client, server) = workspace_http_status_client(vec![(200, "{}".into())], false).await;
    let _ = app.lines((100, 40));
    let hit = app
        .hit_rows
        .borrow()
        .iter()
        .find(|hit| {
            matches!(
                hit.target,
                crate::view::frame::HitTarget::PermissionOption(0)
            )
        })
        .cloned()
        .expect("Allow once chip is clickable");
    let column = u16::try_from(hit.start).unwrap();
    let row = u16::try_from(hit.row).unwrap();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
    }
    assert!(app.pending_permissions.is_empty());
    assert_eq!(server.await.unwrap().len(), 1);
}

fn outside_write_event(thread: &Thread, id: &str) -> haarness_protocol::EventEnvelope {
    fixture_event(
        "permission.requested",
        thread.id,
        thread.id,
        serde_json::json!({
            "permissionId": id,
            "permission": "external_directory",
            "operation": "external_directory:/tmp/elsewhere/*",
            "patterns": ["/tmp/elsewhere/*"],
            "alwaysPatterns": ["external_directory:/tmp/elsewhere/*"],
            "repositoryScope": "/tmp/proj",
            "detail": {"path": "/tmp/elsewhere/notes.txt"},
        }),
    )
}

async fn type_text(app: &mut App, text: &str, client: &crate::api::ApiClient) {
    for character in text.chars() {
        crate::input::handle_key(
            app,
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            client,
        )
        .await
        .unwrap();
    }
}

/// The card takes the keyboard when it arrives on an empty prompt, but a
/// message typed anyway lands in the prompt whole and answers nothing, even
/// when its first letters are hotkeys ("u" is Once + Auto, "a" Always, "r"
/// Repository, "h"/"l" move the selection), or a doubled one that would
/// otherwise confirm what the first staged ("www", "uu").
#[tokio::test]
async fn typing_a_message_while_the_card_waits_answers_nothing() {
    for message in [
        "update the readme too",
        "always ask me",
        "hello",
        "run it again",
        "www.example.com is down",
        "uuid parsing fails",
    ] {
        let thread = fixture_thread();
        let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
        // Any reply would fail against this client; none must be attempted.
        let client = offline_client();
        app.reduce(outside_write_event(&thread, "perm-typing"), &client)
            .await
            .unwrap();
        assert!(app.permission_focus);
        type_text(&mut app, message, &client).await;
        assert_eq!(app.composer.text(), message);
        assert_eq!(app.pending_permissions.len(), 1, "{message}");
        assert!(!app.auto_permissions, "{message}");
        assert_eq!(app.confirm_always, None, "{message}");
        assert_eq!(app.permission_selection, 0, "{message}");
        assert!(!app.permission_focus, "{message}");
        assert!(
            app.notice.contains("ctrl+g to answer"),
            "the card still says how to answer it: {}",
            app.notice
        );
    }
}

/// A prompt from an older server carries no "always" patterns; its Always
/// still waits for a confirming press instead of answering on one letter.
#[tokio::test]
async fn always_without_patterns_is_still_staged() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let mut event = outside_write_event(&thread, "perm-legacy");
    event.payload["alwaysPatterns"] = serde_json::json!([]);
    app.reduce(event, &offline_client()).await.unwrap();
    press_key(&mut app, KeyCode::Char('a')).await;
    assert_eq!(app.confirm_always, Some('a'));
    assert_eq!(app.pending_permissions.len(), 1);
    let text = frame_text(&app, (100, 40));
    assert!(
        text.contains("always allow in every chat until the Haarness server stops"),
        "{text}"
    );
}

#[tokio::test]
async fn once_plus_auto_is_staged_before_it_switches_the_chat() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.reduce(outside_write_event(&thread, "perm-auto"), &offline_client())
        .await
        .unwrap();
    press_key(&mut app, KeyCode::Char('u')).await;
    assert_eq!(app.confirm_always, Some('u'));
    assert!(!app.auto_permissions);
    let text = frame_text(&app, (100, 40));
    assert!(
        text.contains("allow once and switch this chat to Auto"),
        "{text}"
    );
    assert!(
        text.contains("Confirm") && text.contains("Cancel"),
        "{text}"
    );
    // The path rules only matter to "always" answers.
    assert!(!text.contains("/tmp/elsewhere/*"), "{text}");
    assert_eq!(
        app.notice,
        "enter or u confirms, esc cancels · allow once and switch this chat to Auto: later requests are allowed without asking"
    );

    // Pressed again once there was time to read the card, the same letter
    // confirms.
    if let Some((_, _, at)) = &mut app.permission_typed {
        *at -= std::time::Duration::from_secs(1);
    }
    let (client, server) = workspace_http_status_client(vec![(200, "{}".into())], false).await;
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(app.auto_permissions);
    assert!(app.pending_permissions.is_empty());
    assert!(server.await.unwrap()[0].contains("\"decision\":\"once\""));
}

/// A choice staged on a prompt whose turn ended unanswered does not carry over:
/// the next prompt opens on its own chips, so Enter cannot confirm the old
/// grant on a new request.
#[tokio::test]
async fn a_new_prompt_never_inherits_a_staged_answer() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let client = offline_client();
    app.reduce(outside_write_event(&thread, "perm-old"), &client)
        .await
        .unwrap();
    press_key(&mut app, KeyCode::Char('u')).await;
    assert_eq!(app.confirm_always, Some('u'));
    app.reduce(
        fixture_event(
            "turn.finished",
            thread.id,
            thread.id,
            serde_json::json!({"status": "cancelled"}),
        ),
        &client,
    )
    .await
    .unwrap();
    assert!(app.pending_permissions.is_empty());
    app.reduce(outside_write_event(&thread, "perm-new"), &client)
        .await
        .unwrap();
    assert!(app.permission_focus);
    assert_eq!(app.confirm_always, None);
    assert_eq!(app.permission_typed, None);
    let text = frame_text(&app, (100, 40));
    assert!(text.contains("Allow once"), "{text}");
    assert!(!text.contains("Confirm"), "{text}");
}

#[tokio::test]
async fn the_prompt_draws_no_cursor_while_the_card_owns_the_keyboard() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.reduce(
        outside_write_event(&thread, "perm-cursor"),
        &offline_client(),
    )
    .await
    .unwrap();
    let text = frame_text(&app, (100, 40));
    assert!(
        text.contains("Answer the approval above · ctrl+g to type"),
        "{text}"
    );
    assert!(!text.contains("Ask anything…"), "{text}");
    // Ctrl+G hands the keyboard back without answering.
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(!app.permission_focus);
    assert_eq!(app.pending_permissions.len(), 1);
    let text = frame_text(&app, (100, 40));
    assert!(text.contains("Ask anything…"), "{text}");
    assert!(text.contains("ctrl+g answer"), "{text}");
    // A draft keeps its text but loses its live cursor when the card is
    // focused again.
    type_text(&mut app, "draft", &offline_client()).await;
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(app.permission_focus);
    let lines = app.lines((100, 40));
    let row = lines
        .iter()
        .find(|line| line_text(line).contains("draft"))
        .expect("draft row");
    assert!(
        row.spans
            .iter()
            .all(|span| span.bg != Some(app.theme.primary)),
        "no cursor cell while the card owns the keyboard"
    );
}

/// Every choice stays on screen and clickable at 80, 60 and 40 columns: the
/// chips wrap instead of running off the edge, and Reject never disappears.
#[tokio::test]
async fn approval_chips_wrap_instead_of_clipping() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.reduce(outside_write_event(&thread, "perm-wrap"), &offline_client())
        .await
        .unwrap();
    let labels = [
        "Allow once",
        "Always",
        "This folder",
        "Repository",
        "Reject",
        "Once + Auto",
    ];
    for (width, height) in [(120, 36), (80, 24), (60, 20), (40, 24)] {
        let lines = app.lines((width, height));
        let text: String = lines.iter().map(|line| line_text(line) + "\n").collect();
        assert!(
            lines.iter().all(|line| line.width() <= width as usize),
            "{text}"
        );
        for label in labels {
            assert!(text.contains(label), "{width} columns lost {label}: {text}");
        }
        let hits = app.hit_rows.borrow();
        for (index, expected) in labels.iter().enumerate() {
            let hit = hits
                .iter()
                .find(|hit| hit.target == crate::view::frame::HitTarget::PermissionOption(index))
                .unwrap_or_else(|| panic!("chip {index} not clickable at {width}: {text}"));
            assert!(hit.end <= width as usize, "{width}: {text}");
            let row = line_text(&lines[hit.row]);
            let label: String = row
                .chars()
                .skip(hit.start)
                .take(hit.end - hit.start)
                .collect();
            assert!(
                label.contains(expected),
                "{width}: hit {label:?} for {expected}"
            );
        }
        // The headline is not cut short for a queue position it does not have.
        assert!(text.contains("⚠ Access outside the project"), "{text}");
        // One quiet row between the card and the prompt.
        let prompt = lines
            .iter()
            .position(|line| line_text(line).contains("Answer the approval above"))
            .expect("prompt row");
        assert!(line_text(&lines[prompt - 1]).trim().is_empty(), "{text}");
        assert!(
            line_text(&lines[prompt - 2]).contains("esc reject"),
            "{text}"
        );
    }
}

/// A short terminal trims the card's detail before the prompt and band.
#[tokio::test]
async fn a_short_terminal_trims_the_card_not_the_prompt() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let long = format!("/tmp/{}/notes.txt", "deeply/nested/folder".repeat(8));
    let mut event = outside_write_event(&thread, "perm-short");
    event.payload["detail"]["path"] = serde_json::json!(long);
    app.reduce(event, &offline_client()).await.unwrap();
    for (width, height) in [(40, 15), (60, 14), (80, 12)] {
        let lines = app.lines((width, height));
        let text: String = lines.iter().map(|line| line_text(line) + "\n").collect();
        assert_eq!(lines.len(), height as usize);
        assert!(
            line_text(&lines[height as usize - 1]).contains("approve"),
            "{text}"
        );
        // The whole hint stays readable, wrapped if need be, and keeps its
        // quiet row above it.
        assert!(text.contains("to type"), "{text}");
        let prompt = lines
            .iter()
            .position(|line| line_text(line).contains("Answer the approval above"))
            .expect("prompt row");
        assert!(line_text(&lines[prompt - 1]).trim().is_empty(), "{text}");
        assert!(text.contains("⚠ Access outside the project"), "{text}");
        assert!(text.contains("/tmp/deeply"), "{text}");
        for label in ["Allow once", "Reject", "Once + Auto"] {
            assert!(
                text.contains(label),
                "{width}x{height} lost {label}: {text}"
            );
        }
    }
}

/// The card is the attention surface whenever it shows; the toast and the
/// status hint repeat it in its own plain words, never the wire's rule key.
#[tokio::test]
async fn approval_hints_use_plain_words_and_the_toast_yields_to_the_card() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.reduce(
        outside_write_event(&thread, "perm-words"),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(
        app.notice,
        "Access outside the project: /tmp/elsewhere/notes.txt"
    );
    let message = "Haarness needs approval: Access outside the project: /tmp/elsewhere/notes.txt";
    assert_eq!(app.pending_notification.as_deref(), Some(message));
    let text = frame_text(&app, (120, 36));
    assert!(!text.contains("Haarness needs approval"), "{text}");
    assert!(!text.contains("external_directory"), "{text}");
    // Where a dialog hides the card, the toast stands in for it.
    app.open_overlay(crate::state::Overlay::Commands);
    let text = frame_text(&app, (120, 36));
    assert!(text.contains("Haarness needs approval"), "{text}");
    assert!(!text.contains("external_directory"), "{text}");
    app.close_overlay();

    app.release_permission_focus(false);
    assert_eq!(
        app.notice,
        "Approval pending: Access outside the project: /tmp/elsewhere/notes.txt  ·  ctrl+g to answer"
    );
    // Staging Always names its real reach: every chat the server runs.
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
        &offline_client(),
    )
    .await
    .unwrap();
    press_key(&mut app, KeyCode::Char('a')).await;
    assert_eq!(
        app.notice,
        "enter or a confirms, esc cancels · always allow in every chat until the Haarness server stops"
    );
    assert!(!app.notice.contains("session"), "{}", app.notice);
}

#[test]
fn a_rejected_tool_and_a_stopped_turn_read_as_choices_not_failures() {
    let thread = fixture_thread();
    let call = message_item(
        1,
        &thread,
        ItemData::ToolCall {
            call_id: "call-1".into(),
            name: "write".into(),
            arguments: serde_json::json!({"path": "/tmp/elsewhere/notes.txt", "content": "x"}),
            provider_metadata: serde_json::Value::Null,
        },
    );
    let result = message_item(
        2,
        &thread,
        ItemData::ToolResult {
            call_id: "call-1".into(),
            name: "write".into(),
            output: "The user rejected this action. Ask a question or take a different approach instead of retrying.".into(),
            is_error: true,
            metadata: serde_json::Value::Null,
        },
    );
    // The stop also cancels the call that was waiting for its approval.
    let waiting = message_item(
        3,
        &thread,
        ItemData::ToolCall {
            call_id: "call-2".into(),
            name: "write".into(),
            arguments: serde_json::json!({"path": "/tmp/elsewhere/later.txt", "content": "x"}),
            provider_metadata: serde_json::Value::Null,
        },
    );
    let cancelled = message_item(
        4,
        &thread,
        ItemData::ToolResult {
            call_id: "call-2".into(),
            name: "write".into(),
            output: "operation cancelled".into(),
            is_error: true,
            metadata: serde_json::Value::Null,
        },
    );
    let stopped = message_item(
        5,
        &thread,
        ItemData::Error {
            message: "operation cancelled".into(),
            retryable: false,
        },
    );
    let app = App::new(
        thread.clone(),
        vec![thread],
        vec![call, result, waiting, cancelled, stopped],
        vec![],
        vec![],
    );
    let transcript = crate::view::transcript::transcript_lines(&app, 100);
    let text: String = transcript
        .lines
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(
        text.contains("⊘ write /tmp/elsewhere/notes.txt  rejected"),
        "{text}"
    );
    assert!(!text.contains("failed"), "{text}");
    assert!(!text.contains("The user rejected"), "{text}");
    assert!(
        text.contains("⊘ write /tmp/elsewhere/later.txt  cancelled"),
        "{text}"
    );
    assert!(text.contains("· interrupted"), "{text}");
    assert!(!text.contains("operation cancelled"), "{text}");
    for line in &transcript.lines {
        assert!(
            line.spans.iter().all(|span| span.fg != app.theme.danger),
            "{text}"
        );
    }
}

#[test]
fn the_band_names_the_approval_mode_apart_from_a_review_agent() {
    let mut thread = fixture_thread();
    thread.agent = "review".into();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let band = frame_text(&app, (120, 36));
    let band = band.lines().last().unwrap();
    assert!(band.contains("approvals: review"), "{band}");
    assert!(!band.contains("⏸"), "{band}");
    app.auto_permissions = true;
    let band = frame_text(&app, (120, 36));
    assert!(band.lines().last().unwrap().contains("approvals: auto"));
}

#[test]
fn queued_followups_wait_above_the_composer_until_consumed() {
    let thread = fixture_thread();
    let queued = message_item(
        1,
        &thread,
        ItemData::UserMessage {
            text: "also add tests".into(),
            attachments: vec![],
            queued: true,
            synthetic: false,
        },
    );
    let queued_id = queued.id;
    let mut app = App::new(thread.clone(), vec![thread], vec![queued], vec![], vec![]);
    app.active_turn = Some(TurnId::new());

    // The waiting strip owns the message; the transcript does not.
    let transcript: String = crate::view::transcript::transcript_lines(&app, 100)
        .lines
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(!transcript.contains("also add tests"), "{transcript}");
    let text = frame_text(&app, (100, 30));
    assert!(
        text.contains("Steering · 1 — joins at the next step"),
        "{text}"
    );
    assert!(text.contains("1. also add tests"), "{text}");

    // Consumption moves it into the conversation and empties the strip.
    app.items.push(message_item(
        2,
        &app.thread,
        ItemData::QueueConsumed { item_id: queued_id },
    ));
    let transcript: String = crate::view::transcript::transcript_lines(&app, 100)
        .lines
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(transcript.contains("also add tests"), "{transcript}");
    let text = frame_text(&app, (100, 30));
    assert!(!text.contains("Steering ·"), "{text}");
}

#[tokio::test]
async fn unrelated_thread_events_are_dropped() {
    let thread = fixture_thread();
    let foreign_thread = ThreadId::new();
    let foreign_root = ThreadId::new();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let client = offline_client();
    app.reduce(
        fixture_event(
            "permission.requested",
            foreign_thread,
            foreign_root,
            serde_json::json!({"permissionId": "perm-3", "operation": "bash:rm", "detail": {}}),
        ),
        &client,
    )
    .await
    .unwrap();
    assert!(app.pending_permissions.is_empty());
}

#[tokio::test]
async fn global_active_turn_snapshot_keeps_unrelated_root_activity_in_live_cache() {
    let current = fixture_thread();
    let background = Thread {
        title: "Background".into(),
        ..fixture_thread()
    };
    let background_turn = fixture_running_turn(background.id);
    let mut app = App::new(
        current.clone(),
        vec![current.clone(), background.clone()],
        vec![],
        vec![],
        vec![],
    );

    let mut started = fixture_event(
        "turn.started",
        background.id,
        background.id,
        serde_json::json!({}),
    );
    started.turn_id = Some(background_turn.id);
    app.reduce(started, &offline_client()).await.unwrap();
    app.reduce(
        fixture_event(
            "turn.text_delta",
            background.id,
            background.id,
            serde_json::json!({"delta": "still running"}),
        ),
        &offline_client(),
    )
    .await
    .unwrap();

    assert_eq!(app.thread.id, current.id);
    assert_eq!(app.root_thread_id, current.id);
    assert_eq!(
        app.active_turn, None,
        "background work does not steal focus"
    );
    assert!(
        app.preview.is_empty(),
        "background text stays out of this view"
    );
    assert_eq!(
        app.live_threads
            .get(&background.id)
            .map(|live| live.preview.as_str()),
        Some("still running"),
        "unrelated roots remain globally visible after subsequent activity"
    );
}

#[test]
fn global_active_turn_snapshot_hydrates_the_selected_turn_on_attach() {
    let selected = fixture_thread();
    let background = Thread {
        title: "Background".into(),
        ..fixture_thread()
    };
    let selected_turn = fixture_running_turn(selected.id);
    let background_turn = fixture_running_turn(background.id);
    let mut app = App::new(
        selected.clone(),
        vec![selected.clone(), background.clone()],
        vec![],
        vec![],
        vec![],
    );

    app.hydrate_active_turns(vec![background_turn.clone(), selected_turn.clone()]);

    assert_eq!(app.thread.id, selected.id);
    assert_eq!(app.active_turn, Some(selected_turn.id));
    assert_eq!(app.root_active_turn, Some(selected_turn.id));
    assert!(app.turn_started.is_some());
    assert!(app.live_threads.contains_key(&selected.id));
    assert!(app.live_threads.contains_key(&background.id));
}

#[tokio::test]
async fn global_active_turn_snapshot_clears_only_background_completion() {
    let current = fixture_thread();
    let background = Thread {
        title: "Background".into(),
        ..fixture_thread()
    };
    let current_turn = fixture_running_turn(current.id);
    let background_turn = fixture_running_turn(background.id);
    let mut app = App::new(
        current.clone(),
        vec![current.clone(), background.clone()],
        vec![],
        vec![],
        vec![],
    );
    app.hydrate_active_turns(vec![current_turn.clone(), background_turn.clone()]);

    let mut finished = fixture_event(
        "turn.finished",
        background.id,
        background.id,
        serde_json::json!({"status": "completed"}),
    );
    finished.turn_id = Some(background_turn.id);
    app.reduce(finished, &offline_client()).await.unwrap();
    app.release_completions(std::time::Instant::now() + crate::state::COMPLETION_GRACE);

    assert_eq!(
        app.thread.id, current.id,
        "completion cannot navigate the view"
    );
    assert_eq!(app.root_thread_id, current.id);
    assert_eq!(app.active_turn, Some(current_turn.id));
    assert_eq!(app.root_active_turn, Some(current_turn.id));
    assert!(app.live_threads.contains_key(&current.id));
    assert!(!app.live_threads.contains_key(&background.id));
    assert_eq!(
        app.notification_toast
            .as_ref()
            .map(|toast| toast.message.as_str()),
        Some("Haarness finished: Background")
    );
}

#[tokio::test]
async fn background_root_completion_notifies_after_activity_snapshot_race() {
    let current = fixture_thread();
    let background = Thread {
        title: "Background".into(),
        ..fixture_thread()
    };
    let background_turn = fixture_running_turn(background.id);
    let mut app = App::new(
        current.clone(),
        vec![current.clone(), background.clone()],
        vec![],
        vec![],
        vec![],
    );

    // A REST active-turn refresh can observe the terminal state before the
    // matching SSE completion reaches the reducer. The finish event remains
    // authoritative and must still notify even though the activity cache was
    // already pruned by that snapshot.
    app.hydrate_active_turns(Vec::new());
    let mut finished = fixture_event(
        "turn.finished",
        background.id,
        background.id,
        serde_json::json!({"status": "completed"}),
    );
    finished.turn_id = Some(background_turn.id);
    app.reduce(finished, &offline_client()).await.unwrap();
    app.release_completions(std::time::Instant::now() + crate::state::COMPLETION_GRACE);

    assert_eq!(app.thread.id, current.id);
    assert_eq!(
        app.notification_toast
            .as_ref()
            .map(|toast| toast.message.as_str()),
        Some("Haarness finished: Background")
    );
}

#[tokio::test]
async fn turn_status_drives_the_status_line() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.active_turn = Some(haarness_protocol::TurnId::new());
    app.turn_started = Some(std::time::Instant::now());
    let client = offline_client();
    app.reduce(
        fixture_event(
            "turn.status",
            thread.id,
            thread.id,
            serde_json::json!({
                "state": "tool",
                "tool": "bash",
                "callId": "c1",
                "startedAt": Utc::now(),
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    let text: String = app
        .lines((100, 30))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(
        text.contains("bash") && !text.contains("ctrl+c"),
        "status line names the running tool and does not nag about stopping: {text}"
    );
    // The first ctrl+c arms the interrupt; only then does the band explain
    // the second press.
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &offline_client(),
    )
    .await
    .unwrap();
    let text: String = app
        .lines((100, 30))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(text.contains("ctrl+c again to interrupt"), "{text}");
}

#[tokio::test]
async fn concurrent_children_and_serial_parent_tools_have_distinct_statuses() {
    let thread = fixture_thread();
    let child_thread = ThreadId::new();
    let child_turn = TurnId::new();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "call-bash-current".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "find crates -type f"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolCall {
                call_id: "call-read-queued".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": "Cargo.toml"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::ToolCall {
                call_id: "call-child-background".into(),
                name: "task".into(),
                arguments: serde_json::json!({"prompt": "audit dependencies"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::TaskLink {
                call_id: "call-child-background".into(),
                child_thread_id: child_thread,
            },
        ),
        message_item(
            5,
            &thread,
            ItemData::TaskState {
                call_id: "call-child-background".into(),
                child_thread_id: child_thread,
                child_turn_id: child_turn,
                state: "background".into(),
            },
        ),
    ];
    let mut app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    app.active_turn = Some(TurnId::new());
    app.turn_started = Some(std::time::Instant::now());
    let client = offline_client();
    app.reduce(
        fixture_event(
            "turn.status",
            thread.id,
            thread.id,
            serde_json::json!({
                "state": "tool",
                "tool": "bash",
                "callId": "call-bash-current",
                "startedAt": Utc::now(),
            }),
        ),
        &client,
    )
    .await
    .unwrap();

    let rows = app
        .lines((120, 40))
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
    let current = rows
        .iter()
        .find(|line| line.contains("bash find crates"))
        .expect("current bash row");
    let queued = rows
        .iter()
        .find(|line| line.contains("read Cargo.toml"))
        .expect("queued read row");
    assert!(!current.contains("queued"), "{current}");
    assert!(
        queued.contains("○") && queued.contains("queued"),
        "{queued}"
    );
    assert!(
        rows.iter().any(|line| line.contains("bash + 1 agent")),
        "the footer must include parent and child concurrency: {rows:?}"
    );
}

#[test]
fn queued_messages_use_transient_running_status_instead_of_a_transcript_badge() {
    let thread = fixture_thread();
    let queued = message_item(
        1,
        &thread,
        ItemData::UserMessage {
            text: "also add tests".into(),
            attachments: vec![],
            queued: true,
            synthetic: false,
        },
    );
    let queued_id = queued.id;
    let mut app = App::new(thread.clone(), vec![thread], vec![queued], vec![], vec![]);
    app.active_turn = Some(haarness_protocol::TurnId::new());
    let text: String = app
        .lines((100, 30))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(!text.contains("QUEUED"));
    assert!(text.contains("follow-up steer"));
    assert!(text.contains("also add tests"));

    app.items.push(message_item(
        2,
        &app.thread,
        ItemData::QueueConsumed { item_id: queued_id },
    ));
    let delivered: String = app
        .lines((100, 30))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(!delivered.contains("QUEUED"));
    assert!(!delivered.contains("follow-up steer"));
}

#[test]
fn synthetic_workflow_delivery_stays_out_of_transcript_export_and_queue_status() {
    let thread = fixture_thread();
    let synthetic = message_item(
        1,
        &thread,
        ItemData::UserMessage {
            text: "<workflow>internal result</workflow>".into(),
            attachments: Vec::new(),
            queued: true,
            synthetic: true,
        },
    );
    let mut app = App::new(
        thread.clone(),
        vec![thread],
        vec![synthetic],
        vec![],
        vec![],
    );
    app.active_turn = Some(haarness_protocol::TurnId::new());
    let text: String = app
        .lines((100, 30))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();

    assert!(!text.contains("internal result"), "{text}");
    assert!(!text.contains("follow-up steer"), "{text}");
    assert!(!app.transcript_markdown().contains("internal result"));
}

#[tokio::test]
async fn synthetic_queued_event_does_not_show_user_followup_notice() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.notice = "Working".into();
    app.reduce(
        fixture_event(
            "turn.message_queued",
            thread.id,
            thread.id,
            serde_json::json!({"synthetic": true}),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.notice, "Working");

    app.reduce(
        fixture_event(
            "turn.message_queued",
            thread.id,
            thread.id,
            serde_json::json!({"synthetic": false}),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(app.notice.contains("Queued"));
}

#[test]
fn queued_status_yields_before_the_interrupt_shortcut_at_narrow_widths() {
    let thread = fixture_thread();
    let queued = message_item(
        1,
        &thread,
        ItemData::UserMessage {
            text: "also add tests".into(),
            attachments: vec![],
            queued: true,
            synthetic: false,
        },
    );
    let mut app = App::new(thread.clone(), vec![thread], vec![queued], vec![], vec![]);
    app.active_turn = Some(haarness_protocol::TurnId::new());

    // Identity is 23 columns (" build   openai/GPT-5.4"); the right side is
    // "thinking   0s" (13), "… steer" (21) or "… follow-up steer" (31), and a
    // variant fits when identity + variant + 3 <= width. No interrupt hint
    // competes for the room any more.
    for (width, expected_queue, rejected_queue) in [
        (40, "", "steer"),
        (52, "steer", "follow-up steer"),
        (100, "follow-up steer", ""),
    ] {
        let lines = app.lines((width, 30));
        let status = lines
            .iter()
            .find(|line| line_text(line).starts_with(" build "))
            .unwrap_or_else(|| panic!("{width}-column band is missing"));
        assert!(
            status.width() <= usize::try_from(width).unwrap(),
            "{width}-column status overflowed: {}",
            line_text(status)
        );
        if !expected_queue.is_empty() {
            assert!(
                line_text(status).contains(expected_queue),
                "{width}-column status did not preserve {expected_queue:?}: {}",
                line_text(status)
            );
        }
        if !rejected_queue.is_empty() {
            assert!(
                !line_text(status).contains(rejected_queue),
                "{width}-column status retained lower-priority queue text: {}",
                line_text(status)
            );
        }
    }
}

#[test]
fn long_composer_input_wraps_instead_of_truncating() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let long: String = (0..30).fold(String::new(), |mut acc, index| {
        use std::fmt::Write as _;
        let _ = write!(acc, "word{index} ");
        acc
    });
    app.composer.set_text(long.trim_end().into());
    let text: String = app
        .lines((60, 30))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(
        text.contains("word0") && text.contains("word29"),
        "both ends of a long prompt stay visible: {text}"
    );
    assert!(!text.contains("word0 word1 word2 word3 word4 word5 word6 word7 word8 word9 word10"));
}

// ---------------------------------------------------------------------------
// P2: mouse hit targets, sidebar, expand/collapse, selection.
// ---------------------------------------------------------------------------

#[test]
fn provider_failure_text_wraps_instead_of_leaking_past_the_chat_pane() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::Error {
            message: "Provider rejected the request (cyber_policy): Rephrase the request or use an account authorized for this security work.".into(),
            retryable: false,
        },
    )];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let lines = crate::view::transcript::transcript_lines(&app, 48).lines;
    assert!(lines.len() >= 3, "long failures should wrap: {lines:?}");
    assert!(
        lines.iter().all(|line| line.width() <= 48),
        "provider failure overflowed its pane: {lines:?}"
    );
    assert!(line_text(&lines[0]).starts_with("  × Provider rejected"));
    assert!(line_text(&lines[1]).starts_with("    "));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mouse_hover_paints_chrome_but_keeps_transcript_rows_quiet() {
    let thread = fixture_thread();
    let other = Thread {
        title: "Recent hover target".into(),
        ..fixture_thread()
    };
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "read-hover".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": "README.md"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolResult {
                call_id: "read-hover".into(),
                name: "read".into(),
                output: (1..=12)
                    .map(|row| format!("result row {row}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::Question {
                question_id: "question-hover".into(),
                prompt: "Choose one".into(),
                choices: vec!["alpha".into(), "beta".into()],
                multiple: false,
                custom: true,
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::PermissionRequest {
                permission_id: "permission-hover".into(),
                operation: "bash:echo hover".into(),
                detail: serde_json::json!({"command": "echo hover"}),
                permission: "bash".into(),
                patterns: vec!["echo hover".into()],
                always_patterns: vec![],
            },
        ),
    ];
    let result_id = items[1].id;
    let mut app = App::new(
        thread.clone(),
        vec![thread.clone(), other],
        items,
        vec![],
        vec![],
    );
    app.tool_details = true;
    app.pending_questions
        .push(haarness_protocol::PendingQuestionInfo {
            question_id: "question-hover".into(),
            prompt: "Choose one".into(),
            choices: vec!["alpha".into(), "beta".into()],
            multiple: false,
            custom: true,
            thread_id: thread.id,
            root_thread_id: Some(thread.id),
            turn_id: None,
            created_at: Utc::now(),
        });
    app.pending_permissions
        .push(haarness_protocol::PendingPermissionInfo {
            allow_always: None,
            repository_scope: None,
            permission_id: "permission-hover".into(),
            operation: "bash:echo hover".into(),
            detail: serde_json::json!({"command": "echo hover"}),
            permission: "bash".into(),
            patterns: vec!["echo hover".into()],
            always_patterns: vec![],
            thread_id: thread.id,
            root_thread_id: Some(thread.id),
            turn_id: None,
            agent: None,
            created_at: Utc::now(),
        });
    app.children.insert(
        "task-hover".into(),
        crate::state::ChildTask {
            child_thread_id: ThreadId::new(),
            child_turn_id: None,
            invoked_turn_id: None,
            state: "running".into(),
            tool_calls: 1,
            last_tool: Some("read".into()),
            started_at: Some(Utc::now()),
        },
    );
    let client = offline_client();
    let _ = app.lines((140, 60));

    let families: [(&str, HitPredicate, bool); 6] = [
        (
            "tool result",
            |target| matches!(target, crate::view::frame::HitTarget::ToolToggle(_)),
            false,
        ),
        (
            "child task",
            |target| matches!(target, crate::view::frame::HitTarget::TaskOpen(_)),
            true,
        ),
        (
            "thread",
            |target| matches!(target, crate::view::frame::HitTarget::ThreadOpen(_)),
            true,
        ),
        (
            "permission",
            |target| matches!(target, crate::view::frame::HitTarget::PermissionFocus),
            false,
        ),
        (
            "sidebar section",
            |target| matches!(target, crate::view::frame::HitTarget::SidebarSection(_)),
            true,
        ),
        (
            "question choice",
            |target| matches!(target, crate::view::frame::HitTarget::QuestionChoice(_)),
            false,
        ),
    ];
    for (family, predicate, paint_hover) in families {
        let hit = app
            .hit_rows
            .borrow()
            .iter()
            .find(|hit| predicate(&hit.target))
            .cloned()
            .unwrap_or_else(|| panic!("missing {family} hit target"));
        assert_eq!(hit.paint_hover, paint_hover, "{family}");
        let column = u16::try_from(hit.start + 1).unwrap();
        let row = u16::try_from(hit.row).unwrap();
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
        assert_eq!(app.hovered_cell, Some((column, row)), "{family}");
        let lines = app.lines((140, 60));
        if paint_hover {
            assert_hit_focus(&lines, &hit, &app, family);
        }
    }

    let task_regions: Vec<_> = app
        .hit_rows
        .borrow()
        .iter()
        .filter(|hit| {
            matches!(&hit.target, crate::view::frame::HitTarget::TaskOpen(call) if call == "task-hover")
        })
        .cloned()
        .collect();
    assert_eq!(
        task_regions.len(),
        2,
        "a sidebar agent should expose one title row and one activity row"
    );
    let hovered_task = &task_regions[0];
    app.hovered_cell = Some((
        u16::try_from(hovered_task.start + 1).unwrap(),
        u16::try_from(hovered_task.row).unwrap(),
    ));
    let lines = app.lines((140, 60));
    for region in &task_regions {
        assert_hit_focus(&lines, region, &app, "whole child task");
    }

    let repeated: Vec<_> = app
        .hit_rows
        .borrow()
        .iter()
        .filter(|hit| {
            matches!(hit.target, crate::view::frame::HitTarget::ToolToggle(id) if id == result_id)
        })
        .cloned()
        .collect();
    assert!(
        repeated.len() >= 2,
        "tool output fixture has repeated targets"
    );
    let hovered = &repeated[0];
    let ordinary = repeated
        .iter()
        .find(|candidate| candidate.row != hovered.row)
        .expect("a second physical tool row");
    app.hovered_cell = Some((
        u16::try_from(hovered.start + 1).unwrap(),
        u16::try_from(hovered.row).unwrap(),
    ));
    let lines = app.lines((140, 60));
    assert!(
        lines[hovered.row]
            .spans
            .iter()
            .all(|span| span.bg != Some(app.theme.primary)),
        "transcript tool output must stay visually quiet on hover"
    );
    assert!(
        lines[ordinary.row]
            .spans
            .iter()
            .all(|span| span.bg != Some(app.theme.primary)),
        "a repeated target on another physical row must not be highlighted"
    );
}

#[test]
fn sidebar_renders_on_wide_terminals_and_collapses_on_narrow() {
    let thread = fixture_thread();
    let mut recent = fixture_thread();
    recent.id = ThreadId::new();
    recent.title = "Recent root".into();
    let mut app = App::new(
        thread.clone(),
        vec![thread.clone(), recent],
        vec![],
        vec![],
        vec![],
    );
    let empty: String = crate::view::sidebar::sidebar_lines(&app, 36, 40)
        .lines
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(empty.contains("▾ usage"));
    assert!(
        !empty.contains("▾ agents") && !empty.contains("none this turn"),
        "an empty agent roster must not consume sidebar space: {empty}"
    );

    app.todos = vec![haarness_protocol::TodoEntry {
        content: "Actionable work".into(),
        status: "in_progress".into(),
        priority: "high".into(),
    }];
    app.mcp_status = vec![haarness_protocol::McpServerInfo {
        name: "fixture-mcp".into(),
        transport: "stdio".into(),
        status: "connected".into(),
        detail: None,
    }];
    app.children.insert(
        "call-1".into(),
        crate::state::ChildTask {
            child_thread_id: ThreadId::new(),
            child_turn_id: None,
            invoked_turn_id: None,
            state: "running".into(),
            tool_calls: 3,
            last_tool: Some("grep".into()),
            started_at: Some(Utc::now()),
        },
    );
    let wide: String = app
        .lines((140, 40))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(wide.contains("▾ agents"), "sidebar section header: {wide}");
    assert!(wide.contains("▾ usage"));
    let usage = wide.find("▾ usage").unwrap();
    let todo = wide.find("▾ todo").unwrap();
    let agents = wide.find("▾ agents").unwrap();
    let mcp = wide.find("▾ mcp").unwrap();
    let recent = wide.find("▾ recent").unwrap();
    assert!(
        usage < todo && todo < agents && agents < mcp && mcp < recent,
        "sidebar priority order is usage, actionable work, integrations, recent: {wide}"
    );
    assert!(wide.contains("ctrl+b hides"));
    // Sidebar rows are clickable: a section header and the task row.
    let hits = app.hit_rows.borrow().clone();
    assert!(
        hits.iter()
            .any(|hit| matches!(hit.target, crate::view::frame::HitTarget::SidebarSection(_))),
        "{hits:?}"
    );
    assert!(
        hits.iter()
            .any(|hit| matches!(hit.target, crate::view::frame::HitTarget::TaskOpen(_)))
    );
    drop(hits);

    let narrow: String = app
        .lines((90, 30))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(!narrow.contains("▾ agents"), "no sidebar under 110 cols");

    app.sidebar_open = false;
    let hidden: String = app
        .lines((140, 40))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(!hidden.contains("▾ agents"), "ctrl+b hides the sidebar");
}

#[tokio::test]
async fn ctrl_b_toggles_sidebar_silently_in_root_and_child_views() {
    let thread = fixture_thread();
    let mut root = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let client = offline_client();
    root.notice = "existing status".into();
    root.lines((140, 40));
    let initial = root.sidebar_open;

    for expected in [!initial, initial] {
        crate::input::handle_key(
            &mut root,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            &client,
        )
        .await
        .unwrap();
        assert_eq!(root.sidebar_open, expected);
        assert_eq!(root.notice, "existing status");
    }

    let (mut child, _, _, _) = child_navigation_fixture();
    child.notice = "child status".into();
    child.lines((140, 40));
    let initial = child.sidebar_open;
    for expected in [!initial, initial] {
        crate::input::handle_key(
            &mut child,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            &client,
        )
        .await
        .unwrap();
        assert_eq!(child.sidebar_open, expected);
        assert_eq!(child.notice, "child status");
    }
}

#[tokio::test]
async fn a_narrow_terminal_keeps_the_plan_in_one_row_and_ctrl_b_opens_the_sidebar_over_it() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::UserMessage {
                text: "plan it".into(),
                attachments: vec![],
                queued: false,
                synthetic: false,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolCall {
                call_id: "todo-1".into(),
                name: "todowrite".into(),
                arguments: serde_json::json!({"todos": []}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::ToolResult {
                call_id: "todo-1".into(),
                name: "todowrite".into(),
                output: "Todo list updated (3 items)".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let entry = |content: &str, status: &str| haarness_protocol::TodoEntry {
        content: content.into(),
        status: status.into(),
        priority: "medium".into(),
    };
    app.todos = vec![
        entry("Read the code", "completed"),
        entry("Make the greeting friendlier", "in_progress"),
        entry("Run the tests", "pending"),
    ];
    let client = offline_client();
    let ctrl_b = || KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
    let frame = |app: &App, width: u32| {
        let lines = app.lines((width, 24));
        for line in &lines {
            assert!(line.width() <= width as usize, "{}", line_text(line));
        }
        lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
    };

    let narrow = frame(&app, 80);
    assert!(
        narrow.contains("[•] Make the greeting friendlier  ·  1/3 done  ·  ctrl+b shows the plan"),
        "{narrow}"
    );
    assert!(!narrow.contains("▾ todo"), "{narrow}");
    assert!(
        app.hit_rows
            .borrow()
            .iter()
            .any(|hit| { hit.target == crate::view::frame::HitTarget::SidebarToggle })
    );

    crate::input::handle_key(&mut app, ctrl_b(), &client)
        .await
        .unwrap();
    let overlay = frame(&app, 80);
    assert!(
        overlay.contains("▾ todo") && overlay.contains("Run the tests"),
        "{overlay}"
    );
    assert!(
        overlay.contains("plan it"),
        "the conversation stays beside it: {overlay}"
    );
    assert!(!overlay.contains("ctrl+b shows the plan"), "{overlay}");

    crate::input::handle_key(&mut app, ctrl_b(), &client)
        .await
        .unwrap();
    let hidden = frame(&app, 80);
    assert!(!hidden.contains("▾ todo"), "{hidden}");
    assert!(hidden.contains("ctrl+b shows the plan"), "{hidden}");

    // Wide enough to dock, the sidebar holds the plan and the row goes away;
    // hidden there, the row comes back.
    app.sidebar_open = true;
    let docked = frame(&app, 140);
    assert!(docked.contains("▾ todo"), "{docked}");
    assert!(!docked.contains("ctrl+b shows the plan"), "{docked}");
    crate::input::handle_key(&mut app, ctrl_b(), &client)
        .await
        .unwrap();
    let undocked = frame(&app, 140);
    assert!(!undocked.contains("▾ todo"), "{undocked}");
    assert!(undocked.contains("ctrl+b shows the plan"), "{undocked}");

    // A finished plan leaves no row behind.
    for todo in &mut app.todos {
        todo.status = "completed".into();
    }
    assert!(!frame(&app, 80).contains("done  ·"));
}

#[test]
fn terminal_background_mode_adapts_both_light_and_dark_themes_without_flattening_overlays() {
    let theme = crate::theme::Theme::editorial_dark();
    let limited = haarness_opentui::ColorSupport {
        truecolor: false,
        ansi256: true,
    };
    let truecolor = haarness_opentui::ColorSupport {
        truecolor: true,
        ansi256: true,
    };

    assert_eq!(
        crate::page_background(theme.bg, crate::config::BackgroundMode::Auto, limited),
        haarness_opentui::Color::TRANSPARENT
    );
    assert_eq!(
        crate::page_background(theme.bg, crate::config::BackgroundMode::Auto, truecolor,),
        theme.bg
    );
    assert_eq!(
        crate::page_background(theme.bg, crate::config::BackgroundMode::Terminal, truecolor,),
        haarness_opentui::Color::TRANSPARENT
    );
    assert_eq!(
        crate::page_background(theme.bg, crate::config::BackgroundMode::Theme, limited),
        theme.bg
    );
    assert!(crate::uses_terminal_background(
        crate::config::BackgroundMode::Auto,
        limited
    ));
    assert!(!crate::uses_terminal_background(
        crate::config::BackgroundMode::Auto,
        truecolor
    ));
    assert_eq!(
        crate::initial_theme_variant(true, Some("light"), Some("dark")),
        "light",
        "the measured terminal must override a stale saved picker variant"
    );
    assert_eq!(
        crate::initial_theme_variant(false, Some("light"), Some("dark")),
        "dark",
        "a painted theme keeps the explicit saved variant"
    );

    for (variant, expected_text, original_surface) in [
        (
            "dark",
            Theme::editorial_dark().text,
            Theme::editorial_dark().surface,
        ),
        (
            "light",
            Theme::editorial_light().text,
            Theme::editorial_light().surface,
        ),
    ] {
        let thread = fixture_thread();
        let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
        assert!(app.apply_theme("editorial", variant));
        app.set_terminal_background(true);
        assert_eq!(app.theme.text, expected_text);
        assert_eq!(app.theme.surface, haarness_opentui::Color::TRANSPARENT);
        assert_eq!(app.theme.overlay, original_surface);
        assert_ne!(app.theme.selected, haarness_opentui::Color::TRANSPARENT);
        assert_ne!(
            app.theme.diff.added_bg,
            haarness_opentui::Color::TRANSPARENT
        );

        let base = app.lines((140, 32));
        assert!(
            base.iter()
                .flat_map(|line| &line.spans)
                .any(|span| { span.bg == Some(haarness_opentui::Color::TRANSPARENT) })
        );
        assert!(
            base.iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.bg != Some(original_surface))
        );

        app.open_overlay(crate::state::Overlay::Themes);
        let overlay = app.lines((140, 32));
        assert!(
            overlay
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.bg == Some(original_surface))
        );
    }
}

#[tokio::test]
async fn todo_events_drive_wrapped_sidebar_state_and_completed_lists_hide() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let client = offline_client();
    app.reduce(
        fixture_event(
            "todo.updated",
            thread.id,
            thread.id,
            serde_json::json!({
                "threadId": thread.id,
                "todos": [{
                    "content": "A long current task whose final sentinel must word wrap visibly in the sidebar WRAP_SENTINEL",
                    "status": "in_progress",
                    "priority": "high"
                }]
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.todos.len(), 1);
    let visible = app
        .lines((140, 40))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(visible.contains("▾ todo"), "{visible}");
    assert!(visible.contains("WRAP_SENTINEL"), "{visible}");

    app.reduce(
        fixture_event(
            "todo.updated",
            thread.id,
            thread.id,
            serde_json::json!({
                "threadId": thread.id,
                "todos": [{
                    "content": "finished",
                    "status": "completed",
                    "priority": "medium"
                }]
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    let completed = app
        .lines((140, 40))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!completed.contains("▾ todo"), "{completed}");

    app.todos[0].status = "cancelled".into();
    let cancelled = app
        .lines((140, 40))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !cancelled.contains("▾ todo"),
        "cancelled-only lists are terminal: {cancelled}"
    );
}

#[test]
fn todo_updates_render_only_in_sidebar() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "todo-call".into(),
                name: "todo".into(),
                arguments: serde_json::json!({"items":[
                    {"content":"inspect paths","status":"in_progress","priority":"high"},
                    {"content":"ship fix","status":"pending","priority":"medium"}
                ]}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::Todo {
                items: vec![
                    haarness_protocol::TodoEntry {
                        content: "inspect paths".into(),
                        status: "in_progress".into(),
                        priority: "high".into(),
                    },
                    haarness_protocol::TodoEntry {
                        content: "ship fix".into(),
                        status: "pending".into(),
                        priority: "medium".into(),
                    },
                ],
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::ToolResult {
                call_id: "todo-call".into(),
                name: "todo".into(),
                output: "Todo list updated (2 items)".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let transcript = crate::view::transcript::transcript_lines(&app, 100);
    assert!(transcript.lines.is_empty());
    assert_eq!(app.todos.len(), 2);
    let sidebar = crate::view::sidebar::sidebar_lines(&app, 40, 40);
    let text = sidebar
        .lines
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("inspect paths") && text.contains("ship fix"),
        "{text}"
    );
}

#[test]
fn todo_tool_activity_is_hidden_before_and_without_a_snapshot() {
    for name in ["todo", "todowrite", "todoread"] {
        let thread = fixture_thread();
        let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
        let call = message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "todo-call".into(),
                name: name.into(),
                arguments: serde_json::json!({"items":[]}),
                provider_metadata: serde_json::Value::Null,
            },
        );
        let result = message_item(
            2,
            &thread,
            ItemData::ToolResult {
                call_id: "todo-call".into(),
                name: name.into(),
                output: "Todo list updated".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        );
        for items in [vec![call.clone()], vec![call, result.clone()], vec![result]] {
            app.items = items;
            assert!(
                crate::view::transcript::transcript_lines(&app, 100)
                    .lines
                    .is_empty(),
                "{name}"
            );
        }
        app.items.push(message_item(
            3,
            &thread,
            ItemData::AssistantMessage {
                text: "The todo list is ready.".into(),
            },
        ));
        assert!(
            crate::view::transcript::transcript_lines(&app, 100)
                .lines
                .iter()
                .any(|row| row.text().contains("The todo list is ready."))
        );
    }
}

#[test]
fn todo_transcript_fallback_respects_a_pending_revert_boundary() {
    let mut thread = fixture_thread();
    thread.revert = Some(haarness_protocol::RevertState {
        sequence: 1,
        snapshot_id: "backup".into(),
        base_snapshot_id: None,
    });
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::Todo {
                items: vec![haarness_protocol::TodoEntry {
                    content: "kept".into(),
                    status: "in_progress".into(),
                    priority: "high".into(),
                }],
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::Todo {
                items: vec![haarness_protocol::TodoEntry {
                    content: "future".into(),
                    status: "pending".into(),
                    priority: "low".into(),
                }],
            },
        ),
    ];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    assert_eq!(app.todos[0].content, "kept");
    app.todos.clear();
    app.hydrate_todos_from_items();
    assert_eq!(app.todos[0].content, "kept");
}

#[test]
fn sidebar_is_one_fixed_continuous_background_panel() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "A deliberately wide transcript row that must never move the sidebar divider"
                .repeat(3),
        },
    )];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let width = 140_u32;
    let width_usize = usize::try_from(width).unwrap();
    let main_width = width_usize - app.sidebar_width(width_usize);
    let lines = app.lines((width, 40));
    let has_panel = |line: &Line| {
        cell_style(line, main_width).is_some_and(|(_, bg, _)| bg == Some(app.theme.surface))
    };
    let panel_rows = lines[2..].iter().take_while(|line| has_panel(line)).count();
    assert!(panel_rows > 0, "sidebar panel rendered");
    let last = panel_rows + 1;
    assert!(last > 20, "panel fills the pane rather than its content");
    assert!(
        lines[2..=last].iter().all(has_panel),
        "panel background never breaks or shifts: {:?}",
        lines.iter().map(Line::text).collect::<Vec<_>>()
    );
    for (row, line) in lines[2..=last].iter().enumerate() {
        assert_ne!(
            line.text().chars().nth(main_width),
            Some('│'),
            "the old glyph divider must be gone on row {}",
            row + 2
        );
        for column in main_width..width_usize {
            let (_, background, _) = cell_style(line, column)
                .unwrap_or_else(|| panic!("missing sidebar cell at row {}, col {column}", row + 2));
            assert_eq!(
                background,
                Some(app.theme.surface),
                "sidebar is not a solid rectangle at row {}, col {column}",
                row + 2
            );
        }
    }
}

#[tokio::test]
async fn completed_child_stays_navigable_after_parent_turn_finishes() {
    let root = fixture_thread();
    let child = Thread {
        id: ThreadId::new(),
        parent_id: Some(root.id),
        is_subagent: true,
        fork_origin: None,
        title: "Completed child".into(),
        ..fixture_thread()
    };
    let items = vec![
        message_item(
            1,
            &root,
            ItemData::ToolCall {
                call_id: "task-1".into(),
                name: "task".into(),
                arguments: serde_json::json!({"prompt": "inspect it"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &root,
            ItemData::TaskLink {
                call_id: "task-1".into(),
                child_thread_id: child.id,
            },
        ),
        message_item(
            3,
            &root,
            ItemData::ToolResult {
                call_id: "task-1".into(),
                name: "task".into(),
                output: "done".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ];
    let mut app = App::new(
        root.clone(),
        vec![root.clone(), child.clone()],
        items,
        vec![],
        vec![],
    );
    let client = offline_client();
    app.reduce(
        fixture_event("turn.finished", root.id, root.id, serde_json::json!({})),
        &client,
    )
    .await
    .unwrap();

    assert_eq!(
        app.children.get("task-1").map(|task| task.child_thread_id),
        Some(child.id),
        "turn completion preserves the durable child link"
    );
    let _ = app.lines((120, 40));
    assert!(
        app.hit_rows.borrow().iter().any(|hit| {
            matches!(&hit.target, crate::view::frame::HitTarget::TaskOpen(call) if call == "task-1")
        }),
        "completed task row keeps its click target"
    );
}

#[test]
fn tool_output_expands_via_hit_target() {
    let thread = fixture_thread();
    let output: String = (1..=30).fold(String::new(), |mut acc, index| {
        use std::fmt::Write as _;
        let _ = writeln!(acc, "row-{index}");
        acc
    });
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "c1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "seq 30"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolResult {
                call_id: "c1".into(),
                name: "bash".into(),
                output,
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ];
    let item_id = items[1].id;
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.sidebar_open = false;
    let collapsed: String = app
        .lines((100, 45))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(
        !collapsed.contains("30 lines"),
        "no line count: {collapsed}"
    );
    assert!(!collapsed.contains("row-1"), "no head: {collapsed}");
    // The rows carry a ToolToggle target.
    let toggle_row = app
        .hit_rows
        .borrow()
        .iter()
        .find_map(|hit| {
            matches!(hit.target, crate::view::frame::HitTarget::ToolToggle(id) if id == item_id)
                .then_some(hit.row)
        })
        .expect("toggle target present");
    assert_eq!(
        app.hit_at(toggle_row, 4),
        Some(crate::view::frame::HitTarget::ToolToggle(item_id))
    );

    app.expanded.insert(item_id);
    let expanded: String = app
        .lines((100, 45))
        .iter()
        .map(|line| line_text(line) + "\n")
        .collect();
    assert!(expanded.contains("row-30"), "expanded shows everything");
    assert!(expanded.contains("click to collapse"));
}

#[test]
fn workflow_tool_row_names_the_run_and_opens_its_durable_record() {
    let thread = fixture_thread();
    let run_id = haarness_protocol::WorkflowId::new();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::ToolCall {
            call_id: "workflow-wait".into(),
            name: "workflow".into(),
            arguments: serde_json::json!({
                "action": "wait",
                "runID": run_id,
                "timeout": 900_000
            }),
            provider_metadata: serde_json::Value::Null,
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.sidebar_open = false;
    let rendered = app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("workflow wait"), "{rendered}");
    assert!(
        rendered.contains(crate::view::span::short_id(&run_id.to_string())),
        "{rendered}"
    );
    assert!(app.hit_rows.borrow().iter().any(|hit| {
        matches!(
            hit.target,
            crate::view::frame::HitTarget::WorkflowOpen(id) if id == run_id
        )
    }));
}

#[tokio::test]
async fn compact_tool_call_row_toggles_open_and_closed() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "read-1".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": "README.md"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolResult {
                call_id: "read-1".into(),
                name: "read".into(),
                output: "first\nsecond".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ];
    let result = items[1].id;
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.sidebar_open = false;
    let client = offline_client();

    let collapsed = app.lines((100, 30));
    let row = collapsed
        .iter()
        .position(|line| line.text().contains("read README.md"))
        .unwrap();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column: 4,
                row: u16::try_from(row).unwrap(),
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
    }
    assert!(app.expanded.contains(&result), "first click expands");

    let expanded = app.lines((100, 30));
    let row = expanded
        .iter()
        .position(|line| line.text().contains("read README.md"))
        .unwrap();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column: 4,
                row: u16::try_from(row).unwrap(),
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
    }
    assert!(
        !app.expanded.contains(&result),
        "clicking the same call row again collapses it"
    );
}

#[tokio::test]
async fn mouse_hit_testing_respects_sidebar_columns() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "read-1".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": "README.md"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolResult {
                call_id: "read-1".into(),
                name: "read".into(),
                output: "body".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let client = offline_client();
    let lines = app.lines((140, 40));
    let row = lines
        .iter()
        .position(|line| line.text().contains("▾ usage"))
        .expect("sidebar usage section rendered");

    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column: 102,
                row: u16::try_from(row).unwrap(),
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
    }

    assert!(
        app.sidebar_collapsed
            .contains(&crate::view::sidebar::SECTION_CONTEXT),
        "a click in the sidebar must not activate a transcript row sharing the same screen row"
    );
}

#[test]
fn menu_rows_carry_click_targets() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.composer.set_text("/mo".into());
    let _ = app.lines((100, 30));
    let hits = app.hit_rows.borrow();
    assert!(
        hits.iter()
            .any(|hit| matches!(hit.target, crate::view::frame::HitTarget::MenuRow(_))),
        "{hits:?}"
    );
}

#[test]
fn selection_extracts_and_paints_the_dragged_rows() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "alpha beta gamma".into(),
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.sidebar_open = false;
    let lines = app.lines((80, 20));
    let row = u16::try_from(
        lines
            .iter()
            .position(|line| line_text(line).contains("alpha beta gamma"))
            .expect("message row"),
    )
    .unwrap();
    let text = line_text(&lines[row as usize]);
    let column = u16::try_from(text[..text.find("alpha").unwrap()].chars().count()).unwrap();
    app.selection_range = Some(((column, row), (column + 9, row)));
    assert_eq!(app.selected_text(), "alpha beta");
    // Re-render paints the selection background over those cells.
    let painted = app.lines((80, 20));
    let selected = app.theme.primary;
    assert!(
        painted[row as usize]
            .spans
            .iter()
            .any(|span| span.bg == Some(selected) && span.text.contains("alpha")),
        "selection painted"
    );
}

#[test]
fn selection_uses_display_cells_for_wide_glyphs() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::AssistantMessage {
            text: "A界🙂Z".into(),
        },
    )];
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    app.sidebar_open = false;
    let lines = app.lines((80, 20));
    let row = lines
        .iter()
        .position(|line| line_text(line).contains("A界🙂Z"))
        .expect("message row");
    let text = line_text(&lines[row]);
    let prefix = u16::try_from(text.split('A').next().unwrap().width()).expect("small frame");
    let row = u16::try_from(row).expect("small frame");
    app.selection_range = Some(((prefix + 2, row), (prefix + 4, row)));
    assert_eq!(app.selected_text(), "界🙂");
}

// ---------------------------------------------------------------------------
// Shared interaction surfaces: dialogs, toasts, shortcuts,
// truthful disclosure affordances, and display-cell-correct Unicode input.
// ---------------------------------------------------------------------------

#[test]
fn compact_terminal_keeps_the_explicit_dialog_visible() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Models);

    let lines = app.lines((39, 11));
    let text = lines.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert!(
        text.contains("Select model"),
        "compact dialog vanished: {text}"
    );
    assert!(
        text.contains("Type to filter"),
        "compact dialog lost its filter affordance: {text}"
    );
    assert!(
        app.hit_rows
            .borrow()
            .iter()
            .any(|hit| matches!(hit.target, crate::view::frame::HitTarget::MenuRow(_))),
        "compact dialog must retain selectable rows"
    );
    assert!(
        text.contains('━'),
        "compact dialog lost its accent cap: {text}"
    );
    let surface = app.theme.overlay;
    assert!(
        lines
            .iter()
            .filter(|line| line_text(line).contains("Select model"))
            .all(|line| line.spans.iter().any(|span| span.bg == Some(surface))),
        "the card stays a raised panel at compact sizes: {text}"
    );
    assert!(
        lines.iter().all(|line| line.width() <= 39),
        "compact modal rendered past the terminal width"
    );
}

/// Arabic dialog hints such as "esc للإغلاق" hold lam-alef pairs, which
/// `unicode-width` counts as one cell each while the renderer draws two: the
/// border placed after the hint by that width ate the word's last letters.
#[test]
fn arabic_dialog_hints_are_drawn_whole_at_80_and_120_columns() {
    let mut app = selector_fixture();
    app.language = crate::locale::Language::Arabic;
    app.open_overlay(crate::state::Overlay::Agents);
    let hint = app.language.text("esc close").to_owned();
    assert!(
        hint.contains('\u{644}') && hint.contains('\u{627}'),
        "{hint}"
    );
    for width in [80_u32, 120] {
        let lines = app.lines((width, 30));
        assert!(
            lines.iter().any(|line| line.text().contains(&hint)),
            "{width}: the dialog shows no close hint"
        );
        let mut renderer = haarness_opentui::Renderer::new(width, 30, true).unwrap();
        let drawn = renderer.snapshot_lines(&lines);
        assert!(drawn.contains(&hint), "{width}:\n{drawn}");
        for (line, row) in lines.iter().zip(drawn.lines()) {
            assert!(line.width() <= width as usize, "{width}: {}", line.text());
            // What the view measured is what the renderer laid out.
            assert_eq!(
                row.trim_end().width(),
                line.text().trim_end().width(),
                "{width}: {row}"
            );
        }
    }
}

#[test]
fn explicit_dialogs_share_a_bordered_padded_card_and_panels_have_no_search() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Agents);
    let selector = app.lines((100, 30));
    let selector_text = selector
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        selector_text.contains("Select agent") && selector_text.contains("esc close"),
        "selector lacks its title row: {selector_text}"
    );
    let cap = selector
        .iter()
        .find(|line| line_text(line).contains('━'))
        .expect("accent cap");
    assert!(
        cap.spans
            .iter()
            .any(|span| span.fg == app.theme.primary && span.text.starts_with('━')),
        "the card is capped in the brand color, not drawn as a box"
    );
    assert!(
        !selector_text
            .chars()
            .any(|character| matches!(character, '╭' | '╮' | '╰' | '╯' | '│' | '├' | '┤')),
        "no box-drawing chrome survives in a dialog: {selector_text}"
    );
    assert!(
        selector_text.contains("  Type to filter"),
        "{selector_text}"
    );
    // Typing swaps the invitation for the live query.
    app.overlay_filter = "pla".into();
    let filtered = app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(filtered.contains("Search  pla"), "{filtered}");
    app.overlay_filter.clear();

    app.open_overlay(crate::state::Overlay::Panel {
        title: "Plugins".into(),
        lines: vec!["No plugins are installed".into()],
    });
    let panel_text = app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(panel_text.contains("Plugins"), "{panel_text}");
    assert!(panel_text.contains("  No plugins"), "{panel_text}");
    assert!(
        !panel_text.contains("Search") && !panel_text.contains("Type to filter"),
        "read-only panels must not imply filtering: {panel_text}"
    );
}

#[tokio::test]
async fn selector_secondary_rows_share_mouse_focus_and_activation() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Commands);
    let model_index = app
        .filtered_slash_entries("")
        .iter()
        .position(|entry| entry.name == "models")
        .expect("/models command");
    // Narrow enough that descriptions get their own line; the two rows are
    // still one choice for hover and for clicks.
    let _ = app.lines((46, 30));
    let rows = app
        .hit_rows
        .borrow()
        .iter()
        .filter(|hit| {
            matches!(hit.target, crate::view::frame::HitTarget::MenuRow(index) if index == model_index)
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        rows.len(),
        2,
        "normal-width primary and secondary rows must share one target"
    );
    let detail = &rows[1];
    let client = offline_client();
    crate::input::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: u16::try_from(detail.start + 1).unwrap(),
            row: u16::try_from(detail.row).unwrap(),
            modifiers: KeyModifiers::NONE,
        },
        &client,
    )
    .await
    .unwrap();
    assert_eq!(
        app.selection, model_index,
        "hovering metadata focuses its command"
    );
    let focused = app.lines((46, 30));
    for row in &rows {
        assert_hit_focus(
            &focused,
            row,
            &app,
            "every physical row in the focused command item",
        );
    }

    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column: u16::try_from(detail.start + 1).unwrap(),
                row: u16::try_from(detail.row).unwrap(),
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
    }
    assert!(
        matches!(app.overlay, crate::state::Overlay::Models),
        "clicking command metadata activates the same command as its primary row"
    );
}

#[test]
fn selector_dialog_bounds_its_viewport_and_reports_the_visible_item_range() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Options {
        title: "Many choices".into(),
        kind: crate::state::OptionsKind::ReasoningEffort,
        entries: (1..=20)
            .map(|index| crate::state::OptionEntry {
                label: format!("choice-{index}"),
                description: format!("secondary detail for choice {index}"),
                value: format!("value-{index}"),
            })
            .collect(),
    });
    app.selection = 15;
    let lines = app.lines((100, 20));
    let hits = app
        .hit_rows
        .borrow()
        .iter()
        .filter_map(|hit| match &hit.target {
            crate::view::frame::HitTarget::MenuRow(index) => Some(*index),
            _ => None,
        })
        .collect::<Vec<_>>();
    let first = *hits.iter().min().expect("visible option");
    let last = *hits.iter().max().expect("visible option");
    assert!(first <= 15 && last >= 15, "selection left the viewport");
    assert!(last - first + 1 < 20, "the viewport must be bounded");
    let text = lines.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert!(
        text.contains(&format!("{}–{} of 20", first + 1, last + 1)),
        "selector range hint disagrees with rendered rows: {text}"
    );
}

#[test]
fn compact_terminal_keeps_the_toast_visible() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.show_toast(
        Some("Notice".into()),
        "Compact feedback".into(),
        crate::state::ToastVariant::Warning,
        std::time::Duration::from_secs(30),
    );

    let lines = app.lines((39, 11));
    let text = lines.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert!(
        text.contains("Compact feedback"),
        "compact toast vanished: {text}"
    );
    assert!(
        app.hit_rows
            .borrow()
            .iter()
            .any(|hit| matches!(hit.target, crate::view::frame::HitTarget::ToastDismiss)),
        "compact toast must remain dismissible"
    );
}

#[test]
fn model_dialog_marks_the_current_row_and_filters_in_place() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Models);
    let lines = app.lines((100, 30));
    let text = lines.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Select model"), "{text}");
    assert!(
        text.contains("●  GPT-5.4"),
        "current marker missing: {text}"
    );

    let selected = app
        .hit_rows
        .borrow()
        .iter()
        .find(|hit| {
            matches!(hit.target, crate::view::frame::HitTarget::MenuRow(index) if index == app.selection)
        })
        .cloned()
        .expect("selected model row");
    assert_hit_focus(&lines, &selected, &app, "model dialog current row");

    app.overlay_filter = "mini".into();
    app.selection = 0;
    let filtered = app.lines((100, 30));
    let filtered_text = filtered
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(filtered_text.contains("Search  mini▌"), "{filtered_text}");
    assert!(
        filtered_text.contains("openai/gpt-5.4-mini"),
        "{filtered_text}"
    );
    let filtered_hits = app
        .hit_rows
        .borrow()
        .iter()
        .filter(|hit| matches!(hit.target, crate::view::frame::HitTarget::MenuRow(_)))
        .map(|hit| line_text(&filtered[hit.row]))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !filtered_hits.lines().any(|line| {
            line.contains("openai/gpt-5.4") && !line.contains("openai/gpt-5.4-mini")
        }),
        "filter retained a non-matching model row: {filtered_hits}"
    );
}

#[test]
fn model_dialog_rows_do_not_jump_when_catalog_metadata_refreshes() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Models);
    app.overlay_filter = "mini".into();
    app.selection = 0;
    let before = app.lines((100, 30));
    let before_row = before
        .iter()
        .position(|line| line.text().contains("openai/gpt-5.4-mini"))
        .expect("filtered model row");

    let model = app.providers[0]
        .models
        .iter_mut()
        .find(|model| model.id == "gpt-5.4-mini")
        .expect("model metadata");
    model.reasoning = false;
    model.reasoning_efforts.clear();
    model.images = false;
    model.tools = false;
    model.context_window = None;
    model.max_output_tokens = None;
    model.pricing = None;

    let after = app.lines((100, 30));
    let after_row = after
        .iter()
        .position(|line| line.text().contains("openai/gpt-5.4-mini"))
        .expect("refreshed model row");
    assert_eq!(
        after_row, before_row,
        "provider metadata refresh moved the interactive model row"
    );
}

#[tokio::test]
async fn model_picker_hover_scroll_and_keyboard_keep_one_highlighted_choice() {
    let mut app = selector_fixture();
    let template = app.providers[0].models[0].clone();
    app.providers[0].models = (0..80)
        .map(|index| {
            let mut model = template.clone();
            model.id = format!("model-{index:02}");
            model.display_name = format!("Model {index:02}");
            model
        })
        .collect();
    app.open_overlay(crate::state::Overlay::Models);
    app.selection = 35;
    app.lines((120, 32));
    let edge = app
        .hit_rows
        .borrow()
        .iter()
        .rev()
        .find(|hit| matches!(hit.target, crate::view::frame::HitTarget::MenuRow(_)))
        .cloned()
        .unwrap();
    let client = offline_client();
    crate::input::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: u16::try_from(edge.start + 1).unwrap(),
            row: u16::try_from(edge.row).unwrap(),
            modifiers: KeyModifiers::NONE,
        },
        &client,
    )
    .await
    .unwrap();
    for keyboard in [false, true] {
        if keyboard {
            crate::input::handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
                &client,
            )
            .await
            .unwrap();
        }
        let lines = app.lines((120, 32));
        let highlighted: std::collections::BTreeSet<_> = app
            .hit_rows
            .borrow()
            .iter()
            .filter_map(|hit| {
                let crate::view::frame::HitTarget::MenuRow(index) = hit.target else {
                    return None;
                };
                lines[hit.row]
                    .spans
                    .iter()
                    .any(|span| span.bg == Some(app.theme.primary))
                    .then_some(index)
            })
            .collect();
        assert_eq!(
            highlighted,
            std::collections::BTreeSet::from([app.selection]),
            "mouse and keyboard must share one selected row"
        );
    }
}

#[test]
fn cloud_picker_displays_public_prefix_and_omits_connected_status() {
    let mut app = selector_fixture();
    app.providers[0].id = "haarness".into();
    app.thread.provider = "haarness".into();
    app.providers[0].models[0].id = "openai/gpt-6-sol".into();
    app.thread.model = "openai/gpt-6-sol".into();
    app.open_overlay(crate::state::Overlay::Models);
    app.overlay_filter = "haarness-cloud/".into();
    let models = app.filtered_models();
    assert!(!models.is_empty());
    assert!(
        models.iter().all(|id| id.starts_with("haarness/")),
        "stored IDs stay compatible"
    );
    let text = app
        .lines((180, 36))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("haarness-cloud/openai/gpt-6-sol"), "{text}");
    assert!(text.contains("active"), "{text}");
    assert!(!text.contains(" · connected"), "{text}");
}

#[test]
fn panel_dialog_hangs_wrapped_descriptions_below_their_bullet() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.open_overlay(crate::state::Overlay::Panel {
        title: "Plugins".into(),
        lines: vec![
            "●  calendar-tools  personal marketplace  A deliberately long plugin description that must wrap without turning continuation lines into new list bullets"
                .into(),
        ],
    });
    let lines = app.lines((60, 18));
    let text = lines.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert_eq!(
        text.matches('●').count(),
        1,
        "the plugin bullet must remain attached to only its primary row: {text}"
    );
    assert!(text.contains("calendar-tools"), "{text}");
    assert!(
        text.lines()
            .filter(|line| line.contains("description") || line.contains("continuation"))
            .all(|line| !line.contains('●')),
        "wrapped metadata inherited a list bullet: {text}"
    );

    let item_rows = app
        .hit_rows
        .borrow()
        .iter()
        .filter(|hit| matches!(hit.target, crate::view::frame::HitTarget::DialogItem(0)))
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        item_rows.len() >= 3,
        "fixture must render a primary row and wrapped metadata rows"
    );
    let metadata = &item_rows[1];
    app.hovered_cell = Some((
        u16::try_from(metadata.start + 1).unwrap(),
        u16::try_from(metadata.row).unwrap(),
    ));
    let hovered = app.lines((60, 18));
    for row in &item_rows {
        for column in row.start..row.end {
            let (_, background, _) = cell_style(&hovered[row.row], column)
                .unwrap_or_else(|| panic!("missing dialog item cell at {column}"));
            assert_eq!(
                background,
                Some(app.theme.selected),
                "hovering wrapped metadata must raise the entire logical item"
            );
        }
    }
}

#[test]
fn panel_dialog_plain_information_and_card_chrome_do_not_hover() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.open_overlay(crate::state::Overlay::Panel {
        title: "Plugins".into(),
        lines: vec!["No plugins are installed".into()],
    });
    let initial = app.lines((80, 18));
    assert!(
        !app.hit_rows
            .borrow()
            .iter()
            .any(|hit| matches!(hit.target, crate::view::frame::HitTarget::DialogItem(_))),
        "plain informational rows must not advertise item interaction"
    );
    let row = initial
        .iter()
        .position(|line| line.text().contains("No plugins are installed"))
        .expect("informational row");
    let column = initial[row]
        .text()
        .find("No plugins are installed")
        .expect("information column");
    app.hovered_cell = Some((u16::try_from(column).unwrap(), u16::try_from(row).unwrap()));
    let hovered = app.lines((80, 18));
    assert!(
        hovered[row].spans.iter().all(|span| {
            span.bg != Some(app.theme.primary) && span.bg != Some(app.theme.selected)
        }),
        "the dialog click-through shield must remain visually inert"
    );
}

#[tokio::test]
async fn panel_dialog_scroll_keys_are_independent_and_bounded() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.open_overlay(crate::state::Overlay::Panel {
        title: "Long panel".into(),
        lines: (1..=50).map(|index| format!("panel-row-{index}")).collect(),
    });
    let client = offline_client();

    let initial = app.lines((90, 24));
    let limit = app.max_overlay_scroll.get();
    assert!(limit > 0, "fixture must overflow the panel");
    assert_eq!(app.overlay_scroll, 0);
    let visible = 50 - limit;
    let initial_text = initial
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        initial_text.contains(&format!("Rows 1–{visible} of 50")),
        "panel position hint is not derived from its real viewport: {initial_text}"
    );

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.overlay_scroll, limit);
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(
        app.overlay_scroll, limit,
        "panel cannot scroll past its end"
    );
    let bottom = app
        .lines((90, 24))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(bottom.contains("panel-row-50"), "{bottom}");
    assert!(
        bottom.contains(&format!("Rows {}–50 of 50", limit + 1)),
        "bottom position hint is inaccurate: {bottom}"
    );

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(
        app.overlay_scroll, 0,
        "panel cannot scroll before its start"
    );
    assert_eq!(
        app.viewport_offset, 0,
        "modal scrolling never moves transcript scrollback"
    );
}

#[tokio::test]
async fn toast_is_top_right_expires_and_has_a_working_dismiss_target() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.show_toast(
        Some("Saved".into()),
        "Settings updated".into(),
        crate::state::ToastVariant::Success,
        std::time::Duration::from_secs(30),
    );
    let lines = app.lines((100, 30));
    let hit = app
        .hit_rows
        .borrow()
        .iter()
        .find(|hit| matches!(hit.target, crate::view::frame::HitTarget::ToastDismiss))
        .cloned()
        .expect("toast dismiss target");
    assert_eq!(hit.row, 2, "toast is anchored two rows from the top");
    assert_eq!(hit.end, 98, "toast is anchored two cells from the right");
    assert!(
        lines[hit.row]
            .spans
            .iter()
            .any(|span| span.text.starts_with('━') && span.fg == app.theme.success()),
        "toast accent cap missing"
    );

    let client = offline_client();
    let column = u16::try_from(hit.start + 1).unwrap();
    let row = u16::try_from(hit.row).unwrap();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
    }
    assert!(
        app.notification_toast.is_none(),
        "click dismisses the toast"
    );

    app.show_toast(
        None,
        "Already expired".into(),
        crate::state::ToastVariant::Warning,
        std::time::Duration::ZERO,
    );
    app.expire_notification();
    assert!(app.notification_toast.is_none(), "expired toast is removed");
}

#[tokio::test]
async fn ctrl_c_clears_a_draft_once_and_empty_ctrl_c_requires_confirmation_to_quit() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let client = offline_client();
    app.composer.set_text("mistyped prompt".into());

    let action = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(action, crate::input::AppAction::Continue));
    assert!(
        app.composer.is_empty(),
        "one Ctrl+C clears a non-empty draft"
    );
    assert_eq!(app.notice, "Input cleared");
    assert!(
        app.notification_toast.is_none(),
        "clearing a draft is ambient feedback, not a card"
    );
    assert!(
        app.ctrl_c_action.is_none(),
        "clearing input does not arm quit"
    );

    let first = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(first, crate::input::AppAction::Continue));
    assert!(app.ctrl_c_action.is_some());
    assert!(app.notice.contains("again to quit"));

    let second = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(second, crate::input::AppAction::Quit));
}

#[tokio::test]
async fn confirmed_active_interrupt_returns_immediately_and_shows_stopping_state() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let client = offline_client();
    let turn = haarness_protocol::TurnId::new();
    app.active_turn = Some(turn);

    let first = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(first, crate::input::AppAction::Continue));
    assert!(app.notice.contains("again to interrupt"));
    let armed = crate::view::composer::composer_lines(&app, 100)
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(armed.contains("ctrl+c again to interrupt"), "{armed}");

    let second = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        crate::input::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &client,
        ),
    )
    .await
    .expect("the render loop never waits for the interrupt HTTP request")
    .unwrap();
    assert!(matches!(second, crate::input::AppAction::Continue));
    assert_eq!(
        app.phase.as_ref().map(|phase| phase.state.as_str()),
        Some("stopping")
    );
    assert_eq!(app.notice, "Stopping task…");
    assert!(
        app.notification_toast.is_none(),
        "stopping is reported in the status line, not a card"
    );
}

#[tokio::test]
async fn idle_root_with_live_child_stays_working_and_cancels_the_session_not_the_app() {
    let root = fixture_thread();
    let child = Thread {
        parent_id: Some(root.id),
        is_subagent: true,
        fork_origin: None,
        title: "Background audit".into(),
        ..fixture_thread()
    };
    let child_turn = fixture_running_turn(child.id);
    let mut app = App::new(
        root.clone(),
        vec![root.clone(), child.clone()],
        vec![],
        vec![],
        vec![],
    );
    app.hydrate_active_turns(vec![child_turn]);

    assert_eq!(app.active_turn, None, "a child never steals input focus");
    assert_eq!(
        app.root_active_turn, None,
        "the root turn is genuinely idle"
    );
    assert_eq!(app.root_descendant_active_count(), 1);
    assert!(app.session_detail(&root).contains("current · working"));
    let rendered = crate::view::composer::composer_lines(&app, 100)
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("1 child turn running"), "{rendered}");

    let client = offline_client();
    let first = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(first, crate::input::AppAction::Continue));
    assert!(app.notice.contains("again to stop session work"));

    let second = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert!(
        matches!(second, crate::input::AppAction::Continue),
        "session cancellation must not quit the TUI"
    );
    assert_eq!(app.notice, "Stopping session work…");
}

#[test]
fn activity_from_an_unrelated_root_does_not_mark_the_current_session_working() {
    let root = fixture_thread();
    let unrelated_root = fixture_thread();
    let unrelated_child = Thread {
        parent_id: Some(unrelated_root.id),
        is_subagent: true,
        fork_origin: None,
        ..fixture_thread()
    };
    let mut app = App::new(
        root.clone(),
        vec![root.clone(), unrelated_root, unrelated_child.clone()],
        vec![],
        vec![],
        vec![],
    );
    app.hydrate_active_turns(vec![fixture_running_turn(unrelated_child.id)]);

    assert_eq!(app.root_descendant_active_count(), 0);
    assert!(!app.session_detail(&root).contains("working"));
}

#[tokio::test]
async fn ctrl_d_quits_once_from_an_empty_prompt_even_while_active_but_edits_nonempty_input() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let client = offline_client();
    app.active_turn = Some(haarness_protocol::TurnId::new());

    let quit = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(quit, crate::input::AppAction::Quit));

    app.composer.set_text("ab".into());
    app.composer.move_left();
    let edit = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(edit, crate::input::AppAction::Continue));
    assert_eq!(app.composer.text(), "a");
}

#[tokio::test]
async fn suspend_and_composer_history_shortcuts_do_not_overlap() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let client = offline_client();
    app.composer.insert_char('x');

    let suspend = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(suspend, crate::input::AppAction::Suspend));
    assert_eq!(app.composer.text(), "x", "suspend never edits the draft");

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('-'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert!(app.composer.is_empty(), "Ctrl+- is input undo");
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('.'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.composer.text(), "x", "Ctrl+. is input redo");

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::SUPER),
        &client,
    )
    .await
    .unwrap();
    assert!(app.composer.is_empty(), "Cmd+Z is input undo");
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(
            KeyCode::Char('Z'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.composer.text(), "x", "Cmd+Shift+Z is input redo");

    let shifted_control = crate::input::handle_key(
        &mut app,
        KeyEvent::new(
            KeyCode::Char('Z'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(shifted_control, crate::input::AppAction::Continue));
    assert_eq!(app.composer.text(), "x");

    app.open_overlay(crate::state::Overlay::Models);
    let dialog_key = crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(dialog_key, crate::input::AppAction::Continue));
}

#[tokio::test]
async fn escape_hides_autocomplete_without_deleting_its_query() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.composer.set_text("/mod".into());
    assert!(app.slash_suggestions().is_some());

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.composer.text(), "/mod");
    assert!(app.autocomplete_hidden);
    assert!(app.slash_suggestions().is_none());
}

#[tokio::test]
async fn escape_is_non_destructive_in_a_child_agent_view() {
    let root = fixture_thread();
    let child = Thread {
        id: ThreadId::new(),
        title: "Child".into(),
        parent_id: Some(root.id),
        is_subagent: true,
        fork_origin: None,
        ..fixture_thread()
    };
    let mut app = App::new(
        child.clone(),
        vec![root.clone(), child],
        vec![],
        vec![],
        vec![],
    );
    app.parent_stack.push(root.clone());
    app.active_turn = Some(haarness_protocol::TurnId::new());

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(
        app.parent_stack.len(),
        1,
        "Esc does not leave the child pane"
    );
    assert!(
        app.escape_interrupt.is_none(),
        "Esc does not arm a root-turn abort in a child pane"
    );
}

#[tokio::test]
async fn ordinary_forks_remain_editable_and_visible_beside_root_sessions() {
    let root = fixture_thread();
    let fork = Thread {
        title: "Independent fork".into(),
        parent_id: Some(root.id),
        is_subagent: false,
        fork_origin: None,
        ..fixture_thread()
    };
    let child = Thread {
        parent_id: Some(root.id),
        is_subagent: true,
        fork_origin: None,
        ..fixture_thread()
    };
    let mut app = App::new(
        fork.clone(),
        vec![root.clone(), fork.clone(), child.clone()],
        vec![],
        vec![],
        vec![],
    );
    assert!(!app.is_child_view());
    let visible = app.filtered_threads();
    assert!(visible.iter().any(|thread| thread.id == fork.id));
    assert!(visible.iter().any(|thread| thread.id == root.id));
    assert!(!visible.iter().any(|thread| thread.id == child.id));
    assert!(
        app.lines((110, 30))
            .iter()
            .any(|line| line_text(line).contains("Ask anything"))
    );
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.composer.text(), "x");
}

#[tokio::test]
async fn child_agent_view_discards_and_rejects_composer_input() {
    let root = fixture_thread();
    let child = Thread {
        id: ThreadId::new(),
        title: "Child".into(),
        parent_id: Some(root.id),
        is_subagent: true,
        fork_origin: None,
        ..fixture_thread()
    };
    let mut app = App::new(
        child.clone(),
        vec![root.clone(), child],
        vec![],
        vec![],
        vec![],
    );
    app.parent_stack.push(root);
    app.composer.set_text("stale hidden draft".into());
    app.attachments.push("/tmp/stale.png".into());
    app.shell_mode = true;
    let client = offline_client();

    for key in [
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    ] {
        let action = crate::input::handle_key(&mut app, key, &client)
            .await
            .unwrap();
        assert!(matches!(action, crate::input::AppAction::Continue));
        assert!(app.composer.is_empty());
        assert!(app.attachments.is_empty());
        assert!(!app.shell_mode);
    }
}

fn child_navigation_fixture() -> (App, Thread, Thread, Thread) {
    let root = fixture_thread();
    let first = Thread {
        id: ThreadId::new(),
        title: "First child".into(),
        parent_id: Some(root.id),
        is_subagent: true,
        fork_origin: None,
        ..fixture_thread()
    };
    let second = Thread {
        id: ThreadId::new(),
        title: "Second child".into(),
        parent_id: Some(root.id),
        is_subagent: true,
        fork_origin: None,
        ..fixture_thread()
    };
    let items = vec![message_item(
        1,
        &first,
        ItemData::AssistantMessage {
            text: "Child result".into(),
        },
    )];
    let mut app = App::new(
        first.clone(),
        vec![root.clone(), first.clone(), second.clone()],
        items,
        vec![],
        vec![],
    );
    app.parent_stack.push(root.clone());
    for (call_id, thread_id) in [("task-a", first.id), ("task-b", second.id)] {
        app.children.insert(
            call_id.into(),
            crate::state::ChildTask {
                child_thread_id: thread_id,
                child_turn_id: None,
                invoked_turn_id: None,
                state: "done".into(),
                tool_calls: 1,
                last_tool: Some("read".into()),
                started_at: Some(Utc::now()),
            },
        );
    }
    (app, root, first, second)
}

#[tokio::test]
async fn child_header_navigation_is_explicit_hoverable_and_availability_aware() {
    use crate::view::frame::HitTarget;

    let (mut app, _, first, _) = child_navigation_fixture();
    let client = offline_client();
    let _ = app.lines((100, 30));
    let navigation = app
        .hit_rows
        .borrow()
        .iter()
        .filter(|hit| {
            matches!(
                hit.target,
                HitTarget::ChildParent | HitTarget::ChildPrevious | HitTarget::ChildNext
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(navigation.len(), 3, "parent and both sibling directions");
    for (target, label) in [
        (HitTarget::ChildParent, "parent"),
        (HitTarget::ChildPrevious, "previous sibling"),
        (HitTarget::ChildNext, "next sibling"),
    ] {
        let hit = navigation
            .iter()
            .find(|hit| hit.target == target)
            .unwrap_or_else(|| panic!("missing {label} control"));
        let column = u16::try_from(hit.start + 1).unwrap();
        let row = u16::try_from(hit.row).unwrap();
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
        let lines = app.lines((100, 30));
        assert_hit_focus(&lines, hit, &app, label);
    }

    app.children
        .retain(|_, task| task.child_thread_id == first.id);
    app.hovered_cell = None;
    let _ = app.lines((100, 30));
    let hits = app.hit_rows.borrow();
    assert!(
        hits.iter().any(|hit| hit.target == HitTarget::ChildParent),
        "parent remains available"
    );
    assert!(
        !hits
            .iter()
            .any(|hit| matches!(hit.target, HitTarget::ChildPrevious | HitTarget::ChildNext)),
        "a lone child must not advertise unavailable sibling controls"
    );
}

#[tokio::test]
async fn clicking_child_header_navigates_to_a_sibling_and_back_to_the_parent() {
    use crate::view::frame::HitTarget;

    let (mut app, root, first, second) = child_navigation_fixture();
    let shared_title = "!tool bash {\"command\":\"sleep 4\"}";
    app.thread.title = shared_title.into();
    for thread in &mut app.threads {
        if thread.parent_id == Some(root.id) {
            thread.title = shared_title.into();
        }
    }
    app.parent_stack.last_mut().unwrap().title =
        "Echo: Reply with only the requested tmux fixture output".into();

    let lines = app.lines((100, 30));
    let first_header = lines[0].text();
    let first_id = first.id.to_string();
    assert!(
        first_header.contains(crate::view::span::short_id(&first_id)),
        "a truncated breadcrumb must retain the current child's discriminator: {first_header}"
    );
    let next = app
        .hit_rows
        .borrow()
        .iter()
        .find(|hit| hit.target == HitTarget::ChildNext)
        .cloned()
        .expect("next sibling control");
    let (client, server) = navigation_client().await;
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column: u16::try_from(next.start + 1).unwrap(),
                row: u16::try_from(next.row).unwrap(),
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
    }
    let requests = server.await.unwrap();
    assert_eq!(app.thread.id, second.id);
    assert_eq!(
        app.parent_stack.last().map(|thread| thread.id),
        Some(root.id)
    );
    assert!(
        requests
            .first()
            .is_some_and(|request| request.contains(&format!("/{}/items", second.id))),
        "sibling click loaded the wrong thread: {requests:?}"
    );

    let lines = app.lines((100, 30));
    let second_header = lines[0].text();
    let second_id = second.id.to_string();
    assert!(
        second_header.contains(crate::view::span::short_id(&second_id)),
        "the sibling click must expose the new child's discriminator: {second_header}"
    );
    assert_ne!(
        first_header, second_header,
        "identically titled siblings still need visible navigation feedback"
    );
    let parent = app
        .hit_rows
        .borrow()
        .iter()
        .find(|hit| hit.target == HitTarget::ChildParent)
        .cloned()
        .expect("parent control");
    let (client, server) = navigation_client().await;
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column: u16::try_from(parent.start + 1).unwrap(),
                row: u16::try_from(parent.row).unwrap(),
                modifiers: KeyModifiers::NONE,
            },
            &client,
        )
        .await
        .unwrap();
    }
    let requests = server.await.unwrap();
    assert_eq!(app.thread.id, root.id);
    assert!(app.parent_stack.is_empty());
    assert!(
        requests
            .first()
            .is_some_and(|request| request.contains(&format!("/{}/items", root.id))),
        "parent click loaded the wrong thread: {requests:?}"
    );
}

#[test]
fn child_lifecycle_lives_in_the_band_and_stays_accurate_at_every_width() {
    let (mut app, _, first, _) = child_navigation_fixture();
    app.thread.title = "A deliberately long child-session title for narrow terminals".into();
    let task = app
        .children
        .values_mut()
        .find(|task| task.child_thread_id == first.id)
        .expect("current child task");
    task.state = "running".into();
    task.started_at = Some(Utc::now() - chrono::Duration::seconds(8));

    for (width, height) in [(100, 24), (40, 12)] {
        let lines = app.lines((width, height));
        assert_eq!(lines.len(), usize::try_from(height).unwrap());
        assert!(
            lines
                .iter()
                .all(|line| line.width() <= usize::try_from(width).unwrap()),
            "child chrome overflowed at {width}x{height}"
        );
        let band = line_text(lines.last().unwrap());
        assert!(band.contains("child"), "{width}x{height}: {band}");
        assert!(band.contains("running"), "{width}x{height}: {band}");
        assert!(
            !lines
                .iter()
                .any(|line| line_text(line).contains("Ask anything")),
            "a child pane rendered an editable composer at {width}x{height}"
        );
        assert!(
            app.hit_rows
                .borrow()
                .iter()
                .any(|hit| hit.target == crate::view::frame::HitTarget::ChildParent),
            "the pinned breadcrumb keeps a clickable parent control at {width}x{height}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line_text(line).contains("↑ parent")),
            "navigation stays labelled at {width}x{height}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line_text(line).contains("h a a r n e s s")),
            "a child pane does not repeat the masthead under its breadcrumb"
        );
    }

    let task = app
        .children
        .values_mut()
        .find(|task| task.child_thread_id == first.id)
        .expect("current child task");
    task.state = "done".into();
    assert_eq!(
        task.elapsed_seconds(),
        None,
        "terminal child state must not keep a wall-clock timer alive"
    );
    let lines = app.lines((100, 24));
    let band = line_text(lines.last().unwrap());
    assert!(band.contains("completed"), "{band}");
    assert!(!band.contains("running"), "{band}");

    let compact = app.lines((39, 11));
    assert!(
        !compact
            .iter()
            .any(|line| line_text(line).contains("Ask anything")),
        "compact child pane rendered an editable composer"
    );
}

#[tokio::test]
async fn active_turn_requires_two_escapes_and_shift_escape_never_aborts_the_turn() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let thread = fixture_thread();
    let turn = haarness_protocol::TurnId::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let response_body = serde_json::json!({
        "id": turn,
        "threadId": thread.id,
        "requestId": haarness_protocol::RequestId::new(),
        "status": "interrupted",
        "agent": "build",
        "provider": "openai",
        "model": "gpt-5.4",
        "error": null,
        "createdAt": Utc::now(),
        "updatedAt": Utc::now(),
    })
    .to_string();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.active_turn = Some(turn);

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::SHIFT),
        &client,
    )
    .await
    .unwrap();
    assert!(
        app.escape_interrupt.is_none(),
        "Shift+Esc cannot fall through to whole-turn interruption"
    );

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(
        app.escape_interrupt.is_some(),
        "first Esc only arms interrupt"
    );
    assert!(app.notice.contains("again to interrupt"));

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(app.escape_interrupt.is_none());
    assert_eq!(app.notice, "Stopping task…");
    assert_eq!(
        app.phase.as_ref().map(|phase| phase.state.as_str()),
        Some("stopping")
    );
    // Stop is the person's: it stops the tree and asks the server to answer
    // steering the agent had not read yet.
    let request = server.await.unwrap();
    assert!(
        request.starts_with(&format!(
            "POST /v1/threads/{}/interrupt?resume=true ",
            app.thread.id
        )),
        "wrong interrupt request: {request}"
    );
}

#[test]
fn successful_tool_results_collapse_without_line_counts_and_expand_from_the_call_row() {
    let thread = fixture_thread();
    let short = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "short".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "pwd"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolResult {
                call_id: "short".into(),
                name: "bash".into(),
                output: "/workspace".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ];
    let short_id = short[1].id;
    let mut short_app = App::new(thread.clone(), vec![thread.clone()], short, vec![], vec![]);
    let short_text = short_app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!short_text.contains("1 line"), "{short_text}");
    assert!(!short_text.contains("/workspace"), "{short_text}");
    assert!(short_app.hit_rows.borrow().iter().any(|hit| {
        matches!(hit.target, crate::view::frame::HitTarget::ToolToggle(id) if id == short_id)
    }));

    short_app.expanded.insert(short_id);
    let expanded_short_text = short_app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        expanded_short_text.contains("/workspace"),
        "{expanded_short_text}"
    );

    let long_output = (1..=30)
        .map(|index| format!("long-row-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let long = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "long".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "seq 30"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolResult {
                call_id: "long".into(),
                name: "bash".into(),
                output: long_output,
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ];
    let long_id = long[1].id;
    let long_app = App::new(thread.clone(), vec![thread], long, vec![], vec![]);
    let long_text = long_app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!long_text.contains("30 lines"), "{long_text}");
    assert!(!long_text.contains("long-row-1"), "{long_text}");
    assert!(long_app.hit_rows.borrow().iter().any(|hit| {
        matches!(hit.target, crate::view::frame::HitTarget::ToolToggle(id) if id == long_id)
    }));
}

#[test]
fn shell_mode_output_shows_under_the_command_and_a_non_zero_exit_reads_as_failed() {
    let thread = fixture_thread();
    let bash = |sequence: u64, call_id: &str, command: &str, exit: i64, output: &str| {
        vec![
            message_item(
                sequence,
                &thread,
                ItemData::ToolCall {
                    call_id: call_id.into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": command}),
                    provider_metadata: serde_json::Value::Null,
                },
            ),
            message_item(
                sequence + 1,
                &thread,
                ItemData::ToolResult {
                    call_id: call_id.into(),
                    name: "bash".into(),
                    output: format!("exit: {exit}\n{output}"),
                    is_error: false,
                    metadata: serde_json::json!({"exit": exit, "truncated": false}),
                },
            ),
        ]
    };
    let listing = (1..=14)
        .map(|index| format!("file-{index}.py"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut items = bash(1, "shell-1", "ls src", 0, &listing);
    items.extend(bash(
        3,
        "shell-2",
        "cat missing.txt",
        1,
        "cat: missing.txt: No such file or directory",
    ));
    items.extend(bash(5, "agent-1", "grep -r nothing .", 1, ""));
    items.extend(bash(7, "agent-2", "pwd", 0, "/workspace"));
    let listing_id = items[1].id;
    let mut app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    let text = app
        .lines((100, 60))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    // A typed command shows its output, open, without the model's preamble.
    assert!(text.contains("● bash ls src"), "{text}");
    assert!(text.contains("file-1.py"), "{text}");
    assert!(text.contains("file-10.py"), "{text}");
    assert!(!text.contains("file-11.py"), "{text}");
    assert!(text.contains("… +4 more lines · click to expand"), "{text}");
    assert!(!text.contains("exit: 0"), "{text}");
    // A non-zero exit fails the row and keeps its stderr in view, whoever ran it.
    assert!(text.contains("✗ bash cat missing.txt  exit 1"), "{text}");
    assert!(text.contains("No such file or directory"), "{text}");
    assert!(
        text.contains("✗ bash grep -r nothing .  exit 1  ·  no output"),
        "{text}"
    );
    // An agent's successful command stays a compact row.
    assert!(text.contains("● bash pwd"), "{text}");
    assert!(!text.contains("/workspace"), "{text}");

    app.expanded.insert(listing_id);
    let expanded = app
        .lines((100, 60))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(expanded.contains("file-14.py"), "{expanded}");
    assert!(!expanded.contains("exit: 0"), "{expanded}");
}

#[tokio::test]
async fn a_shell_command_in_view_needs_no_card_unless_it_failed() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let client = offline_client();
    let run = |sequence: u64, call_id: &str, command: &str, exit: i64| {
        let call = message_item(
            sequence,
            &thread,
            ItemData::ToolCall {
                call_id: call_id.into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": command}),
                provider_metadata: serde_json::Value::Null,
            },
        );
        let result = message_item(
            sequence + 1,
            &thread,
            ItemData::ToolResult {
                call_id: call_id.into(),
                name: "bash".into(),
                output: format!("exit: {exit}\n"),
                is_error: false,
                metadata: serde_json::json!({"exit": exit}),
            },
        );
        (
            call,
            fixture_event(
                "item.created",
                thread.id,
                thread.id,
                serde_json::to_value(result).unwrap(),
            ),
        )
    };

    // Its output is right there under the command: the status line says so.
    let (call, finished) = run(1, "shell-1", "ls src", 0);
    app.items.push(call);
    app.reduce(finished, &client).await.unwrap();
    assert!(app.notification_toast.is_none());
    assert_eq!(app.notice, "Shell finished");

    // Out of sight, the card says which command finished.
    app.terminal_focused = false;
    let (call, finished) = run(3, "shell-2", "make build", 0);
    app.items.push(call);
    app.reduce(finished, &client).await.unwrap();
    let toast = app.notification_toast.take().expect("a card");
    assert_eq!(
        (toast.title.as_deref(), toast.message.as_str()),
        (Some("Shell finished"), "make build")
    );

    // A failure always earns one.
    app.terminal_focused = true;
    let (call, failed) = run(5, "shell-3", "cat missing.txt", 1);
    app.items.push(call);
    app.reduce(failed, &client).await.unwrap();
    let toast = app.notification_toast.take().expect("a card");
    assert_eq!(
        (toast.title.as_deref(), toast.message.as_str()),
        (Some("Shell failed"), "cat missing.txt · exit 1")
    );
}

#[test]
fn structurally_empty_tool_results_are_semantic_and_not_expandable() {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "mcps".into(),
                name: "mcp_manage".into(),
                arguments: serde_json::json!({"action": "list"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::ToolResult {
                call_id: "mcps".into(),
                name: "mcp_manage".into(),
                output: "[]".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::ToolCall {
                call_id: "plugins".into(),
                name: "plugin_manage".into(),
                arguments: serde_json::json!({"action": "list"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::ToolResult {
                call_id: "plugins".into(),
                name: "plugin_manage".into(),
                output: "{}".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            5,
            &thread,
            ItemData::ToolCall {
                call_id: "quiet".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "true"}),
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            6,
            &thread,
            ItemData::ToolResult {
                call_id: "quiet".into(),
                name: "bash".into(),
                output: " \n".into(),
                is_error: false,
                metadata: serde_json::Value::Null,
            },
        ),
    ];
    let result_ids = [items[1].id, items[3].id, items[5].id];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let text = app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");

    assert_eq!(text.matches("no results").count(), 2, "{text}");
    assert!(text.contains("no output"), "{text}");
    assert!(!text.lines().any(|line| line.trim() == "[]"), "{text}");
    assert!(!text.lines().any(|line| line.trim() == "{}"), "{text}");
    assert!(!app.hit_rows.borrow().iter().any(|hit| {
        matches!(hit.target, crate::view::frame::HitTarget::ToolToggle(id) if result_ids.contains(&id))
    }));
}

#[test]
fn unicode_composer_wrap_and_cursor_use_display_cells() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.composer.set_text("a界🙂b".into());
    app.composer.move_left();
    app.composer.move_left();
    let lines = app.lines((50, 20));
    let cursor = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.text == "🙂" && span.bg == Some(app.theme.primary))
        .expect("wide emoji is the cursor cell");
    assert_eq!(cursor.width(), 2);
    assert_eq!(cursor.fg, app.theme.bg);
    assert!(lines.iter().all(|line| line.width() <= 50));

    app.composer.set_text("界🙂".repeat(30));
    let wrapped = app.lines((50, 20));
    let rendered = wrapped
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        rendered.matches('界').count(),
        30,
        "CJK input was lost while wrapping: {rendered}"
    );
    assert_eq!(
        rendered.matches('🙂').count(),
        30,
        "emoji input was lost while wrapping: {rendered}"
    );
    assert!(wrapped.iter().all(|line| line.width() <= 50));
}

#[test]
fn composer_cursor_is_visible_when_empty_and_at_multiline_end() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);

    let empty = crate::view::composer::composer_lines(&app, 80);
    assert!(
        empty
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.fg == app.theme.bg && span.bg == Some(app.theme.primary)),
        "the placeholder carries the cursor on its first cell"
    );
    assert_eq!(
        line_text(&empty[0]).find("Ask anything…"),
        Some(2),
        "the placeholder shares the typed text's left margin"
    );

    app.composer.set_text("first line\nsecond line".into());
    let multiline = crate::view::composer::composer_lines(&app, 80);
    assert!(multiline.iter().flat_map(|line| &line.spans).any(|span| {
        span.text == " " && span.fg == app.theme.bg && span.bg == Some(app.theme.primary)
    }));
}

#[test]
fn root_composer_floats_on_the_page_between_a_rule_and_the_band() {
    let thread = fixture_thread();
    let app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let rows = crate::view::composer::composer_lines(&app, 80);
    assert_eq!(rows.len(), 3, "prompt, rule, band");

    let prompt = &rows[0];
    assert!(
        prompt
            .spans
            .iter()
            .all(|span| span.bg.is_none() || span.bg == Some(app.theme.primary)),
        "the prompt is page content, not a raised card: {prompt:?}"
    );

    let rule = &rows[1];
    assert_eq!(rule.width(), 80);
    assert!(
        line_text(rule)
            .chars()
            .all(|character| character == '\u{2500}'),
        "an idle hairline runs the full width"
    );
    assert_eq!(rule.spans[0].fg, app.theme.hairline());

    let band = &rows[2];
    assert_eq!(band.width(), 80);
    assert!(
        band.spans.iter().all(|span| {
            matches!(span.bg, Some(background)
                if background == app.theme.surface || background == app.theme.primary)
        }),
        "the band is one continuous raised strip: {band:?}"
    );
    assert!(
        !rows.iter().any(|row| row
            .text()
            .chars()
            .any(|character| matches!(character, '\u{256d}' | '\u{2570}' | '\u{2502}'))),
        "no box-drawing chrome anywhere in the composer"
    );
}

#[test]
fn session_catalog_is_recent_first_truthful_and_exposes_safe_actions() {
    let now = Utc::now();
    let mut current = fixture_thread();
    current.title = "Current work".into();
    current.updated_at = now - chrono::Duration::hours(2);
    let mut recent = fixture_thread();
    recent.title = "Recent investigation".into();
    recent.updated_at = now - chrono::Duration::minutes(3);
    let mut archived = fixture_thread();
    archived.title = "Archived result".into();
    archived.archived = true;
    archived.updated_at = now - chrono::Duration::hours(1);
    let mut app = App::new(
        current.clone(),
        vec![current.clone(), archived.clone(), recent.clone()],
        vec![],
        vec![],
        vec![],
    );
    app.active_turn = Some(haarness_protocol::TurnId::new());

    assert_eq!(
        app.filtered_threads()
            .iter()
            .map(|thread| thread.id)
            .collect::<Vec<_>>(),
        vec![recent.id, current.id],
        "normal session browsing hides archived entries and sorts by activity"
    );
    let current_detail = app.session_detail(&current);
    assert!(
        current_detail.contains("current · working"),
        "{current_detail}"
    );
    assert!(current_detail.contains("GPT-5.4"), "{current_detail}");

    app.session_include_archived = true;
    let archived_detail = app.session_detail(&archived);
    assert!(archived_detail.contains("archived"), "{archived_detail}");
    let archived_id = archived.id.to_string();
    assert!(
        archived_detail.contains(crate::view::span::short_id(&archived_id)),
        "{archived_detail}"
    );

    app.open_overlay(crate::state::Overlay::Threads);
    app.selection = app
        .filtered_threads()
        .iter()
        .position(|thread| thread.id == recent.id)
        .unwrap();
    app.open_selected_session_actions();
    let crate::state::Overlay::Options { kind, entries, .. } = &app.overlay else {
        panic!("session actions did not open");
    };
    assert!(matches!(kind, crate::state::OptionsKind::SessionActions));
    let labels: Vec<_> = entries.iter().map(|entry| entry.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "Resume",
            "Fork and resume",
            "Rename",
            "Archive",
            "Copy session ID"
        ]
    );
    assert!(
        labels.iter().all(|label| !label.contains("Delete")),
        "the reversible API must not imply permanent deletion"
    );
}

#[tokio::test]
async fn session_selector_navigation_and_wheel_stop_at_both_boundaries() {
    let now = Utc::now();
    let mut current = fixture_thread();
    current.title = "Current session".into();
    current.updated_at = now;
    let mut threads = vec![current.clone()];
    for index in 1..29 {
        let mut thread = fixture_thread();
        thread.title = format!("Session {index:02}");
        thread.updated_at = now - chrono::Duration::minutes(i64::from(index));
        threads.push(thread);
    }
    let mut app = App::new(current, threads, vec![], vec![], vec![]);
    let client = offline_client();
    app.open_overlay(crate::state::Overlay::Threads);
    let last = app.filtered_threads().len() - 1;

    app.selection = last;
    for key in [
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
    ] {
        crate::input::handle_key(&mut app, key, &client)
            .await
            .unwrap();
        assert_eq!(
            app.selection, last,
            "keyboard wrapped past the last session"
        );
    }
    crate::input::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.selection, last, "wheel wrapped past the last session");
    let bottom = app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        bottom.contains("–29 of 29"),
        "session footer did not report the real final window: {bottom}"
    );

    app.selection = 0;
    for key in [
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
    ] {
        crate::input::handle_key(&mut app, key, &client)
            .await
            .unwrap();
        assert_eq!(
            app.selection, 0,
            "keyboard wrapped before the first session"
        );
    }
    crate::input::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.selection, 0, "wheel wrapped before the first session");
    let top = app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        top.contains("1–"),
        "session footer did not report the real first window: {top}"
    );

    // Inline autocomplete remains deliberately cyclic; only paged dialogs
    // clamp at their edges.
    app.close_overlay();
    app.composer.set_text("/".into());
    app.selection = 0;
    let autocomplete_last = app.slash_suggestions().expect("slash suggestions").len() - 1;
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.selection, autocomplete_last);
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.selection, 0);
}

#[tokio::test]
async fn session_switcher_right_opens_actions_for_the_highlighted_session() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Threads);
    let selected = app
        .filtered_threads()
        .iter()
        .find(|thread| thread.id != app.thread.id)
        .copied()
        .cloned()
        .expect("second session");
    app.selection = app
        .filtered_threads()
        .iter()
        .position(|thread| thread.id == selected.id)
        .unwrap();

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::SessionActions,
            ..
        }
    ));

    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        unreachable!();
    };
    app.selection = entries
        .iter()
        .position(|entry| entry.label == "Rename")
        .unwrap();
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    let selected_id = selected.id.to_string();
    assert_eq!(
        app.composer.text(),
        format!("/rename {} ", crate::view::span::short_id(&selected_id)),
        "rename action targets the highlighted session without switching first"
    );
    assert_ne!(app.thread.id, selected.id);
}

#[tokio::test]
async fn resume_alias_targets_a_unique_short_session_id_without_an_extra_menu_step() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let current = fixture_thread();
    let target = Thread {
        title: "Target session".into(),
        ..fixture_thread()
    };
    let catalog = serde_json::to_string(&vec![target.clone(), current.clone()]).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        // One catalog request, followed by switch_thread's six resources.
        for index in 0..7 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request_line = String::from_utf8(request)
                .unwrap()
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            let body = if index == 0 {
                catalog.as_str()
            } else if request_line.contains("/goal ") {
                r#"{"goal":null}"#
            } else if request_line.contains("/active ") {
                "null"
            } else {
                "[]"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            requests.push(request_line);
        }
        requests
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    let mut app = App::new(current.clone(), vec![current], vec![], vec![], vec![]);
    let target_id = target.id.to_string();
    let short_id = crate::view::span::short_id(&target_id);

    app.execute_command(&format!("/resume {short_id}"), &client)
        .await
        .unwrap();
    let requests = tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .expect("resume hydration made an unexpected number of requests")
        .unwrap();
    assert_eq!(app.thread.id, target.id);
    assert!(matches!(app.overlay, crate::state::Overlay::None));
    assert_eq!(requests[0], "GET /v1/threads HTTP/1.1");
    assert!(
        requests[1].contains(&format!("/{}/items", target.id)),
        "direct resume loaded the wrong session: {requests:?}"
    );
}

#[tokio::test]
async fn sessions_all_discovers_archived_sessions_without_deleting_or_restoring_them() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let current = fixture_thread();
    let archived = Thread {
        title: "Archived audit".into(),
        archived: true,
        ..fixture_thread()
    };
    let body = serde_json::to_string(&vec![current.clone(), archived.clone()]).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            request.extend_from_slice(&chunk[..read]);
            if read == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    let mut app = App::new(current.clone(), vec![current], vec![], vec![], vec![]);

    app.execute_command("/sessions --all", &client)
        .await
        .unwrap();
    let request = server.await.unwrap();
    assert!(request.starts_with("GET /v1/threads?archived=true "));
    assert!(app.session_include_archived);
    assert!(matches!(app.overlay, crate::state::Overlay::Threads));
    assert!(
        app.filtered_threads()
            .iter()
            .any(|thread| thread.id == archived.id),
        "--all must expose the archived session in-place"
    );
}

fn fixture_workflow_definition(name: &str) -> haarness_protocol::WorkflowDefinition {
    haarness_protocol::WorkflowDefinition {
        name: name.into(),
        invocation: name.into(),
        plugin_name: None,
        dialect: haarness_protocol::WorkflowDialect::Native,
        path: format!("/workspace/.haarness/workflows/{name}.ts"),
        legacy: false,
        meta: haarness_protocol::WorkflowMeta {
            name: name.into(),
            description: Some("Verify and publish a release".into()),
            when_to_use: Some("when shipping".into()),
            phases: vec![],
            arguments: std::collections::BTreeMap::new(),
        },
        findings: vec![],
    }
}

fn fixture_workflow_run(
    name: &str,
    status: haarness_protocol::WorkflowRunStatus,
) -> haarness_protocol::WorkflowRun {
    let now = Utc::now();
    let definition = fixture_workflow_definition(name);
    haarness_protocol::WorkflowRun {
        id: haarness_protocol::WorkflowId::new(),
        thread_id: None,
        directory: "/workspace".into(),
        workflow_name: name.into(),
        source_path: Some(definition.path.clone()),
        source_hash: "abc123".into(),
        args: None,
        status,
        phase: Some("verify".into()),
        status_payload: None,
        logs: vec![haarness_protocol::WorkflowLog {
            sequence: 1,
            time: now,
            message: "verification started".into(),
            phase: Some("verify".into()),
            source: Some(haarness_protocol::WorkflowLogSource::Workflow),
        }],
        warnings: vec![],
        agents: vec![],
        definition: haarness_protocol::WorkflowRunDefinition {
            name: definition.name,
            invocation: definition.invocation,
            launch_thread_id: None,
            plugin_name: None,
            agent: None,
            model: None,
            variant: None,
            dialect: definition.dialect,
            meta: definition.meta,
            source: Some("export default async () => {}".into()),
            source_path: Some(definition.path),
            args_provided: Some(false),
        },
        result: None,
        error: None,
        time: haarness_protocol::WorkflowRunTime {
            created: now,
            updated: now,
            started: Some(now),
            completed: None,
        },
    }
}

#[test]
fn workflow_slash_parser_preserves_json_and_plain_argument_shapes() {
    use crate::commands::{WorkflowSlashCommand, parse_workflow_command, workflow_command_args};

    assert_eq!(
        parse_workflow_command("").unwrap(),
        WorkflowSlashCommand::Dashboard
    );
    assert_eq!(
        parse_workflow_command("status").unwrap(),
        WorkflowSlashCommand::Dashboard
    );
    assert_eq!(
        parse_workflow_command("ls").unwrap(),
        WorkflowSlashCommand::Dashboard
    );
    assert_eq!(
        parse_workflow_command("release summarize all changes").unwrap(),
        WorkflowSlashCommand::Run {
            name: "release".into(),
            args: Some(serde_json::Value::String("summarize all changes".into())),
        }
    );
    assert_eq!(
        parse_workflow_command("release true").unwrap(),
        WorkflowSlashCommand::Run {
            name: "release".into(),
            args: Some(serde_json::Value::String("true".into())),
        },
        "dialect normalization happens after definition selection"
    );
    assert_eq!(
        parse_workflow_command(r#"run release {"dryRun":true,"retries":2}"#).unwrap(),
        WorkflowSlashCommand::Run {
            name: "release".into(),
            args: Some(serde_json::json!({"dryRun": true, "retries": 2})),
        }
    );
    assert_eq!(
        parse_workflow_command("show 019abc").unwrap(),
        WorkflowSlashCommand::Inspect("019abc".into())
    );
    assert_eq!(
        parse_workflow_command("rm 019abc").unwrap(),
        WorkflowSlashCommand::Delete("019abc".into())
    );
    assert_eq!(
        parse_workflow_command("cancel 019abc").unwrap(),
        WorkflowSlashCommand::Cancel("019abc".into())
    );
    assert_eq!(
        parse_workflow_command("pause 019abc").unwrap(),
        WorkflowSlashCommand::Pause("019abc".into())
    );
    assert_eq!(
        parse_workflow_command("resume 019abc").unwrap(),
        WorkflowSlashCommand::Resume("019abc".into())
    );
    assert!(parse_workflow_command("run release {broken").is_err());
    assert!(parse_workflow_command("pause").is_err());

    let native = fixture_workflow_definition("release");
    assert_eq!(
        workflow_command_args(&native, None).unwrap(),
        serde_json::json!({})
    );
    assert_eq!(
        workflow_command_args(&native, Some(serde_json::Value::String("true".into()))).unwrap(),
        serde_json::json!({"input": "true"})
    );
    assert!(workflow_command_args(&native, Some(serde_json::json!([1, 2]))).is_err());

    let mut portable = fixture_workflow_definition("portable");
    portable.dialect = haarness_protocol::WorkflowDialect::ClaudePortable;
    assert_eq!(
        workflow_command_args(&portable, Some(serde_json::Value::String("true".into()))).unwrap(),
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        workflow_command_args(
            &portable,
            Some(serde_json::Value::String("plain text".into()))
        )
        .unwrap(),
        serde_json::json!({"input": "plain text"})
    );
}

#[test]
fn discovered_workflows_are_first_class_exact_slash_commands() {
    let mut app = selector_fixture();
    let mut release = fixture_workflow_definition("release");
    release.meta.arguments.insert(
        "base".into(),
        haarness_protocol::WorkflowArgument {
            kind: Some(haarness_protocol::WorkflowArgumentType::String),
            default: None,
            description: Some("base branch".into()),
        },
    );
    app.workflow_definitions = vec![release];

    assert!(app.is_known_command("/release ship this"));
    let suggestions = app.filtered_slash_entries("rel");
    assert_eq!(suggestions[0].name, "release");
    assert!(suggestions[0].description.starts_with("Workflow ·"));
    assert!(suggestions[0].description.contains("args <base>"));
}

#[test]
fn workflow_dashboard_combines_runs_definitions_and_contextual_actions() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let run = fixture_workflow_run("release", haarness_protocol::WorkflowRunStatus::Running);
    let run_id = run.id.to_string();
    app.open_workflow_dashboard(
        haarness_protocol::WorkflowListResponse {
            workflows: vec![fixture_workflow_definition("release")],
            invalid: vec![],
        },
        vec![run.clone()],
    );

    let crate::state::Overlay::Options { kind, entries, .. } = &app.overlay else {
        panic!("workflow dashboard did not use the shared options modal");
    };
    assert!(matches!(kind, crate::state::OptionsKind::WorkflowDashboard));
    assert!(entries[0].value.starts_with("run:"), "runs stay first");
    assert!(
        entries[1].value.starts_with("definition:"),
        "definitions follow recent activity"
    );
    let rendered = app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Workflows"), "{rendered}");
    assert!(rendered.contains("recent run"), "{rendered}");
    assert!(rendered.contains("Verify and publish"), "{rendered}");

    app.open_workflow_run_actions(&run);
    let crate::state::Overlay::Options { kind, entries, .. } = &app.overlay else {
        panic!("workflow run actions did not open");
    };
    assert!(matches!(
        kind,
        crate::state::OptionsKind::WorkflowRunActions
    ));
    let values = entries
        .iter()
        .map(|entry| entry.value.as_str())
        .collect::<Vec<_>>();
    assert!(values.contains(&format!("inspect:{run_id}").as_str()));
    assert!(values.contains(&format!("pause:{run_id}").as_str()));
    assert!(values.contains(&format!("cancel:{run_id}").as_str()));
    assert!(!values.iter().any(|value| value.starts_with("resume:")));
}

#[test]
fn child_launched_workflow_is_prioritized_and_identified_in_global_dashboard() {
    let root = fixture_thread();
    let child = Thread {
        title: "Provider security review".into(),
        parent_id: Some(root.id),
        is_subagent: true,
        fork_origin: None,
        ..fixture_thread()
    };
    let other = Thread {
        title: "Other conversation".into(),
        ..fixture_thread()
    };
    let mut child_run = fixture_workflow_run(
        "security-audit",
        haarness_protocol::WorkflowRunStatus::Running,
    );
    child_run.definition.launch_thread_id = Some(child.id);
    child_run.time.updated = Utc::now() - chrono::Duration::minutes(5);
    let mut newer_other =
        fixture_workflow_run("release", haarness_protocol::WorkflowRunStatus::Completed);
    newer_other.definition.launch_thread_id = Some(other.id);
    newer_other.time.updated = Utc::now();
    let mut app = App::new(
        child.clone(),
        vec![root, child.clone(), other],
        vec![],
        vec![],
        vec![],
    );

    app.open_workflow_dashboard(
        haarness_protocol::WorkflowListResponse {
            workflows: vec![],
            invalid: vec![],
        },
        vec![newer_other, child_run.clone()],
    );

    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        panic!("workflow dashboard did not open");
    };
    assert_eq!(entries[0].value, format!("run:{}", child_run.id));
    assert!(entries[0].description.contains("this task"));
    let rendered = app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("security-audit"), "{rendered}");
    assert!(rendered.contains("this task"), "{rendered}");
}

#[test]
fn workflow_dashboard_sse_reorder_preserves_the_logical_selection() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let mut old = fixture_workflow_run("release", haarness_protocol::WorkflowRunStatus::Running);
    old.time.updated = Utc::now() - chrono::Duration::minutes(2);
    app.open_workflow_dashboard(
        haarness_protocol::WorkflowListResponse {
            workflows: vec![fixture_workflow_definition("release")],
            invalid: vec![],
        },
        vec![old],
    );
    app.overlay_filter = "release".into();
    app.selection = 1;
    assert!(matches!(
        app.selected_overlay_choice(),
        Some(crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkflowDashboard,
            ref value
        )) if value == "definition:release"
    ));

    let mut newest = fixture_workflow_run("release", haarness_protocol::WorkflowRunStatus::Running);
    newest.time.updated = Utc::now();
    app.upsert_workflow_run(newest);
    app.refresh_open_workflow_dashboard();

    assert_eq!(app.overlay_filter, "release");
    assert!(matches!(
        app.selected_overlay_choice(),
        Some(crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkflowDashboard,
            ref value
        )) if value == "definition:release"
    ));
}

#[tokio::test]
async fn workflow_log_sse_reorders_the_dashboard_without_moving_logical_focus() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let mut active = fixture_workflow_run("active", haarness_protocol::WorkflowRunStatus::Running);
    active.time.updated = Utc::now() - chrono::Duration::minutes(2);
    let mut selected =
        fixture_workflow_run("selected", haarness_protocol::WorkflowRunStatus::Running);
    selected.time.updated = Utc::now() - chrono::Duration::minutes(1);
    let selected_value = format!("run:{}", selected.id);
    app.open_workflow_dashboard(
        haarness_protocol::WorkflowListResponse {
            workflows: vec![],
            invalid: vec![],
        },
        vec![active.clone(), selected],
    );
    assert!(matches!(
        app.selected_overlay_choice(),
        Some(crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkflowDashboard,
            ref value
        )) if value == &selected_value
    ));

    let entry = haarness_protocol::WorkflowLog {
        sequence: 2,
        time: Utc::now(),
        message: "still working".into(),
        phase: Some("verify".into()),
        source: Some(haarness_protocol::WorkflowLogSource::Workflow),
    };
    let event_thread = ThreadId::new();
    app.reduce(
        fixture_event(
            "workflow.log",
            event_thread,
            event_thread,
            serde_json::json!({"runId": active.id, "entry": entry}),
        ),
        &offline_client(),
    )
    .await
    .unwrap();

    assert_eq!(app.workflow_runs[0].id, active.id);
    assert_eq!(app.workflow_runs[0].logs.len(), 2);
    assert!(matches!(
        app.selected_overlay_choice(),
        Some(crate::state::OverlayChoice::Option(
            crate::state::OptionsKind::WorkflowDashboard,
            ref value
        )) if value == &selected_value
    ));
}

#[test]
fn workflow_dashboard_keeps_selected_content_and_chrome_on_a_narrow_terminal() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let runs = (0..6)
        .map(|index| {
            fixture_workflow_run(
                &format!("audit-{index}"),
                haarness_protocol::WorkflowRunStatus::Completed,
            )
        })
        .collect();
    app.open_workflow_dashboard(
        haarness_protocol::WorkflowListResponse {
            workflows: vec![fixture_workflow_definition("release")],
            invalid: vec![],
        },
        runs,
    );
    app.selection = app.current_selection_length() - 1;

    let lines = app.lines((40, 12));
    let text = lines.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Workflows"), "{text}");
    assert!(
        text.contains("release"),
        "selected last row was clipped: {text}"
    );
    assert!(text.contains('━'), "modal cap was clipped: {text}");
    assert!(
        text.contains("pgup/pgdn scroll") || text.contains("of "),
        "modal footer was clipped: {text}"
    );
    assert!(
        lines.iter().all(|line| line.width() <= 40),
        "narrow modal overflowed its terminal"
    );
}

#[test]
fn failed_workflow_details_lead_with_the_failure_reason() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let mut run = fixture_workflow_run(
        "security-audit",
        haarness_protocol::WorkflowRunStatus::Failed,
    );
    run.error = Some("workflow exceeded its execution timeout after 1200ms".into());
    let run_id = run.id;
    app.open_workflow_run_details(&run);

    let crate::state::Overlay::Workflow { ref target } = app.overlay else {
        panic!("workflow details did not open the run inspector");
    };
    assert_eq!(*target, crate::state::WorkflowTarget::Run(run_id));

    let body = crate::view::workflow::run_body(
        &app,
        app.workflow_runs.first().expect("the run is cached"),
        72,
    );
    let text = body.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("security-audit"), "{text}");
    assert!(text.contains("failed · verify"), "{text}");
    let failure = text
        .find("timeout after 1200ms")
        .expect("the failure reason");
    let logs = text.find("verification started").expect("the log tail");
    assert!(
        failure < logs,
        "the failure reason must lead the run's own log history: {text}"
    );
    assert!(
        !text.contains("+00:00"),
        "raw machine timestamps do not belong in the report: {text}"
    );

    // A live inspector keeps rendering from the cached snapshot, so an
    // update that arrives while it is open is visible without reopening.
    let mut later = app.workflow_runs[0].clone();
    later.phase = Some("report".into());
    app.upsert_workflow_run(later);
    let refreshed = crate::view::workflow::run_body(&app, &app.workflow_runs[0], 72)
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(refreshed.contains("failed · report"), "{refreshed}");
}

#[tokio::test]
async fn opening_a_foreign_workflow_session_uses_its_launching_parent() {
    let current = Thread {
        title: "Current conversation".into(),
        ..fixture_thread()
    };
    let launch = Thread {
        title: "Launching conversation".into(),
        ..fixture_thread()
    };
    let child = Thread {
        title: "Workflow · release".into(),
        parent_id: Some(launch.id),
        is_subagent: true,
        fork_origin: None,
        ..fixture_thread()
    };
    let mut run = fixture_workflow_run("release", haarness_protocol::WorkflowRunStatus::Running);
    run.thread_id = Some(child.id);
    run.definition.launch_thread_id = Some(launch.id);
    let mut app = App::new(
        current.clone(),
        vec![current.clone(), launch.clone(), child.clone()],
        vec![],
        vec![],
        vec![],
    );
    app.children.insert(
        "stale-task".into(),
        crate::state::ChildTask {
            child_thread_id: ThreadId::new(),
            child_turn_id: None,
            invoked_turn_id: None,
            state: "running".into(),
            tool_calls: 1,
            last_tool: Some("grep".into()),
            started_at: Some(Utc::now()),
        },
    );
    let (client, server) = navigation_client().await;

    app.enter_workflow_session(&run, &client).await.unwrap();
    let requests = server.await.unwrap();

    assert_eq!(requests.len(), 6);
    assert_eq!(app.thread.id, child.id);
    assert_eq!(app.root_thread_id, launch.id);
    assert_eq!(
        app.parent_stack.last().map(|thread| thread.id),
        Some(launch.id),
        "Up must return to the actual launching conversation"
    );
    assert!(
        app.children.is_empty(),
        "foreign task siblings must not leak into workflow navigation"
    );
    assert!(app.notice.contains("launching conversation"));
}

#[tokio::test]
async fn workflow_command_loads_catalog_and_recent_runs_into_one_modal() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let catalog = serde_json::to_string(&haarness_protocol::WorkflowListResponse {
        workflows: vec![fixture_workflow_definition("release")],
        invalid: vec![],
    })
    .unwrap();
    let runs = serde_json::to_string(&vec![fixture_workflow_run(
        "release",
        haarness_protocol::WorkflowRunStatus::Completed,
    )])
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
                if read == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            let request_line = request.lines().next().unwrap_or_default().to_owned();
            let body = if request_line.contains("/v1/workflows/runs?") {
                &runs
            } else {
                &catalog
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            requests.push(request_line);
        }
        requests
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);

    app.execute_command("/workflows", &client).await.unwrap();
    let requests = server.await.unwrap();
    assert!(
        requests
            .iter()
            .any(|line| line == "GET /v1/workflows HTTP/1.1")
    );
    assert!(
        requests
            .iter()
            .any(|line| line == "GET /v1/workflows/runs?limit=50 HTTP/1.1")
    );
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::WorkflowDashboard,
            ..
        }
    ));
    assert_eq!(app.workflow_definitions.len(), 1);
    assert_eq!(app.workflow_runs.len(), 1);
}

#[tokio::test]
async fn direct_workflow_command_posts_typed_args_on_the_current_thread() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let thread = fixture_thread();
    let mut run = fixture_workflow_run("release", haarness_protocol::WorkflowRunStatus::Running);
    run.thread_id = Some(thread.id);
    let response_body =
        serde_json::to_string(&haarness_protocol::CommandExecutionResponse::WorkflowRun {
            run: Box::new(run),
        })
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.reasoning_effort = Some("high".into());
    app.server_commands = vec![haarness_protocol::CommandInfo {
        execution: haarness_protocol::DefinitionExecution::default(),
        name: "release".into(),
        description: "[portable] release".into(),
        source: Some("workflow".into()),
        agent: None,
        model: None,
        argument_hint: None,
        template: String::new(),
        plugin: None,
    }];

    app.execute_command(r#"/release {"dryRun":true,"retries":2}"#, &client)
        .await
        .unwrap();
    let request = server.await.unwrap();
    assert!(request.starts_with(&format!("POST /v1/threads/{}/commands HTTP/1.1", thread.id)));
    let body = request.split("\r\n\r\n").nth(1).unwrap();
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["name"], "release");
    assert_eq!(
        body["arguments"],
        serde_json::json!({"dryRun": true, "retries": 2})
    );
    assert_eq!(body["agent"], app.agent());
    assert_eq!(body["provider"], thread.provider);
    assert_eq!(body["model"], thread.model);
    assert_eq!(body["variant"], "high");
    assert_eq!(body["wait"], false);
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::WorkflowRunActions,
            ..
        }
    ));
}

#[tokio::test]
async fn configured_server_command_uses_tagged_command_turn_response() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let thread = fixture_thread();
    let turn = haarness_protocol::Turn {
        id: haarness_protocol::TurnId::new(),
        thread_id: thread.id,
        request_id: haarness_protocol::RequestId::new(),
        status: haarness_protocol::TurnStatus::Running,
        agent: "review".into(),
        provider: thread.provider.clone(),
        model: thread.model.clone(),
        ultracode: false,
        error: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let turn_id = turn.id;
    let response_body =
        serde_json::to_string(&haarness_protocol::CommandExecutionResponse::Turn { turn }).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 202 Accepted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.server_commands = vec![haarness_protocol::CommandInfo {
        execution: haarness_protocol::DefinitionExecution::default(),
        name: "review-code".into(),
        description: "configured review".into(),
        source: Some("configured".into()),
        agent: Some("review".into()),
        model: None,
        argument_hint: Some("<focus>".into()),
        template: "hidden on client".into(),
        plugin: None,
    }];
    app.reasoning_effort = Some("medium".into());
    app.attachments.push("/tmp/finding.png".into());

    app.execute_command("/REVIEW-CODE focus here", &client)
        .await
        .unwrap();
    let request = server.await.unwrap();
    assert!(request.starts_with(&format!("POST /v1/threads/{}/commands HTTP/1.1", thread.id)));
    let body: serde_json::Value =
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["name"], "review-code");
    assert_eq!(body["arguments"], "focus here");
    assert_eq!(body["attachments"], serde_json::json!(["/tmp/finding.png"]));
    assert_eq!(body["reasoningEffort"], "medium");
    assert!(body.get("variant").is_none());
    assert_eq!(app.active_turn, Some(turn_id));
    assert!(app.attachments.is_empty());
    assert_eq!(app.notice, "/review-code running…");
}

#[tokio::test]
async fn workflow_manager_run_propagates_current_agent_model_and_variant() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let thread = fixture_thread();
    let mut run = fixture_workflow_run("release", haarness_protocol::WorkflowRunStatus::Running);
    run.definition.launch_thread_id = Some(thread.id);
    let response_body = serde_json::to_string(&run).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 202 Accepted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.workflow_definitions = vec![fixture_workflow_definition("release")];
    app.reasoning_effort = Some("xhigh".into());

    app.execute_command("/workflow run release ship this", &client)
        .await
        .unwrap();
    let request = server.await.unwrap();
    assert!(request.starts_with("POST /v1/workflows/run HTTP/1.1"));
    let body: serde_json::Value =
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["name"], "release");
    assert_eq!(body["args"], serde_json::json!({"input": "ship this"}));
    assert_eq!(body["agent"], "build");
    assert_eq!(body["model"], "openai/gpt-5.4");
    assert_eq!(body["variant"], "xhigh");
    assert_eq!(body["threadId"], thread.id.to_string());
}

#[tokio::test]
async fn workflow_sse_refreshes_the_open_dashboard_and_notifies_terminal_transition() {
    let thread = fixture_thread();
    let mut running =
        fixture_workflow_run("release", haarness_protocol::WorkflowRunStatus::Running);
    let run_thread = ThreadId::new();
    running.thread_id = Some(run_thread);
    running.definition.launch_thread_id = Some(thread.id);
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.open_workflow_dashboard(
        haarness_protocol::WorkflowListResponse {
            workflows: vec![fixture_workflow_definition("release")],
            invalid: vec![],
        },
        vec![running.clone()],
    );
    app.overlay_filter = "release".into();

    let log = haarness_protocol::WorkflowLog {
        sequence: 2,
        time: Utc::now(),
        message: "published".into(),
        phase: Some("publish".into()),
        source: Some(haarness_protocol::WorkflowLogSource::Workflow),
    };
    let unrelated_thread = ThreadId::new();
    app.reduce(
        fixture_event(
            "workflow.log",
            unrelated_thread,
            unrelated_thread,
            serde_json::json!({"runId": running.id, "entry": log}),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.workflow_runs[0].logs.len(), 2);

    let mut completed = running;
    completed.status = haarness_protocol::WorkflowRunStatus::Completed;
    completed.phase = None;
    completed.time.updated = Utc::now();
    completed.time.completed = Some(Utc::now());
    // A completion card is for the reader who looked away; the dashboard in
    // front of a focused reader already shows the new status.
    app.terminal_focused = false;
    app.reduce(
        fixture_event(
            "workflow.updated",
            run_thread,
            thread.id,
            serde_json::json!({"info": completed}),
        ),
        &offline_client(),
    )
    .await
    .unwrap();

    assert_eq!(
        app.workflow_runs[0].status,
        haarness_protocol::WorkflowRunStatus::Completed
    );
    assert_eq!(app.overlay_filter, "release", "SSE keeps dashboard search");
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        panic!("workflow event closed the dashboard");
    };
    assert!(entries[0].description.contains("completed"));
    assert_eq!(
        app.notification_toast
            .as_ref()
            .map(|toast| toast.message.as_str()),
        Some("Workflow completed: release")
    );
    assert_eq!(
        app.pending_notification.as_deref(),
        Some("Workflow completed: release")
    );

    // The open dashboard follows global run state, but a workflow launched
    // by another conversation must not steal this conversation's attention.
    app.notification_toast = None;
    app.pending_notification = None;
    let other_root = ThreadId::new();
    let other_run_thread = ThreadId::new();
    let mut other = fixture_workflow_run(
        "other-release",
        haarness_protocol::WorkflowRunStatus::Running,
    );
    other.thread_id = Some(other_run_thread);
    other.definition.launch_thread_id = Some(other_root);
    app.upsert_workflow_run(other.clone());
    app.refresh_open_workflow_dashboard();
    other.status = haarness_protocol::WorkflowRunStatus::Completed;
    other.time.completed = Some(Utc::now());
    app.reduce(
        fixture_event(
            "workflow.updated",
            other_run_thread,
            other_root,
            serde_json::json!({"info": other}),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(app.notification_toast.is_none());
    assert!(app.pending_notification.is_none());
}

#[test]
fn inspecting_a_definition_documents_its_arguments_before_launching() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let mut definition = fixture_workflow_definition("release");
    definition.meta.arguments.insert(
        "scope".into(),
        haarness_protocol::WorkflowArgument {
            kind: Some(haarness_protocol::WorkflowArgumentType::String),
            default: None,
            description: Some("which package set to release".into()),
        },
    );
    definition.findings = vec![haarness_protocol::WorkflowFinding {
        line: 12,
        rule: "no-network".into(),
        text: "fetch() is not available in a portable workflow".into(),
    }];
    let invocation = definition.invocation.clone();
    app.open_workflow_dashboard(
        haarness_protocol::WorkflowListResponse {
            workflows: vec![definition],
            invalid: vec![],
        },
        vec![],
    );
    app.selection = app
        .current_selection_length()
        .checked_sub(1)
        .expect("the catalog has one row");
    app.inspect_selected_workflow();

    let crate::state::Overlay::Workflow { ref target } = app.overlay else {
        panic!("→ did not open the definition inspector");
    };
    assert_eq!(
        *target,
        crate::state::WorkflowTarget::Definition(invocation.clone())
    );
    let text = app
        .lines((110, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains(&format!("/{invocation}")), "{text}");
    assert!(
        text.contains("scope") && text.contains("required"),
        "an undocumented argument is the reason this view exists: {text}"
    );
    assert!(text.contains("no-network"), "findings are surfaced: {text}");
    assert!(
        text.contains("enter starts this workflow"),
        "the launch affordance is stated: {text}"
    );
}

#[test]
fn inspecting_an_unloadable_definition_shows_the_whole_parse_error() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let error =
        "SyntaxError: unexpected token '}' at line 12, column 3 while parsing the phase table";
    app.open_workflow_dashboard(
        haarness_protocol::WorkflowListResponse {
            workflows: vec![],
            invalid: vec![haarness_protocol::WorkflowInvalidDefinition {
                path: "/workspace/.haarness/workflows/broken.ts".into(),
                plugin_name: None,
                legacy: false,
                error: error.into(),
            }],
        },
        vec![],
    );
    app.selection = 0;
    app.inspect_selected_workflow();
    let crate::state::Overlay::Workflow { ref target } = app.overlay else {
        panic!("→ did not open the invalid-definition inspector");
    };
    assert_eq!(*target, crate::state::WorkflowTarget::Invalid(0));
    let text = app
        .lines((110, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("broken.ts"), "{text}");
    assert!(
        text.contains("while parsing the phase table"),
        "the list row truncates this error; the inspector must not: {text}"
    );
}

#[test]
fn a_run_row_reports_how_long_it_took() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let mut run = fixture_workflow_run("release", haarness_protocol::WorkflowRunStatus::Completed);
    run.time.started = Some(Utc::now() - chrono::Duration::seconds(135));
    run.time.completed = Some(Utc::now() - chrono::Duration::seconds(15));
    app.open_workflow_dashboard(
        haarness_protocol::WorkflowListResponse {
            workflows: vec![],
            invalid: vec![],
        },
        vec![run],
    );
    let text = app
        .lines((110, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("2m 0s"),
        "the list answers how long a run took: {text}"
    );
}

// ---------------------------------------------------------------------------
// Review viewer (`/diff`)
// ---------------------------------------------------------------------------

fn review_files() -> Vec<haarness_protocol::FileDiff> {
    vec![
        haarness_protocol::FileDiff {
            file: "crates/haarness-tui/src/diffview.rs".into(),
            status: haarness_protocol::FileDiffStatus::Modified,
            additions: 3,
            deletions: 2,
            patch: Some(
                "@@ -38,5 +38,6 @@ impl DiffSource {\n     pub const fn label(self) -> &'static str {\n-            Self::WorkingTree => \"working\",\n+            Self::WorkingTree => \"working tree\",\n         }\n@@ -90,3 +92,4 @@\n-    println!(\"{}\", total);\n+    println!(\"total = {}\", total);\n+    finish();\n }\n"
                    .into(),
            ),
            before: None,
            after: None,
            binary: false,
            truncated: false,
        },
        haarness_protocol::FileDiff {
            file: "crates/haarness-tui/src/view/menu.rs".into(),
            status: haarness_protocol::FileDiffStatus::Modified,
            additions: 1,
            deletions: 0,
            patch: Some("@@ -1,2 +1,3 @@\n use crate::state::App;\n+use crate::theme::Theme;\n".into()),
            before: None,
            after: None,
            binary: false,
            truncated: false,
        },
        haarness_protocol::FileDiff {
            file: "README.md".into(),
            status: haarness_protocol::FileDiffStatus::Added,
            additions: 1,
            deletions: 0,
            patch: Some("@@ -0,0 +1 @@\n+# Haarness\n".into()),
            before: None,
            after: None,
            binary: false,
            truncated: false,
        },
    ]
}

fn review_app() -> App {
    let mut app = selector_fixture();
    let viewer = crate::diffview::DiffViewer::new(
        crate::diffview::DiffSource::WorkingTree,
        review_files(),
        crate::diffview::DiffPreferences::default(),
    );
    app.overlay = crate::state::Overlay::Diff(Box::new(viewer));
    app.sync_diff_layout(120);
    app
}

/// The same review, stacked rather than side by side, so assertions can read
/// whole code lines instead of columns clipped to half the pane.
fn review_app_unified() -> App {
    let mut app = review_app();
    viewer_mut(&mut app).layout = Some(crate::diffview::DiffLayout::Unified);
    viewer_mut(&mut app).rebuild_display();
    app
}

fn viewer_of(app: &App) -> &crate::diffview::DiffViewer {
    match &app.overlay {
        crate::state::Overlay::Diff(viewer) => viewer,
        _ => panic!("the review viewer is not open"),
    }
}

fn viewer_mut(app: &mut App) -> &mut crate::diffview::DiffViewer {
    match &mut app.overlay {
        crate::state::Overlay::Diff(viewer) => viewer,
        _ => panic!("the review viewer is not open"),
    }
}

async fn review_key(app: &mut App, code: KeyCode) {
    crate::input::handle_key(
        app,
        KeyEvent::new(code, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
}

#[test]
fn review_takes_the_whole_terminal_with_a_tree_a_patch_and_key_hints() {
    let app = review_app_unified();
    let lines = app.lines((120, 30));
    let rows: Vec<String> = lines.iter().map(line_text).collect();

    assert!(rows[0].contains("Diff"), "{:?}", rows[0]);
    assert!(rows[0].contains("working tree"), "{:?}", rows[0]);
    assert!(rows[0].contains("3 files"), "{:?}", rows[0]);
    assert!(
        rows[0].contains("+5"),
        "totals in the header: {:?}",
        rows[0]
    );
    // The tree nests, collapses single-child chains, and marks each status.
    assert!(
        rows.iter()
            .any(|row| row.contains("crates/haarness-tui/src")),
        "collapsed directory chain missing"
    );
    assert!(rows.iter().any(|row| row.contains("diffview.rs")));
    assert!(rows.iter().any(|row| row.contains("README.md")));
    // The patch pane carries real line numbers from the hunk headers.
    assert!(
        rows.iter()
            .any(|row| row.contains("Self::WorkingTree => \"working tree\"")),
        "{rows:#?}"
    );
    assert!(rows.iter().any(|row| row.contains('⋮')), "hunk elision");
    // Live key hints, not a hidden keymap.
    let footer = rows.last().unwrap();
    assert!(footer.contains("change"), "{footer:?}");
    assert!(footer.contains("close"), "{footer:?}");
    // Nothing from the conversation behind it bleeds through.
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("Ship a verified release")),
        "the review is a full-screen place, not a card over the transcript"
    );
}

#[test]
fn review_paints_added_and_removed_rows_in_their_diff_colors() {
    let app = review_app_unified();
    let lines = app.lines((120, 30));
    let added = lines
        .iter()
        .find(|line| line.text().contains("+ use crate::theme::Theme;"))
        .expect("an added row");
    let sign = added
        .spans
        .iter()
        .find(|span| span.text.trim() == "+")
        .expect("an added sign");
    assert_eq!(sign.fg, app.theme.diff.added);
    assert_eq!(sign.bg, Some(app.theme.diff.added_bg));

    let removed = lines
        .iter()
        .find(|line| line.text().contains("- ") && line.text().contains("=> \"working\""))
        .expect("a removed row");
    let sign = removed
        .spans
        .iter()
        .find(|span| span.text.trim() == "-")
        .expect("a removed sign");
    assert_eq!(sign.fg, app.theme.diff.removed);
    assert_eq!(sign.bg, Some(app.theme.diff.removed_bg));
}

#[test]
fn review_emphasizes_the_words_that_actually_changed() {
    let app = review_app_unified();
    let lines = app.lines((120, 30));
    let added = lines
        .iter()
        .find(|line| line.text().contains("println!(\"total = {}\", total);"))
        .expect("the reworded line");
    // "total = " is new; the surrounding call is not. The changed run gets a
    // stronger fill than the rest of the row.
    let mut backgrounds: Vec<Option<Color>> = added
        .spans
        .iter()
        .filter(|span| !span.text.trim().is_empty())
        .map(|span| span.bg)
        .collect();
    backgrounds.dedup();
    backgrounds.sort_by_key(|color| color.map(|color| color.components()));
    backgrounds.dedup();
    assert!(
        backgrounds.len() > 1,
        "changed words need their own fill: {added:#?}"
    );
}

#[tokio::test]
async fn review_cursor_walks_changes_and_files() {
    let mut app = review_app();
    let first = viewer_of(&app).cursor_line();
    assert!(first.is_some());
    let first_file = viewer_of(&app).active_file;

    review_key(&mut app, KeyCode::Char('n')).await;
    let after_change = viewer_of(&app).cursor_line();
    assert_ne!(first, after_change, "n moves to the next edit");

    review_key(&mut app, KeyCode::Char(']')).await;
    assert_ne!(
        viewer_of(&app).active_file,
        first_file,
        "] selects the next file"
    );
    review_key(&mut app, KeyCode::Char('[')).await;
    assert_eq!(viewer_of(&app).active_file, first_file);

    // j/k step one row and never land on a header or spacer.
    review_key(&mut app, KeyCode::Char('j')).await;
    let cursor = viewer_of(&app).cursor.unwrap();
    assert!(viewer_of(&app).display[cursor].selectable());
}

#[tokio::test]
async fn review_switches_focus_and_opens_a_file_from_the_tree() {
    let mut app = review_app();
    review_key(&mut app, KeyCode::Tab).await;
    assert_eq!(viewer_of(&app).focus, crate::diffview::DiffFocus::Tree);
    // Walk to the README row and open it.
    for _ in 0..8 {
        if viewer_of(&app)
            .tree_rows()
            .get(viewer_of(&app).tree_selection)
            .and_then(|row| row.file)
            .and_then(|file| viewer_of(&app).files.get(file))
            .is_some_and(|file| file.file == "README.md")
        {
            break;
        }
        review_key(&mut app, KeyCode::Char('j')).await;
    }
    review_key(&mut app, KeyCode::Enter).await;
    assert_eq!(
        viewer_of(&app).files[viewer_of(&app).active_file].file,
        "README.md"
    );
    assert_eq!(viewer_of(&app).focus, crate::diffview::DiffFocus::Patch);
}

#[tokio::test]
async fn review_folds_and_unfolds_directories_from_the_tree() {
    let mut app = review_app();
    review_key(&mut app, KeyCode::Tab).await;
    let open = viewer_of(&app).tree_rows().len();
    viewer_mut(&mut app).tree_selection = 0;
    review_key(&mut app, KeyCode::Left).await;
    assert!(viewer_of(&app).tree_rows().len() < open, "left folds");
    review_key(&mut app, KeyCode::Right).await;
    assert_eq!(viewer_of(&app).tree_rows().len(), open, "right unfolds");
}

#[tokio::test]
async fn review_toggles_layout_single_file_and_the_tree() {
    let mut app = review_app();
    assert_eq!(
        viewer_of(&app).layout_in_use(),
        crate::diffview::DiffLayout::Split,
        "a wide pane opens side by side"
    );
    review_key(&mut app, KeyCode::Char('v')).await;
    assert_eq!(
        viewer_of(&app).layout_in_use(),
        crate::diffview::DiffLayout::Unified
    );

    let stacked = viewer_of(&app).display.len();
    review_key(&mut app, KeyCode::Char('o')).await;
    assert!(
        viewer_of(&app).display.len() < stacked,
        "o shows only the selected patch"
    );

    review_key(&mut app, KeyCode::Char('t')).await;
    assert!(!viewer_of(&app).tree_showing());
    let rows: Vec<String> = app.lines((120, 30)).iter().map(line_text).collect();
    assert!(
        !rows.iter().any(|row| row.contains("└─")),
        "the sidebar is gone: {rows:#?}"
    );
}

#[tokio::test]
async fn review_marks_files_reviewed_and_dims_them() {
    let mut app = review_app();
    review_key(&mut app, KeyCode::Char('r')).await;
    let marked = viewer_of(&app).active_file;
    assert!(viewer_of(&app).is_reviewed(marked));
    let rows: Vec<String> = app.lines((120, 30)).iter().map(line_text).collect();
    assert!(
        rows.iter().any(|row| row.contains("reviewed")),
        "the file header says so: {rows:#?}"
    );
    review_key(&mut app, KeyCode::Char('r')).await;
    assert!(!viewer_of(&app).is_reviewed(marked));
}

#[tokio::test]
async fn review_comments_queue_on_a_line_and_land_in_the_composer() {
    let mut app = review_app();
    review_key(&mut app, KeyCode::Char('c')).await;
    assert!(viewer_of(&app).comment_input.is_some(), "an input opens");
    for character in "tighten this".chars() {
        review_key(&mut app, KeyCode::Char(character)).await;
    }
    // The footer shows the draft while it is being typed.
    let rows: Vec<String> = app.lines((120, 30)).iter().map(line_text).collect();
    assert!(
        rows.last().unwrap().contains("tighten this"),
        "{:?}",
        rows.last()
    );
    review_key(&mut app, KeyCode::Enter).await;
    assert_eq!(viewer_of(&app).comments.len(), 1);
    let line = viewer_of(&app).comments[0].line.expect("a line number");
    let file = viewer_of(&app).comments[0].file.clone();

    // The header counts it and the row is marked.
    let rows: Vec<String> = app.lines((120, 30)).iter().map(line_text).collect();
    assert!(rows[0].contains("1 comment"), "{:?}", rows[0]);
    assert!(rows.iter().any(|row| row.contains('●')), "{rows:#?}");

    review_key(&mut app, KeyCode::Char('q')).await;
    assert!(matches!(app.overlay, crate::state::Overlay::None));
    let composed = app.composer.text();
    assert!(
        composed.contains(&format!("line {line} of {file}")),
        "{composed:?}"
    );
    assert!(composed.contains("tighten this"), "{composed:?}");
}

#[tokio::test]
async fn review_help_lists_every_shortcut_and_any_key_returns() {
    let mut app = review_app();
    review_key(&mut app, KeyCode::Char('?')).await;
    let rows: Vec<String> = app.lines((120, 30)).iter().map(line_text).collect();
    assert!(rows.iter().any(|row| row.contains("Review shortcuts")));
    for expected in ["Switch source", "Cycle context", "Mark reviewed", "Comment"] {
        assert!(
            rows.iter().any(|row| row.contains(expected)),
            "{expected} missing from help"
        );
    }
    review_key(&mut app, KeyCode::Esc).await;
    assert!(
        matches!(app.overlay, crate::state::Overlay::Diff(_)),
        "help closes back to the review, not out of it"
    );
}

#[tokio::test]
async fn review_closes_on_escape_without_comments() {
    let mut app = review_app();
    review_key(&mut app, KeyCode::Esc).await;
    assert!(matches!(app.overlay, crate::state::Overlay::None));
    assert!(app.composer.text().is_empty());
}

#[test]
fn review_click_on_a_tree_row_opens_that_file() {
    let mut app = review_app();
    let lines = app.lines((120, 30));
    let hits = app.hit_rows.borrow().clone();
    let readme_row = lines
        .iter()
        .position(|line| line.text().contains("README.md") && line.text().contains("└─"))
        .expect("a README row in the tree");
    let hit = hits
        .iter()
        .find(|hit| {
            hit.row == readme_row
                && matches!(hit.target, crate::view::frame::HitTarget::DiffTreeRow(_))
        })
        .expect("the tree row is clickable")
        .clone();
    drop(lines);
    let crate::view::frame::HitTarget::DiffTreeRow(index) = hit.target else {
        unreachable!()
    };
    viewer_mut(&mut app).focus = crate::diffview::DiffFocus::Tree;
    viewer_mut(&mut app).tree_selection = index;
    viewer_mut(&mut app).activate_tree_row();
    assert_eq!(
        viewer_of(&app).files[viewer_of(&app).active_file].file,
        "README.md"
    );
}

#[test]
fn review_falls_back_to_a_summary_card_on_a_tiny_terminal() {
    let app = review_app();
    let rows: Vec<String> = app.lines((36, 14)).iter().map(line_text).collect();
    assert!(
        rows.iter().any(|row| row.contains("Widen the terminal")),
        "{rows:#?}"
    );
    assert!(rows.iter().any(|row| row.contains("3 files")), "{rows:#?}");
}

// ---------------------------------------------------------------------------
// Dialog design: how the shared modal card lays choices out
// ---------------------------------------------------------------------------

/// The column a row's muted description starts at, or None when the row has
/// no second column.
fn detail_column(line: &Line) -> Option<usize> {
    let mut column = 0;
    let mut seen_label = false;
    for span in &line.spans {
        if span.attributes & attr::BOLD == attr::BOLD && !span.text.trim().is_empty() {
            seen_label = true;
        } else if seen_label && !span.text.trim().is_empty() {
            return Some(column + span.text.len() - span.text.trim_start().len());
        }
        column += span.width();
    }
    None
}

#[test]
fn wide_pickers_put_each_choice_on_one_aligned_row() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Commands);
    let lines = app.lines((120, 34));

    let new_row = lines
        .iter()
        .find(|line| line.text().contains("/new"))
        .expect("the /new row");
    assert!(
        new_row.text().contains("Start a new thread"),
        "label and description share one row: {:?}",
        new_row.text()
    );

    // Every visible choice starts its description in the same column, so the
    // list reads as two columns rather than a ragged edge. Only rows painted
    // on the card surface count; the composer's own hints are not choices.
    let choice_rows: std::collections::BTreeSet<usize> = app
        .hit_rows
        .borrow()
        .iter()
        .filter(|hit| matches!(hit.target, crate::view::frame::HitTarget::MenuRow(_)))
        .map(|hit| hit.row)
        .collect();
    let columns: Vec<usize> = choice_rows
        .iter()
        .filter_map(|row| lines.get(*row))
        .filter_map(detail_column)
        .collect();
    assert!(columns.len() >= 5, "expected several command rows");
    assert!(
        columns.windows(2).all(|pair| pair[0] == pair[1]),
        "descriptions are not aligned: {columns:?}"
    );

    // Density: the whole command set is worth more than a handful of rows.
    let visible = choice_rows.len();
    assert!(visible >= 14, "only {visible} commands visible at 120x34");
}

#[test]
fn narrow_pickers_give_each_description_its_own_line() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Commands);
    let lines = app.lines((46, 30));
    let text = lines.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert!(text.contains("/new"), "{text}");
    let stacked = lines
        .iter()
        .any(|line| line.text().contains("Start a new thread") && !line.text().contains("/new"));
    assert!(stacked, "a narrow card stacks the description: {text}");
}

#[test]
fn the_theme_picker_shows_each_palette_rather_than_only_naming_it() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Themes);
    let lines = app.lines((120, 34));
    let chips_of = |name: &str| -> Vec<Color> {
        lines
            .iter()
            .find(|line| line.text().contains(name) && line.text().contains('\u{2588}'))
            .unwrap_or_else(|| panic!("no swatch row for {name}"))
            .spans
            .iter()
            .filter(|span| span.text.starts_with('\u{2588}'))
            // Adjacent chips of the same colour merge into one span, so read
            // the chips per character rather than per span.
            .flat_map(|span| span.text.chars().map(move |_| span.fg))
            .collect()
    };
    let palette = |name: &str| {
        app.themes
            .iter()
            .find(|entry| entry.name == name)
            .and_then(|entry| entry.dark)
            .unwrap_or_else(|| panic!("no {name} theme"))
    };

    // Read unselected rows: the current theme is painted with the focus fill
    // and its palette is already on screen as the live preview.
    let aura = palette("aura");
    assert_eq!(
        chips_of("aura"),
        vec![aura.bg, aura.text, aura.primary, aura.accent],
        "the page, its type, and the two colours the interface leans on"
    );
    // Every row previews itself rather than repeating one palette down the list.
    let ayu = palette("ayu");
    assert_eq!(
        chips_of("ayu"),
        vec![ayu.bg, ayu.text, ayu.primary, ayu.accent]
    );
    assert_ne!(chips_of("aura"), chips_of("ayu"));
}

#[test]
fn panels_read_as_an_aligned_table_and_keep_their_own_separators() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Panel {
        title: "Status".into(),
        lines: vec![
            "Server  running  \u{00b7}  http://127.0.0.1:8080".into(),
            "Workspace  /Users/alex/work/haarness".into(),
            "Branch  main  \u{00b7}  3 files changed".into(),
        ],
    });
    let lines = app.lines((120, 34));
    let text = lines.iter().map(Line::text).collect::<Vec<_>>().join("\n");
    assert!(
        !text.contains("\u{00b7}  \u{00b7}"),
        "a caller's own separators must not be doubled: {text}"
    );
    for label in ["Server", "Workspace", "Branch"] {
        let row = lines
            .iter()
            .find(|line| line.text().contains(label))
            .unwrap_or_else(|| panic!("missing {label}"));
        assert!(
            row.text().contains("http") || row.text().contains('/') || row.text().contains("main"),
            "label and value share one row: {:?}",
            row.text()
        );
    }
    let columns: Vec<usize> = ["Server", "Workspace", "Branch"]
        .iter()
        .filter_map(|label| {
            lines
                .iter()
                .find(|line| line.text().contains(label))
                .and_then(detail_column)
        })
        .collect();
    assert_eq!(columns.len(), 3);
    assert!(
        columns.windows(2).all(|pair| pair[0] == pair[1]),
        "panel values are not aligned: {columns:?}"
    );
}

#[test]
fn report_sections_are_marked_by_a_labelled_rule() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let run = fixture_workflow_run("release", haarness_protocol::WorkflowRunStatus::Running);
    app.workflow_runs = vec![run.clone()];
    app.open_overlay(crate::state::Overlay::Workflow {
        target: crate::state::WorkflowTarget::Run(run.id),
    });
    let lines = app.lines((120, 34));
    let heading = lines
        .iter()
        .find(|line| line.text().contains("LOGS"))
        .expect("a logs section");
    assert!(
        heading.text().contains('\u{2500}'),
        "the label runs out to a hairline: {:?}",
        heading.text()
    );
    let label = heading
        .spans
        .iter()
        .find(|span| span.text.contains("LOGS"))
        .expect("the label span");
    assert_eq!(label.fg, app.theme.primary);
    assert_eq!(label.attributes & attr::BOLD, attr::BOLD);
}

#[tokio::test]
async fn the_timeline_lists_undoable_checkpoints_by_the_request_that_made_them() {
    let thread = fixture_thread();
    let now = Utc::now();
    let mut sequence = 0_u64;
    let mut items = Vec::new();
    let mut push = |data: ItemData, sequence: &mut u64| {
        *sequence += 1;
        items.push(Item {
            id: ItemId::new(),
            thread_id: thread.id,
            turn_id: None,
            sequence: *sequence,
            created_at: now,
            data,
        });
    };
    // A checkpoint's boundary is the sequence just before the request that
    // opened its turn, exactly as the runtime records it.
    for (prompt, boundary) in [("rename the module", 0_u64), ("now add tests", 2_u64)] {
        push(
            ItemData::UserMessage {
                text: prompt.into(),
                attachments: Vec::new(),
                synthetic: false,
                queued: false,
            },
            &mut sequence,
        );
        push(
            ItemData::Snapshot {
                snapshot_id: format!("after-{boundary}"),
                file_count: 1,
                boundary_sequence: Some(boundary),
                base_snapshot_id: Some(format!("before-{boundary}")),
            },
            &mut sequence,
        );
    }
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);

    app.open_timeline_overlay();
    let crate::state::Overlay::Options { entries, kind, .. } = &app.overlay else {
        panic!("the timeline did not open");
    };
    assert!(matches!(kind, crate::state::OptionsKind::Timeline));
    // Newest first, each named by the request the checkpoint sits in front of.
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].label, "now add tests");
    assert_eq!(entries[1].label, "rename the module");
    assert_eq!(entries[0].value, "2");
    assert_eq!(entries[1].value, "0");
    assert!(entries[0].description.contains("before this request"));

    let rows: Vec<String> = app.lines((120, 34)).iter().map(line_text).collect();
    assert!(rows.iter().any(|row| row.contains("Timeline")), "{rows:#?}");
    assert!(
        rows.iter().any(|row| row.contains("rename the module")),
        "{rows:#?}"
    );
}

#[test]
fn the_timeline_says_so_when_a_session_has_no_checkpoints() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.open_timeline_overlay();
    assert!(matches!(app.overlay, crate::state::Overlay::None));
    assert!(
        app.notice.contains("No folder checkpoints"),
        "{}",
        app.notice
    );
}

#[test]
fn undo_and_redo_report_how_far_the_history_reaches() {
    let thread = fixture_thread();
    let now = Utc::now();
    let items: Vec<Item> = (1..=3)
        .map(|index| Item {
            id: ItemId::new(),
            thread_id: thread.id,
            turn_id: None,
            sequence: index * 2,
            created_at: now,
            data: ItemData::Snapshot {
                snapshot_id: format!("after-{index}"),
                file_count: 1,
                boundary_sequence: Some(index * 2 - 1),
                base_snapshot_id: Some(format!("before-{index}")),
            },
        })
        .collect();
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);

    // Nothing undone yet: three steps back, none forward.
    assert_eq!(app.undo_depth(), (3, 0));
    // Standing at the middle checkpoint: one more back, two forward.
    app.thread.revert = Some(haarness_protocol::RevertState {
        sequence: 3,
        snapshot_id: "backup".into(),
        base_snapshot_id: Some("base".into()),
    });
    assert_eq!(app.undo_depth(), (1, 2));
    // At the earliest checkpoint there is nowhere further back to go.
    app.thread.revert = Some(haarness_protocol::RevertState {
        sequence: 1,
        snapshot_id: "backup".into(),
        base_snapshot_id: Some("base".into()),
    });
    assert_eq!(app.undo_depth(), (0, 3));
}

#[tokio::test]
async fn auto_mode_replies_once_including_background_and_child_requests_and_preserves_review() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let (client, server) =
        workspace_http_status_client(vec![(200, "{}".into()), (200, "{}".into())], false).await;
    let old = fixture_event(
        "permission.requested",
        thread.id,
        thread.id,
        serde_json::json!({"permissionId":"old", "permission":"bash"}),
    );
    app.reduce(old.clone(), &client).await.unwrap();
    app.set_auto_permissions(true);
    app.reduce(old, &client).await.unwrap();
    assert_eq!(
        app.pending_permissions.len(),
        1,
        "pre-existing prompts remain manual"
    );
    let background = ThreadId::new();
    for (id, owner, root) in [
        ("background", background, background),
        ("child", ThreadId::new(), thread.id),
    ] {
        let event = fixture_event(
            "permission.requested",
            owner,
            root,
            serde_json::json!({"permissionId":id,"permission":"bash"}),
        );
        app.reduce(event.clone(), &client).await.unwrap();
        app.reduce(event, &client).await.unwrap();
    }
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.contains("\"decision\":\"once\""))
    );
    assert_eq!(
        app.pending_permissions.len(),
        1,
        "replayed approved prompts must not return"
    );
    app.set_auto_permissions(false);
    app.reduce(
        fixture_event(
            "permission.requested",
            thread.id,
            thread.id,
            serde_json::json!({"permissionId":"manual","permission":"bash"}),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.pending_permissions.len(), 2);
}

#[tokio::test]
async fn auto_mode_failure_stays_manual_and_allow_once_auto_requires_success() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.set_auto_permissions(true);
    let event = fixture_event(
        "permission.requested",
        thread.id,
        thread.id,
        serde_json::json!({"permissionId":"failed","permission":"bash"}),
    );
    let (client, server) = workspace_http_status_client(vec![(503, "{}".into())], false).await;
    app.reduce(event.clone(), &client).await.unwrap();
    app.reduce(event, &offline_client()).await.unwrap();
    assert_eq!(server.await.unwrap().len(), 1);
    assert_eq!(app.pending_permissions.len(), 1);
    app.set_auto_permissions(false);
    // Once + Auto changes the whole chat, so the first press only stages it.
    crate::input::answer_permission(&mut app, &offline_client(), 'u')
        .await
        .unwrap();
    assert_eq!(app.confirm_always, Some('u'));
    crate::input::answer_permission(&mut app, &offline_client(), 'u')
        .await
        .unwrap();
    assert!(!app.auto_permissions);
    assert_eq!(app.pending_permissions.len(), 1);
    let (client, server) = workspace_http_status_client(vec![(200, "{}".into())], false).await;
    for _ in 0..2 {
        crate::input::answer_permission(&mut app, &client, 'u')
            .await
            .unwrap();
    }
    assert!(app.auto_permissions);
    assert!(app.pending_permissions.is_empty());
    assert!(server.await.unwrap()[0].contains("\"decision\":\"once\""));
}

/// `haarness --yolo`: the TUI answers what its own conversation asks, once,
/// even on a server with shared approval modes, and leaves other chats and
/// requests that always ask alone. The server never becomes a yolo server.
#[tokio::test]
async fn yolo_answers_its_own_conversation_once_and_leaves_the_rest() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.yolo = true;
    app.approval_server = true;
    let (client, server) =
        workspace_http_status_client(vec![(200, "{}".into()), (200, "{}".into())], false).await;
    let other_chat = ThreadId::new();
    for (id, owner, root, always) in [
        ("own", thread.id, thread.id, true),
        ("child", ThreadId::new(), thread.id, true),
        ("other-chat", other_chat, other_chat, true),
        ("always-asks", thread.id, thread.id, false),
    ] {
        let event = fixture_event(
            "permission.requested",
            owner,
            root,
            serde_json::json!({"permissionId":id,"permission":"external_directory","allowAlways":always}),
        );
        app.reduce(event.clone(), &client).await.unwrap();
        // A replayed event is not answered twice.
        app.reduce(event, &client).await.unwrap();
    }
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2, "{requests:#?}");
    assert!(requests[0].contains("/permissions/own"), "{}", requests[0]);
    assert!(
        requests[1].contains("/permissions/child"),
        "{}",
        requests[1]
    );
    assert!(
        requests
            .iter()
            .all(|request| request.contains("\"decision\":\"once\""))
    );
    let cards: Vec<_> = app
        .pending_permissions
        .iter()
        .map(|pending| pending.permission_id.as_str())
        .collect();
    assert_eq!(cards, ["always-asks"]);
    assert!(frame_text(&app, (140, 40)).contains("approvals: --yolo"));

    // Opening a chat with requests already waiting answers its own only
    // (the first response is the server info hydration reads).
    let (client, server) =
        workspace_http_status_client(vec![(200, "{}".into()), (200, "{}".into())], false).await;
    let waiting = |id: &str, thread_id, root| haarness_protocol::PendingPermissionInfo {
        allow_always: None,
        repository_scope: None,
        permission_id: id.into(),
        operation: "external_directory:/tmp/x".into(),
        detail: serde_json::Value::Null,
        permission: "external_directory".into(),
        patterns: Vec::new(),
        always_patterns: Vec::new(),
        thread_id,
        root_thread_id: Some(root),
        turn_id: None,
        agent: None,
        created_at: chrono::Utc::now(),
    };
    app.hydrate_permissions(
        vec![
            waiting("waiting", thread.id, thread.id),
            waiting("elsewhere", other_chat, other_chat),
        ],
        &client,
    )
    .await;
    let requests = server.await.unwrap();
    assert!(
        requests[1].contains("/permissions/waiting"),
        "{requests:#?}"
    );
    assert!(app.pending_permissions.is_empty());
}

#[tokio::test]
async fn a_yolo_client_flags_the_turns_it_starts_and_a_plain_one_does_not() {
    for yolo in [true, false] {
        let (client, server) = workspace_http_status_client(vec![(200, "{}".into())], false).await;
        let client = client.with_auto_approve(yolo);
        assert_eq!(client.auto_approves(), yolo);
        // `{}` is not a turn; only the request matters here.
        let _ = client
            .start_turn(
                ThreadId::new(),
                haarness_protocol::StartTurnRequest {
                    request_id: haarness_protocol::RequestId::new(),
                    text: "hi".into(),
                    attachments: Vec::new(),
                    synthetic: false,
                    auto_approve: false,
                    agent: None,
                    provider: None,
                    model: None,
                    reasoning_effort: None,
                    tools: None,
                    command: None,
                },
            )
            .await;
        let requests = server.await.unwrap();
        assert_eq!(
            requests[0].contains("\"autoApprove\":true"),
            yolo,
            "{}",
            requests[0]
        );
    }
}

#[tokio::test]
async fn auto_mode_shortcut_and_restricted_requests_are_manual() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let client = offline_client();
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert!(app.auto_permissions);
    for (id, permission, always, once) in [
        ("import", "instruction_import", true, true),
        ("restricted", "bash", false, true),
        ("permanent", "bash", true, false),
    ] {
        app.reduce(fixture_event("permission.requested",thread.id,thread.id,serde_json::json!({"permissionId":id,"permission":permission,"allowAlways":always,"allowOnce":once})), &client).await.unwrap();
    }
    assert_eq!(app.pending_permissions.len(), 3);
    assert!(app.auto_permissions_seen.values().all(|approved| !approved));
    assert!(frame_text(&app, (140, 40)).contains("approvals: auto"));
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
        &client,
    )
    .await
    .unwrap();
    assert!(!app.auto_permissions);
}

#[test]
fn model_picker_shows_the_long_context_price() {
    let mut app = selector_fixture();
    app.providers[0].models[0].pricing = Some(haarness_protocol::ModelPricing {
        input: 0.1,
        output: 0.5,
        cache_read: 0.01,
        cache_write: 0.125,
        long_context: Some(haarness_protocol::LongContextPricing {
            above_input_tokens: 100_000,
            input: 0.5,
            output: 2.5,
            cache_read: 0.05,
            cache_write: 0.625,
        }),
    });
    let detail = crate::view::menu::model_detail(&app, "openai/gpt-5.4", "");
    assert!(
        detail.contains("$0.1/0.5 per MTok · $0.5/2.5 over 100K"),
        "{detail}"
    );
}

#[test]
fn model_picker_uses_friendly_names_and_only_connected_models() {
    let mut app = selector_fixture();
    app.providers[0].models[0].id = "gpt-6-astra".into();
    app.providers[0].models[0].display_name = "gpt-6-astra".into();
    let mut offline = app.providers[0].clone();
    offline.id = "disconnected".into();
    offline.authenticated = false;
    app.providers.push(offline);
    assert_eq!(app.model_label("openai/gpt-6-astra"), "GPT-6 Astra");
    assert!(
        app.filtered_models()
            .iter()
            .all(|model| !model.starts_with("disconnected/"))
    );
    app.overlay_filter = "astra".into();
    assert_eq!(app.filtered_models(), ["openai/gpt-6-astra"]);
    app.providers[0].authenticated = false;
    assert!(app.filtered_models().is_empty());
}

#[test]
fn management_passwords_never_render_in_fields_or_empty_search_results() {
    let mut app = selector_fixture();
    app.management.view = haarness_protocol::ManagementView {
        title: "Credentials".into(),
        fields: vec![haarness_protocol::ManagementField {
            key: "key".into(),
            label: "API key".into(),
            value: "credential-fixture-never-display".into(),
            required: true,
            secret: true,
            choices: vec![],
        }],
        ..Default::default()
    };
    app.show_management();
    assert!(
        !app.lines((110, 32))
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n")
            .contains("credential-fixture")
    );
    app.open_overlay(crate::state::Overlay::Options {
        title: "API key".into(),
        kind: crate::state::OptionsKind::ManagementField,
        entries: vec![],
    });
    app.overlay_filter = "credential-fixture-never-display".into();
    let rendered = app
        .lines((110, 32))
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("credential-fixture"));
    assert!(rendered.contains('•'));
}

#[test]
fn configurable_shortcuts_reject_collisions_and_protect_interrupt_keys() {
    use crate::keybindings::{parse, validate};
    let mut bindings = std::collections::BTreeMap::from([
        ("models".into(), "alt+m".into()),
        ("sessions".into(), "F6".into()),
    ]);
    validate(&bindings).unwrap();
    bindings.insert("sessions".into(), "alt+m".into());
    assert!(validate(&bindings).is_err());
    assert!(parse("ctrl+c").is_err());
    assert!(parse("q").is_err());
    assert!(parse("f14").is_err());
    assert_eq!(parse("none").unwrap(), None);
}

#[test]
fn prompt_context_snapshots_are_bounded_quoted_and_survive_composer_undo() {
    use crate::prompt_context::{range, snapshot};
    assert_eq!(range("src/a.rs:2-3"), ("src/a.rs", Some((2, 3))));
    assert_eq!(range("name:literal"), ("name:literal", None));
    assert!(snapshot("a", "one\ntwo", Some((1, 3))).is_err());
    let body = snapshot("a<file>", "one\n```\nthree\nfour", Some((2, 3))).unwrap();
    assert!(body.contains("a&lt;file&gt;:2-3"));
    assert!(body.contains("````\n```\nthree\n````"));
    assert!(!body.contains("four"));
    let mut composer = crate::input::composer::Composer::default();
    composer.set_text("Read @a".into());
    composer.attach_context(5, 7, "@a:2-3", &body).unwrap();
    assert_eq!(composer.text(), "Read [@a:2-3] ");
    assert!(composer.expanded_text().contains(&body));
    composer.undo();
    assert_eq!(composer.text(), "Read @a");
    composer.redo();
    assert!(composer.take().contains(&body));
    assert!(composer.history().last().unwrap().contains(&body));
    assert!(snapshot("a", &"x".repeat(128 * 1024 + 1), None).is_err());
}

#[test]
fn model_picker_prioritizes_favorites_then_recents_without_losing_search() {
    let mut app = selector_fixture();
    let first = "openai/gpt-5.4".to_owned();
    let second = "openai/gpt-5.4-mini".to_owned();
    app.model_recents = vec![second.clone()];
    app.model_favorites.clear();
    assert_eq!(app.filtered_models(), [second.clone(), first.clone()]);
    app.model_favorites = vec![first.clone()];
    assert_eq!(app.filtered_models(), [first, second.clone()]);
    app.overlay_filter = "mini".into();
    assert_eq!(app.filtered_models(), [second]);
}

#[test]
fn sent_references_read_as_their_chips_in_the_bubble_and_come_back_as_chips() {
    use crate::prompt_context::{Part, display, parts, snapshot};
    let file = snapshot(
        "src/todo/<store>.py",
        "def add():\n    return 1\n```\n",
        None,
    )
    .unwrap();
    let delegation = "Delegate the relevant work from this message and attached context using \
                      the task tool with agent \"explore\". Keep normal permissions in effect.";
    let sent = format!("Look at {file} and ask {delegation}\nthen report");
    assert_eq!(
        display(&sent),
        "Look at @src/todo/<store>.py and ask @explore\nthen report"
    );
    assert!(matches!(
        parts(&sent).as_slice(),
        [Part::Text("Look at "), Part::Reference { body, .. }, Part::Text(" and ask "), Part::Reference { .. }, Part::Text("\nthen report")]
            if *body == file
    ));
    // Desktop's shape: references after the text, an editor selection that
    // was not saved, and a quoted path.
    assert_eq!(
        display(
            "Fix this\n\n<details>\n<summary>File context: say &quot;hi&quot;.ts:2-3 (unsaved \
             editor snapshot)</summary>\n\n```\nhi()\n```\n\n</details>"
        ),
        "Fix this\n\n@say \"hi\".ts:2-3"
    );
    // Text that only looks like a reference stays as typed.
    for typed in [
        "<details>\n<summary>Notes</summary>\n\nmine\n\n</details>",
        "Delegate the relevant work later",
    ] {
        assert_eq!(display(typed), typed);
    }

    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::UserMessage {
            text: sent.clone(),
            attachments: vec![],
            queued: false,
            synthetic: false,
        },
    )];
    let app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let text = app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("▎ Look at @src/todo/<store>.py and ask @explore"),
        "{text}"
    );
    assert!(text.contains("▎ then report"), "{text}");
    for hidden in ["<details>", "def add", "Delegate the relevant work"] {
        assert!(
            !text.contains(hidden),
            "{hidden} leaked into the bubble: {text}"
        );
    }

    // Up brings the prompt back with its chips, and it sends unchanged.
    let mut composer = crate::input::composer::Composer::default();
    composer.set_text("Look at @sto".into());
    composer
        .attach_context(8, 12, "@src/todo/<store>.py", &file)
        .unwrap();
    composer.insert_str("and ask ");
    composer
        .attach_context(
            composer.text().len(),
            composer.text().len(),
            "@explore",
            delegation,
        )
        .unwrap();
    assert_eq!(
        composer.text(),
        "Look at [@src/todo/<store>.py] and ask [@explore] "
    );
    let sent = composer.take();
    assert!(sent.contains(&file) && sent.contains(delegation), "{sent}");
    assert!(composer.history_prev());
    assert_eq!(
        composer.text(),
        "Look at [@src/todo/<store>.py] and ask [@explore] "
    );
    assert_eq!(composer.expanded_text(), sent);
    composer.backspace();
    composer.backspace();
    assert_eq!(composer.text(), "Look at [@src/todo/<store>.py] and ask ");
}

#[test]
fn a_long_pasted_prompt_folds_to_its_opening_lines_until_clicked() {
    let thread = fixture_thread();
    let pasted = (1..=60)
        .map(|index| format!("pasted line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let items = vec![message_item(
        1,
        &thread,
        ItemData::UserMessage {
            text: format!("Summarise this:\n{pasted}"),
            attachments: vec![],
            queued: false,
            synthetic: false,
        },
    )];
    let id = items[0].id;
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    let text = |app: &App| {
        app.lines((100, 80))
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let folded = text(&app);
    assert!(folded.contains("▎ pasted line 7"), "{folded}");
    assert!(
        !folded
            .lines()
            .any(|row| row.trim_end().ends_with("pasted line 8")),
        "{folded}"
    );
    assert!(!folded.contains("pasted line 60"), "{folded}");
    assert!(
        folded.contains("▎ … +53 more lines · click to expand"),
        "{folded}"
    );
    assert!(
        app.hit_rows
            .borrow()
            .iter()
            .any(|hit| { hit.target == crate::view::frame::HitTarget::ToolToggle(id) })
    );

    app.expanded.insert(id);
    let open = text(&app);
    assert!(open.contains("▎ pasted line 60"), "{open}");
    assert!(open.contains("▎ … click to collapse"), "{open}");
}

#[test]
fn context_reference_deletion_is_atomic_and_undo_restores_its_payload() {
    let mut composer = crate::input::composer::Composer::default();
    composer
        .attach_context(0, 0, "sample.txt", "file payload")
        .unwrap();
    composer.backspace(); // trailing space
    composer.backspace(); // whole reference
    assert!(composer.text().is_empty());
    composer.undo();
    assert_eq!(composer.expanded_text(), "file payload");
}

#[test]
fn removed_context_does_not_consume_future_reference_capacity() {
    let mut composer = crate::input::composer::Composer::default();
    for _ in 0..25 {
        composer
            .attach_context(0, 0, "sample.txt", &"x".repeat(8192))
            .unwrap();
        composer.backspace();
        composer.backspace();
    }
    assert!(composer.text().is_empty());
}

#[tokio::test]
async fn deferred_queue_events_are_scoped_and_never_roll_back_newer_edits() {
    let thread = fixture_thread();
    let id = haarness_protocol::RequestId::new();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let payload = |revision, text: &str| serde_json::json!({"revision":revision,"items":[{"id":id,"text":text,"attachments":[]}]});
    app.reduce(
        fixture_event(
            "input.queue.updated",
            thread.id,
            thread.id,
            payload(3, "latest"),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    app.reduce(
        fixture_event(
            "input.queue.updated",
            thread.id,
            thread.id,
            payload(2, "stale"),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    app.reduce(
        fixture_event(
            "input.queue.updated",
            ThreadId::new(),
            thread.id,
            payload(4, "child"),
        ),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.input_queue.revision, 3);
    assert_eq!(app.input_queue.items[0].text, "latest");
    let rendered = frame_text(&app, (100, 30));
    assert!(
        rendered.contains("Queue paused — send when ready"),
        "{rendered}"
    );
    assert!(rendered.contains("1. latest"), "{rendered}");
    assert!(
        !crate::view::transcript::transcript_lines(&app, 100)
            .lines
            .iter()
            .any(|line| line_text(line).contains("latest"))
    );
}

fn queued_input(text: &str, attachments: &[&str]) -> haarness_protocol::PendingInput {
    haarness_protocol::PendingInput {
        preview: None,
        reasoning_effort: None,
        id: haarness_protocol::RequestId::new(),
        text: text.into(),
        attachments: attachments.iter().map(|path| (*path).to_owned()).collect(),
    }
}

/// A session with `texts` waiting in the deferred queue at revision 7.
fn queue_fixture(texts: &[&str]) -> App {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.input_queue = haarness_protocol::InputQueue {
        revision: 7,
        items: texts.iter().map(|text| queued_input(text, &[])).collect(),
    };
    app
}

fn queue_json(revision: u64, items: &[haarness_protocol::PendingInput]) -> String {
    serde_json::to_string(&haarness_protocol::InputQueue {
        revision,
        items: items.to_vec(),
    })
    .unwrap()
}

/// The rendered row whose text starts with `prefix`.
fn strip_row(app: &App, prefix: &str) -> (usize, Line) {
    app.lines((100, 30))
        .into_iter()
        .enumerate()
        .find(|(_, line)| line_text(line).starts_with(prefix))
        .unwrap_or_else(|| {
            panic!(
                "no row starts with {prefix:?}:\n{}",
                frame_text(app, (100, 30))
            )
        })
}

async fn click_hit(
    app: &mut App,
    target: &crate::view::frame::HitTarget,
    client: &crate::api::ApiClient,
) {
    app.lines((100, 30));
    let hit = app
        .hit_rows
        .borrow()
        .iter()
        .find(|hit| &hit.target == target)
        .cloned()
        .unwrap_or_else(|| panic!("{target:?} is not on screen"));
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::input::mouse::handle_mouse(
            app,
            MouseEvent {
                kind,
                column: u16::try_from(hit.start + 2).unwrap(),
                row: u16::try_from(hit.row).unwrap(),
                modifiers: KeyModifiers::NONE,
            },
            client,
        )
        .await
        .unwrap();
    }
}

fn option_labels(app: &App) -> Vec<String> {
    match &app.overlay {
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::InputQueueActions,
            entries,
            ..
        } => entries.iter().map(|entry| entry.label.clone()).collect(),
        _ => panic!("the queued message's actions are not open"),
    }
}

#[test]
fn queue_strip_header_names_the_working_paused_and_editing_states() {
    let mut app = queue_fixture(&["first", "second"]);
    let second = app.input_queue.items[1].id;

    app.active_turn = Some(TurnId::new());
    let working = frame_text(&app, (100, 30));
    assert!(working.contains("▎ Queued · 2"), "{working}");
    assert!(!working.contains("paused"), "{working}");
    // The strip's text shares the prompt's column, under a rail at the edge.
    strip_row(&app, "▎ 1. first");
    strip_row(&app, "▎ 2. second");
    assert!(working.contains("\n  Ask anything"), "{working}");

    app.active_turn = None;
    let paused = frame_text(&app, (100, 30));
    assert!(
        paused.contains("▎ Queue paused — send when ready"),
        "{paused}"
    );
    assert!(!paused.contains("Queued ·"), "{paused}");

    app.composer.set_text("half-written thought".into());
    app.edit_queued_message(second).unwrap();
    let editing = frame_text(&app, (100, 30));
    assert!(
        editing.contains("▎ Editing — Enter saves it in place"),
        "{editing}"
    );
    assert!(
        editing.contains("▎ Editing queued message · Enter saves · Click to cancel"),
        "{editing}"
    );
    assert!(!editing.contains("half-written thought"), "{editing}");
    // Only the message being edited is raised.
    let (_, edited) = strip_row(&app, "▎ 2. second");
    let (_, other) = strip_row(&app, "▎ 1. first");
    assert_eq!(cell_style(&edited, 2).unwrap().1, Some(app.theme.selected));
    assert_eq!(cell_style(&other, 2).unwrap().1, Some(app.theme.surface));
}

#[test]
fn queue_strip_numbers_three_messages_and_counts_the_rest() {
    let thread = fixture_thread();
    let steering = message_item(
        1,
        &thread,
        ItemData::UserMessage {
            text: "keep the public names".into(),
            attachments: vec![],
            queued: true,
            synthetic: false,
        },
    );
    let mut app = queue_fixture(&["one\nstill one", "two", "three", "four", "five"]);
    app.items = vec![steering];
    app.active_turn = Some(TurnId::new());

    let (steer_header, _) = strip_row(&app, "▎ Steering · 1 — joins at the next step");
    let (steer_row, _) = strip_row(&app, "▎ 1. keep the public names");
    let (queue_header, _) = strip_row(&app, "▎ Queued · 5");
    let (first, _) = strip_row(&app, "▎ 1. one still one");
    strip_row(&app, "▎ 2. two");
    let (third, _) = strip_row(&app, "▎ 3. three");
    let (more, _) = strip_row(&app, "▎ +2 more");
    assert!(steer_header < steer_row && steer_row < queue_header);
    assert!(queue_header < first && first < third && third < more);
    let text = frame_text(&app, (100, 30));
    assert!(!text.contains("4. four"), "{text}");

    // Steering is already with the agent: its rows are not controls. Queued
    // rows, the header, and the overflow line are.
    app.lines((100, 30));
    let targets = app
        .hit_rows
        .borrow()
        .iter()
        .map(|hit| (hit.row, hit.target.clone()))
        .collect::<Vec<_>>();
    let at = |row: usize| {
        targets
            .iter()
            .find(|(r, _)| *r == row)
            .map(|(_, t)| t.clone())
    };
    assert_eq!(at(steer_row), None);
    assert_eq!(
        at(queue_header),
        Some(crate::view::frame::HitTarget::QueueList)
    );
    assert_eq!(
        at(first),
        Some(crate::view::frame::HitTarget::QueueItem(
            app.input_queue.items[0].id
        ))
    );
    assert_eq!(at(more), Some(crate::view::frame::HitTarget::QueueList));
}

#[tokio::test]
async fn clicking_a_queued_row_opens_its_actions() {
    use crate::view::frame::HitTarget;

    let mut app = queue_fixture(&["first", "second", "third"]);
    let client = offline_client();
    let second = app.input_queue.items[1].id;
    app.active_turn = Some(TurnId::new());

    click_hit(&mut app, &HitTarget::QueueItem(second), &client).await;
    assert_eq!(
        option_labels(&app),
        ["Steer now", "Edit", "Remove", "Move up", "Move down"]
    );
    assert_eq!(app.input_queue_edit, Some((second, 7)));
    assert!(!app.input_queue_listed, "a strip click closes after acting");

    // Idle, sending runs the message rather than steering; the first row
    // cannot move up.
    app.close_overlay();
    app.active_turn = None;
    let first = app.input_queue.items[0].id;
    click_hit(&mut app, &HitTarget::QueueItem(first), &client).await;
    assert_eq!(
        option_labels(&app),
        ["Send now", "Edit", "Remove", "Move down"]
    );

    // Edit loads the message into the prompt rather than a dialog.
    app.composer.set_text("my draft".into());
    app.input_queue_action("edit", &client).await.unwrap();
    assert!(matches!(app.overlay, crate::state::Overlay::None));
    assert_eq!(app.composer.text(), "first");
    assert_eq!(app.queue_edit.as_ref().map(|edit| edit.id), Some(first));

    // The editing line cancels on click and brings the draft back.
    click_hit(&mut app, &HitTarget::QueueEditCancel, &client).await;
    assert!(app.queue_edit.is_none());
    assert_eq!(app.composer.text(), "my draft");
}

#[tokio::test]
async fn saving_a_queued_edit_keeps_its_place_and_sends_its_attachments() {
    let mut app = queue_fixture(&["first", "second", "third"]);
    app.input_queue.items[1].attachments = vec!["/tmp/plan.png".into()];
    let second = app.input_queue.items[1].id;
    let mut saved = app.input_queue.items.clone();
    saved[1].text = "second, with more".into();
    saved[1].attachments = vec!["/tmp/plan.png".into(), "/tmp/notes.pdf".into()];
    // The server answers exactly one request: anything else (a Steer that
    // would start the paused queue, say) fails the key press.
    let (client, server) =
        workspace_http_status_client(vec![(200, queue_json(8, &saved))], false).await;

    app.composer.set_text("my draft".into());
    app.attachments = vec!["/tmp/draft.png".into()];
    app.open_queue_item_actions(second, false).unwrap();
    app.input_queue_action("edit", &client).await.unwrap();
    assert_eq!(app.composer.text(), "second");
    assert_eq!(app.attachments, ["/tmp/plan.png"]);

    app.composer.move_end();
    app.composer.insert_str(", with more");
    app.attachments.push("/tmp/notes.pdf".into());
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();

    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].starts_with(&format!("POST /v1/threads/{}/input/queue ", app.thread.id)),
        "{}",
        requests[0]
    );
    let body: serde_json::Value =
        serde_json::from_str(requests[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(
        body,
        serde_json::json!({
            "action": "edit",
            "revision": 7,
            "id": second,
            "text": "second, with more",
            "attachments": ["/tmp/plan.png", "/tmp/notes.pdf"],
        })
    );
    // Same position, the draft is back, and a paused queue stays paused.
    assert_eq!(app.input_queue.items[1].id, second);
    assert_eq!(app.input_queue.items[1].text, "second, with more");
    assert!(app.queue_edit.is_none());
    assert_eq!(app.composer.text(), "my draft");
    assert_eq!(app.attachments, ["/tmp/draft.png"]);
    assert_eq!(app.notice, "Queued message saved");
    let text = frame_text(&app, (100, 30));
    assert!(text.contains("Queue paused — send when ready"), "{text}");
    assert!(text.contains("2. second, with more"), "{text}");
}

#[tokio::test]
async fn esc_cancels_a_queued_edit_and_never_interrupts_running_work() {
    let mut app = queue_fixture(&["first", "second"]);
    let turn = TurnId::new();
    app.active_turn = Some(turn);
    app.composer.set_text("my draft".into());
    app.attachments = vec!["/tmp/draft.png".into()];
    let second = app.input_queue.items[1].id;
    app.edit_queued_message(second).unwrap();
    // An Esc pressed just before editing began must not pair with this one.
    app.escape_interrupt = Some((
        crate::state::InterruptTarget::Turn(turn),
        std::time::Instant::now(),
    ));

    press_key(&mut app, KeyCode::Esc).await;
    assert!(app.queue_edit.is_none());
    assert_eq!(app.composer.text(), "my draft");
    assert_eq!(app.attachments, ["/tmp/draft.png"]);
    assert!(app.escape_interrupt.is_none());
    assert!(app.phase.is_none(), "Esc reached the running turn");
    assert_eq!(app.notice, "Edit cancelled · draft restored");
    assert!(!frame_text(&app, (100, 30)).contains("Editing"));

    // With the edit gone, Esc is the interrupt key again, armed by one press.
    press_key(&mut app, KeyCode::Esc).await;
    assert!(app.escape_interrupt.is_some());
    assert!(app.phase.is_none());

    // Ctrl+C cancels an edit the same way instead of clearing or stopping.
    app.escape_interrupt = None;
    app.edit_queued_message(second).unwrap();
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(app.queue_edit.is_none());
    assert_eq!(app.composer.text(), "my draft");
    assert!(app.ctrl_c_action.is_none());
}

#[tokio::test]
async fn a_conflicting_queued_edit_keeps_the_text_in_the_prompt() {
    let mut app = queue_fixture(&["first", "second"]);
    app.active_turn = Some(TurnId::new());
    let second = app.input_queue.items[1].id;
    let remaining = vec![app.input_queue.items[0].clone()];
    let (client, server) = workspace_http_status_client(
        vec![
            (
                409,
                r#"{"code":"conflict","message":"This message has already been sent or removed"}"#
                    .into(),
            ),
            (200, queue_json(9, &remaining)),
        ],
        false,
    )
    .await;

    app.composer.set_text("my draft".into());
    app.edit_queued_message(second).unwrap();
    app.composer.set_text("second, reworded".into());
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();

    let requests = server.await.unwrap();
    assert!(requests[0].starts_with("POST "), "{}", requests[0]);
    assert!(requests[1].starts_with("GET "), "{}", requests[1]);
    assert_eq!(app.composer.text(), "second, reworded");
    assert!(
        app.notice.contains("Already sent or removed"),
        "{}",
        app.notice
    );
    let edit = app.queue_edit.as_ref().expect("still editing");
    assert_eq!(edit.id, second);
    assert_eq!(app.input_queue.revision, 9);
    let text = frame_text(&app, (100, 30));
    assert!(
        text.contains("▎ Already sent or removed · Esc or click restores your draft"),
        "{text}"
    );

    press_key(&mut app, KeyCode::Esc).await;
    assert_eq!(app.composer.text(), "my draft");
    assert!(app.phase.is_none());
}

#[tokio::test]
async fn a_queued_edit_never_overwrites_a_version_changed_elsewhere() {
    let mut app = queue_fixture(&["first", "second"]);
    app.active_turn = Some(TurnId::new());
    let second = app.input_queue.items[1].id;
    // No request is expected: the refusal happens before anything is sent.
    let (client, server) = workspace_http_status_client(vec![], false).await;

    app.composer.set_text("my draft".into());
    app.edit_queued_message(second).unwrap();
    app.composer.set_text("second, reworded".into());
    // Another client rewrites the same message while this one edits it.
    app.input_queue.items[1].text = "second, by someone else".into();
    app.input_queue.revision += 1;
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();

    assert!(server.await.unwrap().is_empty());
    assert_eq!(app.composer.text(), "second, reworded");
    assert!(app.notice.contains("Changed elsewhere"), "{}", app.notice);
    assert_eq!(app.queue_edit.as_ref().map(|edit| edit.id), Some(second));
    press_key(&mut app, KeyCode::Esc).await;
    assert_eq!(app.composer.text(), "my draft");
}

#[test]
fn language_changes_refresh_root_and_child_chrome_without_translating_chat() {
    for (language, _, _) in crate::locale::Language::ALL {
        let thread = fixture_thread();
        let mut app = App::new(
            thread.clone(),
            vec![thread.clone()],
            vec![message_item(
                1,
                &thread,
                ItemData::AssistantMessage {
                    text: "Keep this model response unchanged.".into(),
                },
            )],
            vec![],
            vec![],
        );
        app.language = language;
        app.notice = "Ready".into();
        let screen = app
            .lines((170, 45))
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains(language.text("Ask anything…")), "{screen}");
        assert!(screen.contains(language.text("ready")), "{screen}");
        assert!(
            screen.contains("Keep this model response unchanged."),
            "{screen}"
        );
        let (mut child, _, _, _) = child_navigation_fixture();
        child.language = language;
        let screen = child
            .lines((170, 45))
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains(language.text("child")), "{screen}");
    }
}

fn connect_fixture() -> App {
    let thread = fixture_thread();
    let provider = |id: &str, name: &str, authenticated: bool| ProviderInfo {
        id: id.into(),
        name: name.into(),
        authenticated,
        models: vec![haarness_protocol::ModelInfo {
            id: "m-1".into(),
            display_name: "Model 1".into(),
            reasoning: false,
            reasoning_efforts: vec![],
            images: false,
            video: false,
            tools: true,
            context_window: None,
            max_output_tokens: None,
            pricing: None,
        }],
    };
    App::new(
        thread.clone(),
        vec![thread],
        vec![],
        vec![
            provider("openai", "OpenAI", false),
            provider("anthropic", "Anthropic", false),
        ],
        vec![],
    )
}

#[tokio::test]
async fn connect_picker_lists_providers_and_falls_back_to_a_key_prompt_offline() {
    let mut app = connect_fixture();
    let client = offline_client();
    app.execute_command("/connect", &client).await.unwrap();
    let crate::state::Overlay::Options {
        kind,
        entries,
        title,
    } = app.overlay.clone()
    else {
        panic!("connect opens the provider picker");
    };
    assert!(matches!(kind, crate::state::OptionsKind::ConnectProvider));
    // Nothing works yet, so the picker is the welcome screen and says why.
    assert_eq!(title, "Welcome to Haarness");
    let rendered = app
        .lines((100, 30))
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Haarness needs an AI model."),
        "{rendered}"
    );
    let labels: Vec<_> = entries.iter().map(|entry| entry.label.as_str()).collect();
    assert!(labels.contains(&"OpenAI"), "{labels:?}");
    assert!(labels.contains(&"Anthropic"), "{labels:?}");
    assert!(labels.contains(&"Custom endpoint…"), "{labels:?}");
    assert!(labels.contains(&"Manage providers…"), "{labels:?}");
    assert!(
        app.connect.onboarding,
        "nothing is connected, so this is onboarding"
    );

    // Offline the server cannot describe sign-in methods, so the key prompt
    // is the only door and opens directly.
    app.selection = labels.iter().position(|label| *label == "OpenAI").unwrap();
    let choice = app.selected_overlay_choice().unwrap();
    app.close_overlay();
    app.execute_choice(choice, &client).await.unwrap();
    assert!(
        matches!(
            &app.overlay,
            crate::state::Overlay::Options { kind: crate::state::OptionsKind::ConnectApiKey, title, .. }
                if title == "Connect OpenAI"
        ),
        "the key prompt is named after the provider"
    );
    app.overlay_filter = "sk-secret-never-shown".into();
    let rendered = app
        .lines((100, 30))
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("sk-secret"), "{rendered}");
    assert!(rendered.contains('•'), "{rendered}");
    assert!(rendered.contains("enter connect"), "{rendered}");

    // Escape goes back to the picker instead of dropping the whole flow.
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(
        &app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::ConnectProvider,
            ..
        }
    ));

    // A named provider skips the picker.
    app.execute_command("/connect anthropic", &client)
        .await
        .unwrap();
    assert!(matches!(
        &app.overlay,
        crate::state::Overlay::Options { kind: crate::state::OptionsKind::ConnectApiKey, title, .. }
            if title == "Connect Anthropic"
    ));
}

#[tokio::test]
async fn sign_in_wait_screen_shows_the_link_and_code_and_recovers_from_failure() {
    let mut app = connect_fixture();
    let client = offline_client();
    app.connect.login = Some(crate::connect::PendingLogin {
        id: "login-1".into(),
        provider: "openai".into(),
        name: "OpenAI".into(),
        method: "device".into(),
        url: "https://auth.example/codex/device".into(),
        code: "ABCD-EFGH".into(),
        state: crate::connect::LoginState::Pending,
        started: std::time::Instant::now(),
        last_poll: None,
        opened: false,
    });
    app.open_overlay(crate::state::Overlay::ProviderLogin);
    let rendered = app
        .lines((100, 30))
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Sign in to OpenAI"), "{rendered}");
    assert!(
        rendered.contains("https://auth.example/codex/device"),
        "{rendered}"
    );
    assert!(rendered.contains("ABCD-EFGH"), "{rendered}");
    assert!(rendered.contains("Waiting for you to finish"), "{rendered}");
    assert!(rendered.contains("c copy code"), "{rendered}");
    assert!(!rendered.contains("Details"), "{rendered}");

    // The server going away is reported on the screen, not swallowed.
    app.poll_login(&client).await.unwrap();
    let state = app.connect.login.as_ref().unwrap().state.clone();
    assert!(
        matches!(state, crate::connect::LoginState::Failed(_)),
        "{state:?}"
    );
    let rendered = app
        .lines((100, 30))
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Could not connect OpenAI"), "{rendered}");
    assert!(rendered.contains("enter retry"), "{rendered}");
    // The header says what Escape does, as the footer does.
    assert!(rendered.contains("esc back"), "{rendered}");
    assert!(!rendered.contains("esc close"), "{rendered}");

    // Escape goes back to the providers, and the failed sign-in is gone.
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::ConnectProvider,
            ..
        }
    ));
    assert!(app.connect.login.is_none());
}

#[tokio::test]
async fn a_sign_in_that_cannot_start_says_why_and_offers_another_try() {
    let mut app = connect_fixture();
    let client = offline_client();
    app.connect.provider = Some(crate::connect::ConnectProvider {
        id: "haarness".into(),
        name: "Haarness Cloud".into(),
        ..crate::connect::ConnectProvider::default()
    });
    // Before, the reason was dropped and the dialog closed on a bare
    // "Could not start signing in to Haarness Cloud".
    app.choose_connect_method("device", &client).await.unwrap();
    assert!(matches!(app.overlay, crate::state::Overlay::ProviderLogin));
    let login = app.connect.login.as_ref().unwrap();
    let crate::connect::LoginState::Failed(reason) = &login.state else {
        panic!("{:?}", login.state);
    };
    assert!(reason.contains("127.0.0.1:1"), "{reason}");
    assert_eq!(login.method, "device");
    let rendered = app
        .lines((100, 30))
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Could not connect Haarness Cloud"),
        "{rendered}"
    );
    assert!(rendered.contains("enter retry"), "{rendered}");
}

#[tokio::test]
async fn key_prompt_explains_a_refused_key_under_the_field_until_it_is_edited() {
    let mut app = connect_fixture();
    let client = offline_client();
    app.connect.provider = Some(crate::connect::ConnectProvider {
        id: "anthropic".into(),
        name: "Anthropic".into(),
        connected: true,
        source: Some("api".into()),
        ..crate::connect::ConnectProvider::default()
    });
    app.choose_connect_method("api", &client).await.unwrap();
    let screen = |app: &App| {
        app.lines((100, 30))
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let rendered = screen(&app);
    // A saved key is not a first connection; a local server is this computer.
    assert!(rendered.contains("A key is already saved"), "{rendered}");
    assert!(rendered.contains("on this computer"), "{rendered}");
    assert!(!rendered.contains("Haarness server"), "{rendered}");
    assert!(rendered.contains("esc back"), "{rendered}");
    assert!(!rendered.contains("esc close"), "{rendered}");

    // An empty key: the reason sits under the field, not in the status bar.
    app.close_overlay();
    app.submit_connect_key("  ", &client).await.unwrap();
    let rendered = screen(&app);
    let field = rendered
        .lines()
        .position(|line| line.contains("Key  "))
        .unwrap();
    assert!(
        rendered
            .lines()
            .nth(field + 1)
            .unwrap()
            .contains("Paste a key first."),
        "{rendered}"
    );
    assert_ne!(app.notice, "Paste a key first");
    press_key(&mut app, KeyCode::Char('s')).await;
    assert!(!screen(&app).contains("Paste a key first."));

    // A key the server could not take keeps the prompt and the key, and
    // says why under the field.
    app.close_overlay();
    app.submit_connect_key("sk-ant-test", &client)
        .await
        .unwrap();
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::ConnectApiKey,
            ..
        }
    ));
    assert_eq!(app.overlay_filter, "sk-ant-test");
    let error = app.connect.key_error.clone().unwrap();
    assert!(error.starts_with("Could not save the key: "), "{error}");
    assert!(screen(&app).contains("Could not save the key"));
    press_key(&mut app, KeyCode::Backspace).await;
    assert!(app.connect.key_error.is_none());
}

#[test]
fn connect_picker_marks_connected_rows_in_the_gutter_and_shelves_popular_first() {
    let mut app = connect_fixture();
    app.providers[1].authenticated = true;
    let row = |id: &str, name: &str, connected: bool, popular: Option<u8>| {
        crate::connect::ConnectProvider {
            id: id.into(),
            name: name.into(),
            connected,
            popular,
            ..crate::connect::ConnectProvider::default()
        }
    };
    app.connect.catalog = vec![
        row("openai", "OpenAI", false, Some(0)),
        row("anthropic", "Anthropic", true, Some(1)),
        row("groq", "Groq", false, None),
    ];
    let entry = |label: &str, value: &str| crate::state::OptionEntry {
        label: label.into(),
        description: "API key".into(),
        value: value.into(),
    };
    app.open_overlay(crate::state::Overlay::Options {
        title: "Connect a provider".into(),
        kind: crate::state::OptionsKind::ConnectProvider,
        entries: vec![
            entry("OpenAI", "openai"),
            entry("Anthropic", "anthropic"),
            entry("Groq", "groq"),
            entry("Custom endpoint…", "\u{0}custom"),
        ],
    });
    let rendered: Vec<String> = app.lines((100, 36)).iter().map(line_text).collect();
    let column = |name: &str| {
        let line = rendered.iter().find(|line| line.contains(name)).unwrap();
        line[..line.find(name).unwrap()].chars().count()
    };
    // The check lives in the gutter, so names share one column.
    assert_eq!(column("Anthropic"), column("OpenAI"));
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("✓") && line.contains("Anthropic")),
        "{rendered:#?}"
    );
    let at = |text: &str| {
        rendered
            .iter()
            .position(|line| line.contains(text))
            .unwrap()
    };
    assert!(at("Popular") < at("OpenAI"));
    assert!(at("Anthropic") < at("More providers"));
    assert!(at("More providers") < at("Groq"));
    assert!(at("Groq") < at("Something else"));
    assert!(at("Something else") < at("Custom endpoint"));
    // A shelf label is not a choice: clicking it selects nothing.
    let heading_row = at("More providers");
    assert!(
        !app.hit_rows
            .borrow()
            .iter()
            .any(|hit| hit.row == heading_row
                && matches!(hit.target, crate::view::frame::HitTarget::MenuRow(_))),
        "the shelf label is not a row"
    );
    // Scrolled so a shelf opens the visible list: its label sits right under
    // the separator, without the blank row that parts it from a shelf above.
    app.selection = 2;
    let rendered: Vec<String> = app.lines((100, 12)).iter().map(line_text).collect();
    let heading = rendered
        .iter()
        .position(|line| line.contains("More providers"))
        .unwrap();
    assert!(!rendered.iter().any(|line| line.contains("OpenAI")));
    assert!(rendered[heading - 1].contains('─'), "{rendered:#?}");
}

#[test]
fn home_names_the_missing_provider_and_the_agent_mention() {
    let mut app = connect_fixture();
    app.server_info = Some(haarness_protocol::ServerInfo {
        name: "haarness".into(),
        version: "0.1.0".into(),
        schema_version: "1".into(),
        server_instance_id: uuid::Uuid::now_v7(),
        event_cursor: 0,
        capabilities: vec![],
        workspace: Some("/Users/alex/work/haarness".into()),
        git_branch: Some("main".into()),
        default_provider: None,
        default_model: None,
        default_agent: None,
        project_trusted: Some(false),
    });
    let text = app
        .lines((100, 30))
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("no provider connected"), "{text}");
    assert!(text.contains("/connect"), "{text}");
    assert!(text.contains("@ files · agents"), "{text}");
    // Narrow terminals keep the hint inside the frame.
    let narrow = app.lines((40, 30));
    assert!(narrow.iter().all(|line| line.width() <= 40));
    // Once something is connected the hint disappears.
    app.providers[0].authenticated = true;
    let text = app
        .lines((100, 30))
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.contains("no provider connected"), "{text}");
}

#[tokio::test]
async fn at_mentions_complete_subagents_and_attach_a_delegation() {
    let thread = fixture_thread();
    let agent = |id: &str, mode: &str, hidden: bool| haarness_protocol::AgentProfile {
        id: id.into(),
        name: id.into(),
        description: format!("{id} agent"),
        hidden,
        max_subagents: 0,
        mode: Some(mode.into()),
        model: None,
    };
    let mut app = App::new(
        thread.clone(),
        vec![thread],
        vec![],
        vec![],
        vec![
            agent("build", "primary", false),
            agent("explore", "subagent", false),
            agent("expert", "all", false),
            agent("exfiltrate", "subagent", true),
        ],
    );
    let client = offline_client();
    app.composer.set_text("@ex".into());
    crate::input::refresh_mentions(&mut app, &client).await;
    assert_eq!(
        app.mention_suggestions,
        vec!["agent://explore".to_owned(), "agent://expert".to_owned()],
        "only visible subagents complete; primary and hidden agents stay out"
    );
    let text = app
        .lines((100, 30))
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("@explore"), "{text}");
    assert!(text.contains("Delegate to agent"), "{text}");
    assert!(text.contains("agents · files"), "{text}");

    assert!(
        crate::prompt_context::apply(&mut app, &client)
            .await
            .unwrap()
    );
    assert!(
        app.composer.text().contains("@explore"),
        "{}",
        app.composer.text()
    );
    assert!(
        app.notice.starts_with("Attached @explore"),
        "{}",
        app.notice
    );
    assert!(app.mention_suggestions.is_empty());
}

#[test]
fn management_lines_read_inline_instead_of_behind_a_details_row() {
    let mut app = selector_fixture();
    app.management.view = haarness_protocol::ManagementView {
        title: "OpenAI".into(),
        lines: vec!["Status  Connected · API key · 2 models".into()],
        entries: vec![haarness_protocol::ManagementEntry {
            label: "Disconnect".into(),
            description: String::new(),
            request: serde_json::json!({"action":"logout"}),
            confirm: None,
        }],
        ..Default::default()
    };
    app.show_management();
    let text = app
        .lines((100, 30))
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Status"), "{text}");
    assert!(text.contains("Connected · API key · 2 models"), "{text}");
    assert!(!text.contains("Details"), "{text}");
    assert!(!text.contains("Read everything"), "{text}");

    app.management.view.lines = (1..=9).map(|n| format!("line {n}")).collect();
    app.show_management();
    let crate::state::Overlay::Options { entries, .. } = &app.overlay else {
        panic!("management view");
    };
    let labels: Vec<_> = entries.iter().map(|entry| entry.label.clone()).collect();
    assert!(
        labels.iter().any(|label| label == "Read everything"),
        "{labels:?}"
    );
}

#[tokio::test]
async fn batch_children_are_live_rows_under_the_batch_call() {
    let thread = fixture_thread();
    let first = ThreadId::new();
    let second = ThreadId::new();
    let arguments = serde_json::json!({"tool_calls":[
        {"tool":"task","parameters":{"agent":"review","description":"Audit server attack surface","prompt":"…"}},
        {"tool":"task","parameters":{"agent":"review","description":"Audit execution and files","prompt":"…"}}
    ]});
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::ToolCall {
                call_id: "call-batch".into(),
                name: "batch".into(),
                arguments,
                provider_metadata: serde_json::Value::Null,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::TaskLink {
                call_id: "call-batch:1".into(),
                child_thread_id: first,
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::TaskState {
                call_id: "call-batch:1".into(),
                child_thread_id: first,
                child_turn_id: haarness_protocol::TurnId::new(),
                state: "done".into(),
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::TaskLink {
                call_id: "call-batch:2".into(),
                child_thread_id: second,
            },
        ),
    ];
    let mut app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    assert_eq!(
        app.children.len(),
        2,
        "both batch children are durable rows"
    );
    let client = offline_client();
    // Live progress for a batch child is accepted even though no `task`
    // tool call carries its id: the `batch` call does.
    app.reduce(
        fixture_event(
            "task.updated",
            thread.id,
            thread.id,
            serde_json::json!({
                "callId": "call-batch:2",
                "childThreadId": second,
                "state": "running",
                "toolCalls": 7,
                "lastTool": "grep"
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.children["call-batch:2"].tool_calls, 7);
    // Another tool is the active phase: an ordinary call would read as
    // queued, but a batch with running children is live work.
    app.phase = Some(crate::state::PhaseInfo {
        state: "tool".into(),
        call_id: Some("some-other-call".into()),
        ..Default::default()
    });
    let lines = app.lines((170, 40));
    let text: String = lines.iter().map(|line| line_text(line) + "\n").collect();
    // The batch wrapper is invisible; each delegation is an ordinary task
    // row, exactly as a direct `task` call would render.
    assert!(!text.contains("batch"), "{text}");
    assert!(!text.contains("queued"), "{text}");
    assert!(
        !text.contains("ctrl+o"),
        "rows do not repeat the status-line hint: {text}"
    );
    assert!(text.contains("task Audit server attack surface"), "{text}");
    assert!(text.contains("└ no tools yet"), "{text}");
    assert!(text.contains("task Audit execution and files"), "{text}");
    assert!(text.contains("↳ grep · 7 tools"), "{text}");
    let targets: Vec<String> = app
        .hit_rows
        .borrow()
        .iter()
        .filter_map(|hit| match &hit.target {
            crate::view::frame::HitTarget::TaskOpen(id) => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert!(targets.contains(&"call-batch:1".to_owned()), "{targets:?}");
    assert!(targets.contains(&"call-batch:2".to_owned()), "{targets:?}");
}

#[tokio::test]
async fn completion_toasts_only_reach_an_unfocused_terminal() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let client = offline_client();
    let turn = haarness_protocol::TurnId::new();
    let finish = |app: &mut App| {
        app.active_turn = Some(turn);
        app.turn_started = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(60))
            .or_else(|| Some(std::time::Instant::now()));
    };
    let payload = serde_json::json!({"turnId": turn, "status": "completed"});
    // Focused: the reply is already on screen, so no card and no banner.
    finish(&mut app);
    app.reduce(
        fixture_event("turn.finished", thread.id, thread.id, payload.clone()),
        &client,
    )
    .await
    .unwrap();
    app.release_completions(std::time::Instant::now() + crate::state::COMPLETION_GRACE);
    assert!(
        app.notification_toast.is_none(),
        "focused terminals get no completion toast"
    );
    // Unfocused: the card and the desktop banner both fire.
    app.terminal_focused = false;
    finish(&mut app);
    app.reduce(
        fixture_event("turn.finished", thread.id, thread.id, payload),
        &client,
    )
    .await
    .unwrap();
    app.release_completions(std::time::Instant::now() + crate::state::COMPLETION_GRACE);
    assert!(
        app.notification_toast
            .as_ref()
            .is_some_and(|toast| toast.message.starts_with("Haarness finished")),
        "{:?}",
        app.notification_toast
            .as_ref()
            .map(|toast| toast.message.clone())
    );
    assert!(
        app.pending_notification
            .as_deref()
            .is_some_and(|m| m.starts_with("Haarness finished"))
    );
}

#[tokio::test]
async fn clearing_input_reports_in_the_status_line_not_a_toast() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.composer.set_text("a draft".into());
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(app.composer.is_empty());
    assert_eq!(app.notice, "Input cleared");
    assert!(
        app.notification_toast.is_none(),
        "ambient feedback belongs to the status line"
    );
}

/// `/diff` must not hold the event loop: the viewer appears immediately in a
/// loading state, the request runs elsewhere, and a later frame collects it.
#[tokio::test]
async fn review_opens_loading_and_is_collected_by_a_later_frame() {
    let body = serde_json::json!([{
        "file": "README.md",
        "status": "added",
        "additions": 1,
        "deletions": 0,
        "patch": "@@ -0,0 +1 @@\n+# Haarness\n",
        "omittedFiles": 12
    }])
    .to_string();
    let (client, server) = workspace_http_client(vec![body], false).await;
    let mut app = selector_fixture();

    app.open_diff_viewer(crate::diffview::DiffSource::WorkingTree, &client);

    // Opening returned without waiting for the server, and the surface says so.
    assert!(viewer_of(&app).loading);
    assert!(
        app.diff_load.is_some(),
        "the request runs off the event loop"
    );
    let rows: Vec<String> = app.lines((120, 30)).iter().map(line_text).collect();
    assert!(
        rows.iter().any(|row| row.contains("Loading review…")),
        "{rows:#?}"
    );
    assert!(rows[0].contains("loading…"), "{:?}", rows[0]);

    let frames = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        for frame in 1..=2000_u32 {
            app.sync_diff_layout(120);
            if !viewer_of(&app).loading {
                return frame;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("the review never arrived");
    })
    .await
    .expect("review delivered");
    assert!(frames > 1, "the answer cannot land on the frame that asked");

    assert_eq!(viewer_of(&app).files.len(), 1);
    assert_eq!(viewer_of(&app).omitted_files, Some(12));
    let rows: Vec<String> = app.lines((120, 30)).iter().map(line_text).collect();
    assert!(
        rows[0].contains("12 not shown"),
        "a bounded review says what it left out: {:?}",
        rows[0]
    );
    assert!(!rows[0].contains("loading…"), "{:?}", rows[0]);
    server.await.unwrap();
}

/// A review that cannot be fetched has to say something the reader can act
/// on, not reqwest's bare transport text.
#[tokio::test]
async fn review_failure_reports_an_actionable_message() {
    let mut app = selector_fixture();
    app.open_diff_viewer(crate::diffview::DiffSource::WorkingTree, &offline_client());

    let notice = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            app.sync_diff_layout(120);
            if app.notice.starts_with("Diff failed") {
                return app.notice.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("failure reported");

    assert!(
        notice.contains("cannot reach the Haarness server"),
        "{notice}"
    );
    assert!(
        !notice.contains("error sending request"),
        "the old message named a transport, not a thing to do: {notice}"
    );
    assert!(
        !matches!(app.overlay, crate::state::Overlay::Diff(_)),
        "a review that never arrived leaves nothing open"
    );
}

/// Escape during a slow review closes it and stops the work behind it.
#[tokio::test]
async fn escape_cancels_a_review_that_is_still_loading() {
    let mut app = selector_fixture();
    app.open_diff_viewer(crate::diffview::DiffSource::WorkingTree, &offline_client());
    assert!(app.diff_load.is_some());

    review_key(&mut app, KeyCode::Esc).await;

    assert!(app.diff_load.is_none(), "Escape leaves no work running");
    assert!(!matches!(app.overlay, crate::state::Overlay::Diff(_)));
    app.sync_diff_layout(120);
    assert!(
        !app.notice.starts_with("Diff failed"),
        "a cancelled review is not a failed one: {}",
        app.notice
    );
}

#[path = "messages_tests.rs"]
mod messages_tests;

#[path = "completion_tests.rs"]
mod completion_tests;

fn remote_control_text(app: &App, size: (u32, u32)) -> String {
    app.lines(size)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn remote_control_shows_the_link_and_a_scannable_code_while_live() {
    let mut app = connect_fixture();
    let client = offline_client();
    let url = "https://app.haarness.ai/app/remote/sessions/0199f2d4-5a1b-7c3d-8e4f-a0b1c2d3e4f5";
    app.remote.thread = Some(app.thread.id);
    app.remote.share = Some(crate::remote_control::Share {
        state: "on".into(),
        url: Some(url.into()),
        computer: Some("Desk".into()),
        chat: Some("Release investigation".into()),
        error: None,
        qr: (0..25)
            .map(|row| {
                (0..25)
                    .map(|column| if (row + column) % 2 == 0 { '1' } else { '0' })
                    .collect()
            })
            .collect(),
    });
    // The band says the chat is reachable from elsewhere.
    assert!(remote_control_text(&app, (100, 40)).contains("remote"));

    app.open_overlay(crate::state::Overlay::RemoteControl);
    let rendered = remote_control_text(&app, (100, 44));
    for expected in [
        "Remote control",
        "Live in Haarness Cloud",
        "Release investigation",
        "on Desk",
        url,
        "Scan the code with your phone",
        "s stop sharing",
    ] {
        assert!(rendered.contains(expected), "{expected}\n{rendered}");
    }
    // 25 modules plus the quiet zone, two modules per row.
    assert_eq!(
        rendered
            .lines()
            .filter(|line| line.contains("▀▀▀▀"))
            .count(),
        15
    );

    // A short terminal keeps the link and drops the code.
    let rendered = remote_control_text(&app, (100, 22));
    assert!(rendered.contains(url), "{rendered}");
    assert!(!rendered.contains('▀'), "{rendered}");

    // Signed out, Enter offers to sign in instead of sharing.
    app.remote.share = Some(crate::remote_control::Share {
        state: "signed_out".into(),
        ..crate::remote_control::Share::default()
    });
    let rendered = remote_control_text(&app, (100, 40));
    assert!(rendered.contains("Sign in to Haarness Cloud"), "{rendered}");
    assert!(rendered.contains("enter sign in"), "{rendered}");
    assert!(!rendered.contains("remote  "), "{rendered}");

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(app.overlay, crate::state::Overlay::None));
}

#[tokio::test]
async fn remote_control_aliases_open_the_dialog_and_report_a_failed_start() {
    let mut app = connect_fixture();
    let client = offline_client();
    for alias in ["/rc", "/remote-control"] {
        app.close_overlay();
        app.execute_command(alias, &client).await.unwrap();
        assert!(
            matches!(app.overlay, crate::state::Overlay::RemoteControl),
            "{alias}"
        );
        assert_eq!(
            app.remote.busy(),
            Some(crate::remote_control::Action::Start),
            "{alias}"
        );
        assert!(remote_control_text(&app, (100, 40)).contains("Sharing this chat"));
        // The server is unreachable: the dialog says so instead of spinning.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.remote.failure.is_none() {
            assert!(std::time::Instant::now() < deadline, "no failure reported");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            app.poll_remote_control(&client);
        }
        let rendered = remote_control_text(&app, (100, 40));
        assert!(rendered.contains("Could not update sharing"), "{rendered}");
        assert!(rendered.contains("enter try again"), "{rendered}");
    }
    let Err(error) = app.execute_command("/remote-control later", &client).await else {
        panic!("an unknown option must be refused");
    };
    assert!(
        error.to_string().contains("/remote-control stop"),
        "{error}"
    );
}

#[tokio::test]
async fn sending_with_nothing_connected_opens_the_picker_and_names_no_provider() {
    let mut app = connect_fixture();
    let client = offline_client();
    // The chat sits on the server's default provider, which nobody chose.
    assert_eq!(app.thread.provider, "openai");
    let sent = app.submit_prompt("hello".into(), &client).await.unwrap();
    assert!(!sent, "the caller keeps the draft");
    assert!(matches!(
        app.overlay,
        crate::state::Overlay::Options {
            kind: crate::state::OptionsKind::ConnectProvider,
            ..
        }
    ));
    assert!(app.connect.onboarding);
    assert!(!app.notice.contains("OpenAI"), "{}", app.notice);
    assert_eq!(
        crate::state::unconnected_notice(&app.providers, "openai"),
        "Connect a provider to start · /connect"
    );
    // Once something works, a chat on another provider names it, with the
    // command that connects exactly that one.
    app.providers[1].authenticated = true;
    assert_eq!(
        crate::state::unconnected_notice(&app.providers, "openai"),
        "OpenAI is not connected · /connect openai"
    );
}

#[tokio::test]
async fn choosing_a_model_of_an_unconnected_provider_says_so() {
    let mut app = connect_fixture();
    app.providers[1].models.clear();
    app.execute_command("/models anthropic/claude-sonnet-4-5", &offline_client())
        .await
        .unwrap();
    assert_eq!(
        app.notice,
        "Anthropic is not connected · /connect anthropic"
    );
    app.execute_command("/models nowhere/model", &offline_client())
        .await
        .unwrap();
    assert_eq!(app.notice, "Unknown model nowhere/model");
}

#[test]
fn undo_redo_and_diff_failures_read_as_what_to_do() {
    let server = |status: u16, code: &str, message: &str| -> anyhow::Error {
        crate::api::ApiRefusal {
            status: reqwest::StatusCode::from_u16(status).unwrap(),
            code: Some(code.into()),
            message: message.into(),
        }
        .into()
    };
    assert_eq!(
        crate::commands::undo_error(
            &server(
                400,
                "invalid_request",
                "invalid request: thread is not reverted"
            ),
            "Redo"
        ),
        "Nothing to redo"
    );
    assert_eq!(
        crate::commands::undo_error(
            &server(404, "not_found", "not found: no snapshot to revert to"),
            "Undo"
        ),
        "Nothing to undo"
    );
    assert_eq!(
        crate::commands::undo_error(
            &server(409, "conflict", "conflict: a turn is running"),
            "Undo"
        ),
        "Undo failed: a turn is running"
    );
    assert_eq!(
        crate::commands::diff_error(&server(
            422,
            "tool_error",
            "tool error: Git diff failed: workspace is not inside a Git worktree: /tmp/x"
        )),
        "This folder isn’t a Git repository · /diff last shows the last turn’s changes"
    );
    assert!(
        !crate::commands::diff_error(&server(422, "tool_error", "tool error: boom"))
            .contains("HTTP")
    );
}

#[tokio::test]
async fn ctrl_u_clears_every_dialog_filter() {
    let mut app = selector_fixture();
    app.open_overlay(crate::state::Overlay::Models);
    app.overlay_filter = "/workspace/help".into();
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(app.overlay_filter.is_empty());
    assert!(matches!(app.overlay, crate::state::Overlay::Models));
    let rendered = app
        .lines((100, 30))
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    // One key-hint style: lowercase, as every other footer.
    assert!(rendered.contains("ctrl+f favorite"), "{rendered}");
    assert!(!rendered.contains("Ctrl+F"), "{rendered}");
}

// ---------------------------------------------------------------------------
// Terminal exits, unknown commands, undo drafts, paging and notices.
// ---------------------------------------------------------------------------

#[test]
fn every_encoding_of_ctrl_bracket_and_f12_leave_the_terminal() {
    use crate::pty_view::is_detach_key;
    // Kitty/CSI-u terminals report the key itself.
    assert!(is_detach_key(KeyEvent::new(
        KeyCode::Char(']'),
        KeyModifiers::CONTROL
    )));
    // tmux and Terminal.app send GS (0x1d), which crossterm decodes as Ctrl+5.
    assert!(is_detach_key(KeyEvent::new(
        KeyCode::Char('5'),
        KeyModifiers::CONTROL
    )));
    assert!(is_detach_key(KeyEvent::new(
        KeyCode::F(12),
        KeyModifiers::NONE
    )));
    for typed in [']', '5'] {
        assert!(!is_detach_key(KeyEvent::new(
            KeyCode::Char(typed),
            KeyModifiers::NONE
        )));
    }
    // The other legacy control symbols reach the shell instead of vanishing.
    for (code, byte) in [('\\', 0x1c), ('4', 0x1c), ('6', 0x1e), ('7', 0x1f)] {
        assert_eq!(
            crate::pty_view::key_bytes(KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)),
            Some(vec![byte]),
            "ctrl+{code}"
        );
    }
}

#[tokio::test]
async fn ctrl_five_detaches_the_terminal_view() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.terminal = Some(crate::pty_view::TerminalWorkspace::new(vec![
        haarness_protocol::PtyInfo {
            id: haarness_protocol::PtyId::new(),
            title: "zsh".into(),
            command: "zsh".into(),
            args: Vec::new(),
            cwd: "/tmp".into(),
            status: haarness_protocol::PtyStatus::Running,
            pid: None,
            cols: 80,
            rows: 24,
        },
    ]));
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('5'), KeyModifiers::CONTROL),
        &offline_client(),
    )
    .await
    .unwrap();
    assert!(app.terminal.is_none());
    assert!(app.notice.contains("Terminal detached"), "{}", app.notice);
}

#[test]
fn a_new_terminal_tab_can_take_focus() {
    let info = |title: &str| haarness_protocol::PtyInfo {
        id: haarness_protocol::PtyId::new(),
        title: title.into(),
        command: "sh".into(),
        args: Vec::new(),
        cwd: "/tmp".into(),
        status: haarness_protocol::PtyStatus::Running,
        pid: None,
        cols: 20,
        rows: 4,
    };
    let (first, second, third) = (info("first"), info("second"), info("third"));
    let mut terminal =
        crate::pty_view::TerminalWorkspace::new(vec![first.clone(), second.clone(), third.clone()]);
    assert_eq!(terminal.active_id(), Some(first.id));
    terminal.focus(second.id);
    assert_eq!(terminal.active_id(), Some(second.id));
    // A shell exiting in another tab leaves the shown one in place.
    terminal.remove(first.id);
    assert_eq!(terminal.active_id(), Some(second.id));
    terminal.remove(second.id);
    assert_eq!(terminal.active_id(), Some(third.id));
    assert_eq!(terminal.page_rows(), 2);
}

#[test]
fn a_typo_of_a_command_is_named_instead_of_sent() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    assert_eq!(crate::commands::edit_distance("stauts", "status"), 1);
    for (draft, unknown) in [
        ("/stauts", Some("stauts")),
        ("/logout now", Some("logout")),
        ("/status", None),
        ("/usr/bin is on PATH", None),
        (" /stauts", None),
        ("/ what is this", None),
        ("plain prose", None),
    ] {
        app.composer.set_text(draft.into());
        assert_eq!(
            crate::input::unknown_slash_command(&app).as_deref(),
            unknown,
            "{draft:?}"
        );
    }
    assert_eq!(
        app.unknown_command_notice("stauts"),
        "Unknown command /stauts · did you mean /status?"
    );
    assert_eq!(
        app.unknown_command_notice("frobnicate"),
        "Unknown command /frobnicate · start with a space to send it as text"
    );
}

#[tokio::test]
async fn enter_on_an_unknown_command_keeps_the_draft_and_sends_nothing() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.composer.set_text("/stauts".into());
    // Nothing listens on the offline client: a prompt submission would fail.
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &offline_client(),
    )
    .await
    .unwrap();
    assert_eq!(app.composer.text(), "/stauts");
    assert_eq!(
        app.notice,
        "Unknown command /stauts · did you mean /status?"
    );
}

fn undo_fixture() -> (App, Thread) {
    let thread = fixture_thread();
    let items = vec![
        message_item(
            1,
            &thread,
            ItemData::UserMessage {
                text: "kept prompt".into(),
                attachments: Vec::new(),
                queued: false,
                synthetic: false,
            },
        ),
        message_item(
            2,
            &thread,
            ItemData::AssistantMessage {
                text: "kept reply".into(),
            },
        ),
        message_item(
            3,
            &thread,
            ItemData::UserMessage {
                text: "write the file".into(),
                attachments: Vec::new(),
                queued: false,
                synthetic: false,
            },
        ),
        message_item(
            4,
            &thread,
            ItemData::AssistantMessage {
                text: "undone reply".into(),
            },
        ),
    ];
    let app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    (app, thread)
}

#[tokio::test]
async fn undo_restores_the_prompt_and_slash_starts_a_command_over_it() {
    let (mut app, thread) = undo_fixture();
    let mut reverted = thread.clone();
    reverted.revert = Some(haarness_protocol::RevertState {
        sequence: 2,
        snapshot_id: "backup".into(),
        base_snapshot_id: Some("base".into()),
    });
    let items = serde_json::to_string(&app.items).unwrap();
    let (client, server) = workspace_http_status_client(
        vec![
            (200, serde_json::to_string(&reverted).unwrap()),
            (200, items),
            (200, "[]".into()),
        ],
        false,
    )
    .await;
    app.execute_command("/undo", &client).await.unwrap();
    server.await.unwrap();
    assert_eq!(app.composer.text(), "write the file");

    // The undone turn folds into one marker instead of reading as live.
    let rows: Vec<String> = app.lines((100, 30)).iter().map(line_text).collect();
    let text = rows.join("\n");
    assert!(text.contains("kept reply"), "{text}");
    assert!(!text.contains("undone reply"), "{text}");
    assert!(text.contains("undone · /redo to restore"), "{text}");

    // `/` starts a command rather than appending to the restored prompt.
    let client = offline_client();
    for character in "/re".chars() {
        crate::input::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &client,
        )
        .await
        .unwrap();
    }
    assert_eq!(app.composer.text(), "/re");
    // Erasing the command brings the prompt back.
    for _ in 0..3 {
        crate::input::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &client,
        )
        .await
        .unwrap();
    }
    assert_eq!(app.composer.text(), "write the file");
    // With the cursor moved into the prompt, `/` is part of an edit.
    for key in [
        KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
    ] {
        crate::input::handle_key(&mut app, key, &client)
            .await
            .unwrap();
    }
    assert_eq!(app.composer.text(), "write the fil/e");
}

#[tokio::test]
async fn an_undone_prompt_with_a_reference_keeps_its_chip_under_a_slash_command() {
    let (mut app, _) = undo_fixture();
    let file = crate::prompt_context::snapshot("src/a.py", "x = 1\n", None).unwrap();
    let sent = format!("fix {file}");
    if let ItemData::UserMessage { text, .. } = &mut app.items[2].data {
        text.clone_from(&sent);
    }
    app.thread.revert = Some(haarness_protocol::RevertState {
        sequence: 2,
        snapshot_id: "backup".into(),
        base_snapshot_id: Some("base".into()),
    });
    // As /undo puts it back: the reference reads as its chip.
    app.composer.set_text(sent.clone());
    assert_eq!(app.composer.text(), "fix [@src/a.py]");
    assert!(app.holds_undo_draft());

    let client = offline_client();
    let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
    crate::input::handle_key(&mut app, press(KeyCode::Char('/')), &client)
        .await
        .unwrap();
    assert_eq!(app.composer.text(), "/");
    crate::input::handle_key(&mut app, press(KeyCode::Backspace), &client)
        .await
        .unwrap();
    assert_eq!(app.composer.text(), "fix [@src/a.py]");
    assert_eq!(app.composer.expanded_text(), sent);
}

#[tokio::test]
async fn redo_typed_over_an_undone_prompt_runs_and_leaves_the_composer_empty() {
    let (mut app, thread) = undo_fixture();
    app.thread.revert = Some(haarness_protocol::RevertState {
        sequence: 2,
        snapshot_id: "backup".into(),
        base_snapshot_id: Some("base".into()),
    });
    // A draft that is still the undone prompt, as the composer holds it
    // after /undo or after coming back to the chat later.
    app.composer.set_text("write the file".into());
    let items = serde_json::to_string(&app.items).unwrap();
    let (client, server) = workspace_http_status_client(
        vec![
            (200, serde_json::to_string(&thread).unwrap()),
            (200, items),
            (200, "[]".into()),
        ],
        false,
    )
    .await;
    for character in "/redo".chars() {
        crate::input::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &client,
        )
        .await
        .unwrap();
    }
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    let requests = server.await.unwrap();
    assert!(requests[0].contains("/unrevert"), "{requests:?}");
    assert!(app.composer.is_empty(), "{}", app.composer.text());
    assert!(app.thread.revert.is_none());
    assert!(app.notice.starts_with("Redone"), "{}", app.notice);
}

#[tokio::test]
async fn redo_from_elsewhere_drops_the_undone_prompt_copy_from_the_composer() {
    let (mut app, thread) = undo_fixture();
    app.thread.revert = Some(haarness_protocol::RevertState {
        sequence: 2,
        snapshot_id: "backup".into(),
        base_snapshot_id: Some("base".into()),
    });
    app.composer.set_text("write the file".into());
    // Moving the cursor is not an edit of the text: the copy still goes.
    app.composer.move_home();
    let items = serde_json::to_string(&app.items).unwrap();
    let (client, server) = workspace_http_status_client(
        vec![
            (200, serde_json::to_string(&thread).unwrap()),
            (200, items),
            (200, "[]".into()),
        ],
        false,
    )
    .await;
    app.execute_command("/redo", &client).await.unwrap();
    server.await.unwrap();
    assert!(app.composer.is_empty(), "{}", app.composer.text());
}

#[tokio::test]
async fn leaving_over_a_set_aside_undone_prompt_keeps_it_as_the_draft() {
    let (mut app, thread) = undo_fixture();
    app.undo_draft = Some(crate::state::UndoDraft {
        text: "write the file".into(),
        attachments: Vec::new(),
    });
    let other = fixture_thread();
    let (client, server) = navigation_client().await;
    app.switch_thread(other.clone(), &client).await.unwrap();
    server.await.unwrap();
    assert_eq!(app.thread.id, other.id);
    assert!(app.composer.is_empty(), "{}", app.composer.text());
    assert!(app.undo_draft.is_none());
    assert_eq!(
        app.drafts.get(&thread.id).map(|draft| draft.text.as_str()),
        Some("write the file")
    );
}

#[test]
fn page_keys_move_by_the_visible_transcript_less_an_overlap() {
    let thread = fixture_thread();
    let items = (1..=200)
        .map(|sequence| {
            message_item(
                sequence,
                &thread,
                ItemData::AssistantMessage {
                    text: format!("line-{sequence}"),
                },
            )
        })
        .collect();
    let mut app = App::new(thread.clone(), vec![thread], items, vec![], vec![]);
    assert_eq!(app.page_rows(), 10, "a default before the first frame");
    let _ = app.lines((120, 36));
    let rows = app.transcript_rows.get();
    assert!(rows > 20, "{rows}");
    app.scroll(app.page_rows());
    assert_eq!(app.viewport_offset, rows - 2);
    let _ = app.lines((120, 60));
    assert!(app.page_rows() > isize::try_from(rows).unwrap());
}

#[test]
fn notices_fade_unless_they_describe_a_standing_state() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let start = std::time::Instant::now();
    app.notice = "Copied selection".into();
    app.expire_notice(start);
    assert!(app.notice_is_fresh(start));
    app.expire_notice(start + std::time::Duration::from_secs(5));
    assert_eq!(app.notice, "Copied selection");
    assert!(!app.notice_is_fresh(start + std::time::Duration::from_secs(5)));
    app.expire_notice(start + crate::state::NOTICE_TTL);
    assert!(app.notice.is_empty());

    app.notice = "shell — Enter runs the command, Esc leaves".into();
    app.shell_mode = true;
    app.expire_notice(start);
    app.expire_notice(start + std::time::Duration::from_secs(60));
    assert!(!app.notice.is_empty(), "shell mode keeps its hint");
}

#[tokio::test]
async fn a_configuration_reload_does_not_replace_the_answer_to_a_setting_change() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let reload = || {
        fixture_event(
            "configuration.reloaded",
            thread.id,
            thread.id,
            serde_json::json!({}),
        )
    };
    app.notice = "Input mode: queue · saved for this project".into();
    app.reduce(reload(), &offline_client()).await.unwrap();
    assert_eq!(app.notice, "Input mode: queue · saved for this project");

    // A reload nobody here asked for still reports itself.
    app.notice.clear();
    app.reduce(reload(), &offline_client()).await.unwrap();
    assert_eq!(app.notice, "Configuration reloaded");
}

#[test]
fn quitting_names_the_chat_and_the_command_that_reopens_it() {
    let thread = ThreadId::new();
    let summary = crate::exit_summary(
        crate::locale::Language::English,
        "Fix the flaky login test",
        &format!("haarness -s {thread}"),
        false,
        false,
    );
    let rows: Vec<&str> = summary.lines().map(str::trim_end).collect();
    assert!(
        rows.contains(&"  Chat      Fix the flaky login test"),
        "{summary}"
    );
    assert!(
        rows.contains(&format!("  Continue  haarness -s {thread}").as_str()),
        "{summary}"
    );
    assert!(!summary.contains('\x1b'), "plain when piped");

    // A turn still running is said, and an untitled chat has no empty row.
    let summary = crate::exit_summary(
        crate::locale::Language::English,
        "  ",
        "haarness -s x",
        true,
        true,
    );
    assert!(!summary.contains("Chat"), "{summary}");
    assert!(
        summary.contains("the agent keeps going on the server"),
        "{summary}"
    );
    assert!(summary.contains("\x1b[1mhaarness -s x\x1b[0m"), "{summary}");

    assert_eq!(crate::shell_quote("/work/my-app"), "/work/my-app");
    assert_eq!(
        crate::shell_quote("/work/My Projects/it's"),
        r"'/work/My Projects/it'\''s'"
    );
}

#[test]
fn a_lost_server_says_what_this_client_does_about_it() {
    use crate::state::ServerRestart;
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    let status = |app: &App| {
        app.lines((120, 30))
            .last()
            .map(line_text)
            .unwrap_or_default()
    };
    app.connection_lost = Some(1);
    assert!(
        status(&app).contains("reconnecting · attempt 1"),
        "{}",
        status(&app)
    );
    // An attached server this client cannot start keeps being retried, and
    // the band stops counting to say so.
    app.connection_lost = Some(4);
    assert!(
        status(&app).contains("server unreachable · still retrying"),
        "{}",
        status(&app)
    );
    app.server_restart = Some(ServerRestart::Starting);
    assert!(
        status(&app).contains("server stopped · restarting it"),
        "{}",
        status(&app)
    );
    app.server_restart = Some(ServerRestart::Failed);
    assert!(
        status(&app).contains("server stopped · restart failed · retrying"),
        "{}",
        status(&app)
    );
}

#[test]
fn arabic_placeholder_keeps_the_cursor_out_of_its_text() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.language = crate::locale::Language::Arabic;
    let placeholder = app.language.text("Ask anything…").to_owned();
    let lines = app.lines((100, 30));
    let row = lines
        .iter()
        .find(|line| line.text().contains(&placeholder))
        .unwrap_or_else(|| panic!("{:#?}", lines.iter().map(Line::text).collect::<Vec<_>>()));
    // A bidi-aware terminal reverses the Arabic placeholder in place; the
    // cursor is a blank cell of its own just before it, so it stays at the
    // edge instead of landing on a letter in the middle of the word.
    let at = row
        .spans
        .iter()
        .position(|span| span.text == placeholder)
        .expect("placeholder drawn as one run");
    assert_eq!(row.spans[at - 1].text, " ");
    // No zero-width bidi marks reach the native renderer, which would give
    // each one a cell (see locale::tests).
    for line in &lines {
        assert!(line.width() <= 100, "{}", line.text());
        assert!(
            !line
                .text()
                .contains(['\u{200e}', '\u{2066}', '\u{2068}', '\u{2069}']),
            "{}",
            line.text()
        );
    }
    // Left-to-right languages keep the cursor on the placeholder's first letter.
    app.language = crate::locale::Language::French;
    let lines = app.lines((100, 30));
    let first = app
        .language
        .text("Ask anything…")
        .chars()
        .next()
        .unwrap()
        .to_string();
    assert!(
        lines
            .iter()
            .any(|line| line.spans.iter().any(|span| span.text == first)),
        "{:#?}",
        lines.iter().map(Line::text).collect::<Vec<_>>()
    );
}

#[test]
fn dialog_chrome_and_command_descriptions_read_from_the_catalog() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread], vec![], vec![], vec![]);
    app.language = crate::locale::Language::French;
    // Built-in descriptions are looked up by their English text at render
    // time, so every one of them must be a catalog key.
    for entry in app.filtered_slash_entries("") {
        if let Some(spec) = COMMANDS.iter().find(|spec| spec.name == entry.name) {
            assert_eq!(entry.description, app.language.text(spec.description));
        }
    }
    app.open_overlay(crate::state::Overlay::Commands);
    let text = app
        .lines((100, 30))
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join("\n");
    for key in ["Type to filter", "esc close", "↑↓ navigate", "enter select"] {
        assert!(text.contains(app.language.text(key)), "{key}: {text}");
    }
    // The notice after /language names the setting with its capital, in
    // every language, rather than the lowercase status key.
    assert_eq!(
        crate::commands::language_notice(crate::locale::Language::English),
        "Language: en"
    );
}

#[test]
fn a_local_model_nobody_chose_is_named_on_the_home_screen() {
    let mut app = connect_fixture();
    app.thread.provider = "ollama".into();
    app.thread.model = "qwen3:0.6b".into();
    let mut ollama = app.providers[0].clone();
    ollama.id = "ollama".into();
    ollama.name = "Ollama".into();
    ollama.authenticated = true;
    app.providers.push(ollama);
    let home = |app: &App| {
        crate::view::home::masthead(app, 100, false)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let text = home(&app);
    assert!(
        text.contains("Replies come from qwen3:0.6b, which Ollama runs on this computer.  ·  /connect"),
        "{text}"
    );
    // Too narrow for the whole sentence: no dangling separator.
    let narrow = crate::view::home::masthead(&app, 40, false)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!narrow.contains("/connect"), "{narrow}");
    // Once a provider of the user's own is connected, the hint goes away.
    app.providers[1].authenticated = true;
    assert!(!home(&app).contains("Replies come from"));
}
