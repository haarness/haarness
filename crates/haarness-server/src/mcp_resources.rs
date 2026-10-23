//! The model's tools for MCP resources: the documents, records and other data
//! connected servers expose beside their tools. They are offered only while a
//! connected server declares the resources capability, so a session whose
//! servers expose tools only keeps the tool list it had. A person's own `@`
//! mention of a resource is read here too, into the message it was sent with.

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use haarness_core::{
    CoreError, CoreResult, McpRegistry, Tool, ToolContext, ToolDefinition, ToolOutput,
};
use haarness_protocol::{MAX_RESOURCE_MENTIONS, ResourceMention};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub(crate) const LIST: &str = "mcp_resource_list";
pub(crate) const READ: &str = "mcp_resource_read";

const LIST_DESCRIPTION: &str = "List the documents, records and other data that connected MCP servers expose as resources. Use this when the user refers to something that is not a local file, such as a URI with a custom scheme, then read a match with mcp_resource_read. Each entry names the server to read it from. Templates are resources addressed by a parameter such as a record id: fill in their uriTemplate placeholders before reading.";
const READ_DESCRIPTION: &str = "Read one MCP resource by server and URI. Not for local files; use read for those. Returns the whole resource rather than a slice: text as text, images and PDFs as attachments for models that take them, and other binary content by its type and size. Long text is cut with a note that says where the full text was saved.";

/// Images and documents a resource may hand the model, as `read` does.
const ATTACHABLE: [&str; 5] = [
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "application/pdf",
];
/// The decoded size of everything one read attaches, as for MCP tool media.
const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

/// Both tools, for a registry whose servers declare resources.
pub(crate) fn tools(registry: &Arc<dyn McpRegistry>) -> [Arc<dyn Tool>; 2] {
    [
        Arc::new(ListTool(registry.clone())),
        Arc::new(ReadTool(registry.clone())),
    ]
}

pub(crate) fn definitions() -> [ToolDefinition; 2] {
    [list_definition(), read_definition()]
}

fn list_definition() -> ToolDefinition {
    ToolDefinition {
        name: LIST.into(),
        description: LIST_DESCRIPTION.into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "server": {"type": "string", "description": "MCP server name as configured. Omit to list every server that has resources."}
            },
            "additionalProperties": false
        }),
    }
}

fn read_definition() -> ToolDefinition {
    ToolDefinition {
        name: READ.into(),
        description: READ_DESCRIPTION.into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "server": {"type": "string", "description": "The server the resource was listed under."},
                "uri": {"type": "string", "description": "The exact resource URI from mcp_resource_list, a filled-in uriTemplate, or the user."}
            },
            "required": ["server", "uri"],
            "additionalProperties": false
        }),
    }
}

/// The servers a call may reach: those the runtime scoped it to
/// (`__servers`, leaving out any whose tools a rule or setting takes away),
/// or else every server that declares resources.
fn reach(registry: &dyn McpRegistry, arguments: &Value) -> Vec<String> {
    arguments
        .get("__servers")
        .and_then(Value::as_array)
        .map(|servers| {
            servers
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_else(|| registry.resource_servers())
}

/// `server` when the call may reach it, otherwise an error that names the
/// servers it may reach, so the model can pick one instead of guessing.
fn resource_server(servers: &[String], server: &str) -> CoreResult<()> {
    if servers.iter().any(|name| name == server) {
        return Ok(());
    }
    Err(CoreError::Tool(format!(
        "MCP server \"{server}\" is not connected or has no resources. Servers with resources: {}.",
        servers.join(", ")
    )))
}

struct ListTool(Arc<dyn McpRegistry>);

#[async_trait]
impl Tool for ListTool {
    fn parallel_safe(&self, _arguments: &Value) -> bool {
        true
    }
    fn definition(&self) -> ToolDefinition {
        list_definition()
    }
    fn side_effectful(&self) -> bool {
        false
    }
    /// A listing of every server reads the servers the runtime named in
    /// `__servers` before its permission check, which asked about each, so
    /// a rule about one server still applies.
    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        let reachable = reach(self.0.as_ref(), &arguments);
        let servers: Vec<String> = match arguments.get("server").and_then(Value::as_str) {
            Some(server) => {
                resource_server(&reachable, server)?;
                vec![server.to_owned()]
            }
            None => reachable,
        };
        let single = arguments.get("server").is_some();
        let listing = async {
            let mut resources = Vec::new();
            let mut templates = Vec::new();
            let mut failures = Vec::new();
            for server in &servers {
                // A server that lists resources but fails to list templates
                // still lists its resources.
                let (listed, offered) =
                    futures::join!(self.0.resources(server), self.0.resource_templates(server));
                match listed {
                    Ok(listed) => resources.extend(listed.iter().map(|entry| {
                        entry_for(
                            server,
                            entry,
                            &["uri", "name", "title", "description", "mimeType", "size"],
                        )
                    })),
                    Err(error) if single => return Err(error),
                    Err(error) => {
                        failures.push(json!({"server": server, "error": error.to_string()}))
                    }
                }
                if let Ok(offered) = offered {
                    templates.extend(offered.iter().map(|entry| {
                        entry_for(
                            server,
                            entry,
                            &["uriTemplate", "name", "title", "description", "mimeType"],
                        )
                    }));
                }
            }
            Ok((resources, templates, failures))
        };
        let (mut resources, mut templates, failures) = tokio::select! {
            biased;
            () = context.cancel.cancelled() => return Err(CoreError::Cancelled),
            result = listing => result?,
        };
        let key = |entry: &Value, field: &str| {
            entry
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        resources
            .sort_by_key(|entry| (key(entry, "server"), key(entry, "name"), key(entry, "uri")));
        templates.sort_by_key(|entry| {
            (
                key(entry, "server"),
                key(entry, "name"),
                key(entry, "uriTemplate"),
            )
        });
        let mut listing = json!({"resources": resources, "templates": templates});
        if !failures.is_empty() {
            listing["failed"] = Value::Array(failures);
        }
        let text = serde_json::to_string_pretty(&listing)
            .map_err(|error| CoreError::Tool(error.to_string()))?;
        crate::tools::truncate_output(text).await
    }
}

/// One listed resource or template with the server to read it from, keeping
/// only the fields that help pick it.
fn entry_for(server: &str, entry: &Value, fields: &[&str]) -> Value {
    let mut picked = serde_json::Map::new();
    picked.insert("server".into(), server.into());
    for field in fields {
        if let Some(value) = entry.get(*field).filter(|value| !value.is_null()) {
            picked.insert((*field).into(), value.clone());
        }
    }
    Value::Object(picked)
}

struct ReadTool(Arc<dyn McpRegistry>);

#[async_trait]
impl Tool for ReadTool {
    fn parallel_safe(&self, _arguments: &Value) -> bool {
        true
    }
    fn definition(&self) -> ToolDefinition {
        read_definition()
    }
    fn side_effectful(&self) -> bool {
        false
    }
    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        let field = |name: &str| {
            arguments
                .get(name)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| CoreError::Invalid(format!("{name} is required")))
        };
        let (server, uri) = (field("server")?, field("uri")?);
        resource_server(&reach(self.0.as_ref(), &arguments), server)?;
        // Stop ends the read here, and the server is told to stop too.
        let result = tokio::select! {
            biased;
            () = context.cancel.cancelled() => return Err(CoreError::Cancelled),
            result = self.0.read_resource(server, uri) => result,
        }
        .map_err(|error| {
            let reason = match error {
                CoreError::Tool(message) => message,
                other => other.to_string(),
            };
            CoreError::Tool(format!(
                "Could not read MCP resource {server}:{uri}: {reason}"
            ))
        })?;
        resource_output(&result, &crate::tools::tool_output_dir(), &mut 0).await
    }
}

/// What a `resources/read` result holds, as the model may take it: its text
/// parts (labelled by URI when there are several); images and PDFs as data
/// URLs, while `attached` (the bytes attached so far) stays within 10 MiB;
/// other binary content by its type and size, never as base64 text.
fn resource_contents(result: &Value, attached: &mut usize) -> (String, Vec<String>) {
    let contents = result
        .get("contents")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let labelled = contents.len() > 1;
    let mut parts = Vec::new();
    let mut attachments = Vec::new();
    for content in contents {
        let uri = content
            .get("uri")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mime = content
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream");
        let text = if let Some(text) = content.get("text").and_then(Value::as_str) {
            text.to_owned()
        } else if let Some(blob) = content.get("blob").and_then(Value::as_str) {
            match STANDARD.decode(blob) {
                Err(_) => format!("[{uri}: {mime} content that is not valid base64]"),
                Ok(bytes) if ATTACHABLE.contains(&mime) => {
                    *attached = attached.saturating_add(bytes.len());
                    if *attached > MAX_ATTACHMENT_BYTES {
                        format!(
                            "[{uri}: {mime}, {} bytes, left out: a read attaches at most 10 MiB]",
                            bytes.len()
                        )
                    } else {
                        attachments.push(format!("data:{mime};base64,{blob}"));
                        format!("[{mime} attachment: {uri}]")
                    }
                }
                Ok(bytes) => format!("[{uri}: {mime}, {} bytes]", bytes.len()),
            }
        } else {
            continue;
        };
        parts.push(if labelled && !uri.is_empty() {
            format!("{uri}\n{text}")
        } else {
            text
        });
    }
    if parts.is_empty() {
        parts.push("The resource is empty.".into());
    }
    (parts.join("\n\n"), attachments)
}

/// What the model gets for a `resources/read` result: its contents, the text
/// cut with a note when long (the whole text saved in `directory`).
async fn resource_output(
    result: &Value,
    directory: &std::path::Path,
    attached: &mut usize,
) -> CoreResult<ToolOutput> {
    let (text, attachments) = resource_contents(result, attached);
    let mut output = crate::tools::truncate_output_at(text, directory).await?;
    if !attachments.is_empty() {
        if !output.metadata.is_object() {
            output.metadata = json!({});
        }
        output.metadata["attachments"] = json!(attachments);
    }
    Ok(output)
}

/// How long all the reads of one message's mentions may take together. It
/// stays well under how long clients wait for a send, so a slow server
/// never costs the message.
pub(crate) const MENTION_BUDGET: Duration = Duration::from_secs(10);
/// The most text one mentioned resource adds to a message, and all of them
/// together; the whole of a longer one is saved for the model to read.
const MENTION_BYTES: usize = 50 * 1024;
const MENTION_LINES: usize = 2_000;
const MENTIONS_BYTES: usize = 128 * 1024;
/// Why a mention was not read when a rule keeps it from the chat. Clients
/// show it in the person's language.
pub(crate) const RULE_REFUSAL: &str = "a rule keeps this resource from the model";

/// Reads the resources a person mentioned with `@` into their message, as
/// `mcp_resource_read` reads one for the model, all within `budget`: each
/// becomes a block after the text, under `MCP resource: <name> ·
/// <server>:<uri>`, its text fenced (cut past 50 KiB, and past 128 KiB for
/// all of them, with a note that says where the whole was saved), and its
/// images and PDFs join `attachments` (10 MiB together). A resource the
/// chat's rules keep from it (`denied`), one that cannot be read or does not
/// answer within `budget`, or an image that is not one, is named in its block
/// as not read, with the reason, and the message goes on without it. No ask
/// is asked: the person picked it. The same server and URI twice is read
/// once; two servers with one URI are two mentions.
pub(crate) async fn read_mentions(
    registry: &dyn McpRegistry,
    workspace: &Path,
    mentions: Vec<ResourceMention>,
    denied: &(dyn Fn(&str, &str) -> bool + Sync),
    budget: Duration,
    text: &mut String,
    attachments: &mut Vec<String>,
) -> CoreResult<()> {
    if mentions.is_empty() {
        return Ok(());
    }
    if mentions.len() > MAX_RESOURCE_MENTIONS {
        return Err(CoreError::Invalid(format!(
            "A message mentions at most {MAX_RESOURCE_MENTIONS} MCP resources"
        )));
    }
    let mut unique: Vec<ResourceMention> = Vec::new();
    for mention in mentions {
        if mention.server.is_empty() || mention.uri.is_empty() {
            return Err(CoreError::Invalid(
                "An MCP resource mention needs a server and a URI".into(),
            ));
        }
        if !unique
            .iter()
            .any(|seen| seen.server == mention.server && seen.uri == mention.uri)
        {
            unique.push(mention);
        }
    }
    let deadline = tokio::time::Instant::now() + budget;
    let reads = futures::future::join_all(unique.iter().map(|mention| async move {
        if denied(&mention.server, &mention.uri) {
            return Err(RULE_REFUSAL.to_owned());
        }
        // A read past the budget is dropped, and the MCP server told so.
        match tokio::time::timeout_at(
            deadline,
            registry.read_resource(&mention.server, &mention.uri),
        )
        .await
        {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(sentence(error)),
            Err(_) => Err(format!(
                "did not answer within {} s",
                budget.as_secs().max(1)
            )),
        }
    }))
    .await;
    let directory = crate::tools::tool_output_dir();
    let mut attached = 0usize;
    let mut room = MENTIONS_BYTES;
    let mut blocks = Vec::new();
    for (mention, read) in unique.iter().zip(reads) {
        let read = match read {
            Ok(result) => {
                let (body, media) = resource_contents(&result, &mut attached);
                match admitted(media, workspace).await {
                    Ok(media) => Ok((body, media)),
                    Err(error) => Err(sentence(error)),
                }
            }
            Err(reason) => Err(reason),
        };
        blocks.push(match read {
            Ok((body, media)) => {
                attachments.extend(media);
                let body = cut(body, MENTION_BYTES.min(room), &directory).await?;
                room = room.saturating_sub(body.len());
                mention_block(mention, &body, false)
            }
            Err(reason) => {
                tracing::info!(
                    server = %mention.server,
                    uri = %mention.uri,
                    "a mentioned MCP resource was not read: {reason}"
                );
                mention_block(mention, &reason, true)
            }
        });
    }
    let typed = text.trim_end();
    *text = std::iter::once(typed)
        .filter(|typed| !typed.is_empty())
        .chain(blocks.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(())
}

/// A mentioned resource's text within `room` bytes (and 2,000 lines): the
/// first whole lines that fit, then a note that says how much is left out
/// and where the whole text was saved for the model to read.
async fn cut(text: String, room: usize, directory: &Path) -> CoreResult<String> {
    if text.len() <= room && text.lines().count() <= MENTION_LINES {
        return Ok(text);
    }
    let mut kept = 0;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        if index == MENTION_LINES || kept + line.len() > room {
            break;
        }
        kept += line.len();
    }
    // A first line longer than the room is cut inside it.
    if kept == 0 {
        kept = room.min(text.len());
        while !text.is_char_boundary(kept) {
            kept -= 1;
        }
    }
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(|error| CoreError::Tool(error.to_string()))?;
    let path = directory.join(format!("resource_{}.txt", uuid::Uuid::now_v7()));
    tokio::fs::write(&path, &text)
        .await
        .map_err(|error| CoreError::Tool(error.to_string()))?;
    Ok(format!(
        "{}\n[Only the first {kept} of {} bytes are here: a message holds up to 50 KiB of one resource and 128 KiB of all it mentions. The whole resource is saved at {}; read it there with offset/limit.]",
        text[..kept].trim_end(),
        text.len(),
        path.display()
    ))
}

/// A read's attachments, each checked as a message's own attachments are,
/// so one a model cannot take fails this mention only rather than the whole
/// message.
async fn admitted(media: Vec<String>, workspace: &Path) -> CoreResult<Vec<String>> {
    haarness_core::attachments::admit_off_executor(media, workspace.to_owned()).await
}

/// Why a read failed, as a sentence without the category the error type
/// adds ("conflict: …").
fn sentence(error: CoreError) -> String {
    match error {
        CoreError::Tool(message)
        | CoreError::Conflict(message)
        | CoreError::NotFound(message)
        | CoreError::Invalid(message)
        | CoreError::PermissionDenied(message) => message,
        other => other.to_string(),
    }
}

/// One mention as the message carries it: a fenced block under its label,
/// which every client folds the way it folds an attached file. The label is
/// escaped as Desktop escapes a file context's. One not read holds the
/// reason alone.
fn mention_block(mention: &ResourceMention, body: &str, failed: bool) -> String {
    let address = format!("{}:{}", mention.server, mention.uri);
    let label = match mention.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() && name != mention.uri => format!("{name} · {address}"),
        _ => address,
    };
    let label = label
        .replace(['\n', '\r'], " ")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    let longest = body.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    let note = if failed { " (not read)" } else { "" };
    format!(
        "<details>\n<summary>MCP resource: {label}{note}</summary>\n\n{fence}\n{body}\n{fence}\n\n</details>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use haarness_protocol::{McpConfigureRequest, McpServerInfo, ThreadId, TurnId};
    use std::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    /// Two servers with resources (`docs` and `notes`, whose template
    /// listing fails) and one without (`tools`). Reading `slow://` never
    /// answers.
    #[derive(Default)]
    struct Resources {
        reads: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl McpRegistry for Resources {
        fn tool_snapshot(&self) -> Vec<ToolDefinition> {
            Vec::new()
        }
        fn resource_servers(&self) -> Vec<String> {
            vec!["docs".into(), "notes".into()]
        }
        async fn list(&self) -> CoreResult<Vec<McpServerInfo>> {
            Ok(Vec::new())
        }
        async fn configure(&self, _: &str, _: McpConfigureRequest) -> CoreResult<McpServerInfo> {
            unreachable!()
        }
        async fn reconnect(&self, _: &str) -> CoreResult<McpServerInfo> {
            unreachable!()
        }
        async fn disconnect(&self, _: &str) -> CoreResult<McpServerInfo> {
            unreachable!()
        }
        async fn tools(&self, _: &str) -> CoreResult<Vec<ToolDefinition>> {
            Ok(Vec::new())
        }
        async fn call(&self, _: &str, _: &str, _: Value) -> CoreResult<ToolOutput> {
            unreachable!()
        }
        async fn resources(&self, name: &str) -> CoreResult<Vec<Value>> {
            Ok(match name {
                "docs" => vec![
                    json!({"uri": "docs://setup", "name": "setup", "mimeType": "text/markdown", "annotations": {"priority": 1}}),
                    json!({"uri": "docs://api", "name": "api", "description": "API notes"}),
                ],
                _ => vec![json!({"uri": "notes://today", "name": "today"})],
            })
        }
        async fn resource_templates(&self, name: &str) -> CoreResult<Vec<Value>> {
            match name {
                "docs" => Ok(vec![
                    json!({"uriTemplate": "docs://page/{id}", "name": "page"}),
                ]),
                _ => Err(CoreError::Tool("templates unavailable".into())),
            }
        }
        async fn read_resource(&self, name: &str, uri: &str) -> CoreResult<Value> {
            self.reads.lock().unwrap().push(format!("{name}:{uri}"));
            let png = STANDARD.encode(b"\x89PNG\r\n\x1a\nfixture");
            match uri {
                "slow://" => std::future::pending().await,
                "docs://setup" => Ok(
                    json!({"contents": [{"uri": uri, "mimeType": "text/markdown", "text": "# Setup\nRun it."}]}),
                ),
                "docs://logo" => {
                    Ok(json!({"contents": [{"uri": uri, "mimeType": "image/png", "blob": png}]}))
                }
                "docs://pixel" => {
                    Ok(json!({"contents": [{"uri": uri, "mimeType": "image/png", "blob": PIXEL}]}))
                }
                "docs://fenced" => Ok(
                    json!({"contents": [{"uri": uri, "mimeType": "text/markdown", "text": "```sh\nrun\n```"}]}),
                ),
                "docs://archive" => Ok(
                    json!({"contents": [{"uri": uri, "mimeType": "application/zip", "blob": STANDARD.encode([0u8; 300])}]}),
                ),
                "docs://big" => {
                    Ok(json!({"contents": [{"uri": uri, "text": "line\n".repeat(30_000)}]}))
                }
                "docs://wide" => Ok(
                    json!({"contents": [{"uri": uri, "text": format!("{}\n", "w".repeat(99)).repeat(1_500)}]}),
                ),
                _ => Err(CoreError::Tool(
                    "MCP error -32002: Resource not found".into(),
                )),
            }
        }
        async fn prompts(&self, _: &str) -> CoreResult<Vec<Value>> {
            Ok(Vec::new())
        }
        async fn get_prompt(&self, _: &str, _: &str, _: Value) -> CoreResult<Value> {
            unreachable!()
        }
    }

    /// A 1×1 PNG, which a message takes as an image.
    const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNg+M/AAAAFAAIB5Sc96QAAAABJRU5ErkJggg==";

    fn context() -> ToolContext {
        ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            workspace: std::env::temp_dir(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        }
    }

    #[tokio::test]
    async fn listing_every_server_names_each_entry_with_its_server_in_a_stable_order() {
        let registry: Arc<dyn McpRegistry> = Arc::new(Resources::default());
        let [list, _] = tools(&registry);
        let output = list.execute(json!({}), context()).await.unwrap();
        let listing: Value = serde_json::from_str(&output.text).unwrap();
        assert_eq!(
            listing["resources"],
            json!([
                {"server": "docs", "uri": "docs://api", "name": "api", "description": "API notes"},
                {"server": "docs", "uri": "docs://setup", "name": "setup", "mimeType": "text/markdown"},
                {"server": "notes", "uri": "notes://today", "name": "today"},
            ])
        );
        // The failed template listing of `notes` leaves its resources listed.
        assert_eq!(
            listing["templates"],
            json!([{"server": "docs", "uriTemplate": "docs://page/{id}", "name": "page"}])
        );
        // The servers the runtime scoped the call to are all it reaches.
        let scoped = list
            .execute(json!({"__servers": ["notes"]}), context())
            .await
            .unwrap();
        let listing: Value = serde_json::from_str(&scoped.text).unwrap();
        assert_eq!(listing["resources"].as_array().unwrap().len(), 1);
        let error = read_error(
            &registry,
            json!({"server": "docs", "uri": "docs://setup", "__servers": ["notes"]}),
        )
        .await;
        assert!(error.contains("Servers with resources: notes."), "{error}");
        let error = list
            .execute(json!({"server": "tools"}), context())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("\"tools\" is not connected or has no resources. Servers with resources: docs, notes."),
            "{error}"
        );
    }

    async fn read_error(registry: &Arc<dyn McpRegistry>, arguments: Value) -> String {
        let [_, read] = tools(registry);
        read.execute(arguments, context())
            .await
            .unwrap_err()
            .to_string()
    }

    #[tokio::test]
    async fn a_read_returns_text_images_and_named_binaries_and_says_why_it_failed() {
        let registry: Arc<dyn McpRegistry> = Arc::new(Resources::default());
        let [_, read] = tools(&registry);
        let text = read
            .execute(json!({"server": "docs", "uri": "docs://setup"}), context())
            .await
            .unwrap();
        assert_eq!(text.text, "# Setup\nRun it.");
        let image = read
            .execute(json!({"server": "docs", "uri": "docs://logo"}), context())
            .await
            .unwrap();
        assert_eq!(image.text, "[image/png attachment: docs://logo]");
        assert!(
            image.metadata["attachments"][0]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        let archive = read
            .execute(
                json!({"server": "docs", "uri": "docs://archive"}),
                context(),
            )
            .await
            .unwrap();
        assert_eq!(archive.text, "[docs://archive: application/zip, 300 bytes]");
        assert!(archive.metadata.get("attachments").is_none());
        let saved =
            std::env::temp_dir().join(format!("haarness-resource-{}", uuid::Uuid::now_v7()));
        let big = resource_output(
            &registry.read_resource("docs", "docs://big").await.unwrap(),
            &saved,
            &mut 0,
        )
        .await
        .unwrap();
        assert!(
            big.text
                .contains("output was truncated. Full output saved to:"),
            "{}",
            big.text
        );
        assert_eq!(big.metadata["truncated"], true);
        assert_eq!(
            std::fs::read_to_string(big.metadata["outputPath"].as_str().unwrap()).unwrap(),
            "line\n".repeat(30_000)
        );
        std::fs::remove_dir_all(saved).unwrap();
        let missing = read
            .execute(json!({"server": "docs", "uri": "docs://gone"}), context())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            missing.contains(
                "Could not read MCP resource docs:docs://gone: MCP error -32002: Resource not found"
            ),
            "{missing}"
        );
    }

    #[tokio::test]
    async fn stop_ends_a_read_that_never_answers() {
        let registry: Arc<dyn McpRegistry> = Arc::new(Resources::default());
        let [_, read] = tools(&registry);
        let context = context();
        let cancel = context.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel.cancel();
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read.execute(json!({"server": "docs", "uri": "slow://"}), context),
        )
        .await
        .expect("Stop must end the read");
        assert!(matches!(result, Err(CoreError::Cancelled)), "{result:?}");
    }

    fn mention(server: &str, uri: &str, name: Option<&str>) -> ResourceMention {
        ResourceMention {
            server: server.into(),
            uri: uri.into(),
            name: name.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn mentions_are_read_into_the_message_each_in_its_own_block() {
        let registry = Resources::default();
        let mut text = "Compare these \n".to_owned();
        let mut attachments = vec!["data:image/png;base64,earlier".to_owned()];
        read_mentions(
            &registry,
            &std::env::temp_dir(),
            vec![
                mention("docs", "docs://setup", Some("setup")),
                // Another server with the same URI is its own mention…
                mention("notes", "docs://setup", None),
                // …and the same one twice is read once.
                mention("docs", "docs://setup", Some("setup")),
                mention("docs", "docs://pixel", Some("docs://pixel")),
                mention("docs", "docs://fenced", Some("<b>fenced</b>")),
            ],
            &|_: &str, _: &str| false,
            MENTION_BUDGET,
            &mut text,
            &mut attachments,
        )
        .await
        .unwrap();
        assert_eq!(
            text,
            [
                "Compare these",
                "<details>\n<summary>MCP resource: setup · docs:docs://setup</summary>\n\n```\n# Setup\nRun it.\n```\n\n</details>",
                "<details>\n<summary>MCP resource: notes:docs://setup</summary>\n\n```\n# Setup\nRun it.\n```\n\n</details>",
                "<details>\n<summary>MCP resource: docs:docs://pixel</summary>\n\n```\n[image/png attachment: docs://pixel]\n```\n\n</details>",
                // The label is escaped, and the fence outgrows the resource's own.
                "<details>\n<summary>MCP resource: &lt;b&gt;fenced&lt;/b&gt; · docs:docs://fenced</summary>\n\n````\n```sh\nrun\n```\n````\n\n</details>",
            ]
            .join("\n\n")
        );
        assert_eq!(
            *registry.reads.lock().unwrap(),
            [
                "docs:docs://setup",
                "notes:docs://setup",
                "docs:docs://pixel",
                "docs:docs://fenced"
            ]
        );
        assert_eq!(attachments.len(), 2, "{attachments:?}");
        assert!(
            attachments[1].starts_with("data:image/png"),
            "{attachments:?}"
        );
    }

    #[tokio::test]
    async fn a_mention_that_cannot_be_read_says_why_and_the_message_still_goes() {
        let registry = Resources::default();
        let mut text = String::new();
        let mut attachments = Vec::new();
        read_mentions(
            &registry,
            &std::env::temp_dir(),
            vec![
                mention("docs", "docs://gone", Some("gone")),
                // An image a model could not take fails its own mention only.
                mention("docs", "docs://logo", Some("logo")),
                mention("docs", "docs://setup", None),
            ],
            &|_: &str, _: &str| false,
            MENTION_BUDGET,
            &mut text,
            &mut attachments,
        )
        .await
        .unwrap();
        let blocks: Vec<&str> = text.split("\n\n<details>").collect();
        assert_eq!(blocks.len(), 3, "{text}");
        assert!(
            text.starts_with(
                "<details>\n<summary>MCP resource: gone · docs:docs://gone (not read)</summary>\n\n```\nMCP error -32002: Resource not found\n```"
            ),
            "{text}"
        );
        assert!(
            blocks[1].contains("MCP resource: logo · docs:docs://logo (not read)"),
            "{text}"
        );
        assert!(blocks[2].contains("# Setup"), "{text}");
        assert!(attachments.is_empty(), "{attachments:?}");
    }

    #[tokio::test]
    async fn a_message_names_at_most_fifty_resources_each_with_a_server_and_a_uri() {
        let registry = Resources::default();
        let many = (0..=MAX_RESOURCE_MENTIONS)
            .map(|index| mention("docs", &format!("docs://{index}"), None))
            .collect();
        let (mut text, mut attachments) = (String::from("hi"), Vec::new());
        let error = read_mentions(
            &registry,
            &std::env::temp_dir(),
            many,
            &|_: &str, _: &str| false,
            MENTION_BUDGET,
            &mut text,
            &mut attachments,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("at most 50"), "{error}");
        let error = read_mentions(
            &registry,
            &std::env::temp_dir(),
            vec![mention("", "docs://setup", None)],
            &|_: &str, _: &str| false,
            MENTION_BUDGET,
            &mut text,
            &mut attachments,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("a server and a URI"), "{error}");
        assert_eq!(text, "hi");
        assert!(registry.reads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_rule_or_a_slow_server_leaves_a_mention_unread_and_the_rest_goes() {
        let registry = Resources::default();
        let (mut text, mut attachments) = (String::from("Look"), Vec::new());
        let started = std::time::Instant::now();
        read_mentions(
            &registry,
            &std::env::temp_dir(),
            vec![
                mention("docs", "docs://private", Some("private")),
                mention("docs", "slow://", Some("slow")),
                mention("docs", "docs://setup", Some("setup")),
            ],
            &|_: &str, uri: &str| uri == "docs://private",
            Duration::from_millis(200),
            &mut text,
            &mut attachments,
        )
        .await
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the budget bounds the reads"
        );
        let blocks: Vec<&str> = text.split("\n\n<details>").collect();
        assert_eq!(blocks.len(), 4, "{text}");
        assert!(
            blocks[1].contains("private · docs:docs://private (not read)</summary>\n\n```\na rule keeps this resource from the model\n```"),
            "{text}"
        );
        assert!(
            blocks[2].contains(
                "slow · docs:slow:// (not read)</summary>\n\n```\ndid not answer within 1 s\n```"
            ),
            "{text}"
        );
        assert!(blocks[3].contains("# Setup"), "{text}");
        // A denied one is never asked for.
        assert_eq!(
            *registry.reads.lock().unwrap(),
            ["docs:slow://", "docs:docs://setup"]
        );
    }

    #[tokio::test]
    async fn long_resources_are_cut_to_fifty_kib_each_and_128_kib_together() {
        let registry = Resources::default();
        let (mut text, mut attachments) = (String::new(), Vec::new());
        read_mentions(
            &registry,
            &std::env::temp_dir(),
            ["docs", "notes", "wiki"]
                .into_iter()
                .map(|server| mention(server, "docs://wide", Some("wide")))
                .collect(),
            &|_: &str, _: &str| false,
            MENTION_BUDGET,
            &mut text,
            &mut attachments,
        )
        .await
        .unwrap();
        let blocks: Vec<&str> = text.split("<details>").skip(1).collect();
        assert_eq!(blocks.len(), 3);
        let kept = |block: &str| {
            block
                .split("[Only the first ")
                .nth(1)
                .and_then(|rest| rest.split(' ').next())
                .and_then(|bytes| bytes.parse::<usize>().ok())
                .unwrap_or_else(|| panic!("no note in {block:.200}"))
        };
        // Whole lines up to 50 KiB each, and the third what is left of 128 KiB.
        assert_eq!(kept(blocks[0]), MENTION_BYTES);
        assert_eq!(kept(blocks[1]), MENTION_BYTES);
        assert!(
            (20_000..MENTION_BYTES).contains(&kept(blocks[2])),
            "{}",
            kept(blocks[2])
        );
        assert!(text.len() < MENTIONS_BYTES + 3 * 1024, "{}", text.len());
        assert!(
            blocks[0].contains("of 150000 bytes are here: a message holds up to 50 KiB of one resource and 128 KiB of all it mentions. The whole resource is saved at "),
            "{:.300}",
            blocks[0]
        );
        let saved = blocks[0]
            .split("saved at ")
            .nth(1)
            .and_then(|rest| rest.split("; read it there").next())
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(saved).unwrap(),
            format!("{}\n", "w".repeat(99)).repeat(1_500)
        );
        std::fs::remove_file(saved).unwrap();
    }
}
