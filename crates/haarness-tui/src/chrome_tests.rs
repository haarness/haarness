use super::*;
use crate::chrome::{
    Access, ExtensionCopy, Host, Hosts, LocalAction, Settings, Setup, SetupBrowser, Unavailable,
};
use crate::state::Overlay;
use std::sync::{Arc, Mutex};

const EXTENSION: &str = "/Users/someone/.local/share/haarness/browser/chrome/Haarness for Chrome";

fn chrome_text(app: &App, size: (u32, u32)) -> String {
    app.lines(size)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The dialog's words with wrapping undone, for sentences that wrap
/// differently at each width.
fn flat(rendered: &str) -> String {
    rendered
        .lines()
        .map(str::trim)
        .filter(|row| !row.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn registered_setup() -> Setup {
    Setup {
        browsers: vec![
            SetupBrowser {
                name: "Google Chrome".into(),
                extensions_page: Some("chrome://extensions".into()),
                registered: true,
                ..SetupBrowser::default()
            },
            SetupBrowser {
                name: "Microsoft Edge".into(),
                extensions_page: Some("edge://extensions".into()),
                registered: true,
                ..SetupBrowser::default()
            },
            SetupBrowser {
                name: "Chromium".into(),
                supported: false,
                note: Some("Snap profiles cannot start Haarness".into()),
                ..SetupBrowser::default()
            },
        ],
        extension: Some(ExtensionCopy {
            path: EXTENSION.into(),
            version: Some("0.1.1-preview.1".into()),
        }),
        ..Setup::default()
    }
}

fn host(name: &str, label: Option<&str>, tabs: u64) -> Host {
    Host {
        name: name.into(),
        label: label.map(str::to_owned),
        extension_version: Some("0.1.1".into()),
        thread_tabs: tabs,
        ..Host::default()
    }
}

/// Deliver whatever the dialog's workers finish, for up to five seconds.
async fn settle_chrome(app: &mut App, client: &crate::api::ApiClient) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        app.poll_chrome(client);
        if app.chrome.idle() || std::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// A server that answers by route: the Chrome hosts, and `browser-settings`
/// reads and writes kept in memory, which the hosts' settings block follows.
/// Records every request it saw.
async fn chrome_server(
    hosts: serde_json::Value,
    settings: serde_json::Value,
) -> (crate::api::ApiClient, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let settings = Arc::new(Mutex::new(settings));
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            let mut expected = None;
            loop {
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
                if expected.is_some_and(|length| request.len() >= length) {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&request).into_owned();
            let body = text
                .split_once("\r\n\r\n")
                .map_or("", |(_, body)| body)
                .to_owned();
            let response = if text.starts_with("GET /v1/browser/hosts") {
                // As a real server does, the hosts report the settings in force.
                let mut hosts = hosts.clone();
                let effective = settings.lock().unwrap()["chrome"]["settings"].clone();
                if effective.is_object() {
                    hosts["chrome"]["enabled"] = effective["enabled"].clone();
                    hosts["chrome"]["defaultBrowser"] = effective["defaultBrowser"].clone();
                }
                hosts.to_string()
            } else if text.starts_with("POST /v1/manage/browser-settings") {
                let input: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let mut data = settings.lock().unwrap();
                if input["action"] == "set" {
                    data["revision"] = serde_json::json!("r2");
                    data["chrome"]["globalSettings"] = input["chrome"].clone();
                    data["chrome"]["settings"] = input["chrome"].clone();
                }
                serde_json::json!({"title": "Browser settings", "data": data.clone()}).to_string()
            } else {
                "{}".into()
            };
            seen.lock().unwrap().push(text);
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                response.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
        }
    });
    let client = crate::api::ApiClient::new(&format!("http://{address}"), "test-token").unwrap();
    (client, requests)
}

#[tokio::test]
async fn chrome_and_its_browser_alias_open_the_dialog_and_say_when_the_server_is_unreachable() {
    let mut app = connect_fixture();
    let client = offline_client();
    for alias in ["/chrome", "/browser"] {
        app.close_overlay();
        app.chrome = crate::chrome::Chrome::default();
        app.execute_command(alias, &client).await.unwrap();
        assert!(matches!(app.overlay, Overlay::Chrome), "{alias}");
        assert!(
            chrome_text(&app, (100, 40)).contains("Checking…"),
            "{alias}"
        );
        settle_chrome(&mut app, &client).await;
        let rendered = chrome_text(&app, (100, 40));
        assert!(
            rendered.contains("Could not reach the server"),
            "{alias}\n{rendered}"
        );
        // An attached client is not on the computer that runs Chrome, so
        // nothing was spawned here.
        assert!(!app.chrome.here);
        assert!(app.chrome.local.is_none());
    }
    // Unknown words are refused before anything opens.
    app.close_overlay();
    assert!(app.execute_command("/chrome later", &client).await.is_err());
    assert!(matches!(app.overlay, Overlay::None));
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(matches!(app.overlay, Overlay::None));
}

/// Setup to connected, at the widths people use: each state names itself in
/// a coloured status line and says what to do next, and the steps carry the
/// folder to paste in full.
#[test]
fn chrome_dialog_walks_from_setup_to_connected_at_80_and_120_columns() {
    let mut app = connect_fixture();
    app.open_overlay(Overlay::Chrome);
    app.chrome.here = true;
    app.chrome.hosts = Some(Ok(Hosts::default()));
    for width in [80, 120] {
        let size = (width, 44);
        app.chrome.local = None;
        let rendered = chrome_text(&app, size);
        assert!(rendered.contains("Checking this computer…"), "{rendered}");

        app.chrome.local = Some(Setup::default());
        let rendered = chrome_text(&app, size);
        for expected in [
            "Chrome",
            "Not set up",
            "Agents can use your own Chrome",
            "Press Enter to let Chrome start Haarness",
            "Let agents use Chrome",
            "Browser for new agent tabs",
            "Automatic",
            "enter set up",
        ] {
            assert!(
                rendered.contains(expected),
                "{width}: {expected}\n{rendered}"
            );
        }

        app.chrome.local = Some(registered_setup());
        let rendered = chrome_text(&app, size);
        for expected in [
            "Waiting for Chrome…",
            "Chrome can start Haarness. Now add Haarness for Chrome:",
            "1  Open chrome://extensions in Google Chrome",
            "or edge://extensions in Microsoft Edge",
            "2  Turn on Developer mode (keep it on)",
            "3  Click Load unpacked, press",
            "4  Pin Haarness to the toolbar",
            "Chromium · Snap profiles cannot start Haarness",
            "c copy path",
            "enter check again",
        ] {
            assert!(
                rendered.contains(expected),
                "{width}: {expected}\n{rendered}"
            );
        }
        // The folder survives wrapping: rejoined, it is the whole path.
        let squeeze = |text: &str| text.split_whitespace().collect::<String>();
        assert!(
            squeeze(&rendered).contains(&squeeze(EXTENSION)),
            "{width}\n{rendered}"
        );
        assert!(flat(&rendered).contains(EXTENSION), "{width}\n{rendered}");
        for row in rendered.lines() {
            assert!(row.width() <= width as usize, "{width}: {row}");
        }

        // A Web Store listing replaces the folder steps.
        app.chrome.local.as_mut().unwrap().web_store_url =
            Some("https://chromewebstore.google.com/detail/haarness".into());
        let rendered = chrome_text(&app, size);
        assert!(rendered.contains("Click Add to Chrome"), "{rendered}");
        assert!(rendered.contains("o open web store"), "{rendered}");
        assert!(!rendered.contains("Load unpacked"), "{rendered}");
        app.chrome.local.as_mut().unwrap().web_store_url = None;

        app.chrome.hosts = Some(Ok(Hosts {
            hosts: vec![host("Google Chrome", Some("Work"), 2)],
            chrome: Access::default(),
        }));
        app.chrome.thread = Some(app.thread.id);
        let rendered = chrome_text(&app, size);
        for expected in [
            "● Connected · Google Chrome (Work)",
            "Haarness for Chrome 0.1.1",
            "This chat has 2 tabs in Chrome.",
            "enter refresh",
        ] {
            assert!(
                rendered.contains(expected),
                "{width}: {expected}\n{rendered}"
            );
        }
        assert!(!rendered.contains("Load unpacked"), "{rendered}");

        app.chrome.hosts = Some(Ok(Hosts {
            hosts: vec![Host {
                paused: true,
                ..host("Google Chrome", None, 0)
            }],
            chrome: Access::default(),
        }));
        let rendered = chrome_text(&app, size);
        assert!(rendered.contains("◐ Paused in Chrome"), "{rendered}");
        assert!(
            rendered.contains("Resume it from the Haarness button"),
            "{rendered}"
        );

        app.chrome.hosts = Some(Ok(Hosts {
            hosts: vec![
                host("Google Chrome", None, 1),
                Host {
                    stopped: true,
                    ..host("Brave", None, 0)
                },
            ],
            chrome: Access::default(),
        }));
        let rendered = chrome_text(&app, size);
        assert!(rendered.contains("2 browsers connected"), "{rendered}");
        assert!(
            rendered.contains("Brave · Haarness for Chrome 0.1.1 · stopped"),
            "{rendered}"
        );
        assert!(
            rendered.contains("This chat has 1 tab in Chrome."),
            "{rendered}"
        );

        if let Some(Ok(hosts)) = &mut app.chrome.hosts {
            hosts.chrome = Access {
                enabled: false,
                default_browser: "chrome".into(),
                ever_connected: true,
            };
        }
        app.chrome.settings = Some(Settings {
            enabled: false,
            default_browser: "chrome".into(),
            restrictions: vec!["Chrome is turned off for this project".into()],
        });
        let rendered = chrome_text(&app, size);
        for expected in [
            "○ Agents may not use Chrome",
            "Off",
            "enter turn on",
            "Restricted by this project's settings:",
            "· Chrome is turned off for this project",
        ] {
            assert!(
                rendered.contains(expected),
                "{width}: {expected}\n{rendered}"
            );
        }
        app.chrome.settings = None;
        app.chrome.hosts = Some(Ok(Hosts::default()));
    }
}

/// Over SSH or attached from another computer the dialog says where Chrome
/// has to be, and a server that cannot offer Chrome says so plainly.
#[test]
fn chrome_dialog_is_honest_about_remote_terminals_and_older_servers() {
    let mut app = connect_fixture();
    app.open_overlay(Overlay::Chrome);
    app.chrome.hosts = Some(Ok(Hosts::default()));
    app.chrome.here = false;
    for width in [80, 120] {
        let rendered = flat(&chrome_text(&app, (width, 40)));
        for expected in [
            "○ Not connected",
            "Chrome works with projects on the computer where Chrome runs.",
            "haarness browser setup",
            "c copy command",
        ] {
            assert!(
                rendered.contains(expected),
                "{width}: {expected}\n{rendered}"
            );
        }
    }
    app.chrome.hosts = Some(Err(Unavailable::Remote));
    let rendered = chrome_text(&app, (100, 40));
    assert!(
        rendered.contains("Not available in this chat"),
        "{rendered}"
    );
    // Nothing to switch where Chrome cannot be reached.
    assert!(!rendered.contains("Let agents use Chrome"), "{rendered}");
    app.chrome.hosts = Some(Err(Unavailable::Unsupported));
    let rendered = chrome_text(&app, (100, 40));
    assert!(
        rendered.contains("This server cannot use Chrome"),
        "{rendered}"
    );
    assert!(rendered.contains("Update Haarness"), "{rendered}");
}

/// `e` and `d` write the whole global Chrome setting, starting from what the
/// global layer holds, so a switch never resets the site table or defaults.
#[tokio::test]
async fn chrome_switches_write_the_complete_global_setting() {
    let global = serde_json::json!({
        "enabled": true,
        "defaults": {"browsing": "ask", "downloads": "deny", "uploads": "ask", "scripts": "ask"},
        "defaultBrowser": "auto",
        "sites": [{"origin": "https://bank.test", "browsing": "deny", "downloads": "deny", "uploads": "deny", "scripts": "deny"}],
    });
    let (client, requests) = chrome_server(
        serde_json::json!({"hosts": [], "chrome": {"enabled": true, "defaultBrowser": "auto", "everConnected": false}}),
        serde_json::json!({"revision": "r1", "chrome": {
            "settings": global, "globalSettings": global,
            "projectRestrictions": ["Downloads in Chrome are blocked in this project"]}}),
    )
    .await;
    let mut app = connect_fixture();
    app.open_chrome(&client);
    settle_chrome(&mut app, &client).await;
    let rendered = chrome_text(&app, (100, 44));
    assert!(
        rendered.contains("· Downloads in Chrome are blocked in this project"),
        "{rendered}"
    );

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(chrome_text(&app, (100, 44)).contains("Saving…"));
    settle_chrome(&mut app, &client).await;
    let writes = |requests: &Arc<Mutex<Vec<String>>>| -> Vec<serde_json::Value> {
        requests
            .lock()
            .unwrap()
            .iter()
            .filter_map(|request| request.split_once("\r\n\r\n"))
            .filter_map(|(_, body)| serde_json::from_str::<serde_json::Value>(body).ok())
            .filter(|body| body["action"] == "set")
            .collect()
    };
    let saved = writes(&requests);
    assert_eq!(saved.len(), 1, "{saved:#?}");
    assert_eq!(saved[0]["scope"], "global");
    assert_eq!(saved[0]["revision"], "r1");
    assert_eq!(saved[0]["chrome"]["enabled"], false);
    assert_eq!(saved[0]["chrome"]["defaults"]["downloads"], "deny");
    assert_eq!(
        saved[0]["chrome"]["sites"][0]["origin"],
        "https://bank.test"
    );
    assert_eq!(saved[0]["chrome"]["defaultBrowser"], "auto");
    assert!(chrome_text(&app, (100, 44)).contains("Agents may not use Chrome"));

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    settle_chrome(&mut app, &client).await;
    let saved = writes(&requests);
    assert_eq!(saved.len(), 2, "{saved:#?}");
    assert_eq!(saved[1]["revision"], "r2");
    assert_eq!(saved[1]["chrome"]["defaultBrowser"], "haarness");
    assert_eq!(saved[1]["chrome"]["enabled"], false);
    assert!(chrome_text(&app, (100, 44)).contains("Haarness browser"));
}

/// The local half: `browser status --json` when the dialog opens, then Enter
/// runs `browser setup --json` and the dialog moves on to the extension.
#[cfg(unix)]
#[tokio::test]
async fn chrome_setup_runs_the_local_command_and_shows_the_extension_steps() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = std::env::temp_dir().join(format!("haarness-chrome-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&directory).unwrap();
    let log = directory.join("calls");
    let script = directory.join("haarness");
    let registered = serde_json::json!({
        "removed": false, "warnings": [],
        "browsers": [{"browser":"chrome","name":"Google Chrome","extensionsPage":"chrome://extensions",
            "detected":true,"registered":true,"supported":true,"note":null}],
        "extension": {"path": EXTENSION, "version": "0.1.1"},
    });
    let missing = serde_json::json!({
        "removed": false, "warnings": [],
        "browsers": [{"browser":"chrome","name":"Google Chrome","extensionsPage":"chrome://extensions",
            "detected":true,"registered":false,"supported":true,"note":null}],
        "extension": null,
    });
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nif [ \"$2\" = setup ]; then echo '{registered}'; else echo '{missing}'; fi\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let client = offline_client();
    let mut app = connect_fixture();
    app.open_overlay(Overlay::Chrome);
    app.chrome.program = Some(script);
    app.chrome.here = true;
    app.chrome.hosts = Some(Ok(Hosts::default()));
    app.request_chrome_local(LocalAction::Status);
    settle_chrome(&mut app, &client).await;
    assert!(chrome_text(&app, (100, 40)).contains("Not set up"));

    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(chrome_text(&app, (100, 40)).contains("Setting up Chrome…"));
    settle_chrome(&mut app, &client).await;
    let rendered = chrome_text(&app, (100, 40));
    assert!(rendered.contains("Load unpacked"), "{rendered}");
    assert!(rendered.contains("Haarness for Chrome"), "{rendered}");
    // The server could not be reached for the refresh that followed; the
    // dialog keeps what it knew rather than flipping to an error.
    assert!(
        !rendered.contains("Could not reach the server"),
        "{rendered}"
    );
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(calls, "browser status --json\nbrowser setup --json\n");

    // A failing command explains itself and Enter tries it again.
    std::fs::write(
        directory.join("haarness"),
        "#!/bin/sh\necho 'error: Move Haarness to Applications, then run setup again' >&2\nexit 1\n",
    )
    .unwrap();
    app.request_chrome_local(LocalAction::Setup);
    settle_chrome(&mut app, &client).await;
    let rendered = chrome_text(&app, (100, 40));
    assert!(rendered.contains("Could not set up Chrome"), "{rendered}");
    assert!(
        rendered.contains("Move Haarness to Applications, then run setup again"),
        "{rendered}"
    );
    assert!(rendered.contains("enter try again"), "{rendered}");
    let _ = std::fs::remove_dir_all(&directory);
}

/// A CLI-only install has no Haarness for Chrome to add: the dialog says so
/// instead of spinning on "Waiting for Chrome…", and Enter looks again for a
/// copy Desktop may have made since.
#[tokio::test]
async fn without_an_extension_to_add_the_dialog_does_not_wait_for_chrome() {
    let client = offline_client();
    let mut app = connect_fixture();
    app.open_overlay(Overlay::Chrome);
    app.chrome.here = true;
    app.chrome.hosts = Some(Ok(Hosts::default()));
    app.chrome.local = Some(Setup {
        extension: None,
        ..registered_setup()
    });
    for width in [80, 120] {
        let rendered = chrome_text(&app, (width, 44));
        assert!(rendered.contains("○ Not set up"), "{width}\n{rendered}");
        assert!(
            flat(&rendered)
                .contains("This build of Haarness has no Haarness for Chrome extension to load."),
            "{width}\n{rendered}"
        );
        assert!(!rendered.contains("Waiting for Chrome"), "{rendered}");
        assert!(!rendered.contains("c copy path"), "{rendered}");
    }
    app.chrome.program = Some("/nonexistent/haarness".into());
    app.chrome_primary(&client);
    assert_eq!(app.chrome.busy_local(), Some(LocalAction::Status));
}

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

/// A browser row says when it ran in the person's own Chrome.
#[test]
fn browser_rows_say_when_they_ran_in_chrome() {
    let thread = fixture_thread();
    let call = |sequence, id: &str, arguments| {
        message_item(
            sequence,
            &thread,
            ItemData::ToolCall {
                call_id: id.into(),
                name: "browser_navigate".into(),
                arguments,
                provider_metadata: serde_json::Value::Null,
            },
        )
    };
    let result = |sequence, id: &str, browser: &str| {
        message_item(
            sequence,
            &thread,
            ItemData::ToolResult {
                call_id: id.into(),
                name: "browser_navigate".into(),
                output: "Navigated".into(),
                is_error: false,
                metadata: serde_json::json!({"browser": browser}),
            },
        )
    };
    let items = vec![
        call(
            1,
            "c1",
            serde_json::json!({"url": "https://mail.google.com"}),
        ),
        result(2, "c1", "chrome"),
        call(3, "c2", serde_json::json!({"url": "https://example.com"})),
        result(4, "c2", "haarness"),
    ];
    let app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    let frame = frame_text(&app, (120, 30));
    let chrome = frame
        .lines()
        .find(|row| row.contains("https://mail.google.com"))
        .unwrap();
    assert!(chrome.contains("·  Chrome"), "{frame}");
    let haarness = frame
        .lines()
        .find(|row| row.contains("https://example.com"))
        .unwrap();
    assert!(!haarness.contains("Chrome"), "{frame}");
}

/// A long address gives way to the Chrome tag on a narrow screen, however
/// long the browser tool's name is.
#[test]
fn a_long_address_leaves_room_for_the_chrome_tag() {
    let thread = fixture_thread();
    let items = vec![message_item(
        1,
        &thread,
        ItemData::ToolCall {
            call_id: "c1".into(),
            name: "browser_tabs_open".into(),
            arguments: serde_json::json!({
                "browser": "chrome",
                "url": "https://www.example.com/a/very/long/path/that/keeps/going?with=query&and=more",
            }),
            provider_metadata: serde_json::Value::Null,
        },
    )];
    let app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    let frame = frame_text(&app, (80, 30));
    let row = frame
        .lines()
        .find(|row| row.contains("browser_tabs_open https://www.example.com"))
        .unwrap();
    assert!(row.trim_end().ends_with("·  Chrome"), "{frame}");
    assert!(row.trim_end().chars().count() <= 80, "{frame}");
}

/// `/status` says whether this chat can use Chrome.
#[tokio::test]
async fn status_has_a_chrome_line() {
    let (client, _) = chrome_server(
        serde_json::json!({"hosts": [{"name": "Google Chrome", "threadTabs": 1}],
            "chrome": {"enabled": true, "defaultBrowser": "auto", "everConnected": true}}),
        serde_json::json!({}),
    )
    .await;
    let mut app = connect_fixture();
    app.execute_command("/status", &client).await.unwrap();
    let Overlay::Panel { ref lines, .. } = app.overlay else {
        panic!("status is a panel");
    };
    assert!(
        lines
            .iter()
            .any(|line| line == "Chrome   Connected · Google Chrome · 1 tab in this chat"),
        "{lines:#?}"
    );
}
