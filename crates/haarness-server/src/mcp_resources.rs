//! The model's tools for MCP resources: the documents, records and other data
//! connected servers expose beside their tools. They are offered only while a
//! connected server declares the resources capability, so a session whose
//! servers expose tools only keeps the tool list it had.

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use haarness_core::{
    CoreError, CoreResult, McpRegistry, Tool, ToolContext, ToolDefinition, ToolOutput,
};
use serde_json::{Value, json};
use std::sync::Arc;

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
        resource_output(&result, &crate::tools::tool_output_dir()).await
    }
}

/// What the model gets for a `resources/read` result: its text, cut with a
/// note when long (the whole text saved in `directory`); images and PDFs as
/// attachments; other binary content by its type and size, never as base64
/// text.
async fn resource_output(result: &Value, directory: &std::path::Path) -> CoreResult<ToolOutput> {
    let contents = result
        .get("contents")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let labelled = contents.len() > 1;
    let mut parts = Vec::new();
    let mut attachments = Vec::new();
    let mut attached = 0usize;
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
                    attached = attached.saturating_add(bytes.len());
                    if attached > MAX_ATTACHMENT_BYTES {
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
    let mut output = crate::tools::truncate_output_at(parts.join("\n\n"), directory).await?;
    if !attachments.is_empty() {
        if !output.metadata.is_object() {
            output.metadata = json!({});
        }
        output.metadata["attachments"] = json!(attachments);
    }
    Ok(output)
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
                "docs://archive" => Ok(
                    json!({"contents": [{"uri": uri, "mimeType": "application/zip", "blob": STANDARD.encode([0u8; 300])}]}),
                ),
                "docs://big" => {
                    Ok(json!({"contents": [{"uri": uri, "text": "line\n".repeat(30_000)}]}))
                }
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
}
