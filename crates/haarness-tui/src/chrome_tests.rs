use super::*;
use crate::chrome::{
    Access, ExtensionCopy, ExtensionSource, Host, Hosts, LocalAction, Restriction, Settings, Setup,
    SetupBrowser, Unavailable,
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

/// The instance id `chrome_server` answers `/v1/info` with.
const SERVER_ID: &str = "0199d0c4-7f3a-7c1e-9a52-3f1d2b4c5e6f";

/// A server that answers by route: the Chrome hosts, `browser-settings`
/// reads and writes kept in memory, which the hosts' settings block follows,
/// and `/v1/info`. A `blockedBy` beside the settings keeps Chrome off under
/// the switch, as agent browser control or a project does; `legacy` answers
/// as a server from before `globalEnabled` and `blockedBy`. Records every
/// request it saw.
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
                let data = settings.lock().unwrap().clone();
                let effective = &data["chrome"]["settings"];
                if effective.is_object() {
                    let switch = effective["enabled"].as_bool().unwrap_or(true);
                    let blocked = data["blockedBy"].clone();
                    hosts["chrome"]["enabled"] = serde_json::json!(switch && blocked.is_null());
                    hosts["chrome"]["defaultBrowser"] = effective["defaultBrowser"].clone();
                    if data["legacy"] != true {
                        hosts["chrome"]["globalEnabled"] = serde_json::json!(switch);
                        hosts["chrome"]["blockedBy"] = if switch {
                            blocked
                        } else {
                            serde_json::Value::Null
                        };
                    }
                }
                hosts.to_string()
            } else if text.starts_with("GET /v1/info") {
                serde_json::json!({
                    "name": "haarness", "version": "test",
                    "schemaVersion": haarness_protocol::SCHEMA_VERSION,
                    "serverInstanceId": SERVER_ID, "capabilities": [],
                    "workspace": "/workspace",
                })
                .to_string()
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

        // A release build carries the extension that setup copies.
        app.chrome.local = Some(Setup {
            extension_source: Some(ExtensionSource {
                kind: "embedded".into(),
            }),
            ..Setup::default()
        });
        let rendered = chrome_text(&app, size);
        assert!(
            !rendered.contains("has no Haarness for Chrome"),
            "{rendered}"
        );
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
                ..Access::default()
            };
        }
        app.chrome.settings = Some(Settings {
            enabled: false,
            default_browser: "chrome".into(),
            restrictions: vec![Restriction::Text(
                "Chrome is turned off for this project".into(),
            )],
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
            flat(&rendered).contains("This Haarness installation doesn't include Haarness for Chrome. Set it up from Haarness Desktop (Settings → Browser → Chrome), then press Enter here."),
            "{width}\n{rendered}"
        );
        assert!(!rendered.contains("Waiting for Chrome"), "{rendered}");
        assert!(!rendered.contains("c copy path"), "{rendered}");
    }
    app.chrome.program = Some("/nonexistent/haarness".into());
    app.chrome_primary(&client);
    assert_eq!(app.chrome.busy_local(), Some(LocalAction::Status));
}

/// Without an extension to load, setup registers nothing; the dialog says
/// why and passes on setup's word on what to install, instead of offering a
/// setup that cannot help.
#[test]
fn a_build_without_the_extension_says_what_to_install() {
    let mut app = connect_fixture();
    app.open_overlay(Overlay::Chrome);
    app.chrome.here = true;
    app.chrome.hosts = Some(Ok(Hosts::default()));
    app.chrome.local = Some(Setup {
        warnings: vec!["Install a Haarness release or Haarness Desktop.".into()],
        ..Setup::default()
    });
    for width in [80, 120] {
        let rendered = chrome_text(&app, (width, 44));
        assert!(rendered.contains("○ Not set up"), "{width}\n{rendered}");
        let text = flat(&rendered);
        assert!(
            text.contains("This build of Haarness has no Haarness for Chrome extension to load.")
                && text.contains("Install a Haarness release or Haarness Desktop."),
            "{width}\n{rendered}"
        );
        assert!(
            !text.contains("Press Enter to let Chrome start Haarness"),
            "{rendered}"
        );
    }
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

/// "Allow once" approves one exact call, so the card shows that call: the
/// address it opens, the script it runs and the files it uploads, wrapped to
/// the card, without the broker's plumbing or opaque tab ids.
#[test]
fn a_chrome_card_shows_the_call_it_approves() {
    let thread = fixture_thread();
    let mut app = App::new(thread.clone(), vec![thread.clone()], vec![], vec![], vec![]);
    let mut pending = chrome_permission(&thread, "script");
    pending.patterns = vec![format!(
        "browser_site:chrome-scripts:{}:https://bank.example",
        thread.id
    )];
    let script = "const response = await fetch('/api/transfer', {method: 'POST', body: JSON.stringify({to: 'DE89370400440532013000', amount: 5000})});\nreturn response.status;";
    pending.detail = serde_json::json!({
        "tabID": "tab_0b5f3a52-5c7e-4c43-9a59-4a2b8f6a0d11",
        "script": script,
        "__browser": {"lease": "secret-lease", "policy": {"enabled": true}},
    });
    app.pending_permissions = vec![pending];
    app.permission_focus = true;
    for width in [80, 120] {
        let frame = frame_text(&app, (width, 40));
        let card = flat(&frame);
        assert!(
            card.contains("Run scripts on https://bank.example in Chrome"),
            "{width}\n{frame}"
        );
        // Wrapped, the script is all there, under its name.
        assert!(
            card.contains("script: const response = await fetch('/api/transfer',"),
            "{width}\n{frame}"
        );
        // Rejoined without the card's rail, the script is whole.
        let squeeze = |text: &str| text.replace('▎', "").split_whitespace().collect::<String>();
        assert!(
            squeeze(&card).contains(&squeeze(script)),
            "{width}\n{frame}"
        );
        for absent in ["__browser", "secret-lease", "tab_0b5f3a52", "tabID"] {
            assert!(!frame.contains(absent), "{width}: {absent}\n{frame}");
        }
        for row in frame.lines() {
            assert!(row.width() <= width as usize, "{width}: {row}");
        }
    }

    // A navigation shows the whole address, an upload its files.
    let mut pending = chrome_permission(&thread, "navigate");
    pending.detail = serde_json::json!({
        "tabID": "tab_0b5f3a52-5c7e-4c43-9a59-4a2b8f6a0d11",
        "url": "https://bank.example/transfer?to=DE89370400440532013000&amount=5000",
    });
    app.pending_permissions = vec![pending];
    let frame = frame_text(&app, (120, 40));
    assert!(
        frame.contains("url: https://bank.example/transfer?to=DE89370400440532013000&amount=5000"),
        "{frame}"
    );
    let mut pending = chrome_permission(&thread, "upload");
    pending.patterns = vec![format!(
        "browser_site:chrome-uploads:{}:https://bank.example",
        thread.id
    )];
    pending.detail = serde_json::json!({
        "tabID": "tab_0b5f3a52-5c7e-4c43-9a59-4a2b8f6a0d11", "ref": "e12",
        "paths": ["statements/2026-09.pdf", "statements/2026-10.pdf"],
    });
    app.pending_permissions = vec![pending];
    let frame = frame_text(&app, (120, 40));
    assert!(
        frame.contains("paths: statements/2026-09.pdf, statements/2026-10.pdf"),
        "{frame}"
    );
    assert!(frame.contains("ref: e12"), "{frame}");

    // A script longer than the card scrolls inside it, and the choices stay
    // on screen.
    let mut pending = chrome_permission(&thread, "long");
    pending.detail = serde_json::json!({
        "script": (1..=40).map(|line| format!("step{line}();")).collect::<Vec<_>>().join("\n"),
    });
    app.pending_permissions = vec![pending];
    let frame = frame_text(&app, (100, 40));
    assert!(frame.contains("step1();"), "{frame}");
    assert!(!frame.contains("step40();"), "{frame}");
    assert!(frame.contains("lines 1–10 of "), "{frame}");
    assert!(frame.contains("Allow once  y"), "{frame}");
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

/// An MCP tool whose server pairs it with a page says, under its row, that
/// the page opens in Desktop; a plain tool and a failed call say nothing.
#[test]
fn an_mcp_tool_with_an_app_says_its_app_opens_in_desktop() {
    let thread = fixture_thread();
    let call = |sequence, id: &str, name: &str| {
        message_item(
            sequence,
            &thread,
            ItemData::ToolCall {
                call_id: id.into(),
                name: name.into(),
                arguments: serde_json::json!({"city": "Porto"}),
                provider_metadata: serde_json::Value::Null,
            },
        )
    };
    let result = |sequence, id: &str, name: &str, is_error: bool, metadata| {
        message_item(
            sequence,
            &thread,
            ItemData::ToolResult {
                call_id: id.into(),
                name: name.into(),
                output: "Porto: sunny, 21 C".into(),
                is_error,
                metadata,
            },
        )
    };
    let page = serde_json::json!({
        "city": "Porto",
        "mcp_app": {"server": "weather", "uri": "ui://weather/view.html", "result": {"content": []}},
    });
    let items = vec![
        call(1, "c1", "mcp__weather__show_weather"),
        result(2, "c1", "mcp__weather__show_weather", false, page.clone()),
        call(3, "c2", "mcp__weather__plain"),
        result(
            4,
            "c2",
            "mcp__weather__plain",
            false,
            serde_json::json!({"city": "Porto"}),
        ),
        call(5, "c3", "mcp__weather__broken"),
        result(6, "c3", "mcp__weather__broken", true, page),
    ];
    let app = App::new(thread.clone(), vec![thread.clone()], items, vec![], vec![]);
    let frame = frame_text(&app, (120, 30));
    // The notice wraps to the transcript's width.
    let notice = "↳ This tool comes with an app; open the chat in Haarness Desktop";
    assert_eq!(frame.matches(notice).count(), 1, "{frame}");
    let rows: Vec<_> = frame.lines().collect();
    let show = rows
        .iter()
        .position(|row| row.contains("mcp__weather__show_weather"))
        .unwrap();
    assert!(rows[show + 1].contains(notice), "{frame}");
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

/// The switch is the shared "Let agents use Chrome" setting, not whether
/// agents may use Chrome in this project. When agent browser control or the
/// project keeps Chrome off under it, the dialog says which, never offers a
/// turn-on that changes nothing, and `e` still turns the switch itself off.
#[tokio::test]
async fn chrome_says_what_keeps_it_off_when_the_switch_is_on() {
    let global = serde_json::json!({
        "enabled": true,
        "defaults": {"browsing": "ask", "downloads": "ask", "uploads": "ask", "scripts": "ask"},
        "defaultBrowser": "auto", "sites": [],
    });
    let hosts = serde_json::json!({"hosts": [], "chrome": {"enabled": true, "defaultBrowser": "auto", "everConnected": false}});
    let sets = |requests: &Arc<Mutex<Vec<String>>>| {
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.contains("\"action\":\"set\""))
            .count()
    };
    for (blocked, legacy, restrictions, heading, advice) in [
        (
            "browser-control",
            false,
            serde_json::json!([]),
            "○ Agent browser control is off",
            "Turn on Agent browser control in Haarness Desktop (Settings → Browser), or set browser.policy.enabled to true",
        ),
        (
            "project",
            false,
            serde_json::json!([{"kind": "off"}]),
            "○ This project's settings turn Chrome off",
            "remove browser.chrome.enabled: false from this project's Haarness config",
        ),
        // A server from before `blockedBy` says only that Chrome is off;
        // the project's restrictions tell the two causes apart.
        (
            "browser-control",
            true,
            serde_json::json!(null),
            "○ Agent browser control is off",
            "Agents can't use any browser in this project, Chrome included.",
        ),
        (
            "project",
            true,
            serde_json::json!(null),
            "○ This project's settings turn Chrome off",
            "Agents can still use Chrome in your other projects.",
        ),
    ] {
        let mut chrome = serde_json::json!({"settings": global, "globalSettings": global,
        "projectRestrictions": if legacy && blocked == "project" {
            serde_json::json!(["Chrome is turned off for this project."])
        } else {
            serde_json::json!([])
        }});
        if !restrictions.is_null() {
            chrome["restrictions"] = restrictions;
        }
        let (client, requests) = chrome_server(
            hosts.clone(),
            serde_json::json!({"revision": "r1", "chrome": chrome, "blockedBy": blocked, "legacy": legacy}),
        )
        .await;
        let mut app = connect_fixture();
        app.open_chrome(&client);
        settle_chrome(&mut app, &client).await;
        for width in [80, 120] {
            let rendered = chrome_text(&app, (width, 44));
            let words = flat(&rendered);
            assert!(
                rendered.contains(heading),
                "{blocked} {legacy}: {width}\n{rendered}"
            );
            assert!(
                words.contains(advice),
                "{blocked} {legacy}: {width}\n{rendered}"
            );
            assert!(
                !rendered.contains("enter turn on"),
                "{blocked}: {width}\n{rendered}"
            );
            assert!(
                rendered.contains("enter check again"),
                "{blocked}: {width}\n{rendered}"
            );
            // The switch shows the shared setting, which is on.
            let row = rendered
                .lines()
                .find(|row| row.contains("Let agents use Chrome"))
                .unwrap();
            assert!(row.contains("On"), "{width}\n{rendered}");
            for row in rendered.lines() {
                assert!(row.width() <= width as usize, "{width}: {row}");
            }
        }
        // Enter looks again rather than rewrite a switch that is already on.
        crate::input::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &client,
        )
        .await
        .unwrap();
        settle_chrome(&mut app, &client).await;
        assert_eq!(sets(&requests), 0, "{blocked} {legacy}");

        // `e` turns the shared switch off; the dialog then offers to turn it
        // back on, the first thing that has to happen.
        crate::input::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
            &client,
        )
        .await
        .unwrap();
        settle_chrome(&mut app, &client).await;
        assert_eq!(sets(&requests), 1, "{blocked} {legacy}");
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.contains("\"action\":\"set\"")
                    && request.contains("\"enabled\":false")),
            "{blocked} {legacy}"
        );
        let rendered = chrome_text(&app, (100, 44));
        assert!(
            rendered.contains("○ Agents may not use Chrome"),
            "{rendered}"
        );
        assert!(rendered.contains("enter turn on"), "{rendered}");
    }
}

/// Turning the switch on shows "Checking…" until the server says what that
/// changed, rather than guess from settings that cannot see agent browser
/// control.
#[tokio::test]
async fn turning_chrome_on_waits_for_the_server_to_say_what_changed() {
    let global = serde_json::json!({
        "enabled": false,
        "defaults": {"browsing": "ask", "downloads": "ask", "uploads": "ask", "scripts": "ask"},
        "defaultBrowser": "auto", "sites": [],
    });
    let (client, _) = chrome_server(
        serde_json::json!({"hosts": [], "chrome": {"enabled": false, "defaultBrowser": "auto", "everConnected": false}}),
        serde_json::json!({"revision": "r1", "chrome": {"settings": global, "globalSettings": global,
            "restrictions": []}, "blockedBy": "browser-control"}),
    )
    .await;
    let mut app = connect_fixture();
    app.open_chrome(&client);
    settle_chrome(&mut app, &client).await;
    assert!(chrome_text(&app, (100, 44)).contains("enter turn on"));
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    // Saved, the switch reads On while the server is asked again.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.chrome.saving().is_some() && std::time::Instant::now() < deadline {
        app.poll_chrome(&client);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    // The poll that took the saved settings has only just asked again.
    let rendered = chrome_text(&app, (100, 44));
    assert!(rendered.contains("Checking…"), "{rendered}");
    assert!(
        !rendered.contains("Agent browser control is off"),
        "{rendered}"
    );
    let row = rendered
        .lines()
        .find(|row| row.contains("Let agents use Chrome"))
        .unwrap();
    assert!(row.contains("On"), "{rendered}");
    settle_chrome(&mut app, &client).await;
    let rendered = chrome_text(&app, (100, 44));
    assert!(
        rendered.contains("○ Agent browser control is off"),
        "{rendered}"
    );
}

/// Project restrictions read in the person's language, from the structured
/// list a server sends, or as sent by an older one.
#[test]
fn project_restrictions_are_worded_in_the_chosen_language() {
    let mut app = connect_fixture();
    app.open_overlay(Overlay::Chrome);
    app.chrome.here = true;
    app.chrome.local = Some(registered_setup());
    app.chrome.hosts = Some(Ok(Hosts::default()));
    let restrictions = vec![
        Restriction::Default {
            category: "scripts".into(),
            decision: "deny".into(),
        },
        Restriction::Site {
            origin: "https://bank.example".into(),
            category: "downloads".into(),
            decision: "ask".into(),
        },
        Restriction::Off,
    ];
    app.chrome.settings = Some(Settings {
        enabled: true,
        default_browser: "auto".into(),
        restrictions,
    });
    let rendered = flat(&chrome_text(&app, (120, 60)));
    for expected in [
        "Restricted by this project's settings:",
        "· Scripts: Block by default",
        "· Downloads on https://bank.example: Requires approval",
        "· Chrome is turned off for this project.",
    ] {
        assert!(rendered.contains(expected), "{expected}\n{rendered}");
    }
    app.language = crate::locale::Language::Japanese;
    let rendered = flat(&chrome_text(&app, (120, 60)));
    for expected in [
        "· スクリプト: 既定でブロック",
        "· https://bank.example のダウンロード: 承認が必要",
        "· このプロジェクトでは Chrome がオフになっています。",
    ] {
        assert!(rendered.contains(expected), "{expected}\n{rendered}");
    }
    assert!(!rendered.contains("by default"), "{rendered}");

    // An older server's sentences show as they came.
    let mut chrome = crate::chrome::Chrome::default();
    let view = serde_json::json!({"settings": {"enabled": true}, "globalSettings": null,
        "projectRestrictions": ["Scripts: Block by default"]});
    let settings = crate::chrome::tests_settings_from_view(&view);
    assert_eq!(
        settings.restrictions,
        vec![Restriction::Text("Scripts: Block by default".into())]
    );
    chrome.settings = Some(settings);
    // A structured list wins, and kinds this client does not know are left out.
    let view = serde_json::json!({"settings": {"enabled": true}, "globalSettings": {"enabled": false},
        "projectRestrictions": ["Scripts: Block by default"],
        "restrictions": [{"kind": "default", "category": "scripts", "decision": "deny"}, {"kind": "later"}]});
    let settings = crate::chrome::tests_settings_from_view(&view);
    assert!(!settings.enabled, "the switch is the global value");
    assert_eq!(
        settings.restrictions,
        vec![Restriction::Default {
            category: "scripts".into(),
            decision: "deny".into()
        }]
    );
}

/// A CLI-only install says once, in the person's language, that there is
/// nothing to add and where to get it; the CLI's own English note for the
/// same thing is not repeated, while its other notes still show.
#[test]
fn nothing_to_add_is_said_once() {
    let mut app = connect_fixture();
    app.open_overlay(Overlay::Chrome);
    app.chrome.here = true;
    app.chrome.hosts = Some(Ok(Hosts::default()));
    app.chrome.local = Some(Setup {
        extension: None,
        warnings: vec![
            "Haarness for Chrome is not included in this installation. Set it up from Haarness Desktop (Settings → Browser → Chrome).".into(),
            "A separate Haarness data folder is in use (/tmp/x), so Chrome will work with that folder's projects only.".into(),
        ],
        ..registered_setup()
    });
    let rendered = flat(&chrome_text(&app, (120, 60)));
    assert!(
        rendered.contains("This Haarness installation doesn't include Haarness for Chrome."),
        "{rendered}"
    );
    assert!(
        !rendered.contains("is not included in this installation"),
        "{rendered}"
    );
    assert!(
        rendered.contains("A separate Haarness data folder is in use"),
        "{rendered}"
    );
    app.language = crate::locale::Language::German;
    let rendered = flat(&chrome_text(&app, (120, 60)));
    assert!(
        rendered.contains("Diese Haarness-Installation enthält Haarness for Chrome nicht."),
        "{rendered}"
    );
    assert!(!rendered.contains("is not included"), "{rendered}");
}

/// At 80x30 the steps no longer fit: the dialog scrolls instead of dropping
/// its last rows, and says how.
#[tokio::test]
async fn a_short_terminal_scrolls_the_chrome_dialog() {
    let client = offline_client();
    let mut app = connect_fixture();
    app.open_overlay(Overlay::Chrome);
    app.chrome.here = true;
    app.chrome.hosts = Some(Ok(Hosts::default()));
    app.chrome.local = Some(registered_setup());
    let last = "Chrome settings apply to every project on this computer.";
    let rendered = chrome_text(&app, (80, 30));
    assert!(rendered.contains("Waiting for Chrome…"), "{rendered}");
    assert!(rendered.contains("pgup/pgdn scroll"), "{rendered}");
    assert!(!rendered.contains(last), "{rendered}");
    for key in [KeyCode::End, KeyCode::Down, KeyCode::PageDown] {
        crate::input::handle_key(&mut app, KeyEvent::new(key, KeyModifiers::NONE), &client)
            .await
            .unwrap();
    }
    let rendered = chrome_text(&app, (80, 30));
    assert!(rendered.contains(last), "{rendered}");
    assert!(rendered.contains("Let agents use Chrome"), "{rendered}");
    assert!(!rendered.contains("Waiting for Chrome…"), "{rendered}");
    crate::input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
        &client,
    )
    .await
    .unwrap();
    assert!(chrome_text(&app, (80, 30)).contains("Waiting for Chrome…"));
    // Tall enough, nothing scrolls and the hint is gone.
    let rendered = chrome_text(&app, (80, 44));
    assert!(rendered.contains(last), "{rendered}");
    assert!(!rendered.contains("pgup/pgdn scroll"), "{rendered}");
}

/// A terminal attached by address to a project on this same computer sets
/// Chrome up here: `browser status` lists the servers Chrome reaches on this
/// computer, and the project's server is one of them. Another server, or
/// none, keeps the "elsewhere" wording, which no longer claims another
/// computer.
#[cfg(unix)]
#[tokio::test]
async fn an_attached_terminal_on_the_same_computer_sets_chrome_up_here() {
    use std::os::unix::fs::PermissionsExt as _;

    if crate::connect::over_ssh() {
        // Over SSH, Chrome is on the other end: nothing is looked up here.
        return;
    }
    let directory = std::env::temp_dir().join(format!("haarness-chrome-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&directory).unwrap();
    let log = directory.join("calls");
    let program = |server: &str| {
        let report = serde_json::json!({
            "removed": false, "warnings": [],
            "browsers": [{"browser":"chrome","name":"Google Chrome","extensionsPage":"chrome://extensions",
                "registered": false, "supported": true}],
            "extension": {"path": EXTENSION},
            "connections": [{"workspace": "/workspace", "serverId": server, "hosts": []}],
        });
        let script = directory.join(format!("haarness-{server}"));
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\necho '{report}'\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    };
    let (client, _) = chrome_server(
        serde_json::json!({"hosts": [], "chrome": {"enabled": true, "defaultBrowser": "auto", "everConnected": false}}),
        serde_json::json!({}),
    )
    .await;
    assert!(!client.supports_local_workspace_switch());

    let mut app = connect_fixture();
    app.chrome.program = Some(program(SERVER_ID));
    app.execute_command("/chrome", &client).await.unwrap();
    assert!(chrome_text(&app, (100, 40)).contains("Checking"));
    settle_chrome(&mut app, &client).await;
    assert!(app.chrome.here);
    let rendered = chrome_text(&app, (100, 40));
    assert!(rendered.contains("○ Not set up"), "{rendered}");
    assert!(rendered.contains("enter set up"), "{rendered}");
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "browser status --json\n"
    );

    // `/chrome setup` sets up once this computer turns out to be the one.
    app.close_overlay();
    app.chrome = crate::chrome::Chrome::default();
    app.chrome.program = Some(program(SERVER_ID));
    app.execute_command("/chrome setup", &client).await.unwrap();
    settle_chrome(&mut app, &client).await;
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "browser status --json\nbrowser status --json\nbrowser setup --json\n"
    );

    // Chrome here reaches another server only: not this project's.
    app.close_overlay();
    app.chrome = crate::chrome::Chrome::default();
    app.chrome.program = Some(program("0199d0c4-0000-7000-8000-000000000000"));
    app.execute_command("/chrome", &client).await.unwrap();
    settle_chrome(&mut app, &client).await;
    assert!(!app.chrome.here);
    for width in [80, 120] {
        let rendered = flat(&chrome_text(&app, (width, 40)));
        assert!(
            rendered.contains("This terminal can't set up Chrome for this project."),
            "{rendered}"
        );
        assert!(!rendered.contains("another computer"), "{rendered}");
        assert!(rendered.contains("c copy command"), "{rendered}");
    }
    let _ = std::fs::remove_dir_all(&directory);
}
