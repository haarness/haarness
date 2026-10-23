//! MCP resources a person mentions with `@` in a message.
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Server capability for [`ResourceMention`]s on `StartTurnRequest`,
/// `QueueInputRequest` and `ExecuteCommandRequest`. Older servers ignore the
/// field, so a client offers resources in its `@` list only when the server
/// says it reads them.
pub const RESOURCE_MENTIONS_CAPABILITY: &str = "mcp-resource-mentions.v1";

/// The most resources one message may mention.
pub const MAX_RESOURCE_MENTIONS: usize = 50;

/// An MCP resource the person picked with `@`. The server reads it from its
/// MCP server when the message arrives and adds it to the message's text as
/// a block under `MCP resource: …` (text cut with the usual note when long,
/// images and PDFs as attachments), so the message keeps what was read. A
/// resource that cannot be read is named in the message with the reason,
/// and the rest of the message is still sent. Picking one is the person's
/// choice, as attaching a file is: no permission is asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceMention {
    /// The MCP server to read it from, as configured. Two servers may expose
    /// the same URI; each is its own mention.
    pub server: String,
    pub uri: String,
    /// The name the server lists it under, shown with it in the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
