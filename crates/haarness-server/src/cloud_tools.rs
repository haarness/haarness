//! Cloud tool schemas stay at the server boundary; account credentials belong to the host adapter.
use crate::ManagementRegistry;
use async_trait::async_trait;
use haarness_core::{CoreResult, Tool, ToolContext, ToolDefinition, ToolOutput};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) struct CloudTool {
    session: bool,
    registry: Arc<dyn ManagementRegistry>,
}
impl CloudTool {
    pub(crate) fn new(session: bool, registry: Arc<dyn ManagementRegistry>) -> Self {
        Self { session, registry }
    }
    fn name(&self) -> &'static str {
        if self.session {
            "cloud_agent_session"
        } else {
            "cloud_agent_manage"
        }
    }
}
#[async_trait]
impl Tool for CloudTool {
    fn definition(&self) -> ToolDefinition {
        let id = json!({"type":"string","format":"uuid"});
        let (description, mut properties) = if self.session {
            (
                "Launch and manage Haarness Cloud sessions on Fly.io using the user's signed-in Haarness account. Requires haarness login (or Desktop Sign in to Haarness); never ask for tokens. Each session owns its configuration, persistent files and conversations. Cloud agents are OPTIONAL reusable templates. create and ask can launch with inline configuration and no cloudAgentId; when supplied, cloudAgentId supplies defaults overridden by configuration. Use start=false with create to prepare a stopped session and ssh_public_key to get its deploy key. update requires sessionId, version from get, and configuration; omitted fields are preserved, supplied maps replace ({} clears), null resets optional values. Stop and wait until stopped before update, then start to apply. save_agent copies the last saved settings into a reusable template, requires name and version, and does not copy environment secrets; it creates no compute. MCP supports command/args/environment or url/headers, with ${VARIABLE} encrypted environment references. skills maps names to {files: a map containing SKILL.md and optional supporting files}; agents maps names to {prompt,description,mode,model,tools,permission}. Use Cloud UI/API for secrets. ask sends a message to sessionId, or creates a new session when sessionId is omitted. Existing stopped sessions require explicit start; reads never start compute. Omit threadId to use the most recent root conversation, or create the first conversation when asking. wait waits for turnId or the active turn; messages reads history; abort interrupts only the specified turn/conversation. create/start/ask wait by default, bounded by timeoutSeconds; wait=false returns after acceptance (ask still waits for startup so its message can be submitted). If startup times out with messageSubmitted=false, retry ask with that sessionId once running. A running result at timeout means work continues: use wait with the returned IDs, never resubmit. needs_attention requires the user to open the returned URL for normal permission approval. No remote permissions are auto-approved. Cancelling a synchronous ask best-effort interrupts its submitted turn but preserves the session. stop releases compute and preserves disk; delete permanently removes the session and disk. Archive only hides it and does not stop billing. Only create, send, change, stop or delete when requested by the user. Do not replace local task/session tools with cloud work without user intent.",
                json!({
                    "action":{"type":"string","enum":["list","get","create","start","stop","rename","update","save_agent","ssh_public_key","archive","delete","ask","messages","wait","abort"]},
                    "sessionId":id,"cloudAgentId":id,"threadId":id,"turnId":id,
                    "configuration":configuration_schema(),
                    "version":{"type":"integer","minimum":1},
                    "name":{"type":"string","minLength":1,"maxLength":128},
                    "description":{"type":"string","maxLength":4096},
                    "start":{"type":"boolean","description":"create only: false prepares a stopped session for SSH setup."},
                    "title":{"type":"string","minLength":1,"maxLength":200},
                    "message":{"type":"string","minLength":1,"maxLength":32768},
                    "reasoningEffort":{"type":"string","description":"Supported reasoning level for ask; omit to preserve session defaults."},
                    "archived":{"type":"boolean","description":"Include archived in list (default false); target state for archive (default true). Set false with action archive to restore."},
                    "wait":{"type":"boolean"},
                    "timeoutSeconds":{"type":"integer","minimum":1,"maximum":600},
                    "after":{"type":"integer","minimum":0,"description":"Read messages after this sequence. Continue with returned nextAfter."}
                }),
            )
        } else {
            (
                "Manage reusable Haarness Cloud agent templates using the user's signed-in Haarness account. Requires haarness login or Desktop Sign in to Haarness; a model-provider login alone is insufficient. list/get inspect templates, ssh_public_key returns only the public deploy key, models discovers priced available Cloud model IDs/capabilities. Never invent model IDs or reasoning levels: use models first, or omit defaults. create requires name; update requires id and the version returned by get, and merges supplied configuration fields while preserving omitted settings/environment. Saving a template starts no machine; cloud_agent_session launches it. Template edits/deletion do not change existing sessions. Only mutate templates when requested. Private keys and write-only environment values are managed in the Cloud UI/API, never requested or returned here. repository accepts empty or git with URL/ref; SSH repositories require trusted knownHosts and the public deploy key installed at the Git host. startupCommand runs on every boot and must be idempotent. Session compute/disk billing and plan limits apply. Use local agent_manage for local agent personas.",
                json!({
                    "action":{"type":"string","enum":["list","get","create","update","delete","ssh_public_key","models"]},
                    "id":id,
                    "name":{"type":"string","minLength":1,"maxLength":128},
                    "description":{"type":"string","maxLength":4096},
                    "version":{"type":"integer","minimum":1},
                    "search":{"type":"string","description":"Case-insensitive name/description filter for list or models. Use returned IDs for mutations."},
                    "configuration":configuration_schema()
                }),
            )
        };
        properties["limit"] = json!({"type":"integer","minimum":1,"maximum":100,"description":"List/history page size, default 50."});
        properties["cursor"] = json!({"type":"string","format":"uuid","description":"Continue list with the returned nextCursor."});
        ToolDefinition {
            name: self.name().into(),
            description: description.into(),
            input_schema: json!({"type":"object","properties":properties,"required":["action"],"additionalProperties":false}),
        }
    }
    // Remote effects do not create a misleading local workspace undo checkpoint.
    fn side_effectful(&self) -> bool {
        false
    }
    fn parallel_safe(&self, args: &Value) -> bool {
        matches!(
            args["action"].as_str(),
            Some("list" | "get" | "models" | "ssh_public_key" | "messages" | "wait")
        )
    }
    async fn execute(&self, args: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        self.registry.execute(self.name(), args, context).await
    }
}

fn configuration_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "schemaVersion":{"type":"integer","enum":[1]},
        "mcp":{"type":"object","description":"Named MCP definitions: command/args/environment/cwd or url/headers, enabled, timeoutMs, oauth. Secrets use ${VARIABLE}."},
        "agents":{"type":"object","description":"Named agents with prompt, description, mode (primary/subagent/all), model, tools, permission, hidden and steps. cloud-agent is reserved."},
        "skills":{"type":"object","description":"Named skills with files map of relative path to UTF-8 contents; include SKILL.md. All configuration max 64 KiB."},
        "permissions":{"description":"Native permission action (allow/ask/deny), or map of tool to action/pattern map."},
        "instructions":{"type":"string","maxLength":32768},
        "model":{"type":["string","null"],"description":"Exact Cloud router model ID, for example openai/gpt-5-nano. Omit/null uses the router default."},
        "reasoningEffort":{"type":["string","null"]},
        "repository":{"type":"object","additionalProperties":false,"properties":{"kind":{"type":"string","enum":["empty","git"]},"url":{"type":"string"},"ref":{"type":["string","null"]}},"required":["kind"]},
        "knownHosts":{"type":"string","maxLength":32768},
        "startupCommand":{"type":["string","null"],"maxLength":8192},
        "region":{"type":["string","null"],"description":"Omit to use the Cloud service's available region."},
        "resourceClass":{"type":"string","enum":["shared-2cpu-4gb","performance-2cpu-4gb","performance-4cpu-8gb"]},
        "diskGb":{"type":"integer","minimum":1,"maximum":500},
        "idleTimeoutSecs":{"type":"integer","minimum":1,"maximum":86400}
    }})
}
