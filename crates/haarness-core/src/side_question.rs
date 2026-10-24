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
        // What came since the turn's request (a tool's screenshots) can pass
        // the provider's image limits; within them nothing changes.
        crate::image_budget::bound(
            &mut request.messages,
            snapshot.provider.image_limits(&request.model),
        );
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

/// A side question as the model reads it. The first question of a side chat
/// carries the instructions; its follow-ups are the question alone, since the
/// instructions are already in the request before them.
pub(crate) fn prompt(question: &str, first: bool) -> String {
    let question = question.trim();
    if !first {
        return format!("<btw-side-question>\n{question}\n</btw-side-question>");
    }
    format!(
        "<btw-side-question>\n{question}\n</btw-side-question>\n\nAnswer using the supplied conversation snapshot. This is an isolated read-only side question. Tool schemas describe the main task but every tool call here is refused. Do not invoke tools or continue the main task. Return only the answer, concise unless the question needs more. More side questions may follow; earlier ones and your answers are context for the next."
    )
}

/// The system prompt: the agent's scaffold plus standing instructions, with
/// `ultracode` the Ultracode section of an Ultracode turn. The memory index
/// and hook context change between turns, so they reach the model as stored
/// turn reminders instead (see `turn_context`).
pub(crate) fn system(
    mut base: String,
    project: &str,
    ultracode: Option<&str>,
    structured: bool,
) -> String {
    if !project.is_empty() {
        base.push_str("\n\nProject and global instructions:\n");
        base.push_str(project);
    }
    if let Some(section) = ultracode {
        base.push_str("\n\n");
        base.push_str(section);
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
    /// Takes two images in a request and is never asked anything.
    struct TwoImages;

    #[async_trait::async_trait]
    impl ModelProvider for TwoImages {
        fn id(&self) -> &str {
            "fixture"
        }
        fn info(&self) -> haarness_protocol::ProviderInfo {
            haarness_protocol::ProviderInfo {
                id: "fixture".into(),
                name: "Fixture".into(),
                authenticated: true,
                models: Vec::new(),
            }
        }
        fn image_limits(&self, _model: &str) -> crate::ImageLimits {
            crate::ImageLimits {
                count: Some(2),
                ..crate::ImageLimits::default()
            }
        }
        async fn stream(
            &self,
            _: ProviderRequest,
            _: tokio_util::sync::CancellationToken,
        ) -> crate::CoreResult<crate::ProviderStream> {
            unreachable!("a prepared request is only read")
        }
    }

    #[test]
    fn a_side_question_on_a_prepared_request_holds_its_images_to_the_limits() {
        let shot = |id: &str| ProviderMessage::ToolResult {
            call_id: id.into(),
            name: "screenshot".into(),
            output: "Screenshot taken".into(),
            is_error: false,
            attachments: vec![format!("data:image/png;name={id}.png;base64,AAAA")],
        };
        let source = vec![text("Look"), call("a"), shot("a")];
        let request = ProviderRequest {
            model: "fixture".into(),
            system: String::new(),
            messages: source.clone(),
            tools: Vec::new(),
            reasoning_effort: None,
            output_schema: None,
            parameters: crate::RequestParameters::default(),
        };
        let (thread, turn) = (ThreadId::new(), TurnId::new());
        let mut prepared = PreparedPrompts::default();
        prepared.remember(
            thread,
            turn,
            Arc::new(TwoImages),
            source.clone(),
            Vec::new(),
            request,
        );
        // Two more screenshots came since the turn's request was sent.
        let mut messages = source;
        messages.extend([call("b"), shot("b"), call("c"), shot("c")]);
        let (_, request) = prepared
            .request(thread, Some(turn), Vec::new(), &messages)
            .expect("the prepared request is reused");
        let sent = request
            .messages
            .iter()
            .filter(|message| {
                matches!(message, ProviderMessage::ToolResult { attachments, .. } if !attachments.is_empty())
            })
            .count();
        assert_eq!(sent, 1, "three images pass two; the oldest two are noted");
        assert!(
            matches!(&request.messages[2], ProviderMessage::ToolResult { output, attachments, .. }
                if attachments.is_empty() && output.contains("[Image a.png omitted"))
        );
    }

    #[test]
    fn only_a_side_chats_first_question_carries_the_instructions() {
        let first = prompt("  What changed?\n", true);
        assert!(first.starts_with("<btw-side-question>\nWhat changed?\n</btw-side-question>\n\n"));
        assert!(first.contains("every tool call here is refused"));
        assert_eq!(
            prompt("And why?", false),
            "<btw-side-question>\nAnd why?\n</btw-side-question>"
        );
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
