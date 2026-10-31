//! Tool schemas Claude refuses. Claude validates every tool before inference
//! and answers HTTP 400 for the whole request when one root input schema
//! combines alternatives, so a single such MCP tool would fail every turn.
//! Adapters that reach Claude omit those tools and warn once per tool name.

use std::{
    collections::HashSet,
    sync::{LazyLock, Mutex},
};

use haarness_core::ToolDefinition;
use serde_json::Value;

/// Root keywords Claude rejects in a tool's `input_schema`. Nested uses and
/// properties with these names are accepted.
const ROOT_COMBINERS: [&str; 3] = ["oneOf", "anyOf", "allOf"];

static WARNED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);

/// Claude models that answer 400 to a forced tool choice (`any` or a named
/// tool), under any vendor prefix: `claude-opus-5-5`, `anthropic/claude-sonnet-5.5`,
/// `us.anthropic.claude-fable-5-1-v1:0`. Structured output then relies on the
/// runtime's request to call the tool and its corrective retry, so a release
/// newer than any Haarness knows is treated as refusing: unforced, it still
/// answers in the schema's shape. Fable and Mythos refuse from 5.1, the
/// other families from 5.5; Haiku 5.5 still takes a forced choice.
pub(crate) fn refuses_forced_tool_choice(model: &str) -> bool {
    let Some(release) = crate::anthropic::claude_release(model) else {
        return false;
    };
    match release.family.as_str() {
        "fable" | "mythos" => release.version >= (5, 1),
        "haiku" => release.version > crate::anthropic::NEWEST_CLAUDE,
        _ => release.version >= (5, 5),
    }
}

pub(crate) fn claude_model(model: &str) -> bool {
    model.to_ascii_lowercase().contains("claude")
}

/// Why Claude would reject `schema` as a tool's root input schema.
pub(crate) fn claude_schema_issues(schema: &Value) -> Vec<String> {
    let Some(root) = schema.as_object() else {
        return vec!["the input schema is not a JSON object".into()];
    };
    let mut issues = ROOT_COMBINERS
        .iter()
        .filter(|key| root.contains_key(**key))
        .map(|key| format!("{key}: unsupported at the top level by Claude tool schema"))
        .collect::<Vec<_>>();
    // A missing type is left alone; MCP definitions fill it in.
    if root.get("type").is_some_and(|kind| kind != "object") {
        issues.push("type: Claude tool input must be an object".into());
    }
    issues
}

/// Drop the tools Claude would reject when `model` is a Claude model.
pub(crate) fn retain_claude_compatible(model: &str, tools: &mut Vec<ToolDefinition>) {
    if !claude_model(model) {
        return;
    }
    tools.retain(|tool| {
        let issues = claude_schema_issues(&tool.input_schema);
        if issues.is_empty() {
            return true;
        }
        if first_warning(&tool.name) {
            tracing::warn!(
                tool = %tool.name,
                issues = %issues.join("; "),
                "skipping provider-incompatible tool schema for Claude"
            );
        }
        false
    });
}

/// The same tool is offered on every request; one warning per process names it.
fn first_warning(tool: &str) -> bool {
    WARNED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(tool.to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_root_combiners_and_non_object_roots_are_rejected() {
        for key in ROOT_COMBINERS {
            let schema = json!({"type":"object", key: [{"required":["a"]},{"required":["b"]}]});
            assert_eq!(
                claude_schema_issues(&schema),
                [format!(
                    "{key}: unsupported at the top level by Claude tool schema"
                )]
            );
            let nested = json!({"type":"object","properties":{
                "value": {key: [{"type":"string"},{"type":"integer"}]},
                key: {"type":"string"}
            }});
            assert!(claude_schema_issues(&nested).is_empty());
        }
        assert!(claude_schema_issues(&json!({"properties":{}})).is_empty());
        assert_eq!(claude_schema_issues(&json!({"type":"string"})).len(), 1);
        assert_eq!(claude_schema_issues(&json!(true)).len(), 1);
    }

    #[test]
    fn claude_models_are_recognized_across_vendors() {
        for model in [
            "claude-opus-5-5",
            "anthropic/claude-sonnet-5.5",
            "us.anthropic.claude-sonnet-4-6-v1:0",
            "Claude-Haiku",
        ] {
            assert!(claude_model(model), "{model}");
        }
        for model in ["MiniMax-M3", "gpt-6.1-sol", "amazon.nova-pro-v1:0"] {
            assert!(!claude_model(model), "{model}");
        }
        for model in [
            "claude-opus-5-5",
            "anthropic/claude-sonnet-5.5",
            "us.anthropic.claude-fable-5-1-v1:0",
            "claude-mythos-5-1",
        ] {
            assert!(refuses_forced_tool_choice(model), "{model}");
        }
        for model in [
            "claude-sonnet-5",
            "anthropic/claude-haiku-5.5",
            "gpt-6.1-sol",
            "claude-3-5-sonnet-20241022",
            "claude-opus-4-20250514",
        ] {
            assert!(!refuses_forced_tool_choice(model), "{model}");
        }
        // A release newer than any listed takes the newest behaviour.
        for model in [
            "claude-opus-6",
            "anthropic/claude-sonnet-5.7",
            "claude-haiku-6-20270101",
            "global.anthropic.claude-nova-6-1-v1:0",
        ] {
            assert!(refuses_forced_tool_choice(model), "{model}");
        }
    }

    #[test]
    fn each_skipped_tool_warns_once_per_process() {
        // No other test offers this tool, so the process has not warned yet.
        let name = "mcp__warn_once_fixture__tool".to_owned();
        assert!(first_warning(&name));
        assert!(!first_warning(&name));
        let mut tools = vec![ToolDefinition {
            name: name.clone(),
            description: String::new(),
            input_schema: json!({"anyOf": []}),
        }];
        retain_claude_compatible("claude-test", &mut tools);
        assert!(tools.is_empty());
    }
}
