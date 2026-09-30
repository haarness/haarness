//! Claude-Code-compatible lifecycle hooks: shell commands configured per
//! event with tool-name matchers, fed JSON on stdin, answering with JSON on
//! stdout (or exit code 2 to block).
//!
//! Config shape (in Haarness JSON and Haarness-managed plugin `hooks/hooks.json`):
//! ```json
//! { "hooks": { "PreToolUse": [ { "matcher": "Bash|Edit",
//!     "hooks": [{ "type": "command", "command": "./check.sh", "timeout": 10 }] } ] } }
//! ```
//!
//! Stdin payload: `{"hook_event_name", "session_id", "cwd", "tool_name",
//! "tool_input", "prompt", "tool_response"}` (fields per event). Stdout may
//! carry `{"decision": "block", "reason", "additionalContext",
//! "hookSpecificOutput": {"permissionDecision": "allow"|"deny"|"ask"}}`.
//!
//! Events: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`,
//! `UserPromptSubmit`, `PermissionRequest`, `Stop`, `SessionStart`,
//! `SessionEnd`, `TurnEnd`, `Notification`, `FileChanged` and `ConfigChange`
//! defined by this module. Matchers compare against the tool name
//! for tool events and `FileChanged`, the `source`/`reason` for session
//! events and the `notification_type` for notifications.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use haarness_core::{HookDecision, HookPort, PermissionDecision};
use haarness_protocol::ThreadId;
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct HookEntry {
    pub event: String,
    pub matcher: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub shell: Option<String>,
    pub timeout: Duration,
    pub asynchronous: bool,
    pub plugin_root: Option<PathBuf>,
}

pub struct ConfiguredHooks {
    entries: Vec<HookEntry>,
    workspace: PathBuf,
}

impl ConfiguredHooks {
    #[must_use]
    pub fn new(entries: Vec<HookEntry>, workspace: PathBuf) -> Self {
        Self { entries, workspace }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Parse a `hooks` config block (haarness/Claude shape) into entries.
    #[must_use]
    pub fn parse_block(block: &Value) -> Vec<HookEntry> {
        Self::parse_block_from(block, None)
    }

    /// Parse hooks bundled in a plugin while preserving the plugin root used
    /// by Claude Code's command placeholders and process environment.
    #[must_use]
    pub fn parse_plugin_block(block: &Value, root: PathBuf) -> Vec<HookEntry> {
        Self::parse_block_from(block, Some(root))
    }

    fn parse_block_from(block: &Value, plugin_root: Option<PathBuf>) -> Vec<HookEntry> {
        let mut entries = Vec::new();
        let Some(events) = block.as_object() else {
            return entries;
        };
        for (event, groups) in events {
            let Some(groups) = groups.as_array() else {
                continue;
            };
            for group in groups {
                if group
                    .get("disabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    continue;
                }
                let matcher = group
                    .get("matcher")
                    .and_then(Value::as_str)
                    .filter(|matcher| !matcher.is_empty() && *matcher != "*")
                    .map(str::to_owned);
                let Some(hooks) = group.get("hooks").and_then(Value::as_array) else {
                    continue;
                };
                for hook in hooks {
                    if hook
                        .get("disabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    if hook
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("command")
                        != "command"
                    {
                        continue;
                    }
                    let Some(command) = hook.get("command").and_then(Value::as_str) else {
                        continue;
                    };
                    let timeout = hook
                        .get("timeout")
                        .and_then(Value::as_u64)
                        .unwrap_or(60)
                        .clamp(1, 600);
                    entries.push(HookEntry {
                        event: event.clone(),
                        matcher: matcher.clone(),
                        command: command.to_owned(),
                        args: hook
                            .get("args")
                            .and_then(Value::as_array)
                            .map(|args| {
                                args.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default(),
                        shell: hook.get("shell").and_then(Value::as_str).map(str::to_owned),
                        timeout: Duration::from_secs(timeout),
                        asynchronous: hook.get("async").and_then(Value::as_bool).unwrap_or(false),
                        plugin_root: plugin_root.clone(),
                    });
                }
            }
        }
        entries
    }

    fn matching(&self, event: &str, tool: Option<&str>) -> Vec<&HookEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.event == event)
            .filter(|entry| match (&entry.matcher, tool) {
                (None, _) | (Some(_), None) => true,
                (Some(matcher), Some(tool)) => matcher_matches(matcher, tool),
            })
            .collect()
    }

    async fn run(&self, entry: &HookEntry, payload: &Value) -> HookOutcome {
        use tokio::io::AsyncWriteExt as _;
        use tokio::process::Command;
        let expand = |value: &str| expand_hook_value(value, entry, &self.workspace);
        let mut command = if entry.args.is_empty() {
            let shell = entry.shell.as_deref().unwrap_or("sh");
            let mut command = Command::new(shell);
            if shell.eq_ignore_ascii_case("powershell") {
                command.args(["-NoProfile", "-Command"]);
            } else {
                command.arg("-lc");
            }
            command.arg(expand(&entry.command));
            command
        } else {
            let mut command = Command::new(expand(&entry.command));
            command.args(entry.args.iter().map(|arg| expand(arg)));
            command
        };
        command
            .current_dir(&self.workspace)
            .env("HAARNESS_PROJECT_DIR", &self.workspace)
            .env("CLAUDE_PROJECT_DIR", &self.workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(root) = &entry.plugin_root {
            command
                .env("HAARNESS_PLUGIN_ROOT", root)
                .env("CLAUDE_PLUGIN_ROOT", root)
                .env("OPENCODE_PLUGIN_ROOT", root)
                .env("HAARNESS_PLUGIN_DATA", root.join(".data"))
                .env("CLAUDE_PLUGIN_DATA", root.join(".data"));
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(command = entry.command, %error, "hook failed to spawn");
                return HookOutcome::default();
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.to_string().as_bytes()).await;
            drop(stdin);
        }
        if entry.asynchronous {
            let timeout = entry.timeout;
            tokio::spawn(async move {
                let _ = tokio::time::timeout(timeout, child.wait_with_output()).await;
            });
            return HookOutcome::default();
        }
        let output = match tokio::time::timeout(entry.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                tracing::warn!(command = entry.command, %error, "hook failed");
                return HookOutcome::default();
            }
            Err(_) => {
                tracing::warn!(command = entry.command, "hook timed out");
                return HookOutcome::default();
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut outcome = HookOutcome::default();
        // Exit code 2 is the Claude "block" convention; stderr is the reason.
        if output.status.code() == Some(2) {
            let reason = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            outcome.block = Some(if reason.is_empty() {
                "blocked by hook".into()
            } else {
                reason
            });
            return outcome;
        }
        if let Ok(parsed) = serde_json::from_str::<Value>(stdout.trim()) {
            if parsed.get("decision").and_then(Value::as_str) == Some("block")
                || parsed.get("continue").and_then(Value::as_bool) == Some(false)
            {
                outcome.block = Some(
                    parsed
                        .get("reason")
                        .or_else(|| parsed.get("stopReason"))
                        .and_then(Value::as_str)
                        .unwrap_or("blocked by hook")
                        .to_owned(),
                );
            }
            if parsed.get("decision").and_then(Value::as_str) == Some("approve") {
                outcome.permission = Some(PermissionDecision::Allow);
            }
            if let Some(context) = parsed.get("additionalContext").and_then(Value::as_str) {
                outcome.context = Some(context.to_owned());
            }
            if let Some(specific) = parsed.get("hookSpecificOutput") {
                match specific.get("permissionDecision").and_then(Value::as_str) {
                    Some("allow") => outcome.permission = Some(PermissionDecision::Allow),
                    Some("deny") => outcome.permission = Some(PermissionDecision::Deny),
                    Some("ask") => outcome.permission = Some(PermissionDecision::Ask),
                    _ => {}
                }
                if let Some(context) = specific.get("additionalContext").and_then(Value::as_str) {
                    outcome.context = Some(context.to_owned());
                }
                if let Some(input) = specific.get("updatedInput").and_then(Value::as_object) {
                    outcome.updated_input = Some(Value::Object(input.clone()));
                }
                match specific
                    .get("decision")
                    .and_then(|decision| decision.get("behavior"))
                    .and_then(Value::as_str)
                {
                    Some("allow") => outcome.permission = Some(PermissionDecision::Allow),
                    Some("deny") => {
                        outcome.permission = Some(PermissionDecision::Deny);
                        outcome.block = specific
                            .get("decision")
                            .and_then(|decision| decision.get("message"))
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                    }
                    _ => {}
                }
            }
        }
        outcome
    }

    async fn run_event(&self, event: &str, tool: Option<&str>, payload: &Value) -> HookOutcome {
        let mut combined = HookOutcome::default();
        for entry in self.matching(event, tool) {
            let outcome = self.run(entry, payload).await;
            if outcome.block.is_some() && combined.block.is_none() {
                combined.block = outcome.block;
            }
            if let Some(context) = outcome.context {
                match &mut combined.context {
                    Some(existing) => {
                        existing.push('\n');
                        existing.push_str(&context);
                    }
                    None => combined.context = Some(context),
                }
            }
            // Deny outranks allow when several hooks answer.
            match (combined.permission, outcome.permission) {
                (_, Some(PermissionDecision::Deny)) => {
                    combined.permission = Some(PermissionDecision::Deny);
                }
                (None, decision) => combined.permission = decision,
                _ => {}
            }
            if outcome.updated_input.is_some() {
                combined.updated_input = outcome.updated_input;
            }
        }
        combined
    }
}

fn expand_hook_value(value: &str, entry: &HookEntry, workspace: &std::path::Path) -> String {
    let root = entry
        .plugin_root
        .as_deref()
        .map_or_else(String::new, |root| root.display().to_string());
    let data = entry
        .plugin_root
        .as_deref()
        .map_or_else(String::new, |root| root.join(".data").display().to_string());
    value
        .replace("${HAARNESS_PROJECT_DIR}", &workspace.display().to_string())
        .replace("${OPENCODE_PROJECT_DIR}", &workspace.display().to_string())
        .replace("${CLAUDE_PROJECT_DIR}", &workspace.display().to_string())
        .replace("${HAARNESS_PLUGIN_ROOT}", &root)
        .replace("${OPENCODE_PLUGIN_ROOT}", &root)
        .replace("${CLAUDE_PLUGIN_ROOT}", &root)
        .replace("${HAARNESS_PLUGIN_DATA}", &data)
        .replace("${CLAUDE_PLUGIN_DATA}", &data)
}

#[derive(Default)]
struct HookOutcome {
    block: Option<String>,
    context: Option<String>,
    permission: Option<PermissionDecision>,
    updated_input: Option<Value>,
}

impl From<HookOutcome> for HookDecision {
    fn from(outcome: HookOutcome) -> Self {
        Self {
            block: outcome.block,
            context: outcome.context,
            permission: outcome.permission,
            updated_input: outcome.updated_input,
        }
    }
}

/// Native tools whose successful completion changes workspace files, with
/// the argument that names the file. Shell commands are not inspected.
fn changed_file(tool: &str, arguments: &Value) -> Option<String> {
    if !matches!(tool, "write" | "edit" | "multiedit" | "notebook_edit" | "notebookedit") {
        return None;
    }
    arguments
        .get("path")
        .or_else(|| arguments.get("file_path"))
        .or_else(|| arguments.get("notebook_path"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// `A|B` alternation or a literal, matched against the haarness tool name,
/// accepting Claude tool aliases (Bash → bash, Edit → edit, …).
fn matcher_matches(matcher: &str, tool: &str) -> bool {
    let canonical_tool = if tool == "todo" { "todowrite" } else { tool };
    let normalized = matcher
        .split('|')
        .map(str::trim)
        .map(|candidate| {
            if candidate.eq_ignore_ascii_case("todo") {
                "todowrite"
            } else {
                claude_alias(candidate).unwrap_or(candidate)
            }
        })
        .collect::<Vec<_>>()
        .join("|");
    if normalized
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "_|-".contains(character))
    {
        return normalized
            .split('|')
            .any(|candidate| candidate == canonical_tool);
    }
    regex::Regex::new(&normalized).is_ok_and(|pattern| pattern.is_match(canonical_tool))
        || matcher.split('|').map(str::trim).any(|candidate| {
            let canonical_candidate = if candidate.eq_ignore_ascii_case("todo") {
                "todowrite"
            } else {
                candidate
            };
            canonical_candidate.eq_ignore_ascii_case(canonical_tool)
                || claude_alias(candidate).is_some_and(|alias| alias == canonical_tool)
        })
}

fn claude_alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "Bash" => "bash",
        "Read" => "read",
        "Edit" | "MultiEdit" | "NotebookEdit" => "edit",
        "Write" => "write",
        "WebFetch" => "webfetch",
        "Glob" => "glob",
        "Grep" => "grep",
        "LS" => "list",
        "Task" => "task",
        "TodoWrite" => "todowrite",
        _ => return None,
    })
}

#[async_trait]
impl HookPort for ConfiguredHooks {
    async fn pre_tool_use(&self, thread: ThreadId, tool: &str, arguments: &Value) -> HookDecision {
        self.run_event(
            "PreToolUse",
            Some(tool),
            &json!({
                "hook_event_name": "PreToolUse",
                "session_id": thread,
                "cwd": self.workspace,
                "tool_name": tool,
                "tool_input": arguments,
            }),
        )
        .await
        .into()
    }

    async fn post_tool_use(
        &self,
        thread: ThreadId,
        tool: &str,
        arguments: &Value,
        output: &str,
        is_error: bool,
    ) -> Option<String> {
        let context = self
            .run_event(
                if is_error {
                    "PostToolUseFailure"
                } else {
                    "PostToolUse"
                },
                Some(tool),
                &json!({
                    "hook_event_name": "PostToolUse",
                    "session_id": thread,
                    "cwd": self.workspace,
                    "tool_name": tool,
                    "tool_input": arguments,
                    "tool_response": {"output": output, "is_error": is_error},
                    "error": is_error.then_some(output),
                }),
            )
            .await
            .context;
        if !is_error && let Some(file_path) = changed_file(tool, arguments) {
            let _ = self
                .run_event(
                    "FileChanged",
                    Some(tool),
                    &json!({
                        "hook_event_name": "FileChanged",
                        "session_id": thread,
                        "cwd": self.workspace,
                        "tool_name": tool,
                        "tool_input": arguments,
                        "file_path": file_path,
                    }),
                )
                .await;
        }
        context
    }

    async fn user_prompt_submit(&self, thread: ThreadId, prompt: &str) -> HookDecision {
        self.run_event(
            "UserPromptSubmit",
            None,
            &json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": thread,
                "cwd": self.workspace,
                "prompt": prompt,
            }),
        )
        .await
        .into()
    }

    async fn permission_request(
        &self,
        thread: ThreadId,
        permission: &str,
        detail: &Value,
    ) -> HookDecision {
        self.run_event(
            "PermissionRequest",
            Some(permission),
            &json!({
                "hook_event_name": "PermissionRequest",
                "session_id": thread,
                "cwd": self.workspace,
                "tool_name": permission,
                "tool_input": detail,
            }),
        )
        .await
        .into()
    }

    async fn post_compact(&self, thread: ThreadId, trigger: &str, item: &haarness_protocol::Item) {
        let _ = self
            .run_event(
                "PostCompact",
                Some(trigger),
                &json!({
                    "hook_event_name":"PostCompact", "session_id":thread, "cwd":self.workspace,
                    "trigger":trigger, "compaction":item,
                }),
            )
            .await;
    }
    async fn stop_failure(
        &self,
        thread: ThreadId,
        turn: haarness_protocol::TurnId,
        detail: &Value,
    ) {
        let mut payload = detail.clone();
        payload["hook_event_name"] = json!("StopFailure");
        payload["session_id"] = json!(thread);
        payload["turn_id"] = json!(turn);
        payload["cwd"] = json!(self.workspace);
        let _ = self
            .run_event("StopFailure", detail["error"].as_str(), &payload)
            .await;
    }

    async fn stop(&self, thread: ThreadId) {
        let _ = self
            .run_event(
                "Stop",
                None,
                &json!({
                    "hook_event_name": "Stop",
                    "session_id": thread,
                    "cwd": self.workspace,
                }),
            )
            .await;
    }

    async fn session_start(&self, thread: ThreadId, agent: &str) {
        let _ = self
            .run_event(
                "SessionStart",
                Some("startup"),
                &json!({
                    "hook_event_name": "SessionStart",
                    "session_id": thread,
                    "cwd": self.workspace,
                    "source": "startup",
                    "agent": agent,
                }),
            )
            .await;
    }

    async fn session_end(&self, thread: ThreadId, reason: &str) {
        let _ = self
            .run_event(
                "SessionEnd",
                Some(reason),
                &json!({
                    "hook_event_name": "SessionEnd",
                    "session_id": thread,
                    "cwd": self.workspace,
                    "reason": reason,
                }),
            )
            .await;
    }

    async fn turn_end(&self, thread: ThreadId, turn: haarness_protocol::TurnId, detail: &Value) {
        let mut payload = json!({
            "hook_event_name": "TurnEnd",
            "session_id": thread,
            "cwd": self.workspace,
            "turn_id": turn,
            "agent": detail.get("agent"),
            "provider": detail.get("provider"),
            "model": detail.get("model"),
            "finish_reason": detail.get("finishReason"),
        });
        if let (Some(target), Some(extra)) = (payload.as_object_mut(), detail.as_object()) {
            for (key, value) in extra {
                target.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        let _ = self.run_event("TurnEnd", None, &payload).await;
    }

    async fn notification(&self, thread: ThreadId, kind: &str, message: &str) {
        let _ = self
            .run_event(
                "Notification",
                Some(kind),
                &json!({
                    "hook_event_name": "Notification",
                    "session_id": thread,
                    "cwd": self.workspace,
                    "notification_type": kind,
                    "message": message,
                }),
            )
            .await;
    }

    async fn config_change(&self, detail: &Value) {
        let mut payload = json!({
            "hook_event_name": "ConfigChange",
            "cwd": self.workspace,
            "source": detail.get("source").cloned().unwrap_or_else(|| json!("reload")),
        });
        if let (Some(target), Some(extra)) = (payload.as_object_mut(), detail.as_object()) {
            for (key, value) in extra {
                target.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        let _ = self.run_event("ConfigChange", None, &payload).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_blocks_parse_into_entries() {
        let block = json!({
            "PreToolUse": [
                {"matcher": "Bash|Edit", "hooks": [
                    {"type": "command", "command": "./check.sh", "timeout": 10},
                ]},
                {"hooks": [{"type": "command", "command": "./always.sh"}]},
            ],
            "Stop": [{"hooks": [{"command": "./notify.sh"}]}],
        });
        let entries = ConfiguredHooks::parse_block(&block);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].event, "PreToolUse");
        assert_eq!(entries[0].matcher.as_deref(), Some("Bash|Edit"));
        assert_eq!(entries[0].timeout, Duration::from_secs(10));
        assert!(entries[1].matcher.is_none());
    }

    #[test]
    fn matchers_accept_claude_tool_aliases() {
        assert!(matcher_matches("Bash|Edit", "bash"));
        assert!(matcher_matches("Bash|Edit", "edit"));
        assert!(matcher_matches("bash", "bash"));
        assert!(!matcher_matches("Bash", "write"));
        assert!(matcher_matches("MultiEdit", "edit"));
        assert!(matcher_matches("TodoWrite", "todo"));
        assert!(matcher_matches("todo", "todowrite"));
        assert!(matcher_matches("^(bash|write)$", "bash"));
        assert!(!matcher_matches("[", "bash"));
    }

    #[tokio::test]
    async fn hooks_block_via_exit_code_and_json() {
        let hooks = ConfiguredHooks::new(
            ConfiguredHooks::parse_block(&json!({
                "PreToolUse": [{"matcher": "Bash", "hooks": [
                    {"command": "echo 'nope' >&2; exit 2"},
                ]}],
            })),
            std::env::temp_dir(),
        );
        let decision = hooks
            .pre_tool_use(ThreadId::new(), "bash", &json!({"command": "true"}))
            .await;
        assert_eq!(decision.block.as_deref(), Some("nope"));

        let hooks = ConfiguredHooks::new(
            ConfiguredHooks::parse_block(&json!({
                "PreToolUse": [{"hooks": [
                    {"command": r#"echo '{"hookSpecificOutput":{"permissionDecision":"allow"},"additionalContext":"trusted path"}'"#},
                ]}],
            })),
            std::env::temp_dir(),
        );
        let decision = hooks
            .pre_tool_use(ThreadId::new(), "bash", &json!({"command": "true"}))
            .await;
        assert!(decision.block.is_none());
        assert_eq!(decision.permission, Some(PermissionDecision::Allow));
        assert_eq!(decision.context.as_deref(), Some("trusted path"));
    }

    #[tokio::test]
    async fn hook_stdin_carries_the_tool_payload() {
        let marker =
            std::env::temp_dir().join(format!("haarness-hook-test-{}.json", uuid::Uuid::now_v7()));
        let hooks = ConfiguredHooks::new(
            ConfiguredHooks::parse_block(&json!({
                "PostToolUse": [{"hooks": [
                    {"command": format!("cat > {}", marker.display())},
                ]}],
            })),
            std::env::temp_dir(),
        );
        let _ = hooks
            .post_tool_use(
                ThreadId::new(),
                "bash",
                &json!({"command": "ls"}),
                "listing",
                false,
            )
            .await;
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
        assert_eq!(written["hook_event_name"], "PostToolUse");
        assert_eq!(written["tool_name"], "bash");
        assert_eq!(written["tool_input"]["command"], "ls");
        assert_eq!(written["tool_response"]["output"], "listing");
        let _ = std::fs::remove_file(&marker);
    }

    #[tokio::test]
    async fn plugin_hooks_expand_root_placeholders_and_environment() {
        let root =
            std::env::temp_dir().join(format!("haarness-plugin-hook-{}", uuid::Uuid::now_v7()));
        let marker = root.join("hook.txt");
        std::fs::create_dir_all(&root).unwrap();
        let hooks = ConfiguredHooks::new(
            ConfiguredHooks::parse_plugin_block(
                &json!({
                    "PostToolUseFailure": [{"hooks": [{
                        "command": "sh",
                        "args": ["-c", format!(
                            "printf '%s|%s' '${{CLAUDE_PLUGIN_ROOT}}' \"$HAARNESS_PLUGIN_ROOT\" > {}",
                            marker.display()
                        )]
                    }]}]
                }),
                root.clone(),
            ),
            root.clone(),
        );
        let _ = hooks
            .post_tool_use(
                ThreadId::new(),
                "bash",
                &json!({"command": "false"}),
                "failed",
                true,
            )
            .await;
        let rendered = root.display().to_string();
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            format!("{rendered}|{rendered}")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn permission_request_hooks_can_resolve_the_prompt() {
        let hooks = ConfiguredHooks::new(
            ConfiguredHooks::parse_block(&json!({
                "PermissionRequest": [{"matcher": "bash", "hooks": [{
                    "command": r#"echo '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}'"#
                }]}]
            })),
            std::env::temp_dir(),
        );
        let decision = hooks
            .permission_request(ThreadId::new(), "bash", &json!({"command": "cargo test"}))
            .await;
        assert_eq!(decision.permission, Some(PermissionDecision::Allow));
        assert!(decision.block.is_none());
    }

    #[tokio::test]
    async fn pre_tool_hooks_can_replace_tool_input() {
        let hooks = ConfiguredHooks::new(
            ConfiguredHooks::parse_block(&json!({
                "PreToolUse": [{"matcher": "Bash", "hooks": [{
                    "command": r#"echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","updatedInput":{"command":"cargo test"}}}'"#
                }]}]
            })),
            std::env::temp_dir(),
        );
        let decision = hooks
            .pre_tool_use(
                ThreadId::new(),
                "bash",
                &json!({"command": "cargo test --all"}),
            )
            .await;
        assert_eq!(
            decision.updated_input,
            Some(json!({"command": "cargo test"}))
        );
    }

    async fn captured(block: Value, run: impl AsyncFnOnce(&ConfiguredHooks)) -> Vec<Value> {
        let root = std::env::temp_dir().join(format!(
            "haarness-lifecycle-hooks-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let log = root.join("events.jsonl");
        let mut block = block;
        for groups in block.as_object_mut().unwrap().values_mut() {
            for group in groups.as_array_mut().unwrap() {
                group["hooks"] = json!([{"command": format!("cat >> {}; echo >> {}", log.display(), log.display())}]);
            }
        }
        let hooks = ConfiguredHooks::new(ConfiguredHooks::parse_block(&block), root.clone());
        run(&hooks).await;
        let written = std::fs::read_to_string(&log).unwrap_or_default();
        std::fs::remove_dir_all(root).unwrap();
        written
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn lifecycle_events_carry_haarness_payloads_and_honor_matchers() {
        let thread = ThreadId::new();
        let turn = haarness_protocol::TurnId::new();
        let events = captured(
            json!({
                "SessionStart": [{"matcher": "startup"}],
                "SessionEnd": [{}],
                "TurnEnd": [{}],
                "Notification": [{"matcher": "permission_prompt"}],
                "ConfigChange": [{}],
            }),
            async |hooks| {
                hooks.session_start(thread, "build").await;
                hooks.session_end(thread, "archived").await;
                hooks
                    .turn_end(
                        thread,
                        turn,
                        &json!({"agent":"build","provider":"echo","model":"echo-1","finishReason":"stop","turnId":turn}),
                    )
                    .await;
                hooks.notification(thread, "turn_completed", "Session completed").await;
                hooks.notification(thread, "permission_prompt", "Permission needed: bash").await;
                hooks.config_change(&json!({"source":"reload"})).await;
            },
        )
        .await;
        let names: Vec<_> = events
            .iter()
            .map(|event| event["hook_event_name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            names,
            ["SessionStart", "SessionEnd", "TurnEnd", "Notification", "ConfigChange"]
        );
        assert_eq!(events[0]["source"], "startup");
        assert_eq!(events[0]["agent"], "build");
        assert_eq!(events[0]["session_id"], json!(thread));
        assert_eq!(events[1]["reason"], "archived");
        assert_eq!(events[2]["turn_id"], json!(turn));
        assert_eq!(events[2]["finish_reason"], "stop");
        assert_eq!(events[2]["model"], "echo-1");
        assert_eq!(events[3]["notification_type"], "permission_prompt");
        assert_eq!(events[3]["message"], "Permission needed: bash");
        assert_eq!(events[4]["source"], "reload");
        assert!(events.iter().all(|event| event["cwd"].is_string()));
    }

    #[tokio::test]
    async fn recovery_lifecycle_hooks_filter_and_deliver_payloads() {
        let thread = ThreadId::new();
        let turn = haarness_protocol::TurnId::new();
        let item: haarness_protocol::Item = serde_json::from_value(json!({
            "id":haarness_protocol::ItemId::new(), "threadId":thread, "turnId":null,
            "sequence":1, "createdAt":"2026-09-30T00:00:00Z",
            "data":{"type":"compaction","summary":"saved summary","files_touched":[],"outstanding":[]}
        })).unwrap();
        let events = captured(json!({
            "PostCompact":[{"matcher":"manual"}],
            "StopFailure":[{"matcher":"incomplete|content_filter"}]
        }), async |hooks| {
            hooks.post_compact(thread, "automatic", &item).await;
            hooks.post_compact(thread, "manual", &item).await;
            hooks.stop_failure(thread, turn, &json!({"error":"provider"})).await;
            hooks.stop_failure(thread, turn, &json!({"error":"incomplete","error_details":"truncated","provider_id":"echo","model_id":"echo-1"})).await;
        }).await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["hook_event_name"], "PostCompact");
        assert_eq!(events[0]["compaction"]["id"], json!(item.id));
        assert_eq!(events[0]["trigger"], "manual");
        assert_eq!(events[1]["hook_event_name"], "StopFailure");
        assert_eq!(events[1]["turn_id"], json!(turn));
        assert_eq!(events[1]["error_details"], "truncated");
        assert_eq!(events[1]["model_id"], "echo-1");
        assert!(
            events
                .iter()
                .all(|event| event["session_id"] == json!(thread) && event["cwd"].is_string())
        );
    }

    #[tokio::test]
    async fn file_changed_follows_successful_write_and_edit_tools_only() {
        let thread = ThreadId::new();
        let events = captured(
            json!({"FileChanged": [{"matcher": "Write|Edit"}]}),
            async |hooks| {
                hooks
                    .post_tool_use(thread, "write", &json!({"path":"src/lib.rs","content":"x"}), "ok", false)
                    .await;
                hooks
                    .post_tool_use(thread, "edit", &json!({"path":"README.md","old":"a","new":"b"}), "failed", true)
                    .await;
                hooks
                    .post_tool_use(thread, "bash", &json!({"command":"touch other"}), "ok", false)
                    .await;
                hooks
                    .post_tool_use(thread, "read", &json!({"path":"src/lib.rs"}), "ok", false)
                    .await;
            },
        )
        .await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["hook_event_name"], "FileChanged");
        assert_eq!(events[0]["tool_name"], "write");
        assert_eq!(events[0]["file_path"], "src/lib.rs");
        assert_eq!(events[0]["tool_input"]["content"], "x");
    }
}
