//! Read-only reuse of requests already prepared by the main turn.
use std::{collections::VecDeque, sync::Arc};

use haarness_protocol::{ThreadId, TurnId};

use crate::{ModelProvider, ProviderMessage, ProviderRequest, ToolDefinition};

#[derive(Default)]
pub(crate) struct PreparedPrompts(VecDeque<Snapshot>);

struct Snapshot {
    thread: ThreadId,
    turn: TurnId,
    provider: Arc<dyn ModelProvider>,
    source: Vec<ProviderMessage>,
    catalog: Vec<ToolDefinition>,
    request: ProviderRequest,
}

impl PreparedPrompts {
    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn remember(
        &mut self,
        thread: ThreadId,
        turn: TurnId,
        provider: Arc<dyn ModelProvider>,
        source: Vec<ProviderMessage>,
        mut catalog: Vec<ToolDefinition>,
        request: ProviderRequest,
    ) {
        self.0.retain(|snapshot| snapshot.thread != thread);
        // Bound both the number and size of retained requests. This is an
        // optimization; a cold/evicted session reconstructs its current context.
        let size = request.system.len()
            + serde_json::to_vec(&source).map_or(usize::MAX / 4, |value| value.len())
            + serde_json::to_vec(&request.messages).map_or(usize::MAX / 4, |value| value.len())
            + serde_json::to_vec(&request.tools).map_or(usize::MAX / 4, |value| value.len())
            + serde_json::to_vec(&catalog).map_or(usize::MAX / 4, |value| value.len());
        if size > 4 * 1024 * 1024 {
            return;
        }
        catalog.sort_by(|a, b| a.name.cmp(&b.name));
        self.0.push_back(Snapshot {
            thread,
            turn,
            provider,
            source,
            catalog,
            request,
        });
        while self.0.len() > 16 {
            self.0.pop_front();
        }
    }

    pub fn request(
        &self,
        thread: ThreadId,
        turn: Option<TurnId>,
        mut catalog: Vec<ToolDefinition>,
        messages: &[ProviderMessage],
    ) -> Option<(Arc<dyn ModelProvider>, ProviderRequest)> {
        catalog.sort_by(|a, b| a.name.cmp(&b.name));
        let snapshot = self.0.iter().find(|snapshot| {
            snapshot.thread == thread
                && Some(snapshot.turn) == turn
                && snapshot.catalog == catalog
                && messages.starts_with(&snapshot.source)
        })?;
        let mut request = snapshot.request.clone();
        request
            .messages
            .extend_from_slice(&messages[snapshot.source.len()..]);
        Some((snapshot.provider.clone(), request))
    }
}

pub(crate) fn settled(mut messages: Vec<ProviderMessage>) -> Vec<ProviderMessage> {
    let boundary = messages.iter().enumerate().find_map(|(index, message)| {
        let ProviderMessage::ToolCall { call_id, .. } = message else { return None };
        (!messages[index + 1..].iter().any(|message| {
            matches!(message, ProviderMessage::ToolResult { call_id: result, .. } if result == call_id)
        })).then_some(index)
    });
    if let Some(boundary) = boundary {
        messages.truncate(boundary);
    }
    messages
}

pub(crate) fn system(
    mut base: String,
    project: &str,
    memory: &str,
    hook: Option<&str>,
    ultracode: bool,
    structured: bool,
) -> String {
    for (heading, value) in [
        ("Project and global instructions:", project),
        ("Durable project memory index:", memory),
        ("Context from hooks:", hook.unwrap_or_default()),
    ] {
        if !value.is_empty() {
            base.push_str("\n\n");
            base.push_str(heading);
            base.push('\n');
            base.push_str(value);
        }
    }
    if ultracode {
        base.push_str("\n\n");
        base.push_str(crate::ULTRACODE_SYSTEM_SECTION);
    }
    if structured {
        base.push_str("\n\nIMPORTANT: This turn requires structured output. Use the StructuredOutput tool for the final answer. Do not finish with plain text; other tools may be used first when needed.");
    }
    base
}

pub(crate) const LAZY_INSTRUCTIONS: &str = "\n\nSome optional tool schemas are deferred to protect the model context. If a required capability is missing, call load_tool with exact ids, wildcard patterns, functional groups, or descriptive search terms; use the loaded tool on the following round.";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn text(value: &str) -> ProviderMessage {
        ProviderMessage::Text {
            role: "user".into(),
            content: value.into(),
        }
    }
    fn call(id: &str) -> ProviderMessage {
        ProviderMessage::ToolCall {
            call_id: id.into(),
            name: "fixture".into(),
            arguments: json!({}),
            provider_metadata: Value::Null,
        }
    }
    fn result(id: &str) -> ProviderMessage {
        ProviderMessage::ToolResult {
            call_id: id.into(),
            name: "fixture".into(),
            output: "done".into(),
            is_error: false,
            attachments: vec![],
        }
    }
    #[test]
    fn side_snapshot_stops_at_the_first_unfinished_call_and_retains_settled_pairs() {
        let settled_prefix = vec![text("User instruction"), call("done"), result("done")];
        let mut input = settled_prefix.clone();
        input.extend([
            call("running"),
            text("Queued instruction"),
            call("later"),
            result("later"),
        ]);
        assert_eq!(settled(input.clone()), settled_prefix);
        input.push(result("running"));
        assert_eq!(settled(input.clone()), input);
    }
}
