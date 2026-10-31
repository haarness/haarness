//! Model lists follow the providers' catalogs as they are now: a chat with
//! no model named gets one from them, and a chat on a model a provider has
//! retired is told so, with models it lists now.
use super::*;

/// A provider that lists two models and refuses every other as unknown, as
/// providers answer a model they have retired.
struct Retiring;
#[async_trait]
impl ModelProvider for Retiring {
    fn id(&self) -> &str {
        "acme"
    }
    fn info(&self) -> ProviderInfo {
        let model = |id: &str, name: &str| ModelInfo {
            id: id.into(),
            display_name: name.into(),
            reasoning: false,
            reasoning_efforts: Vec::new(),
            images: false,
            video: false,
            tools: true,
            context_window: Some(128_000),
            max_output_tokens: None,
            pricing: None,
        };
        ProviderInfo {
            id: "acme".into(),
            name: "Acme AI".into(),
            authenticated: true,
            models: vec![
                model("acme-9", "Acme 9"),
                model("acme-9-mini", "Acme 9 Mini"),
            ],
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        if self
            .info()
            .models
            .iter()
            .any(|model| model.id == request.model)
        {
            return Echo.stream(request, cancel).await;
        }
        Err(CoreError::Provider(format!(
            "HTTP 404 Not Found: The model `{}` does not exist or you do not have access to it.",
            request.model
        )))
    }
}

fn runtime() -> AgentRuntime {
    AgentRuntime::new(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(Retiring) as Arc<dyn ModelProvider>
        ])),
        Arc::new(EmptyToolRegistry),
        EventBus::new(64),
    )
}

#[tokio::test]
async fn a_chat_with_no_model_named_gets_one_its_providers_list_now() {
    let runtime = runtime();
    let thread = runtime
        .create_thread(CreateThreadRequest::default())
        .await
        .unwrap();
    assert_eq!(
        (thread.provider.as_str(), thread.model.as_str()),
        ("acme", "acme-9")
    );
    let named = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("acme".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(named.model, "acme-9");
}

#[tokio::test]
async fn a_chat_on_a_retired_model_is_offered_the_models_listed_now() {
    let runtime = runtime();
    // A stored chat's model the provider no longer lists still sends: the
    // provider decides.
    let thread = runtime
        .create_thread(CreateThreadRequest {
            provider: Some("acme".into()),
            model: Some("acme-7".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(thread.model, "acme-7");
    let turn = runtime
        .start_turn(thread.id, turn_request("hello"))
        .await
        .unwrap();
    let finished = wait_done(&runtime, &turn).await;
    assert_eq!(finished.status, TurnStatus::Failed);
    assert_eq!(
        finished.error.as_deref(),
        Some(
            "Acme AI no longer offers acme-7. Choose another model for this chat, such as Acme 9 or Acme 9 Mini."
        )
    );
}
