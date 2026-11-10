//! Computer use through the real runtime: a scripted model calls the mac_* tools, a stand-in helper
//! (`fake-helper.py`) answers for the Mac, and the tests read what the runtime asked, refused and ran.
#![cfg(target_os = "macos")]

use async_trait::async_trait;
use futures::stream;
use haarness_computer::{Computer, Config};
use haarness_core::{
    AgentRuntime, CoreResult, EventBus, HookCall, HookDecision, HookPort, ModelProvider,
    PermissionDecision, PermissionRule, ProviderEvent, ProviderRequest, ProviderStream,
    RuntimeConfig, StaticProviderRegistry, ToolRegistry,
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

/// The chat's model, and Auto's reviewer when a request carries the verdict schema: each review takes the next of
/// `verdicts` (allow when there is none left) and is kept in `reviews`.
#[derive(Default)]
struct Scripted {
    rounds: Mutex<VecDeque<Vec<ProviderEvent>>>,
    requests: Mutex<Vec<ProviderRequest>>,
    verdicts: Mutex<VecDeque<Value>>,
    reviews: Mutex<Vec<ProviderRequest>>,
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
        let review = request
            .output_schema
            .as_ref()
            .and_then(|schema| schema["description"].as_str())
            == Some(haarness_core::auto_review::VERDICT_SCHEMA_DESCRIPTION);
        if review {
            self.reviews.lock().unwrap().push(request);
            let verdict = self.verdicts.lock().unwrap().pop_front().unwrap_or_else(
                || json!({"risk": "low", "authorization": "high", "outcome": "allow"}),
            );
            return Ok(Box::pin(stream::iter(
                [
                    ProviderEvent::StructuredOutput(verdict),
                    ProviderEvent::Finished {
                        reason: "done".into(),
                    },
                ]
                .into_iter()
                .map(Ok),
            )));
        }
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
        Self::with_runtime(settings, |_| {}, None)
    }

    /// A lab whose runtime configuration `configure` changes, with `hooks` answering its requests.
    fn with_runtime(
        settings: Value,
        configure: impl FnOnce(&mut RuntimeConfig),
        hooks: Option<Arc<dyn HookPort>>,
    ) -> Self {
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
        let mut config = RuntimeConfig {
            approval_mode: ApprovalMode::Review,
            ..RuntimeConfig::default()
        };
        configure(&mut config);
        let runtime = AgentRuntime::with_config(
            Arc::new(SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new(vec![
                provider.clone() as Arc<dyn ModelProvider>
            ])),
            tools.clone(),
            events.clone(),
            root.join("workspace"),
            String::new(),
            config,
        );
        let runtime = Arc::new(match hooks {
            Some(hooks) => runtime.with_hooks(hooks),
            None => runtime,
        });
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
        json!({"enabled": true, "terms": {"version": haarness_computer::terms::VERSION, "acceptedAt": "2026-11-07T00:00:00Z"}})
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
/// not even where it would ask every time (an app set to ask). Never still
/// refuses. Auto with its reviewer off leaves it to a person.
#[tokio::test]
async fn yolo_answers_computer_use_and_auto_without_its_reviewer_leaves_it_to_a_person() {
    let lab = Lab::with_runtime(
        Lab::with(json!({"apps": {"com.apple.TextEdit": "ask", "com.apple.Notes": "deny"}})),
        |config| config.auto_review.enabled = false,
        None,
    );
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
    assert!(pending.review.is_none(), "{:?}", pending.review);
    lab.answer(&pending, PermissionReplyDecision::Reject).await;
    let items = lab.finish(auto).await;
    assert!(result(&items, "auto").1);
    assert!(lab.provider.reviews.lock().unwrap().is_empty());
}

/// The reviewer's prompts so far, as one text each.
fn review_prompts(lab: &Lab) -> Vec<String> {
    lab.provider
        .reviews
        .lock()
        .unwrap()
        .iter()
        .map(|request| {
            request
                .messages
                .iter()
                .filter_map(|message| match message {
                    haarness_core::ProviderMessage::Text { content, .. } => Some(content.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect()
}

/// In a chat in Auto, Auto's reviewer answers computer use as it answers other requests: what it allows runs with no
/// card, what it leaves to the person shows its note, and what it blocks never runs. It reads the app, the element and
/// the text, never the picture of the spot. An app the person set to ask every time goes to them, unreviewed.
#[tokio::test]
async fn auto_s_reviewer_answers_computer_use_and_apps_set_to_ask_stay_with_the_person() {
    let lab = Lab::new(Lab::with(json!({"apps": {"com.apple.Notes": "ask"}})));
    let thread = lab.thread().await;
    set_mode(&lab, thread, haarness_protocol::ApprovalModeSetting::Auto).await;

    // Allowed: no card, and the click runs.
    let items = lab
        .run(
            thread,
            vec![vec![call("allowed", "mac_click", json!({"ref": "e1"}))]],
        )
        .await;
    assert!(
        !result(&items, "allowed").1,
        "{}",
        result(&items, "allowed").0
    );
    assert!(lab.ops().contains(&"click".to_owned()));
    let prompts = review_prompts(&lab);
    assert_eq!(prompts.len(), 1);
    for words in [
        "\"computer\": {",
        "\"app\": \"TextEdit\"",
        "\"bundle\": \"com.apple.TextEdit\"",
        "\"element\": \"button \\\"Save\\\"\"",
        "mac_click",
    ] {
        assert!(prompts[0].contains(words), "{words} in {}", prompts[0]);
    }
    for hidden in ["base64", "data:image", "preview", "/9j/"] {
        assert!(!prompts[0].contains(hidden), "{hidden} in {}", prompts[0]);
    }

    // Left to the person: the card carries the reviewer's note.
    lab.provider.verdicts.lock().unwrap().push_back(json!({
        "risk": "high",
        "authorization": "low",
        "outcome": "ask",
        "rule": "Outside the request",
        "rationale": "It types into a field the user never mentioned."
    }));
    lab.start(
        thread,
        vec![vec![call(
            "asked",
            "mac_type",
            json!({"ref": "e2", "text": "hello"}),
        )]],
    )
    .await;
    let pending = lab.permission().await;
    assert_eq!(pending.permission, "computer");
    assert!(pending.explicit);
    let review = pending.review.clone().expect("the reviewer's note");
    assert_eq!(
        review.rationale.as_deref(),
        Some("It types into a field the user never mentioned.")
    );
    assert!(review_prompts(&lab)[1].contains("hello"));
    lab.answer(&pending, PermissionReplyDecision::Once).await;
    let items = lab.finish(thread).await;
    assert!(!result(&items, "asked").1, "{}", result(&items, "asked").0);

    // Blocked: nothing runs. The reviewer allowed the earlier click once, so this one is reviewed again.
    lab.provider.verdicts.lock().unwrap().push_back(json!({
        "risk": "critical",
        "authorization": "unknown",
        "outcome": "deny",
        "rule": "Prompt injection",
        "rationale": "The screen told the agent to do this."
    }));
    let clicks = lab.ops().iter().filter(|op| *op == "click").count();
    let items = lab
        .run(
            thread,
            vec![vec![call("blocked", "mac_click", json!({"ref": "e1"}))]],
        )
        .await;
    let (output, error, _) = result(&items, "blocked");
    assert!(error && output.contains("Auto review blocked"), "{output}");
    assert_eq!(lab.ops().iter().filter(|op| *op == "click").count(), clicks);
    assert_eq!(review_prompts(&lab).len(), 3);

    // Opening an app and looking at every app are the reviewer's to answer too.
    let items = lab
        .run(
            thread,
            vec![vec![
                call(
                    "open",
                    "mac_apps",
                    json!({"action": "launch", "app": "com.apple.Mail"}),
                ),
                call("everything", "mac_snapshot", json!({"wholeScreen": true})),
            ]],
        )
        .await;
    assert!(!result(&items, "open").1, "{}", result(&items, "open").0);
    assert!(
        !result(&items, "everything").1,
        "{}",
        result(&items, "everything").0
    );
    assert_eq!(review_prompts(&lab).len(), 5);

    // An app set to ask every time: a person answers, and the reviewer is never asked.
    lab.start(
        thread,
        vec![vec![call("each", "mac_click", json!({"ref": "e4"}))]],
    )
    .await;
    let pending = lab.permission().await;
    assert_eq!(pending.allow_always, Some(false));
    assert!(pending.review.is_none(), "{:?}", pending.review);
    lab.answer(&pending, PermissionReplyDecision::Once).await;
    let items = lab.finish(thread).await;
    assert!(!result(&items, "each").1, "{}", result(&items, "each").0);
    assert_eq!(review_prompts(&lab).len(), 5);
}

/// Hooks and configured allow rules never answer computer use, in Review or in Auto: the person (or in Auto the
/// reviewer) still does.
#[tokio::test]
async fn hooks_and_configured_allow_rules_never_answer_computer_use() {
    let lab = Lab::with_runtime(
        Lab::on(),
        |config| {
            config.base_rules.push(PermissionRule {
                pattern: "computer:*".into(),
                decision: PermissionDecision::Allow,
            });
        },
        Some(Arc::new(AllowsEverything)),
    );
    let thread = lab.thread().await;
    lab.start(
        thread,
        vec![vec![call("review", "mac_click", json!({"ref": "e1"}))]],
    )
    .await;
    let pending = lab.permission().await;
    assert_eq!(pending.permission, "computer");
    lab.answer(&pending, PermissionReplyDecision::Once).await;
    lab.finish(thread).await;
    assert!(review_prompts(&lab).is_empty());

    let auto = lab.thread().await;
    set_mode(&lab, auto, haarness_protocol::ApprovalModeSetting::Auto).await;
    lab.provider.verdicts.lock().unwrap().push_back(json!({
        "risk": "medium",
        "authorization": "unknown",
        "outcome": "ask",
        "rule": "Outside the request",
        "rationale": "Nothing the user said asks for this."
    }));
    lab.start(
        auto,
        vec![vec![call("auto", "mac_click", json!({"ref": "e1"}))]],
    )
    .await;
    let pending = lab.permission().await;
    assert!(
        pending.review.is_some(),
        "the reviewer, not the hook, answered"
    );
    lab.answer(&pending, PermissionReplyDecision::Once).await;
    lab.finish(auto).await;
    assert_eq!(review_prompts(&lab).len(), 1);
}

/// Allows every tool call and every request it is shown.
struct AllowsEverything;

#[async_trait]
impl HookPort for AllowsEverything {
    async fn pre_tool_use(&self, _: &HookCall, _: &str, _: &Value) -> HookDecision {
        HookDecision {
            permission: Some(PermissionDecision::Allow),
            ..HookDecision::default()
        }
    }
    async fn post_tool_use(
        &self,
        _: &HookCall,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn user_prompt_submit(&self, _: &HookCall, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: &HookCall,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        HookDecision {
            permission: Some(PermissionDecision::Allow),
            ..HookDecision::default()
        }
    }
    async fn stop(&self, _: &HookCall) {}
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
                    json!({"from": {"ref": "e1"}, "to": {"ref": "e3"}}),
                ),
                call(
                    "drop",
                    "mac_drag",
                    json!({"from": {"ref": "e1"}, "to": {"ref": "e6"}}),
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
        ("haarness", "belongs to Haarness"),
        ("password", "Password fields are refused"),
        ("terminal", "never type"),
        ("logout", "is blocked"),
        ("dock", "Drops onto the Dock are refused"),
        ("drop", "never type"),
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

/// Browsers, System Settings, code editors and terminals have no fixed lock beyond typing into a terminal: they
/// follow the app rules like any other app, quitting included.
#[tokio::test]
async fn browsers_settings_editors_and_terminals_follow_the_app_rules() {
    let lab = Lab::new(Lab::with(
        json!({"defaultAct": "allow", "apps": {"com.apple.Safari": "ask"}}),
    ));
    let thread = lab.thread().await;
    let items = lab
        .run(
            thread,
            vec![vec![
                call("settings", "mac_click", json!({"ref": "e13"})),
                call(
                    "editor",
                    "mac_type",
                    json!({"ref": "e10", "text": "fn main() {}"}),
                ),
                call("terminal", "mac_click", json!({"ref": "e6"})),
                call("tab", "mac_click", json!({"ref": "e5"})),
                call(
                    "quit",
                    "mac_apps",
                    json!({"action": "quit", "app": "com.apple.Terminal"}),
                ),
            ]],
        )
        .await;
    for id in ["settings", "editor", "terminal", "tab", "quit"] {
        let (output, error, _) = result(&items, id);
        assert!(!error, "{id}: {output}");
    }
    let ops = lab.ops();
    assert_eq!(ops.iter().filter(|op| *op == "click").count(), 3, "{ops:?}");
    assert!(
        ops.contains(&"type".to_owned()) && ops.contains(&"quit".to_owned()),
        "{ops:?}"
    );
    // A browser set to ask every time asks, as any app with that rule does; the answer reaches its address field.
    lab.start(
        thread,
        vec![vec![call(
            "browser",
            "mac_type",
            json!({"ref": "e11", "text": "haarness.ai"}),
        )]],
    )
    .await;
    let pending = lab.permission().await;
    assert_eq!(pending.detail["__mac"]["bundle"], "com.apple.Safari");
    assert_eq!(pending.detail["__mac"]["everyTime"], true);
    assert!(pending.detail["__mac"].get("console").is_none());
    lab.answer(&pending, PermissionReplyDecision::Once).await;
    let items = lab.finish(thread).await;
    assert!(
        !result(&items, "browser").1,
        "{}",
        result(&items, "browser").0
    );
}

/// While an approval waits for a person, a terminal or an editor may be showing it in Haarness's terminal UI, so a
/// click, typing or a key there is turned away until it is answered.
#[tokio::test]
async fn input_into_a_terminal_or_editor_waits_while_an_approval_is_open() {
    let lab = Lab::with_runtime(
        Lab::with(json!({"apps": {
            "com.microsoft.VSCode": "allow",
            "com.apple.Terminal": "allow",
            "com.apple.Notes": "allow"
        }})),
        |config| {
            config.base_rules.push(PermissionRule {
                pattern: "bash:*".into(),
                decision: PermissionDecision::Ask,
            });
        },
        None,
    );
    // Another chat's command waits for the person.
    let asking = lab.thread().await;
    lab.start(
        asking,
        vec![vec![call(
            "asks",
            "bash",
            json!({"command": "echo waiting"}),
        )]],
    )
    .await;
    let pending = lab.permission().await;
    let other = lab.thread().await;
    lab.start(
        other,
        vec![vec![
            call("editor", "mac_type", json!({"ref": "e10", "text": "y"})),
            call("terminal", "mac_click", json!({"ref": "e6"})),
            call("notes", "mac_click", json!({"ref": "e4"})),
        ]],
    )
    .await;
    let items = tokio::time::timeout(Duration::from_secs(15), async {
        while lab.runtime.active_turn(other).await.unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        lab.runtime.items(other, None).await.unwrap()
    })
    .await
    .expect("the other chat's turn finishes");
    for id in ["editor", "terminal"] {
        let (output, error, _) = result(&items, id);
        assert!(
            error && output.contains("still has to answer an approval"),
            "{id}: {output}"
        );
    }
    assert!(!result(&items, "notes").1, "{}", result(&items, "notes").0);
    lab.answer(&pending, PermissionReplyDecision::Once).await;
    lab.finish(asking).await;
    let items = lab
        .run(
            other,
            vec![vec![call(
                "later",
                "mac_type",
                json!({"ref": "e10", "text": "y"}),
            )]],
        )
        .await;
    assert!(!result(&items, "later").1, "{}", result(&items, "later").0);
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

/// Starts a turn whose typing the stand-in pauses and whose helper then quits, as one an agent's `pkill` or a crash
/// ends; returns once the helper has gone.
async fn pause_then_lose_the_helper(
    lab: &Lab,
    thread: ThreadId,
    events: &mut tokio::sync::broadcast::Receiver<haarness_protocol::EventEnvelope>,
) {
    lab.start(
        thread,
        vec![
            vec![call(
                "held",
                "mac_type",
                json!({"ref": "e2", "text": "CRASH", "snapshot": false}),
            )],
            vec![call("after", "mac_snapshot", json!({}))],
        ],
    )
    .await;
    assert_eq!(next_update(events).await.payload, json!({"paused": true}));
    tokio::time::sleep(Duration::from_millis(1500)).await;
}

#[tokio::test]
async fn a_helper_that_quits_during_a_pause_leaves_the_agent_paused_until_the_person_resumes() {
    let lab = Lab::new(Lab::with(json!({"defaultAct": "allow"})));
    let thread = lab.thread().await;
    let mut events = lab.events.subscribe(None).receiver;
    pause_then_lose_the_helper(&lab, thread, &mut events).await;
    assert!(lab.computer.is_paused(), "the pause outlives the helper");
    assert_eq!(lab.computer.paused_chat(), Some(thread));
    assert!(
        unread_updates(&mut events).is_empty(),
        "nobody is told it ended"
    );
    assert!(lab.runtime.active_turn(thread).await.unwrap().is_some());
    let items = lab.runtime.items(thread, None).await.unwrap();
    assert!(
        !items.iter().any(
            |item| matches!(&item.data, ItemData::ToolResult { call_id, .. } if call_id == "held")
        ),
        "the agent went on without the person"
    );
    // The person's Resume in the chat lets it go on, with a new helper.
    let answer = lab
        .computer
        .manage(&json!({"action": "resume"}))
        .await
        .unwrap();
    assert_eq!(answer["paused"], false);
    let items = lab.finish(thread).await;
    let (output, error, _) = result(&items, "held");
    assert!(error && output.contains("running again"), "{output}");
    let (output, error, _) = result(&items, "after");
    assert!(!error, "{output}");
    assert_eq!(
        lab.ops().iter().filter(|op| *op == "hello").count(),
        2,
        "{:?}",
        lab.ops()
    );
}

#[tokio::test]
async fn switching_off_frees_an_agent_paused_without_a_helper() {
    let lab = Lab::new(Lab::with(json!({"defaultAct": "allow"})));
    let thread = lab.thread().await;
    let mut events = lab.events.subscribe(None).receiver;
    pause_then_lose_the_helper(&lab, thread, &mut events).await;
    assert!(lab.computer.is_paused());
    lab.computer.enable(false).unwrap();
    let items = lab.finish(thread).await;
    let (output, error, _) = result(&items, "held");
    assert!(error && output.contains("switched off"), "{output}");
    assert!(!lab.computer.is_paused());
}

#[tokio::test]
async fn stop_on_a_strip_paused_while_an_approval_waits_ends_the_turn() {
    let lab = Lab::new(Lab::on());
    let thread = lab.thread().await;
    let mut events = lab.events.subscribe(None).receiver;
    let turn = lab
        .start(
            thread,
            vec![
                vec![call("click", "mac_click", json!({"x": 13, "y": 13}))],
                vec![call("never", "mac_snapshot", json!({}))],
            ],
        )
        .await;
    // The card is open when the person pauses: the chat hears it at once, then Stop ends the turn.
    lab.permission().await;
    let paused = next_update(&mut events).await;
    assert_eq!(paused.thread_id, Some(thread));
    assert_eq!(paused.payload, json!({"paused": true}));
    assert_eq!(
        next_update(&mut events).await.payload,
        json!({"paused": false})
    );
    lab.finish(thread).await;
    let turn = lab.runtime.turn(turn.id).await.unwrap();
    assert_eq!(turn.status, TurnStatus::Interrupted, "{:?}", turn.error);
    assert!(!lab.computer.is_paused());
    let ops = lab.ops();
    assert!(
        !ops.contains(&"click".to_owned()) && !ops.contains(&"snapshot".to_owned()),
        "{ops:?}"
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

/// A helper that works behind the person's window says so, and the result passes it on: that the action went in behind,
/// whether the app showed the change, what it opened and closed, and why it came to the front when it had to. The
/// fresh look is at the app acted on.
#[tokio::test]
async fn an_action_behind_the_person_s_window_says_so_and_looks_at_that_app() {
    let lab = Lab::new(Lab::with(json!({"defaultAct": "allow"})));
    let thread = lab.thread().await;
    let items = lab
        .run(
            thread,
            vec![vec![
                call("click", "mac_click", json!({"ref": "e12"})),
                call(
                    "scroll",
                    "mac_scroll",
                    json!({"ref": "e12", "direction": "down"}),
                ),
                call(
                    "drag",
                    "mac_drag",
                    json!({"from": {"ref": "e12"}, "to": {"ref": "e4"}, "snapshot": false}),
                ),
                call(
                    "hover",
                    "mac_hover",
                    json!({"ref": "e12", "snapshot": false}),
                ),
                call(
                    "open",
                    "mac_apps",
                    json!({"action": "launch", "app": "com.apple.Mail"}),
                ),
            ]],
        )
        .await;
    let (click, error, metadata) = result(&items, "click");
    assert!(!error, "{click}");
    for words in [
        "Clicked button \"Share\" in Notes. This happened behind the person's window",
        "Notes showed the change.",
        "Opened: sheet \"Share Note\" of Notes; menu of Finder.",
        "Closed: popover of Notes.",
        "Notes came forward by itself, so the person's own app was put back in front.",
        "Notes is behind the person's window.",
    ] {
        assert!(click.contains(words), "{words} in {click}");
    }
    assert_eq!(metadata["app"], "Notes", "the look is at the app acted on");
    let (scroll, _, _) = result(&items, "scroll");
    assert!(
        scroll.contains("Notes showed no change") && scroll.contains("mac_apps activate"),
        "{scroll}"
    );
    let (drag, _, _) = result(&items, "drag");
    assert!(
        drag.contains("Notes came to the front for it, since drags use the real pointer.")
            && drag.contains("No snapshot was taken"),
        "{drag}"
    );
    let (hover, _, _) = result(&items, "hover");
    assert!(
        hover.contains("since hovering uses the real pointer"),
        "{hover}"
    );
    let (open, _, _) = result(&items, "open");
    assert!(
        open.starts_with("Opened Mail. This happened behind the person's window")
            && open.contains("Opened: window \"Inbox\" of Mail.")
            && open.contains("they are all minimized"),
        "{open}"
    );
    // A press that changed nothing the helper could read back claims nothing either way; an app that could not open
    // behind says why it came forward, and a hidden one says so.
    let items = lab
        .run(
            thread,
            vec![vec![
                call(
                    "pressed",
                    "mac_click",
                    json!({"ref": "e4", "snapshot": false}),
                ),
                call(
                    "music",
                    "mac_apps",
                    json!({"action": "launch", "app": "com.apple.Music"}),
                ),
            ]],
        )
        .await;
    let (pressed, _, _) = result(&items, "pressed");
    assert!(!pressed.contains("showed"), "{pressed}");
    let (music, _, _) = result(&items, "music");
    assert!(
        music.contains("Music came to the front for it, since it could not take this from behind the person's window.")
            && music.contains("the app is hidden"),
        "{music}"
    );
    let looks: Vec<Value> = lab
        .log()
        .into_iter()
        .filter(|request| request["op"] == "snapshot")
        .collect();
    assert_eq!(looks[0]["bundle"], "com.apple.Notes");
    assert_eq!(looks[0]["pid"], 4, "the very copy acted on");
    assert_eq!(looks[1]["bundle"], "com.apple.Notes");
    // An action whose reply says nothing of the kind reads as before, and its look is at the app in front.
    let items = lab
        .run(
            thread,
            vec![vec![call("plain", "mac_click", json!({"ref": "e1"}))]],
        )
        .await;
    let (plain, _, _) = result(&items, "plain");
    assert!(
        plain.starts_with("Clicked button \"Save\" in TextEdit.\n\nImage size"),
        "{plain}"
    );
    let last = lab
        .log()
        .into_iter()
        .rfind(|request| request["op"] == "snapshot")
        .unwrap();
    assert!(last.get("bundle").is_none(), "{last}");
}

/// A look can keep only the elements that match a text, and show an area up close; both reach the helper, and the
/// result says what the model is looking at.
#[tokio::test]
async fn a_look_can_filter_its_elements_and_show_an_area_up_close() {
    let lab = Lab::new(Lab::on());
    let thread = lab.thread().await;
    let zoom = json!({"x": 70, "y": 5, "width": 40, "height": 20});
    let items = lab
        .run(
            thread,
            vec![vec![call(
                "look",
                "mac_snapshot",
                json!({"find": "Save", "zoom": zoom}),
            )]],
        )
        .await;
    let (output, error, _) = result(&items, "look");
    assert!(!error, "{output}");
    assert!(
        // The helper trimmed the area to the screenshot: the result says what it showed.
        output.starts_with("A close-up at full resolution of the area at x=70, y=5, 30x20 of the latest screenshot, shown 3.3 times larger.")
            && output.contains("Showing only elements matching \"Save\""),
        "{output}"
    );
    let look = lab
        .log()
        .into_iter()
        .find(|request| request["op"] == "snapshot")
        .unwrap();
    assert_eq!(look["find"], "Save");
    assert_eq!(look["zoom"], zoom);
    // A helper that shows no close-up (an older one, here a look at Notes) is not said to.
    let items = lab
        .run(
            thread,
            vec![vec![call(
                "notes",
                "mac_snapshot",
                json!({"app": "com.apple.Notes", "zoom": zoom}),
            )]],
        )
        .await;
    let (output, _, _) = result(&items, "notes");
    assert!(output.starts_with("Image size 100x50 pixels"), "{output}");
    // A zoom must name a whole area.
    let tool = lab.tools.get("mac_snapshot").unwrap();
    assert!(
        tool.validate_arguments(&json!({"zoom": {"x": 1, "y": 1}}))
            .is_err()
    );
}

/// Terminals, script runners and code editors stay with the person in Auto: a click on Paste or Run there runs code
/// the reviewer never sees. So does System Settings when the helper cannot name the open pane, and a drag that touches
/// an app set to ask every time.
#[tokio::test]
async fn auto_s_reviewer_never_answers_for_terminals_editors_unnamed_settings_or_an_app_set_to_ask()
{
    let lab = Lab::new(Lab::with(json!({"apps": {"com.apple.Notes": "ask"}})));
    let thread = lab.thread().await;
    set_mode(&lab, thread, haarness_protocol::ApprovalModeSetting::Auto).await;
    for (id, name, arguments) in [
        ("button", "mac_click", json!({"ref": "e5"})),
        (
            "open",
            "mac_apps",
            json!({"action": "launch", "app": "com.apple.Terminal"}),
        ),
        (
            "drag",
            "mac_drag",
            json!({"from": {"ref": "e1"}, "to": {"ref": "e4"}}),
        ),
        ("editor", "mac_click", json!({"ref": "e10"})),
        ("settings", "mac_click", json!({"ref": "e13"})),
    ] {
        lab.start(thread, vec![vec![call(id, name, arguments)]])
            .await;
        let pending = lab.permission().await;
        assert_eq!(pending.permission, "computer", "{id}");
        assert!(pending.review.is_none(), "{id}: {:?}", pending.review);
        if id == "drag" {
            assert_eq!(pending.allow_always, Some(false));
        }
        lab.answer(&pending, PermissionReplyDecision::Once).await;
        let items = lab.finish(thread).await;
        assert!(!result(&items, id).1, "{id}: {}", result(&items, id).0);
    }
    assert!(
        review_prompts(&lab).is_empty(),
        "the reviewer was never asked"
    );
    // A pane the helper can name, and that is not Privacy & Security, is the reviewer's to answer.
    let items = lab
        .run(
            thread,
            vec![vec![call("displays", "mac_click", json!({"ref": "e17"}))]],
        )
        .await;
    assert!(
        !result(&items, "displays").1,
        "{}",
        result(&items, "displays").0
    );
    assert_eq!(review_prompts(&lab).len(), 1);
}

/// Paste in a terminal runs the clipboard as a command: a click on it is refused before anyone is asked, and every
/// click there tells the helper to refuse Paste if that is where it lands. The Privacy & Security pane's own process is
/// look-only too.
#[tokio::test]
async fn paste_in_a_terminal_and_privacy_settings_are_refused_before_anyone_is_asked() {
    let lab = Lab::new(Lab::with(json!({"defaultAct": "allow"})));
    let thread = lab.thread().await;
    let items = lab
        .run(
            thread,
            vec![vec![
                call("paste", "mac_click", json!({"ref": "e16"})),
                call("unmarked", "mac_click", json!({"ref": "e19"})),
                call("privacy", "mac_click", json!({"ref": "e18"})),
                call("tab", "mac_click", json!({"ref": "e5"})),
                call("save", "mac_click", json!({"ref": "e1"})),
            ]],
        )
        .await;
    let (paste, error, _) = result(&items, "paste");
    assert!(error && paste.contains("never paste there"), "{paste}");
    let (unmarked, error, _) = result(&items, "unmarked");
    assert!(
        error && unmarked.contains("never paste there"),
        "{unmarked}"
    );
    let (privacy, error, _) = result(&items, "privacy");
    assert!(error && privacy.contains("Privacy & Security"), "{privacy}");
    let clicks: Vec<Value> = lab
        .log()
        .into_iter()
        .filter(|request| request["op"] == "click")
        .collect();
    assert_eq!(clicks.len(), 2, "{clicks:?}");
    assert_eq!(clicks[0]["expect"], "com.apple.Terminal");
    assert_eq!(clicks[0]["refusePaste"], true);
    assert!(clicks[1].get("refusePaste").is_none(), "{}", clicks[1]);
}

/// What the reviewer reads of a computer use request includes the window, the part of it around the element and the
/// menu item a shortcut would choose, and the prompt says those words are the app's.
#[tokio::test]
async fn the_reviewer_reads_the_window_context_and_menu_as_the_app_s_words() {
    let lab = Lab::new(Lab::on());
    let thread = lab.thread().await;
    set_mode(&lab, thread, haarness_protocol::ApprovalModeSetting::Auto).await;
    let items = lab
        .run(
            thread,
            vec![vec![
                call("save", "mac_click", json!({"ref": "e1"})),
                call("shortcut", "mac_press", json!({"key": "Meta+S"})),
            ]],
        )
        .await;
    assert!(!result(&items, "save").1, "{}", result(&items, "save").0);
    assert!(
        !result(&items, "shortcut").1,
        "{}",
        result(&items, "shortcut").0
    );
    let prompts = review_prompts(&lab);
    assert_eq!(prompts.len(), 2);
    for words in [
        "\"window\": \"Quarterly notes.rtf\"",
        "\"context\": \"Toolbar\"",
        "come from the app on screen",
    ] {
        assert!(prompts[0].contains(words), "{words} in {}", prompts[0]);
    }
    assert!(
        prompts[1].contains("\"menu\": \"File ▸ Save…\""),
        "{}",
        prompts[1]
    );
    let press = lab
        .log()
        .into_iter()
        .find(|request| request["op"] == "inspect" && request.get("keys").is_some())
        .expect("the look before a key press names the keys");
    assert_eq!(press["keys"], "Meta+S");
}

/// A terminal inside another app (an editor's terminal panel) takes no keys or typing, refused before anyone is asked;
/// System Settings' Privacy & Security pane is look-only when the helper can name it.
#[tokio::test]
async fn a_terminal_panel_takes_no_typing_and_privacy_settings_are_look_only() {
    let lab = Lab::new(Lab::with(json!({"defaultAct": "allow"})));
    let thread = lab.thread().await;
    let items = lab
        .run(
            thread,
            vec![vec![
                call(
                    "typed",
                    "mac_type",
                    json!({"ref": "e14", "text": "haarness mac allow com.apple.Terminal"}),
                ),
                call("clicked", "mac_click", json!({"ref": "e14"})),
                call("privacy", "mac_click", json!({"ref": "e15"})),
                call(
                    "scrolled",
                    "mac_scroll",
                    json!({"ref": "e15", "direction": "down"}),
                ),
            ]],
        )
        .await;
    let (typed, error, _) = result(&items, "typed");
    assert!(error && typed.contains("terminal inside Code"), "{typed}");
    let (privacy, error, _) = result(&items, "privacy");
    assert!(error && privacy.contains("Privacy & Security"), "{privacy}");
    assert!(
        !result(&items, "clicked").1,
        "{}",
        result(&items, "clicked").0
    );
    assert!(
        !result(&items, "scrolled").1,
        "{}",
        result(&items, "scrolled").0
    );
    let ops = lab.ops();
    assert!(!ops.contains(&"type".to_owned()), "{ops:?}");
    assert_eq!(ops.iter().filter(|op| *op == "click").count(), 1, "{ops:?}");
}

/// A workflow's mac_* call in a chat in Auto goes to the reviewer while it is on, and to a person while it is off.
#[tokio::test]
async fn a_workflow_s_computer_use_in_auto_follows_the_reviewer_switch() {
    let lab = Lab::new(Lab::on());
    let thread = lab.thread().await;
    set_mode(&lab, thread, haarness_protocol::ApprovalModeSetting::Auto).await;
    let done = tokio::time::timeout(
        Duration::from_secs(15),
        lab.runtime.run_workflow_tool(
            thread,
            "mac_click",
            json!({"ref": "e1"}),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the reviewer answers: no card waits for a person")
    .unwrap_or_else(|error| panic!("{error}"));
    assert!(!done.is_error, "{}", done.output.text);
    assert_eq!(review_prompts(&lab).len(), 1);
    assert!(lab.runtime.pending_permissions().is_empty());

    let off = Lab::with_runtime(Lab::on(), |config| config.auto_review.enabled = false, None);
    let thread = off.thread().await;
    set_mode(&off, thread, haarness_protocol::ApprovalModeSetting::Auto).await;
    let runtime = off.runtime.clone();
    let call = tokio::spawn(async move {
        runtime
            .run_workflow_tool(
                thread,
                "mac_click",
                json!({"ref": "e1"}),
                CancellationToken::new(),
            )
            .await
            .map(|done| done.is_error)
    });
    let pending = off.permission().await;
    assert_eq!(pending.permission, "computer");
    assert!(pending.explicit);
    off.answer(&pending, PermissionReplyDecision::Once).await;
    assert_eq!(call.await.unwrap().ok(), Some(false));
    assert!(off.provider.reviews.lock().unwrap().is_empty());
}

/// A scheduled run starts at its time without anyone in front of the screen, so it may act on the Mac only in a chat the
/// person set to Yolo: elsewhere a request for an app the person has not allowed fails at once, and the reviewer is not
/// asked.
#[tokio::test]
async fn a_schedule_s_run_uses_the_screen_only_in_a_chat_set_to_yolo() {
    let lab = Lab::new(Lab::on());
    for (mode, allowed) in [
        (haarness_protocol::ApprovalModeSetting::Auto, false),
        (haarness_protocol::ApprovalModeSetting::Yolo, true),
    ] {
        let thread = lab
            .runtime
            .create_thread(CreateThreadRequest {
                title: Some("Nightly".into()),
                provider: Some("fixture".into()),
                model: Some("fixture".into()),
                schedule: Some(haarness_protocol::ScheduleOrigin {
                    id: uuid::Uuid::now_v7(),
                    name: "Nightly".into(),
                }),
                ..Default::default()
            })
            .await
            .unwrap()
            .id;
        set_mode(&lab, thread, mode).await;
        lab.provider.rounds.lock().unwrap().push_back(vec![call(
            "nightly",
            "mac_click",
            json!({"ref": "e1"}),
        )]);
        lab.runtime
            .start_turn(
                thread,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text: "Save the report".into(),
                    auto_approve: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let items = lab.finish(thread).await;
        let (output, error, _) = result(&items, "nightly");
        assert_eq!(!error, allowed, "{output}");
        if !allowed {
            assert!(output.contains("this run cannot ask"), "{output}");
        }
    }
    assert_eq!(lab.ops().iter().filter(|op| *op == "click").count(), 1);
    assert!(review_prompts(&lab).is_empty());
}

/// A published chat never gets this Mac's screen: not in its turns, and not through a workflow's or a client's call,
/// which Auto's reviewer could otherwise answer on a viewer's word.
#[tokio::test]
async fn a_published_chat_gets_no_computer_use_from_turns_or_workflows() {
    let lab = Lab::new(Lab::with(json!({"defaultAct": "allow"})));
    let thread = lab.thread().await;
    set_mode(&lab, thread, haarness_protocol::ApprovalModeSetting::Auto).await;
    haarness_core::shared_chats::mark(thread);
    let items = lab
        .run(
            thread,
            vec![vec![call("turn", "mac_click", json!({"ref": "e1"}))]],
        )
        .await;
    assert!(result(&items, "turn").1, "{}", result(&items, "turn").0);
    let Err(error) = lab
        .runtime
        .run_workflow_tool(
            thread,
            "mac_click",
            json!({"ref": "e1"}),
            CancellationToken::new(),
        )
        .await
    else {
        panic!("a published chat's workflow used the screen");
    };
    assert!(error.to_string().contains("published chat"), "{error}");
    assert!(!lab.ops().contains(&"click".to_owned()), "{:?}", lab.ops());
    assert!(review_prompts(&lab).is_empty());
}
