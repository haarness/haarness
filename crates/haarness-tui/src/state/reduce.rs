//! SSE event reduction into client state.
//!
//! Events are kept when they belong to the current thread OR to the same
//! root conversation — child-agent activity (progress, permission prompts)
//! must reach the view even though it happens on other threads. Dropping
//! everything else keeps unrelated threads from mutating this view; only
//! which chats wait for a person is followed everywhere, for the session list.

use anyhow::Result;
use haarness_protocol::{
    EventEnvelope, GoalStatus, GoalUpdatedEvent, Item, ItemData, PendingPermissionInfo,
    PendingQuestionInfo,
};

use crate::api::ApiClient;
use crate::state::{
    App, ChildTask, PhaseInfo, ToastVariant, TurnEnd, Waiting, WaitingRequest,
    task_runs_in_background,
};

fn string_list(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

impl App {
    /// Read the server's Auto/Review setting for the open chat. A server
    /// that predates shared modes leaves this TUI's own switch in charge.
    pub async fn refresh_approval_mode(&mut self, client: &ApiClient) {
        // A server that does not answer has not lost its shared modes: turning
        // this TUI's own switch back on with the last chat's Auto would answer
        // the new chat's requests from here.
        match client.info().await {
            Ok(info) => {
                let has = |name: &str| {
                    info.capabilities
                        .iter()
                        .any(|capability| capability == name)
                };
                self.approval_server = has(haarness_protocol::APPROVAL_MODE_CAPABILITY);
                self.approval_reviewer = has(haarness_protocol::APPROVAL_REVIEW_CAPABILITY);
            }
            Err(error) => tracing::debug!(%error, "could not read the server's capabilities"),
        }
        if !self.approval_server {
            self.approval_mode = None;
            return;
        }
        match client.approval_mode(self.thread.id).await {
            Ok(info) => self.show_approval_mode(info),
            Err(error) => {
                tracing::debug!(%error, "could not read the approval mode");
                // Never show another chat's mode as this one's: until the
                // server says otherwise (its next change event, or ctrl+y),
                // this chat reads as Review, the mode that answers nothing.
                if self
                    .approval_mode
                    .as_ref()
                    .is_none_or(|info| info.thread_id != self.thread.id)
                {
                    self.approval_mode = None;
                    self.keep_permission_choice(|app| app.auto_permissions = false);
                }
            }
        }
    }

    fn show_approval_mode(&mut self, info: haarness_protocol::ApprovalModeInfo) {
        if info.thread_id != self.thread.id {
            return;
        }
        self.keep_permission_choice(|app| {
            app.auto_permissions = info.mode == haarness_protocol::ApprovalMode::Auto;
        });
        self.approval_mode = Some(info);
    }

    /// Switch the open chat between Auto and Review. The server keeps it, so
    /// every window and the phone see the same mode.
    pub async fn change_approval_mode(&mut self, enabled: bool, client: &ApiClient) {
        if !self.approval_server {
            self.set_auto_permissions(enabled);
            return;
        }
        let request = haarness_protocol::SetApprovalModeRequest {
            mode: if enabled {
                haarness_protocol::ApprovalModeSetting::Auto
            } else {
                haarness_protocol::ApprovalModeSetting::Review
            },
            revision: self
                .approval_mode
                .as_ref()
                .map(|info| info.revision.clone()),
        };
        match client.set_approval_mode(self.thread.id, &request).await {
            Ok(info) => {
                self.show_approval_mode(info);
                self.notice = if enabled && self.approval_reviewer {
                    "Auto on for this chat · a reviewer model answers requests · ctrl+y to review"
                } else if enabled {
                    "Auto on for this chat · requests are allowed once · ctrl+y to review"
                } else {
                    "Review · asks before risky steps · ctrl+y for Auto"
                }
                .into();
            }
            Err(error) => {
                let conflict = error
                    .downcast_ref::<reqwest::Error>()
                    .and_then(reqwest::Error::status)
                    == Some(reqwest::StatusCode::CONFLICT);
                self.refresh_approval_mode(client).await;
                self.notice = if conflict {
                    "Approvals changed in another window · ctrl+y to switch".into()
                } else {
                    format!("Could not change approvals: {error}")
                };
            }
        }
    }

    /// Reply once for unseen requests only. A known manual/failed request cannot
    /// become automatic through replay, navigation or a mode toggle. A server
    /// with shared modes answers for Auto chats itself, so this stays quiet.
    async fn auto_permission(&mut self, id: &str, eligible: bool, client: &ApiClient) -> bool {
        if self.approval_server {
            return false;
        }
        if let Some(approved) = self.auto_permissions_seen.get(id) {
            return *approved;
        }
        self.auto_permissions_seen.insert(id.to_owned(), false);
        if !self.auto_permissions || !eligible {
            return false;
        }
        let reply = client.reply_permission(
            id,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Once,
                feedback: None,
            },
        );
        if matches!(
            tokio::time::timeout(std::time::Duration::from_secs(5), reply).await,
            Ok(Ok(()))
        ) {
            self.auto_permissions_seen.insert(id.to_owned(), true);
            return true;
        }
        self.show_toast(
            None,
            "Auto approval failed · review the request manually".into(),
            ToastVariant::Warning,
            std::time::Duration::from_secs(5),
        );
        false
    }

    /// `--yolo`: answer a request in the conversation this TUI shows once,
    /// as `haarness run --yolo` does for its run. A request that must ask
    /// every time (no Always choice) and Chrome site access (`explicit`) still
    /// get their card.
    async fn yolo_permission(&mut self, id: &str, eligible: bool, client: &ApiClient) -> bool {
        if !self.yolo || !eligible {
            return false;
        }
        if let Some(approved) = self.auto_permissions_seen.get(id) {
            return *approved;
        }
        let reply = client.reply_permission(
            id,
            haarness_protocol::PermissionReply {
                decision: haarness_protocol::PermissionReplyDecision::Once,
                feedback: None,
            },
        );
        let approved = matches!(
            tokio::time::timeout(std::time::Duration::from_secs(5), reply).await,
            Ok(Ok(()))
        );
        self.auto_permissions_seen.insert(id.to_owned(), approved);
        if !approved {
            self.show_toast(
                None,
                "Auto approval failed · review the request manually".into(),
                ToastVariant::Warning,
                std::time::Duration::from_secs(5),
            );
        }
        approved
    }

    pub async fn hydrate_permissions(
        &mut self,
        requests: Vec<PendingPermissionInfo>,
        client: &ApiClient,
    ) {
        self.refresh_approval_mode(client).await;
        let mut pending = Vec::new();
        let mut waiting = Vec::new();
        for request in requests {
            // Chrome site access always waits for a person, whatever this
            // client's yolo or Auto says.
            let eligible = request.allow_always != Some(false)
                && !request.explicit
                && !matches!(request.permission.as_str(), "instruction_import");
            let in_conversation = request.root_thread_id == Some(self.root_thread_id)
                || request.thread_id == self.thread.id;
            if in_conversation
                && self
                    .yolo_permission(&request.permission_id, eligible, client)
                    .await
            {
                continue;
            }
            if self
                .auto_permission(&request.permission_id, eligible, client)
                .await
            {
                continue;
            }
            waiting.push((
                request.permission_id.clone(),
                request.thread_id,
                request.root_thread_id,
            ));
            if in_conversation {
                pending.push(request);
            }
        }
        self.hydrate_waiting(Waiting::Approval, waiting);
        self.pending_permissions = pending;
    }

    pub async fn reduce(&mut self, event: EventEnvelope, client: &ApiClient) -> Result<()> {
        // An event can reorder the session list under its cursor: the cursor
        // keeps its chat.
        let anchor = self.session_list_anchor();
        let result = self.reduce_event(event, client).await;
        self.keep_session_list_anchor(anchor);
        result
    }

    /// Follow which chats wait for a person, in every chat on the server.
    fn follow_waiting(&mut self, event: &EventEnvelope) {
        let (kind, id) = match event.event_type.as_str() {
            "permission.requested" | "permission.resolved" => {
                (Waiting::Approval, event.payload["permissionId"].as_str())
            }
            "question.requested" | "question.resolved" => {
                (Waiting::Answer, event.payload["questionId"].as_str())
            }
            _ => return,
        };
        let Some(id) = id else {
            return;
        };
        if event.event_type.ends_with(".resolved") {
            self.waiting.remove(id);
        } else if let Some(thread_id) = event.thread_id {
            self.waiting.insert(
                id.to_owned(),
                WaitingRequest {
                    thread_id,
                    root_thread_id: event.root_thread_id,
                    kind,
                },
            );
        }
    }

    async fn reduce_event(&mut self, event: EventEnvelope, client: &ApiClient) -> Result<()> {
        if event.event_type == "approval_mode.changed" {
            if let Ok(info) = serde_json::from_value(event.payload.clone()) {
                self.show_approval_mode(info);
            }
            return Ok(());
        }
        if event.event_type == "permission.requested"
            && let Some(id) = event.payload["permissionId"].as_str()
        {
            let eligible = event.payload["allowOnce"] != false
                && event.payload["allowAlways"] != false
                && event.payload["explicit"] != true
                && !matches!(
                    event.payload["permission"].as_str(),
                    Some("instruction_import")
                );
            let in_conversation = event.thread_id == Some(self.thread.id)
                || event.root_thread_id == Some(self.root_thread_id);
            if in_conversation && self.yolo_permission(id, eligible, client).await {
                return Ok(());
            }
            if self.auto_permission(id, eligible, client).await {
                return Ok(());
            }
        }
        self.follow_waiting(&event);
        let on_current = event.thread_id == Some(self.thread.id);
        let in_root = event.root_thread_id == Some(self.root_thread_id);
        // A workflow dashboard is a global run catalog, so keep its rows live
        // even when a CLI or another session launched the run. Notifications
        // below remain scoped to this conversation's launching thread.
        let following_workflows = matches!(
            event.event_type.as_str(),
            "workflow.updated" | "workflow.log"
        ) && matches!(
            self.overlay,
            crate::state::Overlay::Options {
                kind: crate::state::OptionsKind::WorkflowDashboard,
                ..
            }
        );
        // Turn execution belongs to the server, not the selected view. Keep a
        // small global activity cache for background roots/children so /new,
        // /sessions, another TUI, or a future desktop client cannot make live
        // work look stale merely by navigating away.
        let global_activity = matches!(
            event.event_type.as_str(),
            "turn.text_delta"
                | "turn.reasoning_delta"
                | "turn.status"
                | "turn.started"
                | "turn.finished"
                | "assets.updated"
                | "configuration.reloaded"
                | "configuration.reload_queued"
                | "configuration.reload_failed"
        );
        if !on_current && !in_root && !following_workflows && !global_activity {
            return Ok(());
        }
        match event.event_type.as_str() {
            "input.queue.updated" if on_current => {
                if let Ok(queue) =
                    serde_json::from_value::<haarness_protocol::InputQueue>(event.payload)
                {
                    if queue.revision >= self.input_queue.revision {
                        self.input_queue = queue;
                    }
                }
            }
            "assets.updated" => {
                self.server_commands = client.commands().await.unwrap_or_default();
                // The server may wait up to 10 s for a provider's list: keys,
                // redraws and streaming go on meanwhile.
                if let Some(catalog) = self.catalog_tx.clone() {
                    let client = client.clone();
                    tokio::spawn(async move {
                        if let Ok(providers) = client.providers().await {
                            let _ = catalog.send(Some(providers)).await;
                        }
                    });
                } else if let Ok(providers) = client.providers().await {
                    self.catalog_arrived(providers, client).await;
                }
                if let Ok(agents) = client.agents().await {
                    self.agents = agents;
                }
                if let Ok(catalog) = client.workflows().await {
                    self.workflow_definitions = catalog.workflows;
                    self.workflow_invalid = catalog.invalid;
                }
            }
            "configuration.reloaded" => {
                // Most reloads follow a setting this client just saved; the
                // answer to that ("Anthropic connected · pick a model",
                // "Input mode: queue · saved for this project") says more.
                if !self.notice_is_fresh(std::time::Instant::now()) {
                    self.notice = "Configuration reloaded".into();
                }
            }
            "configuration.reload_queued" => {
                self.notice =
                    "Configuration changed · reload queued until active work is idle".into();
            }
            "configuration.reload_failed" => {
                let error = event.payload["error"]
                    .as_str()
                    .unwrap_or("invalid configuration");
                self.notice = format!("Configuration reload failed · {error}");
            }
            "turn.text_delta" => {
                if let Some(delta) = event.payload["delta"].as_str() {
                    if let Some(thread_id) = event.thread_id {
                        self.live_threads
                            .entry(thread_id)
                            .or_default()
                            .preview
                            .push_str(delta);
                    }
                    if on_current {
                        self.preview.push_str(delta);
                    }
                }
            }
            "turn.reasoning_delta" => {
                if let Some(delta) = event.payload["delta"].as_str() {
                    if let Some(thread_id) = event.thread_id {
                        self.live_threads
                            .entry(thread_id)
                            .or_default()
                            .reasoning_preview
                            .push_str(delta);
                    }
                    if on_current {
                        self.reasoning_preview.push_str(delta);
                    }
                }
            }
            "turn.status" => {
                let state = event.payload["state"].as_str().unwrap_or("").to_owned();
                // A review that goes on keeps the moment it was first heard of.
                let received = event
                    .thread_id
                    .and_then(|thread| self.live_threads.get(&thread))
                    .and_then(|live| live.phase.as_ref())
                    .filter(|phase| phase.state == state)
                    .and_then(|phase| phase.received)
                    .unwrap_or_else(std::time::Instant::now);
                let mut phase = PhaseInfo {
                    tool: event.payload["tool"].as_str().map(str::to_owned),
                    call_id: event.payload["callId"].as_str().map(str::to_owned),
                    since: event.payload["startedAt"]
                        .as_str()
                        .and_then(|value| value.parse().ok()),
                    received: Some(received),
                    state,
                };
                // A turn waiting for a person (an approval, a question's
                // answer) names no call: it is the one that was running (or
                // under review) when it asked, so its row can wait still
                // while the calls after it queue.
                if matches!(
                    phase.state.as_str(),
                    "waiting_permission" | "waiting_question"
                ) && phase.call_id.is_none()
                {
                    let previous = event
                        .thread_id
                        .and_then(|thread| self.live_threads.get(&thread))
                        .and_then(|live| live.phase.as_ref())
                        .or(self.phase.as_ref().filter(|_| on_current))
                        .filter(|previous| matches!(previous.state.as_str(), "tool" | "reviewing"));
                    if let Some(previous) = previous {
                        phase.tool.clone_from(&previous.tool);
                        phase.call_id.clone_from(&previous.call_id);
                    }
                }
                if let Some(thread_id) = event.thread_id {
                    self.live_threads.entry(thread_id).or_default().phase = Some(phase.clone());
                }
                if on_current {
                    self.phase = Some(phase);
                }
            }
            "task.updated" => {
                if let (Some(call_id), Ok(child_thread_id)) = (
                    event.payload["callId"].as_str(),
                    serde_json::from_value(event.payload["childThreadId"].clone()),
                ) {
                    // A child spawned from inside a `batch` carries the batch
                    // call's id plus an index; its progress is just as real.
                    let batch_parent = crate::state::batch_parent_call(call_id);
                    let known_task = self.children.contains_key(call_id)
                        || self.items.iter().any(|item| {
                            matches!(
                                &item.data,
                                ItemData::ToolCall {
                                    call_id: candidate,
                                    name,
                                    ..
                                } if (candidate == call_id && name == "task")
                                    || (name == "batch" && batch_parent == Some(candidate.as_str()))
                            )
                        });
                    if known_task {
                        let invoked_turn_id = self.items.iter().rev().find_map(|item| {
                            let matches_call = match &item.data {
                                ItemData::TaskLink {
                                    call_id: candidate, ..
                                } => candidate == call_id,
                                ItemData::ToolCall {
                                    call_id: candidate,
                                    name,
                                    ..
                                } => candidate == call_id && name == "task",
                                _ => false,
                            };
                            matches_call.then_some(item.turn_id).flatten()
                        });
                        let child_turn_id =
                            serde_json::from_value(event.payload["childTurnId"].clone()).ok();
                        let started_at = event.payload["startedAt"]
                            .as_str()
                            .and_then(|value| value.parse().ok());
                        let background =
                            task_runs_in_background(&self.items, call_id, child_thread_id);
                        let row = self
                            .child_row_for_thread(child_thread_id)
                            .unwrap_or_else(|| call_id.to_owned());
                        let task = self.children.entry(row).or_insert(ChildTask {
                            child_thread_id,
                            child_turn_id,
                            invoked_turn_id,
                            state: if background { "background" } else { "running" }.into(),
                            tool_calls: 0,
                            last_tool: None,
                            started_at,
                        });
                        task.child_thread_id = child_thread_id;
                        task.invoked_turn_id = invoked_turn_id.or(task.invoked_turn_id);
                        if child_turn_id.is_some() {
                            task.child_turn_id = child_turn_id;
                        }
                        if let Some(state) = event.payload["state"].as_str() {
                            // `running` is live activity, not foreground mode.
                            // Keep the durable background lane visible while
                            // still allowing approval and terminal states to
                            // take precedence.
                            let visible_state = if state == "running" && background {
                                "background"
                            } else {
                                state
                            };
                            visible_state.clone_into(&mut task.state);
                        }
                        if let Some(tool_calls) = event.payload["toolCalls"].as_u64() {
                            task.tool_calls = tool_calls;
                        }
                        if let Some(last_tool) = event.payload["lastTool"].as_str() {
                            task.last_tool = Some(last_tool.to_owned());
                        }
                        if started_at.is_some() {
                            task.started_at = started_at;
                        }
                    }
                }
            }
            "permission.requested" => {
                // Full metadata rides on the event, so prompts from child
                // threads surface here without any extra fetch.
                if let Some(id) = event.payload["permissionId"].as_str() {
                    // Announced again (Auto's reviewer took up a request
                    // already shown, or left it to the person): the card
                    // takes the new note in place, nothing else moves.
                    let known = self
                        .pending_permissions
                        .iter_mut()
                        .find(|pending| pending.permission_id == id);
                    let again = known.is_some();
                    if let Some(known) = known {
                        // Announced again (Auto turned on, the reviewer left
                        // it to the person): its note and its script as they
                        // are now.
                        known.review = serde_json::from_value(event.payload["review"].clone()).ok();
                        known.script = serde_json::from_value(event.payload["script"].clone()).ok();
                    } else {
                        self.pending_permissions.push(PendingPermissionInfo {
                            allow_always: event.payload["allowAlways"].as_bool(),
                            explicit: event.payload["explicit"].as_bool().unwrap_or(false),
                            repository_scope: event.payload["repositoryScope"]
                                .as_str()
                                .map(str::to_owned),
                            permission_id: id.to_owned(),
                            operation: event.payload["operation"]
                                .as_str()
                                .unwrap_or("?")
                                .to_owned(),
                            permission: event.payload["permission"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned(),
                            patterns: string_list(&event.payload["patterns"]),
                            always_patterns: string_list(&event.payload["alwaysPatterns"]),
                            detail: event.payload["detail"].clone(),
                            script: serde_json::from_value(event.payload["script"].clone())
                                .ok()
                                .flatten(),
                            thread_id: event.thread_id.unwrap_or(self.thread.id),
                            root_thread_id: event.root_thread_id,
                            turn_id: event.turn_id,
                            agent: event.payload["agent"].as_str().map(str::to_owned),
                            created_at: event.created_at,
                            review: serde_json::from_value(event.payload["review"].clone()).ok(),
                        });
                    }
                    // Nothing new to point at: no focus change, no toast.
                    if again {
                        return Ok(());
                    }
                    if self.pending_permissions.len() == 1 {
                        // A fresh head prompt starts at the first chip, with
                        // nothing staged or typed for one that went away
                        // unanswered (its turn ended): a bare Enter must not
                        // confirm an old Always or Once + Auto here.
                        self.permission_selection = 0;
                        self.confirm_always = None;
                        self.permission_typed = None;
                    }
                    if self.composer.is_empty() && self.pending_questions.is_empty() {
                        self.permission_focus = true;
                    }
                    self.update_permission_notice();
                    if let Some(message) = self
                        .pending_permissions
                        .iter()
                        .find(|pending| pending.permission_id == id)
                        .map(|pending| self.approval_notification(pending))
                    {
                        self.pending_notification = Some(message.clone());
                        // Drawn only where the card itself is hidden.
                        self.show_toast(
                            None,
                            message,
                            ToastVariant::Warning,
                            std::time::Duration::from_secs(8),
                        );
                        if let Some(toast) = &mut self.notification_toast {
                            toast.approval = true;
                        }
                    }
                }
            }
            "permission.resolved" => {
                // Covers cascaded resolutions too (Always resolving identical
                // siblings, Reject clearing the root): the prompt is gone
                // everywhere, so it must leave this view as well.
                if let Some(id) = event.payload["permissionId"].as_str() {
                    let child_thread_id = event.thread_id.or_else(|| {
                        self.pending_permissions
                            .iter()
                            .find(|pending| pending.permission_id == id)
                            .map(|pending| pending.thread_id)
                    });
                    let resolved_notification = self
                        .pending_permissions
                        .iter()
                        .find(|pending| pending.permission_id == id)
                        .map(|pending| self.approval_notification(pending));
                    let was_first = self
                        .pending_permissions
                        .first()
                        .is_some_and(|pending| pending.permission_id == id);
                    self.pending_permissions
                        .retain(|pending| pending.permission_id != id);
                    if was_first {
                        // The panel now fronts a different prompt: a staged
                        // "always" confirmation or chip selection for the old
                        // one must not carry over.
                        self.confirm_always = None;
                        self.permission_typed = None;
                        self.permission_selection = 0;
                    }
                    if let Some(message) = resolved_notification {
                        self.dismiss_notification(&message);
                    }
                    if self.pending_permissions.is_empty() {
                        self.permission_focus = false;
                    } else {
                        self.update_permission_notice();
                    }
                    if let Some(child_thread_id) = child_thread_id {
                        self.resume_child_after_permission(child_thread_id);
                    }
                }
            }
            "question.requested" => {
                // Child questions block their child exactly like permissions
                // did; accept them root-wide so nothing waits invisibly.
                if let Some(id) = event.payload["questionId"].as_str() {
                    let prompt = event.payload["prompt"].as_str().unwrap_or("?").to_owned();
                    let was_empty = self.pending_questions.is_empty();
                    self.queue_question(PendingQuestionInfo {
                        question_id: id.to_owned(),
                        prompt: prompt.clone(),
                        choices: string_list(&event.payload["choices"]),
                        multiple: event.payload["multiple"].as_bool().unwrap_or(false),
                        custom: event.payload["custom"].as_bool().unwrap_or(false),
                        thread_id: event.thread_id.unwrap_or(self.thread.id),
                        root_thread_id: event.root_thread_id,
                        turn_id: event.turn_id,
                        created_at: event.created_at,
                    });
                    if was_empty {
                        prompt.clone_into(&mut self.notice);
                    }
                    let message = format!("Haarness needs your answer: {prompt}");
                    self.pending_notification = Some(message.clone());
                    self.show_toast(
                        None,
                        message,
                        ToastVariant::Warning,
                        std::time::Duration::from_secs(8),
                    );
                }
            }
            "question.resolved" => {
                if let Some(id) = event.payload["questionId"].as_str() {
                    let resolved_notification = self
                        .pending_questions
                        .iter()
                        .find(|pending| pending.question_id == id)
                        .map(|pending| format!("Haarness needs your answer: {}", pending.prompt));
                    self.question_resolved(id);
                    if let Some(message) = resolved_notification {
                        self.dismiss_notification(&message);
                    }
                }
            }
            "item.created" => {
                if let Ok(item) = serde_json::from_value::<Item>(event.payload.clone()) {
                    // The prompt a turn starts with: a person's own, not a
                    // background result the runtime delivered.
                    if let ItemData::UserMessage {
                        queued: false,
                        synthetic: false,
                        ..
                    } = &item.data
                    {
                        self.completions.prompted(item.thread_id);
                    }
                    if let Some(thread_id) = event.thread_id {
                        let live = self.live_threads.entry(thread_id).or_default();
                        match &item.data {
                            ItemData::Reasoning { .. } => live.reasoning_preview.clear(),
                            ItemData::AssistantMessage { .. } => live.preview.clear(),
                            _ => {}
                        }
                    }
                    if on_current {
                        // Install every durable item before clearing its live
                        // preview. Besides clearing QUEUED immediately, this
                        // prevents a completed answer from blinking away until
                        // the next REST reconciliation tick.
                        if !self.items.iter().any(|current| current.id == item.id) {
                            self.items.push(item.clone());
                            self.items.sort_by_key(|current| current.sequence);
                            self.refresh_open_messages();
                        }
                        if let ItemData::ToolResult {
                            call_id,
                            name,
                            output,
                            is_error,
                            metadata,
                        } = &item.data
                            && name == "bash"
                            && call_id.starts_with("shell-")
                        {
                            self.notice = "Ready".into();
                            if *is_error && output.starts_with("permission denied") {
                                // The person just answered the prompt; a red
                                // "failed" card would contradict their choice.
                                self.notice = "Shell command not run".into();
                            } else {
                                // The output joins the transcript under the
                                // command. A card only says how it ended, and
                                // only when that needs attention: it failed,
                                // or the terminal was not in front.
                                let exit = metadata
                                    .get("exit")
                                    .and_then(serde_json::Value::as_i64)
                                    .filter(|code| *code != 0);
                                let failed = *is_error || exit.is_some();
                                let command =
                                    self.items
                                        .iter()
                                        .rev()
                                        .find_map(|candidate| match &candidate.data {
                                            ItemData::ToolCall {
                                                call_id: candidate_id,
                                                arguments,
                                                ..
                                            } if candidate_id == call_id => arguments
                                                .get("command")
                                                .and_then(serde_json::Value::as_str)
                                                .and_then(|command| command.lines().next())
                                                .map(str::to_owned),
                                            _ => None,
                                        });
                                let message = match (exit, command) {
                                    _ if *is_error => output.clone(),
                                    (Some(code), Some(command)) => {
                                        format!("{command} · exit {code}")
                                    }
                                    (Some(code), None) => format!("exit {code}"),
                                    (None, Some(command)) => command,
                                    (None, None) => "Command completed".into(),
                                };
                                if failed || !self.terminal_focused {
                                    self.show_toast(
                                        Some(if failed {
                                            "Shell failed".into()
                                        } else {
                                            "Shell finished".into()
                                        }),
                                        message,
                                        if failed {
                                            ToastVariant::Error
                                        } else {
                                            ToastVariant::Success
                                        },
                                        std::time::Duration::from_secs(if failed { 8 } else { 4 }),
                                    );
                                } else {
                                    self.notice = "Shell finished".into();
                                }
                            }
                        }
                        let item_turn_id = item.turn_id;
                        match item.data {
                            ItemData::Reasoning { .. } => {
                                // The durable item supersedes the live preview.
                                self.reasoning_preview.clear();
                            }
                            ItemData::AssistantMessage { .. } => {
                                self.preview.clear();
                            }
                            ItemData::TaskLink {
                                call_id,
                                child_thread_id,
                            } => {
                                let row = self
                                    .child_row_for_thread(child_thread_id)
                                    .unwrap_or(call_id);
                                let task = self.children.entry(row).or_insert(ChildTask {
                                    child_thread_id,
                                    child_turn_id: None,
                                    invoked_turn_id: item_turn_id,
                                    state: "running".into(),
                                    tool_calls: 0,
                                    last_tool: None,
                                    started_at: Some(item.created_at),
                                });
                                task.invoked_turn_id = item_turn_id.or(task.invoked_turn_id);
                            }
                            ItemData::ToolResult {
                                call_id,
                                name,
                                output,
                                is_error,
                                ..
                            } if name == "task" => {
                                let row = self
                                    .children
                                    .contains_key(&call_id)
                                    .then_some(call_id.clone())
                                    .or_else(|| {
                                        self.child_thread_for_call(&call_id)
                                            .and_then(|child| self.child_row_for_thread(child))
                                    });
                                if let Some(task) =
                                    row.as_ref().and_then(|row| self.children.get_mut(row))
                                {
                                    task.state =
                                        if !is_error && output.contains("state=\"running\"") {
                                            "background"
                                        } else if !is_error {
                                            "done"
                                        } else if output.trim() == "operation cancelled" {
                                            "cancelled"
                                        } else {
                                            "failed"
                                        }
                                        .into();
                                }
                            }
                            ItemData::TaskState {
                                call_id,
                                child_thread_id,
                                child_turn_id,
                                state,
                            } => {
                                let shared_state = state.clone();
                                let row = self
                                    .child_row_for_thread(child_thread_id)
                                    .unwrap_or(call_id);
                                let task = self.children.entry(row).or_insert(ChildTask {
                                    child_thread_id,
                                    child_turn_id: Some(child_turn_id),
                                    invoked_turn_id: item_turn_id,
                                    state: shared_state.clone(),
                                    tool_calls: 0,
                                    last_tool: None,
                                    started_at: Some(item.created_at),
                                });
                                task.child_thread_id = child_thread_id;
                                task.child_turn_id = Some(child_turn_id);
                                task.state.clone_from(&shared_state);
                            }
                            _ => {}
                        }
                        self.refresh_needed = true;
                    }
                }
            }
            "workflow.log" => {
                if let (Ok(run_id), Ok(entry)) = (
                    serde_json::from_value::<haarness_protocol::WorkflowId>(
                        event.payload["runId"].clone(),
                    ),
                    serde_json::from_value::<haarness_protocol::WorkflowLog>(
                        event.payload["entry"].clone(),
                    ),
                ) && let Some(run) = self.workflow_runs.iter_mut().find(|run| run.id == run_id)
                    && !run
                        .logs
                        .iter()
                        .any(|current| current.sequence == entry.sequence)
                {
                    run.time.updated = entry.time;
                    run.logs.push(entry);
                    run.logs.sort_by_key(|log| log.sequence);
                    self.workflow_runs
                        .sort_by(|left, right| right.time.updated.cmp(&left.time.updated));
                    self.refresh_open_workflow_dashboard();
                    self.refresh_needed = true;
                }
            }
            "workflow.updated" => {
                if let Ok(run) = serde_json::from_value::<haarness_protocol::WorkflowRun>(
                    event.payload["info"].clone(),
                ) {
                    let previous = self
                        .workflow_runs
                        .iter()
                        .find(|current| current.id == run.id)
                        .map(|current| current.status);
                    let status = run.status;
                    let launching_thread_visible = run
                        .definition
                        .launch_thread_id
                        .or(run.thread_id)
                        .is_some_and(|thread_id| {
                            thread_id == self.thread.id || thread_id == self.root_thread_id
                        });
                    let terminal = matches!(
                        status,
                        haarness_protocol::WorkflowRunStatus::Completed
                            | haarness_protocol::WorkflowRunStatus::Failed
                            | haarness_protocol::WorkflowRunStatus::Cancelled
                            | haarness_protocol::WorkflowRunStatus::Interrupted
                    );
                    let transitioned = previous != Some(status);
                    let name = run.workflow_name.clone();
                    self.upsert_workflow_run(run);
                    self.refresh_open_workflow_dashboard();
                    if launching_thread_visible && terminal && transitioned {
                        let (verb, variant) = match status {
                            haarness_protocol::WorkflowRunStatus::Completed => {
                                ("completed", ToastVariant::Success)
                            }
                            haarness_protocol::WorkflowRunStatus::Failed => {
                                ("failed", ToastVariant::Error)
                            }
                            haarness_protocol::WorkflowRunStatus::Cancelled => {
                                ("cancelled", ToastVariant::Warning)
                            }
                            haarness_protocol::WorkflowRunStatus::Interrupted => {
                                ("interrupted", ToastVariant::Warning)
                            }
                            haarness_protocol::WorkflowRunStatus::Running
                            | haarness_protocol::WorkflowRunStatus::Paused => unreachable!(),
                        };
                        let message = format!("Workflow {verb}: {name}");
                        self.pending_notification = Some(message.clone());
                        if variant != ToastVariant::Success || !self.terminal_focused {
                            self.show_toast(
                                None,
                                message,
                                variant,
                                std::time::Duration::from_secs(6),
                            );
                        }
                    }
                }
            }
            "goal.updated" if on_current => {
                let update = serde_json::from_value::<GoalUpdatedEvent>(event.payload.clone())
                    .or_else(|_| {
                        // Completion copy is model-authored and may be absent
                        // or partial. Do not discard the otherwise-valid goal
                        // state when only that optional UI payload is invalid.
                        let mut payload = event.payload.clone();
                        if let Some(object) = payload.as_object_mut() {
                            object.remove("completion");
                        }
                        serde_json::from_value::<GoalUpdatedEvent>(payload)
                    });
                if let Ok(update) = update {
                    let status = update.state.status;
                    let goal_id = update.state.goal_id.clone();
                    let objective = update.state.goal.clone();
                    let status_label = crate::commands::goal_status_label(&update.state);
                    let completion_dialog_open = matches!(
                        self.overlay,
                        crate::state::Overlay::Options {
                            kind: crate::state::OptionsKind::GoalCompletion,
                            ..
                        }
                    );
                    self.goal = (status != GoalStatus::Cleared).then_some(update.state);
                    self.goal_snapshots
                        .insert(self.thread.id, self.goal.clone());
                    self.goal_refresh_needed = false;
                    if status == GoalStatus::Complete {
                        if let Some(completion) = update.completion {
                            self.goal_completion = Some(completion.clone());
                            // A durable snapshot may have opened the English
                            // fallback before SSE replays the localized event.
                            // Refresh the visible choices without resetting
                            // the user's current selection.
                            if completion_dialog_open {
                                let selection = self.selection;
                                self.open_goal_completion(completion);
                                self.selection = selection.min(3);
                            }
                        } else {
                            self.goal_completion
                                .get_or_insert_with(crate::commands::default_goal_completion);
                        }
                    } else if let Some(completion) = update.completion {
                        self.goal_completion = Some(completion);
                    }
                    if status == GoalStatus::Cleared {
                        self.goal_completion = None;
                        self.last_goal_completion = None;
                    }
                    if let Some(message) = update.message {
                        self.notice.clone_from(&message);
                        let terminal = matches!(
                            status,
                            GoalStatus::Complete
                                | GoalStatus::Blocked
                                | GoalStatus::UsageLimited
                                | GoalStatus::BudgetLimited
                        );
                        if terminal {
                            let variant = if status == GoalStatus::Complete {
                                ToastVariant::Success
                            } else {
                                ToastVariant::Warning
                            };
                            self.pending_notification = Some(message.clone());
                            self.show_toast(
                                Some(if status == GoalStatus::Complete {
                                    "Goal achieved".into()
                                } else {
                                    "Goal stopped".into()
                                }),
                                message,
                                variant,
                                std::time::Duration::from_secs(7),
                            );
                        }
                    } else {
                        self.notice = format!("Goal {status_label}: {objective}");
                    }
                    if self.last_goal_completion.as_deref() != Some(&goal_id) {
                        self.reveal_goal_completion();
                    }
                }
            }
            "todo.updated" if on_current => {
                if let Ok(todos) = serde_json::from_value(event.payload["todos"].clone()) {
                    self.todos = todos;
                }
            }
            "turn.message_queued"
                if on_current && event.payload["synthetic"].as_bool() != Some(true) =>
            {
                self.notice = "Queued — will reach the model mid-turn".into();
            }
            "turn.started" => {
                if let Some(thread_id) = event.thread_id {
                    self.live_threads.entry(thread_id).or_default();
                    // The next queued prompt began: a "finished" banner the
                    // conversation was holding is not true.
                    self.completions.resume(thread_id);
                }
                if event.thread_id == Some(self.root_thread_id) {
                    self.root_active_turn = event.turn_id;
                }
                if on_current && self.active_turn.is_none() {
                    self.active_turn = event.turn_id;
                    self.turn_started = Some(std::time::Instant::now());
                }
            }
            "turn.finished" => {
                let background_root = event.thread_id.is_some_and(|thread_id| {
                    thread_id != self.thread.id
                        && (event.root_thread_id == Some(thread_id)
                            || self
                                .threads
                                .iter()
                                .find(|thread| thread.id == thread_id)
                                .is_some_and(|thread| !thread.is_subagent))
                });
                if let Some(thread_id) = event.thread_id {
                    self.live_threads.remove(&thread_id);
                }
                if event.thread_id == Some(self.root_thread_id) {
                    self.root_active_turn = None;
                    // Approvals are transient, but completed child-thread
                    // links remain navigable for the life of the session.
                    self.pending_permissions.clear();
                    self.permission_focus = false;
                }
                let end = TurnEnd::from_payload(&event.payload);
                if on_current {
                    let goal_continues = self
                        .goal
                        .as_ref()
                        .is_some_and(|goal| goal.status == GoalStatus::Active);
                    // Long work earns a "finished" banner after a short grace
                    // (see `state::completion`); a goal's next turn follows on
                    // its own, so it is not finished yet.
                    if !(goal_continues && end == TurnEnd::Completed) {
                        let title = self.thread.title.clone();
                        self.turn_ended(self.thread.id, end, &title, false, self.turn_started);
                    }
                    self.active_turn = None;
                    self.turn_started = None;
                    self.phase = None;
                    self.preview.clear();
                    self.reasoning_preview.clear();
                    self.notice = if goal_continues {
                        "Continuing toward goal…".into()
                    } else {
                        "Ready".into()
                    };
                    // A transient fetch failure must not kill the event loop;
                    // the next tick retries via refresh_needed.
                    match client.items(self.thread.id).await {
                        Ok(items) => self.items = items,
                        Err(_) => self.refresh_needed = true,
                    }
                    self.refresh_open_messages();
                    match client.todos(self.thread.id).await {
                        Ok(todos) => self.todos = todos,
                        Err(_) => self.hydrate_todos_from_items(),
                    }
                    self.reveal_goal_completion();
                } else if background_root {
                    let title = event
                        .thread_id
                        .and_then(|thread_id| {
                            self.threads
                                .iter()
                                .find(|thread| thread.id == thread_id)
                                .map(|thread| thread.title.clone())
                        })
                        .unwrap_or_else(|| "Background task".into());
                    if let Some(thread_id) = event.thread_id {
                        self.turn_ended(thread_id, end, &title, true, None);
                    }
                }
            }
            "thread.updated" => {
                if let Ok(thread) =
                    serde_json::from_value::<haarness_protocol::Thread>(event.payload.clone())
                {
                    if thread.id == self.thread.id {
                        if thread.agent != self.thread.agent
                            && let Some(index) = self
                                .agents
                                .iter()
                                .position(|agent| agent.id == thread.agent)
                        {
                            self.agent_index = index;
                        }
                        // Undo/redo elsewhere, or the next turn confirming an
                        // undo, changes which items are live: reload them.
                        if thread.revert.as_ref().map(|revert| revert.sequence)
                            != self.thread.revert.as_ref().map(|revert| revert.sequence)
                        {
                            self.refresh_needed = true;
                        }
                        self.thread = thread.clone();
                        self.normalize_reasoning();
                    }
                    for ancestor in &mut self.parent_stack {
                        if ancestor.id == thread.id {
                            *ancestor = thread.clone();
                        }
                    }
                    if let Some(entry) = self
                        .threads
                        .iter_mut()
                        .find(|candidate| candidate.id == thread.id)
                    {
                        *entry = thread;
                    } else {
                        self.threads.push(thread);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The desktop banner and toast for one approval, in the card's words.
    fn approval_notification(&self, pending: &PendingPermissionInfo) -> String {
        format!(
            "{}: {}",
            self.language.text("Haarness needs approval"),
            crate::view::approval::summary(pending, self.language)
        )
    }

    /// Refresh the status-line hint for pending approvals, in the card's
    /// words. The chips carry their own keys, so the hint does not list them.
    pub fn update_permission_notice(&mut self) {
        if let Some(notice) = self.permission_notice() {
            self.notice = notice;
        }
    }

    /// The status-line hint for pending approvals, if any wait: the first
    /// one in the card's words while the card has the keyboard, otherwise
    /// how many wait and how to reach them.
    pub fn permission_notice(&self) -> Option<String> {
        let first = self.pending_permissions.first()?;
        let summary = crate::view::approval::summary(first, self.language);
        let language = self.language;
        let notice = match (self.pending_permissions.len(), self.permission_focus) {
            (1, true) => summary,
            (count, true) => format!("[1/{count}] {summary}"),
            (1, false) => format!(
                "{}: {summary}  ·  {}",
                language.text("Approval pending"),
                language.text("ctrl+g to answer")
            ),
            (count, false) => format!(
                "{}  ·  {}",
                language
                    .text("{count} approvals pending")
                    .replace("{count}", &count.to_string()),
                language.text("ctrl+g to answer")
            ),
        };
        Some(notice)
    }

    /// Give the keyboard back to the composer while an approval waits. With
    /// `keep_typed`, letters the card took as hotkeys before it was clear the
    /// user was typing a message go to the composer; otherwise they are
    /// dropped. Either way the card returns to the choice it showed before
    /// them, and nothing staged stays staged.
    pub fn release_permission_focus(&mut self, keep_typed: bool) {
        if self.confirm_always.take().is_some() {
            self.permission_selection = 0;
        }
        if let Some((typed, selection, _)) = self.permission_typed.take() {
            self.permission_selection = selection;
            if keep_typed {
                for letter in typed.chars() {
                    self.composer.insert_char(letter);
                }
            }
        }
        self.permission_focus = false;
        self.update_permission_notice();
    }

    /// Drop an answered permission and move focus to the next one (or back to
    /// the composer when none remain).
    pub fn permission_answered(&mut self, id: &str) {
        let answered = self
            .pending_permissions
            .iter()
            .find(|pending| pending.permission_id == id)
            .map(|pending| (self.approval_notification(pending), pending.thread_id));
        self.pending_permissions
            .retain(|pending| pending.permission_id != id);
        self.waiting.remove(id);
        self.confirm_always = None;
        self.permission_typed = None;
        self.permission_selection = 0;
        if let Some((message, _)) = &answered {
            self.dismiss_notification(message);
        }
        if self.pending_permissions.is_empty() {
            self.permission_focus = false;
            self.notice = "Permission answered".into();
        } else {
            self.update_permission_notice();
        }
        if let Some((_, child_thread_id)) = answered {
            self.resume_child_after_permission(child_thread_id);
        }
    }

    /// Permission resolution and the resumed child's next progress event cross
    /// independent HTTP/SSE scheduling boundaries. Once no prompt remains for
    /// that child, stop rendering the stale approval marker immediately; the
    /// authoritative running/terminal event will refine it moments later.
    fn resume_child_after_permission(&mut self, child_thread_id: haarness_protocol::ThreadId) {
        if self
            .pending_permissions
            .iter()
            .any(|pending| pending.thread_id == child_thread_id)
        {
            return;
        }
        let updates = self
            .children
            .iter()
            .filter(|(_, task)| {
                task.child_thread_id == child_thread_id && task.state == "waiting_permission"
            })
            .map(|(row, task)| {
                (
                    row.clone(),
                    task_runs_in_background(&self.items, row, task.child_thread_id),
                )
            })
            .collect::<Vec<_>>();
        for (row, background) in updates {
            if let Some(task) = self.children.get_mut(&row) {
                task.state = if background { "background" } else { "running" }.into();
            }
        }
    }
}
