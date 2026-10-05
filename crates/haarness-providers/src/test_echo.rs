use async_trait::async_trait;
use futures::stream;
use haarness_core::{
    CoreError, CoreResult, ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest,
    ProviderStream,
};
use haarness_protocol::{ModelInfo, ProviderInfo};
use tokio_util::sync::CancellationToken;

/// Deterministic test provider.
///
/// Beyond echoing the last user message, it understands scripting directives
/// that let end-to-end tests exercise tool, permission, reasoning, and
/// rendering paths without a real model:
///
/// - `!tool <name> <json-args>` — first round emits a tool call; the follow-up
///   round emits `Tool <name> => <output>` from the tool result.
/// - `!preface <name> <json-args>` — like `!tool`, but emits assistant prose
///   before the call so persistence/rendering order can be tested.
/// - `!multi <json-array>` — emits several tool calls in one round, e.g.
///   `[{"name":"task","arguments":{...}}, ...]`; the follow-up round emits
///   `All tools finished`.
/// - `::reason <text>` — emits a reasoning delta before the text reply.
/// - `::md` — emits a markdown-rich reply (bold, inline code, fenced block).
/// - `::mdslow` — emits Markdown in delayed chunks for live-rendering E2E.
/// - `::scrollstream` — reflows delayed Markdown while scroll tests are paused.
/// - `::long <n>` — emits `n` numbered marker lines.
pub struct EchoProvider;

#[async_trait]
impl ModelProvider for EchoProvider {
    fn id(&self) -> &str {
        "echo"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "echo".into(),
            name: "Deterministic test provider".into(),
            authenticated: true,
            models: vec![ModelInfo {
                id: "echo-1".into(),
                display_name: "Echo 1".into(),
                reasoning: false,
                reasoning_efforts: Vec::new(),
                images: false,
                video: false,
                tools: true,
                context_window: None,
                max_output_tokens: None,
                pricing: None,
            }],
        }
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream> {
        let mut last_user_index = None;
        let mut last_tool_result = None;
        for (index, message) in request.messages.iter().enumerate() {
            match message {
                ProviderMessage::Text { role, .. } if role == "user" => {
                    last_user_index = Some(index);
                }
                ProviderMessage::Media { role, .. } if role == "user" => {
                    last_user_index = Some(index);
                }
                ProviderMessage::ToolResult { name, output, .. } => {
                    last_tool_result = Some((index, name.clone(), output.clone()));
                }
                _ => {}
            }
        }
        let prompt = last_user_index
            .and_then(|index| match &request.messages[index] {
                ProviderMessage::Text { content, .. } => Some(content.clone()),
                ProviderMessage::Media {
                    content,
                    attachments,
                    ..
                } => Some(format!("{content}\n[attachments: {}]", attachments.len())),
                _ => None,
            })
            .unwrap_or_default();

        // Auxiliary naming uses the same provider without executing its
        // scripted tools or waiting for a scripted main-response stream.
        if request.system == haarness_core::system_prompt_for("title") {
            // Titles quote the first message; echo the message itself.
            let message = prompt
                .split_once("<message>\n")
                .and_then(|(_, rest)| rest.rsplit_once("\n</message>"))
                .map_or(prompt.as_str(), |(message, _)| message);
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta(format!("Echo: {message}"))),
                Ok(ProviderEvent::Finished {
                    reason: "completed".into(),
                }),
            ])));
        }

        let mut events = Vec::new();
        // Goal pursuit prepends a hidden user-role doctrine to the real user
        // text. A real model follows the trailing request; keep the
        // deterministic provider equally useful for goal/tmux coverage by
        // scripting against that request instead of the doctrine envelope.
        let directive = prompt
            .rsplit_once("</goal-harness>\n\n")
            .map_or_else(|| prompt.trim_start(), |(_, request)| request.trim_start());
        // `!linger <ms> <directive>` keeps the turn running that long before
        // its final reply, so a client can observe live state (a plan, an
        // activity line) without parking the turn on a prompt.
        let (linger, directive) = directive
            .strip_prefix("!linger ")
            .and_then(|rest| {
                let (millis, rest) = rest.split_once(' ')?;
                Some((
                    Some(std::time::Duration::from_millis(millis.parse().ok()?)),
                    rest.trim_start(),
                ))
            })
            .unwrap_or((None, directive));
        if let Some(schema) = request.output_schema.as_ref() {
            // A schema turn still permits ordinary tools first, mirroring the
            // required-any tool choice used by real providers. Once no tool is
            // pending, Echo deterministically completes with a valid value.
            let unresolved_tool = directive
                .strip_prefix("!tool ")
                .filter(|_| last_tool_result.is_none());
            if let Some(script) = unresolved_tool {
                let (name, arguments) = parse_tool_script(script);
                return Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderEvent::ToolCall {
                        call_id: "echo-call-1".into(),
                        name,
                        arguments,
                        provider_metadata: serde_json::Value::Null,
                    }),
                    Ok(ProviderEvent::Finished {
                        reason: "tool_calls".into(),
                    }),
                ])));
            }
            events.push(Ok(ProviderEvent::StructuredOutput(example_for_schema(
                schema,
            ))));
            events.push(Ok(ProviderEvent::Usage {
                input_tokens: prompt.split_whitespace().count() as u64,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }));
            events.push(Ok(ProviderEvent::Finished {
                reason: "completed".into(),
            }));
            return Ok(Box::pin(stream::iter(events)));
        }
        if directive.starts_with("::mdslow") {
            let input_tokens = prompt.split_whitespace().count() as u64;
            return Ok(Box::pin(async_stream::stream! {
                yield Ok(ProviderEvent::TextDelta("**Live markdown**".into()));
                tokio::time::sleep(std::time::Duration::from_millis(900)).await;
                yield Ok(ProviderEvent::TextDelta("\n\n- streams while working".into()));
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                yield Ok(ProviderEvent::Usage {
                    input_tokens,
                    output_tokens: 6,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                });
                yield Ok(ProviderEvent::Finished { reason: "completed".into() });
            }));
        }
        if directive.starts_with("::scrollstream") {
            let input_tokens = prompt.split_whitespace().count() as u64;
            return Ok(Box::pin(async_stream::stream! {
                yield Ok(ProviderEvent::TextDelta(concat!(
                    "STREAM_REFLOW_STAGE_ONE\n\n",
                    "[release notes](https://example.com/a/very/long/streamed/link/",
                    "whose/unclosed/destination/wraps/across/several/terminal/rows/",
                    "before/the/provider/finishes/the/markdown",
                ).into()));
                tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
                // Closing the link hides the long destination and contracts
                // the rendered live block despite this being an append-only
                // provider delta.
                yield Ok(ProviderEvent::TextDelta(")".into()));
                tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                yield Ok(ProviderEvent::TextDelta(concat!(
                    "\n\nSTREAM_REFLOW_DONE",
                    "\n\n- final streamed row one",
                    "\n- final streamed row two",
                ).into()));
                yield Ok(ProviderEvent::Usage {
                    input_tokens,
                    output_tokens: 20,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                });
                yield Ok(ProviderEvent::Finished { reason: "completed".into() });
            }));
        }
        if let Some(script) = directive.strip_prefix("!preface ") {
            let resolved = last_tool_result.filter(|(index, _, _)| Some(*index) > last_user_index);
            if let Some((_, name, output)) = resolved {
                events.push(Ok(ProviderEvent::TextDelta(format!(
                    "Tool {name} => {output}"
                ))));
            } else {
                let (name, arguments) = parse_tool_script(script);
                events.push(Ok(ProviderEvent::TextDelta(
                    "I’ll inspect the repository first.".into(),
                )));
                events.push(Ok(ProviderEvent::ToolCall {
                    call_id: "echo-call-1".into(),
                    name,
                    arguments,
                    provider_metadata: serde_json::Value::Null,
                }));
            }
        } else if let Some(script) = directive.strip_prefix("!tool ") {
            let resolved = last_tool_result.filter(|(index, _, _)| Some(*index) > last_user_index);
            if let Some((_, name, output)) = resolved {
                events.push(Ok(ProviderEvent::TextDelta(format!(
                    "Tool {name} => {output}"
                ))));
            } else {
                let (name, arguments) = parse_tool_script(script);
                events.push(Ok(ProviderEvent::ToolCall {
                    call_id: "echo-call-1".into(),
                    name,
                    arguments,
                    provider_metadata: serde_json::Value::Null,
                }));
            }
        } else if let Some(script) = directive.strip_prefix("!multi ") {
            let resolved = last_tool_result.filter(|(index, _, _)| Some(*index) > last_user_index);
            if resolved.is_some() {
                events.push(Ok(ProviderEvent::TextDelta("All tools finished".into())));
            } else {
                let calls: Vec<serde_json::Value> =
                    serde_json::from_str(script.trim()).unwrap_or_default();
                for (index, call) in calls.iter().enumerate() {
                    events.push(Ok(ProviderEvent::ToolCall {
                        call_id: format!("echo-call-{}", index + 1),
                        name: call["name"].as_str().unwrap_or("bash").to_owned(),
                        arguments: call["arguments"].clone(),
                        provider_metadata: serde_json::Value::Null,
                    }));
                }
            }
        } else if let Some(rest) = directive.strip_prefix("::reason") {
            events.push(Ok(ProviderEvent::ReasoningDelta(format!(
                "Considering{}",
                if rest.trim().is_empty() {
                    " the request".to_owned()
                } else {
                    format!(" {}", rest.trim())
                }
            ))));
            events.push(Ok(ProviderEvent::TextDelta("Reasoned reply".into())));
        } else if directive.starts_with("::md") {
            events.push(Ok(ProviderEvent::TextDelta(
                "**bold** and `code`\n\n```rust\nfn main() {}\n```".into(),
            )));
        } else if let Some(rest) = directive.strip_prefix("::long") {
            use std::fmt::Write as _;
            let count: usize = rest.trim().parse().unwrap_or(30);
            let mut body = String::new();
            for line in 1..=count {
                let _ = writeln!(body, "line {line} marker-{line}");
            }
            events.push(Ok(ProviderEvent::TextDelta(body)));
        } else {
            events.push(Ok(ProviderEvent::TextDelta(format!("Echo: {prompt}"))));
        }
        events.push(Ok(ProviderEvent::Usage {
            input_tokens: prompt.split_whitespace().count() as u64,
            output_tokens: prompt.split_whitespace().count() as u64 + 1,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }));
        events.push(Ok(ProviderEvent::Finished {
            reason: "completed".into(),
        }));
        let wants_tools = events
            .iter()
            .any(|event| matches!(event, Ok(ProviderEvent::ToolCall { .. })));
        if let Some(delay) = linger.filter(|_| !wants_tools) {
            return Ok(Box::pin(async_stream::stream! {
                tokio::select! {
                    () = cancel.cancelled() => {
                        yield Err(CoreError::Cancelled);
                        return;
                    }
                    () = tokio::time::sleep(delay) => {}
                }
                for event in events {
                    yield event;
                }
            }));
        }
        Ok(Box::pin(stream::iter(events)))
    }
}

fn parse_tool_script(script: &str) -> (String, serde_json::Value) {
    let script = script.trim();
    let (name, rest) = script
        .split_once(char::is_whitespace)
        .unwrap_or((script, ""));
    let arguments = serde_json::from_str(rest.trim()).unwrap_or_else(|_| serde_json::json!({}));
    (name.to_owned(), arguments)
}

fn example_for_schema(schema: &serde_json::Value) -> serde_json::Value {
    if let Some(value) = schema.get("const") {
        return value.clone();
    }
    if let Some(value) = schema.get("default") {
        return value.clone();
    }
    if let Some(value) = schema
        .get("enum")
        .and_then(serde_json::Value::as_array)
        .and_then(|values| values.first())
    {
        return value.clone();
    }
    if let Some(branch) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(serde_json::Value::as_array)
        .and_then(|values| values.first())
    {
        return example_for_schema(branch);
    }
    match schema.get("type").and_then(serde_json::Value::as_str) {
        Some("object") | None if schema.get("properties").is_some() => {
            let required = schema
                .get("required")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .collect::<std::collections::HashSet<_>>();
            let values = schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .into_iter()
                .flatten()
                .filter(|(key, _)| required.contains(key.as_str()))
                .map(|(key, value)| (key.clone(), example_for_schema(value)))
                .collect();
            serde_json::Value::Object(values)
        }
        Some("array") => {
            let count = schema
                .get("minItems")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let item = schema
                .get("items")
                .map(example_for_schema)
                .unwrap_or(serde_json::Value::Null);
            serde_json::Value::Array((0..count).map(|_| item.clone()).collect())
        }
        Some("integer") => serde_json::json!(
            schema
                .get("minimum")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0)
        ),
        Some("number") => serde_json::json!(
            schema
                .get("minimum")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0)
        ),
        Some("boolean") => serde_json::Value::Bool(false),
        Some("null") => serde_json::Value::Null,
        _ => {
            let length = schema
                .get("minLength")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            serde_json::Value::String("x".repeat(usize::try_from(length).unwrap_or(0)))
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use haarness_core::{ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest};
    use tokio_util::sync::CancellationToken;

    use super::EchoProvider;

    fn request(messages: Vec<ProviderMessage>) -> ProviderRequest {
        ProviderRequest {
            model: "echo-1".into(),
            system: String::new(),
            messages,
            tools: Vec::new(),
            reasoning_effort: None,
            parameters: haarness_core::RequestParameters::default(),
            output_schema: None,
        }
    }

    async fn events(messages: Vec<ProviderMessage>) -> Vec<ProviderEvent> {
        EchoProvider
            .stream(request(messages), CancellationToken::new())
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await
    }

    fn user(content: &str) -> ProviderMessage {
        ProviderMessage::Text {
            role: "user".into(),
            content: content.into(),
        }
    }

    #[tokio::test]
    async fn tool_script_emits_a_call_then_reports_the_result() {
        let first = events(vec![user(r#"!tool list {"path":"."}"#)]).await;
        assert!(matches!(
            &first[0],
            ProviderEvent::ToolCall { call_id, name, arguments, .. }
                if call_id == "echo-call-1"
                    && name == "list"
                    && arguments["path"] == "."
        ));

        let second = events(vec![
            user(r#"!tool list {"path":"."}"#),
            ProviderMessage::ToolCall {
                call_id: "echo-call-1".into(),
                name: "list".into(),
                arguments: serde_json::json!({"path":"."}),
                provider_metadata: serde_json::Value::Null,
            },
            ProviderMessage::ToolResult {
                call_id: "echo-call-1".into(),
                name: "list".into(),
                output: "Cargo.toml src".into(),
                is_error: false,
                attachments: Vec::new(),
            },
        ])
        .await;
        assert!(matches!(
            &second[0],
            ProviderEvent::TextDelta(text) if text == "Tool list => Cargo.toml src"
        ));
    }

    #[tokio::test]
    async fn goal_doctrine_keeps_the_trailing_user_directive_executable() {
        let wrapped = concat!(
            "<goal-harness>\n",
            "standing goal doctrine\n",
            "</goal-harness>\n\n",
            "!tool update_goal {\"status\":\"complete\"}",
        );
        let result = events(vec![user(wrapped)]).await;
        assert!(matches!(
            &result[0],
            ProviderEvent::ToolCall { name, arguments, .. }
                if name == "update_goal" && arguments["status"] == "complete"
        ));
    }

    #[tokio::test]
    async fn display_directives_produce_deterministic_output() {
        let markdown = events(vec![user("::md")]).await;
        assert!(matches!(
            &markdown[0],
            ProviderEvent::TextDelta(text) if text.contains("**bold**")
        ));

        let long = events(vec![user("::long 3")]).await;
        assert!(matches!(
            &long[0],
            ProviderEvent::TextDelta(text)
                if text.contains("marker-1") && text.contains("marker-3")
        ));

        let reasoned = events(vec![user("::reason carefully")]).await;
        assert!(matches!(
            &reasoned[0],
            ProviderEvent::ReasoningDelta(text) if text == "Considering carefully"
        ));
        assert!(matches!(
            &reasoned[1],
            ProviderEvent::TextDelta(text) if text == "Reasoned reply"
        ));
    }

    #[tokio::test]
    async fn plain_prompts_still_echo() {
        let plain = events(vec![user("hello")]).await;
        assert!(matches!(
            &plain[0],
            ProviderEvent::TextDelta(text) if text == "Echo: hello"
        ));
    }

    #[tokio::test]
    async fn schema_requests_produce_deterministic_structured_output() {
        let mut request = request(vec![user("count")]);
        request.output_schema = Some(serde_json::json!({
            "type":"object",
            "properties":{"count":{"type":"integer","minimum":2}},
            "required":["count"],
            "additionalProperties":false
        }));
        let events: Vec<_> = EchoProvider
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await;
        assert!(matches!(
            &events[0],
            ProviderEvent::StructuredOutput(value) if *value == serde_json::json!({"count":2})
        ));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ProviderEvent::TextDelta(_)))
        );
    }
}
