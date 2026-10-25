//! Account-authenticated Cloud orchestration shared by model-facing adapters.
//! No model input can choose an issuer, credential path, token or gateway URL.
use crate::{Client, Error, Result, safe_url};
use chrono::{DateTime, Utc};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_RESPONSE: usize = 8 * 1024 * 1024;
mod events;

use haarness_protocol::{
    CloudAgentAction as AgentAction, CloudAgentInput as AgentInput, CloudChatAction as ChatAction,
    CloudChatInput as ChatInput, CloudScheduleAction as ScheduleAction,
    CloudScheduleInput as ScheduleInput, CloudSessionAction as SessionAction,
    CloudSessionInput as SessionInput,
};

fn invalid(message: impl Into<String>) -> Error {
    Error::Other(message.into())
}
fn required<T>(value: Option<T>, name: &str) -> Result<T> {
    value.ok_or_else(|| invalid(format!("{name} is required for this action")))
}
fn limit(value: Option<u64>) -> Result<u64> {
    let value = value.unwrap_or(50);
    if !(1..=100).contains(&value) {
        return Err(invalid("limit must be between 1 and 100"));
    }
    Ok(value)
}
fn nonempty(value: &str, max: usize, name: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > max {
        return Err(invalid(format!("{name} must contain 1–{max} bytes")));
    }
    Ok(())
}

fn validate_configuration(configuration: Option<&Value>) -> Result<()> {
    if let Some(configuration) = configuration {
        let fields = configuration
            .as_object()
            .ok_or_else(|| invalid("configuration must be an object"))?;
        for key in fields.keys() {
            if !matches!(
                key.as_str(),
                "instructions"
                    | "model"
                    | "reasoningEffort"
                    | "repository"
                    | "knownHosts"
                    | "startupCommand"
                    | "region"
                    | "resourceClass"
                    | "diskGb"
                    | "idleTimeoutSecs"
                    | "schemaVersion"
                    | "mcp"
                    | "agents"
                    | "skills"
                    | "permissions"
            ) {
                return Err(invalid(format!(
                    "Unsupported configuration field {key}; manage environment secrets in the Cloud UI/API"
                )));
            }
        }
    }
    Ok(())
}

fn model_search_text(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Clone)]
pub struct Cloud {
    client: Client,
    account: Uuid,
    cancel: CancellationToken,
}

impl Cloud {
    /// Recheck sign-in for every tool call; servers do not cache a logged-in boolean.
    pub async fn new(client: Client, cancel: CancellationToken) -> Result<Self> {
        let status = tokio::select! {
            () = cancel.cancelled() => return Err(invalid("Cloud operation cancelled")),
            status = client.status() => status?,
        };
        if status["loggedIn"] != true {
            return Err(Error::NotLoggedIn);
        }
        let account = status["selectedAccountId"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or_else(|| invalid("Select a Haarness Cloud account before continuing"))?;
        Ok(Self {
            client,
            account,
            cancel,
        })
    }

    async fn api(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        tokio::select! {
            () = self.cancel.cancelled() => Err(invalid("Cloud operation cancelled; accepted Cloud work is retained. List sessions to inspect it.")),
            result = self.client.request(method, path, body) => result,
        }
    }

    async fn owned(&self, resource: &str, id: Uuid) -> Result<Value> {
        let value = self
            .api(Method::GET, &format!("api/v1/{resource}/{id}"), None)
            .await?;
        if value["accountId"].as_str() != Some(self.account.to_string().as_str()) {
            return Err(invalid(
                "This resource belongs to another Haarness Cloud account. Switch accounts before using it.",
            ));
        }
        Ok(value)
    }

    fn url(&self, id: Uuid, resource: &str) -> Result<String> {
        Ok(self
            .client
            .endpoint(&format!("app/{resource}/{id}"))?
            .to_string())
    }

    async fn page(
        &self,
        resource: &str,
        cursor: Option<Uuid>,
        count: u64,
        extra: &str,
    ) -> Result<Value> {
        let mut path = format!(
            "api/v1/{resource}?accountId={}&limit={count}{extra}",
            self.account
        );
        if let Some(cursor) = cursor {
            path.push_str(&format!("&cursor={cursor}"));
        }
        self.api(Method::GET, &path, None).await
    }

    pub async fn agent(&self, arguments: Value) -> Result<Value> {
        let input: AgentInput = serde_json::from_value(arguments)?;
        let count = limit(input.limit)?;
        validate_configuration(input.configuration.as_ref())?;
        match input.action {
            AgentAction::List => {
                // Search spans pages, but every result includes a continuation cursor.
                let mut cursor = input.cursor;
                let mut seen = HashSet::new();
                loop {
                    let mut page = self.page("cloud-agents", cursor, count, "").await?;
                    if let Some(search) = &input.search {
                        let search = search.to_lowercase();
                        if let Some(items) = page["items"].as_array_mut() {
                            items.retain(|row| {
                                format!(
                                    "{} {}",
                                    row["name"].as_str().unwrap_or(""),
                                    row["description"].as_str().unwrap_or("")
                                )
                                .to_lowercase()
                                .contains(&search)
                            });
                        }
                    }
                    if input.search.is_none()
                        || page["items"].as_array().is_some_and(|v| !v.is_empty())
                        || page["nextCursor"].is_null()
                    {
                        return Ok(page);
                    }
                    cursor = Some(serde_json::from_value(page["nextCursor"].clone())?);
                    if !seen.insert(cursor) {
                        return Err(invalid("Cloud pagination cursor repeated"));
                    }
                }
            }
            AgentAction::Models => {
                let discover = async {
                    let session = self.client.router_session().await?;
                    let mut url = safe_url(&session.router_url)?;
                    url = url
                        .join("models")
                        .map_err(|_| invalid("Invalid router catalogue URL"))?;
                    let response = self
                        .client
                        .http
                        .get(url)
                        .bearer_auth(&session.token)
                        .send()
                        .await?;
                    let value = response_json(response).await?;
                    let mut models = value["data"]
                        .as_array()
                        .cloned()
                        .ok_or_else(|| invalid("Invalid Cloud model catalogue"))?;
                    if let Some(search) = input.search {
                        let search = model_search_text(&search);
                        models.retain(|m| model_search_text(&m.to_string()).contains(&search));
                    }
                    // Metadata is public; the router credential stays inside this service.
                    Ok(json!({"models":models}))
                };
                tokio::select! {
                    () = self.cancel.cancelled() => Err(invalid("Cloud model discovery cancelled")),
                    result = discover => result,
                }
            }
            AgentAction::Create => {
                let name = required(input.name, "name")?;
                nonempty(&name, 128, "name")?;
                self.api(Method::POST, "api/v1/cloud-agents", Some(json!({
                    "accountId":self.account,"name":name,"description":input.description.unwrap_or_default(),
                    "configuration":input.configuration.unwrap_or_else(|| json!({}))
                }))).await
            }
            AgentAction::Get
            | AgentAction::SshPublicKey
            | AgentAction::Update
            | AgentAction::Delete => {
                let id = required(input.id, "id")?;
                let mut agent = self.owned("cloud-agents", id).await?;
                match input.action {
                    AgentAction::Get => Ok(agent),
                    AgentAction::SshPublicKey => Ok(
                        json!({"id":id,"name":agent["name"],"sshPublicKey":agent["sshPublicKey"]}),
                    ),
                    AgentAction::Delete => {
                        self.api(Method::DELETE, &format!("api/v1/cloud-agents/{id}"), None)
                            .await?;
                        Ok(json!({"id":id,"deleted":true,"existingSessionsPreserved":true}))
                    }
                    AgentAction::Update => {
                        let version = required(input.version, "version (from get)")?;
                        if let Some(name) = input.name {
                            nonempty(&name, 128, "name")?;
                            agent["name"] = json!(name);
                        }
                        if let Some(description) = input.description {
                            agent["description"] = json!(description);
                        }
                        if let Some(configuration) = input.configuration {
                            let fields = configuration
                                .as_object()
                                .ok_or_else(|| invalid("configuration must be an object"))?;
                            for (key, value) in fields {
                                agent["configuration"][key] = value.clone();
                            }
                        }
                        self.api(Method::PUT,&format!("api/v1/cloud-agents/{id}"),Some(json!({"version":version,"name":agent["name"],"description":agent["description"],"configuration":agent["configuration"]}))).await
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    /// Durable Cloud worker schedules, never device-local scheduling.
    pub async fn schedule(&self, arguments: Value) -> Result<Value> {
        let input: ScheduleInput = serde_json::from_value(arguments)?;
        let count = limit(input.limit)?;
        if matches!(input.action, ScheduleAction::List) {
            let extra = input
                .session_id
                .map(|id| format!("&sessionId={id}"))
                .unwrap_or_default();
            return self.page("schedules", input.cursor, count, &extra).await;
        }
        if matches!(input.action, ScheduleAction::Create) {
            let session = required(input.session_id, "sessionId")?;
            self.owned("sessions", session).await?;
            let name = required(input.name, "name")?;
            nonempty(&name, 200, "name")?;
            let prompt = required(input.prompt, "prompt")?;
            nonempty(&prompt, 32768, "prompt")?;
            return self.api(Method::POST,"api/v1/schedules",Some(json!({"sessionId":session,"chatId":required(input.chat_id,"chatId")?,"name":name,"prompt":prompt,"at":input.at,"everySeconds":input.every_seconds}))).await;
        }
        let id = required(input.id, "id")?;
        let row = self.owned("schedules", id).await?;
        let path = format!("api/v1/schedules/{id}");
        match input.action {
            ScheduleAction::Get => Ok(row),
            ScheduleAction::Delete => self.api(Method::DELETE, &path, None).await,
            ScheduleAction::Run => {
                self.api(Method::POST, &format!("{path}/run"), Some(json!({})))
                    .await
            }
            ScheduleAction::History => {
                self.page(&format!("schedules/{id}/runs"), input.cursor, count, "")
                    .await
            }
            ScheduleAction::Update | ScheduleAction::Pause | ScheduleAction::Resume => {
                let version = required(input.version, "version")?;
                if row["version"].as_i64() != Some(version) {
                    return Err(invalid("Schedule changed; get it again before updating"));
                }
                let enabled = match input.action {
                    ScheduleAction::Pause => Some(false),
                    ScheduleAction::Resume => Some(true),
                    _ => input.enabled,
                };
                self.api(Method::PATCH,&path,Some(json!({"version":version,"name":input.name,"prompt":input.prompt,"at":input.at,"enabled":enabled}))).await
            }
            _ => Err(invalid("Unsupported schedule action")),
        }
    }

    /// Explicit chat management scoped to one session; never provisions compute.
    pub async fn chat(&self, arguments: Value) -> Result<Value> {
        let input: ChatInput = serde_json::from_value(arguments)?;
        let count = limit(input.limit)? as usize;
        if let Some(title) = &input.title {
            nonempty(title, 200, "title")?;
        }
        let id = input.session_id;
        let requires_chat = !matches!(input.action, ChatAction::List | ChatAction::Create);
        if requires_chat && input.chat_id.is_none() {
            return Err(invalid(
                "chatId is required; list chats first or create a new chat in this session",
            ));
        }
        if !requires_chat && (input.chat_id.is_some() || input.turn_id.is_some()) {
            return Err(invalid(
                "list/create cannot target an existing chat or turn",
            ));
        }
        if matches!(input.action, ChatAction::Rename) {
            required(input.title.as_ref(), "title")?;
        }
        if matches!(input.action, ChatAction::Ask) {
            nonempty(
                required(input.message.as_deref(), "message")?,
                32768,
                "message",
            )?;
        }
        let session = self.owned("sessions", id).await?;
        if session["workspace"]["observedState"] != "running" {
            return Err(invalid(
                "Start the Cloud session before using its chats; no message was submitted",
            ));
        }
        let mut remote = Remote::new(self, id).await?;
        let url = self.url(id, "sessions")?;
        if matches!(input.action, ChatAction::List) {
            let rows = remote
                .request(
                    Method::GET,
                    &format!("v1/threads?archived={}", input.archived.unwrap_or(false)),
                    None,
                )
                .await?;
            let mut rows: Vec<Value> = rows
                .as_array()
                .ok_or_else(|| invalid("Invalid chat list"))?
                .iter()
                .filter(|r| {
                    r["parentId"].is_null()
                        && (input.archived == Some(true) || r["archived"] != true)
                })
                .cloned()
                .collect();
            // Stable identity ordering makes cursors independent of chat activity.
            rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            if let Some(cursor) = input.cursor {
                let cursor = cursor.to_string();
                rows.retain(|r| r["id"].as_str().is_some_and(|id| id > cursor.as_str()));
            }
            let next = if rows.len() > count {
                rows[count - 1]["id"].clone()
            } else {
                Value::Null
            };
            rows.truncate(count);
            return Ok(json!({"sessionId":id,"chats":rows,"nextCursor":next,"url":url}));
        }
        if matches!(input.action, ChatAction::Create) {
            let info = remote.request(Method::GET, "v1/info", None).await?;
            let chat = remote.request(Method::POST, "v1/threads", Some(json!({"title":input.title,
                "agent":info["defaultAgent"],"provider":info["defaultProvider"],"model":info["defaultModel"]}))).await?;
            return Ok(
                json!({"sessionId":id,"chatId":chat["id"],"threadId":chat["id"],"chat":chat,"url":format!("{url}?thread={}",chat["id"].as_str().unwrap_or_default())}),
            );
        }
        let chat_id = required(input.chat_id, "chatId")?;
        let path = format!("v1/threads/{chat_id}");
        let mut chat = remote.request(Method::GET, &path, None).await?;
        if !chat["parentId"].is_null() {
            return Err(invalid(
                "chatId must identify a root chat, not a subagent thread",
            ));
        }
        match input.action {
            ChatAction::Get | ChatAction::Rename | ChatAction::Archive => {
                if !matches!(input.action, ChatAction::Get) {
                    let body = if matches!(input.action, ChatAction::Rename) {
                        json!({"title":input.title})
                    } else {
                        json!({"archived":input.archived.unwrap_or(true)})
                    };
                    chat = remote.request(Method::PATCH, &path, Some(body)).await?;
                }
                Ok(
                    json!({"sessionId":id,"chatId":chat_id,"threadId":chat_id,"chat":chat,"url":format!("{url}?thread={chat_id}")}),
                )
            }
            _ => {
                let mut args = serde_json::to_value(input)?;
                let fields = args.as_object_mut().unwrap();
                fields.remove("chatId");
                // Archiving belongs to chats; sessions have no archived state.
                fields.remove("archived");
                args["threadId"] = json!(chat_id);
                let mut result = self.session(args).await?;
                result["chatId"] = json!(chat_id);
                Ok(result)
            }
        }
    }

    pub async fn session(&self, arguments: Value) -> Result<Value> {
        let input: SessionInput = serde_json::from_value(arguments)?;
        validate_configuration(input.configuration.as_ref())?;
        if matches!(input.action, SessionAction::Ask) && input.start == Some(false) {
            return Err(invalid(
                "ask requires a running session; use create with start=false to prepare one",
            ));
        }
        let count = limit(input.limit)?;
        let seconds = input.timeout_seconds.unwrap_or(120);
        if !(1..=600).contains(&seconds) {
            return Err(invalid("timeoutSeconds must be between 1 and 600"));
        }
        let deadline = Instant::now() + Duration::from_secs(seconds);
        if matches!(input.action, SessionAction::List) {
            // Sessions have no archived state: every one is listed until it is deleted.
            let extra = input
                .cloud_agent_id
                .map(|agent| format!("&cloudAgentId={agent}"))
                .unwrap_or_default();
            return self.page("sessions", input.cursor, count, &extra).await;
        }
        // Validate before provisioning: malformed asks must not allocate a machine.
        if matches!(input.action, SessionAction::Ask) {
            nonempty(
                required(input.message.as_deref(), "message")?,
                32768,
                "message",
            )?;
        }
        if let Some(title) = &input.title {
            nonempty(title, 200, "title")?;
        }
        let creating = matches!(input.action, SessionAction::Create)
            || matches!(input.action, SessionAction::Ask) && input.session_id.is_none();
        let mut session = if creating {
            if input.session_id.is_some() || input.thread_id.is_some() || input.turn_id.is_some() {
                return Err(invalid(
                    "A new session cannot use an existing sessionId, threadId or turnId",
                ));
            }
            if let Some(agent) = input.cloud_agent_id {
                self.owned("cloud-agents", agent).await?;
            }
            self.api(
                Method::POST,
                "api/v1/sessions",
                Some(json!({"accountId":self.account,"cloudAgentId":input.cloud_agent_id,"title":input.title,"configuration":input.configuration,"start":input.start})),
            )
            .await?["session"]
                .clone()
        } else {
            self.owned("sessions", required(input.session_id, "sessionId")?)
                .await?
        };
        let id: Uuid = serde_json::from_value(session["id"].clone())?;
        let path = format!("api/v1/sessions/{id}");
        let url = self.url(id, "sessions")?;
        match input.action {
            SessionAction::Get => return Ok(json!({"session":session,"url":url})),
            SessionAction::SshPublicKey => {
                return Ok(json!({"sessionId":id,"sshPublicKey":session["sshPublicKey"]}));
            }
            SessionAction::Update => {
                let version = required(input.version, "version")?;
                if session["version"].as_i64() != Some(version) {
                    return Err(invalid("Session changed; get it again before saving"));
                }
                return self.api(Method::PATCH, &path, Some(json!({"version":version,"configuration":required(input.configuration,"configuration")?}))).await;
            }
            SessionAction::SaveAgent => {
                let name = required(input.name, "name")?;
                nonempty(&name, 128, "name")?;
                return self.api(Method::POST, &format!("{path}/save-agent"), Some(json!({"name":name,"description":input.description.unwrap_or_default(),"version":required(input.version,"version")?}))).await;
            }
            SessionAction::Stop | SessionAction::Delete => {
                let deleting = matches!(input.action, SessionAction::Delete);
                let result = self
                    .api(
                        if deleting {
                            Method::DELETE
                        } else {
                            Method::POST
                        },
                        &format!("{path}{}", if deleting { "" } else { "/stop" }),
                        Some(json!({})),
                    )
                    .await?;
                return Ok(
                    json!({"sessionId":id,"url":url,"operation":result["operation"],"status":"accepted"}),
                );
            }
            SessionAction::Rename => {
                let body = json!({"title":required(input.title,"title")?});
                return self.api(Method::PATCH, &path, Some(body)).await;
            }
            SessionAction::Start => {
                self.api(Method::POST, &format!("{path}/start"), Some(json!({})))
                    .await?;
            }
            _ => {}
        }
        if creating || matches!(input.action, SessionAction::Start) {
            // An asynchronous ask still needs a ready runtime to submit its message.
            // wait=false skips waiting for the response, never silently drops the prompt.
            if input.start != Some(false)
                && (matches!(input.action, SessionAction::Ask) || input.wait.unwrap_or(true))
            {
                let workspace = session["workspace"]["id"]
                    .as_str()
                    .and_then(|id| id.parse().ok());
                session = self.ready(id, workspace, deadline).await?;
            } else {
                session = self.owned("sessions", id).await?;
            }
            if !matches!(input.action, SessionAction::Ask)
                || session["workspace"]["observedState"] != "running"
            {
                return Ok(
                    json!({"session":session,"sessionId":id,"url":url,"status":session["workspace"]["observedState"],"messageSubmitted":false,"message":"No chat message was submitted. Once running, ask using this sessionId; do not create a duplicate session."}),
                );
            }
        }
        if session["workspace"]["observedState"] != "running" {
            return Ok(
                json!({"sessionId":id,"url":url,"status":session["workspace"]["observedState"],"message":"Start the session before chatting or reading its runtime. No message was submitted."}),
            );
        }
        let mut remote = Remote::new(self, id).await?;
        let thread = if let Some(thread) = input.thread_id {
            remote
                .request(Method::GET, &format!("v1/threads/{thread}"), None)
                .await?;
            thread
        } else {
            let threads = remote.request(Method::GET, "v1/threads", None).await?;
            let roots: Vec<&Value> = threads
                .as_array()
                .into_iter()
                .flatten()
                .filter(|row| row["parentId"].is_null() && row["archived"] != true)
                .collect();
            if roots.len() > 1 {
                return Err(invalid(
                    "Multiple chats in this session; list them with cloud_chat and supply chatId (or threadId for this legacy tool)",
                ));
            }
            let candidate = roots.first();
            if let Some(thread) = candidate {
                serde_json::from_value(thread["id"].clone())?
            } else if matches!(input.action, SessionAction::Ask) {
                let info = remote.request(Method::GET, "v1/info", None).await?;
                let agent = info["defaultAgent"]
                    .as_str()
                    .or_else(|| (!session["agentSnapshot"].is_null()).then_some("cloud-agent"));
                let row=remote.request(Method::POST,"v1/threads",Some(json!({"title":input.title,"agent":agent,"provider":info["defaultProvider"],"model":info["defaultModel"]}))).await?;
                serde_json::from_value(row["id"].clone())?
            } else {
                return Ok(json!({"sessionId":id,"url":url,"status":"idle","items":[]}));
            }
        };
        let thread_path = format!("v1/threads/{thread}");
        match input.action {
            SessionAction::Messages => {
                let items = remote.items(thread, input.after, count).await?;
                Ok(json!({"sessionId":id,"threadId":thread,"url":url,"messages":items}))
            }
            SessionAction::Abort => {
                // Never allow a turn ID from a different conversation to be interrupted.
                let result = if let Some(turn) = input.turn_id {
                    remote.turn(thread, turn).await?;
                    remote
                        .request(
                            Method::POST,
                            &format!("v1/turns/{turn}/interrupt"),
                            Some(json!({})),
                        )
                        .await?
                } else {
                    remote
                        .request(
                            Method::POST,
                            &format!("{thread_path}/interrupt"),
                            Some(json!({})),
                        )
                        .await?
                };
                Ok(
                    json!({"sessionId":id,"threadId":thread,"url":url,"status":"interrupt_requested","result":result}),
                )
            }
            SessionAction::Ask | SessionAction::Wait => {
                let asking = matches!(input.action, SessionAction::Ask);
                let turn = if asking {
                    // No nested control-plane access or interactive questions in remote delegated work.
                    let effort = input.reasoning_effort.as_ref().map_or_else(
                        || session.get("configuration").unwrap_or(&session["agentSnapshot"]["configuration"])["reasoningEffort"].clone(),
                        |e| json!(e),
                    );
                    // The agent's words, marked as an agent's: Auto's reviewer there never
                    // reads them as the person's.
                    let text = input.message.as_deref().map(|message| {
                        format!("{}{message}", haarness_protocol::AGENT_MESSAGE_PREFIX)
                    });
                    remote.request(Method::POST,&format!("{thread_path}/turns"),Some(json!({"requestId":Uuid::now_v7(),"text":text,"reasoningEffort":effort,"tools":{"question":false,"cloud_agent_manage":false,"cloud_agent_session":false,"cloud_chat":false,"cloud_schedule":false}}))).await?
                } else if let Some(turn) = input.turn_id {
                    remote.turn(thread, turn).await?
                } else {
                    remote
                        .request(Method::GET, &format!("{thread_path}/active"), None)
                        .await?
                };
                if turn.is_null() {
                    return Ok(
                        json!({"sessionId":id,"threadId":thread,"url":url,"status":"idle","messages":remote.items(thread,input.after,count).await?}),
                    );
                }
                let turn_id: Uuid = serde_json::from_value(turn["id"].clone())?;
                let result = if !asking || input.wait.unwrap_or(true) {
                    remote.wait(thread, turn_id, deadline, asking).await?
                } else {
                    turn
                };
                // These cursors are sufficient to continue a timed-out or asynchronous call without resubmission.
                Ok(
                    json!({"sessionId":id,"threadId":thread,"turnId":turn_id,"url":format!("{url}?thread={thread}"),"status":result["status"],"messageSubmitted":asking,"turn":result,"messages":remote.items(thread,input.after,count).await?}),
                )
            }
            _ => Err(invalid("Unsupported session action")),
        }
    }

    async fn ready(&self, id: Uuid, workspace: Option<Uuid>, deadline: Instant) -> Result<Value> {
        let mut updates = events::Updates::control(self.clone(), id, workspace, deadline).await;
        loop {
            let session = match self.owned("sessions", id).await {
                Err(_) if self.cancel.is_cancelled() => {
                    return Err(invalid(format!(
                        "Cloud wait cancelled. Session {id} is retained; inspect or stop it explicitly."
                    )));
                }
                result => result?,
            };
            if matches!(
                session["workspace"]["observedState"].as_str(),
                Some("running" | "failed" | "deleted")
            ) || (session["workspace"]["observedState"] == "stopped"
                && session["workspace"]["desiredState"] != "running")
                || Instant::now() >= deadline
            {
                return Ok(session);
            }
            updates.wait(deadline, &self.cancel).await;
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attachment {
    gateway_url: String,
    ticket: String,
    expires_at: DateTime<Utc>,
}
struct Remote<'a> {
    cloud: &'a Cloud,
    id: Uuid,
    attachment: Attachment,
}
impl<'a> Remote<'a> {
    async fn new(cloud: &'a Cloud, id: Uuid) -> Result<Self> {
        let attachment = Self::attach(cloud, id).await?;
        Ok(Self {
            cloud,
            id,
            attachment,
        })
    }
    async fn attach(cloud: &Cloud, id: Uuid) -> Result<Attachment> {
        let attachment: Attachment = serde_json::from_value(
            cloud
                .api(
                    Method::POST,
                    &format!("api/v1/sessions/{id}/attach"),
                    Some(json!({})),
                )
                .await?,
        )?;
        safe_url(&attachment.gateway_url)?;
        Ok(attachment)
    }
    async fn request(&mut self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        if self.attachment.expires_at <= Utc::now() + chrono::Duration::seconds(30) {
            self.attachment = Self::attach(self.cloud, self.id).await?;
        }
        for attempt in 0..2 {
            let url = safe_url(&self.attachment.gateway_url)?
                .join(path)
                .map_err(|_| invalid("Invalid session path"))?;
            let mut request = self
                .cloud
                .client
                .http
                .request(method.clone(), url)
                .bearer_auth(&self.attachment.ticket);
            if let Some(body) = &body {
                request = request.json(body);
            }
            let response = tokio::select! {()=self.cloud.cancel.cancelled()=>return Err(invalid("Cloud operation cancelled")),result=request.send()=>result?};
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.attachment = Self::attach(self.cloud, self.id).await?;
                continue;
            }
            let (status, value) = tokio::select! {
                () = self.cloud.cancel.cancelled() => return Err(invalid("Cloud operation cancelled")),
                result = read_response(response) => result?,
            };
            if attempt == 0
                && method == Method::GET
                && status == StatusCode::BAD_REQUEST
                && value["error"]["code"] == "remote_project"
                && value["error"]["message"]
                    .as_str()
                    .is_some_and(|v| v.contains("workspace restarted"))
            {
                // Only repeat reads after a generation change; never replay an ask.
                self.attachment = Self::attach(self.cloud, self.id).await?;
                continue;
            }
            return checked_response(status, value);
        }
        Err(Error::NotLoggedIn)
    }
    async fn turn(&mut self, thread: Uuid, turn: Uuid) -> Result<Value> {
        let value = self
            .request(Method::GET, &format!("v1/turns/{turn}"), None)
            .await?;
        if value["threadId"].as_str() != Some(thread.to_string().as_str()) {
            return Err(invalid("turnId does not belong to threadId"));
        }
        Ok(value)
    }
    async fn items(&mut self, thread: Uuid, after: Option<u64>, count: u64) -> Result<Value> {
        let items = self
            .request(
                Method::GET,
                &format!(
                    "v1/threads/{thread}/items{}",
                    after.map(|v| format!("?after={v}")).unwrap_or_default()
                ),
                None,
            )
            .await?;
        let all = items
            .as_array()
            .ok_or_else(|| invalid("Invalid conversation response"))?;
        let take = usize::try_from(count).unwrap_or(100);
        let start = if after.is_some() {
            0
        } else {
            all.len().saturating_sub(take)
        };
        let page = all
            .iter()
            .skip(start)
            .take(take)
            .cloned()
            .collect::<Vec<_>>();
        let next = page.last().and_then(|v| v.get("sequence")).cloned();
        Ok(
            json!({"items":page,"nextAfter":next,"hasMore":start+take<all.len(),"truncatedBefore":start>0}),
        )
    }
    async fn wait(
        &mut self,
        thread: Uuid,
        turn: Uuid,
        deadline: Instant,
        abort_on_cancel: bool,
    ) -> Result<Value> {
        let mut updates = events::Updates::runtime(
            self.cloud.clone(),
            self.id,
            self.attachment.clone(),
            thread,
            turn,
            deadline,
        )
        .await;
        loop {
            if self.cloud.cancel.is_cancelled() {
                if abort_on_cancel {
                    // Best effort only for the turn submitted by this call. Cancellation must not
                    // stop another user's turn or destroy the persistent session.
                    let url = safe_url(&self.attachment.gateway_url)?
                        .join(&format!("v1/turns/{turn}/interrupt"))
                        .map_err(|_| invalid("Invalid session path"))?;
                    let _ = self
                        .cloud
                        .client
                        .http
                        .post(url)
                        .bearer_auth(&self.attachment.ticket)
                        .json(&json!({}))
                        .timeout(Duration::from_secs(3))
                        .send()
                        .await;
                }
                return Err(invalid(format!(
                    "Cloud wait cancelled. Session {} and thread {thread} are retained.",
                    self.id
                )));
            }
            let result = match self.turn(thread, turn).await {
                Ok(value) => value,
                Err(_) if self.cloud.cancel.is_cancelled() => continue,
                Err(e) => return Err(e),
            };
            if !matches!(
                result["status"].as_str(),
                Some("pending" | "queued" | "running" | "in_progress")
            ) || Instant::now() >= deadline
            {
                return Ok(result);
            }
            let pending = match self.request(Method::GET, "v1/permissions", None).await {
                Ok(value) => value,
                Err(_) if self.cloud.cancel.is_cancelled() => continue,
                Err(error) => return Err(error),
            };
            // Pending permissions are handled by the runtime's ordinary user interface.
            let permissions = pending
                .as_array()
                .ok_or_else(|| invalid("Invalid permission response"))?
                .iter()
                .filter(|p| {
                    p["threadId"].as_str() == Some(thread.to_string().as_str())
                        || p["rootThreadId"].as_str() == Some(thread.to_string().as_str())
                })
                .cloned()
                .collect::<Vec<_>>();
            if !permissions.is_empty() {
                let mut result = result;
                result["status"] = json!("needs_attention");
                result["permissions"] = json!(permissions);
                return Ok(result);
            }
            updates.wait(deadline, &self.cloud.cancel).await;
        }
    }
}

async fn response_json(response: reqwest::Response) -> Result<Value> {
    let (status, value) = read_response(response).await?;
    checked_response(status, value)
}

async fn read_response(mut response: reqwest::Response) -> Result<(StatusCode, Value)> {
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > MAX_RESPONSE {
            return Err(invalid(
                "Cloud response is too large; request a smaller page or use an after cursor",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| invalid(format!("Cloud returned an invalid response ({status})")))?
    };
    Ok((status, value))
}

fn checked_response(status: StatusCode, value: Value) -> Result<Value> {
    if !status.is_success() {
        return Err(invalid(format!(
            "Cloud runtime {status}: {}",
            value
                .pointer("/error/message")
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("request failed")
        )));
    }
    Ok(value)
}
