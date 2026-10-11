use super::*;

fn chrome_permission(thread: &Thread, id: &str) -> haarness_protocol::PendingPermissionInfo {
    let mut pending = pending_permission_fixture(thread, id);
    let site = |origin: &str| format!("browser_site:chrome-browsing:{}:{origin}", thread.id);
    pending.explicit = true;
    pending.permission = "multiple".into();
    pending.operation = site("https://mail.google.com");
    pending.detail = serde_json::json!({"url": "https://mail.google.com/mail/u/0/"});
    pending.patterns = vec![
        "browser:navigate:https://mail.google.com/mail/u/0/".into(),
        site("https://mail.google.com"),
        site("https://www.mail.google.com"),
        format!(
            "browser_site:chrome-scripts:{}:https://mail.google.com",
            thread.id
        ),
    ];
    pending.always_patterns = pending.patterns.clone();
    pending
}

/// A Chrome site request reads as what it lets the agent do, offers this chat
/// or this once, and is answered for the chat with a single press.
#[tokio::test]
async fn chrome_approval_offers_this_chat_once_or_deny_and_allows_with_one_press() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.pending_permissions = vec![chrome_permission(&thread, "chrome")];
    app.permission_focus = true;
    for width in [60, 120] {
        let frame = frame_text(&app, (width, 40));
        for expected in [
            "Allow in Chrome",
            "Use https://mail.google.com in Chrome",
            "Run scripts on https://mail.google.com in Chrome",
            "Allow in this chat  a",
            "Allow once  y",
            "Deny  n",
        ] {
            assert!(frame.contains(expected), "{width}: {expected}\n{frame}");
        }
        for absent in [
            "www.mail.google.com",
            "This folder",
            "Repository",
            "Once + Auto",
            "always allows",
            "browser_site:",
        ] {
            assert!(!frame.contains(absent), "{width}: {absent}\n{frame}");
        }
    }
    assert_eq!(
        crate::view::approval::summary(
            &app.pending_permissions[0],
            crate::locale::Language::English
        ),
        "Allow in Chrome: Use https://mail.google.com in Chrome"
    );

    // Folder, repository and Auto answers do not exist here.
    let client = offline_client();
    for letter in ['w', 'r', 'u'] {
        crate::input::answer_permission(&mut app, &client, letter)
            .await
            .unwrap();
        assert_eq!(app.confirm_always, None, "{letter}");
        assert_eq!(app.pending_permissions.len(), 1, "{letter}");
    }

    let (client, server) = workspace_http_status_client(vec![(204, String::new())], false).await;
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    let requests = server.await.unwrap();
    assert!(
        requests[0].contains("/permissions/chrome"),
        "{}",
        requests[0]
    );
    assert!(
        requests[0].contains("\"decision\":\"always\""),
        "{}",
        requests[0]
    );
    assert!(app.pending_permissions.is_empty());
    assert_eq!(app.confirm_always, None);
}

/// Enter on a fresh Chrome card picks the highlighted Allow in this chat.
#[tokio::test]
async fn enter_on_a_chrome_card_allows_it_in_this_chat() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.pending_permissions = vec![chrome_permission(&thread, "enter")];
    app.permission_focus = true;
    app.permission_selection = 0;
    let (client, server) = workspace_http_status_client(vec![(204, String::new())], false).await;
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    let requests = server.await.unwrap();
    assert!(
        requests[0].contains("\"decision\":\"always\""),
        "{}",
        requests[0]
    );
}

/// `--yolo` and the client-side Auto answer nothing a person must grant:
/// Chrome site requests keep their card.
#[tokio::test]
async fn yolo_and_auto_leave_chrome_site_requests_to_a_person() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    app.yolo = true;
    app.set_auto_permissions(true);
    // No request may reach the server: the offline client would fail it,
    // and a failed auto answer raises a warning toast.
    let client = offline_client();
    app.reduce(
        fixture_event(
            "permission.requested",
            thread.id,
            thread.id,
            serde_json::json!({
                "permissionId": "site", "permission": "browser_site", "explicit": true,
                "allowAlways": true,
                "patterns": [format!("chrome-browsing:{}:https://mail.google.com", thread.id)],
            }),
        ),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(app.pending_permissions.len(), 1);
    assert!(app.pending_permissions[0].explicit);
    assert!(!app.auto_permissions_seen.values().any(|approved| *approved));
    assert!(frame_text(&app, (120, 40)).contains("Use https://mail.google.com in Chrome"));

    app.hydrate_permissions(vec![chrome_permission(&thread, "hydrated")], &client)
        .await;
    assert_eq!(app.pending_permissions.len(), 1);
    assert_eq!(app.pending_permissions[0].permission_id, "hydrated");
}
