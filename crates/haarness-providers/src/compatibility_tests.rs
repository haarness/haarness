//! Real HTTP/SSE contract regression tests across old and new model families.
use super::*;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, Uri, header::CONTENT_TYPE},
};
use futures::StreamExt;
use haarness_core::{
    ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest, RequestParameters,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

type Captures = Arc<Mutex<Vec<(Value, HeaderMap)>>>;
async fn fixture() -> (String, Captures, tokio::task::JoinHandle<()>) {
    async fn reply(
        State(captures): State<Captures>,
        uri: Uri,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        captures.lock().unwrap().push((body, headers));
        let events = if uri.path().ends_with("/responses") {
            vec![
                json!({"type":"response.output_item.done","item":{"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"opaque-reasoning"}}),
                json!({"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"read","arguments":"{\"path\":\"proof.txt\"}"}}),
                json!({"type":"response.completed","response":{"usage":{"input_tokens":4,"output_tokens":6}}}),
            ]
        } else if uri.path().ends_with("/messages") {
            vec![
                json!({"type":"content_block_start","content_block":{"type":"thinking","thinking":"","signature":""}}),
                json!({"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"Read the fixture."}}),
                json!({"type":"content_block_delta","delta":{"type":"signature_delta","signature":"opaque-"}}),
                json!({"type":"content_block_delta","delta":{"type":"signature_delta","signature":"signature"}}),
                json!({"type":"content_block_stop"}),
                json!({"type":"content_block_start","content_block":{"type":"redacted_thinking","data":"opaque-redacted"}}),
                json!({"type":"content_block_stop"}),
                json!({"type":"content_block_start","content_block":{"type":"tool_use","id":"call_1","name":"read","input":{}}}),
                json!({"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"path\":\"proof.txt\"}"}}),
                json!({"type":"content_block_stop"}),
                // Zero-argument tools may have no input_json_delta events.
                json!({"type":"content_block_start","content_block":{"type":"tool_use","id":"call_2","name":"list","input":{}}}),
                json!({"type":"content_block_stop"}),
                // Gateways may deliver complete input on content_block_start.
                json!({"type":"content_block_start","content_block":{"type":"tool_use","id":"call_3","name":"read","input":{"path":"other.txt"}}}),
                json!({"type":"content_block_stop"}),
                json!({"type":"message_stop"}),
            ]
        } else if uri.path().contains("streamGenerateContent") {
            vec![
                json!({"candidates":[{"content":{"parts":[{"functionCall":{"id":"call_1","name":"read","args":{"path":"proof.txt"}},"thoughtSignature":"opaque-signature"},{"functionCall":{"id":"call_2","name":"read","args":{"path":"other.txt"}}}]},"finishReason":"STOP"}]}),
            ]
        } else {
            vec![
                json!({"choices":[{"delta":{"reasoning_content":"Read "}}]}),
                json!({"choices":[{"delta":{"reasoning_content":"the fixture.","content":[{"type":"thinking","thinking":[{"type":"text","text":"Mistral thinking"}]},{"type":"text","text":"Visible answer"}]}}]}),
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"path\":\"proof.txt\"}"}}]},"finish_reason":"tool_calls"}]}),
            ]
        };
        (
            [(CONTENT_TYPE, "text/event-stream")],
            events
                .iter()
                .map(|event| format!("data: {event}\n\n"))
                .collect(),
        )
    }
    let captures = Captures::default();
    let router = Router::new().fallback(reply).with_state(captures.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, captures, task)
}
fn native(base: &str, id: &str, model: &str) -> NativeInstance {
    NativeInstance {
        id: id.into(),
        name: id.into(),
        base_url: base.into(),
        api_key: Some("fixture".into()),
        models: vec![model.into()],
        discover_models: false,
        model_defaults: CompatibleModelDefaults::default(),
        assume_authenticated: false,
        headers: ProviderHeaders::default(),
    }
}
fn request(model: &str, effort: Option<&str>) -> ProviderRequest {
    ProviderRequest {
        model: model.into(),
        system: "Synthetic contract fixture".into(),
        messages: vec![ProviderMessage::Text {
            role: "user".into(),
            content: "Read proof.txt".into(),
        }],
        tools: vec![],
        reasoning_effort: effort.map(str::to_owned),
        output_schema: None,
        parameters: RequestParameters {
            temperature: Some(0.3),
            top_p: Some(0.8),
            top_k: Some(7),
            max_output_tokens: Some(1024),
            ..Default::default()
        },
    }
}
async fn events(provider: &dyn ModelProvider, request: ProviderRequest) -> Vec<ProviderEvent> {
    provider
        .stream(request, CancellationToken::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect()
}

#[tokio::test]
async fn transport_failures_keep_endpoint_context_without_url_credentials() {
    for transport in ["responses", "compatible", "messages", "gemini", "bedrock"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            // Close a real connection before HTTP headers arrive.
            drop(socket);
        });
        let endpoint = format!(
            "http://fixture-user:fixture-password@{address}/endpoint?key=private-query#private-fragment"
        );
        let provider: Box<dyn ModelProvider> = match transport {
            "responses" => Box::new(OpenAiProvider::for_test(endpoint, "fixture")),
            "compatible" => Box::new(OpenAiCompatibleProvider::new(CompatibleInstance {
                id: "fixture".into(),
                name: "Fixture".into(),
                base_url: endpoint,
                api_key: Some("fixture".into()),
                models: vec!["fixture-model".into()],
                discover_models: false,
                server_output_limit: false,
                model_defaults: CompatibleModelDefaults::default(),
                assume_authenticated: false,
                headers: ProviderHeaders::default(),
            })),
            "messages" => Box::new(AnthropicProvider::new(native(
                &endpoint,
                "fixture",
                "fixture-model",
            ))),
            "gemini" => Box::new(GeminiProvider::new(native(
                &endpoint,
                "fixture",
                "fixture-model",
            ))),
            "bedrock" => Box::new(BedrockProvider::new(
                native(&endpoint, "fixture", "fixture-model"),
                None,
            )),
            _ => unreachable!(),
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            provider.stream(request("fixture-model", None), CancellationToken::new()),
        )
        .await
        .expect("transport failure must settle");
        let error = match result {
            Ok(_) => panic!("the disconnected endpoint must fail: {transport}"),
            Err(error) => error.to_string(),
        };
        task.await.unwrap();
        assert!(
            error.contains(&format!("{address}/endpoint")),
            "{transport} endpoint context missing: {error}"
        );
        for secret in [
            "fixture-user",
            "fixture-password",
            "private-query",
            "private-fragment",
        ] {
            assert!(
                !error.contains(secret),
                "{transport} transport error exposed {secret}"
            );
        }
    }
}

fn continuation(request: &mut ProviderRequest, events: Vec<ProviderEvent>) {
    let mut results = vec![];
    for event in events {
        if let ProviderEvent::ToolCall {
            call_id,
            name,
            arguments,
            provider_metadata,
        } = event
        {
            results.push(ProviderMessage::ToolResult {
                call_id: call_id.clone(),
                name: name.clone(),
                output: "fixture-result".into(),
                is_error: false,
                attachments: vec![],
            });
            request.messages.push(ProviderMessage::ToolCall {
                call_id,
                name,
                arguments,
                provider_metadata,
            });
        }
    }
    assert!(!results.is_empty());
    request.messages.extend(results);
}

#[tokio::test]
async fn openai_new_and_legacy_sampling_and_encrypted_tool_round_trip() {
    let (base, captures, task) = fixture().await;
    let provider = OpenAiProvider::for_test(format!("{base}/responses"), "fixture");
    for (model, effort, sampling) in [
        ("gpt-6-astra", Some("low"), false),
        ("gpt-6-sol", None, false),
        ("gpt-6-sol", Some("none"), true),
        ("gpt-6-luna", Some("low"), false),
        ("gpt-6-luna", Some("none"), true),
        ("gpt-4.1", None, true),
    ] {
        let mut req = request(model, effort);
        let first = events(&provider, req.clone()).await;
        continuation(&mut req, first);
        events(&provider, req).await;
        let captures = captures.lock().unwrap();
        let body = &captures.last().unwrap().0;
        assert_eq!(body.get("temperature").is_some(), sampling, "{model}");
        assert_eq!(body.get("top_p").is_some(), sampling, "{model}");
        assert_eq!(body["input"][1]["encrypted_content"], "opaque-reasoning");
        assert_eq!(body["input"][2]["call_id"], "call_1");
        assert_eq!(body["input"][3]["type"], "function_call_output");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    }
    for model in ["gpt-6-sol", "gpt-6-luna"] {
        let info = provider
            .info()
            .models
            .into_iter()
            .find(|m| m.id == model)
            .unwrap();
        assert!(info.reasoning_efforts.contains(&"none".into()));
        assert_eq!(info.context_window, Some(1_050_000));
    }
    task.abort();
}

#[tokio::test]
async fn claude_signed_and_redacted_thinking_round_trip_and_legacy_sampling() {
    let (base, captures, task) = fixture().await;
    for (model, effort, sampling, binding) in [
        ("claude-opus-5-5", Some("low"), false, true),
        ("claude-fable-5-1", Some("high"), false, true),
        ("claude-opus-5", Some("low"), false, false),
        ("claude-sonnet-5", None, false, false),
        ("claude-opus-4-7", None, false, false),
        ("claude-haiku-4-5", None, true, false),
        ("claude-haiku-4-5", Some("low"), false, false),
    ] {
        let provider = AnthropicProvider::new(native(&base, "anthropic", model));
        let mut req = request(model, effort);
        req.parameters
            .headers
            .insert("anthropic-beta".into(), "existing-beta".into());
        let first = events(&provider, req.clone()).await;
        continuation(&mut req, first);
        events(&provider, req).await;
        let captures = captures.lock().unwrap();
        let (body, headers) = captures.last().unwrap();
        assert_eq!(body.get("temperature").is_some(), sampling, "{model}");
        assert_eq!(body.get("top_k").is_some(), sampling, "{model}");
        let blocks = &body["messages"][1]["content"];
        assert_eq!(body["messages"][2]["content"][0]["input"], json!({}));
        assert_eq!(
            body["messages"][3]["content"][0]["input"],
            json!({"path":"other.txt"})
        );
        assert_eq!(
            blocks[0],
            json!({"type":"thinking","thinking":"Read the fixture.","signature":"opaque-signature"})
        );
        assert_eq!(
            blocks[1],
            json!({"type":"redacted_thinking","data":"opaque-redacted"})
        );
        assert_eq!(blocks[2]["type"], "tool_use");
        assert_eq!(body["thinking"].get("block_binding").is_some(), binding);
        assert_eq!(
            headers["anthropic-beta"]
                .to_str()
                .unwrap()
                .contains("thinking-binding-controls"),
            binding
        );
        assert!(
            headers["anthropic-beta"]
                .to_str()
                .unwrap()
                .starts_with("existing-beta")
        );
    }
    task.abort();
}

#[tokio::test]
async fn gemini_thinking_controls_and_parallel_signed_call_ids() {
    let (base, captures, task) = fixture().await;
    for (model, effort, field, value) in [
        ("gemini-3.8-flash", "low", "thinkingLevel", json!("low")),
        ("gemini-3.7-flash", "minimal", "thinkingLevel", json!("low")),
        (
            "gemini-3.6-flash",
            "minimal",
            "thinkingLevel",
            json!("minimal"),
        ),
        (
            "gemini-3.1-pro-preview",
            "medium",
            "thinkingLevel",
            json!("medium"),
        ),
        ("gemini-2.5-flash", "minimal", "thinkingBudget", json!(0)),
        ("gemini-2.5-pro", "low", "thinkingBudget", json!(2048)),
        ("gemini-2.5-pro", "minimal", "thinkingBudget", json!(128)),
    ] {
        let provider = GeminiProvider::new(native(&base, "gemini", model));
        let mut req = request(model, Some(effort));
        let first = events(&provider, req.clone()).await;
        continuation(&mut req, first);
        events(&provider, req).await;
        let captures = captures.lock().unwrap();
        let body = &captures.last().unwrap().0;
        let config = &body["generationConfig"]["thinkingConfig"];
        assert_eq!(config[field], value, "{model}");
        assert_eq!(config.as_object().unwrap().len(), 2);
        let contents = &body["contents"];
        assert_eq!(contents[1]["parts"].as_array().unwrap().len(), 2);
        assert_eq!(
            contents[1]["parts"][0]["thoughtSignature"],
            "opaque-signature"
        );
        assert_eq!(contents[1]["parts"][1]["functionCall"]["id"], "call_2");
        assert_eq!(contents[2]["parts"][1]["functionResponse"]["id"], "call_2");
        if model.starts_with("gemini-3.7") || model.starts_with("gemini-3.8") {
            assert!(
                !provider.info().models[0]
                    .reasoning_efforts
                    .contains(&"minimal".into())
            );
        }
    }
    task.abort();
}

#[tokio::test]
async fn compatible_reasoning_is_provider_scoped_and_replayed() {
    let (base, captures, task) = fixture().await;
    for (id, model, effort) in [
        ("deepseek", "deepseek-flash", "max"),
        ("deepseek", "deepseek-v4-pro", "none"),
        ("xai", "grok-4.7", "xhigh"),
        ("mistral", "mistral-medium-3-5", "high"),
        ("groq", "openai/gpt-oss-120b", "low"),
        ("custom", "legacy-model", "high"),
    ] {
        let provider = OpenAiCompatibleProvider::new(CompatibleInstance {
            id: id.into(),
            name: id.into(),
            base_url: base.clone(),
            api_key: Some("fixture".into()),
            models: vec![model.into()],
            discover_models: false,
            server_output_limit: false,
            model_defaults: CompatibleModelDefaults::default(),
            assume_authenticated: false,
            headers: ProviderHeaders::default(),
        });
        let mut req = request(model, Some(effort));
        let first = events(&provider, req.clone()).await;
        assert!(
            first
                .iter()
                .any(|e| matches!(e,ProviderEvent::TextDelta(s) if s=="Visible answer"))
        );
        assert!(
            first
                .iter()
                .any(|e| matches!(e,ProviderEvent::ReasoningDelta(s) if s=="Mistral thinking"))
        );
        continuation(&mut req, first);
        events(&provider, req).await;
        let captures = captures.lock().unwrap();
        let body = &captures.last().unwrap().0;
        if id == "deepseek" {
            assert_eq!(
                body["messages"][2]["reasoning_content"],
                "Read the fixture."
            );
            assert_eq!(
                body["thinking"]["type"],
                if effort == "none" {
                    "disabled"
                } else {
                    "enabled"
                }
            );
        } else {
            assert!(body.get("thinking").is_none());
            assert!(body["messages"][2].get("reasoning_content").is_none());
        }
        if id == "custom" || (id == "deepseek" && effort == "none") {
            assert!(body.get("reasoning_effort").is_none());
        } else {
            assert_eq!(body["reasoning_effort"], effort);
        }
        if id == "mistral" {
            assert_eq!(body["messages"][2]["content"][0]["type"], "thinking");
        }
    }
    task.abort();
}

#[tokio::test]
async fn truncated_http_streams_never_fabricate_successful_completion() {
    async fn truncated(uri: Uri) -> ([(axum::http::HeaderName, &'static str); 1], String) {
        let data = if uri.path().ends_with("/responses") {
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\ndata: {\"type\":\"response.incomplete\",\"response\":{}}\n\n"
        } else {
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\ndata: [DONE]\n\n"
        };
        ([(CONTENT_TYPE, "text/event-stream")], data.into())
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(truncated))
            .await
            .unwrap()
    });
    let responses = OpenAiProvider::for_test(format!("{base}/responses"), "fixture");
    let compatible = OpenAiCompatibleProvider::new(CompatibleInstance {
        id: "custom".into(),
        name: "Fixture".into(),
        base_url: base,
        api_key: Some("fixture".into()),
        models: vec!["fixture-model".into()],
        discover_models: false,
        server_output_limit: false,
        model_defaults: CompatibleModelDefaults::default(),
        assume_authenticated: false,
        headers: ProviderHeaders::default(),
    });
    for provider in [
        &responses as &dyn ModelProvider,
        &compatible as &dyn ModelProvider,
    ] {
        let received = events(provider, request("fixture-model", None)).await;
        assert!(
            received
                .iter()
                .any(|event| matches!(event, ProviderEvent::TextDelta(text) if text=="partial"))
        );
        assert!(received.iter().any(|event| matches!(event, ProviderEvent::Finished {reason} if matches!(ProviderEvent::finish_error(reason), Some(haarness_core::CoreError::ProviderIncomplete(_))))));
        assert!(!received.iter().any(|event| matches!(event, ProviderEvent::Finished {reason} if ProviderEvent::finish_error(reason).is_none())));
    }
    task.abort();
}
