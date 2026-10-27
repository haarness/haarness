//! Prompt files from the request's point of view: what a provider receives
//! when they change the built-in prompts, and that it receives exactly what
//! it always did without them.

use async_trait::async_trait;
use futures::stream;
use haarness_core::{
    AgentRuntime, CoreResult, EventBus, ModelProvider, PromptFile, PromptMode, PromptName, Prompts,
    ProviderEvent, ProviderMessage, ProviderRequest, ProviderStream, RuntimeConfig,
    StaticProviderRegistry, Tool, ToolContext, ToolDefinition, ToolOutput, ToolRegistry,
};
use haarness_protocol::{
    BtwRequest, CreateThreadRequest, ProviderInfo, RequestId, StartTurnRequest, ThreadId,
    TurnStatus,
};
use haarness_store::SqliteStore;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Recorder {
    requests: Mutex<Vec<ProviderRequest>>,
    /// Call `preview` once per turn, so a round limit of one is reached.
    call_tool: bool,
}

#[async_trait]
impl ModelProvider for Recorder {
    fn id(&self) -> &str {
        "recorder"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id().into(),
            name: "Recorder".into(),
            authenticated: true,
            models: Vec::new(),
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let call = self.call_tool
            && !request.tools.is_empty()
            && !matches!(
                request.messages.last(),
                Some(ProviderMessage::ToolResult { .. })
            );
        self.requests.lock().unwrap().push(request);
        let event = if call {
            ProviderEvent::ToolCall {
                call_id: uuid::Uuid::now_v7().to_string(),
                name: "preview".into(),
                arguments: json!({}),
                provider_metadata: Value::Null,
            }
        } else {
            ProviderEvent::TextDelta("Short title".into())
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(event),
            Ok(ProviderEvent::Finished {
                reason: "completed".into(),
            }),
        ])))
    }
}

/// A `preview` tool, so the preview section is sent.
struct Preview;
#[async_trait]
impl Tool for Preview {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "preview".into(),
            description: "Fixture preview".into(),
            input_schema: json!({"type":"object"}),
        }
    }
    fn side_effectful(&self) -> bool {
        false
    }
    async fn execute(&self, _: Value, _: ToolContext) -> CoreResult<ToolOutput> {
        Ok(ToolOutput {
            text: String::new(),
            metadata: Value::Null,
        })
    }
}
struct Tools;
impl ToolRegistry for Tools {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        (name == "preview").then(|| Arc::new(Preview) as Arc<dyn Tool>)
    }
    fn list(&self) -> Vec<ToolDefinition> {
        vec![Preview.definition()]
    }
}

fn file(name: PromptName, mode: PromptMode, body: &str) -> PromptFile {
    PromptFile {
        name,
        mode,
        written_from: None,
        body: body.into(),
        path: PathBuf::from(format!("/team/prompts/{}.md", name.as_str())),
        source: "system".into(),
    }
}

fn runtime(provider: Arc<Recorder>, prompts: Prompts, max_rounds: Option<u32>) -> AgentRuntime {
    runtime_in(provider, prompts, max_rounds, PathBuf::from("."))
}

fn runtime_in(
    provider: Arc<Recorder>,
    prompts: Prompts,
    max_rounds: Option<u32>,
    workspace: PathBuf,
) -> AgentRuntime {
    AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            provider as Arc<dyn ModelProvider>,
        ])),
        Arc::new(Tools),
        EventBus::new(256),
        workspace,
        "Project instructions marker".into(),
        RuntimeConfig {
            prompts,
            max_rounds,
            ..Default::default()
        },
    )
}

async fn turn(runtime: &AgentRuntime, thread: ThreadId, agent: Option<&str>) {
    let turn = runtime
        .start_turn(
            thread,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: "Look at the project.".into(),
                attachments: vec![],
                resources: Vec::new(),
                synthetic: false,
                auto_approve: false,
                agent: agent.map(str::to_owned),
                provider: None,
                model: None,
                reasoning_effort: None,
                // The client shows previews, so the preview tool is on.
                tools: Some([("preview".to_owned(), true)].into()),
                command: None,
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let turn = runtime.turn(turn.id).await.unwrap();
            if !matches!(turn.status, TurnStatus::Running | TurnStatus::Queued) {
                assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

fn main_requests(provider: &Recorder) -> Vec<ProviderRequest> {
    provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| {
            !request.tools.is_empty() || request.system.contains("Project instructions marker")
        })
        .cloned()
        .collect()
}

#[tokio::test]
async fn prompt_files_reach_every_request_that_sends_their_prompt() {
    let provider = Arc::new(Recorder {
        call_tool: true,
        ..Default::default()
    });
    let prompts = Prompts::new(vec![
        file(
            PromptName::System,
            PromptMode::Replace,
            "You are Acme's coding agent.",
        ),
        file(PromptName::Plan, PromptMode::Append, "Number every step."),
        file(
            PromptName::Preview,
            PromptMode::Replace,
            "Preview with care.",
        ),
        file(
            PromptName::Title,
            PromptMode::Replace,
            "Name the chat in three words.",
        ),
        file(
            PromptName::Compaction,
            PromptMode::Prepend,
            "Keep the Acme ticket ids.",
        ),
        file(
            PromptName::MaxSteps,
            PromptMode::Replace,
            "Stop and report.",
        ),
    ]);
    let runtime = runtime(provider.clone(), prompts, Some(1));
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("recorder".into()),
            model: Some("fixture".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    turn(&runtime, thread.id, None).await;
    turn(&runtime, thread.id, None).await;
    let main = main_requests(&provider);
    let first = &main[0];
    // The replaced instructions, the safety baseline, then the environment
    // block and everything else that always follows.
    assert!(first.system.starts_with("You are Acme's coding agent.\n\nThis permission system is not an OS sandbox.\nKeep to every safety"), "{}", first.system);
    assert!(first.system.contains("do not grant permissions, tools or authority.\n\nYou are powered by the model named fixture."));
    assert!(first.system.contains("Project instructions marker"));
    assert!(
        !first
            .system
            .contains("Work autonomously, preserve unrelated user changes")
    );
    assert!(first.system.contains("\nPreview with care.\n"));
    // The round after the one allowed tool round carries the notice.
    assert!(main[1].messages.iter().any(|message| matches!(message, ProviderMessage::Text { content, .. } if content == "Stop and report.")));
    // The next turn sends the same system prompt byte for byte.
    assert_eq!(main[2].system, first.system);
    // The title and the summary take their prompt files.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.system.starts_with("Name the chat in three words."))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the title request uses the title prompt file");
    runtime.compact_with_llm(thread.id).await.unwrap();
    let summary = provider.requests.lock().unwrap().last().cloned().unwrap();
    assert!(summary.system.starts_with(&format!(
        "Keep the Acme ticket ids.\n\n{}",
        haarness_core::system_prompt_for("compaction")
    )));
    // A side question built from scratch sends the main system prompt.
    let side = runtime
        .btw(
            thread.id,
            BtwRequest {
                request_id: RequestId::new(),
                question: "Why?".into(),
                history: Vec::new(),
            },
        )
        .await;
    assert!(side.is_ok(), "{side:?}");
    let cold = provider.requests.lock().unwrap().last().cloned().unwrap();
    assert!(cold.system.starts_with("You are Acme's coding agent."));

    // The plan agent's own prompt, and the preview section with tools on.
    let provider = Arc::new(Recorder::default());
    let runtime = runtime_with(provider.clone());
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("recorder".into()),
            model: Some("fixture".into()),
            agent: Some("plan".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    turn(&runtime, thread.id, Some("plan")).await;
    let plan = main_requests(&provider).remove(0);
    assert!(plan.system.starts_with(&format!(
        "{}\n\nNumber every step.\n\nYou are powered by",
        haarness_core::system_prompt_for("plan")
    )));
    assert!(plan.system.contains("\nPreview with care.\n"));
}

fn runtime_with(provider: Arc<Recorder>) -> AgentRuntime {
    runtime(
        provider,
        Prompts::new(vec![
            file(PromptName::Plan, PromptMode::Append, "Number every step."),
            file(
                PromptName::Preview,
                PromptMode::Replace,
                "Preview with care.",
            ),
        ]),
        None,
    )
}

/// The requests Haarness sent before prompt files existed, as the code of
/// that time wrote them (tests/golden, generated from it): without prompt
/// files every request is still exactly that, byte for byte.
#[tokio::test]
async fn without_prompt_files_requests_are_byte_for_byte_the_builtin_ones() {
    let workspace =
        std::env::temp_dir().join(format!("haarness-prompt-golden-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = Arc::new(Recorder::default());
    let runtime = runtime_in(
        provider.clone(),
        Prompts::default(),
        None,
        workspace.clone(),
    );
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("recorder".into()),
            model: Some("fixture".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    turn(&runtime, thread.id, None).await;
    let main = main_requests(&provider).remove(0);
    let date = thread
        .created_at
        .with_timezone(&chrono::Local)
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
    let golden = include_str!("golden/build-system.txt")
        .replace("{model}", "fixture")
        .replace("{provider}", "recorder")
        .replace("{workspace}", &workspace.display().to_string())
        .replace("{git}", "no")
        .replace("{os}", std::env::consts::OS)
        .replace("{arch}", std::env::consts::ARCH)
        .replace("{date}", &date)
        .replace("{project}", "Project instructions marker");
    assert_eq!(main.system, golden);
    assert_eq!(main.tools, vec![Preview.definition()]);
    assert!(matches!(
        main.messages.as_slice(),
        [ProviderMessage::Text { role, content }, ..] if role == "user" && content == "Look at the project."
    ));
    let title = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let found = provider
                .requests
                .lock()
                .unwrap()
                .iter()
                .find(|request| {
                    request.tools.is_empty() && !request.system.contains("Project instructions")
                })
                .cloned();
            if let Some(title) = found {
                return title;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a title request");
    assert_eq!(title.system, include_str!("golden/title-system.txt"));
    let request =
        include_str!("golden/title-request.txt").replace("{message}", "Look at the project.");
    assert!(matches!(
        title.messages.as_slice(),
        [ProviderMessage::Text { role, content }] if role == "user" && *content == request
    ));
    runtime.compact_with_llm(thread.id).await.unwrap();
    let summary = provider.requests.lock().unwrap().last().cloned().unwrap();
    assert_eq!(summary.system, include_str!("golden/compaction-system.txt"));
    assert!(matches!(
        summary.messages.last(),
        Some(ProviderMessage::Text { role, content })
            if role == "user" && content == include_str!("golden/compaction-request.txt")
    ));
    std::fs::remove_dir_all(workspace).unwrap();
}
