//! Computer use through the real runtime: a scripted model calls the mac_* tools, a stand-in helper
//! (`fake-helper.py`) answers for the Mac, and the tests read what the runtime asked, refused and ran.
#![cfg(target_os = "macos")]

use async_trait::async_trait;
use futures::stream;
use haarness_computer::{Computer, Config};
use haarness_core::{
    AgentRuntime, CoreResult, EventBus, ModelProvider, ProviderEvent, ProviderRequest,
    ProviderStream, RuntimeConfig, StaticProviderRegistry, ToolRegistry,
};
use haarness_protocol::{
    ApprovalMode, CreateThreadRequest, Item, ItemData, PendingPermissionInfo, PermissionReply,
    PermissionReplyDecision, ProviderInfo, RequestId, StartTurnRequest, ThreadId, TurnStatus,
};
use haarness_server::NativeToolRegistry;
use haarness_store::SqliteStore;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Scripted {
    rounds: Mutex<VecDeque<Vec<ProviderEvent>>>,
    requests: Mutex<Vec<ProviderRequest>>,
}

#[async_trait]
impl ModelProvider for Scripted {
    fn id(&self) -> &str {
        "fixture"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "fixture".into(),
            name: "Computer use fixture".into(),
            authenticated: true,
            models: vec![],
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        self.requests.lock().unwrap().push(request);
        let mut events = self
            .rounds
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![ProviderEvent::TextDelta("Done".into())]);
        if !matches!(events.last(), Some(ProviderEvent::Finished { .. })) {
            events.push(ProviderEvent::Finished {
                reason: "done".into(),
            });
        }
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

fn call(id: &str, name: &str, arguments: Value) -> ProviderEvent {
    ProviderEvent::ToolCall {
        call_id: id.into(),
        name: name.into(),
        arguments,
        provider_metadata: Value::Null,
    }
}

struct Lab {
    root: PathBuf,
    runtime: Arc<AgentRuntime>,
    provider: Arc<Scripted>,
    computer: Arc<Computer>,
    tools: Arc<NativeToolRegistry>,
    events: EventBus,
}

impl Lab {
    fn new(settings: Value) -> Self {
        let root = std::env::temp_dir().join(format!("haarness-computer-{}", uuid::Uuid::now_v7()));
        for folder in ["workspace", "config", "data", "cache"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        std::fs::write(
            root.join("config/computer-use.json"),
            serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
        // Each lab's stand-in writes its own log: tests run side by side in one process.
        let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fake-helper.py");
        let helper = root.join("helper");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\nFAKE_HELPER_LOG='{}' exec python3 '{}' \"$@\"\n",
                root.join("helper.log").display(),
                fake.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let computer = Computer::new(Config {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            helper: Some(helper),
            console: Some(true),
        });
        let provider = Arc::new(Scripted::default());
        let events = EventBus::new(256);
        computer.watch(&events);
        let tools =
            Arc::new(NativeToolRegistry::new(root.join("workspace")).with_tools(computer.tools()));
        let runtime = Arc::new(AgentRuntime::with_config(
            Arc::new(SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new(vec![
                provider.clone() as Arc<dyn ModelProvider>
            ])),
            tools.clone(),
            events.clone(),
            root.join("workspace"),
            String::new(),
            RuntimeConfig {
                approval_mode: ApprovalMode::Review,
                ..RuntimeConfig::default()
            },
        ));
        Self {
            root,
            runtime,
            provider,
            computer,
            tools,
            events,
        }
    }

    fn on() -> Value {
        json!({"enabled": true, "terms": {"version": "1", "acceptedAt": "2026-11-07T00:00:00Z"}})
    }

    fn with(extra: Value) -> Value {
        let mut settings = Self::on();
        for (key, value) in extra.as_object().unwrap() {
            settings[key] = value.clone();
        }
        settings
    }

    fn settings(&self, settings: Value) {
        // A later write in the same millisecond keeps its size apart from the cached one by its content.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(
            self.root.join("config/computer-use.json"),
            serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
    }

    fn log(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.join("helper.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn ops(&self) -> Vec<String> {
        self.log()
            .iter()
            .map(|request| request["op"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    async fn thread(&self) -> ThreadId {
        self.runtime
            .create_thread(CreateThreadRequest {
                title: Some("Computer use".into()),
                provider: Some("fixture".into()),
                model: Some("fixture".into()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
    }

    async fn start(
        &self,
        thread: ThreadId,
        rounds: Vec<Vec<ProviderEvent>>,
    ) -> haarness_protocol::Turn {
        self.provider.rounds.lock().unwrap().extend(rounds);
        self.runtime
            .start_turn(
                thread,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text: "Use the Mac".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
    }

    async fn permission(&self) -> PendingPermissionInfo {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(pending) = self.runtime.pending_permissions().into_iter().next() {
                    return pending;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("an approval is asked")
    }

    async fn answer(&self, pending: &PendingPermissionInfo, decision: PermissionReplyDecision) {
        self.runtime
            .reply_permission(
                &pending.permission_id,
                PermissionReply {
                    decision,
                    feedback: None,
                },
            )
            .await
            .unwrap();
    }

    async fn finish(&self, thread: ThreadId) -> Vec<Item> {
        tokio::time::timeout(Duration::from_secs(15), async {
            while self.runtime.active_turn(thread).await.unwrap().is_some() {
                let unexpected: Vec<_> = self
                    .runtime
                    .pending_permissions()
                    .into_iter()
                    .filter(|pending| pending.thread_id == thread)
                    .collect();
                assert!(
                    unexpected.is_empty(),
                    "an approval nobody expected: {unexpected:?}"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the turn finishes");
        self.runtime.items(thread, None).await.unwrap()
    }

    async fn run(&self, thread: ThreadId, rounds: Vec<Vec<ProviderEvent>>) -> Vec<Item> {
        self.start(thread, rounds).await;
        self.finish(thread).await
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn result<'a>(items: &'a [Item], id: &str) -> (&'a str, bool, &'a Value) {
    items
        .iter()
        .find_map(|item| match &item.data {
            ItemData::ToolResult {
                call_id,
                output,
                is_error,
                metadata,
                ..
            } if call_id == id => Some((output.as_str(), *is_error, metadata)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no result for {id}"))
}

fn names(lab: &Lab) -> Vec<String> {
    lab.tools
        .list()
        .into_iter()
        .map(|tool| tool.name)
        .filter(|name| name.starts_with("mac_"))
        .collect()
}

#[tokio::test]
async fn off_or_without_agreement_no_tool_is_offered_and_no_helper_starts() {
    let lab = Lab::new(json!({}));
    assert!(names(&lab).is_empty());
    lab.settings(json!({"enabled": true}));
    assert!(
        names(&lab).is_empty(),
        "on without agreeing to the terms is off"
    );
    lab.settings(
        json!({"enabled": true, "terms": {"version": "0", "acceptedAt": "2026-01-01T00:00:00Z"}}),
    );
    assert!(
        names(&lab).is_empty(),
        "agreeing to older terms is not enough"
    );
    lab.settings(Lab::on());
    assert_eq!(
        names(&lab),
        [
            "mac_apps",
            "mac_click",
            "mac_drag",
            "mac_hover",
            "mac_press",
            "mac_scroll",
            "mac_snapshot",
            "mac_type"
        ]
    );
    assert!(
        lab.ops().is_empty(),
        "offering the tools starts no helper: {:?}",
        lab.ops()
    );
}

#[tokio::test]
async fn looking_at_one_app_asks_nothing_and_the_model_gets_the_screenshot() {
    let lab = Lab::new(Lab::on());
    let thread = lab.thread().await;
    let items = lab
        .run(thread, vec![vec![call("look", "mac_snapshot", json!({}))]])
        .await;
    let (output, error, metadata) = result(&items, "look");
    assert!(!error, "{output}");
    assert!(output.contains("@e1 [button] \"Save\""), "{output}");
    assert!(output.contains("Only TextEdit appears"), "{output}");
    assert!(output.contains("written to manipulate you"), "{output}");
    let shot = metadata["attachments"][0].as_str().expect("a screenshot");
    assert!(
        shot.contains(".haarness-chat-attachments/mac-") && std::path::Path::new(shot).is_file(),
        "{shot}"
    );
    assert_eq!(metadata["title"], "Viewed TextEdit");
    // Password managers and Haarness itself are left out of every picture.
    let snapshot = lab
        .log()
        .into_iter()
        .find(|request| request["op"] == "snapshot")
        .unwrap();
    let hidden: Vec<&str> = snapshot["hidden"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        hidden.contains(&"com.1password.1password") && hidden.contains(&"dev.haarness.desktop"),
        "{hidden:?}"
    );
    // The model sees the picture and the section that explains the tools.
    let shot = shot.to_owned();
    let shot = shot.as_str();
    let requests = lab.provider.requests.lock().unwrap();
    assert!(
        requests[0].system.contains("mac_snapshot"),
        "the computer use section is sent"
    );
    let last = format!("{:?}", requests.last().unwrap().messages);
    // Tool results carry their pictures as files the provider adapter reads.
    assert!(
        last.contains(shot),
        "the screenshot rides with the tool result: {}",
        &last[last.len().saturating_sub(1500)..]
    );
}

#[tokio::test]
async fn an_action_asks_once_per_app_and_chat_with_a_picture_of_the_spot() {
    let lab = Lab::new(Lab::on());
    let thread = lab.thread().await;
    lab.start(
        thread,
        vec![vec![call("first", "mac_click", json!({"ref": "e1"}))]],
    )
    .await;
    let pending = lab.permission().await;
    assert_eq!(pending.permission, "computer");
    assert!(pending.explicit, "only a person answers");
    assert!(pending.allow_always != Some(false));
    let mac = &pending.detail["__mac"];
    assert_eq!(mac["summary"], "Click button \"Save\" in TextEdit");
    assert!(
        mac["preview"]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,")
    );
    assert_eq!(mac["bundle"], "com.apple.TextEdit");
    assert!(
        !lab.ops().contains(&"click".to_owned()),
        "nothing acts before the answer"
    );
    lab.answer(&pending, PermissionReplyDecision::Always).await;
    let items = lab.finish(thread).await;
    let (output, error, _) = result(&items, "first");
    assert!(!error, "{output}");
    assert!(
        output.starts_with("Clicked button \"Save\" in TextEdit"),
        "{output}"
    );
    let click = lab
        .log()
        .into_iter()
        .find(|request| request["op"] == "click")
        .unwrap();
    assert_eq!(
        click["expect"], "com.apple.TextEdit",
        "the helper re-checks the target app just before the click"
    );
    assert_eq!(click["ref"], 1);
    // The same app in the same chat: no new question, also for typing.
    let items = lab
        .run(
            thread,
            vec![vec![
                call("again", "mac_click", json!({"ref": "e1"})),
                call("type", "mac_type", json!({"ref": "e2", "text": "hello"})),
            ]],
        )
        .await;
    assert!(!result(&items, "again").1 && !result(&items, "type").1);
    // Another chat asks again.
    let other = lab.thread().await;
    lab.start(
        other,
        vec![vec![call("elsewhere", "mac_click", json!({"ref": "e1"}))]],
    )
    .await;
    let pending = lab.permission().await;
    lab.answer(&pending, PermissionReplyDecision::Once).await;
    lab.finish(other).await;
}

#[tokio::test]
async fn a_drag_into_another_app_asks_only_about_the_app_not_yet_allowed() {
    let lab = Lab::new(Lab::with(json!({"apps": {"com.apple.TextEdit": "allow"}})));
    let thread = lab.thread().await;
    lab.start(
        thread,
        vec![vec![call(
            "drag",
            "mac_drag",
            json!({"from": {"ref": "e1"}, "to": {"ref": "e4"}}),
        )]],
    )
    .await;
    let pending = lab.permission().await;
    // Clients name and offer to allow the app the request asks about, which is the drop's, not the pickup's.
    let mac = &pending.detail["__mac"];
    assert_eq!(mac["bundle"], "com.apple.TextEdit");
    assert_eq!(mac["endBundle"], "com.apple.Notes");
    assert_eq!(mac["endApp"], "Notes");
    assert_eq!(pending.patterns, [format!("act:{thread}:com.apple.Notes")]);
    assert_eq!(mac["asks"], json!(pending.patterns));
    lab.answer(&pending, PermissionReplyDecision::Always).await;
    let items = lab.finish(thread).await;
    assert!(!result(&items, "drag").1, "{}", result(&items, "drag").0);
}

async fn set_mode(lab: &Lab, thread: ThreadId, mode: haarness_protocol::ApprovalModeSetting) {
    lab.runtime
        .set_approval_mode(
            thread,
            haarness_protocol::SetApprovalModeRequest {
                mode,
                revision: None,
            },
        )
        .await
        .unwrap();
}

/// Yolo is yes to everything: in a chat in Yolo computer use asks nothing,
/// not even where it would ask every time (an app set to ask, a terminal's
/// text). Never still refuses, and Auto still leaves it to a person.
#[tokio::test]
async fn yolo_answers_computer_use_and_auto_leaves_it_to_a_person() {
    let lab = Lab::new(Lab::with(
        json!({"apps": {"com.apple.TextEdit": "ask", "com.apple.Notes": "deny"}}),
    ));
    let thread = lab.thread().await;
    set_mode(&lab, thread, haarness_protocol::ApprovalModeSetting::Yolo).await;
    for (id, element) in [("ruled", "e1"), ("terminal", "e6"), ("button", "e5")] {
        let items = lab
            .run(
                thread,
                vec![vec![call(id, "mac_click", json!({"ref": element}))]],
            )
            .await;
        let (output, error, _) = result(&items, id);
        assert!(!error, "{id}: {output}");
    }
    assert_eq!(
        lab.ops().iter().filter(|op| *op == "click").count(),
        3,
        "{:?}",
        lab.ops()
    );
    let items = lab
        .run(
            thread,
            vec![vec![call("never", "mac_click", json!({"ref": "e4"}))]],
        )
        .await;
    let (output, error, _) = result(&items, "never");
    assert!(
        error && output.contains("Notes is set to Never"),
        "{output}"
    );

    let auto = lab.thread().await;
    set_mode(&lab, auto, haarness_protocol::ApprovalModeSetting::Auto).await;
    lab.start(
        auto,
        vec![vec![call("auto", "mac_click", json!({"ref": "e5"}))]],
    )
    .await;
    let pending = lab.permission().await;
    assert_eq!(pending.permission, "computer");
    lab.answer(&pending, PermissionReplyDecision::Reject).await;
    let items = lab.finish(auto).await;
    assert!(result(&items, "auto").1);
}

#[tokio::test]
async fn app_rules_allow_without_asking_ask_every_time_or_refuse() {
    let lab = Lab::new(Lab::with(
        json!({"apps": {"com.apple.TextEdit": "allow", "com.apple.Notes": "deny"}}),
    ));
    let thread = lab.thread().await;
    let items = lab
        .run(
            thread,
            vec![vec![call("allowed", "mac_click", json!({"ref": "e1"}))]],
        )
        .await;
    assert!(!result(&items, "allowed").1);
    let items = lab
        .run(
            thread,
            vec![vec![call("denied", "mac_click", json!({"ref": "e4"}))]],
        )
        .await;
    let (output, error, _) = result(&items, "denied");
    assert!(
        error && output.contains("Notes is set to Never"),
        "{output}"
    );
    // Set to ask: asked each time, with nothing lasting on offer.
    lab.settings(Lab::with(json!({"apps": {"com.apple.TextEdit": "ask"}})));
    for round in ["one", "two"] {
        lab.start(
            thread,
            vec![vec![call(round, "mac_click", json!({"ref": "e1"}))]],
        )
        .await;
        let pending = lab.permission().await;
        assert_eq!(pending.allow_always, Some(false), "{round}");
        assert_eq!(pending.detail["__mac"]["everyTime"], true);
        lab.answer(&pending, PermissionReplyDecision::Always).await;
        lab.finish(thread).await;
    }
    // Acting in apps without a rule can be allowed for every app.
    lab.settings(Lab::with(json!({"defaultAct": "allow"})));
    let items = lab
        .run(
            thread,
            vec![vec![call("open", "mac_click", json!({"ref": "e1"}))]],
        )
        .await;
    assert!(!result(&items, "open").1);
}

#[tokio::test]
async fn fixed_groups_and_password_fields_are_refused_before_anyone_is_asked() {
    let lab = Lab::new(Lab::with(json!({"defaultAct": "allow"})));
    let thread = lab.thread().await;
    let items = lab
        .run(
            thread,
            vec![vec![
                call("browser", "mac_click", json!({"ref": "e9"})),
                call("haarness", "mac_click", json!({"ref": "e7"})),
                call(
                    "password",
                    "mac_type",
                    json!({"ref": "e8", "text": "secret"}),
                ),
                call(
                    "terminal",
                    "mac_type",
                    json!({"ref": "e6", "text": "rm -rf ~"}),
                ),
                call("logout", "mac_press", json!({"key": "Meta+Shift+Q"})),
                call(
                    "dock",
                    "mac_drag",
                    json!({"from": {"ref": "e1"}, "to": {"ref": "e9"}}),
                ),
                call(
                    "hidden",
                    "mac_snapshot",
                    json!({"app": "com.1password.1password"}),
                ),
            ]],
        )
        .await;
    for (id, words) in [
        ("browser", "browser_* tools"),
        ("haarness", "belongs to Haarness"),
        ("password", "Password fields are refused"),
        ("terminal", "never type"),
        ("logout", "is blocked"),
        ("dock", "browser_* tools"),
        ("hidden", "invisible to computer use"),
    ] {
        let (output, error, _) = result(&items, id);
        assert!(error && output.contains(words), "{id}: {output}");
    }
    let ops = lab.ops();
    assert!(
        !ops.iter()
            .any(|op| matches!(op.as_str(), "click" | "type" | "press" | "drag")),
        "{ops:?}"
    );
}

#[tokio::test]
async fn a_click_on_terminal_text_asks_every_time_and_its_buttons_once_per_chat() {
    let lab = Lab::new(Lab::with(
        json!({"defaultAct": "allow", "apps": {"com.apple.Terminal": "allow"}}),
    ));
    let thread = lab.thread().await;
    for round in ["text-one", "text-two"] {
        lab.start(
            thread,
            vec![vec![call(round, "mac_click", json!({"ref": "e6"}))]],
        )
        .await;
        let pending = lab.permission().await;
        assert_eq!(pending.allow_always, Some(false));
        assert_eq!(pending.detail["__mac"]["console"], true);
        lab.answer(&pending, PermissionReplyDecision::Always).await;
        lab.finish(thread).await;
    }
    // Its own buttons follow the app rule, but no rule allows a terminal for good: it asks once in the chat.
    lab.start(
        thread,
        vec![vec![call("button", "mac_click", json!({"ref": "e5"}))]],
    )
    .await;
    let pending = lab.permission().await;
    assert!(pending.allow_always != Some(false));
    lab.answer(&pending, PermissionReplyDecision::Always).await;
    lab.finish(thread).await;
    let items = lab
        .run(
            thread,
            vec![vec![call(
                "button-again",
                "mac_click",
                json!({"ref": "e5"}),
            )]],
        )
        .await;
    assert!(!result(&items, "button-again").1);
}

#[tokio::test]
async fn launching_and_the_whole_screen_ask_and_bringing_forward_does_not() {
    let lab = Lab::new(Lab::on());
    let thread = lab.thread().await;
    let items = lab
        .run(
            thread,
            vec![vec![
                call(
                    "front",
                    "mac_apps",
                    json!({"action": "activate", "app": "TextEdit"}),
                ),
                call("list", "mac_apps", json!({"action": "list"})),
            ]],
        )
        .await;
    assert!(!result(&items, "front").1, "{}", result(&items, "front").0);
    let (list, _, _) = result(&items, "list");
    assert!(
        list.contains("TextEdit") && !list.contains("1Password"),
        "{list}"
    );
    lab.start(
        thread,
        vec![vec![call(
            "open",
            "mac_apps",
            json!({"action": "launch", "app": "com.apple.Notes"}),
        )]],
    )
    .await;
    let pending = lab.permission().await;
    assert_eq!(pending.detail["__mac"]["summary"], "Open Notes");
    lab.answer(&pending, PermissionReplyDecision::Once).await;
    let items = lab.finish(thread).await;
    assert!(!result(&items, "open").1, "{}", result(&items, "open").0);
    lab.start(
        thread,
        vec![vec![call(
            "everything",
            "mac_snapshot",
            json!({"wholeScreen": true}),
        )]],
    )
    .await;
    let pending = lab.permission().await;
    assert_eq!(pending.detail["__mac"]["kind"], "screen");
    lab.answer(&pending, PermissionReplyDecision::Once).await;
    lab.finish(thread).await;
    assert!(
        lab.log()
            .iter()
            .any(|request| request["op"] == "snapshot" && request["whole"] == true)
    );
}

#[tokio::test]
async fn one_chat_at_a_time_holds_the_screen_until_its_turn_ends() {
    let lab = Lab::new(Lab::on());
    let first = lab.thread().await;
    let second = lab.thread().await;
    // The first chat looks, then waits on an approval: its turn still runs, so it holds the screen.
    lab.start(
        first,
        vec![
            vec![call("look", "mac_snapshot", json!({}))],
            vec![call("click", "mac_click", json!({"ref": "e1"}))],
        ],
    )
    .await;
    let pending = lab.permission().await;
    let items = lab
        .run(second, vec![vec![call("busy", "mac_snapshot", json!({}))]])
        .await;
    let (output, error, _) = result(&items, "busy");
    assert!(
        error && output.contains("another chat is using it"),
        "{output}"
    );
    lab.answer(&pending, PermissionReplyDecision::Once).await;
    lab.finish(first).await;
    // The turn ended: the screen is free and the bar was told to go.
    let items = lab
        .run(second, vec![vec![call("free", "mac_snapshot", json!({}))]])
        .await;
    assert!(!result(&items, "free").1, "{}", result(&items, "free").0);
    assert!(lab.ops().contains(&"release".to_owned()), "{:?}", lab.ops());
}

#[tokio::test]
async fn turning_it_off_while_an_approval_waits_stops_the_action() {
    let lab = Lab::new(Lab::on());
    let thread = lab.thread().await;
    lab.start(
        thread,
        vec![vec![call("late", "mac_click", json!({"ref": "e1"}))]],
    )
    .await;
    let pending = lab.permission().await;
    lab.computer.enable(false).unwrap();
    lab.answer(&pending, PermissionReplyDecision::Once).await;
    let items = lab.finish(thread).await;
    let (output, error, _) = result(&items, "late");
    assert!(error && output.contains("switched off"), "{output}");
    assert!(!lab.ops().contains(&"click".to_owned()));
    assert!(
        names(&lab).is_empty(),
        "and the next turn has no computer tools"
    );
}

#[tokio::test]
async fn a_pause_holds_the_agent_and_after_resume_it_looks_again() {
    let lab = Lab::new(Lab::with(json!({"defaultAct": "allow"})));
    let thread = lab.thread().await;
    let items = lab
        .run(
            thread,
            vec![vec![call(
                "paused",
                "mac_type",
                json!({"ref": "e2", "text": "PAUSE"}),
            )]],
        )
        .await;
    let (output, error, _) = result(&items, "paused");
    assert!(
        error && output.contains("Paused") && output.contains("Call mac_snapshot"),
        "{output}"
    );
    assert!(!lab.computer.is_paused(), "the stand-in resumed");
}

/// The next computer use event clients are sent.
async fn next_update(
    events: &mut tokio::sync::broadcast::Receiver<haarness_protocol::EventEnvelope>,
) -> haarness_protocol::EventEnvelope {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.expect("the event stream stays open");
            if event.event_type == haarness_computer::UPDATED_EVENT {
                return event;
            }
        }
    })
    .await
    .expect("clients hear about the pause")
}

/// The computer use events clients were sent and nobody has read yet.
fn unread_updates(
    events: &mut tokio::sync::broadcast::Receiver<haarness_protocol::EventEnvelope>,
) -> Vec<haarness_protocol::EventEnvelope> {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| event.event_type == haarness_computer::UPDATED_EVENT)
        .collect()
}

#[tokio::test]
async fn a_paused_chat_is_told_so_and_resume_from_a_client_lets_the_agent_go_on() {
    let lab = Lab::new(Lab::with(json!({"defaultAct": "allow"})));
    let thread = lab.thread().await;
    let mut events = lab.events.subscribe(None).receiver;
    lab.start(
        thread,
        vec![
            vec![call(
                "held",
                "mac_type",
                json!({"ref": "e2", "text": "HOLD", "snapshot": false}),
            )],
            vec![call("after", "mac_snapshot", json!({}))],
        ],
    )
    .await;
    // The person pressed Pause on the strip: the chat holding the screen hears it, and nobody is asked anything.
    let paused = next_update(&mut events).await;
    assert_eq!(paused.thread_id, Some(thread));
    assert_eq!(paused.payload, json!({"paused": true}));
    assert_eq!(lab.computer.paused_chat(), Some(thread));
    assert_eq!(lab.computer.status().await["pausedChat"], json!(thread));
    // A client that connects meanwhile learns it from the answer to the words it hands over.
    let words = lab
        .computer
        .manage(&json!({"action": "words"}))
        .await
        .unwrap();
    assert_eq!(words["pausedChat"], json!(thread));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        lab.runtime.active_turn(thread).await.unwrap().is_some(),
        "the agent waits for the person"
    );
    // Its action waits too, even though the strip's refusal came before the press reached the server.
    let items = lab.runtime.items(thread, None).await.unwrap();
    assert!(
        !items.iter().any(
            |item| matches!(&item.data, ItemData::ToolResult { call_id, .. } if call_id == "held")
        ),
        "the paused action ended before the person resumed"
    );
    assert!(lab.runtime.pending_permissions().is_empty());
    // A program Haarness started, such as an agent's command, cannot end the person's pause.
    let inside = haarness_computer::INSIDE_FIELD;
    let refused = lab
        .computer
        .manage(&json!({"action": "resume", inside: true}))
        .await
        .unwrap_err();
    assert_eq!(refused, haarness_computer::INSIDE_RESUME_REFUSAL);
    assert!(lab.computer.is_paused());
    // Resume in a client: the strip hears it first, then the chat, and the agent goes on.
    let status = lab
        .computer
        .manage(&json!({"action": "resume"}))
        .await
        .unwrap();
    assert_eq!(status["paused"], false);
    assert_eq!(status["pausedChat"], Value::Null);
    let resumed = next_update(&mut events).await;
    assert_eq!(resumed.thread_id, Some(thread));
    assert_eq!(resumed.payload, json!({"paused": false}));
    let items = lab.finish(thread).await;
    let (output, error, _) = result(&items, "held");
    assert!(
        error && output.contains("running again") && output.contains("Call mac_snapshot"),
        "{output}"
    );
    let (output, error, _) = result(&items, "after");
    assert!(!error, "{output}");
    let ops = lab.ops();
    let resume = ops
        .iter()
        .position(|op| op == "resume")
        .expect("the strip is told");
    assert!(
        !ops[..resume].contains(&"snapshot".to_owned())
            && ops[resume..].contains(&"snapshot".to_owned()),
        "the agent looked only after the resume: {ops:?}"
    );
    // Said once: neither the turn's end nor a second Resume says it again.
    lab.computer
        .manage(&json!({"action": "resume"}))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(unread_updates(&mut events).is_empty());
    assert_eq!(
        ops.iter().filter(|op| *op == "resume").count(),
        lab.ops().iter().filter(|op| *op == "resume").count(),
        "nothing paused, nothing sent to the strip"
    );
}

#[tokio::test]
async fn stop_on_a_paused_strip_still_ends_the_turn_and_the_paused_line_goes() {
    let lab = Lab::new(Lab::with(json!({"defaultAct": "allow"})));
    let thread = lab.thread().await;
    let mut events = lab.events.subscribe(None).receiver;
    let turn = lab
        .start(
            thread,
            vec![
                vec![call(
                    "held",
                    "mac_type",
                    json!({"ref": "e2", "text": "STOP", "snapshot": false}),
                )],
                vec![call("never", "mac_snapshot", json!({}))],
            ],
        )
        .await;
    assert_eq!(
        next_update(&mut events).await.payload,
        json!({"paused": true})
    );
    let ended = next_update(&mut events).await;
    assert_eq!(ended.thread_id, Some(thread));
    assert_eq!(ended.payload, json!({"paused": false}));
    lab.finish(thread).await;
    let turn = lab.runtime.turn(turn.id).await.unwrap();
    assert_eq!(turn.status, TurnStatus::Interrupted, "{:?}", turn.error);
    assert!(!lab.computer.is_paused());
    assert!(
        !lab.ops().contains(&"snapshot".to_owned()),
        "{:?}",
        lab.ops()
    );
}

#[tokio::test]
async fn escape_ends_the_turn_using_the_screen_and_the_action_under_way() {
    let lab = Lab::new(Lab::with(json!({"defaultAct": "allow"})));
    let thread = lab.thread().await;
    let turn = lab
        .start(
            thread,
            vec![vec![call(
                "long",
                "mac_type",
                json!({"ref": "e2", "text": "ESCAPE"}),
            )]],
        )
        .await;
    lab.finish(thread).await;
    let turn = lab.runtime.turn(turn.id).await.unwrap();
    assert_eq!(turn.status, TurnStatus::Interrupted, "{:?}", turn.error);
}
