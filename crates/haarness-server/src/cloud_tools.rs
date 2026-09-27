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
                "Launch and manage Haarness Cloud sessions on Fly.io using the user's signed-in Haarness account. Requires haarness login, Desktop Sign in to Haarness, or HAARNESS_API_KEY with the needed Cloud permissions; never ask for tokens. Each session owns its configuration, persistent files and conversations. Cloud agents are OPTIONAL reusable templates. create and ask can launch with inline configuration and no cloudAgentId; when supplied, cloudAgentId supplies defaults overridden by configuration. For freestyle work, prefer ask with message, title, and optional configuration, omitting sessionId and cloudAgentId. ask creates, starts, and messages the new session in one call. create only prepares/starts a session; it does not send a message. Use title to name sessions; name is only for save_agent. Remote sessions already have native bash/read/write tools: use permissions.bash for shell policy, never add an MCP shell server just to run commands. To reuse a template, first list/get it with cloud_agent_manage and pass its returned id as cloudAgentId. When the user wants an answer or work performed, use ask with cloudAgentId and message, just as for freestyle work. A create result with messageSubmitted=false has not performed the requested work; follow it with ask using the returned sessionId and await the answer. Use start=false with create to prepare a stopped session and ssh_public_key to get its deploy key. update requires sessionId, version from get, and configuration; omitted fields are preserved, supplied maps replace ({} clears), null resets optional values. Stop and wait until stopped before update, then start to apply. save_agent copies the last saved settings into a reusable template, requires name and version, and does not copy environment secrets; it creates no compute. MCP supports command/args/environment or url/headers, with ${VARIABLE} encrypted environment references. skills maps names to {files: a map containing SKILL.md and optional supporting files}; agents maps names to {prompt,description,mode,model,tools,permission}. Use Cloud UI/API for secrets. ask sends a message to sessionId, or creates a new session when sessionId is omitted. Existing stopped sessions require explicit start; reads never start compute. Use cloud_chat to list/create/manage separate chats sharing this session filesystem. threadId is a chat ID. Omit it only if there is at most one root conversation; multiple chats require an explicit ID. wait waits for turnId or the active turn; messages reads history; abort interrupts only the specified turn/conversation. create/start/ask wait by default, bounded by timeoutSeconds; waits follow live state/completion/approval events and recover with snapshots after disconnection; wait=false returns after acceptance (ask still waits for startup so its message can be submitted). If startup times out with messageSubmitted=false, retry ask with that sessionId once running. A running result at timeout means work continues: use wait with the returned IDs, never resubmit. needs_attention requires the user to open the returned URL for normal permission approval. No remote permissions are auto-approved. Cancelling a synchronous ask best-effort interrupts its submitted turn but preserves the session. stop releases compute and preserves disk; delete permanently removes the session and disk. Archive only hides it and does not stop billing. Only create, send, change, stop or delete when requested by the user. Do not replace local task/session tools with cloud work without user intent.",
                json!({
                    "action":{"type":"string","enum":["list","get","create","start","stop","rename","update","save_agent","ssh_public_key","archive","delete","ask","messages","wait","abort"]},
                    "sessionId":id,"cloudAgentId":{"type":"string","format":"uuid","description":"Optional reusable template ID from cloud_agent_manage list/get. Omit for a freestyle session with inline configuration."},"threadId":id,"turnId":id,
                    "configuration":configuration_schema(),
                    "version":{"type":"integer","minimum":1},
                    "name":{"type":"string","minLength":1,"maxLength":128,"description":"save_agent only: name of the new reusable template. To name a session, use title."},
                    "description":{"type":"string","maxLength":4096},
                    "start":{"type":"boolean","description":"create only: false prepares a stopped session for SSH setup."},
                    "title":{"type":"string","minLength":1,"maxLength":200,"description":"Session name for create/ask, or the new title for rename. Use this field to name a session."},
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
                "Manage reusable Haarness Cloud agent templates using the user's signed-in Haarness account. Requires haarness login, Desktop Sign in to Haarness, or HAARNESS_API_KEY with the needed Cloud permissions; a model-provider login alone is insufficient. list/get inspect templates, ssh_public_key returns only the public deploy key, models discovers priced available Cloud model IDs/capabilities. Never invent model IDs or reasoning levels: use models first, or omit defaults. create requires name; update requires id and the version returned by get, and merges supplied configuration fields while preserving omitted settings/environment. Saving a template starts no machine; cloud_agent_session launches it. Template edits/deletion do not change existing sessions. Only mutate templates when requested. Private keys and write-only environment values are managed in the Cloud UI/API, never requested or returned here. repository accepts empty or git with URL/ref; SSH repositories require trusted knownHosts and the public deploy key installed at the Git host. startupCommand runs on every boot and must be idempotent. Session compute/disk billing and plan limits apply. Use local agent_manage for local agent personas.",
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

/// Separate chat discovery prevents a second conversation from allocating another machine.
pub(crate) struct CloudChatTool(pub Arc<dyn ManagementRegistry>);
#[async_trait]
impl Tool for CloudChatTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "cloud_chat".into(),
            description: "Manage chats inside a running Haarness Cloud session. sessionId identifies the machine/shared filesystem; chatId identifies one root conversation (runtime threadId); turnId identifies one response. All chats share files, compute and session configuration, but have separate histories. list discovers chat IDs; create adds a fresh empty chat without allocating or starting compute. get reads chat metadata; rename changes only the chat title; archive hides/restores only that chat and does not stop compute. ask/messages/wait/abort require explicit chatId. ask sends one message; wait observes work without resubmission; abort interrupts only the targeted chat/turn. Use cloud_agent_session for session lifecycle and freestyle or template launches; cloud_agent_manage for templates. Stopped sessions require explicit start. Waits use live events; timeout means work continues. Permissions require normal user approval. Only create, send, rename, archive or interrupt when requested.".into(),
            input_schema: json!({"type":"object","additionalProperties":false,"required":["action","sessionId"],"properties":{
                "action":{"type":"string","enum":["list","get","create","rename","archive","ask","messages","wait","abort"]},
                "sessionId":{"type":"string","format":"uuid","description":"Cloud machine/workspace session ID, not a chat ID."},
                "chatId":{"type":"string","format":"uuid","description":"Chat ID from list/create. Required except for list/create. Same value as runtime threadId."},
                "turnId":{"type":"string","format":"uuid"},
                "title":{"type":"string","minLength":1,"maxLength":200},
                "message":{"type":"string","minLength":1,"maxLength":32768},
                "reasoningEffort":{"type":"string"},"archived":{"type":"boolean"},"wait":{"type":"boolean"},
                "timeoutSeconds":{"type":"integer","minimum":1,"maximum":600},
                "cursor":{"type":"string","format":"uuid"},"limit":{"type":"integer","minimum":1,"maximum":100},"after":{"type":"integer","minimum":0}
            }}),
        }
    }
    fn side_effectful(&self) -> bool {
        false
    }
    fn parallel_safe(&self, args: &Value) -> bool {
        matches!(
            args["action"].as_str(),
            Some("list" | "get" | "messages" | "wait")
        )
    }
    async fn execute(&self, args: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        self.0.execute("cloud_chat", args, context).await
    }
}

pub(crate) struct CloudScheduleTool(pub Arc<dyn ManagementRegistry>);
#[async_trait]
impl Tool for CloudScheduleTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
  name:"cloud_schedule".into(),
  description:"Schedule prompts on the Haarness Cloud worker, independent of this computer. DIFFERENT from schedule, which installs local device timers. Always use cloud_schedule when the user requests Cloud scheduling. A Cloud schedule targets an existing sessionId (machine) and chatId (conversation); first use cloud_agent_session and cloud_chat to launch/list/create them if needed. It can wake a stopped session, and normal session compute/storage and router usage charges apply. create requires name, prompt, sessionId, chatId, and at (RFC3339 with explicit timezone offset) and/or everySeconds (60..31536000). at is the first run; everySeconds repeats. Missed ticks coalesce; runs do not overlap. list/get/history inspect schedules and durable run outcomes, including chatId/turnId. update/pause/resume require version from get. run triggers one immediate run; delete cancels future and unsubmitted runs, not an already-running turn. Use cloud_chat abort to interrupt that turn. Session deletion cancels its schedules; archiving a chat prevents future scheduled submissions. Permission approvals are never automatic. Only schedule, run or change future work when the user requests it.".into(),
  input_schema:json!({"type":"object","additionalProperties":false,"required":["action"],"properties":{
   "action":{"type":"string","enum":["list","get","create","update","pause","resume","run","history","delete"]},
   "id":{"type":"string","format":"uuid"},"sessionId":{"type":"string","format":"uuid"},"chatId":{"type":"string","format":"uuid"},
   "name":{"type":"string","minLength":1,"maxLength":200},"prompt":{"type":"string","minLength":1,"maxLength":32768},
   "at":{"type":"string","format":"date-time"},"everySeconds":{"type":"integer","minimum":60,"maximum":31536000},
   "version":{"type":"integer","minimum":1},"enabled":{"type":"boolean"},"cursor":{"type":"string","format":"uuid"},"limit":{"type":"integer","minimum":1,"maximum":100}
  }})
 }
    }
    fn side_effectful(&self) -> bool {
        false
    }
    fn parallel_safe(&self, args: &Value) -> bool {
        matches!(args["action"].as_str(), Some("list" | "get" | "history"))
    }
    async fn execute(&self, args: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        self.0.execute("cloud_schedule", args, context).await
    }
}
